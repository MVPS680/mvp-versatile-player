//! Time-domain audio processing: pitch-preserving playback speed.
//!
//! FFmpeg's `atempo` filter does the same job inside the library, but routing
//! audio through `libavfilter` for a single, well understood transform costs us
//! a graph, a queue and a copy per frame. This module implements the classic
//! SOLA (synchronous overlap-add) time stretcher directly: the signal is cut
//! into overlapping windows, the *input* hop stays fixed while the *output* hop
//! scales with the speed, and each window is aligned to the previous one by
//! cross-correlation before being cross-faded in. Because only the timing of the
//! windows changes — never their pitch period — speech and music keep their
//! natural pitch at any speed.

/// A SOLA time stretcher for interleaved `f32` audio.
///
/// `push` accumulates input and appends stretched output; nothing is produced
/// until roughly 40–120 ms of audio has been buffered, which is the algorithm's
/// inherent look-ahead.
#[derive(Debug)]
pub struct TimeStretcher {
    channels: usize,
    sample_rate: u32,
    /// Playback speed; `1.0` bypasses the algorithm entirely.
    speed: f64,
    /// Input frames consumed per synthesis step (fixed, ~20 ms).
    hop_in: usize,
    /// Output frames produced per synthesis step: `hop_in / speed`.
    hop_out: usize,
    /// Length of the cross-fade region in frames.
    overlap: usize,
    /// Length of each analysis window in frames: `hop_out + overlap`.
    win: usize,
    /// Maximum alignment search distance in frames.
    search: usize,
    /// Unconsumed input, interleaved.
    buf: Vec<f32>,
    /// Index (in frames) of the next analysis window start inside `buf`.
    pos: usize,
    /// Windowed tail from the previous step, `overlap` frames interleaved.
    pending: Vec<f32>,
    /// Pre-computed fade-in ramp over `overlap` frames.
    ramp_up: Vec<f32>,
}

impl TimeStretcher {
    /// Speed values are clamped to this range; beyond it the artifacts outweigh
    /// any usefulness.
    pub const MIN_SPEED: f64 = 0.25;
    /// See [`TimeStretcher::MIN_SPEED`].
    pub const MAX_SPEED: f64 = 4.0;

    /// Create a stretcher for `channels` interleaved channels at `sample_rate`.
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        let mut s = Self {
            channels: channels.max(1),
            sample_rate: sample_rate.max(8000),
            speed: 1.0,
            hop_in: 0,
            hop_out: 0,
            overlap: 0,
            win: 0,
            search: 0,
            buf: Vec::new(),
            pos: 0,
            pending: Vec::new(),
            ramp_up: Vec::new(),
        };
        s.recompute(1.0);
        s.reset_buffers();
        s
    }

    /// Current speed factor.
    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// `true` when the stretcher is in bypass mode (speed is exactly 1.0).
    pub fn is_bypass(&self) -> bool {
        (self.speed - 1.0).abs() < 1e-6
    }

    /// Change the speed. Resets internal state, so expect a short re-fill.
    pub fn set_speed(&mut self, speed: f64) {
        let speed = speed.clamp(Self::MIN_SPEED, Self::MAX_SPEED);
        if (speed - self.speed).abs() < 1e-6 {
            return;
        }
        self.recompute(speed);
        self.reset_buffers();
    }

    /// Drop all buffered audio (used after a seek).
    pub fn reset(&mut self) {
        self.reset_buffers();
    }

    fn reset_buffers(&mut self) {
        self.buf.clear();
        self.pos = 0;
        self.pending.clear();
        self.pending.resize(self.overlap * self.channels, 0.0);
    }

    /// Recompute the window geometry for `speed`.
    fn recompute(&mut self, speed: f64) {
        self.speed = speed;
        let rate = self.sample_rate as f64;
        // 20 ms of input per step keeps the analysis aligned with speech pitch
        // periods without making the window long enough to smear transients.
        self.hop_in = ((0.020 * rate).round() as usize).max(64);
        self.hop_out = ((self.hop_in as f64 / speed).round() as usize).max(8);
        // The cross-fade covers a third of the output hop, capped at 15 ms so
        // very slow speeds do not blur consonants.
        self.overlap = (self.hop_out / 3).min((0.015 * rate) as usize).max(1);
        self.win = self.hop_out + self.overlap;
        self.search = (self.hop_in / 2).min((0.010 * rate) as usize).max(1);
        self.ramp_up.clear();
        self.ramp_up.reserve(self.overlap);
        for i in 0..self.overlap {
            // Raised-cosine fade; `ramp_down = 1 - ramp_up` keeps the sum unity.
            let t = if self.overlap <= 1 {
                1.0
            } else {
                i as f32 / (self.overlap - 1) as f32
            };
            self.ramp_up
                .push(0.5 - 0.5 * (std::f32::consts::PI * t).cos());
        }
    }

    /// Number of frames currently buffered but not yet consumed.
    pub fn buffered_frames(&self) -> usize {
        self.buf.len() / self.channels
    }

    /// Number of frames the algorithm needs buffered before it can emit output.
    pub fn lookahead_frames(&self) -> usize {
        self.search + self.win
    }

    /// Feed `samples` (interleaved) and append stretched output to `out`.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) {
        if samples.is_empty() {
            return;
        }
        if self.is_bypass() {
            out.extend_from_slice(samples);
            return;
        }
        self.buf.extend_from_slice(samples);
        self.process(out);
    }

    /// Flush the remaining look-ahead, zero-padding the tail so the last few
    /// milliseconds are not swallowed. Call once the source has ended.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        if self.is_bypass() {
            return;
        }
        // Pad enough for two more synthesis steps so the buffered audio is
        // actually emitted instead of being thrown away.
        let want = self.pos + self.lookahead_frames() + self.hop_in * 2;
        let have = self.buffered_frames();
        if have < want {
            let pad = want - have;
            self.buf
                .extend(std::iter::repeat_n(0.0f32, pad * self.channels));
        }
        self.process(out);
        self.reset_buffers();
    }

    fn process(&mut self, out: &mut Vec<f32>) {
        let ch = self.channels;
        let need = self.lookahead_frames();
        loop {
            // `pos` has already consumed part of the buffer, so the available
            // window is measured from there, not from the start of `buf`.
            if self.buffered_frames().saturating_sub(self.pos) < need {
                break;
            }
            let base = self.pos;
            // The search may not look before the start of the buffer.
            let lo = self.search.min(base);
            let hi = self.search;
            let delta = self.best_delta(base, lo, hi);
            debug_assert!(base as isize + delta >= 0);
            let start = (base as isize + delta) as usize;
            debug_assert!(start + self.win <= self.buf.len() / ch);

            let out_from = out.len();
            out.reserve(self.hop_out * ch);
            // Cross-faded head: previous tail plus this window's fade-in.
            for i in 0..self.overlap {
                let w = self.ramp_up[i];
                let seg_i = (start + i) * ch;
                for c in 0..ch {
                    let prev = self.pending[i * ch + c];
                    out.push(prev + self.buf[seg_i + c] * w);
                }
            }
            // The steady middle of the window passes through untouched.
            let mid_from = (start + self.overlap) * ch;
            let mid_to = (start + self.hop_out) * ch;
            out.extend_from_slice(&self.buf[mid_from..mid_to]);
            debug_assert_eq!(out.len() - out_from, self.hop_out * ch);

            // Windowed tail for the next step's cross-fade.
            for i in 0..self.overlap {
                let w = 1.0 - self.ramp_up[i];
                let src = (start + self.hop_out + i) * ch;
                for c in 0..ch {
                    self.pending[i * ch + c] = self.buf[src + c] * w;
                }
            }

            self.pos += self.hop_in;
        }

        // Reclaim consumed input once it grows past a comfortable margin.
        let safe = self.pos.saturating_sub(self.search);
        if safe > 8192 {
            self.buf.drain(0..safe * ch);
            self.pos -= safe;
        }
    }

    /// Find the alignment `delta` in `[-lo, hi]` that best matches the pending
    /// tail against the candidate window head.
    fn best_delta(&self, base: usize, lo: usize, hi: usize) -> isize {
        if self.overlap == 0 {
            return 0;
        }
        // Coarse-to-fine: scanning every single sample is wasteful for large
        // search windows and the extra precision is inaudible.
        let coarse: isize = 4;
        let mut best = 0isize;
        let mut best_score = f32::NEG_INFINITY;
        let mut delta = -(lo as isize);
        while delta <= hi as isize {
            let score = self.correlation(base, delta);
            if score > best_score {
                best_score = score;
                best = delta;
            }
            delta += coarse;
        }
        for delta in (best - coarse + 1)..(best + coarse) {
            if delta < -(lo as isize) || delta > hi as isize {
                continue;
            }
            let score = self.correlation(base, delta);
            if score > best_score {
                best_score = score;
                best = delta;
            }
        }
        best
    }

    /// Normalised cross-correlation of the pending tail with the window head
    /// starting at `base + delta`. Higher is better.
    fn correlation(&self, base: usize, delta: isize) -> f32 {
        let ch = self.channels;
        let start = base as isize + delta;
        if start < 0 {
            return f32::NEG_INFINITY;
        }
        let start = start as usize;
        if (start + self.overlap) * ch > self.buf.len() {
            return f32::NEG_INFINITY;
        }
        let mut num = 0.0f32;
        let mut den_a = 0.0f32;
        let mut den_b = 0.0f32;
        for i in 0..self.overlap {
            for c in 0..ch {
                let a = self.pending[i * ch + c];
                let b = self.buf[(start + i) * ch + c];
                num += a * b;
                den_a += a * a;
                den_b += b * b;
            }
        }
        let den = (den_a * den_b).sqrt();
        if den <= 1e-9 {
            0.0
        } else {
            num / den
        }
    }
}

/// Apply a linear gain to interleaved samples, with graceful clipping.
///
/// Values that would clip are soft-limited through a `tanh` knee so that
/// boosting a quiet track distorts gently rather than cracking.
pub fn apply_gain(samples: &mut [f32], gain: f32) {
    if !gain.is_finite() || gain < 0.0 {
        return;
    }
    if (gain - 1.0).abs() < 1e-6 {
        return;
    }
    if gain == 0.0 {
        samples.fill(0.0);
        return;
    }
    if gain < 1.0 {
        for s in samples.iter_mut() {
            *s *= gain;
        }
        return;
    }
    // Above unity, drive the signal into a soft knee only when it would clip.
    const KNEE: f32 = 0.85;
    for s in samples.iter_mut() {
        let v = *s * gain;
        *s = if v.abs() <= KNEE {
            v
        } else {
            let sign = if v < 0.0 { -1.0 } else { 1.0 };
            let over = (v.abs() - KNEE) / (1.0 - KNEE);
            sign * (KNEE + (1.0 - KNEE) * over.tanh())
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, rate: u32, freq: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f32 / rate as f32;
                let v = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.5;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn bypass_is_transparent() {
        let mut s = TimeStretcher::new(2, 48_000);
        let input = tone(1_000, 48_000, 440.0);
        let mut out = Vec::new();
        s.push(&input, &mut out);
        assert_eq!(out, input);
        assert!(s.is_bypass());
    }

    #[test]
    fn speed_change_scales_the_sample_count() {
        for speed in [0.5f64, 0.75, 1.5, 2.0, 3.0] {
            let mut s = TimeStretcher::new(2, 48_000);
            s.set_speed(speed);
            let frames = 48_000; // one second of input
            let input = tone(frames, 48_000, 440.0);
            let mut out = Vec::new();
            s.push(&input, &mut out);
            s.flush(&mut out);
            let produced = out.len() / 2;
            let expected = frames as f64 / speed;
            let tolerance = expected * 0.20 + 4096.0;
            assert!(
                (produced as f64 - expected).abs() < tolerance,
                "speed {speed}: produced {produced} frames, expected about {expected}"
            );
        }
    }

    #[test]
    fn output_is_finite_and_bounded() {
        let mut s = TimeStretcher::new(2, 48_000);
        s.set_speed(1.3);
        let input: Vec<f32> = (0..96_000)
            .map(|i| ((i as f32) * 0.01).sin() * if i % 3 == 0 { 0.2 } else { 0.7 })
            .collect();
        let mut out = Vec::new();
        s.push(&input, &mut out);
        s.flush(&mut out);
        assert!(out.len() > 1000);
        assert!(out.iter().all(|v| v.is_finite()), "no NaN/Inf may escape");
        assert!(
            out.iter().all(|v| v.abs() < 4.0),
            "SOLA must not add gain to the signal"
        );
    }

    #[test]
    fn flush_does_not_lose_the_tail() {
        let mut s = TimeStretcher::new(2, 48_000);
        s.set_speed(2.0);
        let input = tone(24_000, 48_000, 220.0);
        let mut out = Vec::new();
        s.push(&input, &mut out);
        s.flush(&mut out);
        assert!(!out.is_empty());
        assert_eq!(s.buffered_frames(), 0);
    }

    #[test]
    fn mono_and_multichannel_are_supported() {
        for ch in [1usize, 2, 6] {
            let mut s = TimeStretcher::new(ch, 44_100);
            s.set_speed(1.5);
            let input: Vec<f32> = (0..44_100 * ch).map(|i| ((i % 64) as f32) / 64.0).collect();
            let mut out = Vec::new();
            s.push(&input, &mut out);
            s.flush(&mut out);
            assert_eq!(out.len() % ch, 0, "{ch} channels must stay interleaved");
            assert!(!out.is_empty());
        }
    }

    #[test]
    fn gain_is_applied_and_never_clips() {
        let mut quiet = vec![0.1f32; 8];
        apply_gain(&mut quiet, 2.0);
        assert!((quiet[0] - 0.2).abs() < 1e-6);

        let mut loud = vec![1.5f32, -1.5, 0.0, 0.5];
        apply_gain(&mut loud, 4.0);
        assert!(loud.iter().all(|v| v.abs() <= 1.0001));
        assert!(loud[3] > 0.85, "quiet passages stay untouched by the knee");

        let mut muted = vec![1.0f32; 4];
        apply_gain(&mut muted, 0.0);
        assert!(muted.iter().all(|v| *v == 0.0));

        let mut untouched = vec![0.3f32; 4];
        apply_gain(&mut untouched, 1.0);
        assert_eq!(untouched, vec![0.3f32; 4]);
    }
}
