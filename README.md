# Audiophore

Low-latency Rust bridge from [Synesthesia](https://synesthesia.live/) VJ software to the wider lighting ecosystem: Hue Entertainment, Nanoleaf Streaming, WLED (E1.31/sACN/DDP), Art-Net DMX, Ether Dream lasers, and generic OSC out.

This repo holds the Audiophore application. The roadmap, hardware plan, deployment design, protocol references, and Rust ecosystem evaluations live in [`audiophore/planning`](https://github.com/audiophore/planning).

## Status

Pre-M1 skeleton. The Cargo workspace and crate layout exist; M1 implementation (Synesthesia OSC → WLED via E1.31, hardcoded) starts when dev hardware arrives.

## Workspace layout

Cargo workspace from day one. Each adapter is its own crate so third parties can publish without forking the project.

```text
crates/
├── audiophore-core/    # types, event bus, traits
├── audiophore-engine/  # mapping, scheduling, show files
├── audiophore-audio/   # OSC ingress, audio frame production
├── audiophore-cli/     # binary entry point
├── adapter-core/       # adapter trait + capability flags
└── adapter-sacn/       # E1.31 / sACN adapter (M1 target)
```

Remaining adapter crates (`adapter-artnet`, `adapter-ddp`, `adapter-hue`, `adapter-nanoleaf`, `adapter-osc`, `adapter-etherdream`) and the `audiophore-script` / `audiophore-api` crates and `ui/` frontend are added as their milestones kick off. The full target layout is documented in [`planning/IMPLEMENTATION_PLAN.md`](https://github.com/audiophore/planning/blob/main/IMPLEMENTATION_PLAN.md) *Workspace layout*.

## Building

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

`rust-toolchain.toml` pins the channel to `stable`; bump to a specific version when the M1 work locks an MSRV.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
