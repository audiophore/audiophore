//! `audiophore-audio`: OSC ingress and audio frame production.
//!
//! Accepts Synesthesia's OSC stream on UDP, decodes via [`rosc`],
//! dispatches by address, and normalizes the values into
//! [`audiophore_core::AudioFrame`] instances at the engine's clock.
//!
//! The 5 wire-dump TODOs from `planning/SYNESTHESIA_OSC.md` are coded
//! against working assumptions; each assumption is marked in-source with
//! `WIRE-DUMP TODO (per SYNESTHESIA_OSC.md):` so a `grep` finds every
//! spot a future B.1 capture might flip.
//!
//! ## Public surface
//!
//! - [`Listener`] — async UDP listener; produces [`AudioFrame`]s via
//!   [`Listener::recv`]. Constructed with a bind address; owns the
//!   `tokio::net::UdpSocket`.
//! - [`AudioFrameBuilder`] — the per-tick accumulator the listener
//!   feeds. Exposed for tests / monitoring tools that want to feed
//!   synthesized OSC packets without binding a socket.
//! - [`dispatch_packet`] — low-level entry point: hand it raw OSC bytes
//!   and a builder and it will mutate the builder. Used by the
//!   eventual `audiophore monitor` CLI and the unit tests here.

pub mod frame;

use std::net::SocketAddr;

use audiophore_core::AudioFrame;
use rosc::{OscMessage, OscPacket, OscType};
use thiserror::Error;
use tokio::net::{ToSocketAddrs, UdpSocket};

pub use crate::frame::{AudioFrameBuilder, Band};

/// Maximum UDP datagram payload we'll accept in one `recv_from`.
///
/// Sized comfortably above the practical OSC bundle Synesthesia emits
/// per tick (~hundreds of bytes) without being so large the per-recv
/// stack pressure matters.
pub const RECV_BUFFER_BYTES: usize = 64 * 1024;

/// Errors produced while ingesting OSC.
#[derive(Debug, Error)]
pub enum IngressError {
    /// Underlying socket I/O failed.
    #[error("UDP I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// `rosc` failed to decode the datagram.
    #[error("OSC decode error: {0:?}")]
    Decode(rosc::OscError),
}

impl From<rosc::OscError> for IngressError {
    fn from(value: rosc::OscError) -> Self {
        Self::Decode(value)
    }
}

/// Async UDP listener for Synesthesia OSC.
///
/// Construct with [`Listener::bind`], then drive with
/// [`Listener::recv`]. Each `recv` returns one [`AudioFrame`] — a
/// snapshot of every OSC value observed since the previous `recv`.
///
/// One Synesthesia tick typically arrives as a bundle of ~15–25
/// messages; we accept either a single message or a bundle per UDP
/// datagram and snapshot once per datagram. That matches Synesthesia's
/// ~60 Hz output cadence and gives the engine a predictable
/// one-frame-per-datagram contract.
pub struct Listener {
    socket: UdpSocket,
    builder: AudioFrameBuilder,
    buf: Vec<u8>,
}

impl Listener {
    /// Bind a UDP socket on `addr` and return a listener ready to
    /// receive OSC.
    ///
    /// # Errors
    /// Returns [`IngressError::Io`] if the socket bind fails (e.g. port
    /// already in use).
    pub async fn bind<A: ToSocketAddrs>(addr: A) -> Result<Self, IngressError> {
        let socket = UdpSocket::bind(addr).await?;
        Ok(Self {
            socket,
            builder: AudioFrameBuilder::new(),
            buf: vec![0_u8; RECV_BUFFER_BYTES],
        })
    }

    /// Local address the socket is bound to.
    ///
    /// # Errors
    /// Returns [`IngressError::Io`] if querying the socket fails.
    pub fn local_addr(&self) -> Result<SocketAddr, IngressError> {
        self.socket.local_addr().map_err(Into::into)
    }

    /// Receive one OSC datagram and return the resulting
    /// [`AudioFrame`] snapshot.
    ///
    /// Bundles and single messages are both supported; unknown
    /// addresses and malformed values are logged and skipped rather
    /// than turned into errors.
    ///
    /// # Errors
    /// Returns [`IngressError::Io`] if the underlying socket fails or
    /// [`IngressError::Decode`] if `rosc` rejects the datagram.
    pub async fn recv(&mut self) -> Result<AudioFrame, IngressError> {
        let (n, peer) = self.socket.recv_from(&mut self.buf).await?;
        tracing::trace!(bytes = n, %peer, "OSC datagram received");
        dispatch_packet(&self.buf[..n], &mut self.builder)?;
        Ok(self.builder.snapshot())
    }

    /// Borrow the underlying builder for tests / monitoring surfaces
    /// that want to inspect mid-tick state without consuming a frame.
    #[must_use]
    pub fn builder(&self) -> &AudioFrameBuilder {
        &self.builder
    }
}

/// Decode a raw OSC datagram and apply each contained message to
/// `builder`.
///
/// Unknown OSC addresses and per-message type-mismatches are logged at
/// `debug`/`trace` level and skipped; only a top-level
/// [`rosc::OscError`] surfaces as an error.
///
/// # Errors
/// Returns [`IngressError::Decode`] if `rosc::decoder::decode_udp`
/// rejects the bytes.
pub fn dispatch_packet(bytes: &[u8], builder: &mut AudioFrameBuilder) -> Result<(), IngressError> {
    let (_, packet) = rosc::decoder::decode_udp(bytes)?;
    apply_packet(packet, builder);
    Ok(())
}

fn apply_packet(packet: OscPacket, builder: &mut AudioFrameBuilder) {
    match packet {
        OscPacket::Message(msg) => apply_message(&msg, builder),
        OscPacket::Bundle(bundle) => {
            for inner in bundle.content {
                apply_packet(inner, builder);
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "single dispatch table is clearer than splitting the address arms across helpers"
)]
fn apply_message(msg: &OscMessage, builder: &mut AudioFrameBuilder) {
    match msg.addr.as_str() {
        // ── Levels ──────────────────────────────────────────────────
        "/audio/level" => with_unit(msg, |v| builder.set_level(Band::Whole, v)),
        "/audio/bassLevel" => with_unit(msg, |v| builder.set_level(Band::Bass, v)),
        "/audio/midLevel" => with_unit(msg, |v| builder.set_level(Band::Mid, v)),
        "/audio/midHighLevel" => with_unit(msg, |v| builder.set_level(Band::MidHigh, v)),
        "/audio/highLevel" => with_unit(msg, |v| builder.set_level(Band::High, v)),

        // ── Hits ────────────────────────────────────────────────────
        "/audio/hits" => with_unit(msg, |v| builder.set_hit(Band::Whole, v)),
        "/audio/bassHits" => with_unit(msg, |v| builder.set_hit(Band::Bass, v)),
        "/audio/midHits" => with_unit(msg, |v| builder.set_hit(Band::Mid, v)),
        "/audio/midHighHits" => with_unit(msg, |v| builder.set_hit(Band::MidHigh, v)),
        "/audio/highHits" => with_unit(msg, |v| builder.set_hit(Band::High, v)),

        // ── Presence ────────────────────────────────────────────────
        "/audio/presence" => with_unit(msg, |v| builder.set_presence(Band::Whole, v)),
        "/audio/bassPresence" => with_unit(msg, |v| builder.set_presence(Band::Bass, v)),
        "/audio/midPresence" => with_unit(msg, |v| builder.set_presence(Band::Mid, v)),
        "/audio/midHighPresence" => with_unit(msg, |v| builder.set_presence(Band::MidHigh, v)),
        "/audio/highPresence" => with_unit(msg, |v| builder.set_presence(Band::High, v)),

        // ── Beat detection ──────────────────────────────────────────
        "/audio/onBeat" => {
            // WIRE-DUMP TODO (per SYNESTHESIA_OSC.md): assuming
            // Synesthesia emits `i 1` on the beat tick and `i 0`
            // otherwise (OSC 1.0 has no native bool typetag). Also
            // accept `T`/`F` in case the build is OSC 1.1+.
            if let Some(b) = first_bool_like(msg) {
                builder.set_on_beat(b);
            } else {
                tracing::debug!(addr = msg.addr, ?msg.args, "onBeat arg shape unrecognized");
            }
        }
        "/audio/toggleOnBeat" => {
            // WIRE-DUMP TODO (per SYNESTHESIA_OSC.md): same typetag
            // question as onBeat; toggles 0↔1 each beat. We don't
            // surface this field on AudioFrame today — log only.
            tracing::trace!(addr = msg.addr, ?msg.args, "toggleOnBeat (not consumed)");
        }
        "/audio/beatTime" => {
            // WIRE-DUMP TODO (per SYNESTHESIA_OSC.md): working
            // assumption is single `f` arg = seconds since last beat.
            // Until B.1 confirms semantics, derive nothing; just log.
            tracing::trace!(addr = msg.addr, ?msg.args, "beatTime (semantics unconfirmed)");
        }

        // ── BPM ─────────────────────────────────────────────────────
        "/audio/bpm" => with_float(msg, |v| builder.set_bpm(v)),
        "/audio/bpmConfidence" => with_unit(msg, |v| builder.set_bpm_confidence(v)),
        "/audio/bpmSin" | "/audio/bpmTri" => {
            // Synesthesia-derived oscillators; modes can recompute, so
            // we deliberately skip rather than store them on AudioFrame.
            tracing::trace!(addr = msg.addr, "bpm oscillator (skipped — derivable)");
        }

        // ── Macro features ──────────────────────────────────────────
        "/audio/intensity" => {
            // WIRE-DUMP TODO (per SYNESTHESIA_OSC.md): treating as a
            // unit-clamped accumulator (0.0..=1.0). Decay shape and
            // saturation behaviour at song end need live verification.
            with_unit(msg, |v| builder.set_intensity(v));
        }
        "/audio/fadeInOut" => with_unit(msg, |v| builder.set_fade(v)),

        // ── Time uniforms — intentionally skipped ───────────────────
        "/audio/time" | "/audio/bassTime" | "/audio/midTime" | "/audio/midHighTime"
        | "/audio/highTime" | "/audio/curvedTime" => {
            tracing::trace!(
                addr = msg.addr,
                "time uniform (skipped per SYNESTHESIA_OSC.md)"
            );
        }

        // ── Anything else ───────────────────────────────────────────
        other => {
            // WIRE-DUMP TODO (per SYNESTHESIA_OSC.md):
            // /controls/meta/{low,high}_color etc. land here today.
            // Scene-state plumbing is out of scope for A.4; A.x will
            // route these into a SceneState builder. Log only.
            tracing::debug!(addr = other, ?msg.args, "unhandled OSC address");
        }
    }
}

/// Pull a single `f32` out of `msg.args`, accepting `f` (Float) and
/// silently coercing `i` (Int) / `d` (Double) when present.
fn first_float(msg: &OscMessage) -> Option<f32> {
    let arg = msg.args.first()?;
    match *arg {
        OscType::Float(v) => Some(v),
        OscType::Double(v) => {
            // Truncation is intentional: AudioFrame fields are f32.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "AudioFrame field is f32; double-precision input is rare and downcast is intentional"
            )]
            Some(v as f32)
        }
        OscType::Int(v) => {
            // i32 → f32 loses precision above 2^24, but every audio
            // field that ever flows through this fallback is a tiny
            // unit value (0/1 for booleans, 0..=220 for BPM); the
            // precision loss is unreachable in practice.
            #[allow(
                clippy::cast_precision_loss,
                reason = "audio fields stay well inside f32 mantissa range"
            )]
            Some(v as f32)
        }
        _ => None,
    }
}

fn with_float(msg: &OscMessage, mut setter: impl FnMut(f32)) {
    if let Some(v) = first_float(msg) {
        setter(v);
    } else {
        tracing::debug!(addr = msg.addr, ?msg.args, "expected float arg, got something else");
    }
}

fn with_unit(msg: &OscMessage, setter: impl FnMut(f32)) {
    // Clamping happens inside the builder setters; this wrapper exists
    // for symmetry / future divergence if we ever need a different
    // arg-extraction policy for unit-range fields.
    with_float(msg, setter);
}

/// Coerce an arg into bool, accepting `T`/`F` (OSC 1.1) and `i 1`/`i 0`
/// (the OSC-1.0 idiom). Other shapes return `None`.
fn first_bool_like(msg: &OscMessage) -> Option<bool> {
    let arg = msg.args.first()?;
    match *arg {
        OscType::Bool(b) => Some(b),
        OscType::Int(v) => Some(v != 0),
        OscType::Float(v) => Some(v.abs() > f32::EPSILON),
        _ => None,
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "tests panic on assertion failure by design; expect/unwrap make the failure message concrete"
)]
mod tests {
    use super::{
        AudioFrameBuilder, IngressError, Listener, dispatch_packet, first_bool_like, first_float,
    };
    use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType, encoder};

    fn encode_msg(addr: &str, args: Vec<OscType>) -> Vec<u8> {
        let packet = OscPacket::Message(OscMessage {
            addr: addr.to_string(),
            args,
        });
        encoder::encode(&packet).expect("encode test packet")
    }

    fn encode_bundle(msgs: Vec<(&str, Vec<OscType>)>) -> Vec<u8> {
        let content = msgs
            .into_iter()
            .map(|(addr, args)| {
                OscPacket::Message(OscMessage {
                    addr: addr.to_string(),
                    args,
                })
            })
            .collect();
        let packet = OscPacket::Bundle(OscBundle {
            timetag: OscTime::from((0, 1)),
            content,
        });
        encoder::encode(&packet).expect("encode test bundle")
    }

    #[test]
    fn dispatch_bpm_message_sets_bpm() {
        let bytes = encode_msg("/audio/bpm", vec![OscType::Float(128.5)]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        let f = b.snapshot();
        assert!((f.bpm - 128.5).abs() < 1e-4);
    }

    #[test]
    fn dispatch_levels_and_hits() {
        let bytes = encode_bundle(vec![
            ("/audio/bassLevel", vec![OscType::Float(0.75)]),
            ("/audio/midLevel", vec![OscType::Float(0.4)]),
            ("/audio/bassHits", vec![OscType::Float(0.9)]),
        ]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        let f = b.snapshot();
        assert!((f.levels.bass - 0.75).abs() < 1e-4);
        assert!((f.levels.mid - 0.4).abs() < 1e-4);
        assert!((f.hits.bass - 0.9).abs() < 1e-4);
    }

    #[test]
    fn dispatch_on_beat_int_one_sets_true() {
        let bytes = encode_msg("/audio/onBeat", vec![OscType::Int(1)]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        assert!(b.snapshot().on_beat);
    }

    #[test]
    fn dispatch_on_beat_int_zero_sets_false() {
        let bytes = encode_msg("/audio/onBeat", vec![OscType::Int(0)]);
        let mut b = AudioFrameBuilder::new();
        b.mark_on_beat();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        assert!(!b.snapshot().on_beat);
    }

    #[test]
    fn dispatch_on_beat_bool_typetag_supported() {
        let bytes = encode_msg("/audio/onBeat", vec![OscType::Bool(true)]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        assert!(b.snapshot().on_beat);
    }

    #[test]
    fn dispatch_intensity_and_fade() {
        let bytes = encode_bundle(vec![
            ("/audio/intensity", vec![OscType::Float(0.42)]),
            ("/audio/fadeInOut", vec![OscType::Float(0.88)]),
        ]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        let f = b.snapshot();
        assert!((f.intensity - 0.42).abs() < 1e-4);
        assert!((f.fade - 0.88).abs() < 1e-4);
    }

    #[test]
    fn out_of_range_bpm_clamps_without_panic() {
        let bytes = encode_msg("/audio/bpm", vec![OscType::Float(500.0)]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        let f = b.snapshot();
        assert!((f.bpm - super::frame::BPM_MAX).abs() < f32::EPSILON);
    }

    #[test]
    fn unknown_address_is_skipped_silently() {
        let bytes = encode_msg("/totally/made/up", vec![OscType::Float(1.0)]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        // No assertion about state — just must not panic / error.
        let _ = b.snapshot();
    }

    #[test]
    fn malformed_packet_returns_decode_error_no_panic() {
        let bytes = b"not a valid osc packet at all".to_vec();
        let mut b = AudioFrameBuilder::new();
        let err = dispatch_packet(&bytes, &mut b).expect_err("must fail");
        assert!(matches!(err, IngressError::Decode(_)));
    }

    #[test]
    fn missing_arg_is_skipped_without_panic() {
        let bytes = encode_msg("/audio/bpm", vec![]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        // No bpm change; default still applies.
        let f = b.snapshot();
        assert!((f.bpm - 120.0).abs() < f32::EPSILON);
    }

    #[test]
    fn wrong_arg_type_is_skipped_without_panic() {
        let bytes = encode_msg("/audio/bpm", vec![OscType::String("nope".into())]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        let f = b.snapshot();
        assert!((f.bpm - 120.0).abs() < f32::EPSILON);
    }

    #[test]
    fn first_float_coerces_int_and_double() {
        let m = |arg: OscType| OscMessage {
            addr: "/x".into(),
            args: vec![arg],
        };
        assert!((first_float(&m(OscType::Int(7))).expect("some") - 7.0).abs() < f32::EPSILON);
        assert!((first_float(&m(OscType::Double(3.5))).expect("some") - 3.5).abs() < 1e-6);
        assert!(first_float(&m(OscType::String("nope".into()))).is_none());
    }

    #[test]
    fn first_bool_like_handles_all_three_shapes() {
        let m = |arg: OscType| OscMessage {
            addr: "/x".into(),
            args: vec![arg],
        };
        assert_eq!(first_bool_like(&m(OscType::Bool(true))), Some(true));
        assert_eq!(first_bool_like(&m(OscType::Int(0))), Some(false));
        assert_eq!(first_bool_like(&m(OscType::Int(1))), Some(true));
        assert_eq!(first_bool_like(&m(OscType::Float(0.0))), Some(false));
        assert_eq!(first_bool_like(&m(OscType::Float(0.5))), Some(true));
        assert_eq!(first_bool_like(&m(OscType::String("x".into()))), None);
    }

    #[tokio::test]
    async fn listener_round_trip_over_loopback() {
        let listener = Listener::bind("127.0.0.1:0").await.expect("bind");
        let bound = listener.local_addr().expect("local addr");

        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind sender");
        let bytes = encode_bundle(vec![
            ("/audio/bpm", vec![OscType::Float(140.0)]),
            ("/audio/bassLevel", vec![OscType::Float(0.6)]),
            ("/audio/onBeat", vec![OscType::Int(1)]),
        ]);
        sender.send_to(&bytes, bound).await.expect("send");

        let mut listener = listener;
        let frame = listener.recv().await.expect("recv");
        assert!((frame.bpm - 140.0).abs() < 1e-4);
        assert!((frame.levels.bass - 0.6).abs() < 1e-4);
        assert!(frame.on_beat);
        // Subsequent snapshot with no new data preserves continuous
        // fields but clears on_beat.
        let mirror = listener.builder().clone();
        assert!((mirror.bpm() - 140.0).abs() < 1e-4);
    }
}
