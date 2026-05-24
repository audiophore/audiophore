//! M1 hardcoded mapping from [`AudioFrame`] to [`ResolvedFrame`].
//!
//! M1 ships a single hardcoded mapping rule — three-band RGB across a
//! pixel strip — proving the OSC → engine → adapter pipeline works
//! end-to-end. M2 replaces this with config-driven mapping (TOML / Lua
//! per [`IMPLEMENTATION_PLAN.md`](https://github.com/audiophore/planning/blob/main/IMPLEMENTATION_PLAN.md)).
//!
//! Spatial mapping (per-pixel position-based colors), additional
//! `ZoneKind`s beyond `PixelStrip`, and any form of show-file
//! configuration are deliberately out of scope for M1.

use std::collections::HashMap;

use audiophore_core::{AudioFrame, ResolvedFrame, Rgb, Zone, ZoneKind, ZonePayload, ZoneSize};

/// Build a [`ResolvedFrame`] from an [`AudioFrame`] using the M1 mapping rule.
///
/// The rule for every [`ZoneKind::PixelStrip`] zone:
/// - **Red channel** = `audio.levels.bass * audio.intensity`
/// - **Green channel** = `audio.levels.mid * audio.intensity`
/// - **Blue channel** = `audio.levels.high * audio.intensity`
/// - An additive flash of `0.3 * audio.on_beat` (the beat envelope) is
///   applied to every channel, so beats register even on quiet inputs and
///   the flash decays as the envelope decays.
/// - All channels clamp to `0.0..=1.0` before being written.
///
/// Every pixel in a strip receives the same color — uniform / no spatial
/// component at M1. Non-`PixelStrip` zones (fixtures, DMX universes,
/// laser vectors) are silently skipped; they'll need their own M2+
/// mapping rules.
///
/// `tick` is supplied by the caller (engine's monotonic tick counter);
/// `audio.t` is propagated to [`ResolvedFrame::t`].
#[must_use]
pub fn map_m1(audio: &AudioFrame, zones: &[Zone], tick: u64) -> ResolvedFrame {
    let mut payloads = HashMap::with_capacity(zones.len());
    for zone in zones {
        // M1 only handles single-row pixel strips. Other kinds get
        // routed through future mapping rules (M2+). Skip silently;
        // the adapter sees a zone with no payload and treats it as
        // off / unchanged.
        let (ZoneKind::PixelStrip, ZoneSize::Strip { count }) = (zone.kind, zone.size) else {
            tracing::trace!(
                zone_id = %zone.id.0,
                "M1 mapping skipped non-PixelStrip zone",
            );
            continue;
        };
        payloads.insert(
            zone.id.clone(),
            ZonePayload::Pixels(build_strip_pixels(audio, count as usize)),
        );
    }
    ResolvedFrame {
        tick,
        t: audio.t,
        zones: payloads,
    }
}

/// Build the per-pixel buffer for one `PixelStrip` zone at M1.
///
/// Uniform color across the strip — see [`map_m1`] doc comment.
fn build_strip_pixels(audio: &AudioFrame, count: usize) -> Vec<Rgb> {
    let intensity = audio.intensity.clamp(0.0, 1.0);
    // `on_beat` is a 0.0..=1.0 envelope; scaling the flash by it makes the
    // beat boost decay with the envelope instead of switching on/off.
    let beat_boost = 0.3 * audio.on_beat.clamp(0.0, 1.0);
    let r = (audio.levels.bass * intensity + beat_boost).clamp(0.0, 1.0);
    let g = (audio.levels.mid * intensity + beat_boost).clamp(0.0, 1.0);
    let b = (audio.levels.high * intensity + beat_boost).clamp(0.0, 1.0);
    let pixel = Rgb { r, g, b };
    vec![pixel; count]
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests panic on assertion failure by design"
)]
mod tests {
    use audiophore_core::{BandValues, ZoneId};

    use super::*;

    fn strip_zone(id: &str, count: u32) -> Zone {
        Zone {
            id: ZoneId(id.to_owned()),
            kind: ZoneKind::PixelStrip,
            size: ZoneSize::Strip { count },
        }
    }

    fn fixture_zone(id: &str) -> Zone {
        Zone {
            id: ZoneId(id.to_owned()),
            kind: ZoneKind::Fixture,
            size: ZoneSize::Single,
        }
    }

    fn frame_with(levels: BandValues, intensity: f32, on_beat: f32, t: f64) -> AudioFrame {
        AudioFrame {
            t,
            bpm: 120.0,
            bpm_confidence: 1.0,
            beat_phase: 0.0,
            on_beat,
            levels,
            hits: BandValues::default(),
            presence: BandValues::default(),
            intensity,
            fade: 1.0,
        }
    }

    #[test]
    fn zero_audio_produces_all_black_pixels() {
        let zones = vec![strip_zone("strip", 8)];
        let audio = frame_with(BandValues::default(), 0.0, 0.0, 0.0);

        let resolved = map_m1(&audio, &zones, 0);

        let ZonePayload::Pixels(pixels) = resolved
            .zones
            .get(&ZoneId("strip".to_owned()))
            .expect("strip zone present")
        else {
            panic!("expected Pixels payload");
        };
        assert_eq!(pixels.len(), 8);
        for pixel in pixels {
            assert_eq!(*pixel, Rgb::default());
        }
    }

    #[test]
    fn each_band_drives_its_color_channel() {
        let zones = vec![strip_zone("strip", 4)];
        let levels = BandValues {
            whole: 0.0,
            bass: 0.8,
            mid: 0.5,
            mid_high: 0.0,
            high: 0.2,
        };
        let audio = frame_with(levels, 1.0, 0.0, 0.0);

        let resolved = map_m1(&audio, &zones, 7);

        let ZonePayload::Pixels(pixels) = resolved.zones.values().next().unwrap() else {
            panic!("expected Pixels payload");
        };
        assert_eq!(pixels.len(), 4);
        for pixel in pixels {
            assert!((pixel.r - 0.8).abs() < f32::EPSILON);
            assert!((pixel.g - 0.5).abs() < f32::EPSILON);
            assert!((pixel.b - 0.2).abs() < f32::EPSILON);
        }
        assert_eq!(resolved.tick, 7);
    }

    #[test]
    fn intensity_scales_all_channels() {
        let zones = vec![strip_zone("strip", 1)];
        let levels = BandValues {
            whole: 0.0,
            bass: 1.0,
            mid: 1.0,
            mid_high: 0.0,
            high: 1.0,
        };
        let audio = frame_with(levels, 0.5, 0.0, 0.0);

        let resolved = map_m1(&audio, &zones, 0);

        let ZonePayload::Pixels(pixels) = resolved.zones.values().next().unwrap() else {
            panic!("expected Pixels payload");
        };
        let pixel = pixels[0];
        assert!((pixel.r - 0.5).abs() < f32::EPSILON);
        assert!((pixel.g - 0.5).abs() < f32::EPSILON);
        assert!((pixel.b - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn on_beat_adds_white_flash_even_at_zero_levels() {
        let zones = vec![strip_zone("strip", 1)];
        let audio = frame_with(BandValues::default(), 0.0, 1.0, 0.0);

        let resolved = map_m1(&audio, &zones, 0);

        let ZonePayload::Pixels(pixels) = resolved.zones.values().next().unwrap() else {
            panic!("expected Pixels payload");
        };
        let pixel = pixels[0];
        assert!((pixel.r - 0.3).abs() < f32::EPSILON);
        assert!((pixel.g - 0.3).abs() < f32::EPSILON);
        assert!((pixel.b - 0.3).abs() < f32::EPSILON);
    }

    #[test]
    fn on_beat_envelope_scales_flash() {
        // A half-strength beat envelope yields half the flash (0.15), so
        // the boost decays smoothly with the envelope rather than gating.
        let zones = vec![strip_zone("strip", 1)];
        let audio = frame_with(BandValues::default(), 0.0, 0.5, 0.0);

        let resolved = map_m1(&audio, &zones, 0);

        let ZonePayload::Pixels(pixels) = resolved.zones.values().next().unwrap() else {
            panic!("expected Pixels payload");
        };
        let pixel = pixels[0];
        assert!((pixel.r - 0.15).abs() < 1e-6);
        assert!((pixel.g - 0.15).abs() < 1e-6);
        assert!((pixel.b - 0.15).abs() < 1e-6);
    }

    #[test]
    fn channels_clamp_at_one() {
        let zones = vec![strip_zone("strip", 1)];
        let levels = BandValues {
            whole: 0.0,
            bass: 1.0,
            mid: 1.0,
            mid_high: 0.0,
            high: 1.0,
        };
        // intensity=1.0 plus beat_boost=0.3 would yield 1.3 — must clamp.
        let audio = frame_with(levels, 1.0, 1.0, 0.0);

        let resolved = map_m1(&audio, &zones, 0);

        let ZonePayload::Pixels(pixels) = resolved.zones.values().next().unwrap() else {
            panic!("expected Pixels payload");
        };
        let pixel = pixels[0];
        assert!((pixel.r - 1.0).abs() < f32::EPSILON);
        assert!((pixel.g - 1.0).abs() < f32::EPSILON);
        assert!((pixel.b - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn out_of_range_intensity_clamps() {
        let zones = vec![strip_zone("strip", 1)];
        let levels = BandValues {
            whole: 0.0,
            bass: 1.0,
            mid: 0.0,
            mid_high: 0.0,
            high: 0.0,
        };
        // Synthetic out-of-range intensity. Source should never produce
        // this, but the mapping must not amplify garbage past 1.0.
        let audio = frame_with(levels, 5.0, 0.0, 0.0);

        let resolved = map_m1(&audio, &zones, 0);

        let ZonePayload::Pixels(pixels) = resolved.zones.values().next().unwrap() else {
            panic!("expected Pixels payload");
        };
        assert!((pixels[0].r - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn empty_zone_list_yields_empty_resolved_frame() {
        let audio = frame_with(BandValues::default(), 1.0, 0.0, 0.0);

        let resolved = map_m1(&audio, &[], 42);

        assert!(resolved.zones.is_empty());
        assert_eq!(resolved.tick, 42);
    }

    #[test]
    fn non_pixel_strip_zones_are_skipped() {
        let zones = vec![strip_zone("strip", 2), fixture_zone("par-can")];
        let audio = frame_with(BandValues::default(), 1.0, 0.0, 0.0);

        let resolved = map_m1(&audio, &zones, 0);

        assert_eq!(resolved.zones.len(), 1);
        assert!(resolved.zones.contains_key(&ZoneId("strip".to_owned())));
        assert!(!resolved.zones.contains_key(&ZoneId("par-can".to_owned())));
    }

    #[test]
    fn multiple_pixel_strip_zones_are_all_populated() {
        let zones = vec![strip_zone("left", 5), strip_zone("right", 3)];
        let audio = frame_with(BandValues::default(), 1.0, 0.0, 1.5);

        let resolved = map_m1(&audio, &zones, 11);

        assert_eq!(resolved.zones.len(), 2);
        let Some(ZonePayload::Pixels(left)) = resolved.zones.get(&ZoneId("left".to_owned())) else {
            panic!("left zone missing");
        };
        let Some(ZonePayload::Pixels(right)) = resolved.zones.get(&ZoneId("right".to_owned()))
        else {
            panic!("right zone missing");
        };
        assert_eq!(left.len(), 5);
        assert_eq!(right.len(), 3);
        assert!((resolved.t - 1.5).abs() < f64::EPSILON);
    }
}
