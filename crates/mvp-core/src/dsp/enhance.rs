//! Real-time audio enhancement: the ten-band equaliser and the effects after it.
//!
//! # Where this runs
//!
//! In the device callback, on the real-time thread. That is the whole reason for the shape of
//! this module: nothing here allocates, locks, logs or touches the file system after
//! [`EnhanceChain::new`] — the buffers are sized once, from the device's own sample rate and
//! channel count, and the parameters arrive through atomics ([`EnhanceParams`]).
//!
//! # The chain, and why this order
//!
//! ```text
//! EQ (10 bands) → bass shelf + harmonics → clarity shelf + transients
//!               → loudness (K-weighted) → spatial (width + reverb) → soft limiter
//! ```
//!
//! * the equaliser first, because it is the stage the user is looking at: the curve on
//!   screen and the first thing the signal meets should be the same thing;
//! * bass and clarity next, so their thresholds mean the same thing under every preset;
//! * loudness *after* those, because it normalises what they produced — moving it earlier
//!   would have it chase every equaliser change;
//! * spatial before the limiter, because the tail it adds can raise peaks;
//! * and the limiter last, with nothing after it that could add gain.
//!
//! `dsp::apply_gain_sample` (the volume slider's own soft knee) still runs after this, so a
//! 200 % volume is protected by a second, independent ceiling.
//!
//! # What "off" means
//!
//! [`Snapshot::bypass`] — the default — makes [`EnhanceChain::process`] return without
//! touching a single sample, and [`EnhanceParams::bypassed`] is one relaxed atomic load, so
//! the callback does not even enter the chain.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::biquad::{Biquad, Coeffs};

/// Number of equaliser bands. Fixed, so the parameter block is a flat array and the
/// interface, the preset table and the DSP all agree without allocating.
pub const EQ_BANDS: usize = 10;

/// Centre frequencies, in Hz: the classic ten-band octave layout.
pub const EQ_FREQS: [f32; EQ_BANDS] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

/// How long a parameter takes to travel to its new value.
///
/// Fast enough to feel immediate while a slider is dragged, slow enough that the change is a
/// glide rather than a staircase — this is the anti-zipper control, applied per *block*
/// ([`BLOCK_SMOOTHING`]) rather than per sample.
const SMOOTHING_SECONDS: f32 = 0.030;

/// Bass and clarity shelf corners.
const BASS_CORNER_HZ: f32 = 120.0;
const CLARITY_CORNER_HZ: f32 = 6_000.0;

/// One consistent set of parameters, as the DSP reads them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    /// Master bypass. `true` means the chain is not called at all.
    pub bypass: bool,
    /// Per-band gain in dB, `-12.0..=12.0`.
    pub eq_gain: [f32; EQ_BANDS],
    /// Q of the peaking bands, `0.3..=6.0`.
    pub eq_q: f32,
    /// Low-shelf bass boost in dB, `0.0..=12.0`.
    pub bass_db: f32,
    /// Second-harmonic bass content, `0.0..=1.0`.
    pub bass_harmonics: f32,
    /// High-shelf clarity in dB, `0.0..=12.0`.
    pub clarity_db: f32,
    /// Transient emphasis, `0.0..=1.0`.
    pub clarity_transient: f32,
    /// Loudness target in dBFS; below `-59.0` disables the normaliser.
    pub loudness_target_db: f32,
    /// How fast the normaliser follows, in seconds.
    pub loudness_speed_s: f32,
    /// True-peak ceiling in dBFS, `-6.0..=0.0`.
    pub limiter_ceiling_db: f32,
    /// Stereo width, `0.0..=2.0` (`1.0` is untouched).
    pub spatial_width: f32,
    /// Amount of spatial tail: `0.0` is off, `1.0` is a large, long room. What it moves is both
    /// how loud the reverb is and how long it rings.
    pub spatial_room: f32,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            bypass: true, // ships off
            eq_gain: [0.0; EQ_BANDS],
            eq_q: 1.0,
            bass_db: 0.0,
            bass_harmonics: 0.0,
            clarity_db: 0.0,
            clarity_transient: 0.0,
            loudness_target_db: -60.0, // below the -59 threshold: normalisation off
            loudness_speed_s: 2.0,
            limiter_ceiling_db: -0.3,
            spatial_width: 1.0,
            spatial_room: 0.0,
        }
    }
}

impl Snapshot {
    /// `true` when every stage would do nothing, so the chain can be skipped entirely.
    pub fn is_idle(&self) -> bool {
        self.eq_gain.iter().all(|g| g.abs() < 1e-4)
            && self.bass_db.abs() < 1e-4
            && self.bass_harmonics.abs() < 1e-4
            && self.clarity_db.abs() < 1e-4
            && self.clarity_transient.abs() < 1e-4
            && self.loudness_target_db <= -59.0
            && (self.spatial_width - 1.0).abs() < 1e-4
            && self.spatial_room.abs() < 1e-4
    }

    /// Pull everything into the range the DSP is built for.
    ///
    /// `settings.json` is a file people edit by hand, and a wild number here becomes a filter
    /// coefficient — this is the last place that can stop it.
    pub fn clamp(&mut self) {
        for gain in &mut self.eq_gain {
            *gain = clamp_finite(*gain, -12.0, 12.0);
        }
        self.eq_q = clamp_finite(self.eq_q, 0.3, 6.0);
        self.bass_db = clamp_finite(self.bass_db, 0.0, 12.0);
        self.bass_harmonics = clamp_finite(self.bass_harmonics, 0.0, 1.0);
        self.clarity_db = clamp_finite(self.clarity_db, 0.0, 12.0);
        self.clarity_transient = clamp_finite(self.clarity_transient, 0.0, 1.0);
        self.loudness_target_db = clamp_finite(self.loudness_target_db, -60.0, -10.0);
        self.loudness_speed_s = clamp_finite(self.loudness_speed_s, 0.5, 10.0);
        self.limiter_ceiling_db = clamp_finite(self.limiter_ceiling_db, -6.0, 0.0);
        self.spatial_width = clamp_finite(self.spatial_width, 0.0, 2.0);
        self.spatial_room = clamp_finite(self.spatial_room, 0.0, 1.0);
    }

    /// Move every parameter a fraction `alpha` of the way towards `target`.
    ///
    /// The *parameters* are smoothed, not the coefficients: re-deriving a biquad per sample
    /// from a moving gain is exactly what produces the soft chirp people call zipper noise.
    fn lerp_towards(&mut self, target: &Snapshot, alpha: f32) {
        let one = 1.0 - alpha;
        let mix = |from: &mut f32, to: f32| *from = *from * one + to * alpha;
        for (from, to) in self.eq_gain.iter_mut().zip(target.eq_gain.iter()) {
            mix(from, *to);
        }
        mix(&mut self.eq_q, target.eq_q);
        mix(&mut self.bass_db, target.bass_db);
        mix(&mut self.bass_harmonics, target.bass_harmonics);
        mix(&mut self.clarity_db, target.clarity_db);
        mix(&mut self.clarity_transient, target.clarity_transient);
        mix(&mut self.loudness_target_db, target.loudness_target_db);
        mix(&mut self.spatial_width, target.spatial_width);
        mix(&mut self.spatial_room, target.spatial_room);
        // The ceiling is deliberately *not* smoothed: a safety limit that glides is a safety
        // limit that is briefly wrong.
        self.limiter_ceiling_db = target.limiter_ceiling_db;
        self.loudness_speed_s = target.loudness_speed_s;
        self.bypass = target.bypass;
    }

    /// `true` when this set is close enough to `other` that rebuilding coefficients would be
    /// wasted work.
    fn close_to(&self, other: &Snapshot) -> bool {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        self.eq_gain
            .iter()
            .zip(other.eq_gain.iter())
            .all(|(a, b)| near(*a, *b))
            && near(self.eq_q, other.eq_q)
            && near(self.bass_db, other.bass_db)
            && near(self.bass_harmonics, other.bass_harmonics)
            && near(self.clarity_db, other.clarity_db)
            && near(self.clarity_transient, other.clarity_transient)
            && near(self.spatial_width, other.spatial_width)
            && near(self.spatial_room, other.spatial_room)
            && near(self.loudness_target_db, other.loudness_target_db)
    }
}

fn clamp_finite(value: f32, low: f32, high: f32) -> f32 {
    if value.is_finite() {
        value.clamp(low, high)
    } else {
        low
    }
}

/// Where each parameter lives in the atomic block.
const P_EQ_Q: usize = 0;
const P_BASS: usize = 1;
const P_BASS_HARM: usize = 2;
const P_CLARITY: usize = 3;
const P_CLARITY_TRANS: usize = 4;
const P_LOUD_TARGET: usize = 5;
const P_LOUD_SPEED: usize = 6;
const P_LIMIT_CEIL: usize = 7;
const P_SPATIAL_WIDTH: usize = 8;
const P_SPATIAL_ROOM: usize = 9;
const P_EQ_0: usize = 10;
const PARAM_COUNT: usize = P_EQ_0 + EQ_BANDS;

/// The parameter block the interface publishes and the device callback reads.
///
/// Atomics plus a generation counter rather than a lock: the callback runs on a real-time
/// thread and the rule in `audio.rs` is that it never blocks, never allocates and never waits
/// for the interface. The generation is a seqlock — odd while a write is in flight — so the
/// callback reads one *consistent* set rather than a half-updated one, which is what a preset
/// change would otherwise sound like.
#[derive(Debug)]
pub struct EnhanceParams {
    generation: AtomicU64,
    values: [AtomicU32; PARAM_COUNT],
    /// Mirror of the bypass flag as a plain bool: the callback reads this first and, when it
    /// is set, never touches the rest of the block.
    bypass: AtomicBool,
    /// Set once a chain has been built for the open device. The interface shows a skeleton row
    /// until then, and a hint when it never arrives (no output device at all).
    pub ready: AtomicBool,
    /// Delay the chain adds, as `f32` bits. Written by the chain, read by the interface.
    pub latency_ms: AtomicU32,
}

impl Default for EnhanceParams {
    fn default() -> Self {
        let params = Self {
            generation: AtomicU64::new(0),
            values: std::array::from_fn(|_| AtomicU32::new(0)),
            bypass: AtomicBool::new(true),
            ready: AtomicBool::new(false),
            latency_ms: AtomicU32::new(0.0f32.to_bits()),
        };
        params.publish(Snapshot::default());
        params
    }
}

impl EnhanceParams {
    /// Publish a whole set. Cheap enough to call on every slider frame: twenty relaxed stores.
    pub fn publish(&self, mut snapshot: Snapshot) {
        snapshot.clamp();
        let start = self.generation.load(Ordering::Relaxed);
        self.generation.store(start.wrapping_add(1), Ordering::Relaxed); // odd: writing
        for (index, gain) in snapshot.eq_gain.iter().enumerate() {
            self.put(P_EQ_0 + index, *gain);
        }
        self.put(P_EQ_Q, snapshot.eq_q);
        self.put(P_BASS, snapshot.bass_db);
        self.put(P_BASS_HARM, snapshot.bass_harmonics);
        self.put(P_CLARITY, snapshot.clarity_db);
        self.put(P_CLARITY_TRANS, snapshot.clarity_transient);
        self.put(P_LOUD_TARGET, snapshot.loudness_target_db);
        self.put(P_LOUD_SPEED, snapshot.loudness_speed_s);
        self.put(P_LIMIT_CEIL, snapshot.limiter_ceiling_db);
        self.put(P_SPATIAL_WIDTH, snapshot.spatial_width);
        self.put(P_SPATIAL_ROOM, snapshot.spatial_room);
        self.generation.store(start.wrapping_add(2), Ordering::Relaxed); // even: settled
        self.bypass.store(snapshot.bypass, Ordering::Relaxed);
    }

    /// Read one consistent set. Called once per audio block, from the device callback.
    ///
    /// Four attempts, then the default: the writer is the interface thread and only publishes
    /// when a control moves, so a retry is already the rare case — and a bounded loop is what
    /// keeps a pathological writer from stalling the real-time thread.
    pub fn load(&self) -> Snapshot {
        for _ in 0..4 {
            let before = self.generation.load(Ordering::Acquire);
            if before & 1 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let snapshot = Snapshot {
                bypass: self.bypass.load(Ordering::Relaxed),
                eq_gain: std::array::from_fn(|index| self.get(P_EQ_0 + index)),
                eq_q: self.get(P_EQ_Q),
                bass_db: self.get(P_BASS),
                bass_harmonics: self.get(P_BASS_HARM),
                clarity_db: self.get(P_CLARITY),
                clarity_transient: self.get(P_CLARITY_TRANS),
                loudness_target_db: self.get(P_LOUD_TARGET),
                loudness_speed_s: self.get(P_LOUD_SPEED),
                limiter_ceiling_db: self.get(P_LIMIT_CEIL),
                spatial_width: self.get(P_SPATIAL_WIDTH),
                spatial_room: self.get(P_SPATIAL_ROOM),
            };
            if self.generation.load(Ordering::Acquire) == before {
                return snapshot;
            }
        }
        Snapshot::default()
    }

    /// One relaxed read of the bypass flag, for the callback's fast path.
    pub fn bypassed(&self) -> bool {
        self.bypass.load(Ordering::Relaxed)
    }

    /// Delay of the built chain, in milliseconds.
    pub fn latency_ms(&self) -> f32 {
        f32::from_bits(self.latency_ms.load(Ordering::Relaxed))
    }

    #[inline]
    fn put(&self, index: usize, value: f32) {
        if let Some(slot) = self.values.get(index) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
    }

    #[inline]
    fn get(&self, index: usize) -> f32 {
        match self.values.get(index) {
            Some(slot) => f32::from_bits(slot.load(Ordering::Relaxed)),
            None => 0.0,
        }
    }
}

/// One equaliser band: coefficients plus one biquad of state per channel.
struct Band {
    coeffs: Coeffs,
    state: Vec<Biquad>,
}

impl Band {
    fn new(channels: usize) -> Self {
        Self { coeffs: Coeffs::IDENTITY, state: vec![Biquad::default(); channels] }
    }

    /// Rebuild for the current sample rate, frequency and gain.
    ///
    /// The frequency is clamped below Nyquist: at 8 kHz the 16 kHz band would otherwise ask
    /// for a filter that cannot exist.
    fn rebuild(&mut self, sample_rate: f32, freq: f32, q: f32, gain_db: f32) {
        self.coeffs = if gain_db.abs() < 1e-4 {
            Coeffs::IDENTITY
        } else {
            Coeffs::peaking(sample_rate, freq.min(sample_rate * 0.45), q, gain_db)
        };
        for biquad in &mut self.state {
            biquad.set(self.coeffs);
        }
    }

    #[inline]
    fn process(&mut self, channel: usize, x: f32) -> f32 {
        if self.coeffs.is_identity() {
            return x;
        }
        match self.state.get_mut(channel) {
            Some(biquad) => biquad.process(x),
            None => x,
        }
    }

    fn reset(&mut self) {
        for biquad in &mut self.state {
            biquad.reset();
        }
    }
}

/// A single biquad applied to every channel with the same coefficients (a shelf, a filter).
struct Shelf {
    coeffs: Coeffs,
    state: Vec<Biquad>,
}

impl Shelf {
    fn new(channels: usize) -> Self {
        Self { coeffs: Coeffs::IDENTITY, state: vec![Biquad::default(); channels] }
    }

    fn rebuild(&mut self, coeffs: Coeffs) {
        if self.coeffs != coeffs {
            self.coeffs = coeffs;
            for biquad in &mut self.state {
                biquad.set(coeffs);
            }
        }
    }

    #[inline]
    fn process(&mut self, channel: usize, x: f32) -> f32 {
        if self.coeffs.is_identity() {
            return x;
        }
        match self.state.get_mut(channel) {
            Some(biquad) => biquad.process(x),
            None => x,
        }
    }

    fn reset(&mut self) {
        for biquad in &mut self.state {
            biquad.reset();
        }
    }
}

/// Transient emphasis: what "clarity" mostly is.
///
/// The difference between a fast and a slow envelope follower is loud exactly at the start of
/// a note, which is where clarity is heard; a plain high shelf (the `clarity` field) does the
/// rest of the job.
struct Transient {
    fast: Vec<f32>,
    slow: Vec<f32>,
}

impl Transient {
    fn new(channels: usize) -> Self {
        Self { fast: vec![0.0; channels], slow: vec![0.0; channels] }
    }

    fn reset(&mut self) {
        self.fast.iter_mut().for_each(|s| *s = 0.0);
        self.slow.iter_mut().for_each(|s| *s = 0.0);
    }

    #[inline]
    fn process(&mut self, channel: usize, x: f32, amount: f32) -> f32 {
        let (Some(fast), Some(slow)) = (self.fast.get_mut(channel), self.slow.get_mut(channel))
        else {
            return x;
        };
        let level = x.abs();
        *fast += (level - *fast) * 0.35;
        *slow += (level - *slow) * 0.002;
        let attack = (*fast - *slow).max(0.0);
        x * (1.0 + attack * amount * 2.0)
    }
}

/// Loudness normalisation: a K-weighted short-term level and one slow gain dragging the signal
/// towards the target.
///
/// Approximate on purpose — a full BS.1770 measurement adds gating and an integrated average
/// over seconds, which is a mastering tool's job rather than a player's. What this does is the
/// part a listener notices: dialogue and music stop being twenty decibels apart.
struct Loudness {
    shelf: Shelf,
    high_pass: Shelf,
    power: Vec<f32>,
    gain: f32,
    speed: f32,
}

impl Loudness {
    fn new(sample_rate: f32, channels: usize) -> Self {
        let mut me = Self {
            shelf: Shelf::new(channels),
            high_pass: Shelf::new(channels),
            power: vec![0.0; channels],
            gain: 1.0,
            speed: 2.0,
        };
        // ITU-R BS.1770 K-weighting, as the two biquads it is defined as.
        me.shelf.rebuild(Coeffs::high_shelf(sample_rate, 1_682.0, 4.0, 0.707));
        me.high_pass.rebuild(Coeffs::high_pass(sample_rate, 38.0, 0.5));
        me
    }

    fn reset(&mut self) {
        self.shelf.reset();
        self.high_pass.reset();
        self.power.iter_mut().for_each(|p| *p = 0.0);
        self.gain = 1.0;
    }

    /// Measure this frame and apply the correcting gain to it.
    #[inline]
    fn process_frame(&mut self, frame: &mut [f32], s: &Snapshot) {
        if s.loudness_target_db <= -59.0 {
            // Normalisation is off — that is the default — so the gain glides back to unity
            // instead of snapping (which would be a step in level) and no measurement runs, so
            // a disabled normaliser cannot move the level at all.
            self.gain += (1.0 - self.gain) * 0.05;
            if (self.gain - 1.0).abs() < 1e-4 {
                self.gain = 1.0;
            }
            for sample in frame.iter_mut() {
                *sample *= self.gain;
            }
            return;
        }
        self.speed = s.loudness_speed_s;
        let frames = frame.len().max(1) as f32;
        // One-pole over roughly the stated following time (48 blocks per second, four blocks
        // per decision: the exact constant matters far less than its being slow).
        let alpha = (1.0 / (self.speed.max(0.1) * 4_800.0 * frames)).min(1.0);
        let channels = frame.len().min(self.power.len());
        for channel in 0..channels {
            let weighted =
                self.high_pass.process(channel, self.shelf.process(channel, frame[channel]));
            self.power[channel] += (weighted * weighted - self.power[channel]) * alpha;
        }
        let count = channels.max(1) as f32;
        let mean_power = self.power.iter().take(channels.max(1)).sum::<f32>() / count;
        let level_db = 10.0 * mean_power.max(1e-12).log10();
        // Bounded: a normaliser that can jump twelve decibels is a normaliser that pumps.
        let wanted_db = (s.loudness_target_db - level_db).clamp(-12.0, 12.0);
        self.gain += (10f32.powf(wanted_db / 20.0) - self.gain) * alpha.min(0.05);
        for sample in frame.iter_mut() {
            *sample *= self.gain;
        }
    }
}

/// Comb delays in samples, quoted at 44.1 kHz: mutually prime, so their resonances do not line
/// up into a pitched artefact.
const COMB_TUNING: [usize; 8] = [1_116, 1_188, 1_277, 1_356, 1_422, 1_491, 1_557, 1_617];

/// Allpass delays in samples at 44.1 kHz: the diffusion that turns eight echoes into a tail.
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];

/// Samples added to the right channel's delays. This is what makes the tail *stereo*: two
/// channels fed the same signal from differently tuned lines decorrelate, which is heard as the
/// sound leaving the speakers instead of sitting between them.
const STEREO_SPREAD: usize = 23;

/// How long the tail is held back before it starts, in seconds.
///
/// Short enough to still read as the same event, long enough that the reflections are not a
/// comb filter over the dry signal — which is what the previous single-tap version actually was.
const PREDELAY_SECONDS: f32 = 0.018;

/// Comb feedback at room `0.0` and at room `1.0`. The gap is the other half of what the control
/// does: a small room stops in a tenth of a second, a large one rings for about two seconds.
const FEEDBACK_MIN: f32 = 0.70;
const FEEDBACK_MAX: f32 = 0.97;

/// High-frequency damping inside the comb loops: the tail loses its highs as it decays, the way
/// a real room does.
const DAMPING: f32 = 0.25;

/// Allpass feedback. Diffuses without colouring: an allpass passes every frequency at the same
/// level and only scrambles phase.
const ALLPASS_FEEDBACK: f32 = 0.5;

/// Wet level at room `1.0`, before the room curve and the bank's own normalisation. Calibrated by
/// `the_room_control_leaves_an_audible_tail_behind_the_sound`: at the top of the slider the tail
/// is plainly there, and still a little under the dry signal rather than over it.
const WET_MAX: f32 = 0.30;

/// One damped comb filter: a delay line whose feedback runs through a one-pole low pass.
struct Comb {
    buffer: Vec<f32>,
    write: usize,
    damping: f32,
    store: f32,
}

impl Comb {
    fn new(samples: usize) -> Self {
        Self { buffer: vec![0.0; samples.max(1)], write: 0, damping: DAMPING, store: 0.0 }
    }

    fn reset(&mut self) {
        self.buffer.iter_mut().for_each(|sample| *sample = 0.0);
        self.write = 0;
        self.store = 0.0;
    }

    /// One sample in, the echo from `buffer.len()` samples ago out.
    #[inline]
    fn process(&mut self, input: f32, feedback: f32) -> f32 {
        let echo = self.buffer.get(self.write).copied().unwrap_or(0.0);
        self.store = echo * (1.0 - self.damping) + self.store * self.damping;
        if let Some(slot) = self.buffer.get_mut(self.write) {
            *slot = input + self.store * feedback;
        }
        self.advance();
        echo
    }

    #[inline]
    fn advance(&mut self) {
        self.write += 1;
        if self.write >= self.buffer.len() {
            self.write = 0;
        }
    }
}

/// One allpass: flat response, scrambled phase.
struct Allpass {
    buffer: Vec<f32>,
    write: usize,
}

impl Allpass {
    fn new(samples: usize) -> Self {
        Self { buffer: vec![0.0; samples.max(1)], write: 0 }
    }

    fn reset(&mut self) {
        self.buffer.iter_mut().for_each(|sample| *sample = 0.0);
        self.write = 0;
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32) -> f32 {
        let buffered = self.buffer.get(self.write).copied().unwrap_or(0.0);
        if let Some(slot) = self.buffer.get_mut(self.write) {
            *slot = input + buffered * feedback;
        }
        self.write += 1;
        if self.write >= self.buffer.len() {
            self.write = 0;
        }
        buffered - input
    }
}

/// The spatial stage: a mid/side width control and a small Schroeder reverb.
///
/// Deliberately *not* an HRIR convolution: a 128-tap HRIR per ear at 48 kHz is 512 multiply
/// accumulates per sample, twenty times the whole equaliser. What is here is the cheap half of
/// "space": eight damped comb filters in parallel into four allpasses — the Schroeder layout
/// Freeverb made popular — with the two channels' delays nudged apart so their tails do not
/// correlate.
///
/// This stage used to be a single 12 ms cross-feed tap, and that is why the control could not be
/// heard: one tap is a comb filter, not a room. On real programme material it colours the sound
/// slightly instead of putting anything around it, and a mono source gets the *same* reflection
/// added to both channels, which collapses straight back to the middle. A bank of reflections
/// builds up a tail that reads as space, and per-channel tunings keep that tail wide even from a
/// mono file.
///
/// It is a **wet** stage: the tail is added to a dry signal that leaves untouched, so the stage
/// adds no delay of its own (only the limiter's look-ahead does) and a frame it does not want to
/// change comes out identical, sample for sample.
///
/// Only the first two channels are touched: a 5.1 stream keeps its centre and surrounds, which
/// are already positioned. The tail is built for two channels whatever the device width is — a
/// little over 100 kB, allocated once in [`Spatial::new`] and never in the callback.
struct Spatial {
    /// One bank of combs per channel, feeding one chain of allpasses per channel.
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
    /// Pre-delay per channel, so the reflections do not read as a doubled dry signal.
    predelay: Vec<f32>,
    write: usize,
    frames: usize,
    channels: usize,
    /// `true` while the tail is being fed. Switching the room off drops what is in the buffers
    /// rather than leaving it to leak back the next time the control is moved.
    ringing: bool,
}

impl Spatial {
    fn new(sample_rate: f32, channels: usize) -> Self {
        // The tunings are quoted at 44.1 kHz, so a 96 kHz device needs proportionally longer
        // lines for the same room.
        let scale = (sample_rate / 44_100.0).max(0.05);
        let combs = std::array::from_fn(|channel| {
            COMB_TUNING
                .iter()
                .map(|tuning| Comb::new(scaled(*tuning + spread_of(channel), scale)))
                .collect()
        });
        let allpasses = std::array::from_fn(|channel| {
            ALLPASS_TUNING
                .iter()
                .map(|tuning| Allpass::new(scaled(*tuning + spread_of(channel), scale)))
                .collect()
        });
        let frames = ((sample_rate * PREDELAY_SECONDS).round() as usize).clamp(1, 8_192);
        Self {
            combs,
            allpasses,
            predelay: vec![0.0; frames * 2],
            write: 0,
            frames,
            channels,
            ringing: false,
        }
    }

    /// Drop everything in the buffers, so a switch back on starts from silence rather than from
    /// whatever was playing when it was switched off.
    fn reset(&mut self) {
        for bank in &mut self.combs {
            for comb in bank.iter_mut() {
                comb.reset();
            }
        }
        for bank in &mut self.allpasses {
            for allpass in bank.iter_mut() {
                allpass.reset();
            }
        }
        self.predelay.iter_mut().for_each(|sample| *sample = 0.0);
        self.write = 0;
        self.ringing = false;
    }

    #[inline]
    fn process_frame(&mut self, frame: &mut [f32], s: &Snapshot) {
        if self.channels < 2 || frame.len() < 2 {
            return;
        }
        // `1.0` is the untouched width, and `spatial_width` is clamped to `0.0..=2.0`.
        let widened = (s.spatial_width - 1.0).abs() > 1e-4;
        let reverberant = s.spatial_room > 1e-4;
        if !widened && !reverberant {
            // Both controls are at their do-nothing position, so the frame leaves untouched —
            // this is what keeps "on but not spatial" identical to "off" even while the other
            // stages are working.
            if self.ringing {
                self.reset();
            }
            return;
        }

        let (dry_left, dry_right) = (frame[0], frame[1]);
        let (mut left, mut right) = if widened {
            let mid = (dry_left + dry_right) * 0.5;
            let side = (dry_left - dry_right) * 0.5 * s.spatial_width.max(0.0);
            (mid + side, mid - side)
        } else {
            (dry_left, dry_right)
        };

        if reverberant {
            let (wet_left, wet_right) = self.reverb(left, right, s.spatial_room);
            left += wet_left;
            right += wet_right;
            self.ringing = true;
        } else if self.ringing {
            // The room just went back to zero: drop the tail rather than leave it sitting in the
            // buffers until the control is moved again.
            self.reset();
        }

        frame[0] = left;
        frame[1] = right;
    }

    /// Push one frame through the reverb, returning what should be *added* to the dry signal.
    ///
    /// The level carries two things: the room curve, and the comb bank's own normalisation. A
    /// comb with feedback `f` amplifies a sustained signal by about `1 / (1 - f)`, so without the
    /// second term the control would trade tail length against loudness and the top of the slider
    /// would come out *quieter* than the middle. It is deliberately under-normalised (see
    /// [`comb_normalisation`]) so the reflections a listener notices first stay audible.
    fn reverb(&mut self, left: f32, right: f32, room: f32) -> (f32, f32) {
        let (delayed_left, delayed_right) = self.predelay_frame(left, right);
        let feedback = FEEDBACK_MIN + (FEEDBACK_MAX - FEEDBACK_MIN) * room;

        let mut wet_left = 0.0;
        let mut wet_right = 0.0;
        for comb in &mut self.combs[0] {
            wet_left += comb.process(delayed_left, feedback);
        }
        for comb in &mut self.combs[1] {
            wet_right += comb.process(delayed_right, feedback);
        }
        for allpass in &mut self.allpasses[0] {
            wet_left = allpass.process(wet_left, ALLPASS_FEEDBACK);
        }
        for allpass in &mut self.allpasses[1] {
            wet_right = allpass.process(wet_right, ALLPASS_FEEDBACK);
        }

        // Squared rather than linear: the bottom of the slider is a hint of space and the top is a
        // hall. Measured at the top: the tail just after the sound stops is about the level of the
        // sound itself, and half a second later it is still a fifth of it.
        let wet = WET_MAX * (room * room) * comb_normalisation(feedback);
        (wet_left * wet, wet_right * wet)
    }

    /// One frame in, the frame from `PREDELAY_SECONDS` ago out.
    #[inline]
    fn predelay_frame(&mut self, left: f32, right: f32) -> (f32, f32) {
        let slot = self.write * 2;
        let out = (
            self.predelay.get(slot).copied().unwrap_or(0.0),
            self.predelay.get(slot + 1).copied().unwrap_or(0.0),
        );
        if let Some(sample) = self.predelay.get_mut(slot) {
            *sample = left;
        }
        if let Some(sample) = self.predelay.get_mut(slot + 1) {
            *sample = right;
        }
        self.write += 1;
        if self.write >= self.frames {
            self.write = 0;
        }
        out
    }
}

/// One tuning, scaled from the 44.1 kHz it is quoted at to this device's rate.
fn scaled(samples: usize, scale: f32) -> usize {
    ((samples as f32 * scale).round() as usize).max(1)
}

/// How many samples to add to a channel's delays. The left channel keeps the nominal tunings;
/// the right one is nudged, which is what decorrelates the two tails.
fn spread_of(channel: usize) -> usize {
    if channel == 0 {
        0
    } else {
        STEREO_SPREAD
    }
}

/// The reciprocal of what the comb bank does to a sustained signal.
///
/// Eight combs with delays long enough to be mutually incoherent add in power, so the bank's gain
/// is `sqrt(1 / (1 - f)) * sqrt(8)` in RMS. Dividing by an under-scaled version of that
/// (`f^-0.85` rather than `f^-1`) keeps the tail's level following the room control instead of
/// fighting it — the knob lengthens the tail *and* fills it — while leaving the first reflections
/// a few decibels proud, which is what makes the effect audible the moment it is switched on.
fn comb_normalisation(feedback: f32) -> f32 {
    let per_comb = (1.0 / (1.0 - feedback).max(1e-3)).sqrt();
    let bank = per_comb * (COMB_TUNING.len() as f32).sqrt();
    (1.0 / bank.max(1.0)).powf(0.85)
}

/// The safety stage: a look-ahead peak limiter with a `tanh` knee.
///
/// Look-ahead rather than a plain clipper, because a clipper distorts *before* it limits — and
/// the whole point of this stage is that the fifteen decibels a preset can add never reach the
/// device as a square wave. Its delay is the only latency the chain has, and the interface
/// reports it.
struct Limiter {
    delay: Vec<f32>,
    write: usize,
    channels: usize,
    lookahead: usize,
    gain: f32,
    release: f32,
    ceiling: f32,
}

impl Limiter {
    fn new(sample_rate: f32, channels: usize) -> Self {
        let lookahead = ((sample_rate * 0.001).round() as usize).clamp(1, 4_096);
        Self {
            delay: vec![0.0; lookahead * channels.max(1)],
            write: 0,
            channels: channels.max(1),
            lookahead,
            gain: 1.0,
            // ~50 ms release: slow enough not to pump, fast enough to recover between hits.
            release: 1.0 - (-1.0 / (sample_rate * 0.050).max(1.0)).exp(),
            ceiling: 10f32.powf(-0.3 / 20.0),
        }
    }

    fn latency_samples(&self) -> usize {
        self.lookahead
    }

    fn reset(&mut self) {
        self.delay.iter_mut().for_each(|s| *s = 0.0);
        self.write = 0;
        self.gain = 1.0;
    }

    /// Empty the delay line, keeping the gain. Used when the chain goes idle, so a bypassed
    /// chain neither holds stale audio nor moves the playhead.
    fn clear_delay(&mut self) {
        self.delay.iter_mut().for_each(|s| *s = 0.0);
        self.write = 0;
    }

    /// Limit one frame, in place.
    ///
    /// The reduction comes from the **maximum** across the channels and is applied to all of
    /// them: per-channel limiting moves the stereo image, which is audible as a singer
    /// wandering off centre during a loud passage.
    #[inline]
    fn process_frame(&mut self, frame: &mut [f32], ceiling_db: f32) {
        self.ceiling = 10f32.powf(ceiling_db / 20.0);
        let channels = self.channels.min(frame.len());
        if channels == 0 || self.delay.len() < channels {
            return;
        }
        let slot = (self.write * channels) % self.delay.len();

        let mut peak = 0.0f32;
        for channel in 0..channels {
            let delayed = self.delay.get(slot + channel).copied().unwrap_or(0.0);
            peak = peak.max(delayed.abs()).max(frame[channel].abs());
        }

        let wanted = if peak > self.ceiling { self.ceiling / peak } else { 1.0 };
        if wanted < self.gain {
            // Instantaneous attack: the peak that caused it is still inside the delay line.
            self.gain = wanted;
        } else {
            self.gain += (wanted - self.gain) * self.release;
        }

        for channel in 0..channels {
            let Some(delayed) = self.delay.get_mut(slot + channel) else {
                continue;
            };
            let incoming = frame[channel];
            let outgoing = *delayed * self.gain;
            *delayed = incoming;
            frame[channel] = soft_knee(outgoing);
        }

        self.write = (self.write + 1) % self.lookahead;
    }
}

/// Everything below −3 dBFS passes untouched; everything above is folded into the remaining
/// headroom by a `tanh`.
///
/// Below the knee this is *exactly* transparent, sample for sample, which is what lets the
/// tests assert that a quiet passage survives the chain bit-identically.
#[inline]
fn soft_knee(x: f32) -> f32 {
    const KNEE: f32 = 0.707_945_8; // −3 dBFS
    if x.abs() <= KNEE {
        x
    } else {
        let sign = x.signum();
        sign * (KNEE + (1.0 - KNEE) * ((x.abs() - KNEE) / (1.0 - KNEE)).tanh())
    }
}

/// The whole chain, owned by the device callback.
///
/// Every buffer is sized in [`EnhanceChain::new`] and never grows: `process` takes `&mut self`
/// and `&EnhanceParams`, and touches no allocator, no lock and no clock.
pub struct EnhanceChain {
    sample_rate: f32,
    channels: usize,
    bands: Vec<Band>,
    bass: Shelf,
    clarity: Shelf,
    transient: Transient,
    loudness: Loudness,
    spatial: Spatial,
    limiter: Limiter,
    /// The smoothed, currently applied set — not the target.
    current: Snapshot,
    /// One frame, reused. Taken out with `mem::take` so the per-frame methods can take
    /// `&mut self`.
    scratch: Vec<f32>,
    /// The delay lines hold audio, so they must be cleared when the chain goes idle.
    holding: bool,
}

impl EnhanceChain {
    /// Build for a device. Allocation happens here and only here.
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let sample_rate = sample_rate.max(8_000) as f32;
        let channels = (channels.max(1)) as usize;
        let mut chain = Self {
            sample_rate,
            channels,
            bands: (0..EQ_BANDS).map(|_| Band::new(channels)).collect(),
            bass: Shelf::new(channels),
            clarity: Shelf::new(channels),
            transient: Transient::new(channels),
            loudness: Loudness::new(sample_rate, channels),
            spatial: Spatial::new(sample_rate, channels),
            limiter: Limiter::new(sample_rate, channels),
            current: Snapshot::default(),
            scratch: vec![0.0; channels],
            holding: false,
        };
        chain.rebuild();
        chain
    }

    /// Delay the chain adds, in milliseconds. The interface shows this.
    ///
    /// Only the limiter's look-ahead counts. The spatial stage is *wet*: it adds its tail to a dry
    /// signal it leaves alone, so it delays nothing — the 12 ms the single cross-feed tap used to
    /// contribute was a phase offset, not latency, and reporting it told the interface to show a
    /// number that did not describe what was heard.
    pub fn latency_ms(&self) -> f32 {
        let samples = self.limiter.latency_samples();
        samples as f32 / self.sample_rate * 1_000.0
    }

    /// Process one interleaved block, in place. The only entry point the callback uses.
    pub fn process(&mut self, samples: &mut [f32], params: &EnhanceParams) {
        let channels = self.channels.max(1);
        let frames = samples.len() / channels;

        if params.bypassed() {
            // Clear the delay lines once, on the way out: leaving a millisecond of stale audio
            // in them would make the moment the chain is switched back on a ghost of what was
            // playing before it was switched off.
            if self.holding {
                self.limiter.clear_delay();
                self.spatial.reset();
                self.holding = false;
            }
            self.current = Snapshot::default();
            return;
        }

        let target = params.load();
        if !self.current.close_to(&target) {
            // The fraction of the distance to close comes from the *time* the glide should
            // take, not from the block size: a driver with 256-frame buffers must not smooth
            // eight times faster than one with 2048.
            let elapsed = frames as f32 / self.sample_rate;
            self.current
                .lerp_towards(&target, (elapsed / SMOOTHING_SECONDS).clamp(0.02, 1.0));
            self.rebuild();
        }

        if self.current.is_idle() {
            // Every control is at its neutral position. Getting out here is what makes "on but
            // untouched" indistinguishable from "off" — and skipping the limiter is safe
            // because nothing upstream added any gain.
            if self.holding {
                self.limiter.clear_delay();
                self.spatial.reset();
                self.holding = false;
            }
            return;
        }

        if frames == 0 {
            return;
        }
        self.holding = true;
        let snapshot = self.current;
        let mut scratch = std::mem::take(&mut self.scratch);

        for index in 0..frames {
            let base = index * channels;
            let Some(frame) = scratch.get_mut(..channels) else {
                break;
            };
            for channel in 0..channels {
                frame[channel] = samples.get(base + channel).copied().unwrap_or(0.0);
            }
            self.process_frame(frame, &snapshot);
            for channel in 0..channels {
                if let Some(target) = samples.get_mut(base + channel) {
                    *target = frame[channel];
                }
            }
        }

        self.scratch = scratch;
    }

    /// One frame (all channels of one time step) through the whole chain.
    #[inline]
    fn process_frame(&mut self, frame: &mut [f32], s: &Snapshot) {
        for (channel, sample) in frame.iter_mut().enumerate() {
            let mut x = *sample;
            for band in &mut self.bands {
                x = band.process(channel, x);
            }
            x = self.bass.process(channel, x);
            if s.bass_harmonics > 1e-4 {
                x = bass_harmonic(x, s.bass_harmonics);
            }
            x = self.clarity.process(channel, x);
            x = self.transient.process(channel, x, s.clarity_transient);
            *sample = x;
        }
        self.loudness.process_frame(frame, s);
        self.spatial.process_frame(frame, s);
        self.limiter.process_frame(frame, s.limiter_ceiling_db);
    }

    /// Re-derive the coefficients from the smoothed parameters. At most once per block.
    fn rebuild(&mut self) {
        let (s, rate) = (self.current, self.sample_rate);
        for (index, band) in self.bands.iter_mut().enumerate() {
            let freq = EQ_FREQS.get(index).copied().unwrap_or(1_000.0);
            let gain = s.eq_gain.get(index).copied().unwrap_or(0.0);
            band.rebuild(rate, freq, s.eq_q, gain);
        }
        self.bass.rebuild(shelf_low(rate, BASS_CORNER_HZ, s.bass_db));
        self.clarity.rebuild(shelf_high(rate, CLARITY_CORNER_HZ, s.clarity_db));
    }

    /// Forget every filter's history, and the smoothing state with it. Used after a seek: a
    /// burst of stale energy must not ring into the new position.
    pub fn reset(&mut self) {
        self.bands.iter_mut().for_each(Band::reset);
        self.bass.reset();
        self.clarity.reset();
        self.transient.reset();
        self.loudness.reset();
        self.spatial.reset();
        self.limiter.reset();
        self.current = Snapshot::default();
        self.holding = false;
    }
}

fn shelf_low(sample_rate: f32, freq: f32, gain_db: f32) -> Coeffs {
    if gain_db.abs() < 1e-4 {
        Coeffs::IDENTITY
    } else {
        Coeffs::low_shelf(sample_rate, freq, gain_db, 1.0)
    }
}

fn shelf_high(sample_rate: f32, freq: f32, gain_db: f32) -> Coeffs {
    if gain_db.abs() < 1e-4 {
        Coeffs::IDENTITY
    } else {
        Coeffs::high_shelf(sample_rate, freq, gain_db, 1.0)
    }
}

/// Second-harmonic content for the bass, without a pitch shift: a `tanh` term that only bites on
/// the loud half of the waveform, so a small speaker's missing fundamental is at least implied
/// by its octave.
#[inline]
fn bass_harmonic(x: f32, amount: f32) -> f32 {
    if x.abs() < 0.25 {
        x
    } else {
        x + (x * 2.0).tanh() * 0.5 * amount
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block of `frames` interleaved frames, from a generator of (frame, channel) -> sample.
    fn block(frames: usize, channels: usize, f: impl Fn(usize, usize) -> f32) -> Vec<f32> {
        let mut out = vec![0.0; frames * channels];
        for frame in 0..frames {
            for channel in 0..channels {
                out[frame * channels + channel] = f(frame, channel);
            }
        }
        out
    }

    /// 440 Hz at `peak`, the right channel half as loud as the left.
    fn tone(frames: usize, peak: f32) -> Vec<f32> {
        block(frames, 2, |frame, channel| {
            let gain = if channel == 0 { peak } else { peak * 0.5 };
            gain * (frame as f32 * 0.0576).sin()
        })
    }

    fn params(snapshot: Snapshot) -> EnhanceParams {
        let params = EnhanceParams::default();
        params.publish(snapshot);
        params
    }

    #[test]
    fn a_bypassed_chain_does_not_touch_a_single_sample() {
        let params = EnhanceParams::default(); // ships bypassed
        let mut chain = EnhanceChain::new(48_000, 2);
        let mut samples = tone(512, 0.9);
        let original = samples.clone();
        chain.process(&mut samples, &params);
        assert_eq!(samples, original, "a bypassed chain rewrote the buffer");
    }

    #[test]
    fn an_enabled_but_neutral_chain_is_bit_identical_to_a_bypassed_one() {
        // "Off" in this feature means the chain is not in the picture at all, and this is the
        // test that keeps it that way: switch it on, move nothing, hear nothing.
        let params = params(Snapshot { bypass: false, ..Snapshot::default() });
        let mut chain = EnhanceChain::new(48_000, 2);
        let mut samples = tone(2_048, 0.6);
        let original = samples.clone();
        chain.process(&mut samples, &params);
        assert_eq!(samples, original, "a neutral chain is not transparent");
    }

    #[test]
    fn the_soft_knee_is_exactly_transparent_below_its_knee() {
        for x in [0.0f32, 0.1, -0.25, 0.5, -0.707] {
            assert_eq!(soft_knee(x), x, "{x} was altered below the knee");
        }
        // Above the knee the fold is asymptotic: `tanh` saturates, so full scale is the
        // ceiling and never one decibel past it.
        assert!(soft_knee(4.0) <= 1.0, "a 4.0 peak escaped the ceiling");
        assert!(soft_knee(-4.0) >= -1.0);
        assert!(soft_knee(4.0) > 0.99, "the knee must not swallow a hot signal");
    }

    #[test]
    fn the_limiter_holds_a_hot_block_under_its_ceiling() {
        // The equaliser is moved by a hair so the chain is genuinely running, then a signal
        // thirteen decibels over the ceiling is pushed through it.
        let mut snapshot = Snapshot { bypass: false, ..Snapshot::default() };
        snapshot.eq_gain[EQ_BANDS - 1] = 0.5;
        let params = params(snapshot);
        let mut chain = EnhanceChain::new(48_000, 2);
        let mut samples = tone(4_800, 1.6);
        chain.process(&mut samples, &params);

        let ceiling = 10f32.powf(-0.3 / 20.0);
        let peak = samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
        assert!(peak <= ceiling, "the limiter let {peak} past a {ceiling} ceiling");
        assert!(peak > ceiling * 0.5, "the limiter swallowed the whole signal ({peak})");
        assert!(samples.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn the_limiter_gives_both_channels_the_same_gain() {
        // A reduction taken per channel moves the image: a singer drifts off centre during a
        // loud passage. The gain comes from the loudest channel and applies to all of them,
        // which shows here as the *quiet* channel being pulled down too instead of sailing
        // through at 0.75.
        let mut limiter = Limiter::new(48_000.0, 2);
        let mut last = [0.0f32; 2];
        for _ in 0..2_000 {
            let mut frame = [1.5f32, 0.75];
            limiter.process_frame(&mut frame, -0.3);
            last = frame;
        }
        let ceiling = 10f32.powf(-0.3 / 20.0);
        let shared_gain = ceiling / 1.5;
        assert!(
            (last[1] - 0.75 * shared_gain).abs() < 0.02,
            "the quiet channel ({}) did not take the shared gain {shared_gain}",
            last[1]
        );
        // The loud channel is folded by the knee, which bends the 2:1 ratio slightly — but
        // only slightly, and with nothing left unattenuated.
        let ratio = last[0] / last[1];
        assert!(
            (1.8..=2.0).contains(&ratio),
            "the 2:1 image became {ratio}:1 under limiting"
        );
    }

    #[test]
    fn the_parameters_survive_a_round_trip_through_the_atomics() {
        let wanted = Snapshot {
            bypass: false,
            eq_gain: [1.0, -2.0, 3.0, 0.0, -4.5, 6.0, 0.0, -1.0, 2.0, 12.0],
            eq_q: 2.5,
            bass_db: 7.5,
            bass_harmonics: 0.25,
            clarity_db: 4.0,
            clarity_transient: 0.6,
            loudness_target_db: -18.0,
            loudness_speed_s: 3.5,
            limiter_ceiling_db: -1.5,
            spatial_width: 1.4,
            spatial_room: 0.3,
        };
        let params = params(wanted);
        assert_eq!(params.load(), wanted, "a published set came back different");
        assert!(!params.bypassed());
    }

    #[test]
    fn a_fresh_parameter_block_ships_bypassed_and_not_ready() {
        let params = EnhanceParams::default();
        assert!(params.bypassed(), "the feature must ship switched off");
        assert!(!params.ready.load(Ordering::Relaxed));
        assert_eq!(params.latency_ms(), 0.0);
    }

    #[test]
    fn a_hand_edited_parameter_block_is_clamped_before_it_reaches_a_filter() {
        let mut wild = Snapshot {
            bypass: false,
            eq_gain: [f32::NAN; EQ_BANDS],
            eq_q: 0.0,
            bass_db: 400.0,
            bass_harmonics: -3.0,
            clarity_db: f32::INFINITY,
            clarity_transient: 9.0,
            loudness_target_db: f32::NAN,
            loudness_speed_s: 0.0,
            limiter_ceiling_db: 20.0,
            spatial_width: -5.0,
            spatial_room: 99.0,
        };
        wild.clamp();
        assert_eq!(wild.eq_gain, [-12.0; EQ_BANDS]);
        assert_eq!(wild.eq_q, 0.3);
        assert_eq!(wild.bass_db, 12.0);
        assert_eq!(wild.bass_harmonics, 0.0);
        assert_eq!(wild.clarity_db, 0.0); // an infinity falls back to the low end
        assert_eq!(wild.clarity_transient, 1.0);
        assert_eq!(wild.loudness_target_db, -60.0);
        assert_eq!(wild.loudness_speed_s, 0.5);
        assert_eq!(wild.limiter_ceiling_db, 0.0);
        assert_eq!(wild.spatial_width, 0.0);
        assert_eq!(wild.spatial_room, 1.0);
    }

    #[test]
    fn neutral_parameters_are_idle_and_a_moved_band_is_not() {
        assert!(Snapshot::default().is_idle());
        let mut moved = Snapshot::default();
        moved.eq_gain[3] = 1.0;
        assert!(!moved.is_idle(), "a moved band must wake the chain up");
        let mut normalised = Snapshot::default();
        normalised.loudness_target_db = -18.0;
        assert!(!normalised.is_idle(), "normalisation must wake the chain up");
        let mut wide = Snapshot::default();
        wide.spatial_width = 1.5;
        assert!(!wide.is_idle(), "the width control must wake the chain up");
    }

    #[test]
    fn the_chain_reports_only_the_latency_it_really_adds() {
        let params = EnhanceParams::default();
        let mut chain = EnhanceChain::new(48_000, 2);
        let look_ahead_only = chain.latency_ms();
        assert!(
            (0.5..2.0).contains(&look_ahead_only),
            "the limiter's look-ahead should be about a millisecond, got {look_ahead_only}"
        );

        // The reverb is *added* to a dry signal that keeps its timing, so switching it on must not
        // change what the interface reports.
        params.publish(Snapshot { bypass: false, spatial_room: 1.0, ..Snapshot::default() });
        let mut samples = tone(4_800, 0.1);
        chain.process(&mut samples, &params);
        assert_eq!(chain.latency_ms(), look_ahead_only);
    }

    /// RMS of one channel of an interleaved stereo block.
    fn channel_rms(samples: &[f32], channel: usize) -> f32 {
        let mut sum = 0.0;
        let mut count = 0usize;
        for frame in samples.chunks(2) {
            if let Some(sample) = frame.get(channel) {
                sum += sample * sample;
                count += 1;
            }
        }
        if count == 0 {
            0.0
        } else {
            (sum / count as f32).sqrt()
        }
    }

    /// Broadband, deterministic and identical in both channels: a mono source, which is the case
    /// the single cross-feed tap could do nothing for.
    fn mono_noise(frames: usize, peak: f32) -> Vec<f32> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut samples = vec![0.0f32; frames * 2];
        for frame in samples.chunks_mut(2) {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            // `>> 40` leaves 24 bits, so dividing by 2^23 gives `0..2` and subtracting one lands on
            // `-1.0..1.0`: a centred signal, with no DC for the comb loops to amplify into a common
            // component in both channels.
            let unit = (state >> 40) as f32 / (1u64 << 23) as f32 - 1.0;
            frame[0] = unit * peak;
            frame[1] = unit * peak;
        }
        samples
    }

    /// What the reverb leaves behind: a burst of noise, then silence, with `room` set throughout.
    /// Returns the RMS of the burst and the whole block of silence that follows it, so a test can
    /// look both at how loud the tail is and at how long it lasts.
    fn burst_then_silence(
        room: f32,
        burst_frames: usize,
        silence_frames: usize,
    ) -> (f32, Vec<f32>) {
        let params = params(Snapshot { bypass: false, spatial_room: room, ..Snapshot::default() });
        let mut chain = EnhanceChain::new(48_000, 2);
        let mut burst = mono_noise(burst_frames, 0.5);
        chain.process(&mut burst, &params);
        let mut silence = vec![0.0f32; silence_frames * 2];
        chain.process(&mut silence, &params);
        (channel_rms(&burst, 0), silence)
    }

    /// Correlation between the two channels of an interleaved block. `1.0` means the two channels
    /// are the same signal — which is what a mono source stays when nothing decorrelates it.
    fn channel_correlation(samples: &[f32]) -> f32 {
        let mut product = 0.0;
        let mut left = 0.0;
        let mut right = 0.0;
        for frame in samples.chunks(2) {
            product += frame[0] * frame[1];
            left += frame[0] * frame[0];
            right += frame[1] * frame[1];
        }
        let norm = (left * right).sqrt();
        if norm > 0.0 {
            product / norm
        } else {
            0.0
        }
    }

    #[test]
    fn the_room_control_leaves_an_audible_tail_behind_the_sound() {
        // What a listener hears, measured on a mono noise burst: the tail is the first 100 ms after
        // the sound stops, and how *long* the room rings is the half second that follows it. Every
        // number here is relative to the burst's own level.
        const BURST: usize = 9_600;
        const SILENCE: usize = 24_000;
        let windows = |room: f32| {
            let (burst, silence) = burst_then_silence(room, BURST, SILENCE);
            let early = channel_rms(&silence[..4_800 * 2], 0) / burst;
            let late = channel_rms(&silence[16_800 * 2..], 0) / burst;
            (burst, early, late)
        };

        let (burst, early, late) = windows(1.0);
        assert!(burst > 0.2, "the test's own burst is wrong: {burst}");
        assert!(
            early > 0.25,
            "a room at 100 % left only {:.1} % of the sound behind it",
            early * 100.0
        );
        assert!(early < 1.2, "the tail swamps the dry signal: {:.1} %", early * 100.0);
        assert!(late > 0.05, "the tail is gone after half a second: {:.1} %", late * 100.0);

        // Every notch has to do something: a quarter of the slider is plainly there, half is
        // obvious, and neither is what the top reaches.
        let (_, quarter, quarter_late) = windows(0.25);
        assert!(
            quarter > 0.02,
            "a quarter of the room was inaudible: {:.1} %",
            quarter * 100.0
        );
        let (_, half, half_late) = windows(0.5);
        assert!(half > 0.08, "half the room was inaudible: {:.1} %", half * 100.0);
        assert!(
            quarter < half && half < early * 0.8,
            "the control does not rise: {:.1} % at 25 %, {:.1} % at 50 %, {:.1} % at 100 %",
            quarter * 100.0,
            half * 100.0,
            early * 100.0
        );
        // Louder *and* longer: that is the other half of "a bigger room", and the half the old
        // single tap could not do at all.
        assert!(
            late > half_late * 5.0 && late > quarter_late * 5.0,
            "the tail does not lengthen: {:.2} % at 25 %, {:.2} % at 50 %, {:.2} % at 100 %",
            quarter_late * 100.0,
            half_late * 100.0,
            late * 100.0
        );
    }

    #[test]
    fn a_flooded_room_still_respects_the_limiter() {
        // The reverb can only add level, so the stage after it has to hold: a hot burst with the
        // room wide open must leave the limiter inside its ceiling, not wrapped around it.
        let params = params(Snapshot { bypass: false, spatial_room: 1.0, ..Snapshot::default() });
        let mut chain = EnhanceChain::new(48_000, 2);
        let mut samples = mono_noise(24_000, 0.95);
        chain.process(&mut samples, &params);
        let peak = samples.iter().fold(0.0f32, |worst, sample| worst.max(sample.abs()));
        assert!(peak <= 1.0, "the chain let {peak:.4} through");
        assert!(samples.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn a_mono_source_gets_a_tail_that_is_not_mono() {
        // The single cross-feed tap this stage replaced added the *same* reflection to both
        // channels, so a mono file stayed in the middle and the control could not be heard.
        // Differently tuned delays have to decorrelate the two tails instead.
        let dry = mono_noise(4_800, 0.5);
        assert!(
            channel_correlation(&dry) > 0.999,
            "the test's own source is not mono: {}",
            channel_correlation(&dry)
        );

        let (_, tail) = burst_then_silence(1.0, 9_600, 24_000);
        let correlation = channel_correlation(&tail);
        assert!(
            correlation < 0.5,
            "the two tails are the same signal (correlation {correlation:.3}), so a mono file \
             would stay in the middle"
        );
    }

    #[test]
    fn a_silent_room_hands_the_frame_back_untouched() {
        // `room == 0` with a neutral width is the do-nothing position, and the other stages being
        // busy must not change that — this is what keeps "on but not spatial" identical to "off".
        let mut spatial = Spatial::new(48_000.0, 2);
        let snapshot = Snapshot { bypass: false, bass_db: 6.0, ..Snapshot::default() };
        let mut samples = tone(512, 0.8);
        let original = samples.clone();
        for frame in samples.chunks_mut(2) {
            spatial.process_frame(frame, &snapshot);
        }
        assert_eq!(samples, original, "a room at 0 rewrote the buffer");
    }

    #[test]
    fn switching_the_room_off_does_not_leave_a_tail_to_leak_back() {
        // Whatever was ringing when the control was turned down has to be dropped, or the next time
        // it is turned up the listener hears audio from minutes ago.
        let mut spatial = Spatial::new(48_000.0, 2);
        let ringing = Snapshot { bypass: false, spatial_room: 1.0, ..Snapshot::default() };
        let closed = Snapshot { bypass: false, ..Snapshot::default() };

        let mut burst = mono_noise(4_800, 0.5);
        for frame in burst.chunks_mut(2) {
            spatial.process_frame(frame, &ringing);
        }
        // Room back to zero, then up again: the buffers have been emptied in between.
        for _ in 0..64 {
            let mut frame = [0.0f32; 2];
            spatial.process_frame(&mut frame, &closed);
        }
        let mut silence = vec![0.0f32; 2_048];
        for frame in silence.chunks_mut(2) {
            spatial.process_frame(frame, &ringing);
        }
        assert!(
            silence.iter().all(|sample| *sample == 0.0),
            "a stale tail leaked back: {:?}",
            &silence[..8]
        );
    }
}
