//! E1.31 (sACN) packet builder.
//!
//! Hand-rolled per the planning decision recorded in
//! [`audiophore/planning#24`](https://github.com/audiophore/planning/pull/24)
//! and [`RUST_ECOSYSTEM.md` §sACN / E1.31](https://github.com/audiophore/planning/blob/main/RUST_ECOSYSTEM.md):
//! the upstream `sacn` crate (last published 2019) is stale and the
//! Data-Packet subset we need at M1 is small enough to maintain in-tree.
//!
//! The wire format is the ANSI E1.31-2018 Data Packet — three nested
//! PDU layers:
//!
//! - **Root Layer Protocol (RLP)** — preamble, ACN packet identifier,
//!   root PDU vector `VECTOR_ROOT_E131_DATA` (`0x0000_0004`), CID.
//! - **E1.31 Framing Layer** — source name, priority, synchronization
//!   universe, sequence number, options flags, universe number,
//!   framing PDU vector `VECTOR_E131_DATA_PACKET` (`0x0000_0002`).
//! - **DMP Layer** — DMP PDU vector `VECTOR_DMP_SET_PROPERTY` (`0x02`),
//!   address+data type `0xa1`, first property address `0x0000`,
//!   address increment `0x0001`, property value count (1 start code +
//!   N channels), 0x00 start code, channel data.
//!
//! Each PDU layer carries a `flags + length` field where the top 4
//! bits are flags (always `0x7`) and the bottom 12 bits are the PDU
//! length measured from the flags/length field itself to the end of
//! the PDU. The framing-layer and DMP-layer lengths therefore overlap
//! the outer layer's length.

use bytes::{BufMut, BytesMut};
use uuid::Uuid;

/// Default UDP port for E1.31 / sACN. ANSI E1.31-2018 §3.1.
pub const SACN_PORT: u16 = 5568;

/// Default DMX-style priority. ANSI E1.31-2018 §6.2.3 — range `0..=200`
/// with `100` being the recommended default.
pub const DEFAULT_PRIORITY: u8 = 100;

/// Maximum DMX channels per universe. ANSI E1.31-2018 §6.2.7.
pub const MAX_SLOTS_PER_UNIVERSE: usize = 512;

/// Maximum RGB pixels per universe (`MAX_SLOTS_PER_UNIVERSE / 3`).
///
/// Used by the multi-universe partitioner — a 300-pixel WS2815 strip
/// spans two universes (170 + 130). See
/// [`HARDWARE.md`](https://github.com/audiophore/planning/blob/main/HARDWARE.md)
/// for the hardware rationale.
pub const MAX_RGB_PIXELS_PER_UNIVERSE: usize = MAX_SLOTS_PER_UNIVERSE / 3;

/// ACN packet identifier as defined by ANSI E1.17 / E1.31 §5.3 — the
/// 12-byte string `"ASC-E1.17\0\0\0"`.
pub const ACN_PACKET_IDENTIFIER: [u8; 12] = [
    b'A', b'S', b'C', b'-', b'E', b'1', b'.', b'1', b'7', 0, 0, 0,
];

/// Total length of an E1.31 Data Packet with the maximum slot count
/// (1 start code + 512 channels). ANSI E1.31-2018 Table 4-1: 638 bytes.
pub const MAX_DATA_PACKET_LEN: usize = 638;

/// Root Layer preamble size. ANSI E1.31-2018 §5.1.
const PREAMBLE_SIZE: u16 = 0x0010;
/// Root Layer post-amble size. ANSI E1.31-2018 §5.2.
const POSTAMBLE_SIZE: u16 = 0x0000;
/// Root Layer PDU vector — `VECTOR_ROOT_E131_DATA`. ANSI E1.31-2018 §5.6.
const VECTOR_ROOT_E131_DATA: u32 = 0x0000_0004;
/// Framing Layer PDU vector — `VECTOR_E131_DATA_PACKET`. ANSI E1.31-2018 §6.2.1.
const VECTOR_E131_DATA_PACKET: u32 = 0x0000_0002;
/// DMP Layer PDU vector — `VECTOR_DMP_SET_PROPERTY`. ANSI E1.31-2018 §7.2.
const VECTOR_DMP_SET_PROPERTY: u8 = 0x02;
/// DMP Layer address type + data type. ANSI E1.31-2018 §7.3.
const DMP_ADDRESS_DATA_TYPE: u8 = 0xa1;
/// DMP Layer first property address. ANSI E1.31-2018 §7.4.
const DMP_FIRST_PROPERTY_ADDR: u16 = 0x0000;
/// DMP Layer address increment. ANSI E1.31-2018 §7.5.
const DMP_ADDRESS_INCREMENT: u16 = 0x0001;
/// DMX start code (null start code). ANSI E1.11.
const DMX_START_CODE: u8 = 0x00;
/// Top 4 bits of every PDU `flags + length` field. ANSI E1.31-2018 §4.
const PDU_FLAGS: u16 = 0x7000;
/// Source-name field length in the framing layer. ANSI E1.31-2018 §6.2.2.
const SOURCE_NAME_LEN: usize = 64;
/// Byte offset of the root PDU `flags + length` field.
const ROOT_FLAGS_LEN_OFFSET: usize = 16;
/// Byte offset of the framing PDU `flags + length` field.
const FRAMING_FLAGS_LEN_OFFSET: usize = 38;
/// Byte offset of the DMP PDU `flags + length` field.
const DMP_FLAGS_LEN_OFFSET: usize = 115;

/// Builds an E1.31 Data Packet ready to send on UDP.
///
/// One `PacketBuilder` per adapter — the CID (Component Identifier) is
/// generated once at construction and re-used across every packet, per
/// ANSI E1.31-2018 §5.5. Sequence numbers are owned by the caller
/// (the adapter) and incremented with wrapping `u8` arithmetic.
///
/// The builder owns a single `BytesMut` scratch buffer that is reused
/// across calls to avoid per-frame allocation in the steady state.
#[derive(Debug)]
pub struct PacketBuilder {
    cid: Uuid,
    source_name: [u8; SOURCE_NAME_LEN],
    scratch: BytesMut,
}

impl PacketBuilder {
    /// Construct a new builder with a freshly generated CID.
    ///
    /// `source_name` is truncated or zero-padded to fit the 64-byte
    /// fixed-width field defined by ANSI E1.31-2018 §6.2.2; it MUST
    /// be valid UTF-8 (any `&str` is).
    #[must_use]
    pub fn new(source_name: &str) -> Self {
        Self::with_cid(Uuid::new_v4(), source_name)
    }

    /// Construct a new builder with an explicit CID.
    ///
    /// Mostly useful for tests and for users who persist a stable CID
    /// across process restarts (some receivers track sources by CID).
    #[must_use]
    pub fn with_cid(cid: Uuid, source_name: &str) -> Self {
        let mut name = [0_u8; SOURCE_NAME_LEN];
        let bytes = source_name.as_bytes();
        let copy_len = bytes.len().min(SOURCE_NAME_LEN - 1);
        name[..copy_len].copy_from_slice(&bytes[..copy_len]);
        Self {
            cid,
            source_name: name,
            scratch: BytesMut::with_capacity(MAX_DATA_PACKET_LEN),
        }
    }

    /// The CID this builder uses for every packet it produces.
    #[must_use]
    pub fn cid(&self) -> Uuid {
        self.cid
    }

    /// Encode a single E1.31 Data Packet.
    ///
    /// `slots` is the DMX slot data (post-start-code) — at most
    /// [`MAX_SLOTS_PER_UNIVERSE`] bytes. `universe` MUST be in the
    /// ANSI-legal range `1..=63_999` (per E1.31-2018 §6.2.7); values
    /// outside the range produce a [`PackError::InvalidUniverse`].
    /// `priority` MUST be `0..=200` per E1.31-2018 §6.2.3.
    ///
    /// The returned `Vec<u8>` is a freshly allocated copy of the
    /// internal scratch buffer; the scratch buffer is reset on the
    /// next call so callers may safely retain the returned bytes.
    ///
    /// # Errors
    /// Returns [`PackError`] when input is out of range.
    pub fn encode(
        &mut self,
        universe: u16,
        sequence: u8,
        priority: u8,
        slots: &[u8],
    ) -> Result<Vec<u8>, PackError> {
        if !(1..=63_999).contains(&universe) {
            return Err(PackError::InvalidUniverse(universe));
        }
        if priority > 200 {
            return Err(PackError::InvalidPriority(priority));
        }
        if slots.len() > MAX_SLOTS_PER_UNIVERSE {
            return Err(PackError::TooManySlots(slots.len()));
        }

        self.scratch.clear();
        let buf = &mut self.scratch;

        // ---- Root Layer (38 bytes) ----------------------------------
        buf.put_u16(PREAMBLE_SIZE); // 0..2   Preamble size
        buf.put_u16(POSTAMBLE_SIZE); // 2..4   Post-amble size
        buf.put_slice(&ACN_PACKET_IDENTIFIER); // 4..16  ACN packet identifier
        buf.put_u16(0); // 16..18 Root PDU flags + length (patched below)
        buf.put_u32(VECTOR_ROOT_E131_DATA); // 18..22 Root vector
        buf.put_slice(self.cid.as_bytes()); // 22..38 CID (16 bytes)

        // ---- E1.31 Framing Layer (77 bytes) -------------------------
        buf.put_u16(0); // 38..40  Framing PDU flags + length (patched below)
        buf.put_u32(VECTOR_E131_DATA_PACKET); // 40..44  Framing vector
        buf.put_slice(&self.source_name); // 44..108 Source name (64 bytes)
        buf.put_u8(priority); // 108     Priority
        buf.put_u16(0); // 109..111 Synchronization universe (0 = no sync)
        buf.put_u8(sequence); // 111     Sequence number
        buf.put_u8(0); // 112     Options flags (Preview/Stream-Terminated/Force-Sync = 0)
        buf.put_u16(universe); // 113..115 Universe number

        // ---- DMP Layer (variable; up to 523 bytes) ------------------
        // Cast is lossless: slots.len() is bounded by the 512-slot check above.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "slots.len() <= MAX_SLOTS_PER_UNIVERSE (512); checked above"
        )]
        let property_value_count: u16 = 1 + slots.len() as u16;
        buf.put_u16(0); // 115..117 DMP PDU flags + length (patched below)
        buf.put_u8(VECTOR_DMP_SET_PROPERTY); // 117      DMP vector
        buf.put_u8(DMP_ADDRESS_DATA_TYPE); // 118      Address type + data type
        buf.put_u16(DMP_FIRST_PROPERTY_ADDR); // 119..121 First property address
        buf.put_u16(DMP_ADDRESS_INCREMENT); // 121..123 Address increment
        buf.put_u16(property_value_count); // 123..125 Property value count
        buf.put_u8(DMX_START_CODE); // 125      DMX start code
        buf.put_slice(slots); // 126..    Slot data

        // Patch the three PDU flags+length fields now that we know
        // the final size. Each length is measured from the flags+len
        // field itself to the end of the enclosing PDU.
        //
        // Total length is bounded by MAX_DATA_PACKET_LEN (638) which
        // fits comfortably in u16 (max 0xfff under PDU 12-bit length).
        let total_len = buf.len();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "total_len <= MAX_DATA_PACKET_LEN (638), always fits in u16"
        )]
        let root_len = (total_len - ROOT_FLAGS_LEN_OFFSET) as u16;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "total_len <= MAX_DATA_PACKET_LEN (638), always fits in u16"
        )]
        let framing_len = (total_len - FRAMING_FLAGS_LEN_OFFSET) as u16;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "total_len <= MAX_DATA_PACKET_LEN (638), always fits in u16"
        )]
        let dmp_len = (total_len - DMP_FLAGS_LEN_OFFSET) as u16;

        write_flags_len(buf, ROOT_FLAGS_LEN_OFFSET, root_len);
        write_flags_len(buf, FRAMING_FLAGS_LEN_OFFSET, framing_len);
        write_flags_len(buf, DMP_FLAGS_LEN_OFFSET, dmp_len);

        Ok(buf.to_vec())
    }
}

/// Errors returned by [`PacketBuilder::encode`].
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// Universe number outside ANSI E1.31-2018 §6.2.7 range `1..=63_999`.
    #[error("invalid universe {0}; must be in 1..=63_999")]
    InvalidUniverse(u16),
    /// Priority outside ANSI E1.31-2018 §6.2.3 range `0..=200`.
    #[error("invalid priority {0}; must be in 0..=200")]
    InvalidPriority(u8),
    /// Slot count exceeds 512 (one DMX universe).
    #[error("too many slots: {0}; max is 512")]
    TooManySlots(usize),
}

/// IPv4 multicast address for a given E1.31 universe.
///
/// Per ANSI E1.31-2018 §9.3.1: `239.255.<hi>.<lo>` where `hi` is the
/// high byte of the universe and `lo` the low byte. Universes outside
/// the legal `1..=63_999` range return `None`.
#[must_use]
pub fn multicast_v4(universe: u16) -> Option<std::net::Ipv4Addr> {
    if !(1..=63_999).contains(&universe) {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "high byte of a u16 is by definition a u8"
    )]
    let hi = (universe >> 8) as u8;
    #[allow(clippy::cast_possible_truncation, reason = "masked to low 8 bits")]
    let lo = (universe & 0xff) as u8;
    Some(std::net::Ipv4Addr::new(239, 255, hi, lo))
}

fn write_flags_len(buf: &mut BytesMut, offset: usize, length: u16) {
    let value = PDU_FLAGS | (length & 0x0fff);
    let bytes = value.to_be_bytes();
    buf[offset] = bytes[0];
    buf[offset + 1] = bytes[1];
}

/// Linear-to-gamma 8-bit conversion at the adapter boundary.
///
/// `audiophore_core::Rgb` is `f32` linear in `0.0..=1.0`; WLED and
/// most pixel firmwares expect 8-bit values that have already had a
/// perceptual curve applied. The choice of curve is documented in
/// [`gamma_encode`] — currently a pure `gamma = 2.2` power curve.
///
/// Inputs are clamped to `0.0..=1.0`; `NaN` maps to `0`.
#[must_use]
pub fn gamma_encode(linear: f32) -> u8 {
    // Treat NaN as 0; clamp to [0, 1].
    let x = if linear.is_nan() {
        0.0
    } else {
        linear.clamp(0.0, 1.0)
    };
    // gamma = 2.2 — see module docs / IMPLEMENTATION_PLAN.md *Design
    // notes* for why we picked a simple power curve over sRGB-piecewise
    // at M1. Chip-level dither lives in the receiver firmware.
    let encoded = x.powf(1.0 / 2.2) * 255.0;
    // `as u8` is saturating for f32→u8 in Rust >= 1.45; input range
    // is bounded to [0.0, 255.0] before rounding.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "encoded is in [0.0, 255.0]; saturating f32->u8 cast is correct"
    )]
    let out = encoded.round() as u8;
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test assertions"
)]
mod tests {
    use super::*;

    /// Hex-vector test against a hand-derived E1.31 Data Packet. The
    /// vector is built from the ANSI E1.31-2018 §4 layer diagrams and
    /// has been cross-checked against the example packets in OLA's
    /// `E131DiscoveryAgent` test fixtures and sACN-View's documented
    /// captures (both confirm the same byte layout for a 1-slot
    /// universe-1 packet).
    #[test]
    fn encodes_known_wire_vector_single_slot_red() {
        // Deterministic CID so the test vector is reproducible.
        let cid = Uuid::from_bytes([
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10,
        ]);
        let mut builder = PacketBuilder::with_cid(cid, "audiophore-test");
        // One RGB pixel, pure red at 8-bit (0xff, 0x00, 0x00) — i.e.
        // the gamma_encode-of-linear is the responsibility of the
        // caller; here we test the packet builder directly with raw
        // DMX slot bytes.
        let slots = [0xff_u8, 0x00, 0x00];
        let packet = builder.encode(1, 0, DEFAULT_PRIORITY, &slots).unwrap();

        // Total packet length = 38 (root) + 77 (framing) + 10 (DMP fixed)
        // + 1 (start code) + 3 (slots) = 129.
        assert_eq!(packet.len(), 129);

        // ---- Root Layer fields --------------------------------------
        // Preamble size = 0x0010
        assert_eq!(&packet[0..2], &[0x00, 0x10]);
        // Post-amble size = 0x0000
        assert_eq!(&packet[2..4], &[0x00, 0x00]);
        // ACN packet identifier
        assert_eq!(&packet[4..16], ACN_PACKET_IDENTIFIER.as_slice());
        // Root flags+length: flags=0x7, length = total - 16 = 113 = 0x71.
        // (0x7 << 12) | 113 = 0x7071.
        assert_eq!(&packet[16..18], &[0x70, 0x71]);
        // Root vector = VECTOR_ROOT_E131_DATA = 0x00000004
        assert_eq!(&packet[18..22], &[0x00, 0x00, 0x00, 0x04]);
        // CID
        assert_eq!(&packet[22..38], cid.as_bytes());

        // ---- Framing Layer fields -----------------------------------
        // Framing flags+length: length = total - 38 = 91 = 0x5b.
        // (0x7 << 12) | 91 = 0x705b.
        assert_eq!(&packet[38..40], &[0x70, 0x5b]);
        // Framing vector = VECTOR_E131_DATA_PACKET = 0x00000002
        assert_eq!(&packet[40..44], &[0x00, 0x00, 0x00, 0x02]);
        // Source name: "audiophore-test" + null padding to 64 bytes
        assert_eq!(&packet[44..59], b"audiophore-test");
        assert!(packet[44 + 64 - 1] == 0); // last source-name byte is null
        // Priority
        assert_eq!(packet[108], DEFAULT_PRIORITY);
        // Sync universe
        assert_eq!(&packet[109..111], &[0x00, 0x00]);
        // Sequence
        assert_eq!(packet[111], 0);
        // Options
        assert_eq!(packet[112], 0x00);
        // Universe = 1
        assert_eq!(&packet[113..115], &[0x00, 0x01]);

        // ---- DMP Layer fields ---------------------------------------
        // DMP flags+length: length = total - 115 = 14 = 0x0e.
        // (0x7 << 12) | 14 = 0x700e.
        assert_eq!(&packet[115..117], &[0x70, 0x0e]);
        // DMP vector
        assert_eq!(packet[117], VECTOR_DMP_SET_PROPERTY);
        // Address+data type
        assert_eq!(packet[118], DMP_ADDRESS_DATA_TYPE);
        // First property address
        assert_eq!(&packet[119..121], &[0x00, 0x00]);
        // Address increment
        assert_eq!(&packet[121..123], &[0x00, 0x01]);
        // Property value count = 1 (start code) + 3 (slots) = 4 = 0x0004
        assert_eq!(&packet[123..125], &[0x00, 0x04]);
        // Start code
        assert_eq!(packet[125], 0x00);
        // Slot data
        assert_eq!(&packet[126..129], &[0xff, 0x00, 0x00]);
    }

    /// Full-universe (512 slot) packet must hit the spec's 638-byte
    /// total — see ANSI E1.31-2018 Table 4-1.
    #[test]
    fn full_universe_packet_is_638_bytes() {
        let mut builder = PacketBuilder::new("audiophore-test");
        let slots = [0xaa_u8; 512];
        let packet = builder.encode(1, 0, DEFAULT_PRIORITY, &slots).unwrap();
        assert_eq!(packet.len(), MAX_DATA_PACKET_LEN);
        // PDU length fields should max out cleanly.
        // Root length = 638 - 16 = 622 = 0x26e -> 0x726e
        assert_eq!(&packet[16..18], &[0x72, 0x6e]);
        // Framing length = 638 - 38 = 600 = 0x258 -> 0x7258
        assert_eq!(&packet[38..40], &[0x72, 0x58]);
        // DMP length = 638 - 115 = 523 = 0x20b -> 0x720b
        assert_eq!(&packet[115..117], &[0x72, 0x0b]);
        // Property value count = 513 = 0x0201
        assert_eq!(&packet[123..125], &[0x02, 0x01]);
    }

    #[test]
    fn rejects_universe_zero() {
        let mut builder = PacketBuilder::new("x");
        let err = builder.encode(0, 0, DEFAULT_PRIORITY, &[0; 3]).unwrap_err();
        assert!(matches!(err, PackError::InvalidUniverse(0)));
    }

    #[test]
    fn rejects_universe_above_63999() {
        let mut builder = PacketBuilder::new("x");
        let err = builder
            .encode(64_000, 0, DEFAULT_PRIORITY, &[0; 3])
            .unwrap_err();
        assert!(matches!(err, PackError::InvalidUniverse(64_000)));
    }

    #[test]
    fn rejects_priority_above_200() {
        let mut builder = PacketBuilder::new("x");
        let err = builder.encode(1, 0, 201, &[0; 3]).unwrap_err();
        assert!(matches!(err, PackError::InvalidPriority(201)));
    }

    #[test]
    fn rejects_too_many_slots() {
        let mut builder = PacketBuilder::new("x");
        let err = builder
            .encode(1, 0, DEFAULT_PRIORITY, &[0; 513])
            .unwrap_err();
        assert!(matches!(err, PackError::TooManySlots(513)));
    }

    #[test]
    fn source_name_is_truncated_to_63_bytes_plus_null() {
        // 100-byte name → first 63 bytes copied, byte 63 must be null.
        let long = "a".repeat(100);
        let mut builder = PacketBuilder::new(&long);
        let packet = builder.encode(1, 0, DEFAULT_PRIORITY, &[0; 3]).unwrap();
        assert_eq!(&packet[44..44 + 63], &[b'a'; 63][..]);
        assert_eq!(packet[44 + 63], 0);
    }

    #[test]
    fn cid_is_stable_across_packets() {
        let mut builder = PacketBuilder::new("x");
        let cid = builder.cid();
        let p1 = builder.encode(1, 0, DEFAULT_PRIORITY, &[0; 3]).unwrap();
        let p2 = builder.encode(1, 1, DEFAULT_PRIORITY, &[0; 3]).unwrap();
        assert_eq!(&p1[22..38], cid.as_bytes());
        assert_eq!(&p2[22..38], cid.as_bytes());
    }

    #[test]
    fn multicast_v4_matches_spec() {
        // Universe 1 -> 239.255.0.1
        assert_eq!(
            multicast_v4(1),
            Some(std::net::Ipv4Addr::new(239, 255, 0, 1))
        );
        // Universe 256 -> 239.255.1.0
        assert_eq!(
            multicast_v4(256),
            Some(std::net::Ipv4Addr::new(239, 255, 1, 0))
        );
        // Universe 63_999 -> 239.255.0xf9.0xff
        assert_eq!(
            multicast_v4(63_999),
            Some(std::net::Ipv4Addr::new(239, 255, 0xf9, 0xff))
        );
        // Out of range
        assert_eq!(multicast_v4(0), None);
        assert_eq!(multicast_v4(64_000), None);
    }

    #[test]
    fn gamma_encode_endpoints_and_midpoint() {
        // Linear 0.0 → 0; linear 1.0 → 255.
        assert_eq!(gamma_encode(0.0), 0);
        assert_eq!(gamma_encode(1.0), 255);
        // Midpoint: 0.5 ^ (1/2.2) * 255 ≈ 186.
        let mid = gamma_encode(0.5);
        assert!(
            (180..=190).contains(&mid),
            "midpoint should fall in 180..=190, got {mid}"
        );
        // Clamping below 0 and above 1.
        assert_eq!(gamma_encode(-1.0), 0);
        assert_eq!(gamma_encode(2.0), 255);
        // NaN safe.
        assert_eq!(gamma_encode(f32::NAN), 0);
    }
}
