//! Picture adjustment and quality enhancement: the numbers, not the drawing.
//!
//! This module decides *what* the shader is told, and nothing here touches OpenGL,
//! egui or the engine. The split is deliberate: the same decisions are worth
//! testing without a window, and the parts that are easy to get wrong — a slider
//! mapped to a uniform, the smoothing that keeps an automatic enhancement from
//! flickering, the guard that keeps a `NaN` out of a uniform — all live here.
//!
//! # The zero channel
//!
//! [`uniforms`] returns `None` for a neutral picture with the enhancement switched
//! off, and the renderer then draws the frame exactly the way it was drawn before
//! this feature existed. "Turning it off changes nothing on screen" is therefore a
//! property of the code rather than a hope about the numbers: there is no shader in
//! the path to leave a residue.
//!
//! # What the analysis costs
//!
//! [`analyse`] reads a copy of the frame that is at most [`ANALYSIS_SIDE`] pixels on
//! its long side — a 1080p frame becomes at most 160x90 — and the renderer asks for
//! it at most ten times a second. A blurred copy of that size measured 3.14 ms at
//! worst on this project's development machine; these statistics are one pass with
//! no blur, so the bill is smaller, and it is paid ten times a second rather than
//! sixty.
//!
//! # What this deliberately does not do
//!
//! No temporal filtering (keeping a history of frames is what produces the smeared
//! edges people call "soap opera"), and no attempt at heavy denoising: everything
//! happens in one pass over a frame that is already on the GPU. The sub-effects are
//! light by construction, and the settings describe them that way.

use crate::settings::{EnhanceSettings, PictureSettings};

/// Longest side of the copy the statistics are taken from, in pixels.
pub const ANALYSIS_SIDE: usize = 160;

/// How much of the way the smoothed enhancement moves towards its target per frame.
///
/// Small enough that a cut between two shots fades in over a few frames instead of
/// stepping, large enough that the fade is over in well under a second.
pub const SMOOTHING: f32 = 0.15;

/// Movement smaller than this is ignored, so a still shot holds its values exactly.
pub const DEAD_ZONE: f32 = 0.004;

/// What one frame looks like, in the few numbers the enhancement needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameStats {
    /// Luminance at the first percentile, `0.0..=1.0` — the black point.
    pub p1: f32,
    /// Median luminance, `0.0..=1.0`. Tells a dark shot from a bright one.
    pub p50: f32,
    /// Luminance at the ninety-ninth percentile — the white point.
    pub p99: f32,
    /// Mean colourfulness, `0.0..=1.0`, where `0` is grey.
    pub saturation: f32,
    /// Mean red-minus-green and blue-minus-green, each `-1.0..=1.0`.
    pub colour_bias: [f32; 2],
    /// Mean absolute luminance step between neighbours: how much detail there is,
    /// and therefore how much a sharpener would be amplifying.
    pub detail: f32,
}

impl Default for FrameStats {
    fn default() -> Self {
        Self {
            p1: 0.0,
            p50: 0.5,
            p99: 1.0,
            saturation: 0.0,
            colour_bias: [0.0, 0.0],
            detail: 0.0,
        }
    }
}

/// What the shader is told to do automatically, after smoothing.
///
/// The default is the **identity**, not the zero vector: `black = 0` and
/// `white = 1` leave the levels alone, and every other field at `0` leaves its
/// effect off. That matters because a state of all zeros with `white = 0` would ask
/// the shader to stretch a range of width zero, and the frame would come out white.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnhanceState {
    /// Black point the frame is stretched down to.
    pub black: f32,
    /// White point it is stretched up to.
    pub white: f32,
    /// Shadow lift, `0.0..=1.0`.
    pub levels: f32,
    /// Extra saturation for an undersaturated frame, `0.0..=1.0`.
    pub colour: f32,
    /// Extra sharpening, `0.0..=1.0`.
    pub sharpen: f32,
    /// Edge-aware smoothing, `0.0..=1.0`.
    pub denoise: f32,
    /// Block-edge smoothing, `0.0..=1.0`.
    pub deblock: f32,
}

impl Default for EnhanceState {
    fn default() -> Self {
        Self {
            black: 0.0,
            white: 1.0,
            levels: 0.0,
            colour: 0.0,
            sharpen: 0.0,
            denoise: 0.0,
            deblock: 0.0,
        }
    }
}

impl EnhanceState {
    /// `true` when the shader has nothing to do.
    pub fn is_idle(&self) -> bool {
        *self == Self::default()
    }

    /// `true` when every field is a real number.
    pub fn is_finite(&self) -> bool {
        [
            self.black,
            self.white,
            self.levels,
            self.colour,
            self.sharpen,
            self.denoise,
            self.deblock,
        ]
        .iter()
        .all(|v| v.is_finite())
    }
}

/// The values one shader pass needs.
///
/// Flat fields rather than a struct of structs: they go straight into uniform
/// locations, and a name here without a matching uniform there is a bug waiting to
/// happen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureUniforms {
    /// `x` brightness, `y` contrast, `z` temperature, `w` tint.
    pub bias: [f32; 4],
    /// `1.0` leaves the curve alone.
    pub gamma: f32,
    /// `0.0` leaves the colours alone.
    pub saturation: f32,
    /// `0.0` is no sharpening.
    pub sharpness: f32,
    /// `x` auto levels, `y` colour boost, `z` denoise, `w` deblock.
    pub enhance: [f32; 4],
    /// Black and white points the auto levels stretch between.
    pub levels: [f32; 2],
}

/// Take one frame's statistics from a copy of it.
///
/// `rgba` is the frame as the upload path already holds it: packed `R, G, B, A`
/// bytes. The copy is walked on a stride and every sampled pixel counts once, which
/// is what makes this cheap enough to run ten times a second.
pub fn analyse(rgba: &[u8], width: usize, height: usize, max_side: usize) -> FrameStats {
    if width == 0 || height == 0 || max_side == 0 || rgba.len() < width * height * 4 {
        return FrameStats::default();
    }
    let stride = (width.max(height).div_ceil(max_side)).max(1);

    let mut histogram = [0u32; 256];
    let mut sampled = 0f32;
    let mut saturation = 0f32;
    let mut red = 0f32;
    let mut green = 0f32;
    let mut blue = 0f32;
    let mut detail = 0f32;
    let mut detail_samples = 0f32;

    for y in (0..height).step_by(stride) {
        let mut previous: Option<f32> = None;
        for x in (0..width).step_by(stride) {
            let index = (y * width + x) * 4;
            let r = f32::from(rgba[index]) / 255.0;
            let g = f32::from(rgba[index + 1]) / 255.0;
            let b = f32::from(rgba[index + 2]) / 255.0;
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

            histogram[(luma.clamp(0.0, 1.0) * 255.0) as usize] += 1;
            sampled += 1.0;
            let high = r.max(g).max(b);
            let low = r.min(g).min(b);
            saturation += if high > 0.0 { (high - low) / high } else { 0.0 };
            red += r;
            green += g;
            blue += b;

            // Detail is measured between neighbours in the *same* row, one stride
            // apart, so a skipped pixel cannot look like an edge.
            if let Some(previous) = previous {
                detail += (luma - previous).abs();
                detail_samples += 1.0;
            }
            previous = Some(luma);
        }
    }

    if sampled <= 0.0 {
        return FrameStats::default();
    }
    let mean_red = red / sampled;
    let mean_green = green / sampled;
    let mean_blue = blue / sampled;

    FrameStats {
        p1: percentile(&histogram, sampled, 0.01),
        p50: percentile(&histogram, sampled, 0.50),
        p99: percentile(&histogram, sampled, 0.99),
        saturation: (saturation / sampled).clamp(0.0, 1.0),
        colour_bias: [
            (mean_red - mean_green).clamp(-1.0, 1.0),
            (mean_blue - mean_green).clamp(-1.0, 1.0),
        ],
        detail: if detail_samples <= 0.0 {
            0.0
        } else {
            (detail / detail_samples).clamp(0.0, 1.0)
        },
    }
}

/// The luminance at `fraction` through the histogram's cumulative count.
///
/// Percentiles rather than the extremes on purpose: one blown highlight or one dead
/// pixel should not decide what "white" is for the whole frame.
fn percentile(histogram: &[u32; 256], total: f32, fraction: f32) -> f32 {
    let target = total * fraction.clamp(0.0, 1.0);
    let mut seen = 0f32;
    for (bin, count) in histogram.iter().enumerate() {
        seen += *count as f32;
        if seen >= target {
            return bin as f32 / 255.0;
        }
    }
    1.0
}

/// What the enhancement would like to do with this frame, before smoothing.
///
/// All zeros when the enhancement is switched off, which is what makes "off" mean
/// *not applied* rather than *applied with zero strength*.
pub fn target_state(stats: &FrameStats, settings: &EnhanceSettings) -> EnhanceState {
    if !settings.enabled {
        return EnhanceState::default();
    }
    let strength = settings.strength.clamp(0.0, 1.0);

    // The auto levels stretch the frame between its own black and white points, and
    // `strength` says how far towards that ideal the shader may go: at 0 the pair is
    // (0, 1), which is the identity, and the picture is left alone.
    let levels = if settings.auto_levels { strength } else { 0.0 };
    let black = stats.p1.clamp(0.0, 0.5) * levels;
    let white = 1.0 - (1.0 - stats.p99.clamp(0.5, 1.0)) * levels;

    // Colour only for a frame that is short of it, and never more than a third even
    // at full strength: the job is to undo a washed-out source, not to invent a
    // grade the film does not have.
    let colour = if settings.auto_colour {
        (1.0 - stats.saturation).clamp(0.0, 1.0) * strength * 0.35
    } else {
        0.0
    };

    // Sharpening backs off as the frame gets more detailed, because what a sharpener
    // amplifies in a detailed frame is mostly compression noise.
    let sharpen =
        (settings.sharpening.clamp(0.0, 1.0) * strength * (1.0 - stats.detail)).clamp(0.0, 1.0);

    EnhanceState {
        black,
        white: white.max(black + 0.05),
        levels,
        colour,
        sharpen,
        denoise: settings.denoise.clamp(0.0, 1.0) * strength,
        deblock: settings.deblock.clamp(0.0, 1.0) * strength,
    }
}

/// Move `previous` a fraction of the way towards `target`, ignoring small movement.
///
/// The dead zone is the anti-flicker measure: without it, a still shot whose
/// statistics wobble in the third decimal would have its brightness wobble with
/// them, and the eye sees that as a picture that will not sit still.
pub fn smooth(previous: Option<&EnhanceState>, target: EnhanceState, alpha: f32) -> EnhanceState {
    let Some(previous) = previous else {
        // The first frame of a file arrives at its value rather than fading in from
        // nothing: a fade at the start of every file would be a visible artefact of
        // its own.
        return target;
    };
    let alpha = alpha.clamp(0.0, 1.0);
    let step = |from: f32, to: f32| {
        let delta = to - from;
        if delta.abs() < DEAD_ZONE {
            from
        } else {
            from + delta * alpha
        }
    };
    EnhanceState {
        black: step(previous.black, target.black),
        white: step(previous.white, target.white),
        levels: step(previous.levels, target.levels),
        colour: step(previous.colour, target.colour),
        sharpen: step(previous.sharpen, target.sharpen),
        denoise: step(previous.denoise, target.denoise),
        deblock: step(previous.deblock, target.deblock),
    }
}

/// The uniforms for one frame, or `None` when the renderer must take the untouched
/// path.
///
/// This is the last gate before the GPU, so it is where the two guarantees live: a
/// disabled enhancement contributes nothing at all — the state is dropped here
/// rather than trusted from the caller — and a settings file with a `NaN` in it
/// falls back to the untouched path instead of turning the picture black.
pub fn uniforms(
    picture: &PictureSettings,
    enhance: &EnhanceSettings,
    state: EnhanceState,
) -> Option<PictureUniforms> {
    let state = if enhance.enabled {
        state
    } else {
        EnhanceState::default()
    };
    if picture.is_neutral() && state.is_idle() {
        return None;
    }
    if !picture.is_finite() || !state.is_finite() {
        return None;
    }
    Some(PictureUniforms {
        bias: [
            picture.brightness,
            picture.contrast,
            picture.temperature,
            picture.tint,
        ],
        gamma: picture.gamma,
        saturation: picture.saturation,
        sharpness: picture.sharpness,
        enhance: [
            state.levels,
            state.colour,
            state.denoise,
            state.deblock,
        ],
        levels: [state.black, state.white],
    })
}

/// Whether the picture channel would actually draw this frame.
///
/// The panel's status line asks this and nothing else, which is the point: a panel
/// that read "中性：未启用画面通道" while the shader was running would be describing
/// a different player from the one on screen. Before this existed the note looked
/// only at the sliders, so an enhancement that was switched on with the sliders
/// untouched — the default way to use it — was reported as *not adjusted* while the
/// picture was being adjusted.
pub fn is_active(
    picture: &PictureSettings,
    enhance: &EnhanceSettings,
    state: EnhanceState,
) -> bool {
    uniforms(picture, enhance, state).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of `width` x `height`, built one pixel at a time.
    fn frame(width: usize, height: usize, pixel: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = pixel(x, y);
                out.extend_from_slice(&[r, g, b, 255]);
            }
        }
        out
    }

    /// A state with something in every field, as if the previous frame had been an
    /// extreme one.
    fn busy_state() -> EnhanceState {
        EnhanceState {
            black: 0.2,
            white: 0.8,
            levels: 0.9,
            colour: 0.3,
            sharpen: 0.5,
            denoise: 0.4,
            deblock: 0.2,
        }
    }

    fn enhancing() -> EnhanceSettings {
        EnhanceSettings {
            enabled: true,
            strength: 1.0,
            ..Default::default()
        }
    }

    /// The feature hangs on this one: neutral sliders with the enhancement off must
    /// not reach the GPU at all, or "switching it off restores the picture" is a
    /// promise about numbers instead of a property of the code.
    #[test]
    fn a_neutral_picture_with_the_enhancement_off_takes_the_untouched_path() {
        let off = EnhanceSettings::default();
        assert!(uniforms(&PictureSettings::default(), &off, EnhanceState::default()).is_none());
        assert!(
            uniforms(&PictureSettings::default(), &off, busy_state()).is_none(),
            "a stale enhancement state must not keep the shader alive"
        );
    }

    /// Requirement 5, at the last gate before the GPU: switching the enhancement off
    /// drops its contribution at once, instead of waiting for the smoothing to decay
    /// towards zero over the next few frames.
    #[test]
    fn switching_the_enhancement_off_drops_its_contribution_at_once() {
        let off = EnhanceSettings::default();
        let picture = PictureSettings {
            brightness: 0.4,
            ..Default::default()
        };
        let drawn = uniforms(&picture, &off, busy_state()).expect("a slider is a reason to draw");
        assert_eq!(drawn.enhance, [0.0; 4], "no automatic effect survives");
        assert_eq!(drawn.levels, [0.0, 1.0], "no stretch survives");
    }

    #[test]
    fn the_sliders_reach_the_uniforms_unchanged() {
        let picture = PictureSettings {
            brightness: 0.25,
            contrast: -0.5,
            saturation: 0.75,
            temperature: -0.1,
            tint: 0.05,
            sharpness: 1.5,
            gamma: 0.8,
        };
        let drawn = uniforms(&picture, &EnhanceSettings::default(), EnhanceState::default())
            .expect("not neutral");
        assert_eq!(drawn.bias, [0.25, -0.5, -0.1, 0.05]);
        assert_eq!(drawn.gamma, 0.8);
        assert_eq!(drawn.saturation, 0.75);
        assert_eq!(drawn.sharpness, 1.5);
    }

    /// A hand-edited `settings.json` must not be able to black out the picture: a
    /// `NaN` reaches a uniform as "no colour at all" and 1e9 as pure white.
    #[test]
    fn a_settings_file_with_a_wild_value_falls_back_to_the_untouched_path() {
        let mut wild = PictureSettings {
            gamma: f32::NAN,
            ..Default::default()
        };
        assert!(
            uniforms(&wild, &EnhanceSettings::default(), EnhanceState::default()).is_none(),
            "a NaN must never reach a uniform"
        );

        wild.gamma = 1e9;
        wild.brightness = -37.0;
        wild.sharpness = 99.0;
        wild.clamp();
        assert!(wild.is_finite());
        // Clamped, not reset: a file that says "brightness -37" still means "as dark
        // as this slider goes", and quietly rewriting it to 0 would be a different
        // kind of surprise.
        assert_eq!(wild.brightness, -1.0);
        assert_eq!(wild.gamma, 2.5);
        assert_eq!(wild.sharpness, 2.0);
        assert!(!wild.is_neutral(), "clamping is not resetting");
    }

    #[test]
    fn analyse_reads_a_ramp_across_its_whole_range() {
        let width = 256;
        let rgba = frame(width, 4, |x, _| {
            let v = (x * 255 / (width - 1)) as u8;
            [v, v, v]
        });
        let stats = analyse(&rgba, width, 4, ANALYSIS_SIDE);
        assert!(stats.p1 <= 0.08, "the black point is near black: {}", stats.p1);
        assert!(
            (stats.p50 - 0.5).abs() < 0.06,
            "the median of a ramp is mid grey: {}",
            stats.p50
        );
        assert!(stats.p99 >= 0.92, "the white point is near white: {}", stats.p99);
        assert!(stats.p1 < stats.p50 && stats.p50 < stats.p99, "the order holds");
        assert!(stats.saturation < 0.02, "grey has no saturation");
    }

    #[test]
    fn analyse_measures_colour_and_detail() {
        let flat = frame(32, 32, |_, _| [128, 128, 128]);
        let flat_stats = analyse(&flat, 32, 32, ANALYSIS_SIDE);
        assert_eq!(flat_stats.detail, 0.0, "a flat frame has no detail");
        assert!(flat_stats.saturation < 0.01, "grey is not colourful");
        assert!(flat_stats.colour_bias[0].abs() < 0.01, "and it is not biased");

        let red = frame(32, 32, |_, _| [255, 0, 0]);
        let red_stats = analyse(&red, 32, 32, ANALYSIS_SIDE);
        assert!(red_stats.saturation > 0.9, "pure red is fully saturated");
        assert!(
            red_stats.colour_bias[0] > 0.9,
            "and it shows up as red-minus-green: {:?}",
            red_stats.colour_bias
        );

        let stripes = frame(64, 4, |x, _| {
            if x % 2 == 0 {
                [0, 0, 0]
            } else {
                [255, 255, 255]
            }
        });
        assert!(
            analyse(&stripes, 64, 4, ANALYSIS_SIDE).detail > 0.5,
            "a one-pixel stripe pattern is all detail"
        );
    }

    #[test]
    fn analyse_survives_a_buffer_that_is_too_short() {
        assert_eq!(analyse(&[], 0, 0, ANALYSIS_SIDE), FrameStats::default());
        assert_eq!(
            analyse(&[0; 4], 100, 100, ANALYSIS_SIDE),
            FrameStats::default(),
            "a buffer that cannot hold the frame is not read past its end"
        );
        assert_eq!(
            analyse(&[255, 255, 255, 255], 1, 1, ANALYSIS_SIDE).p50,
            1.0,
            "a one-pixel frame is still a frame"
        );
    }

    /// Requirement 5's "no flicker": a still shot whose statistics wobble has to
    /// settle and then stop moving, rather than track the noise.
    #[test]
    fn a_static_frame_settles_and_then_stops_moving() {
        let target = EnhanceState {
            black: 0.05,
            white: 0.95,
            levels: 0.6,
            colour: 0.1,
            ..Default::default()
        };
        let mut state = EnhanceState::default();
        for _ in 0..200 {
            state = smooth(Some(&state), target, SMOOTHING);
        }
        let settled = state;
        assert_eq!(
            smooth(Some(&settled), target, SMOOTHING),
            settled,
            "a settled state must not keep drifting"
        );

        let wobble = EnhanceState {
            levels: target.levels + DEAD_ZONE * 0.5,
            ..target
        };
        assert_eq!(
            smooth(Some(&target), wobble, SMOOTHING),
            target,
            "the dead zone stops the picture breathing"
        );
    }

    #[test]
    fn the_first_frame_arrives_at_its_target_rather_than_fading_in() {
        let target = busy_state();
        assert_eq!(smooth(None, target, SMOOTHING), target);
    }

    #[test]
    fn the_auto_levels_are_the_identity_at_zero_strength() {
        let stats = FrameStats {
            p1: 0.10,
            p50: 0.40,
            p99: 0.90,
            ..Default::default()
        };
        let flat = EnhanceSettings {
            strength: 0.0,
            ..enhancing()
        };
        let state = target_state(&stats, &flat);
        assert_eq!(state.black, 0.0);
        assert_eq!(state.white, 1.0);
        assert_eq!(state.levels, 0.0);

        // At full strength it reaches for the frame's own range, and never past it.
        let full = target_state(&stats, &enhancing());
        assert!(state.black <= full.black && full.black <= 0.5);
        assert!(state.white >= full.white && full.white >= 0.5);
        assert!(full.white > full.black + 0.04, "the range never inverts");
    }

    #[test]
    fn the_enhancement_is_idle_when_its_switch_is_off() {
        assert!(!target_state(&FrameStats::default(), &enhancing()).is_idle());
        let off = EnhanceSettings {
            enabled: false,
            ..enhancing()
        };
        assert!(target_state(&FrameStats::default(), &off).is_idle());
    }

    #[test]
    fn colour_is_boosted_only_when_the_frame_is_short_of_it() {
        let settings = enhancing();
        let grey = FrameStats {
            saturation: 0.1,
            ..Default::default()
        };
        let vivid = FrameStats {
            saturation: 1.0,
            ..Default::default()
        };
        assert!(target_state(&grey, &settings).colour > 0.2);
        assert_eq!(target_state(&vivid, &settings).colour, 0.0);

        let no_colour = EnhanceSettings {
            auto_colour: false,
            ..settings
        };
        assert_eq!(target_state(&grey, &no_colour).colour, 0.0);
    }

    /// A detailed frame is mostly compression noise where a sharpener works, so the
    /// automatic part backs off on exactly the frames that need it least.
    #[test]
    fn sharpening_backs_off_on_a_detailed_frame() {
        let settings = EnhanceSettings {
            sharpening: 1.0,
            ..enhancing()
        };
        let smooth = FrameStats {
            detail: 0.05,
            ..Default::default()
        };
        let busy = FrameStats {
            detail: 0.90,
            ..Default::default()
        };
        assert!(
            target_state(&smooth, &settings).sharpen > target_state(&busy, &settings).sharpen,
            "the busy frame gets less sharpening"
        );
    }

    /// The note the panel shows is the zero channel said out loud, so it has to
    /// answer exactly what `uniforms` answers — including for the combination that
    /// the old note got wrong: an enhancement that is switched on while the sliders
    /// are untouched.
    #[test]
    fn the_panel_note_asks_the_same_question_as_the_zero_channel() {
        let off = EnhanceSettings::default();
        assert!(
            !is_active(&PictureSettings::default(), &off, busy_state()),
            "a disabled enhancement contributes nothing, whatever the state says"
        );

        let on = enhancing();
        assert!(is_active(&PictureSettings::default(), &on, busy_state()));
        assert!(
            !is_active(&PictureSettings::default(), &on, EnhanceState::default()),
            "switched on but with nothing to do yet is still an untouched picture"
        );
        assert!(is_active(
            &PictureSettings {
                brightness: 0.2,
                ..Default::default()
            },
            &off,
            EnhanceState::default()
        ));
        assert!(!is_active(
            &PictureSettings::default(),
            &off,
            EnhanceState::default()
        ));
    }

    /// Not a benchmark: a guard on the one part of this feature that costs CPU at all.
    ///
    /// The analysis is sampled down to [`ANALYSIS_SIDE`] on its long side, so a 4K frame is
    /// expected to cost about what a 1080p one does — ten times a second, that is the whole
    /// reason the statistics are taken from a copy rather than the frame. If this ever
    /// scales with the frame, the stride has stopped doing its job.
    #[test]
    fn analysing_a_frame_does_not_scale_with_the_frame() {
        let mut timings = Vec::new();
        for (label, width, height) in [("1080p", 1920usize, 1080usize), ("4K", 3840, 2160)] {
            let rgba = vec![128u8; width * height * 4];
            let started = std::time::Instant::now();
            let stats = analyse(&rgba, width, height, ANALYSIS_SIDE);
            let millis = started.elapsed().as_secs_f64() * 1000.0;
            eprintln!("picture analysis {label}: {millis:.2} ms per call");

            assert!(
                (stats.p50 - 0.5).abs() < 0.02,
                "a flat half-grey frame has a mid median, got {}",
                stats.p50
            );
            assert!(
                millis < 10.0,
                "{label} analysis took {millis:.2} ms, which is not the small sampled copy \
                 it is supposed to be"
            );
            timings.push((label, millis));
        }

        // Generous on purpose: this is a guard against the sampling being lost, not a
        // comparison of two numbers measured on a busy machine.
        let small = timings[0].1;
        let large = timings[1].1;
        assert!(
            large < small * 12.0 + 2.0,
            "a 4K frame cost {large:.2} ms against {small:.2} ms for 1080p: the sampling is gone"
        );
    }
}
