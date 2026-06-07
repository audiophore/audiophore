# Changelog

All notable changes to the Audiophore application are documented here. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims
to follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html) once releases
begin.

No versions have been tagged yet — the first release (`v0.1.0`) is cut at M1 hardware
first-light. Everything below is unreleased.

## [Unreleased]

### Added

- `audiophore-core` — shared types: `AudioFrame`, `BandValues`, `Rgb`, `Zone`,
  `ResolvedFrame`, `ZonePayload`.
- `audiophore-adapter-core` — the `OutputAdapter` trait and `Capability` flags that
  define the third-party adapter contract.
- `audiophore-audio` — OSC ingress (`Listener`, `AudioFrameBuilder`) decoding
  Synesthesia's verified `/audio/<category>/<name>` wire schema.
- `audiophore-engine` — the lock-free `Bus` (arc-swap snapshot), the hardcoded M1
  mapping (`map_m1`), and the per-adapter render loop (`run_adapter`).
- `audiophore-adapter-sacn` — E1.31 / sACN output with a hand-rolled `PacketBuilder`
  and multi-universe pixel partitioning.
- `audiophore-cli` — `monitor` (decode and print frames, `--raw` packet dump) and
  `run` (full OSC → E1.31 pipeline) subcommands.
- `audiophore-ui` — macOS-gated Tauri desktop spike (M2 prep; not in the M1 path),
  with Developer ID signing, notarization, and `tauri-plugin-updater` against a test
  feed proving the M2 packaging path end to end.
- Loopback integration test covering OSC ingress → mapping → sACN output.
- Test depth: a coverage-guided `cargo-fuzz` target for the OSC ingress, `proptest`
  invariants for `AudioFrameBuilder` clamping, and a criterion benchmark of the
  per-frame pipeline latency.
- CI and automation: CodeQL (Rust), Dependabot (`cargo` + `github-actions`), a
  macOS Tauri-bundle check, and a staged manual-dispatch release workflow.

### Changed

- Relicensed the application from `MIT OR Apache-2.0` to the PolyForm Noncommercial
  License 1.0.0 (see [LICENSE.md](LICENSE.md)).

[Unreleased]: https://github.com/audiophore/audiophore/commits/main
