//! `audiophore-engine`: the mapping and scheduling layer.
//!
//! Owns the run loop, zone-to-adapter routing, and show-file parsing.
//! Sits between `audiophore-audio` (input frames) and the `adapter-*`
//! crates (output sinks).
