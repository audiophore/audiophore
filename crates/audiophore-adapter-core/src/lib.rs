//! `audiophore-adapter-core`: the contract every Audiophore output adapter implements.
//!
//! Holds the adapter trait and the capability flags that describe what
//! each downstream protocol can express (pixels, DMX channels, scenes,
//! laser frames, etc.). Each `audiophore-adapter-*` crate is one implementation.
//!
//! See [`IMPLEMENTATION_PLAN.md`](https://github.com/audiophore/planning/blob/main/IMPLEMENTATION_PLAN.md)
//! `§The adapter trait` for the design context, and the `#[async_trait]`
//! trade-off note (object-safety for `Box<dyn OutputAdapter>` justifies
//! the per-call allocation cost at M1 scale).

use std::time::Duration;

use audiophore_core::{ResolvedFrame, Zone};

/// The contract every Audiophore output adapter implements.
///
/// This is the **only** stable third-party-facing API surface — version
/// extremely carefully (semver matters here). Each `audiophore-adapter-*`
/// crate provides one implementation.
///
/// Uses `#[async_trait]` for object-safety; the run loop holds adapters
/// as `Box<dyn OutputAdapter>`. The per-call allocation cost is
/// acceptable at M1 scale; revisit if profiling shows otherwise (see the
/// trait design note in `IMPLEMENTATION_PLAN.md` `§The adapter trait`).
#[async_trait::async_trait]
pub trait OutputAdapter: Send + Sync + 'static {
    /// Stable identifier, e.g. `"sacn"`, `"hue-entertainment"`.
    fn name(&self) -> &str;

    /// Capabilities advertised to the engine for zone matching.
    fn capabilities(&self) -> &[Capability];

    /// Zones this adapter is configured to drive.
    fn zones(&self) -> &[Zone];

    /// Native render rate. The engine will call `render` at this rate.
    fn native_rate(&self) -> Duration;

    /// Open device connections, perform handshakes.
    ///
    /// # Errors
    /// Returns an `AdapterError` if connect / handshake fails.
    async fn connect(&mut self) -> Result<(), AdapterError>;

    /// Render one frame.
    ///
    /// Called on the adapter's clock, not the bus tick. MUST NOT block;
    /// adapters are responsible for their own buffering.
    ///
    /// # Errors
    /// Returns an `AdapterError` if rendering the frame fails.
    async fn render(&mut self, frame: &ResolvedFrame) -> Result<(), AdapterError>;

    /// Clean shutdown. Should be idempotent.
    ///
    /// # Errors
    /// Returns an `AdapterError` if disconnect fails. Implementations
    /// should still tear down state on error.
    async fn disconnect(&mut self) -> Result<(), AdapterError>;
}

/// Capabilities an adapter advertises to the engine for zone matching.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Capability {
    /// Per-channel RGB color.
    Rgb,
    /// Dimming (overall intensity).
    Dimmer,
    /// Strobe / shutter control.
    Strobe,
    /// Addressable pixel strip (one-dimensional).
    PixelStrip,
    /// Addressable pixel grid (two-dimensional).
    PixelGrid,
    /// Raw DMX channel output.
    DmxRaw,
    /// Laser vector output.
    LaserVector,
    /// Adapter can accept update rates above 100 Hz.
    HighRate,
}

/// Errors an `OutputAdapter` can return.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// Device connect / handshake failed.
    #[error("connect failed: {0}")]
    Connect(String),

    /// Render of a frame failed.
    #[error("render failed: {0}")]
    Render(String),

    /// Underlying I/O error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Protocol-level error (malformed packet, unsupported feature, etc.).
    #[error("protocol: {0}")]
    Protocol(String),
}
