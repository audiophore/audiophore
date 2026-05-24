//! `audiophore-audio`: OSC ingress and audio frame production.
//!
//! Accepts Synesthesia's OSC stream on UDP, decodes via [`rosc`],
//! dispatches by address, and normalizes the values into
//! [`audiophore_core::AudioFrame`] instances at the engine's clock.
//!
//! Address matching and value semantics were verified against a live
//! Synesthesia wire dump (2026-05-21, Pro `p.1.25.2.184`) — see
//! `planning/SYNESTHESIA_OSC.md`. Every audio variable arrives as a
//! single float on a nested, lowercase `/audio/<category>/<name>`
//! address.
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
        let (frame, _packet, _peer) = self.recv_with_packet().await?;
        Ok(frame)
    }

    /// Receive one OSC datagram and return the resulting
    /// [`AudioFrame`] snapshot **plus** the decoded [`OscPacket`] and
    /// the sender's address.
    ///
    /// Decodes the datagram exactly once. Inspection surfaces — e.g.
    /// `audiophore monitor --raw` — use this to print the raw OSC
    /// structure without re-decoding what [`recv`](Self::recv) already
    /// parsed.
    ///
    /// # Errors
    /// Returns [`IngressError::Io`] if the underlying socket fails or
    /// [`IngressError::Decode`] if `rosc` rejects the datagram.
    pub async fn recv_with_packet(
        &mut self,
    ) -> Result<(AudioFrame, OscPacket, SocketAddr), IngressError> {
        let (n, peer) = self.socket.recv_from(&mut self.buf).await?;
        tracing::trace!(bytes = n, %peer, "OSC datagram received");
        let (_, packet) = rosc::decoder::decode_udp(&self.buf[..n])?;
        apply_packet(&packet, &mut self.builder);
        Ok((self.builder.snapshot(), packet, peer))
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
    apply_packet(&packet, builder);
    Ok(())
}

fn apply_packet(packet: &OscPacket, builder: &mut AudioFrameBuilder) {
    match packet {
        OscPacket::Message(msg) => apply_message(msg, builder),
        OscPacket::Bundle(bundle) => {
            for inner in &bundle.content {
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
        // ── Levels (/audio/level/<band>) ────────────────────────────
        "/audio/level/all" => with_unit(msg, |v| builder.set_level(Band::Whole, v)),
        "/audio/level/bass" => with_unit(msg, |v| builder.set_level(Band::Bass, v)),
        "/audio/level/mid" => with_unit(msg, |v| builder.set_level(Band::Mid, v)),
        "/audio/level/midhigh" => with_unit(msg, |v| builder.set_level(Band::MidHigh, v)),
        "/audio/level/high" => with_unit(msg, |v| builder.set_level(Band::High, v)),
        // `level/raw` is the unsmoothed whole-band level; we prefer the
        // smoothed `all`, so ignore it.
        "/audio/level/raw" => {}

        // ── Hits (/audio/hits/<band>) ───────────────────────────────
        "/audio/hits/all" => with_unit(msg, |v| builder.set_hit(Band::Whole, v)),
        "/audio/hits/bass" => with_unit(msg, |v| builder.set_hit(Band::Bass, v)),
        "/audio/hits/mid" => with_unit(msg, |v| builder.set_hit(Band::Mid, v)),
        "/audio/hits/midhigh" => with_unit(msg, |v| builder.set_hit(Band::MidHigh, v)),
        "/audio/hits/high" => with_unit(msg, |v| builder.set_hit(Band::High, v)),

        // ── Presence (/audio/presence/<band>) ───────────────────────
        "/audio/presence/all" => with_unit(msg, |v| builder.set_presence(Band::Whole, v)),
        "/audio/presence/bass" => with_unit(msg, |v| builder.set_presence(Band::Bass, v)),
        "/audio/presence/mid" => with_unit(msg, |v| builder.set_presence(Band::Mid, v)),
        "/audio/presence/midhigh" => with_unit(msg, |v| builder.set_presence(Band::MidHigh, v)),
        "/audio/presence/high" => with_unit(msg, |v| builder.set_presence(Band::High, v)),

        // ── Beat detection ──────────────────────────────────────────
        // `onbeat` is a 0.0..=1.0 envelope (spikes on a beat, decays);
        // consumed directly so the beat-flash decays naturally.
        "/audio/beat/onbeat" => with_unit(msg, |v| builder.set_on_beat(v)),
        // `randomonbeat` (random float per beat) and `beattime`
        // (monotonic counter) aren't surfaced on AudioFrame today.
        "/audio/beat/randomonbeat" | "/audio/beat/beattime" => {
            tracing::trace!(addr = msg.addr, ?msg.args, "beat aux field (not consumed)");
        }

        // ── BPM ─────────────────────────────────────────────────────
        "/audio/bpm/bpm" => with_float(msg, |v| builder.set_bpm(v)),
        "/audio/bpm/bpmconfidence" => with_unit(msg, |v| builder.set_bpm_confidence(v)),
        // Derived oscillators (`bpmsin*`, `bpmtri*`) + the `bpmtwitcher`
        // counter; modes can recompute these, so skip rather than store.
        addr if addr.starts_with("/audio/bpm/") => {
            tracing::trace!(addr = msg.addr, "bpm aux field (skipped — derivable)");
        }

        // ── Macro features ──────────────────────────────────────────
        // Saturating 0.0..=1.0 accumulator (verified: floor ~0.25,
        // saturates at 1.0).
        "/audio/energy/intensity" => with_unit(msg, |v| builder.set_intensity(v)),

        // ── Time uniforms — intentionally skipped ───────────────────
        // `/audio/time/<band>` clocks are unbounded; modes derive their
        // own from BPM.
        addr if addr.starts_with("/audio/time/") => {
            tracing::trace!(
                addr = msg.addr,
                "time uniform (skipped per SYNESTHESIA_OSC.md)"
            );
        }

        // ── Anything else ───────────────────────────────────────────
        other => {
            // Scene / meta state (e.g. `/controls/meta/*`) lands here
            // today; a later milestone routes it into a SceneState
            // builder. Log only.
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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "tests panic on assertion failure by design; expect/unwrap make the failure message concrete"
)]
mod tests {
    use super::{AudioFrameBuilder, IngressError, Listener, dispatch_packet, first_float};
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
        let bytes = encode_msg("/audio/bpm/bpm", vec![OscType::Float(128.5)]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        let f = b.snapshot();
        assert!((f.bpm - 128.5).abs() < 1e-4);
    }

    #[test]
    fn dispatch_levels_and_hits() {
        let bytes = encode_bundle(vec![
            ("/audio/level/bass", vec![OscType::Float(0.75)]),
            ("/audio/level/mid", vec![OscType::Float(0.4)]),
            ("/audio/hits/bass", vec![OscType::Float(0.9)]),
        ]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        let f = b.snapshot();
        assert!((f.levels.bass - 0.75).abs() < 1e-4);
        assert!((f.levels.mid - 0.4).abs() < 1e-4);
        assert!((f.hits.bass - 0.9).abs() < 1e-4);
    }

    #[test]
    fn dispatch_onbeat_sets_envelope() {
        let bytes = encode_msg("/audio/beat/onbeat", vec![OscType::Float(0.8)]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        assert!((b.snapshot().on_beat - 0.8).abs() < 1e-4);
    }

    #[test]
    fn dispatch_onbeat_clamps_above_one() {
        let bytes = encode_msg("/audio/beat/onbeat", vec![OscType::Float(2.0)]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        assert!((b.snapshot().on_beat - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn dispatch_intensity() {
        let bytes = encode_msg("/audio/energy/intensity", vec![OscType::Float(0.42)]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        let f = b.snapshot();
        assert!((f.intensity - 0.42).abs() < 1e-4);
    }

    #[test]
    fn out_of_range_bpm_clamps_without_panic() {
        let bytes = encode_msg("/audio/bpm/bpm", vec![OscType::Float(500.0)]);
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
        let bytes = encode_msg("/audio/bpm/bpm", vec![]);
        let mut b = AudioFrameBuilder::new();
        dispatch_packet(&bytes, &mut b).expect("decode ok");
        // No bpm change; default still applies.
        let f = b.snapshot();
        assert!((f.bpm - 120.0).abs() < f32::EPSILON);
    }

    #[test]
    fn wrong_arg_type_is_skipped_without_panic() {
        let bytes = encode_msg("/audio/bpm/bpm", vec![OscType::String("nope".into())]);
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

    #[tokio::test]
    async fn listener_round_trip_over_loopback() {
        let listener = Listener::bind("127.0.0.1:0").await.expect("bind");
        let bound = listener.local_addr().expect("local addr");

        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind sender");
        let bytes = encode_bundle(vec![
            ("/audio/bpm/bpm", vec![OscType::Float(140.0)]),
            ("/audio/level/bass", vec![OscType::Float(0.6)]),
            ("/audio/beat/onbeat", vec![OscType::Float(0.7)]),
        ]);
        sender.send_to(&bytes, bound).await.expect("send");

        let mut listener = listener;
        let frame = listener.recv().await.expect("recv");
        assert!((frame.bpm - 140.0).abs() < 1e-4);
        assert!((frame.levels.bass - 0.6).abs() < 1e-4);
        assert!((frame.on_beat - 0.7).abs() < 1e-4);
        // Subsequent snapshot with no new data preserves continuous
        // fields (including the on_beat envelope).
        let mirror = listener.builder().clone();
        assert!((mirror.bpm() - 140.0).abs() < 1e-4);
    }

    #[tokio::test]
    async fn recv_with_packet_returns_frame_and_decoded_packet() {
        let listener = Listener::bind("127.0.0.1:0").await.expect("bind");
        let bound = listener.local_addr().expect("local addr");

        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind sender");
        let bytes = encode_bundle(vec![
            ("/audio/bpm/bpm", vec![OscType::Float(96.0)]),
            ("/audio/level/mid", vec![OscType::Float(0.33)]),
        ]);
        sender.send_to(&bytes, bound).await.expect("send");

        let mut listener = listener;
        let (frame, packet, peer) = listener.recv_with_packet().await.expect("recv_with_packet");
        assert!((frame.bpm - 96.0).abs() < 1e-4);
        assert!((frame.levels.mid - 0.33).abs() < 1e-4);
        assert!(peer.ip().is_loopback(), "expected loopback sender");
        // The returned packet is the decoded bundle, available for raw
        // inspection without a second decode.
        match packet {
            OscPacket::Bundle(b) => assert_eq!(b.content.len(), 2),
            OscPacket::Message(_) => panic!("expected a bundle"),
        }
    }
}
