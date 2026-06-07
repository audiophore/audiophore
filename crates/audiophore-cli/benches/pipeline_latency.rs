//! Per-frame latency of the M1 hot path: OSC datagram → `AudioFrame` →
//! `map_m1` → E1.31 packets. Low latency is the product's whole point, so this
//! quantifies the CPU cost added per Synesthesia tick and breaks it down by
//! stage. At the M1 cadence (~44 Hz) the budget per frame is ~22.7 ms; these
//! numbers are microseconds, so the headroom is enormous — the bench exists to
//! catch regressions, not because we're near the limit.
//!
//! Four benchmarks:
//! 1. `osc_decode`   — `dispatch_packet` + `snapshot` on a ~15-message bundle.
//! 2. `map_m1`       — the hardcoded 300-pixel mapping.
//! 3. `sacn_pack`    — gamma-encode + 170/130 universe split + `PacketBuilder`.
//! 4. `end_to_end`   — all three chained, the real per-tick compute.
//!
//! The socket `send_to` is deliberately excluded: it's I/O, not the compute
//! latency this guards. The gamma+chunk in `sacn_pack` mirrors the adapter's
//! private `partition_rgb`; the `170 + 130` split matches the 300-pixel WS2815
//! strip across universes 1 and 2.

#![allow(missing_docs, reason = "criterion bench harness; private target")]
#![allow(
    clippy::missing_docs_in_private_items,
    reason = "criterion bench harness"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "bench-only: a pack/setup error would be a bug worth panicking on"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bench-only: pixel-count fits u32; values are bounded"
)]

use std::hint::black_box;

use audiophore_adapter_sacn::{
    DEFAULT_PRIORITY, MAX_RGB_PIXELS_PER_UNIVERSE, PacketBuilder, gamma_encode,
};
use audiophore_audio::{AudioFrameBuilder, dispatch_packet};
use audiophore_core::{Rgb, Zone, ZoneId, ZoneKind, ZonePayload, ZoneSize};
use audiophore_engine::map_m1;
use criterion::{Criterion, criterion_group, criterion_main};
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType, encoder};

/// M1 reference strip: 300-pixel WS2815, spanning universes 1 + 2.
const PIXELS: u32 = 300;
const UNIVERSES: [u16; 2] = [1, 2];

/// A Synesthesia-shaped OSC bundle (every value a single float on a nested
/// lowercase address), sized like one real tick (~15 messages).
fn synthetic_bundle() -> Vec<u8> {
    let msg = |addr: &str, v: f32| {
        OscPacket::Message(OscMessage {
            addr: addr.to_string(),
            args: vec![OscType::Float(v)],
        })
    };
    let bundle = OscPacket::Bundle(OscBundle {
        timetag: OscTime::from((0, 1)),
        content: vec![
            msg("/audio/level/all", 0.6),
            msg("/audio/level/bass", 0.8),
            msg("/audio/level/mid", 0.4),
            msg("/audio/level/midhigh", 0.3),
            msg("/audio/level/high", 0.2),
            msg("/audio/hits/bass", 0.9),
            msg("/audio/hits/mid", 0.5),
            msg("/audio/presence/bass", 0.7),
            msg("/audio/presence/mid", 0.4),
            msg("/audio/beat/onbeat", 0.85),
            msg("/audio/bpm/bpm", 128.0),
            msg("/audio/bpm/bpmconfidence", 0.95),
            msg("/audio/energy/intensity", 0.66),
            msg("/audio/time/sin", 0.5),
            msg("/controls/meta/scene", 3.0),
        ],
    });
    encoder::encode(&bundle).expect("encode synthetic bundle")
}

fn strip_zones() -> Vec<Zone> {
    vec![Zone {
        id: ZoneId("strip".to_string()),
        kind: ZoneKind::PixelStrip,
        size: ZoneSize::Strip { count: PIXELS },
    }]
}

/// Gamma-encode + split a pixel buffer across universes and pack each into an
/// E1.31 packet. Mirrors the adapter's private `partition_rgb` + `send_slots`
/// (minus the socket write). Returns total wire bytes to keep the work observed.
fn pack_pixels(builder: &mut PacketBuilder, pixels: &[Rgb]) -> usize {
    let mut total = 0;
    for (chunk, &universe) in pixels
        .chunks(MAX_RGB_PIXELS_PER_UNIVERSE)
        .zip(UNIVERSES.iter())
    {
        let mut slots = Vec::with_capacity(chunk.len() * 3);
        for px in chunk {
            slots.push(gamma_encode(px.r));
            slots.push(gamma_encode(px.g));
            slots.push(gamma_encode(px.b));
        }
        let packet = builder
            .encode(universe, 0, DEFAULT_PRIORITY, &slots)
            .unwrap();
        total += packet.len();
    }
    total
}

fn extract_pixels(frame: &audiophore_core::ResolvedFrame) -> Vec<Rgb> {
    frame
        .zones
        .values()
        .find_map(|p| match p {
            ZonePayload::Pixels(px) => Some(px.clone()),
            _ => None,
        })
        .expect("strip payload present")
}

fn bench_pipeline(c: &mut Criterion) {
    let bundle = synthetic_bundle();
    let zones = strip_zones();

    c.bench_function("osc_decode", |b| {
        b.iter(|| {
            let mut builder = AudioFrameBuilder::new();
            dispatch_packet(black_box(&bundle), &mut builder).unwrap();
            black_box(builder.snapshot())
        });
    });

    let frame = {
        let mut builder = AudioFrameBuilder::new();
        dispatch_packet(&bundle, &mut builder).unwrap();
        builder.snapshot()
    };
    c.bench_function("map_m1", |b| {
        b.iter(|| black_box(map_m1(black_box(&frame), black_box(&zones), 0)));
    });

    let pixels = extract_pixels(&map_m1(&frame, &zones, 0));
    c.bench_function("sacn_pack", |b| {
        let mut packer = PacketBuilder::new("audiophore-bench");
        b.iter(|| black_box(pack_pixels(&mut packer, black_box(&pixels))));
    });

    c.bench_function("end_to_end", |b| {
        let mut packer = PacketBuilder::new("audiophore-bench");
        b.iter(|| {
            let mut builder = AudioFrameBuilder::new();
            dispatch_packet(black_box(&bundle), &mut builder).unwrap();
            let frame = builder.snapshot();
            let resolved = map_m1(&frame, &zones, 0);
            let pixels = extract_pixels(&resolved);
            black_box(pack_pixels(&mut packer, &pixels))
        });
    });
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
