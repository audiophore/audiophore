# Contributing

Audiophore is a solo project for now, but the workflow is written down so it stays
consistent. Issues and discussion are welcome.

## Building and testing

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
cargo deny check        # license + advisory gate (CI runs this)
```

`rust-toolchain.toml` pins the toolchain; the workspace sets `unsafe_code = "deny"`
and `missing_docs = "warn"`, so public items need doc comments and unsafe is off-limits.

### Fuzzing the OSC ingress

`audiophore-audio` decodes untrusted UDP, so its dispatch path has a
coverage-guided fuzz target. It needs nightly + [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz):

```sh
cargo install cargo-fuzz                        # once
cargo +nightly fuzz run osc_decode              # from crates/audiophore-audio/
```

The invariant under test: no datagram, however malformed, may panic
`dispatch_packet` — a decode *error* is the expected outcome. The fuzz crate is a
standalone workspace (its own `[workspace]`), so it stays out of
`cargo build --workspace` and the stable CI toolchain.

A quick smoke test without hardware:

```sh
# terminal 1 — print decoded Synesthesia frames
cargo run -p audiophore-cli -- monitor --port 9000

# terminal 2 — drive a WLED controller end to end
cargo run -p audiophore-cli -- run --sacn-host <wled-ip> --pixels 300
```

## Commits and branches

- **Conventional Commits**, single line, signed off:
  `git commit -s -m "type(scope): subject"`. No body unless it genuinely adds
  information; keep the subject imperative and lower-case.
- **Branch names** `type/short-description`, e.g. `feat/adapter-artnet`,
  `fix/osc-clamp-range`, `docs/architecture`.
- One logical change per PR. Open against `main`; CI (fmt, clippy, test, `cargo deny`)
  must be green before merge.

## Architecture

Read [ARCHITECTURE.md](ARCHITECTURE.md) before adding an adapter or touching the hot
path. The short version: implement the `OutputAdapter` trait in a new
`audiophore-adapter-<name>` crate; the engine's snapshot bus and render loop do the
rest. Keep `render` non-blocking — it runs on the adapter's clock and must not stall.

## License of contributions

The application is licensed under the
[PolyForm Noncommercial License 1.0.0](LICENSE.md). Unless you state otherwise, any
contribution you submit for inclusion is licensed under the same terms. This governs
the code; it is not legal advice.
