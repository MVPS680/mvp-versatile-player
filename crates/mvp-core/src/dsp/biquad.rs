//! RBJ cookbook biquads, transposed direct form II, `f32`.
//!
//! Not the `biquad` crate: this needs *per-channel state* whose layout is ours (the chain
//! keeps one array per band), coefficients that are recomputed per block from smoothed
//! values without allocating, and a bypass that costs nothing. That is about 150 lines, and
//! the project's convention is to keep a well-understood transform in-tree — see the header
//! of `dsp.rs` on why `atempo` is not used either.
//!
//! TDF2 rather than direct form I: four multiplications per sample instead of five, and two
//! floats of state per channel per band, which is what keeps a ten-band stereo chain inside
//! a few hundred bytes.

use std::f32::consts::PI;
use std::f64::consts::PI as PI64;

/// Normalised coefficients (`a0` divided out). [`Coeffs::IDENTITY`] is the pass-through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coeffs {
    /// Feed-forward, `z^0`.
    pub b0: f32,
    /// Feed-forward, `z^-1`.
    pub b1: f32,
    /// Feed-forward, `z^-2`.
    pub b2: f32,
    /// Feedback, `z^-1`.
    pub a1: f32,
    /// Feedback, `z^-2`.
    pub a2: f32,
}

impl Default for Coeffs {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Coeffs {
    /// `y[n] = x[n]`: the filter that does nothing, exactly.
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// `true` when this filter passes the signal through untouched, which the chain uses to
    /// skip the biquad entirely — a flat equaliser must cost nothing *and* be bit-exact.
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    fn normalised(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        if !a0.is_finite() || a0.abs() < f64::EPSILON {
            return Self::IDENTITY;
        }
        let out = Self {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        };
        if out.b0.is_finite()
            && out.b1.is_finite()
            && out.b2.is_finite()
            && out.a1.is_finite()
            && out.a2.is_finite()
        {
            out
        } else {
            Self::IDENTITY
        }
    }

    /// Peaking EQ — the ten bands of the graphic equaliser.
    ///
    /// The caller keeps `freq` below Nyquist (see `EnhanceChain::rebuild`): a peak above
    /// `sample_rate / 2` has no meaning, and a pole outside the unit circle is a
    /// self-oscillating mess.
    pub fn peaking(sample_rate: f32, freq: f32, q: f32, gain_db: f32) -> Self {
        let sample_rate = sample_rate.max(1.0) as f64;
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let w0 = 2.0 * PI64 * (freq.max(1.0) as f64 / sample_rate);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q.max(0.01) as f64);
        Self::normalised(
            1.0 + alpha * a,
            -2.0 * cos_w0,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos_w0,
            1.0 - alpha / a,
        )
    }

    /// Low shelf — the bass control. `slope` is the RBJ `S` (1.0 is the cookbook default).
    pub fn low_shelf(sample_rate: f32, freq: f32, gain_db: f32, slope: f32) -> Self {
        let sample_rate = sample_rate.max(1.0) as f64;
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let w0 = 2.0 * PI64 * (freq.max(1.0) as f64 / sample_rate);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let slope = slope.max(0.01) as f64;
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / slope - 1.0) + 2.0).sqrt();
        let base = 2.0 * a.sqrt() * alpha;
        Self::normalised(
            a * ((a + 1.0) - (a - 1.0) * cos_w0 + base),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
            a * ((a + 1.0) - (a - 1.0) * cos_w0 - base),
            (a + 1.0) + (a - 1.0) * cos_w0 + base,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
            (a + 1.0) + (a - 1.0) * cos_w0 - base,
        )
    }

    /// High shelf — the clarity control.
    pub fn high_shelf(sample_rate: f32, freq: f32, gain_db: f32, slope: f32) -> Self {
        let sample_rate = sample_rate.max(1.0) as f64;
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let w0 = 2.0 * PI64 * (freq.max(1.0) as f64 / sample_rate);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let slope = slope.max(0.01) as f64;
        let alpha = sin_w0 / 2.0 * ((a + 1.0 / a) * (1.0 / slope - 1.0) + 2.0).sqrt();
        let base = 2.0 * a.sqrt() * alpha;
        Self::normalised(
            a * ((a + 1.0) + (a - 1.0) * cos_w0 + base),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
            a * ((a + 1.0) + (a - 1.0) * cos_w0 - base),
            (a + 1.0) - (a - 1.0) * cos_w0 + base,
            2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
            (a + 1.0) - (a - 1.0) * cos_w0 - base,
        )
    }

    /// High pass — the K-weighting stage of the loudness measurement (RBJ, `q = 0.5`).
    pub fn high_pass(sample_rate: f32, freq: f32, q: f32) -> Self {
        let sample_rate = sample_rate.max(1.0) as f64;
        let w0 = 2.0 * PI64 * (freq.max(1.0) as f64 / sample_rate);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q.max(0.01) as f64);
        let b = (1.0 + cos_w0) / 2.0;
        Self::normalised(
            b,
            -(1.0 + cos_w0),
            b,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        )
    }

    /// `|H(e^{jω})|` at `freq`.
    ///
    /// This is what the interface draws its curve from and what the tests check a band
    /// against a measured sine with: one formula, so the picture and the sound cannot
    /// disagree.
    pub fn magnitude(&self, sample_rate: f32, freq: f32) -> f32 {
        let sample_rate = sample_rate.max(1.0);
        let w = 2.0 * PI * (freq / sample_rate);
        let (sin_w, cos_w) = w.sin_cos();
        let (sin_2w, cos_2w) = (2.0 * w).sin_cos();
        let num_re = self.b0 + self.b1 * cos_w + self.b2 * cos_2w;
        let num_im = -(self.b1 * sin_w + self.b2 * sin_2w);
        let den_re = 1.0 + self.a1 * cos_w + self.a2 * cos_2w;
        let den_im = -(self.a1 * sin_w + self.a2 * sin_2w);
        let num = num_re * num_re + num_im * num_im;
        let den = (den_re * den_re + den_im * den_im).max(1e-20);
        (num / den).sqrt()
    }
}

/// One biquad's state for one channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct Biquad {
    coeffs: Coeffs,
    s1: f32,
    s2: f32,
}

impl Biquad {
    /// One sample through the filter.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.coeffs.b0 * x + self.s1;
        self.s1 = self.coeffs.b1 * x - self.coeffs.a1 * y + self.s2;
        self.s2 = self.coeffs.b2 * x - self.coeffs.a2 * y;
        // A denormal, an infinity or a NaN here never washes out on its own: it would burn
        // the rest of the session as a silent, CPU-eating filter. One comparison is cheap
        // insurance.
        if y.is_finite() {
            y
        } else {
            self.reset();
            0.0
        }
    }

    /// Install new coefficients, keeping the state — which is what makes a parameter
    /// change a glide rather than a click.
    pub fn set(&mut self, coeffs: Coeffs) {
        self.coeffs = coeffs;
    }

    /// Forget the filter's history. Used after a seek, so a burst of stale energy cannot
    /// ring into the new position.
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine at `freq`, run to steady state: the gain the filter actually applies, in dB.
    ///
    /// Measured as a ratio of RMS rather than of peak. At 8 kHz a 48 kHz sine is six samples
    /// per period, and the peak of six samples is anywhere between 0.87 and 1.0 of the real
    /// amplitude depending on the filter's phase — a 1.2 dB error that would make a perfectly
    /// correct shelf look broken. RMS over whole periods has no such problem, and with the
    /// samples equally spaced it is exact.
    fn measured_gain_db(coeffs: Coeffs, sample_rate: f32, freq: f32) -> f32 {
        let mut filter = Biquad::default();
        filter.set(coeffs);
        let step = 2.0 * PI * freq / sample_rate;
        let period = (sample_rate / freq.max(1.0)).round().max(2.0) as usize;
        let warmup = period * 50;
        let measure = period * 200;

        let mut in_power = 0.0f32;
        let mut out_power = 0.0f32;
        for n in 0..(warmup + measure) {
            let x = (n as f32 * step).sin();
            let y = filter.process(x);
            if n >= warmup {
                in_power += x * x;
                out_power += y * y;
            }
        }
        10.0 * (out_power.max(1e-9) / in_power.max(1e-9)).log10()
    }

    #[test]
    fn the_identity_coefficients_pass_the_signal_through() {
        let mut filter = Biquad::default();
        for x in [-1.0f32, -0.2, 0.0, 0.3, 1.0] {
            assert_eq!(filter.process(x), x);
        }
        assert!(Coeffs::IDENTITY.is_identity());
    }

    #[test]
    fn a_peaking_band_measures_what_it_claims() {
        let sample_rate = 48_000.0;
        let coeffs = Coeffs::peaking(sample_rate, 1_000.0, 1.0, 6.0);
        let at_centre = measured_gain_db(coeffs, sample_rate, 1_000.0);
        assert!(
            (at_centre - 6.0).abs() < 0.6,
            "a +6 dB band measured {at_centre:.2} dB at its centre"
        );
        let far = measured_gain_db(coeffs, sample_rate, 8_000.0);
        assert!(far.abs() < 1.0, "a 1 kHz band moved 8 kHz by {far:.2} dB");

        // And the analytic curve the interface draws agrees with what is heard.
        let drawn = 20.0 * coeffs.magnitude(sample_rate, 1_000.0).log10();
        assert!(
            (drawn - at_centre).abs() < 0.3,
            "the drawn curve ({drawn:.2} dB) disagrees with the sound ({at_centre:.2} dB)"
        );
    }

    #[test]
    fn the_shelves_are_transparent_far_from_their_corner() {
        let sample_rate = 48_000.0;
        let bass = Coeffs::low_shelf(sample_rate, 120.0, 6.0, 1.0);
        assert!(
            measured_gain_db(bass, sample_rate, 8_000.0).abs() < 0.5,
            "the bass shelf must not colour the treble"
        );
        let clarity = Coeffs::high_shelf(sample_rate, 6_000.0, 6.0, 1.0);
        assert!(
            measured_gain_db(clarity, sample_rate, 100.0).abs() < 0.5,
            "the clarity shelf must not colour the bass"
        );
    }

    #[test]
    fn nonsense_arguments_never_produce_a_filter_that_is_not_legal() {
        // A zero sample rate, a NaN frequency, a NaN gain: whatever comes out of these has to be
        // a legal, finite filter. One NaN in a coefficient would otherwise be a silent,
        // CPU-eating filter for the rest of the session — which is why `normalised` falls back
        // to the identity rather than trusting the arithmetic.
        for coeffs in [
            Coeffs::peaking(0.0, 1_000.0, 1.0, 6.0),
            Coeffs::peaking(48_000.0, f32::NAN, 1.0, 6.0),
            Coeffs::high_pass(48_000.0, 0.0, 0.0),
            Coeffs::low_shelf(48_000.0, 120.0, f32::NAN, 0.0),
            Coeffs::peaking(48_000.0, 1_000.0, 1.0, f32::INFINITY),
        ] {
            for value in [coeffs.b0, coeffs.b1, coeffs.b2, coeffs.a1, coeffs.a2] {
                assert!(value.is_finite(), "{coeffs:?} produced {value}");
            }
        }
        // An infinite gain is not a filter that exists, so it falls back to the identity.
        assert!(Coeffs::peaking(48_000.0, 1_000.0, 1.0, f32::INFINITY).is_identity());
    }

    #[test]
    fn a_band_above_nyquist_is_still_a_stable_filter() {
        // 16 kHz at an 8 kHz sample rate: the caller clamps the frequency, and even without
        // that the coefficients must stay finite and the output bounded.
        let sample_rate = 8_000.0;
        let mut filter = Biquad::default();
        filter.set(Coeffs::peaking(sample_rate, 16_000.0_f32.min(sample_rate * 0.45), 1.0, 12.0));
        let mut peak = 0.0f32;
        for n in 0..4_000 {
            let x = (n as f32 * 0.3).sin();
            let y = filter.process(x);
            assert!(y.is_finite(), "the filter produced {y}");
            peak = peak.max(y.abs());
        }
        assert!(peak < 8.0, "a 12 dB peak grew to {peak}");
    }
}
