# Audiophore

Low-latency Rust bridge from [Synesthesia](https://synesthesia.live/) VJ software to the wider lighting ecosystem: Hue Entertainment, Nanoleaf Streaming, WLED (E1.31/sACN/DDP), Art-Net DMX, Ether Dream lasers, and generic OSC out.

This repo holds the Audiophore application. The roadmap, hardware plan, deployment design, protocol references, and Rust ecosystem evaluations live in [`audiophore/planning`](https://github.com/audiophore/planning).

## Status

M1 software-complete. The full Synesthesia OSC → WLED-via-E1.31 pipeline is implemented and merged — OSC ingress, the audio frame bus, the sACN packer, the `monitor` and `run` CLIs, and a loopback integration test. The remaining M1 gate is hardware first-light (lighting a real WS2815 strip), not code.

## Workspace layout

Cargo workspace from day one. Each adapter is its own crate so third parties can publish without forking the project.

```text
crates/
├── audiophore-core/    # types, event bus, traits
├── audiophore-engine/  # mapping, scheduling, show files
├── audiophore-audio/   # OSC ingress, audio frame production
├── audiophore-cli/     # binary entry point
├── audiophore-adapter-core/   # adapter trait + capability flags
└── audiophore-adapter-sacn/   # E1.31 / sACN adapter (M1 target)
```

Remaining adapter crates (`audiophore-adapter-artnet`, `audiophore-adapter-ddp`, `audiophore-adapter-hue`, `audiophore-adapter-nanoleaf`, `audiophore-adapter-osc`, `audiophore-adapter-etherdream`) and the `audiophore-script` / `audiophore-api` crates and `ui/` frontend are added as their milestones kick off. The full target layout is documented in [`planning/IMPLEMENTATION_PLAN.md`](https://github.com/audiophore/planning/blob/main/IMPLEMENTATION_PLAN.md) *Workspace layout*.

## Building

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

`rust-toolchain.toml` pins the channel to `stable`; bump to a specific version when the M1 work locks an MSRV.

## License

Source-available under the [PolyForm Noncommercial License 1.0.0](LICENSE.md). Free for any noncommercial purpose — personal use, research, education, hobby projects. Commercial use requires a separate license; contact <mrcupp@mrcupp.com>.

Audiophore backs a commercial product, so the application is noncommercial-licensed rather than permissively licensed. Earlier commits were published under `MIT OR Apache-2.0`; that grant is irrevocable for those snapshots, but this and all later versions are licensed as above.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work shall be licensed under the PolyForm Noncommercial License 1.0.0, without any additional terms or conditions.
