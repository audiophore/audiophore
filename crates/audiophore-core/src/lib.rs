//! `audiophore-core`: the type vocabulary of Audiophore.
//!
//! Holds the fundamental data structures every other crate builds on
//! (audio frames, scene state, zones, payloads). Kept small and
//! dependency-light so the rest of the workspace can move independently.
//!
//! See [`IMPLEMENTATION_PLAN.md`](https://github.com/audiophore/planning/blob/main/IMPLEMENTATION_PLAN.md)
//! `§Core types` for the design context behind each item.

use std::collections::HashMap;

/// A snapshot of audio analysis state at a point in time.
///
/// Produced by `audiophore-audio` from the Synesthesia OSC stream and
/// consumed by the engine (and later by Lua scripts in M2+).
#[derive(Clone, Debug)]
pub struct AudioFrame {
    /// Monotonic seconds since engine start.
    pub t: f64,

    /// Estimated BPM (Synesthesia exposes the range `50.0..=220.0`).
    pub bpm: f32,

    /// Confidence in the BPM estimate, `0.0..=1.0`.
    pub bpm_confidence: f32,

    /// Phase within the current beat, `0.0..=1.0`.
    pub beat_phase: f32,

    /// Beat envelope, `0.0..=1.0`. Synesthesia's `/audio/beat/onbeat`
    /// spikes toward `1.0` on a beat and decays over the following
    /// frames — consumed directly so downstream effects (e.g. the M1
    /// beat-flash) decay naturally rather than switching on/off.
    pub on_beat: f32,

    /// Per-band signal levels.
    pub levels: BandValues,

    /// Per-band transient hits.
    pub hits: BandValues,

    /// Per-band sustained presence.
    pub presence: BandValues,

    /// Macro intensity (slow accumulator), `0.0..=1.0`.
    pub intensity: f32,

    /// Fade-in/out envelope, `0.0..=1.0`.
    pub fade: f32,
}

/// Per-band values for each FFT band Synesthesia exposes.
///
/// All fields are `0.0..=1.0` unless noted otherwise. `whole` is the
/// aggregate; the named bands are the discrete splits.
#[derive(Clone, Copy, Debug, Default)]
pub struct BandValues {
    /// Aggregate across all bands.
    pub whole: f32,
    /// Bass band.
    pub bass: f32,
    /// Mid band.
    pub mid: f32,
    /// Mid-high band.
    pub mid_high: f32,
    /// High band.
    pub high: f32,
}

/// Scene / palette state from Synesthesia.
#[derive(Clone, Debug)]
pub struct SceneState {
    /// Human-readable scene name (Synesthesia "scene" label).
    pub scene_name: String,
    /// Low-end palette color (e.g. background, bass).
    pub low_color: Rgb,
    /// High-end palette color (e.g. accent, treble).
    pub high_color: Rgb,
    /// Other scene controls keyed by name (e.g. `"separation" -> 0.42`).
    pub controls: HashMap<String, f32>,
}

/// Linear RGB in the `0.0..=1.0` range.
///
/// Linear (not sRGB / gamma-corrected) by convention. Adapters convert
/// to gamma / 8-bit / device-specific encodings at the adapter boundary
/// because different protocols and devices want different curves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rgb {
    /// Red channel, `0.0..=1.0` linear.
    pub r: f32,
    /// Green channel, `0.0..=1.0` linear.
    pub g: f32,
    /// Blue channel, `0.0..=1.0` linear.
    pub b: f32,
}

/// A logical group of pixels / channels addressed as a unit by the engine.
///
/// Adapters know how to render a `Zone`'s resolved values into their
/// protocol's wire format.
#[derive(Clone, Debug)]
pub struct Zone {
    /// Stable identifier for this zone.
    pub id: ZoneId,
    /// What kind of output this zone represents.
    pub kind: ZoneKind,
    /// Size / shape of this zone.
    pub size: ZoneSize,
}

/// Stable identifier for a `Zone`.
///
/// Wrapped `String` for now; could become an interned ID later if
/// profiling demands.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ZoneId(pub String);

/// What kind of output a `Zone` represents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ZoneKind {
    /// Addressable pixel strip (one row of LEDs).
    PixelStrip,
    /// Addressable pixel grid (two-dimensional matrix).
    PixelGrid,
    /// Single addressable light (e.g. Hue bulb, par can).
    Fixture,
    /// A full 512-channel raw DMX universe.
    DmxUniverse,
    /// A laser vector path.
    LaserVector,
}

/// Size / shape descriptor for a `Zone`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ZoneSize {
    /// One-dimensional pixel strip.
    Strip {
        /// Pixel count along the strip.
        count: u32,
    },
    /// Two-dimensional pixel grid.
    Grid {
        /// Width in pixels.
        w: u32,
        /// Height in pixels.
        h: u32,
    },
    /// Raw DMX channels.
    Channels {
        /// Channel count.
        count: u16,
    },
    /// Single fixture (one logical light).
    Single,
}

/// Output from the mapping layer for one tick.
///
/// Adapters consume the `ZonePayload` they care about by looking up
/// their `ZoneId`s.
#[derive(Clone, Debug, Default)]
pub struct ResolvedFrame {
    /// Monotonic tick counter from the engine.
    pub tick: u64,
    /// Wall-clock time of this frame (monotonic seconds since engine start).
    pub t: f64,
    /// Per-zone payloads for this tick.
    pub zones: HashMap<ZoneId, ZonePayload>,
}

/// The rendered payload for a single `Zone` at a single tick.
#[derive(Clone, Debug)]
pub enum ZonePayload {
    /// Per-pixel colors for a strip or grid (row-major).
    Pixels(Vec<Rgb>),
    /// Raw DMX channel bytes.
    Channels(Vec<u8>),
    /// Color + intensity for a single fixture.
    Fixture(Rgb, f32),
    /// Per-point laser vector payload.
    LaserPoints(Vec<LaserPoint>),
}

/// A single point in a laser vector frame.
///
/// Coordinate ranges follow the ILDA / Ether Dream conventions: `x`/`y`
/// are signed 16-bit (centered on `0`); `r`/`g`/`b` are unsigned 16-bit.
#[derive(Clone, Copy, Debug)]
pub struct LaserPoint {
    /// X coordinate, `-32768..=32767`.
    pub x: i16,
    /// Y coordinate, `-32768..=32767`.
    pub y: i16,
    /// Red channel, `0..=65535`.
    pub r: u16,
    /// Green channel, `0..=65535`.
    pub g: u16,
    /// Blue channel, `0..=65535`.
    pub b: u16,
    /// True when the beam should be off at this point.
    pub blank: bool,
}
