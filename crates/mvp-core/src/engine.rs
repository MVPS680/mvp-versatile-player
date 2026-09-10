//! The playback engine: a small façade over three worker threads.
//!
//! ```text
//!   ┌──────────┐  packets   ┌───────────────┐  frames   ┌────────────┐
//!   │ demuxer  ├───────────►│ video decoder ├──────────►│ frame queue│──► UI
//!   │  thread  │            └───────────────┘           └────────────┘
//!   │          │  packets   ┌───────────────┐  f32      ┌────────────┐
//!   │          ├───────────►│ audio decoder ├──────────►│ cpal device│
//!   │          │            └───────────────┘           └────────────┘
//!   │          │  packets   ┌───────────────┐
//!   │          ├───────────►│ sub decoder   │──► Subtitle cues
//!   └──────────┘            └───────────────┘
//! ```
//!
//! Splitting demuxing from decoding means a slow video decoder can never starve
//! the audio device, and the bounded queues mean a slow disk can never grow the
//! player's memory without limit. All control is lock-free-ish: the UI thread
//! sets atomics or a small mutex and the demuxer picks the change up between
//! packets, which keeps the UI perfectly responsive even when a network stream
//! has stalled.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{bounded, Receiver, Sender};
use ffmpeg_next as ffmpeg;
use parking_lot::Mutex;

use crate::audio::AudioSink;
use crate::error::{MediaError, Result};
use crate::info::MediaInfo;
use crate::video::{FramePool, VideoFrame};
use mvp_subtitle::Subtitle;

pub mod clock;
pub mod queue;
mod workers;

pub use clock::Clock;
pub use queue::{AudioMsg, PacketMsg, VideoMsg, VideoQueue};

/// What the player is asked to play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSource {
    /// A local file.
    Path(PathBuf),
    /// A network stream (`http://`, `rtsp://`, ...).
    Url(String),
}

impl MediaSource {
    /// Build a source from anything path-like, detecting URLs automatically.
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        if crate::util::is_url(&value) {
            MediaSource::Url(value)
        } else {
            MediaSource::Path(PathBuf::from(value))
        }
    }

    /// The path or URL as a string, ready for FFmpeg.
    pub fn as_str(&self) -> String {
        match self {
            MediaSource::Path(p) => p.to_string_lossy().into_owned(),
            MediaSource::Url(u) => u.clone(),
        }
    }

    /// Path form, for local files.
    pub fn as_path(&self) -> Option<&Path> {
        match self {
            MediaSource::Path(p) => Some(p),
            MediaSource::Url(_) => None,
        }
    }

    /// A short label for the window title and playlist.
    pub fn display_name(&self) -> String {
        match self {
            MediaSource::Path(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string_lossy().into_owned()),
            MediaSource::Url(u) => u.clone(),
        }
    }
}

impl From<PathBuf> for MediaSource {
    fn from(value: PathBuf) -> Self {
        MediaSource::Path(value)
    }
}

impl From<&Path> for MediaSource {
    fn from(value: &Path) -> Self {
        MediaSource::Path(value.to_path_buf())
    }
}

/// Lifecycle state of the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackState {
    /// Nothing loaded.
    Idle,
    /// Opening a file or buffering a stream.
    Opening,
    /// Actively playing.
    Playing,
    /// Loaded but paused.
    Paused,
    /// Reached the end of the media.
    Ended,
    /// Failed; the string is a user-presentable message.
    Error(String),
}

impl PlaybackState {
    /// `true` for states where the clock should advance.
    pub fn is_playing(&self) -> bool {
        matches!(self, PlaybackState::Playing)
    }

    /// `true` when media is loaded (playing or paused).
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Ended
        )
    }
}

/// Asynchronous notifications for the UI.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// A file finished opening successfully.
    Opened(Arc<MediaInfo>),
    /// The lifecycle state changed.
    StateChanged(PlaybackState),
    /// Subtitle cues became available (embedded or side-car).
    SubtitleChanged(Arc<Subtitle>),
    /// The end of the media was reached.
    Ended,
    /// A non-fatal or fatal error occurred; the string is user presentable.
    Error(String),
}

/// Tunables for the engine, normally supplied by the player's settings.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Try a hardware decoder before falling back to software.
    pub hardware_decoding: bool,
    /// Open the audio device and play sound.
    pub audio_enabled: bool,
    /// Output device name; `None` means the system default.
    pub audio_device: Option<String>,
    /// Initial linear volume.
    pub volume: f32,
    /// Initial playback rate.
    pub speed: f64,
    /// Pixel-byte budget for the decoded frame queue.
    pub frame_queue_bytes: usize,
    /// Upper bound on queued frames regardless of size.
    pub frame_queue_frames: usize,
    /// Byte budget for the RGB buffer free-list.
    pub buffer_pool_bytes: usize,
    /// Start playing as soon as the file is open.
    pub autoplay: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            hardware_decoding: true,
            audio_enabled: true,
            audio_device: None,
            volume: 1.0,
            speed: 1.0,
            // ~192 MB of decoded video: about 6 frames of 1080p RGBA, or 2 of
            // 4K, which is plenty of slack without ever being a memory hog.
            frame_queue_bytes: 192 * 1024 * 1024,
            frame_queue_frames: 16,
            buffer_pool_bytes: 192 * 1024 * 1024,
            autoplay: true,
        }
    }
}

/// A cheap, `Clone`-able view of the engine's state, taken once per UI frame.
#[derive(Debug, Clone)]
pub struct EngineSnapshot {
    /// Lifecycle state.
    pub state: PlaybackState,
    /// Current position in seconds.
    pub position: f64,
    /// Total duration in seconds (`0.0` for live streams).
    pub duration: f64,
    /// Playback rate.
    pub speed: f64,
    /// Linear volume.
    pub volume: f32,
    /// Mute flag.
    pub muted: bool,
    /// Currently selected audio stream index.
    pub audio_track: Option<usize>,
    /// Currently selected subtitle stream index.
    pub subtitle_track: Option<usize>,
    /// Probed description of the open file.
    pub info: Option<Arc<MediaInfo>>,
    /// `true` while the position is derived from the sound card.
    pub audio_clock: bool,
    /// Number of decoded frames waiting to be shown.
    pub queued_frames: usize,
    /// Seconds of video sitting in the queue.
    pub video_queue_seconds: f64,
    /// Seconds of audio sitting in front of the device.
    pub audio_queue_seconds: f64,
    /// Frames discarded because the decoder fell behind.
    pub dropped_frames: u64,
    /// Frames decoded since the file was opened.
    pub decoded_frames: u64,
    /// Audio chunks dropped because the queue was full.
    pub dropped_audio: u64,
    /// Device underruns since start-up.
    pub underruns: u64,
    /// `true` when a seek has been requested but has not completed.
    pub seeking: bool,
    /// Audio delay in seconds applied to the reported position.
    pub audio_delay: f64,
    /// Subtitle delay in seconds.
    pub subtitle_delay: f64,
    /// A–B loop range, when set.
    pub ab_loop: Option<(f64, f64)>,
    /// Whether playback repeats from the start at the end of the file.
    pub looping: bool,
    /// `true` when the current file's audio track ended but video continues.
    pub audio_ended: bool,
}

/// Shared state between the UI thread and the workers.
pub(crate) struct Shared {
    pub state: Mutex<PlaybackState>,
    pub info: Mutex<Option<Arc<MediaInfo>>>,
    pub duration: Mutex<f64>,
    pub clock: Clock,
    pub video_queue: Mutex<VideoQueue>,
    pub pool: FramePool,
    pub subtitle: Mutex<Option<Arc<Subtitle>>>,
    pub external_subtitle: Mutex<Option<Arc<Subtitle>>>,
    pub audio: Mutex<Option<Arc<AudioSink>>>,
    pub event_tx: Sender<EngineEvent>,

    pub generation: AtomicU64,
    pub abort: AtomicBool,
    pub seek_request: Mutex<Option<f64>>,
    pub seek_target: Mutex<Option<f64>>,
    pub stepping: AtomicI64,
    pub audio_track: AtomicI64,
    pub subtitle_track: AtomicI64,
    pub target_size: Mutex<Option<(u32, u32)>>,
    pub speed_bits: AtomicU64,
    pub volume_bits: AtomicU64,
    pub muted: AtomicBool,
    pub looping: AtomicBool,
    pub hw_decoding: AtomicBool,
    pub audio_delay_bits: AtomicU64,
    pub subtitle_delay_bits: AtomicU64,
    pub ab_loop: Mutex<Option<(f64, f64)>>,

    pub decoded_frames: AtomicU64,
    pub dropped_frames: AtomicU64,
    pub audio_ended: AtomicBool,
    pub workers: Mutex<Vec<JoinHandle<()>>>,
    pub config: Mutex<EngineConfig>,

    /// Pending "drop everything and start again at this position" requests.
    ///
    /// Seek and track changes are delivered through shared state rather than
    /// through the packet channels, so a control request is acted on
    /// immediately even when hundreds of packets are already queued. A blocking
    /// send here used to deadlock a seek behind a full video channel.
    pub video_flush: Mutex<Option<FlushRequest>>,
    pub audio_flush: Mutex<Option<FlushRequest>>,
    /// Replacement audio stream parameters, applied by the audio worker.
    pub audio_stream: Mutex<Option<Box<ffmpeg::codec::Parameters>>>,
}

/// A request to abandon everything the workers are doing and restart at
/// `target` seconds on generation `generation`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlushRequest {
    /// Decode session the workers should move to.
    pub generation: u64,
    /// Media position the seek landed on.
    pub target: f64,
}

impl Shared {
    fn new(config: EngineConfig, event_tx: Sender<EngineEvent>) -> Self {
        let speed = config.speed;
        let volume = config.volume;
        Self {
            state: Mutex::new(PlaybackState::Idle),
            info: Mutex::new(None),
            duration: Mutex::new(0.0),
            clock: Clock::new(),
            video_queue: Mutex::new(VideoQueue::new(
                config.frame_queue_bytes,
                config.frame_queue_frames,
            )),
            pool: FramePool::new(config.buffer_pool_bytes),
            subtitle: Mutex::new(None),
            external_subtitle: Mutex::new(None),
            audio: Mutex::new(None),
            event_tx,
            generation: AtomicU64::new(1),
            abort: AtomicBool::new(false),
            seek_request: Mutex::new(None),
            seek_target: Mutex::new(None),
            stepping: AtomicI64::new(0),
            audio_track: AtomicI64::new(-2),
            subtitle_track: AtomicI64::new(-2),
            target_size: Mutex::new(None),
            speed_bits: AtomicU64::new(speed.to_bits()),
            volume_bits: AtomicU64::new((volume as f64).to_bits()),
            muted: AtomicBool::new(false),
            looping: AtomicBool::new(false),
            hw_decoding: AtomicBool::new(config.hardware_decoding),
            audio_delay_bits: AtomicU64::new(0.0f64.to_bits()),
            subtitle_delay_bits: AtomicU64::new(0.0f64.to_bits()),
            ab_loop: Mutex::new(None),
            decoded_frames: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            audio_ended: AtomicBool::new(false),
            workers: Mutex::new(Vec::new()),
            config: Mutex::new(config),
            video_flush: Mutex::new(None),
            audio_flush: Mutex::new(None),
            audio_stream: Mutex::new(None),
        }
    }

    /// Publish a state change, skipping duplicates.
    pub fn set_state(&self, state: PlaybackState) {
        let mut current = self.state.lock();
        if *current == state {
            return;
        }
        *current = state.clone();
        drop(current);
        let _ = self.event_tx.send(EngineEvent::StateChanged(state));
    }

    /// Current state, cloned.
    pub fn state(&self) -> PlaybackState {
        self.state.lock().clone()
    }

    /// Current speed.
    pub fn speed(&self) -> f64 {
        f64::from_bits(self.speed_bits.load(Ordering::Relaxed))
    }

    /// Ask both workers to abandon what they are doing and restart at `target`.
    pub fn request_flush(&self, generation: u64, target: f64) {
        let request = FlushRequest { generation, target };
        *self.video_flush.lock() = Some(request);
        *self.audio_flush.lock() = Some(request);
    }

    /// Take a pending flush request for the video worker.
    pub fn take_video_flush(&self) -> Option<FlushRequest> {
        self.video_flush.lock().take()
    }

    /// Take a pending flush request for the audio worker.
    pub fn take_audio_flush(&self) -> Option<FlushRequest> {
        self.audio_flush.lock().take()
    }

    /// `true` when the video worker should stop what it is doing right now.
    pub fn video_flush_pending(&self) -> bool {
        self.video_flush.lock().is_some()
    }

    /// Take replacement audio stream parameters, if a track change is pending.
    pub fn take_audio_stream(&self) -> Option<Box<ffmpeg::codec::Parameters>> {
        self.audio_stream.lock().take()
    }

    /// Current volume.
    pub fn volume(&self) -> f32 {
        f64::from_bits(self.volume_bits.load(Ordering::Relaxed)) as f32
    }
}

/// The player's media engine.
pub struct Engine {
    shared: Arc<Shared>,
    events: Receiver<EngineEvent>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    source: Mutex<Option<MediaSource>>,
}

impl Engine {
    /// Create an engine. No device is opened and no thread is started until
    /// [`Engine::open`] is called, which keeps start-up instant.
    pub fn new(config: EngineConfig) -> Result<Self> {
        crate::init()?;
        let (event_tx, events) = bounded::<EngineEvent>(256);
        Ok(Self {
            shared: Arc::new(Shared::new(config, event_tx)),
            events,
            threads: Mutex::new(Vec::new()),
            source: Mutex::new(None),
        })
    }

    /// Open `source` and start playing it. Any previous media is stopped first.
    ///
    /// Whether it actually starts running is the engine's `autoplay`
    /// configuration; see [`Engine::set_autoplay`].
    pub fn open(&self, source: MediaSource) -> Result<()> {
        self.stop();

        // Reset all per-file state.
        self.shared.video_queue.lock().clear();
        self.shared.pool.clear();
        *self.shared.info.lock() = None;
        *self.shared.duration.lock() = 0.0;
        *self.shared.subtitle.lock() = None;
        *self.shared.external_subtitle.lock() = None;
        *self.shared.seek_request.lock() = None;
        *self.shared.seek_target.lock() = None;
        self.shared.generation.store(1, Ordering::SeqCst);
        self.shared.audio_ended.store(false, Ordering::Relaxed);
        self.shared.decoded_frames.store(0, Ordering::Relaxed);
        self.shared.dropped_frames.store(0, Ordering::Relaxed);
        self.shared.abort.store(false, Ordering::SeqCst);
        self.shared.clock.set_audio(None);
        self.shared.clock.seek(0.0);
        self.shared.clock.set_running(false);
        *self.shared.audio.lock() = None;
        *self.source.lock() = Some(source.clone());

        self.shared.set_state(PlaybackState::Opening);

        let shared = Arc::clone(&self.shared);
        let config = self.shared.config.lock().clone();
        let handle = std::thread::Builder::new()
            .name("mvp-demux".into())
            .stack_size(1024 * 1024)
            .spawn(move || workers::run_demuxer(shared, source, config))
            .map_err(|e| MediaError::other(format!("无法创建解码线程: {e}")))?;
        self.threads.lock().push(handle);
        Ok(())
    }

    /// Set whether the next [`Engine::open`] starts playing by itself.
    ///
    /// The interface flips this per open. A file the player was told to open
    /// follows the user's "start playing" preference; a file the user picked
    /// inside the player always plays, and it says so by clearing this first.
    pub fn set_autoplay(&self, autoplay: bool) {
        let mut config = self.shared.config.lock();
        if config.autoplay != autoplay {
            config.autoplay = autoplay;
        }
    }

    /// `true` when the next opened file will start playing by itself.
    pub fn autoplay(&self) -> bool {
        self.shared.config.lock().autoplay
    }

    /// Stop playback and release every worker.
    pub fn stop(&self) {
        self.shared.abort.store(true, Ordering::SeqCst);
        let thread = self.threads.lock().pop();
        if let Some(handle) = thread {
            let _ = handle.join();
        }
        for handle in self.shared.workers.lock().drain(..) {
            let _ = handle.join();
        }
        self.shared.video_queue.lock().clear();
        self.shared.pool.clear();
        self.shared.clock.set_running(false);
        if let Some(sink) = self.shared.audio.lock().take() {
            sink.flush(0.0);
            drop(sink);
        }
        self.shared.clock.set_audio(None);
        if self.shared.state().is_active() {
            self.shared.set_state(PlaybackState::Idle);
        }
    }

    /// Begin or resume playback.
    pub fn play(&self) {
        let state = self.shared.state();
        if !state.is_active() {
            return;
        }
        if let Some(sink) = self.shared.audio.lock().as_ref() {
            sink.set_paused(false);
        }
        self.shared.clock.set_running(true);
        self.shared.set_state(PlaybackState::Playing);
    }

    /// Pause playback.
    pub fn pause(&self) {
        if !self.shared.state().is_playing() {
            return;
        }
        if let Some(sink) = self.shared.audio.lock().as_ref() {
            sink.set_paused(true);
        }
        self.shared.clock.set_running(false);
        self.shared.set_state(PlaybackState::Paused);
    }

    /// Toggle between playing and paused.
    pub fn toggle_pause(&self) {
        if self.shared.state().is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// `true` while the clock is advancing.
    pub fn is_playing(&self) -> bool {
        self.shared.state().is_playing()
    }

    /// Current lifecycle state.
    pub fn state(&self) -> PlaybackState {
        self.shared.state()
    }

    /// Request a seek to an absolute position in seconds.
    ///
    /// The seek itself happens on the demuxer thread; this returns immediately.
    pub fn seek(&self, position: f64) {
        let duration = *self.shared.duration.lock();
        let target = if duration > 0.0 {
            position.clamp(0.0, (duration - 0.05).max(0.0))
        } else {
            position.max(0.0)
        };
        *self.shared.seek_request.lock() = Some(target);
    }

    /// Seek by a relative offset.
    pub fn seek_relative(&self, delta: f64) {
        let now = self.shared.clock.now();
        self.seek(now + delta);
    }

    /// Seek by a percentage of the total duration, `0.0..=1.0`.
    pub fn seek_fraction(&self, fraction: f64) {
        let duration = *self.shared.duration.lock();
        if duration > 0.0 {
            self.seek(duration * fraction.clamp(0.0, 1.0));
        }
    }

    /// Set the playback rate (0.25x – 4x). Pitch is preserved.
    pub fn set_speed(&self, speed: f64) {
        let speed = speed.clamp(crate::dsp::TimeStretcher::MIN_SPEED, crate::dsp::TimeStretcher::MAX_SPEED);
        self.shared.speed_bits.store(speed.to_bits(), Ordering::Relaxed);
        self.shared.clock.set_speed(speed);
    }

    /// Current playback rate.
    pub fn speed(&self) -> f64 {
        self.shared.speed()
    }

    /// Set the linear volume (`1.0` is unity).
    pub fn set_volume(&self, volume: f32) {
        let volume = volume.clamp(0.0, 2.0);
        self.shared
            .volume_bits
            .store((volume as f64).to_bits(), Ordering::Relaxed);
        if volume > 0.0 {
            self.set_muted(false);
        }
    }

    /// Current linear volume.
    pub fn volume(&self) -> f32 {
        self.shared.volume()
    }

    /// Mute or unmute.
    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
    }

    /// `true` when muted.
    pub fn is_muted(&self) -> bool {
        self.shared.muted.load(Ordering::Relaxed)
    }

    /// Select an audio stream by container index, or `None` to disable audio.
    pub fn set_audio_track(&self, index: Option<usize>) {
        let value = match index {
            Some(i) => i as i64,
            None => -1,
        };
        self.shared.audio_track.store(value, Ordering::Relaxed);
    }

    /// Index of the selected audio stream.
    pub fn audio_track(&self) -> Option<usize> {
        let v = self.shared.audio_track.load(Ordering::Relaxed);
        if v < 0 {
            None
        } else {
            Some(v as usize)
        }
    }

    /// Select a subtitle stream by container index; `None` turns subtitles off.
    pub fn set_subtitle_track(&self, index: Option<usize>) {
        let value = match index {
            Some(i) => i as i64,
            None => -1,
        };
        self.shared.subtitle_track.store(value, Ordering::Relaxed);
        // Turning embedded subtitles off must also drop the decoded cues.
        if index.is_none() {
            *self.shared.subtitle.lock() = None;
            let _ = self
                .shared
                .event_tx
                .send(EngineEvent::SubtitleChanged(Arc::new(Subtitle::empty())));
        }
    }

    /// Index of the selected subtitle stream.
    pub fn subtitle_track(&self) -> Option<usize> {
        let v = self.shared.subtitle_track.load(Ordering::Relaxed);
        if v < 0 {
            None
        } else {
            Some(v as usize)
        }
    }

    /// Provide an external subtitle file (or clear it with `None`).
    pub fn set_external_subtitle(&self, subtitle: Option<Arc<Subtitle>>) {
        let empty = subtitle.is_none();
        *self.shared.external_subtitle.lock() = subtitle.clone();
        if empty {
            *self.shared.subtitle.lock() = None;
            let _ = self
                .shared
                .event_tx
                .send(EngineEvent::SubtitleChanged(Arc::new(Subtitle::empty())));
        } else {
            *self.shared.subtitle.lock() = subtitle.clone();
            if let Some(s) = subtitle {
                let _ = self
                    .shared
                    .event_tx
                    .send(EngineEvent::SubtitleChanged(s));
            }
        }
    }

    /// Currently active subtitle cues (external wins over embedded).
    pub fn subtitle(&self) -> Option<Arc<Subtitle>> {
        self.shared.subtitle.lock().clone()
    }

    /// Shift the audio relative to the video, in seconds (positive delays audio).
    pub fn set_audio_delay(&self, seconds: f64) {
        self.shared
            .audio_delay_bits
            .store(seconds.clamp(-10.0, 10.0).to_bits(), Ordering::Relaxed);
    }

    /// Current audio delay.
    pub fn audio_delay(&self) -> f64 {
        f64::from_bits(self.shared.audio_delay_bits.load(Ordering::Relaxed))
    }

    /// Shift subtitles in seconds (positive shows them later).
    pub fn set_subtitle_delay(&self, seconds: f64) {
        self.shared
            .subtitle_delay_bits
            .store(seconds.clamp(-60.0, 60.0).to_bits(), Ordering::Relaxed);
    }

    /// Current subtitle delay.
    pub fn subtitle_delay(&self) -> f64 {
        f64::from_bits(self.shared.subtitle_delay_bits.load(Ordering::Relaxed))
    }

    /// Set or clear an A–B loop range in seconds.
    pub fn set_ab_loop(&self, range: Option<(f64, f64)>) {
        *self.shared.ab_loop.lock() = range;
    }

    /// The current A–B loop range.
    pub fn ab_loop(&self) -> Option<(f64, f64)> {
        *self.shared.ab_loop.lock()
    }

    /// Repeat from the beginning when the end is reached.
    pub fn set_looping(&self, looping: bool) {
        self.shared.looping.store(looping, Ordering::Relaxed);
    }

    /// `true` when the file repeats.
    pub fn is_looping(&self) -> bool {
        self.shared.looping.load(Ordering::Relaxed)
    }

    /// Enable or disable hardware decoding for the next decoder restart.
    pub fn set_hardware_decoding(&self, enabled: bool) {
        self.shared.hw_decoding.store(enabled, Ordering::Relaxed);
    }

    /// `true` when hardware decoding is requested.
    pub fn hardware_decoding(&self) -> bool {
        self.shared.hw_decoding.load(Ordering::Relaxed)
    }

    /// Tell the engine the size the video is displayed at, so frames can be
    /// scaled during conversion instead of by the GPU.
    ///
    /// This is the single biggest memory and bandwidth saving in the pipeline:
    /// a 4K file in a 1080p window decodes into 1080p RGBA buffers.
    pub fn set_target_size(&self, width: u32, height: u32) {
        let size = (width.max(16), height.max(16));
        let mut current = self.shared.target_size.lock();
        if *current != Some(size) {
            *current = Some(size);
        }
    }

    /// Advance by `steps` frames while paused (negative steps are ignored,
    /// stepping backwards is not supported by this engine).
    pub fn step_frame(&self, steps: i32) {
        if steps <= 0 {
            return;
        }
        self.pause();
        self.shared.stepping.fetch_add(steps as i64, Ordering::Relaxed);
    }

    /// Total duration in seconds.
    pub fn duration(&self) -> f64 {
        *self.shared.duration.lock()
    }

    /// Current position in seconds.
    pub fn position(&self) -> f64 {
        self.shared.clock.now()
    }

    /// Position with the user's audio delay applied (what the OSD shows).
    pub fn display_position(&self) -> f64 {
        (self.shared.clock.now() + self.audio_delay()).max(0.0)
    }

    /// Probed information about the open file.
    pub fn info(&self) -> Option<Arc<MediaInfo>> {
        self.shared.info.lock().clone()
    }

    /// Pull the next displayable frame.
    ///
    /// Returns `Some` only when a *new* frame became due; the caller is expected
    /// to keep showing the previous one otherwise.
    ///
    /// The returned `Arc` is the **only** reference: the caller is expected to
    /// move the pixel buffer straight into a GPU texture, which is why nothing
    /// inside the engine keeps a copy. Anything that needs the pixels later
    /// (a snapshot, for instance) has to remember them itself.
    ///
    /// While a frame-step is pending the timestamp check is bypassed and exactly
    /// one frame is taken from the queue, which is what makes single-frame
    /// stepping work while paused.
    pub fn take_frame(&self, now: f64) -> Option<Arc<VideoFrame>> {
        if self.shared.stepping.load(Ordering::Relaxed) > 0 {
            let frame = self.shared.video_queue.lock().pop_front();
            if let Some(frame) = frame {
                self.shared.stepping.fetch_sub(1, Ordering::Relaxed);
                self.shared.clock.seek(frame.pts);
                return Some(frame);
            }
            return None;
        }
        let tolerance = 0.005;
        self.shared.video_queue.lock().take_ready(now, tolerance)
    }

    /// The subtitle cue that should be visible at `time` (seconds, already
    /// adjusted for the user's subtitle delay).
    pub fn active_subtitle(&self, time: f64) -> Option<mvp_subtitle::Cue> {
        let subtitle = self.shared.subtitle.lock();
        let subtitle = subtitle.as_ref()?;
        subtitle.active_at(time).cloned()
    }

    /// Non-blocking read of the next engine event.
    pub fn poll_event(&self) -> Option<EngineEvent> {
        self.events.try_recv().ok()
    }

    /// Take a consistent snapshot of everything the UI needs this frame.
    pub fn snapshot_state(&self) -> EngineSnapshot {
        let (queued_frames, video_queue_seconds) = {
            let queue = self.shared.video_queue.lock();
            let now = self.shared.clock.now();
            let seconds = queue
                .front_pts()
                .map(|pts| (pts - now).max(0.0))
                .unwrap_or(0.0);
            (queue.len(), seconds)
        };
        let audio = self.shared.audio.lock();
        EngineSnapshot {
            state: self.shared.state(),
            position: self.shared.clock.now(),
            duration: *self.shared.duration.lock(),
            speed: self.shared.speed(),
            volume: self.shared.volume(),
            muted: self.shared.muted.load(Ordering::Relaxed),
            audio_track: self.audio_track(),
            subtitle_track: self.subtitle_track(),
            info: self.shared.info.lock().clone(),
            audio_clock: self.shared.clock.is_audio_driven(),
            queued_frames,
            video_queue_seconds,
            audio_queue_seconds: audio.as_ref().map(|a| a.queued_seconds()).unwrap_or(0.0),
            dropped_frames: self.shared.dropped_frames.load(Ordering::Relaxed),
            decoded_frames: self.shared.decoded_frames.load(Ordering::Relaxed),
            dropped_audio: audio.as_ref().map(|a| a.dropped_chunks()).unwrap_or(0),
            underruns: audio.as_ref().map(|a| a.underruns()).unwrap_or(0),
            seeking: self.shared.seek_request.lock().is_some(),
            audio_delay: self.audio_delay(),
            subtitle_delay: self.subtitle_delay(),
            ab_loop: *self.shared.ab_loop.lock(),
            looping: self.shared.looping.load(Ordering::Relaxed),
            audio_ended: self.shared.audio_ended.load(Ordering::Relaxed),
        }
    }

    /// The source currently open, if any.
    pub fn source(&self) -> Option<MediaSource> {
        self.source.lock().clone()
    }

    /// Seconds until the next frame is due, used to schedule the next repaint.
    pub fn time_until_next_frame(&self) -> Option<f64> {
        let now = self.shared.clock.now();
        self.shared.video_queue.lock().time_until_next(now)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // A dropped-but-running engine used to leave its workers and the audio
        // device alive, which is enough to wedge a process during teardown.
        // `stop` is idempotent and every worker polls the abort flag, so this is
        // bounded and safe from any thread that is not itself a worker.
        self.stop();
    }
}

/// Convert an FFmpeg `Rational` to `f64`, guarding against a zero denominator.
pub(crate) fn rational_to_f64(value: ffmpeg::Rational) -> f64 {
    let denominator = value.denominator();
    if denominator == 0 {
        0.0
    } else {
        value.numerator() as f64 / denominator as f64
    }
}
