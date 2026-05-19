//! `OutputAdapter` implementation for E1.31 / sACN.
//!
//! Wraps [`crate::packet::PacketBuilder`] with a [`tokio::net::UdpSocket`]
//! transport and the multi-universe partitioning rule from the
//! planning [`HARDWARE.md`](https://github.com/audiophore/planning/blob/main/HARDWARE.md):
//! a 300-pixel WS2815 strip spans two E1.31 universes (170 + 130).
//! The constructor takes a universe **list** for that reason — single
//! universe is just a list of length one.
//!
//! Pixel-source resolution: at M1 the adapter renders the first
//! `PixelStrip` payload it finds in a [`ResolvedFrame`]; the mapping
//! layer (`audiophore-engine` §1.2) will explicitly route zones to
//! adapters when it lands. Linear `Rgb` from core is gamma-encoded to
//! 8-bit at the adapter boundary via [`crate::packet::gamma_encode`].

use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use audiophore_adapter_core::{AdapterError, Capability, OutputAdapter};
use audiophore_core::{ResolvedFrame, Rgb, Zone, ZonePayload};
use tokio::net::UdpSocket;
use uuid::Uuid;

use crate::packet::{
    DEFAULT_PRIORITY, MAX_RGB_PIXELS_PER_UNIVERSE, PacketBuilder, SACN_PORT, gamma_encode,
};

/// Default native render rate: 44 Hz, matching DMX wire timing per
/// [`PROTOCOLS.md` §E1.31/sACN](https://github.com/audiophore/planning/blob/main/PROTOCOLS.md).
/// WLED and most ESP32 firmwares accept higher; bump via
/// [`SacnAdapterBuilder::native_rate`] if desired.
pub const DEFAULT_NATIVE_RATE: Duration = Duration::from_micros(22_727); // ~44 Hz

/// Maximum reasonable universe count we'll partition into. Guards
/// against a misconfigured zone with millions of pixels silently
/// fanning out into thousands of UDP sends per tick.
pub const MAX_UNIVERSES: usize = 64;

/// E1.31 / sACN [`OutputAdapter`] implementation.
///
/// One `SacnAdapter` instance drives one logical pixel stream split
/// across one or more contiguous E1.31 universes — for example
/// `universes = [1, 2]` for the 300-pixel WS2815 strip (170 px in
/// universe 1, 130 px in universe 2).
///
/// Constructed via [`SacnAdapterBuilder`].
#[derive(Debug)]
pub struct SacnAdapter {
    name: String,
    capabilities: Vec<Capability>,
    zones: Vec<Zone>,
    native_rate: Duration,
    target: SocketAddr,
    universes: Vec<u16>,
    priority: u8,
    builder: PacketBuilder,
    socket: Option<UdpSocket>,
    sequence: u8,
}

impl SacnAdapter {
    /// Borrow the [`PacketBuilder`] this adapter uses. Mostly useful
    /// for tests that want to inspect the CID.
    #[must_use]
    pub fn packet_builder(&self) -> &PacketBuilder {
        &self.builder
    }

    /// Current sequence number — the value used for the **next**
    /// packet. Wraps at `u8::MAX` per ANSI E1.31-2018 §6.2.4.
    #[must_use]
    pub fn next_sequence(&self) -> u8 {
        self.sequence
    }

    /// Convenience: partition a slice of linear `Rgb` pixels into the
    /// per-universe 8-bit DMX slot buffers this adapter would send for
    /// one tick. Exposed for tests and for the M1 loopback harness.
    #[must_use]
    pub fn partition_pixels(&self, pixels: &[Rgb]) -> Vec<Vec<u8>> {
        partition_rgb(pixels, self.universes.len())
    }

    /// The universes this adapter targets (in order).
    #[must_use]
    pub fn universes(&self) -> &[u16] {
        &self.universes
    }

    /// Encode + send one packet per universe with the given DMX slot
    /// buffers. Mostly an internal helper, but exposed for tests.
    ///
    /// # Errors
    /// Returns [`AdapterError::Protocol`] if the slot buffers don't
    /// match the universe count or if packet encoding fails.
    /// Returns [`AdapterError::Connect`] if the socket is not yet open.
    /// Returns [`AdapterError::Io`] on UDP send failure.
    pub async fn send_slots(&mut self, slot_universes: &[Vec<u8>]) -> Result<(), AdapterError> {
        if slot_universes.len() != self.universes.len() {
            return Err(AdapterError::Protocol(format!(
                "slot universe count {} != adapter universe count {}",
                slot_universes.len(),
                self.universes.len()
            )));
        }
        let socket = self
            .socket
            .as_ref()
            .ok_or_else(|| AdapterError::Connect("socket not open; call connect()".into()))?;

        for (universe, slots) in self.universes.iter().zip(slot_universes.iter()) {
            let packet = self
                .builder
                .encode(*universe, self.sequence, self.priority, slots)
                .map_err(|e| AdapterError::Protocol(e.to_string()))?;
            socket.send_to(&packet, self.target).await?;
        }
        self.sequence = self.sequence.wrapping_add(1);
        Ok(())
    }
}

#[async_trait]
impl OutputAdapter for SacnAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn zones(&self) -> &[Zone] {
        &self.zones
    }

    fn native_rate(&self) -> Duration {
        self.native_rate
    }

    async fn connect(&mut self) -> Result<(), AdapterError> {
        // Bind ephemeral; ANSI E1.31 listeners accept traffic from any
        // source port. `0.0.0.0:0` lets the OS pick.
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        self.socket = Some(socket);
        Ok(())
    }

    async fn render(&mut self, frame: &ResolvedFrame) -> Result<(), AdapterError> {
        // M1: render the first PixelStrip payload we find. The mapping
        // layer (§1.2 A.2) will replace this with explicit zone routing.
        let pixels = frame.zones.values().find_map(|payload| match payload {
            ZonePayload::Pixels(p) => Some(p.as_slice()),
            _ => None,
        });
        let Some(pixels) = pixels else {
            // Nothing to render this tick is not an error — adapters
            // may be configured before their zone payloads arrive.
            return Ok(());
        };

        let slot_universes = partition_rgb(pixels, self.universes.len());
        self.send_slots(&slot_universes).await
    }

    async fn disconnect(&mut self) -> Result<(), AdapterError> {
        // Dropping the socket is sufficient; idempotent.
        self.socket = None;
        Ok(())
    }
}

/// Partition a flat slice of linear `Rgb` pixels across `n` universes,
/// returning one DMX slot buffer per universe.
///
/// The first `n - 1` universes get [`MAX_RGB_PIXELS_PER_UNIVERSE`]
/// (170 px / 510 bytes); the last universe gets whatever remains.
/// Trailing slots in a partially filled universe are **not** zero-
/// padded — most receivers (WLED included) treat a short slot count
/// as "leave the rest alone" rather than blanking.
///
/// If `pixels` contains more than `n * MAX_RGB_PIXELS_PER_UNIVERSE`
/// the excess is dropped; misconfiguration should be caught earlier.
fn partition_rgb(pixels: &[Rgb], universe_count: usize) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(universe_count);
    if universe_count == 0 {
        return out;
    }
    let chunk_pixels = MAX_RGB_PIXELS_PER_UNIVERSE;
    let take = pixels
        .len()
        .min(universe_count.saturating_mul(chunk_pixels));
    for chunk in pixels[..take].chunks(chunk_pixels) {
        let mut slots = Vec::with_capacity(chunk.len() * 3);
        for px in chunk {
            slots.push(gamma_encode(px.r));
            slots.push(gamma_encode(px.g));
            slots.push(gamma_encode(px.b));
        }
        out.push(slots);
    }
    // If we have fewer pixels than universes, pad the remainder with
    // empty universes so the caller still gets one slot vec per
    // configured universe.
    while out.len() < universe_count {
        out.push(Vec::new());
    }
    out
}

/// Builder for [`SacnAdapter`]. `universes`, `target_host`, and a name
/// are required; everything else has a sensible default.
#[derive(Debug)]
pub struct SacnAdapterBuilder {
    name: String,
    target_host: std::net::IpAddr,
    target_port: u16,
    universes: Vec<u16>,
    priority: u8,
    native_rate: Duration,
    source_name: String,
    cid: Option<Uuid>,
    zones: Vec<Zone>,
    capabilities: Vec<Capability>,
}

impl SacnAdapterBuilder {
    /// New builder for a unicast target. For multicast use the
    /// per-universe address returned by [`crate::packet::multicast_v4`]
    /// — at M1 we only support unicast.
    #[must_use]
    pub fn new(name: impl Into<String>, target_host: std::net::IpAddr) -> Self {
        Self {
            name: name.into(),
            target_host,
            target_port: SACN_PORT,
            universes: vec![1],
            priority: DEFAULT_PRIORITY,
            native_rate: DEFAULT_NATIVE_RATE,
            source_name: "audiophore".to_string(),
            cid: None,
            zones: Vec::new(),
            capabilities: vec![Capability::Rgb, Capability::PixelStrip],
        }
    }

    /// Set the universe list. Must be non-empty and at most
    /// [`MAX_UNIVERSES`] entries; every entry must be in `1..=63_999`.
    #[must_use]
    pub fn universes(mut self, universes: Vec<u16>) -> Self {
        self.universes = universes;
        self
    }

    /// Override the UDP destination port (default: 5568, the IANA
    /// well-known E1.31 port).
    #[must_use]
    pub fn target_port(mut self, port: u16) -> Self {
        self.target_port = port;
        self
    }

    /// Override DMX priority. Must be `0..=200`.
    #[must_use]
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Override the engine-facing native render rate (default: 44 Hz).
    #[must_use]
    pub fn native_rate(mut self, rate: Duration) -> Self {
        self.native_rate = rate;
        self
    }

    /// Override the source name embedded in every packet (E1.31
    /// framing layer §6.2.2). Truncated to 63 bytes + null.
    #[must_use]
    pub fn source_name(mut self, name: impl Into<String>) -> Self {
        self.source_name = name.into();
        self
    }

    /// Override the CID. Default is a fresh `Uuid::new_v4()` per
    /// adapter instance; override to persist a stable CID across
    /// restarts if your receiver tracks sources by CID.
    #[must_use]
    pub fn cid(mut self, cid: Uuid) -> Self {
        self.cid = Some(cid);
        self
    }

    /// Zones this adapter drives — passed straight through to
    /// [`OutputAdapter::zones`].
    #[must_use]
    pub fn zones(mut self, zones: Vec<Zone>) -> Self {
        self.zones = zones;
        self
    }

    /// Override the capability set. Default is
    /// `[Capability::Rgb, Capability::PixelStrip]`.
    #[must_use]
    pub fn capabilities(mut self, caps: Vec<Capability>) -> Self {
        self.capabilities = caps;
        self
    }

    /// Build the adapter, validating configuration.
    ///
    /// # Errors
    /// Returns [`AdapterError::Protocol`] if the configuration is
    /// invalid (empty universe list, universe out of range, too many
    /// universes, priority out of range).
    pub fn build(self) -> Result<SacnAdapter, AdapterError> {
        if self.universes.is_empty() {
            return Err(AdapterError::Protocol(
                "universe list must not be empty".into(),
            ));
        }
        if self.universes.len() > MAX_UNIVERSES {
            return Err(AdapterError::Protocol(format!(
                "too many universes ({}); max is {}",
                self.universes.len(),
                MAX_UNIVERSES
            )));
        }
        for u in &self.universes {
            if !(1..=63_999).contains(u) {
                return Err(AdapterError::Protocol(format!(
                    "universe {u} out of range 1..=63_999"
                )));
            }
        }
        if self.priority > 200 {
            return Err(AdapterError::Protocol(format!(
                "priority {} out of range 0..=200",
                self.priority
            )));
        }

        let builder = match self.cid {
            Some(cid) => PacketBuilder::with_cid(cid, &self.source_name),
            None => PacketBuilder::new(&self.source_name),
        };

        Ok(SacnAdapter {
            name: self.name,
            capabilities: self.capabilities,
            zones: self.zones,
            native_rate: self.native_rate,
            target: SocketAddr::new(self.target_host, self.target_port),
            universes: self.universes,
            priority: self.priority,
            builder,
            socket: None,
            sequence: 0,
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "test assertions"
)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    use audiophore_core::{ResolvedFrame, Rgb, ZoneId, ZonePayload};

    use super::*;

    fn linear_rgb(r: f32, g: f32, b: f32) -> Rgb {
        Rgb { r, g, b }
    }

    fn black() -> Rgb {
        linear_rgb(0.0, 0.0, 0.0)
    }

    #[test]
    fn partitions_300_pixels_into_170_plus_130() {
        // 300-pixel WS2815 strip: HARDWARE.md says this spans 2
        // universes, 170 + 130 pixels.
        let pixels = vec![black(); 300];
        let chunks = partition_rgb(&pixels, 2);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 170 * 3); // 510 slot bytes
        assert_eq!(chunks[1].len(), 130 * 3); // 390 slot bytes
    }

    #[test]
    fn partitions_170_pixels_into_one_universe() {
        let pixels = vec![black(); 170];
        let chunks = partition_rgb(&pixels, 2);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 170 * 3);
        // Second universe gets nothing for this tick.
        assert_eq!(chunks[1].len(), 0);
    }

    #[test]
    fn partitions_zero_universes_returns_empty() {
        let pixels = vec![black(); 10];
        let chunks = partition_rgb(&pixels, 0);
        assert!(chunks.is_empty());
    }

    #[test]
    fn partition_applies_gamma_to_pure_red() {
        let pixels = vec![linear_rgb(1.0, 0.0, 0.0); 1];
        let chunks = partition_rgb(&pixels, 1);
        // (255, 0, 0) — gamma_encode(1.0) = 255, gamma_encode(0.0) = 0.
        assert_eq!(chunks[0], vec![0xff, 0x00, 0x00]);
    }

    #[test]
    fn build_rejects_empty_universe_list() {
        let err = SacnAdapterBuilder::new("test", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .universes(vec![])
            .build()
            .unwrap_err();
        assert!(matches!(err, AdapterError::Protocol(_)));
    }

    #[test]
    fn build_rejects_too_many_universes() {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "MAX_UNIVERSES is 64; fits in u16"
        )]
        let limit = MAX_UNIVERSES as u16 + 1;
        let universes: Vec<u16> = (1..=limit).collect();
        let err = SacnAdapterBuilder::new("test", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .universes(universes)
            .build()
            .unwrap_err();
        assert!(matches!(err, AdapterError::Protocol(_)));
    }

    #[test]
    fn build_rejects_universe_out_of_range() {
        let err = SacnAdapterBuilder::new("test", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .universes(vec![0])
            .build()
            .unwrap_err();
        assert!(matches!(err, AdapterError::Protocol(_)));
        let err = SacnAdapterBuilder::new("test", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .universes(vec![64_000])
            .build()
            .unwrap_err();
        assert!(matches!(err, AdapterError::Protocol(_)));
    }

    #[test]
    fn build_rejects_priority_above_200() {
        let err = SacnAdapterBuilder::new("test", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .priority(201)
            .build()
            .unwrap_err();
        assert!(matches!(err, AdapterError::Protocol(_)));
    }

    #[tokio::test]
    async fn render_to_loopback_round_trip_two_universes() {
        // End-to-end: stand up a UDP receiver, render a 300-pixel
        // frame, assert two packets arrive with universes 1 and 2,
        // sequence 0 in both (sequence increments per tick, not per
        // universe).
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = receiver.local_addr().unwrap();

        let mut adapter = SacnAdapterBuilder::new("test", recv_addr.ip())
            .target_port(recv_addr.port())
            .universes(vec![1, 2])
            .source_name("audiophore-test")
            .build()
            .unwrap();
        adapter.connect().await.unwrap();

        let mut zones = HashMap::new();
        zones.insert(
            ZoneId("strip".to_string()),
            ZonePayload::Pixels(vec![linear_rgb(1.0, 0.0, 0.0); 300]),
        );
        let frame = ResolvedFrame {
            tick: 0,
            t: 0.0,
            zones,
        };
        adapter.render(&frame).await.unwrap();

        // Two packets expected (one per universe).
        let mut buf = [0_u8; 700];
        let mut packets: Vec<Vec<u8>> = Vec::new();
        for _ in 0..2 {
            let (n, _peer) =
                tokio::time::timeout(Duration::from_millis(500), receiver.recv_from(&mut buf))
                    .await
                    .unwrap()
                    .unwrap();
            packets.push(buf[..n].to_vec());
        }

        // Universe-1 packet: 170 pixels * 3 + headers = 38 (root) + 77
        // (framing) + 10 (DMP header) + 1 (start code) + 510 (slots) = 636.
        // Universe-2 packet: 130 pixels * 3 + headers = 38 + 77 + 10 +
        // 1 + 390 = 516.
        let lens: Vec<usize> = packets.iter().map(Vec::len).collect();
        assert!(
            lens.contains(&636) && lens.contains(&516),
            "expected packet lengths 636 and 516, got {lens:?}"
        );

        // Universe field is bytes 113..115; sequence is byte 111.
        for pkt in &packets {
            assert_eq!(pkt[111], 0, "sequence should be 0 on the first tick");
            let universe = u16::from_be_bytes([pkt[113], pkt[114]]);
            assert!(
                universe == 1 || universe == 2,
                "unexpected universe {universe}"
            );
        }

        // CID stable across both packets.
        let cid_a = &packets[0][22..38];
        let cid_b = &packets[1][22..38];
        assert_eq!(cid_a, cid_b);
    }

    #[tokio::test]
    async fn sequence_wraps_at_u8_max() {
        // Render 257 times; the 257th packet must carry sequence 0
        // again. Use a single-universe adapter so each render is
        // exactly one packet.
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = receiver.local_addr().unwrap();

        let mut adapter = SacnAdapterBuilder::new("test", recv_addr.ip())
            .target_port(recv_addr.port())
            .universes(vec![1])
            .source_name("audiophore-test")
            .build()
            .unwrap();
        adapter.connect().await.unwrap();

        let mut zones = HashMap::new();
        zones.insert(
            ZoneId("strip".to_string()),
            ZonePayload::Pixels(vec![linear_rgb(0.5, 0.5, 0.5); 1]),
        );
        let frame = ResolvedFrame {
            tick: 0,
            t: 0.0,
            zones,
        };

        let mut buf = [0_u8; 200];
        for expected_seq in 0_u32..=257 {
            adapter.render(&frame).await.unwrap();
            let (n, _peer) =
                tokio::time::timeout(Duration::from_millis(500), receiver.recv_from(&mut buf))
                    .await
                    .unwrap()
                    .unwrap();
            let got = buf[..n][111];
            #[allow(
                clippy::cast_possible_truncation,
                reason = "intentional u32->u8 truncation to test wrap"
            )]
            let want = expected_seq as u8;
            assert_eq!(
                got, want,
                "tick {expected_seq}: expected seq {want}, got {got}"
            );
        }
        // After the 258th render (indices 0..=257), next_sequence
        // should be 258 mod 256 = 2.
        assert_eq!(adapter.next_sequence(), 2);
    }

    #[tokio::test]
    async fn render_with_no_pixel_payload_is_noop() {
        // Frame with only a fixture payload — adapter should not error
        // and should not send anything.
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let recv_addr = receiver.local_addr().unwrap();
        let mut adapter = SacnAdapterBuilder::new("test", recv_addr.ip())
            .target_port(recv_addr.port())
            .universes(vec![1])
            .build()
            .unwrap();
        adapter.connect().await.unwrap();

        let mut zones = HashMap::new();
        zones.insert(
            ZoneId("bulb".to_string()),
            ZonePayload::Fixture(linear_rgb(1.0, 1.0, 1.0), 0.5),
        );
        let frame = ResolvedFrame {
            tick: 0,
            t: 0.0,
            zones,
        };
        adapter.render(&frame).await.unwrap();

        // Confirm nothing arrived.
        let mut buf = [0_u8; 100];
        let res =
            tokio::time::timeout(Duration::from_millis(50), receiver.recv_from(&mut buf)).await;
        assert!(res.is_err(), "expected timeout, but received a packet");
    }

    #[tokio::test]
    async fn render_before_connect_returns_connect_error() {
        let mut adapter = SacnAdapterBuilder::new("test", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .universes(vec![1])
            .build()
            .unwrap();
        let mut zones = HashMap::new();
        zones.insert(
            ZoneId("strip".to_string()),
            ZonePayload::Pixels(vec![linear_rgb(0.0, 0.0, 0.0); 1]),
        );
        let frame = ResolvedFrame {
            tick: 0,
            t: 0.0,
            zones,
        };
        let err = adapter.render(&frame).await.unwrap_err();
        assert!(matches!(err, AdapterError::Connect(_)));
    }

    #[tokio::test]
    async fn disconnect_is_idempotent() {
        let mut adapter = SacnAdapterBuilder::new("test", IpAddr::V4(Ipv4Addr::LOCALHOST))
            .universes(vec![1])
            .build()
            .unwrap();
        adapter.connect().await.unwrap();
        adapter.disconnect().await.unwrap();
        adapter.disconnect().await.unwrap();
    }
}
