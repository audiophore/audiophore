# Architecture

How Audiophore turns Synesthesia's OSC output into light. This is the conceptual
map; the canonical product roadmap and protocol research live in
[`audiophore/planning`](https://github.com/audiophore/planning).

## Shape

Audiophore is a Cargo workspace. Audio analysis arrives over OSC, is mapped to
per-zone color, and is rendered to protocol-native packets by pluggable output
adapters. The hot path is a lock-free snapshot bus: one producer publishes resolved
frames, each adapter reads the latest frame on its own clock. No mutexes, no channels
on the render path.

```text
Synesthesia ──OSC/UDP──▶ audiophore-audio ──AudioFrame──▶ map_m1 ──ResolvedFrame──▶ Bus
                                                                                      │
                                                          ┌───────────────────────────┤ (snapshot)
                                                          ▼                           ▼
                                                  audiophore-adapter-sacn       (future adapters)
                                                          │
                                                   E1.31/UDP ▶ WLED ▶ LED strip
```

## Crates

| Crate | Responsibility | Load-bearing items |
|---|---|---|
| `audiophore-core` | Shared vocabulary, zero deps | `AudioFrame`, `BandValues` (whole/bass/mid/mid_high/high), `Rgb` (linear 0..=1), `Zone`/`ZoneKind`, `ResolvedFrame`, `ZonePayload` |
| `audiophore-adapter-core` | The third-party adapter contract | `OutputAdapter` trait (`Send + Sync + 'static`; async `connect`/`render`/`disconnect`), `Capability`, `AdapterError` |
| `audiophore-audio` | OSC ingress → audio frames | `Listener` (tokio UDP), `AudioFrameBuilder` accumulator, `dispatch_packet` |
| `audiophore-engine` | Mapping + the bus + the render loop | `Bus` (arc-swap snapshot), `map_m1`, `run_adapter`, `RenderErrorPolicy`, `DEFAULT_TICK_RATE` (60 Hz) |
| `audiophore-adapter-sacn` | E1.31 / sACN output | `SacnAdapter` + builder, hand-rolled `PacketBuilder`, `gamma_encode`, `MAX_RGB_PIXELS_PER_UNIVERSE` (170) |
| `audiophore-cli` | Binary entry point | `monitor` and `run` subcommands |
| `audiophore-ui` | M2 Tauri desktop shell | macOS-gated spike; not wired into the M1 pipeline |

Each adapter is its own crate so third parties can publish one without forking the
project — the only contract is `OutputAdapter` from `audiophore-adapter-core`.

## Runtime data flow (M1)

1. **Ingress.** `Listener` binds a UDP socket and decodes each Synesthesia datagram
   (`rosc`). Addresses follow `/audio/<category>/<name>` (verified against a live wire
   dump). An `AudioFrameBuilder` accumulates the latest value per field and clamps to
   legal ranges; `snapshot()` freezes an `AudioFrame`.
2. **Mapping.** `map_m1(audio, zones, tick)` is the hardcoded M1 rule: per `PixelStrip`
   zone, a uniform color where R/G/B track the bass/mid/high bands scaled by `intensity`
   with a beat-flash boost. Non-strip zones are skipped at M1. Returns a `ResolvedFrame`.
3. **Publish.** The CLI loop publishes each `ResolvedFrame` to the `Bus` — an atomic
   `arc-swap` pointer swap. The tick counter increases monotonically.
4. **Render.** `run_adapter` runs each adapter on its own tokio task, ticking at the
   adapter's `native_rate` (~44 Hz for sACN). Each tick reads `bus.snapshot()` (a cheap
   Arc load) and calls `render`. A slow adapter never blocks the producer or peers — it
   simply sees the latest frame, occasionally skipping one.
5. **Wire.** `SacnAdapter` gamma-encodes linear RGB to 8-bit, partitions pixels across
   universes (170 RGB px each), and `PacketBuilder` assembles E1.31 data packets
   (root + framing + DMP layers) sent unicast to the WLED controller on port 5568.

## Concurrency model

Single-producer / many-consumer over `arc-swap::ArcSwap<Arc<ResolvedFrame>>`. The
producer (OSC loop) calls `Bus::publish`; each adapter task calls `Bus::snapshot`
independently on its own clock. This was chosen over channels deliberately: publish
never blocks on slow consumers, multi-consumer reads are cheap Arc clones, and each
reader gets latest-wins snapshot semantics with no queue to manage. One multi-threaded
tokio runtime hosts the OSC loop and all adapter tasks; `ctrl_c` drives graceful
shutdown. Render errors are handled per `RenderErrorPolicy` (M1 default: log and
continue) so one bad packet or frame never tears down the engine.

## Deliberate decisions

- **Hand-rolled E1.31 packer** rather than the stale upstream `sacn` crate — the
  ANSI E1.31-2018 data packet is three small nested layers; ~300 LOC in-tree beats
  vendoring dead code. (See `planning/RUST_ECOSYSTEM.md`.)
- **Linear RGB in core, gamma at the adapter boundary** — keeps core math portable;
  each protocol owns its own color-space transform.
- **Hardcoded `map_m1`** proves the whole pipeline end-to-end. M2 replaces it with
  config-driven mapping (show files, then Lua).
- **UI isolated and deferred** — M1 ships headless/CLI; the Tauri crate is a macOS-gated
  M2 spike that doesn't touch the render path.

## Where to go next

- Build/test/lint and contribution flow: [CONTRIBUTING.md](CONTRIBUTING.md)
- Milestones, hardware, protocol schemas: [`audiophore/planning`](https://github.com/audiophore/planning)
