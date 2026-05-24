//! M1 §1.6 (B.3) — end-to-end loopback integration test.
//!
//! Wires the M1 pipeline together without hardware:
//!
//! 1. Build a synthetic Synesthesia-shaped OSC bundle (BPM, on-beat,
//!    bass level, …) and send it over UDP to an `audiophore-audio`
//!    [`audiophore_audio::Listener`] bound on an ephemeral loopback
//!    port.
//! 2. Drain one [`audiophore_core::AudioFrame`] from the listener and
//!    map it (in-test, by hand) to a single-zone
//!    [`audiophore_core::ResolvedFrame`] carrying an 8-pixel strip
//!    coloured from `levels.bass`. The mapping layer in
//!    `audiophore-engine` is still scaffolding (A.2 was bus + run-loop;
//!    mapping logic doesn't land until M2), so the test stands in for
//!    that step.
//! 3. Publish the `ResolvedFrame` onto an
//!    [`audiophore_engine::Bus`]; spawn
//!    [`audiophore_engine::run_adapter`] driving an
//!    [`audiophore_adapter_sacn::SacnAdapter`] whose UDP target is a
//!    second mock receiver socket on `127.0.0.1:<ephemeral>`.
//! 4. Wait (with a generous [`tokio::time::timeout`], never wall-clock
//!    sleeps) for the mock E1.31 receiver to observe a packet; decode
//!    the bytes enough to assert the ANSI E1.31-2018 wire shape and
//!    that the bass-derived slot byte is non-zero.
//!
//! Placement note (Option A from the issue): kept inside
//! `audiophore-cli/tests/` rather than spinning up a dedicated
//! `audiophore-tests` crate — the CLI is the natural integration point
//! and dropping it here avoids a new workspace member + Cargo.toml
//! ceremony.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    reason = "integration test: panics-on-assertion are appropriate; \
              the top-level test body is intentionally linear and \
              readable rather than split across helpers"
)]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use audiophore_adapter_sacn::{ACN_PACKET_IDENTIFIER, SacnAdapterBuilder, gamma_encode};
use audiophore_audio::Listener;
use audiophore_core::{ResolvedFrame, Rgb, Zone, ZoneId, ZoneKind, ZonePayload, ZoneSize};
use audiophore_engine::{Bus, RenderErrorPolicy, map_m1, run_adapter};
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType, encoder};
use tokio::net::UdpSocket;

/// Pixel count for the test zone — small enough that the universe-1
/// packet stays well under MAX (170 px) and a single send covers it.
const PIXEL_COUNT: usize = 8;

/// Universe the adapter is configured for.
const TEST_UNIVERSE: u16 = 1;

/// Generous packet-arrival timeout. The render loop ticks at the
/// adapter's native rate (~44 Hz / ~23 ms by default), so 2 seconds is
/// >> a couple of ticks even on a heavily-loaded CI runner.
const PACKET_TIMEOUT: Duration = Duration::from_secs(2);

/// Build a synthetic Synesthesia-shape OSC bundle carrying the fields
/// B.3 round-trips: BPM, bass level, and macro intensity.
///
/// `/audio/beat/onbeat` is deliberately *not* sent: the production
/// `map_m1` adds a beat flash of `0.3 * on_beat` to every channel, which
/// would lift the green/blue slots off zero. Leaving `on_beat` at its
/// `0.0` default keeps the wire assertions a clean `bass → red, 0 →
/// green, 0 → blue`. The beat-flash path itself is covered by
/// `audiophore_engine::mapping`'s own unit tests.
fn synthetic_synesthesia_bundle(bpm: f32, bass: f32, intensity: f32) -> Vec<u8> {
    let bundle = OscPacket::Bundle(OscBundle {
        // Synesthesia emits a real timetag; the listener doesn't gate
        // on it, so any deterministic value works for the test.
        timetag: OscTime::from((0, 1)),
        content: vec![
            OscPacket::Message(OscMessage {
                addr: "/audio/bpm/bpm".to_string(),
                args: vec![OscType::Float(bpm)],
            }),
            OscPacket::Message(OscMessage {
                addr: "/audio/level/bass".to_string(),
                args: vec![OscType::Float(bass)],
            }),
            OscPacket::Message(OscMessage {
                addr: "/audio/energy/intensity".to_string(),
                args: vec![OscType::Float(intensity)],
            }),
        ],
    });
    encoder::encode(&bundle).expect("encode synthetic OSC bundle")
}

/// The single pixel-strip zone the M1 mapping drives in this test.
fn test_zones() -> Vec<Zone> {
    vec![Zone {
        id: ZoneId("strip".to_string()),
        kind: ZoneKind::PixelStrip,
        size: ZoneSize::Strip {
            count: u32::try_from(PIXEL_COUNT).expect("pixel count fits in u32"),
        },
    }]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m1_loopback_osc_to_sacn_mock_receiver() {
    // ---- 1. Stand up the mock E1.31 receiver --------------------------
    // Ephemeral port on loopback; we'll point the sACN adapter at it.
    let mock_receiver = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind mock E1.31 receiver");
    let mock_addr = mock_receiver.local_addr().expect("query mock addr");

    // ---- 2. Stand up the audiophore-audio OSC listener ---------------
    let listener = Listener::bind("127.0.0.1:0")
        .await
        .expect("bind OSC listener");
    let listener_addr = listener.local_addr().expect("query listener addr");

    // ---- 3. Fire a synthetic Synesthesia bundle at the listener ------
    let sender = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind OSC sender");
    let bass = 0.8_f32;
    let bytes = synthetic_synesthesia_bundle(120.0, bass, 1.0);
    sender
        .send_to(&bytes, listener_addr)
        .await
        .expect("send synthetic OSC bundle");

    // ---- 4. Drain one AudioFrame from the listener -------------------
    let mut listener = listener;
    let audio_frame = tokio::time::timeout(PACKET_TIMEOUT, listener.recv())
        .await
        .expect("listener.recv timed out")
        .expect("listener.recv decode error");

    assert!(
        (audio_frame.bpm - 120.0).abs() < 1e-4,
        "BPM round-trip mismatch: got {}",
        audio_frame.bpm,
    );
    assert!(
        (audio_frame.intensity - 1.0).abs() < 1e-4,
        "intensity round-trip mismatch: got {}",
        audio_frame.intensity,
    );
    assert!(
        (audio_frame.levels.bass - bass).abs() < 1e-4,
        "bass level round-trip mismatch: got {}",
        audio_frame.levels.bass,
    );

    // ---- 5. Map AudioFrame → ResolvedFrame via the real engine
    //         mapping and publish ------------------------------------
    //
    // With intensity=1.0 and no beat flash, `map_m1` paints every pixel
    // `(bass, mid, high)` = `(0.8, 0.0, 0.0)`, which is what the wire
    // assertions below check for.
    let zones = test_zones();
    let bus = Arc::new(Bus::empty());
    let resolved = map_m1(&audio_frame, &zones, 1);
    bus.publish(resolved);

    // ---- 6. Build the sACN adapter targeting the mock receiver -------
    //
    // Use a quick native_rate so we don't sit waiting for the default
    // 44 Hz tick on a freshly seeded bus.
    let adapter = SacnAdapterBuilder::new("loopback-test", IpAddr::V4(Ipv4Addr::LOCALHOST))
        .target_port(mock_addr.port())
        .universes(vec![TEST_UNIVERSE])
        .native_rate(Duration::from_millis(5))
        .source_name("audiophore-loopback")
        .build()
        .expect("build sACN adapter");

    // ---- 7. Spawn the engine's per-adapter run loop ------------------
    let bus_for_loop = Arc::clone(&bus);
    let adapter_task = tokio::spawn(run_adapter(
        Box::new(adapter),
        bus_for_loop,
        RenderErrorPolicy::LogAndContinue,
    ));

    // ---- 8. Await one E1.31 packet on the mock receiver --------------
    let mut buf = [0_u8; 700];
    let (n, peer) = tokio::time::timeout(PACKET_TIMEOUT, mock_receiver.recv_from(&mut buf))
        .await
        .expect("mock receiver: no E1.31 packet observed within timeout")
        .expect("mock receiver: recv_from error");
    let packet = &buf[..n];

    // Sender came from loopback.
    assert!(
        peer.ip().is_loopback(),
        "expected loopback source, got {peer}",
    );

    // ---- 9. Decode the packet enough to verify wire shape ------------
    //
    // Byte offsets per ANSI E1.31-2018 §4 layer diagrams (cross-checked
    // against the packet-builder source in audiophore-adapter-sacn).

    // Root layer preamble + ACN packet identifier.
    assert!(
        packet.len() >= 38,
        "packet too short for root layer: {} bytes",
        packet.len(),
    );
    assert_eq!(&packet[0..2], &[0x00, 0x10], "preamble size mismatch");
    assert_eq!(&packet[2..4], &[0x00, 0x00], "post-amble size mismatch");
    assert_eq!(
        &packet[4..16],
        ACN_PACKET_IDENTIFIER.as_slice(),
        "ACN packet identifier mismatch",
    );

    // Root vector = VECTOR_ROOT_E131_DATA (0x00000004).
    assert_eq!(
        &packet[18..22],
        &[0x00, 0x00, 0x00, 0x04],
        "root vector should be VECTOR_ROOT_E131_DATA",
    );

    // Framing vector = VECTOR_E131_DATA_PACKET (0x00000002).
    assert_eq!(
        &packet[40..44],
        &[0x00, 0x00, 0x00, 0x02],
        "framing vector should be VECTOR_E131_DATA_PACKET",
    );

    // Universe number (bytes 113..115).
    let universe = u16::from_be_bytes([packet[113], packet[114]]);
    assert_eq!(
        universe, TEST_UNIVERSE,
        "universe mismatch: expected {TEST_UNIVERSE}, got {universe}",
    );

    // DMP vector (byte 117) = VECTOR_DMP_SET_PROPERTY (0x02).
    assert_eq!(
        packet[117], 0x02,
        "DMP vector should be VECTOR_DMP_SET_PROPERTY",
    );

    // Property value count = 1 (start code) + N slots, big-endian.
    let property_value_count = u16::from_be_bytes([packet[123], packet[124]]);
    let expected_slots = u16::try_from(PIXEL_COUNT * 3).expect("slot count fits in u16");
    assert_eq!(
        property_value_count,
        1 + expected_slots,
        "property value count mismatch (start code + slots)",
    );

    // Start code is null.
    assert_eq!(packet[125], 0x00, "DMX start code should be null (0x00)");

    // Total packet length matches the formula:
    //   38 (root) + 77 (framing) + 10 (DMP fixed) + 1 (start) + slots.
    let expected_total = 38 + 77 + 10 + 1 + usize::from(expected_slots);
    assert_eq!(
        packet.len(),
        expected_total,
        "packet length mismatch (expected {expected_total} bytes)",
    );

    // ---- 10. Confirm the bass input drove the wire bytes -------------
    //
    // The adapter applies `gamma_encode` to each linear-RGB channel at
    // the boundary; the first pixel's red byte (slot 0 = byte 126)
    // must match gamma_encode(bass) and must be non-zero.
    let expected_red = gamma_encode(bass);
    assert!(
        expected_red > 0,
        "test precondition: gamma_encode(bass) > 0; got {expected_red}",
    );
    assert_eq!(
        packet[126], expected_red,
        "first pixel red slot did not match gamma_encode(bass={bass}); \
         got byte=0x{:02x}, expected 0x{:02x}",
        packet[126], expected_red,
    );
    // Green and blue for the first pixel should both be zero (we only
    // painted red in the test mapping).
    assert_eq!(packet[127], 0, "first pixel green slot should be 0");
    assert_eq!(packet[128], 0, "first pixel blue slot should be 0");

    // ---- 11. Tear down ----------------------------------------------
    adapter_task.abort();
    let _ = adapter_task.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m1_loopback_zero_input_renders_zero_slots() {
    // Same plumbing as the positive test but with bass=0.0 — the wire
    // bytes for every slot should be zero. Documents the "force-shorten
    // a universe send" / failure-message acceptance line in the issue:
    // a mismatch here produces a concrete byte-level diff.

    let mock_receiver = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("bind mock receiver");
    let mock_addr = mock_receiver.local_addr().expect("query mock addr");

    let bus = Arc::new(Bus::empty());

    // Construct the ResolvedFrame directly (skip the OSC leg — the
    // first test already proves it). Eight black pixels.
    let pixels: Vec<Rgb> = vec![Rgb::default(); PIXEL_COUNT];
    let mut zones = HashMap::new();
    zones.insert(ZoneId("strip".to_string()), ZonePayload::Pixels(pixels));
    bus.publish(ResolvedFrame {
        tick: 1,
        t: 0.0,
        zones,
    });

    let adapter = SacnAdapterBuilder::new("loopback-zero", IpAddr::V4(Ipv4Addr::LOCALHOST))
        .target_port(mock_addr.port())
        .universes(vec![TEST_UNIVERSE])
        .native_rate(Duration::from_millis(5))
        .zones(vec![Zone {
            id: ZoneId("strip".to_string()),
            kind: ZoneKind::PixelStrip,
            size: ZoneSize::Strip {
                count: u32::try_from(PIXEL_COUNT).expect("pixel count fits in u32"),
            },
        }])
        .build()
        .expect("build sACN adapter");

    let adapter_task = tokio::spawn(run_adapter(
        Box::new(adapter),
        Arc::clone(&bus),
        RenderErrorPolicy::LogAndContinue,
    ));

    let mut buf = [0_u8; 700];
    let (n, _) = tokio::time::timeout(PACKET_TIMEOUT, mock_receiver.recv_from(&mut buf))
        .await
        .expect("mock receiver: no packet observed within timeout")
        .expect("mock receiver: recv_from error");
    let packet = &buf[..n];

    // Slot bytes start at offset 126; PIXEL_COUNT * 3 bytes.
    let slots_start = 126;
    let slots_end = slots_start + PIXEL_COUNT * 3;
    assert!(
        packet.len() >= slots_end,
        "packet too short for {} slot bytes",
        PIXEL_COUNT * 3,
    );
    for (i, byte) in packet[slots_start..slots_end].iter().enumerate() {
        assert_eq!(
            *byte, 0,
            "slot byte {i} should be 0 for an all-black input; got 0x{byte:02x}",
        );
    }

    adapter_task.abort();
    let _ = adapter_task.await;
}
