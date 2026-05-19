//! `audiophore-engine`: the mapping and scheduling layer.
//!
//! Owns the run loop, zone-to-adapter routing, and (in M2+) show-file
//! parsing. Sits between `audiophore-audio` (input frames) and the
//! `audiophore-adapter-*` crates (output sinks).
//!
//! The piece landed in M1 §1.2 is the **bus pattern**: a lock-free
//! single-producer / many-consumer snapshot channel built on
//! [`arc_swap::ArcSwap`]. The engine's tick publishes the latest
//! [`ResolvedFrame`]; each adapter render loop reads the most recent
//! snapshot at its own clock without blocking other adapters.
//!
//! See [`IMPLEMENTATION_PLAN.md` §Scheduling and the event bus](https://github.com/audiophore/planning/blob/main/IMPLEMENTATION_PLAN.md)
//! for the design context.

pub mod mapping;

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use audiophore_adapter_core::OutputAdapter;
use audiophore_core::ResolvedFrame;

pub use mapping::map_m1;

/// Lock-free single-producer / many-consumer snapshot bus.
///
/// The engine publishes a new [`ResolvedFrame`] via [`Bus::publish`]
/// each time it has one resolved. Adapters poll the most recent
/// snapshot via [`Bus::snapshot`] on their own
/// [`tokio::time::interval`] cadence.
///
/// Lock-free, atomic, no contention between many readers and one
/// writer. A slow consumer cannot block fast consumers or the producer.
///
/// Internally backed by [`arc_swap::ArcSwap`] holding the current
/// `Arc<ResolvedFrame>`; publish is an atomic pointer swap.
pub struct Bus {
    current: ArcSwap<ResolvedFrame>,
}

impl Bus {
    /// Construct a new bus seeded with `initial` as the starting snapshot.
    ///
    /// Consumers that read before the first [`publish`](Self::publish)
    /// will observe `initial`, so callers should pass a sensible
    /// all-zero / black frame for the show.
    #[must_use]
    pub fn new(initial: ResolvedFrame) -> Self {
        Self {
            current: ArcSwap::from_pointee(initial),
        }
    }

    /// Construct a new bus seeded with [`ResolvedFrame::default`].
    #[must_use]
    pub fn empty() -> Self {
        Self::new(ResolvedFrame::default())
    }

    /// Publish a fresh frame.
    ///
    /// Any subsequent [`snapshot`](Self::snapshot) call returns this
    /// frame (or a later one if another publish has happened in
    /// between).
    ///
    /// Takes `&self` so the producer side can be shared via `Arc<Bus>`
    /// without needing a `Mutex`; the swap itself is atomic.
    pub fn publish(&self, frame: ResolvedFrame) {
        self.current.store(Arc::new(frame));
    }

    /// Publish a frame that has already been wrapped in an `Arc`.
    ///
    /// Useful when the producer wants to keep its own reference to the
    /// frame after publishing (avoids cloning the inner payload).
    pub fn publish_arc(&self, frame: Arc<ResolvedFrame>) {
        self.current.store(frame);
    }

    /// Take a snapshot of the most recently published frame.
    ///
    /// Returns an `Arc<ResolvedFrame>` that consumers can hold for as
    /// long as they need without affecting the producer or other
    /// consumers. Cloning the `Arc` is cheap.
    #[must_use]
    pub fn snapshot(&self) -> Arc<ResolvedFrame> {
        self.current.load_full()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::empty()
    }
}

impl std::fmt::Debug for Bus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let snap = self.snapshot();
        f.debug_struct("Bus")
            .field("tick", &snap.tick)
            .field("t", &snap.t)
            .field("zones", &snap.zones.len())
            .finish()
    }
}

/// How [`run_adapter`] should react when an adapter's `render` returns
/// an error.
///
/// Strategy is per-loop, not per-call, so an adapter that hits a
/// transient error can pace its retries without the bus stalling.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RenderErrorPolicy {
    /// Log the error and continue ticking. Default for M1.
    #[default]
    LogAndContinue,
    /// Log the error and exit the loop (caller can re-spawn).
    LogAndStop,
}

/// Run one adapter's render loop against the shared [`Bus`].
///
/// Ticks at `adapter.native_rate()` (M1: a single fixed rate per
/// adapter). On each tick, takes a [`Bus::snapshot`] and calls
/// [`OutputAdapter::render`] with the latest frame.
///
/// Uses [`tokio::time::MissedTickBehavior::Delay`] so a stalled adapter
/// does *not* stampede multiple renders to catch up: the next tick
/// fires one period after the late one.
///
/// `connect` and `render` errors are surfaced through `tracing` events
/// rather than propagated, so a single adapter's failure does not tear
/// down the engine. A failed `connect` exits early; `render` failures
/// are handled per `policy`.
///
/// The function runs until either `connect` fails, the loop is
/// cancelled by the runtime (e.g. task abort), or `policy` is
/// [`RenderErrorPolicy::LogAndStop`] and a render error occurs.
pub async fn run_adapter(
    mut adapter: Box<dyn OutputAdapter>,
    bus: Arc<Bus>,
    policy: RenderErrorPolicy,
) {
    let rate = adapter.native_rate();
    let mut tick = tokio::time::interval(rate);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    if let Err(err) = adapter.connect().await {
        tracing::error!(
            adapter = adapter.name(),
            rate_ms = u64::try_from(rate.as_millis()).unwrap_or(u64::MAX),
            error = %err,
            "adapter connect failed",
        );
        return;
    }

    loop {
        tick.tick().await;
        let frame = bus.snapshot();
        if let Err(err) = adapter.render(&frame).await {
            tracing::warn!(
                adapter = adapter.name(),
                tick = frame.tick,
                error = %err,
                "adapter render failed",
            );
            match policy {
                RenderErrorPolicy::LogAndContinue => {}
                RenderErrorPolicy::LogAndStop => break,
            }
        }
    }

    if let Err(err) = adapter.disconnect().await {
        tracing::warn!(
            adapter = adapter.name(),
            error = %err,
            "adapter disconnect failed",
        );
    }
}

/// Default render interval used by the engine when no adapter-specific
/// rate dominates. 60Hz (~16.67ms), matching the M1 WLED target.
pub const DEFAULT_TICK_RATE: Duration = Duration::from_micros(16_667);

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "tests: thread join / task await panics are acceptable failure modes"
)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::time::Duration;

    use audiophore_adapter_core::{AdapterError, Capability, OutputAdapter};
    use audiophore_core::{ResolvedFrame, Rgb, Zone, ZoneId, ZoneKind, ZonePayload, ZoneSize};

    use super::{Bus, RenderErrorPolicy, run_adapter};

    fn frame_with_tick(tick: u64) -> ResolvedFrame {
        let mut zones = HashMap::new();
        zones.insert(
            ZoneId("test".to_owned()),
            ZonePayload::Fixture(Rgb::default(), 0.0),
        );
        #[allow(clippy::cast_precision_loss, reason = "test-only tick→f64 conversion")]
        let t = tick as f64 / 60.0;
        ResolvedFrame { tick, t, zones }
    }

    #[test]
    fn empty_bus_yields_default_frame() {
        let bus = Bus::empty();
        let snap = bus.snapshot();
        assert_eq!(snap.tick, 0);
        assert!(snap.zones.is_empty());
    }

    #[test]
    fn publish_then_snapshot_returns_latest() {
        let bus = Bus::empty();
        bus.publish(frame_with_tick(1));
        bus.publish(frame_with_tick(2));
        bus.publish(frame_with_tick(3));
        assert_eq!(bus.snapshot().tick, 3);
    }

    #[test]
    fn snapshot_arc_outlives_subsequent_publishes() {
        let bus = Bus::empty();
        bus.publish(frame_with_tick(7));
        let pinned = bus.snapshot();
        for tick in 8..20 {
            bus.publish(frame_with_tick(tick));
        }
        // Old snapshot still valid and pinned to the value at read time.
        assert_eq!(pinned.tick, 7);
        // Latest snapshot reflects newest publish.
        assert_eq!(bus.snapshot().tick, 19);
    }

    #[test]
    fn multi_consumer_concurrent_read_sees_monotonic_frames() {
        let bus = Arc::new(Bus::empty());
        bus.publish(frame_with_tick(1));

        let consumer_count = 8;
        let reads_per_consumer = 1_000;
        let mut handles = Vec::with_capacity(consumer_count);

        for _ in 0..consumer_count {
            let bus = Arc::clone(&bus);
            handles.push(std::thread::spawn(move || {
                let mut max_seen = 0u64;
                for _ in 0..reads_per_consumer {
                    let snap = bus.snapshot();
                    // Producer only publishes increasing ticks, so any
                    // snapshot tick observed must be >= every previous one.
                    assert!(
                        snap.tick >= max_seen,
                        "non-monotonic snapshot: {} after {}",
                        snap.tick,
                        max_seen,
                    );
                    max_seen = snap.tick;
                }
                max_seen
            }));
        }

        for tick in 2..=200 {
            bus.publish(frame_with_tick(tick));
        }

        for h in handles {
            let max_seen = h.join().expect("consumer thread panicked");
            assert!(max_seen >= 1);
        }
        assert_eq!(bus.snapshot().tick, 200);
    }

    #[test]
    fn slow_consumer_does_not_block_fast_consumer() {
        let bus = Arc::new(Bus::empty());
        bus.publish(frame_with_tick(0));

        let stop = Arc::new(AtomicBool::new(false));

        let slow = {
            let bus = Arc::clone(&bus);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut count = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    let _ = bus.snapshot();
                    std::thread::sleep(Duration::from_millis(50));
                    count += 1;
                }
                count
            })
        };

        let fast_count = Arc::new(AtomicU32::new(0));
        let fast = {
            let bus = Arc::clone(&bus);
            let stop = Arc::clone(&stop);
            let counter = Arc::clone(&fast_count);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let _ = bus.snapshot();
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            })
        };

        let producer = {
            let bus = Arc::clone(&bus);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut tick = 1u64;
                while !stop.load(Ordering::Relaxed) {
                    bus.publish(frame_with_tick(tick));
                    tick += 1;
                    std::thread::sleep(Duration::from_micros(100));
                }
                tick
            })
        };

        std::thread::sleep(Duration::from_millis(150));
        stop.store(true, Ordering::Relaxed);

        let slow_reads = slow.join().expect("slow consumer panicked");
        fast.join().expect("fast consumer panicked");
        let final_tick = producer.join().expect("producer panicked");

        let fast_reads = fast_count.load(Ordering::Relaxed);

        // Conservative floor: at least 10x. In practice it's 1000x+.
        assert!(
            fast_reads >= slow_reads.saturating_mul(10),
            "fast consumer ({fast_reads}) was not >=10x the slow one ({slow_reads})",
        );
        assert!(final_tick > 1);
    }

    /// Minimal in-process adapter used to exercise [`run_adapter`].
    struct CountingAdapter {
        name: &'static str,
        rate: Duration,
        zones: Vec<Zone>,
        capabilities: Vec<Capability>,
        connect_calls: Arc<AtomicU32>,
        render_calls: Arc<AtomicU32>,
        last_tick: Arc<AtomicU64>,
    }

    impl CountingAdapter {
        fn new(name: &'static str, rate: Duration) -> Self {
            Self {
                name,
                rate,
                zones: vec![Zone {
                    id: ZoneId("test".to_owned()),
                    kind: ZoneKind::Fixture,
                    size: ZoneSize::Single,
                }],
                capabilities: vec![Capability::Rgb],
                connect_calls: Arc::new(AtomicU32::new(0)),
                render_calls: Arc::new(AtomicU32::new(0)),
                last_tick: Arc::new(AtomicU64::new(0)),
            }
        }
    }

    #[async_trait::async_trait]
    impl OutputAdapter for CountingAdapter {
        fn name(&self) -> &str {
            self.name
        }
        fn capabilities(&self) -> &[Capability] {
            &self.capabilities
        }
        fn zones(&self) -> &[Zone] {
            &self.zones
        }
        fn native_rate(&self) -> Duration {
            self.rate
        }
        async fn connect(&mut self) -> Result<(), AdapterError> {
            self.connect_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        async fn render(&mut self, frame: &ResolvedFrame) -> Result<(), AdapterError> {
            self.render_calls.fetch_add(1, Ordering::Relaxed);
            self.last_tick.store(frame.tick, Ordering::Relaxed);
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), AdapterError> {
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn run_adapter_ticks_and_reads_latest_frame() {
        let bus = Arc::new(Bus::empty());
        let adapter = CountingAdapter::new("counter", Duration::from_millis(10));
        let connect_calls = Arc::clone(&adapter.connect_calls);
        let render_calls = Arc::clone(&adapter.render_calls);
        let last_tick = Arc::clone(&adapter.last_tick);

        let task = tokio::spawn(run_adapter(
            Box::new(adapter),
            Arc::clone(&bus),
            RenderErrorPolicy::LogAndContinue,
        ));

        // Drive 50 publishes over 100ms of virtual time.
        for tick in 1..=50u64 {
            bus.publish(frame_with_tick(tick));
            tokio::time::advance(Duration::from_millis(2)).await;
        }

        // Let the adapter loop see the final state.
        tokio::time::advance(Duration::from_millis(20)).await;
        tokio::task::yield_now().await;

        task.abort();
        let _ = task.await;

        assert_eq!(connect_calls.load(Ordering::Relaxed), 1);
        let renders = render_calls.load(Ordering::Relaxed);
        assert!(renders >= 1, "expected at least one render, got {renders}");
        assert_eq!(last_tick.load(Ordering::Relaxed), 50);
    }
}
