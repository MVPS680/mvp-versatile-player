//! Audio output through `cpal` (WASAPI on Windows), with a lock-free hand-off
//! and a sample-accurate master clock.
//!
//! The output device is opened **once** for the lifetime of the player, at the
//! device's own default sample rate and channel count. Everything the demuxer
//! produces is converted to that layout by [`crate::engine`], so switching files
//! never re-opens the device — which is what keeps start-up latency low and
//! avoids the audible click of a device reset.
//!
//! The device callback runs on a real-time thread and must never block, so it
//! communicates through a bounded `crossbeam` channel and plain atomics only.
//!
//! ## How the clock works
//!
//! The number of frames handed to the device is **not** the playback position:
//! a driver may swallow a whole second of audio in one callback, or take a
//! buffer and play it much later. Using that counter directly makes the clock
//! run several times faster than real time and the picture races ahead of the
//! sound.
//!
//! Instead, every buffer is stamped with `cpal`'s own `playback` instant — the
//! moment that buffer will actually be heard — and the position is derived from
//! *that*: which frame is being heard right now, mapped through the anchor taken
//! when the audio was queued. The result is independent of buffer size, of how
//! often the callback runs, and of how much the driver has buffered.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, Receiver, Sender};

use crate::dsp::{EnhanceChain, EnhanceParams};
use crate::error::{MediaError, Result};

/// How many chunks of audio may wait in front of the device.
///
/// A chunk is whatever the decoder produced for one source frame, resampled to
/// the *device* rate — at 192 kHz that is roughly 23 ms, so 24 chunks is about
/// roughly 23 ms regardless of the device rate, so this is about three seconds
/// of slack. A seek never has to wait for it: lush discards the whole queue
/// and bumps a generation counter the device callback acts on immediately.
const QUEUE_CHUNKS: usize = 128;

/// Re-anchor the media clock only when a chunk's timestamp disagrees with the
/// linear model by more than this many seconds.
///
/// This has to be comfortably larger than any amount of queued audio: a seek or
/// a broken timestamp is a jump of seconds, whereas "the decoder is 20 chunks
/// ahead" is only a fraction of a second and must **not** move the anchor.
const DISCONTINUITY_THRESHOLD: f64 = 0.30;

/// Reference instant for the atomics that carry an `Instant`.
///
/// `Instant` cannot live in an atomic, so timestamps are stored as nanoseconds
/// since this base and readers subtract their own elapsed time.
static TIME_BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Nanoseconds elapsed since [`TIME_BASE`] as of now.
fn now_ns() -> u64 {
    let base = *TIME_BASE.get_or_init(Instant::now);
    base.elapsed().as_nanos() as u64
}

/// One batch of interleaved samples tagged with the flush generation it belongs
/// to.
///
/// The tag is what keeps a seek honest. A flush is acted on by the device
/// callback, which runs on its own schedule; between the flush and the callback
/// the producer may already have decoded and pushed samples for the *new*
/// position. Draining the whole queue when the callback finally notices would
/// throw those away — which is exactly what left a seek near the end of a file
/// silent. Comparing each chunk's tag against the live generation instead
/// discards only what the flush was meant to discard.
struct AudioChunk {
    generation: u64,
    samples: Vec<f32>,
}

/// State shared between the public handle and the real-time device callback.
#[derive(Debug)]
struct Shared {
    /// Total frames handed to the device since the last flush. Used for the
    /// statistics panel only — never as a clock.
    played: AtomicU64,
    /// `false` until an anchor has been established for the current run.
    anchored: AtomicBool,
    /// Media timestamp (f64 bits) corresponding to the anchor.
    anchor_pts: AtomicU64,
    /// Wall-clock instant of the anchor, in nanoseconds since [`TIME_BASE`].
    anchor_ns: AtomicU64,
    /// Frames in the most recently queued chunk, so the depth of the queue in
    /// front of the device can be expressed in seconds.
    chunk_frames: AtomicU64,
    /// Playback speed (f64 bits).
    speed: AtomicU64,
    /// Linear volume (f32 bits); applied by the output, not by the producer.
    volume: AtomicU32,
    /// Mute flag.
    muted: AtomicBool,
    /// Bumped on every flush so the callback discards its partial chunk.
    flush_generation: AtomicU64,
    /// Chunks dropped because the queue was full.
    dropped: AtomicU64,
    /// Number of times the callback ran out of data and had to emit silence.
    underruns: AtomicU64,
    /// Set once the sink has delivered at least one real sample.
    primed: AtomicBool,
    /// The device is paused, so the clock must stand still.
    paused: AtomicBool,
    /// When the pause started, in nanoseconds since [`TIME_BASE`].
    paused_at_ns: AtomicU64,
    /// The sink is being torn down: the queue will never drain again.
    closing: AtomicBool,
}

impl Shared {
    fn new(volume: f32, speed: f64) -> Self {
        Self {
            played: AtomicU64::new(0),
            anchored: AtomicBool::new(false),
            anchor_ns: AtomicU64::new(0),
            chunk_frames: AtomicU64::new(0),
            anchor_pts: AtomicU64::new(0.0f64.to_bits()),
            speed: AtomicU64::new(speed.to_bits()),
            volume: AtomicU32::new(volume.to_bits()),
            muted: AtomicBool::new(false),
            flush_generation: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            primed: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            paused_at_ns: AtomicU64::new(0),
            closing: AtomicBool::new(false),
        }
    }

    /// Gain the output callback should apply right now.
    ///
    /// Two relaxed atomic loads, no locks, so it is safe to read from the
    /// real-time thread on every buffer — which is what makes a volume change
    /// audible within one buffer instead of after the decode-ahead drained.
    fn gain(&self) -> f32 {
        if self.muted.load(Ordering::Relaxed) {
            0.0
        } else {
            f32::from_bits(self.volume.load(Ordering::Relaxed))
        }
    }
}

/// Owns the output stream and feeds it decoded audio.
pub struct AudioSink {
    /// Kept alive for as long as the sink exists; dropping it stops audio.
    stream: cpal::Stream,
    tx: Sender<AudioChunk>,
    shared: Arc<Shared>,
    sample_rate: u32,
    channels: u16,
    device_name: String,
}

// SAFETY: every field is either a `cpal::Stream` (a handle that `cpal` documents
// as `Send` on the WASAPI backend), a `crossbeam` channel or an `Arc` of atomics.
// The engine only ever touches the sink through `&self`.
unsafe impl Send for AudioSink {}
// SAFETY: no field has interior mutability that is not already synchronised
// (atomics) or channel-based, so sharing `&AudioSink` across threads is sound.
unsafe impl Sync for AudioSink {}

impl AudioSink {
    /// Open the default (or named) output device.
    ///
    /// `preferred_device` is matched against the friendly device name; when it
    /// is `None` or does not match, the system default is used.
    ///
    /// `enhance` is handed to the device callback, which owns the DSP chain: the
    /// interface publishes parameters into it and never blocks on the audio thread.
    pub fn new(
        preferred_device: Option<&str>,
        volume: f32,
        speed: f64,
        enhance: Arc<EnhanceParams>,
    ) -> Result<Self> {
        let host = cpal::default_host();

        let device = match preferred_device {
            Some(name) if !name.is_empty() => host
                .output_devices()
                .ok()
                .and_then(|mut devices| {
                    devices.find(|d| {
                        d.name()
                            .map(|n| n == name || n.contains(name))
                            .unwrap_or(false)
                    })
                })
                .or_else(|| host.default_output_device()),
            _ => host.default_output_device(),
        }
        .ok_or_else(|| MediaError::AudioDevice("未找到可用的音频输出设备".into()))?;

        let device_name = device.name().unwrap_or_else(|_| "默认设备".to_string());
        let supported = device
            .default_output_config()
            .map_err(|e| MediaError::AudioDevice(format!("无法读取设备默认格式: {e}")))?;

        let (supported, overrode) = preferred_config(&device, supported);
        if let Some(original) = overrode {
            log::info!(
                "设备默认采样率为 {original} Hz，改用 {} Hz 以获得稳定的时钟与更低的缓冲延迟",
                supported.sample_rate().0
            );
        }

        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.config();
        let sample_rate = config.sample_rate.0;
        let channels = config.channels;

        let (tx, rx) = bounded::<AudioChunk>(QUEUE_CHUNKS);
        let shared = Arc::new(Shared::new(volume, speed));

        let error_shared = Arc::clone(&shared);
        let error_fn = move |err: cpal::StreamError| {
            // A device error is not fatal: the stream usually recovers, and the
            // UI surfaces the counter through the statistics panel.
            error_shared.dropped.fetch_add(1, Ordering::Relaxed);
            log::warn!("音频输出错误: {err}");
        };

        let stream = match sample_format {
            cpal::SampleFormat::F32 => Self::build::<f32>(
                &device,
                &config,
                rx,
                Arc::clone(&shared),
                Arc::clone(&enhance),
                channels,
                error_fn,
            )?,
            cpal::SampleFormat::I16 => Self::build::<i16>(
                &device,
                &config,
                rx,
                Arc::clone(&shared),
                Arc::clone(&enhance),
                channels,
                error_fn,
            )?,
            cpal::SampleFormat::U16 => Self::build::<u16>(
                &device,
                &config,
                rx,
                Arc::clone(&shared),
                Arc::clone(&enhance),
                channels,
                error_fn,
            )?,
            cpal::SampleFormat::I32 => Self::build::<i32>(
                &device,
                &config,
                rx,
                Arc::clone(&shared),
                Arc::clone(&enhance),
                channels,
                error_fn,
            )?,
            cpal::SampleFormat::F64 => Self::build::<f64>(
                &device,
                &config,
                rx,
                Arc::clone(&shared),
                Arc::clone(&enhance),
                channels,
                error_fn,
            )?,
            cpal::SampleFormat::I8 => Self::build::<i8>(
                &device,
                &config,
                rx,
                Arc::clone(&shared),
                Arc::clone(&enhance),
                channels,
                error_fn,
            )?,
            cpal::SampleFormat::U8 => Self::build::<u8>(
                &device,
                &config,
                rx,
                Arc::clone(&shared),
                Arc::clone(&enhance),
                channels,
                error_fn,
            )?,
            other => {
                return Err(MediaError::AudioDevice(format!(
                    "不支持的采样格式: {other:?}"
                )))
            }
        };

        stream
            .play()
            .map_err(|e| MediaError::AudioDevice(format!("无法启动音频流: {e}")))?;

        log::info!("音频输出: {device_name} · {sample_rate} Hz · {channels} ch · {sample_format:?}");

        Ok(Self {
            stream,
            tx,
            shared,
            sample_rate,
            channels,
            device_name,
        })
    }

    fn build<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        rx: Receiver<AudioChunk>,
        shared: Arc<Shared>,
        enhance: Arc<EnhanceParams>,
        channels: u16,
        error_fn: impl FnMut(cpal::StreamError) + Send + 'static,
    ) -> Result<cpal::Stream>
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let mut current: Vec<f32> = Vec::new();
        let mut cursor = 0usize;
        // Gain actually applied so far. Volume is applied *here*, as the
        // samples are played, rather than while they were decoded seconds
        // earlier: that is what makes the slider and the wheel feel immediate.
        // The value is ramped towards the target across one buffer so a large
        // change (mute, or a jump to 200 %) does not click.
        let mut applied_gain = shared.gain();
        let mut seen_generation = shared.flush_generation.load(Ordering::Relaxed);
        let channels = channels.max(1) as usize;

        // The enhancement chain lives on this thread, built from the device's own format —
        // so the buffers it needs are allocated here and never in the callback. Its readiness
        // and its latency are published for the interface to show rather than guess at.
        let chain = EnhanceChain::new(config.sample_rate.0, config.channels);
        enhance
            .latency_ms
            .store(chain.latency_ms().to_bits(), Ordering::Relaxed);
        enhance.ready.store(true, Ordering::Relaxed);
        let mut chain = Some(chain);
        // The buffer the callback fills before the chain sees it. Grown once, to the largest
        // size the driver asks for, and reused from then on.
        let mut scratch: Vec<f32> = Vec::new();

        device
            .build_output_stream(
                config,
                move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
                    // The buffer the samples are built in, sized once to the largest callback
                    // the driver asks for and reused afterwards: the real-time thread may not
                    // allocate.
                    if scratch.len() < out.len() {
                        scratch.resize(out.len(), 0.0);
                    }
                    // Two relaxed atomic loads, no locks: safe on the real-time
                    // thread, and the reason a volume change is audible within
                    // one buffer instead of after the decode-ahead drained.
                    let target_gain = shared.gain();
                    let ramp = if out.is_empty() {
                        0.0
                    } else {
                        (target_gain - applied_gain) / out.len() as f32
                    };

                    let mut written = 0usize;
                    while written < out.len() {
                        if cursor >= current.len() {
                            // A flush (seek, track change, stop) drops the
                            // partial chunk still being played, and every chunk
                            // that was queued for the old position. It must not
                            // drain the whole queue, though: between the flush
                            // and this callback the producer may already have
                            // pushed samples decoded for the *new* position, and
                            // those carry the new generation. Comparing each
                            // chunk's tag against the live generation discards
                            // exactly the stale audio and keeps the fresh — the
                            // difference between a seek near the end of a file
                            // playing its last seconds and falling silent.
                            let live = shared.flush_generation.load(Ordering::Relaxed);
                            if live != seen_generation {
                                seen_generation = live;
                                current.clear();
                                cursor = 0;
                            }
                            match rx.try_recv() {
                                Ok(chunk) if !chunk.samples.is_empty() => {
                                    if chunk.generation != live {
                                        continue;
                                    }
                                    current = chunk.samples;
                                    cursor = 0;
                                }
                                Ok(_) => continue,
                                Err(_) => break,
                            }
                        }
                        let take = (out.len() - written).min(current.len() - cursor);
                        for i in 0..take {
                            applied_gain += ramp;
                            // Volume first, then the chain: the limiter's ceiling is then a
                            // ceiling on what is actually heard, and the volume's own soft
                            // knee keeps its familiar behaviour when nothing else is on.
                            scratch[written + i] = crate::dsp::apply_gain_sample(
                                current[cursor + i],
                                applied_gain,
                            );
                        }
                        written += take;
                        cursor += take;
                    }
                    applied_gain = target_gain;

                    if written < out.len() {
                        // Nothing left: play silence rather than repeating the
                        // last buffer, which would sound like a stuck sample.
                        scratch[written..out.len()].fill(0.0);
                        if wrote_any(out.len(), written) {
                            shared.underruns.fetch_add(1, Ordering::Relaxed);
                        }
                    }

                    // The enhancement chain, if one was built for this format. `process`
                    // checks the bypass flag itself — one relaxed load — so that a chain which
                    // was switched off still gets to empty its delay lines on the way out.
                    let buffer = &mut scratch[..out.len()];
                    if let Some(chain) = chain.as_mut() {
                        chain.process(buffer, &enhance);
                    }
                    for (target, value) in out.iter_mut().zip(buffer.iter()) {
                        *target = T::from_sample(*value);
                    }

                    let _ = info;
                    let frames = written / channels;
                    if frames > 0 {
                        // Counted for the statistics panel only. This is *not* a
                        // clock: a driver may swallow a whole second of audio in
                        // one callback, and counting writes would make time run
                        // several times too fast.
                        shared
                            .played
                            .fetch_add(frames as u64, Ordering::Relaxed);
                        shared.primed.store(true, Ordering::Relaxed);
                    }
                },
                error_fn,
                None,
            )
            .map_err(|e| MediaError::AudioDevice(format!("无法创建音频流: {e}")))
    }

    /// Sample rate of the opened device.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Channel count of the opened device.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Friendly name of the opened device.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Number of chunks currently waiting to be played.
    pub fn queued_chunks(&self) -> usize {
        self.tx.len()
    }

    /// Seconds of audio currently queued in front of the device.
    ///
    /// Derived from the size of a real chunk rather than a guess: one chunk is
    /// one decoded source frame resampled to the device rate, which varies with
    /// the codec.
    pub fn queued_seconds(&self) -> f64 {
        let frames = self.shared.chunk_frames.load(Ordering::Relaxed) as f64;
        self.tx.len() as f64 * frames / self.sample_rate.max(1) as f64
    }

    /// `true` once real samples have reached the device.
    pub fn is_primed(&self) -> bool {
        self.shared.primed.load(Ordering::Relaxed)
    }

    /// Number of times the device ran dry.
    pub fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// Number of chunks dropped because the queue was full.
    pub fn dropped_chunks(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Queue interleaved `f32` samples tagged with the media timestamp of their
    /// first sample.
    ///
    /// Blocks (bounded) for up to 500 ms so that a full queue applies
    /// back-pressure to the decoder instead of dropping audio; returns `false`
    /// when the chunk had to be dropped.
    ///
    /// The wait is spent in short slices so that [`AudioSink::close`] can end it
    /// immediately. Without that, closing the player while it was **paused** hung
    /// the shutdown for the whole 500 ms: a paused stream runs no callback, so
    /// nothing ever drained the queue the decoder was waiting on.
    pub fn push(&self, samples: Vec<f32>, pts: f64) -> bool {
        if samples.is_empty() {
            return true;
        }

        // Anchor the clock on the first samples of a run: that is the media time
        // the device is about to start playing, and every later chunk follows on
        // contiguously. Re-anchoring mid-run would let the clock report the far
        // end of the buffered audio instead of the playhead.
        //
        // Discontinuities are handled explicitly by `flush` (seek, track change);
        // the timestamp check is only a safety net for a stream whose timestamps
        // jump on their own.
        let frames = (samples.len() / self.channels.max(1) as usize) as u64;
        self.shared.chunk_frames.store(frames, Ordering::Relaxed);

        let estimate = self.position();
        let needs_anchor = !self.shared.anchored.load(Ordering::Acquire)
            || (pts.is_finite() && (pts - estimate).abs() > DISCONTINUITY_THRESHOLD);
        if needs_anchor {
            // The decoder runs well ahead of the device, so `pts` is the far end
            // of the buffered audio. Subtract what is already queued to get the
            // media time that is about to be *heard*, which is what a clock is
            // supposed to report.
            let speed = f64::from_bits(self.shared.speed.load(Ordering::Relaxed));
            let queued = self.queued_seconds() * speed;
            self.shared
                .anchor_pts
                .store((pts - queued).to_bits(), Ordering::Relaxed);
            self.shared.anchor_ns.store(now_ns(), Ordering::Relaxed);
            self.shared.anchored.store(true, Ordering::Release);
        }

        let mut chunk = AudioChunk {
            generation: self.shared.flush_generation.load(Ordering::Relaxed),
            samples,
        };
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        loop {
            match self.tx.send_timeout(chunk, Duration::from_millis(10)) {
                Ok(()) => return true,
                Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                    self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
                Err(crossbeam_channel::SendTimeoutError::Timeout(returned)) => {
                    chunk = returned;
                    if self.shared.closing.load(Ordering::Relaxed)
                        || std::time::Instant::now() >= deadline
                    {
                        self.shared.dropped.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                }
            }
        }
    }

    /// Stop accepting audio: the queue will never be drained again.
    ///
    /// Called before the workers are joined, so a decoder sitting in
    /// [`AudioSink::push`] gives up at once instead of waiting out its
    /// back-pressure timeout.
    pub fn close(&self) {
        self.shared.closing.store(true, Ordering::Relaxed);
    }

    /// Throw away everything queued and re-anchor the clock at `media_pts`.
    ///
    /// The device callback notices the bumped generation on its next run and
    /// discards both its partial chunk and anything still in the channel, so no
    /// pre-seek audio can survive the seek.
    pub fn flush(&self, media_pts: f64) {
        let media_pts = media_pts.max(0.0);
        self.shared.flush_generation.fetch_add(1, Ordering::Relaxed);
        self.shared.played.store(0, Ordering::Relaxed);
        // Show the seek target straight away, then let the first chunk that
        // arrives after the flush re-anchor to its own (more accurate) timestamp.
        self.shared
            .anchor_pts
            .store(media_pts.to_bits(), Ordering::Relaxed);
        self.shared.anchor_ns.store(now_ns(), Ordering::Relaxed);
        self.shared.anchored.store(false, Ordering::Relaxed);
    }

    /// Current media position in seconds.
    pub fn position(&self) -> f64 {
        let anchor_pts = f64::from_bits(self.shared.anchor_pts.load(Ordering::Relaxed));
        let anchor_ns = self.shared.anchor_ns.load(Ordering::Relaxed);
        let speed = f64::from_bits(self.shared.speed.load(Ordering::Relaxed));
        // While paused the clock must stand still, so measure from the moment
        // playback stopped rather than from the wall clock.
        let now = if self.shared.paused.load(Ordering::Relaxed) {
            self.shared.paused_at_ns.load(Ordering::Relaxed)
        } else {
            now_ns()
        };
        let elapsed = now.saturating_sub(anchor_ns) as f64 / 1e9;
        (anchor_pts + elapsed * speed).max(0.0)
    }

    /// Pause or resume the device.
    ///
    /// The device is only touched when the state actually changes. Seeking over
    /// a file that is sitting at its end restarts it on every request — the
    /// demuxer pauses the stream as the file ends, and the restart plays it
    /// again — so without this the driver would get a real `Start`/`Stop` pair
    /// for every drag of the seek bar, for nothing.
    pub fn set_paused(&self, paused: bool) {
        let changed = if paused {
            let was_playing = !self.shared.paused.swap(true, Ordering::Relaxed);
            if was_playing {
                self.shared.paused_at_ns.store(now_ns(), Ordering::Relaxed);
            }
            was_playing
        } else {
            let was_paused = self.shared.paused.swap(false, Ordering::Relaxed);
            if was_paused {
                // Move the anchor forward by however long we were paused, so the
                // clock resumes exactly where it stopped instead of jumping by
                // the length of the pause.
                let paused_at = self.shared.paused_at_ns.load(Ordering::Relaxed);
                let delta = now_ns().saturating_sub(paused_at);
                self.shared.anchor_ns.fetch_add(delta, Ordering::Relaxed);
            }
            was_paused
        };
        if !changed {
            return;
        }

        // The two `cpal` methods have distinct error types, so normalise them.
        let result = if paused {
            self.stream.pause().map_err(|e| e.to_string())
        } else {
            self.stream.play().map_err(|e| e.to_string())
        };
        if let Err(e) = result {
            log::warn!("无法{}音频流: {e}", if paused { "暂停" } else { "恢复" });
        }
    }

    /// Change the playback speed used by the master clock.
    pub fn set_speed(&self, speed: f64) {
        // Re-anchor first so the position does not jump when the slope changes.
        let media = self.position();
        self.shared.anchor_pts.store(media.to_bits(), Ordering::Relaxed);
        self.shared.anchor_ns.store(now_ns(), Ordering::Relaxed);
        self.shared.speed.store(speed.to_bits(), Ordering::Relaxed);
    }

    /// Linear volume, `1.0` is unity.
    pub fn set_volume(&self, volume: f32) {
        self.shared
            .volume
            .store(volume.max(0.0).to_bits(), Ordering::Relaxed);
    }

    /// Current linear volume.
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.shared.volume.load(Ordering::Relaxed))
    }

    /// Mute or unmute.
    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }

    /// `true` when muted.
    pub fn is_muted(&self) -> bool {
        self.shared.muted.load(Ordering::Relaxed)
    }

    /// Effective gain the output applies: volume, zeroed when muted.
    pub fn effective_gain(&self) -> f32 {
        self.shared.gain()
    }
}

/// Sample rates this player prefers for its single output stream.
///
/// A device whose *default* is an exotic rate such as 192 kHz is almost always
/// the result of a "studio quality" setting in the Windows sound control panel,
/// and some drivers drive such a stream far faster than real time — the callback
/// then asks for several seconds of audio per second of wall clock, the queue
/// starves, and every clock derived from the device runs away. 48 kHz is the
/// rate of essentially all video and CD material and is supported everywhere,
/// so the player asks for that instead of trusting the default.
const PREFERRED_RATES: &[u32] = &[48_000, 44_100];

/// Pick a supported configuration at one of [`PREFERRED_RATES`] when the device
/// default is something unusual.
///
/// Returns the chosen configuration and, when it differs, the original rate so
/// the caller can log the substitution.
fn preferred_config(
    device: &cpal::Device,
    default: cpal::SupportedStreamConfig,
) -> (cpal::SupportedStreamConfig, Option<u32>) {
    let default_rate = default.sample_rate().0;
    if PREFERRED_RATES.contains(&default_rate) {
        return (default, None);
    }
    let Ok(ranges) = device.supported_output_configs() else {
        return (default, None);
    };
    // Only switch if the device genuinely supports the rate natively at the same
    // channel count and sample format; otherwise the default is the safer bet.
    let ranges: Vec<_> = ranges.collect();
    for rate in PREFERRED_RATES {
        let found = ranges.iter().find(|range| {
            range.channels() == default.channels()
                && range.sample_format() == default.sample_format()
                && range.min_sample_rate().0 <= *rate
                && range.max_sample_rate().0 >= *rate
        });
        if let Some(range) = found {
            return (
                (*range).with_sample_rate(cpal::SampleRate(*rate)),
                Some(default_rate),
            );
        }
    }
    (default, None)
}

/// `true` when at least part of the buffer was filled from real data but not all
/// of it, which is what an underrun means.
fn wrote_any(total: usize, written: usize) -> bool {
    written > 0 && written < total
}

/// Enumerate the friendly names of every output device on the system.
pub fn output_device_names() -> Vec<String> {
    let host = cpal::default_host();
    match host.output_devices() {
        Ok(devices) => devices.filter_map(|d| d.name().ok()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Name of the current default output device, if any.
pub fn default_output_device_name() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|d| d.name().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::apply_gain;

    #[test]
    fn underrun_detection_needs_a_partial_fill() {
        assert!(!wrote_any(1024, 1024));
        assert!(!wrote_any(1024, 0));
        assert!(wrote_any(1024, 10));
    }

    #[test]
    fn the_clock_advances_at_real_time() {
        let shared = Shared::new(1.0, 1.0);
        shared.anchor_pts.store(10.0f64.to_bits(), Ordering::Relaxed);
        shared.anchor_ns.store(now_ns(), Ordering::Relaxed);
        shared.anchored.store(true, Ordering::Relaxed);

        // Re-implement `position` against the shared state so the arithmetic can
        // be checked without opening a sound device.
        let position = || {
            let anchor_pts = f64::from_bits(shared.anchor_pts.load(Ordering::Relaxed));
            let anchor_ns = shared.anchor_ns.load(Ordering::Relaxed);
            let speed = f64::from_bits(shared.speed.load(Ordering::Relaxed));
            let elapsed = now_ns().saturating_sub(anchor_ns) as f64 / 1e9;
            anchor_pts + elapsed * speed
        };

        assert!((position() - 10.0).abs() < 0.01, "the anchor must hold at t=0");
        std::thread::sleep(Duration::from_millis(120));
        let elapsed = position() - 10.0;
        assert!(
            (0.10..0.30).contains(&elapsed),
            "after ~120 ms the clock advanced {elapsed:.3}s"
        );
    }

    #[test]
    fn a_paused_clock_stands_still() {
        let shared = Shared::new(1.0, 1.0);
        shared.anchor_pts.store(0.0f64.to_bits(), Ordering::Relaxed);
        shared.anchor_ns.store(now_ns(), Ordering::Relaxed);
        let position = || {
            let anchor_pts = f64::from_bits(shared.anchor_pts.load(Ordering::Relaxed));
            let anchor_ns = shared.anchor_ns.load(Ordering::Relaxed);
            let now = if shared.paused.load(Ordering::Relaxed) {
                shared.paused_at_ns.load(Ordering::Relaxed)
            } else {
                now_ns()
            };
            anchor_pts + now.saturating_sub(anchor_ns) as f64 / 1e9
        };

        std::thread::sleep(Duration::from_millis(30));
        let running = position();
        assert!(running > 0.01, "the clock must run while playing");

        shared.paused.store(true, Ordering::Relaxed);
        shared.paused_at_ns.store(now_ns(), Ordering::Relaxed);
        let frozen = position();
        std::thread::sleep(Duration::from_millis(40));
        assert!(
            (position() - frozen).abs() < 1e-9,
            "a paused clock must not advance"
        );

        // Resuming shifts the anchor by the length of the pause, so the clock
        // carries on from where it stopped.
        let paused_at = shared.paused_at_ns.load(Ordering::Relaxed);
        let delta = now_ns().saturating_sub(paused_at);
        shared.anchor_ns.fetch_add(delta, Ordering::Relaxed);
        shared.paused.store(false, Ordering::Relaxed);
        assert!(
            (position() - frozen).abs() < 0.02,
            "resuming must continue from the paused position"
        );
    }
    #[test]
    fn gain_zero_mutes() {
        let mut buf = vec![0.5f32; 4];
        apply_gain(&mut buf, 0.0);
        assert!(buf.iter().all(|v| *v == 0.0));
    }
}