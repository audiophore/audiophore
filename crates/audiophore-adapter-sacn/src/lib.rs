//! `audiophore-adapter-sacn`: E1.31 / sACN output adapter.
//!
//! M1 target: deliver hardcoded Synesthesia-derived frames to a WLED
//! controller over E1.31. Multi-universe handling is required even at
//! M1 — a 300-pixel WS2815 strip spans 2 universes (170 + 130).
