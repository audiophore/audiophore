//! `audiophore-adapter-sacn`: E1.31 / sACN output adapter.
//!
//! M1 target: deliver hardcoded Synesthesia-derived frames to a WLED
//! controller over E1.31. Multi-universe handling is required even at
//! M1 — a 300-pixel WS2815 strip spans 2 universes (170 + 130).
//!
//! Hand-rolled per the planning decision recorded in
//! [`audiophore/planning#24`](https://github.com/audiophore/planning/pull/24);
//! the upstream `sacn` crate (last published 2019) is stale and the
//! Data-Packet subset we need is small enough to maintain in-tree
//! (~300 LOC). See [`packet`] for the wire format and
//! [`SacnAdapter`] / [`SacnAdapterBuilder`] for the `OutputAdapter`
//! impl.

pub mod adapter;
pub mod packet;

pub use adapter::{DEFAULT_NATIVE_RATE, MAX_UNIVERSES, SacnAdapter, SacnAdapterBuilder};
pub use packet::{
    ACN_PACKET_IDENTIFIER, DEFAULT_PRIORITY, MAX_DATA_PACKET_LEN, MAX_RGB_PIXELS_PER_UNIVERSE,
    MAX_SLOTS_PER_UNIVERSE, PackError, PacketBuilder, SACN_PORT, gamma_encode, multicast_v4,
};
