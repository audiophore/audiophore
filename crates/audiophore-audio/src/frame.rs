//! `AudioFrame` builder / per-tick accumulator.
//!
//! Synesthesia emits each audio variable as its own OSC message at frame
//! rate (~60 Hz). The listener accumulates the most recently observed
//! value for each field into an [`AudioFrameBuilder`], then snapshots it
//! into an [`AudioFrame`] on a tick boundary (or when the engine asks).
//!
//! This module is intentionally pure (no I/O, no async) so it's trivial
//! to unit-test against synthetic inputs.

use audiophore_core::{AudioFrame, BandValues};

/// Inclusive lower bound on Synesthesia's reported BPM range.
pub const BPM_MIN: f32 = 50.0;
/// Inclusive upper bound on Synesthesia's reported BPM range.
pub const BPM_MAX: f32 = 220.0;

/// Per-tick accumulator that builds an [`AudioFrame`] from individual
/// OSC message updates.
///
/// Each Synesthesia variable arrives in its own OSC message; we stash
/// each value as it comes in and emit a complete frame on
/// [`AudioFrameBuilder::snapshot`]. The builder keeps the last observed
/// value for every field across snapshots so a frame at tick `N` still
/// has sensible levels even if Synesthesia happened to skip a level
/// update on tick `N`.
///
/// `on_beat` is a continuous beat *envelope* (`0.0..=1.0`): like the other
/// continuous fields it retains its last observed value across snapshots,
/// tracking the decay of Synesthesia's `/audio/beat/onbeat`.
#[derive(Clone, Debug)]
pub struct AudioFrameBuilder {
    t: f64,
    bpm: f32,
    bpm_confidence: f32,
    beat_phase: f32,
    on_beat: f32,
    levels: BandValues,
    hits: BandValues,
    presence: BandValues,
    intensity: f32,
    fade: f32,
}

impl Default for AudioFrameBuilder {
    fn default() -> Self {
        Self {
            t: 0.0,
            // Mid-range default; first BPM message will overwrite.
            bpm: 120.0,
            bpm_confidence: 0.0,
            beat_phase: 0.0,
            on_beat: 0.0,
            levels: BandValues::default(),
            hits: BandValues::default(),
            presence: BandValues::default(),
            intensity: 0.0,
            fade: 1.0,
        }
    }
}

impl AudioFrameBuilder {
    /// Create a fresh builder with default-zeroed values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the monotonic timestamp (seconds since engine start) for the
    /// next [`AudioFrameBuilder::snapshot`].
    pub fn set_t(&mut self, t: f64) {
        // NaN/inf would propagate into the frame timestamp; clamp to a
        // sane non-negative value so downstream scheduling math stays
        // monotone.
        if t.is_finite() && t >= 0.0 {
            self.t = t;
        }
    }

    /// Update BPM; clamps to `BPM_MIN..=BPM_MAX` and logs out-of-range
    /// values at debug level.
    pub fn set_bpm(&mut self, bpm: f32) {
        let clamped = clamp_with_log(bpm, BPM_MIN, BPM_MAX, "bpm");
        self.bpm = clamped;
    }

    /// Update BPM-tracker confidence; clamps to `0.0..=1.0`.
    pub fn set_bpm_confidence(&mut self, v: f32) {
        self.bpm_confidence = clamp_unit(v);
    }

    /// Update beat phase; clamps to `0.0..=1.0`.
    pub fn set_beat_phase(&mut self, v: f32) {
        self.beat_phase = clamp_unit(v);
    }

    /// Update the beat envelope from `/audio/beat/onbeat`; clamps to
    /// `0.0..=1.0`. Continuous — persists across snapshots like the
    /// other level/intensity fields, so its natural decay is preserved.
    pub fn set_on_beat(&mut self, v: f32) {
        self.on_beat = clamp_unit(v);
    }

    /// Update one slot of the per-band levels view.
    pub fn set_level(&mut self, band: Band, v: f32) {
        set_band(&mut self.levels, band, v);
    }

    /// Update one slot of the per-band hits view.
    pub fn set_hit(&mut self, band: Band, v: f32) {
        set_band(&mut self.hits, band, v);
    }

    /// Update one slot of the per-band presence view.
    pub fn set_presence(&mut self, band: Band, v: f32) {
        set_band(&mut self.presence, band, v);
    }

    /// Update macro intensity; clamps to `0.0..=1.0`.
    pub fn set_intensity(&mut self, v: f32) {
        self.intensity = clamp_unit(v);
    }

    /// Update fade envelope; clamps to `0.0..=1.0`.
    pub fn set_fade(&mut self, v: f32) {
        self.fade = clamp_unit(v);
    }

    /// Snapshot the current accumulator into an [`AudioFrame`].
    ///
    /// Every field — including the `on_beat` envelope — retains its last
    /// observed value so the next frame reflects it even if no fresh OSC
    /// message arrives between ticks.
    pub fn snapshot(&mut self) -> AudioFrame {
        AudioFrame {
            t: self.t,
            bpm: self.bpm,
            bpm_confidence: self.bpm_confidence,
            beat_phase: self.beat_phase,
            on_beat: self.on_beat,
            levels: self.levels,
            hits: self.hits,
            presence: self.presence,
            intensity: self.intensity,
            fade: self.fade,
        }
    }

    /// Read-only access to the current BPM, useful for snapshot-less
    /// inspection in monitoring / debugging surfaces.
    #[must_use]
    pub fn bpm(&self) -> f32 {
        self.bpm
    }
}

/// Which Synesthesia band a given OSC message addresses.
///
/// The trailing path segment (`all`/`bass`/`mid`/`midhigh`/`high`) of a
/// `/audio/level/*`, `/audio/hits/*`, or `/audio/presence/*` address maps
/// onto one of these.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Band {
    /// Aggregate across all bands (`/audio/level/all`, …).
    Whole,
    /// Bass band (`/audio/level/bass`, …).
    Bass,
    /// Mid band (`/audio/level/mid`, …).
    Mid,
    /// Mid-high band (`/audio/level/midhigh`, …).
    MidHigh,
    /// High band (`/audio/level/high`, …).
    High,
}

fn set_band(bands: &mut BandValues, band: Band, v: f32) {
    let v = clamp_unit(v);
    match band {
        Band::Whole => bands.whole = v,
        Band::Bass => bands.bass = v,
        Band::Mid => bands.mid = v,
        Band::MidHigh => bands.mid_high = v,
        Band::High => bands.high = v,
    }
}

/// Clamp a `0.0..=1.0` unit value, treating NaN as `0.0`.
fn clamp_unit(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

fn clamp_with_log(v: f32, lo: f32, hi: f32, field: &'static str) -> f32 {
    if v.is_nan() {
        tracing::debug!(field, "OSC value was NaN; substituting lower bound");
        return lo;
    }
    if v < lo || v > hi {
        tracing::debug!(field, value = v, lo, hi, "OSC value out of range; clamping");
    }
    v.clamp(lo, hi)
}

#[cfg(test)]
mod tests {
    use super::{AudioFrameBuilder, BPM_MAX, BPM_MIN, Band};

    #[test]
    fn snapshot_preserves_all_continuous_fields_including_on_beat() {
        let mut b = AudioFrameBuilder::new();
        b.set_t(1.25);
        b.set_bpm(128.0);
        b.set_bpm_confidence(0.8);
        b.set_beat_phase(0.5);
        b.set_level(Band::Bass, 0.7);
        b.set_hit(Band::Mid, 0.4);
        b.set_presence(Band::High, 0.2);
        b.set_intensity(0.6);
        b.set_fade(0.9);
        b.set_on_beat(0.9);

        let f1 = b.snapshot();
        assert!((f1.t - 1.25).abs() < f64::EPSILON);
        assert!((f1.bpm - 128.0).abs() < f32::EPSILON);
        assert!((f1.bpm_confidence - 0.8).abs() < 1e-6);
        assert!((f1.beat_phase - 0.5).abs() < 1e-6);
        assert!((f1.on_beat - 0.9).abs() < 1e-6);
        assert!((f1.levels.bass - 0.7).abs() < 1e-6);
        assert!((f1.hits.mid - 0.4).abs() < 1e-6);
        assert!((f1.presence.high - 0.2).abs() < 1e-6);
        assert!((f1.intensity - 0.6).abs() < 1e-6);
        assert!((f1.fade - 0.9).abs() < 1e-6);

        // Second snapshot keeps every continuous value — including the
        // on_beat envelope (no per-tick clear).
        let f2 = b.snapshot();
        assert!((f2.on_beat - 0.9).abs() < 1e-6);
        assert!((f2.bpm - 128.0).abs() < f32::EPSILON);
        assert!((f2.levels.bass - 0.7).abs() < 1e-6);
    }

    #[test]
    fn bpm_out_of_range_clamps_to_bounds() {
        let mut b = AudioFrameBuilder::new();
        b.set_bpm(500.0);
        assert!((b.snapshot().bpm - BPM_MAX).abs() < f32::EPSILON);
        b.set_bpm(-10.0);
        assert!((b.snapshot().bpm - BPM_MIN).abs() < f32::EPSILON);
    }

    #[test]
    fn unit_fields_clamp_to_zero_one() {
        let mut b = AudioFrameBuilder::new();
        b.set_intensity(5.0);
        b.set_fade(-1.0);
        b.set_level(Band::Whole, 2.0);
        b.set_bpm_confidence(2.0);
        let f = b.snapshot();
        assert!((f.intensity - 1.0).abs() < f32::EPSILON);
        assert!((f.fade - 0.0).abs() < f32::EPSILON);
        assert!((f.levels.whole - 1.0).abs() < f32::EPSILON);
        assert!((f.bpm_confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn nan_values_default_to_safe_bounds() {
        let mut b = AudioFrameBuilder::new();
        b.set_bpm(f32::NAN);
        b.set_intensity(f32::NAN);
        let f = b.snapshot();
        assert!((f.bpm - BPM_MIN).abs() < f32::EPSILON);
        assert!((f.intensity - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn negative_or_nonfinite_t_is_ignored() {
        let mut b = AudioFrameBuilder::new();
        b.set_t(2.0);
        b.set_t(-1.0);
        b.set_t(f64::NAN);
        let f = b.snapshot();
        assert!((f.t - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn on_beat_envelope_clamps_and_persists() {
        let mut b = AudioFrameBuilder::new();
        b.set_on_beat(2.0);
        // Clamped to 1.0 and retained across an empty snapshot.
        assert!((b.snapshot().on_beat - 1.0).abs() < f32::EPSILON);
        assert!((b.snapshot().on_beat - 1.0).abs() < f32::EPSILON);
        b.set_on_beat(-1.0);
        assert!(b.snapshot().on_beat.abs() < f32::EPSILON);
    }
}
