//! Criterion benches validating the `arc-swap`-backed [`Bus`] meets the
//! perf envelope assumed by `IMPLEMENTATION_PLAN.md` §Scheduling and
//! the event bus. Closes the M1 prep eval item from
//! `RUST_ECOSYSTEM.md` *Open evaluation items*.
//!
//! Four scenarios:
//!
//! 1. **publisher rate** — single-thread `Bus::publish` throughput.
//! 2. **single-consumer read** — single-thread `Bus::snapshot` throughput.
//! 3. **N-consumer concurrent read** — many threads hammering
//!    `snapshot` while one thread publishes.
//! 4. **slow-consumer-doesn't-stall-fast-consumer** — count fast-loop
//!    iterations while a sleep-bound slow loop runs alongside; the
//!    fast loop should approach the single-consumer baseline.

#![allow(missing_docs, reason = "criterion bench harness; private target")]
#![allow(
    clippy::missing_docs_in_private_items,
    reason = "criterion bench harness"
)]
#![allow(
    clippy::unwrap_used,
    reason = "bench-only: thread joins / channel sends panicking is fine"
)]
#![allow(
    clippy::cast_precision_loss,
    reason = "bench-only: tick→f64 conversion is fine at our scale"
)]
#![allow(
    clippy::cast_possible_truncation,
    reason = "bench-only: counter→u64 conversions are fine at our scale"
)]

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use audiophore_core::{ResolvedFrame, Rgb, ZoneId, ZonePayload};
use audiophore_engine::Bus;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

/// Build a representative `ResolvedFrame` carrying a 300-pixel WS2815
/// payload (the M1 strip target). Keeps the snapshot's `Arc` content
/// non-trivial so we're not benching a degenerate empty struct.
fn make_frame(tick: u64) -> ResolvedFrame {
    let pixels = vec![Rgb::default(); 300];
    let mut zones = HashMap::with_capacity(1);
    zones.insert(
        ZoneId("booth_strip".to_owned()),
        ZonePayload::Pixels(pixels),
    );
    ResolvedFrame {
        tick,
        t: tick as f64 / 60.0,
        zones,
    }
}

fn bench_publisher(c: &mut Criterion) {
    let mut group = c.benchmark_group("bus/publish");
    group.throughput(Throughput::Elements(1));
    let bus = Bus::empty();
    group.bench_function("single_thread", |b| {
        let mut tick = 0u64;
        b.iter(|| {
            tick += 1;
            bus.publish(black_box(make_frame(tick)));
        });
    });
    group.finish();
}

fn bench_single_consumer_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("bus/snapshot");
    group.throughput(Throughput::Elements(1));
    let bus = Bus::empty();
    bus.publish(make_frame(1));
    group.bench_function("single_thread", |b| {
        b.iter(|| {
            let snap = bus.snapshot();
            black_box(snap.tick);
        });
    });
    group.finish();
}

fn bench_n_consumer_concurrent_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("bus/snapshot_concurrent");

    for &n in &[2usize, 4, 8, 16] {
        // Each iteration of the bench: every consumer thread takes
        // `reads_per_iter` snapshots and waits at a barrier. Producer
        // publishes in parallel.
        let reads_per_iter = 10_000u64;
        group.throughput(Throughput::Elements(reads_per_iter * n as u64));

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &consumers| {
            b.iter_custom(|iters| {
                let bus = Arc::new(Bus::empty());
                bus.publish(make_frame(1));

                let stop = Arc::new(AtomicBool::new(false));
                let start_barrier = Arc::new(Barrier::new(consumers + 2)); // consumers + producer + main
                let end_barrier = Arc::new(Barrier::new(consumers + 1)); // consumers + main

                // Spawn N consumers.
                let mut handles = Vec::with_capacity(consumers);
                for _ in 0..consumers {
                    let bus = Arc::clone(&bus);
                    let start = Arc::clone(&start_barrier);
                    let end = Arc::clone(&end_barrier);
                    handles.push(thread::spawn(move || {
                        start.wait();
                        for _ in 0..(iters * reads_per_iter) {
                            let snap = bus.snapshot();
                            black_box(snap.tick);
                        }
                        end.wait();
                    }));
                }

                // Producer thread: keeps publishing until told to stop.
                let producer = {
                    let bus = Arc::clone(&bus);
                    let stop = Arc::clone(&stop);
                    let start = Arc::clone(&start_barrier);
                    thread::spawn(move || {
                        start.wait();
                        let mut tick = 2u64;
                        while !stop.load(Ordering::Relaxed) {
                            bus.publish(make_frame(tick));
                            tick = tick.wrapping_add(1);
                        }
                    })
                };

                start_barrier.wait();
                let t0 = Instant::now();
                end_barrier.wait();
                let elapsed = t0.elapsed();

                stop.store(true, Ordering::Relaxed);
                producer.join().unwrap();
                for h in handles {
                    h.join().unwrap();
                }
                elapsed
            });
        });
    }

    group.finish();
}

fn bench_slow_consumer_does_not_stall_fast_consumer(c: &mut Criterion) {
    let mut group = c.benchmark_group("bus/slow_no_stall");

    // Single number we care about: throughput of the fast consumer
    // while a slow (50ms-sleeping) consumer runs alongside. If the
    // slow consumer were gating the fast one, throughput would
    // collapse toward 1/0.05s == 20 ops/s.
    group.throughput(Throughput::Elements(1));
    group.measurement_time(Duration::from_secs(5));

    group.bench_function("fast_with_slow_sibling", |b| {
        b.iter_custom(|iters| {
            let bus = Arc::new(Bus::empty());
            bus.publish(make_frame(1));
            let stop = Arc::new(AtomicBool::new(false));

            // Slow consumer: sleep 50ms between reads.
            let slow_reads = Arc::new(AtomicU64::new(0));
            let slow = {
                let bus = Arc::clone(&bus);
                let stop = Arc::clone(&stop);
                let counter = Arc::clone(&slow_reads);
                thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let snap = bus.snapshot();
                        black_box(snap.tick);
                        thread::sleep(Duration::from_millis(50));
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                })
            };

            // Producer.
            let producer = {
                let bus = Arc::clone(&bus);
                let stop = Arc::clone(&stop);
                thread::spawn(move || {
                    let mut tick = 2u64;
                    while !stop.load(Ordering::Relaxed) {
                        bus.publish(make_frame(tick));
                        tick = tick.wrapping_add(1);
                    }
                })
            };

            // Fast consumer (the thing we're measuring) does `iters`
            // snapshots.
            let t0 = Instant::now();
            for _ in 0..iters {
                let snap = bus.snapshot();
                black_box(snap.tick);
            }
            let elapsed = t0.elapsed();

            stop.store(true, Ordering::Relaxed);
            slow.join().unwrap();
            producer.join().unwrap();
            // Sanity: slow consumer made *some* progress; we're not
            // asserting on it here but the value is observable on
            // criterion's stderr in custom-measure mode if it ever
            // becomes interesting.
            black_box(slow_reads.load(Ordering::Relaxed));
            elapsed
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_publisher,
    bench_single_consumer_read,
    bench_n_consumer_concurrent_read,
    bench_slow_consumer_does_not_stall_fast_consumer,
);
criterion_main!(benches);
