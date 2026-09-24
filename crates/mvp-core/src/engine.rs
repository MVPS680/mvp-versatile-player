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
use parking_lot::{Condvar, Mutex};

use crate::audio::AudioSink;
use crate::bitmap_subtitle::BitmapSubtitle;
use crate::error::{MediaError, Result};
use crate::info::MediaInfo;
use crate::util::MsEwma;
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
    /// Graphical (bitmap) subtitle cues became available.
    BitmapSubtitleChanged(Arc<BitmapSubtitle>),
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
    /// Bring HDR (PQ / HLG) frames into the range an SDR display can show.
    pub hdr_tone_map: bool,
    /// Apply the Dolby Vision reshaping the RPU describes (see
    /// `mvp_core::dolby::reshape`; off by default because only half of it has
    /// been checked against a reference).
    pub dv_reshape: bool,
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
    /// Upper bound on queued frames expressed as seconds of media.
    ///
    /// Bounds *lag* rather than memory: the decoder may not read and convert
    /// more material than this ahead of the playhead. `0.0` disables the bound
    /// and leaves only the byte and frame limits.
    pub frame_queue_seconds: f64,
    /// Byte budget for the RGB buffer free-list.
    pub buffer_pool_bytes: usize,
    /// Start playing as soon as the file is open.
    pub autoplay: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            hardware_decoding: true,
            hdr_tone_map: true,
            dv_reshape: false,
            audio_enabled: true,
            audio_device: None,
            volume: 1.0,
            speed: 1.0,
            // ~192 MB of decoded video: about 6 frames of 1080p RGBA, or 2 of
            // 4K, which is plenty of slack without ever being a memory hog.
            frame_queue_bytes: 192 * 1024 * 1024,
            frame_queue_frames: 16,
            // ...and no more than a quarter of a second of *media*. Bytes alone
            // bound memory but not latency: at 24 fps the byte budget would let
            // the decoder sit on ten frames, which is the better part of a
            // second of material read and converted before anyone asks for it.
            frame_queue_seconds: 0.25,
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
    /// Frames thrown away *before* they were downloaded and converted.
    ///
    /// A seek lands on the keyframe before the target, and the frames between
    /// that keyframe and the target are of no interest to anyone; discarding
    /// them here rather than downstream is the difference between paying for a
    /// GPU copy and a colour conversion or not paying for it.
    pub skipped_frames: u64,
    /// Frames that were decoded and converted and then replaced by a newer
    /// frame before the interface asked for one.
    ///
    /// High-frame-rate material on a slower screen spends its time here: at
    /// 120 fps on a 60 Hz display every other converted frame is superseded
    /// before it can be shown. The number is what separates "the decoder cannot
    /// keep up" from "the decoder is working on frames nobody sees".
    pub presentation_drops: u64,
    /// Milliseconds a frame spent being copied out of GPU memory.
    pub download_ms: f32,
    /// Milliseconds a frame spent in colour conversion and scaling.
    pub convert_ms: f32,
    /// The part of `convert_ms` spent tone mapping HDR into SDR range.
    pub tone_map_ms: f32,
    /// Milliseconds the decoder spent waiting for room in the frame queue.
    pub queue_wait_ms: f32,
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

/// Where the video worker's time goes, and how many frames never reach the
/// screen.
///
/// Every field is an atomic that one thread writes and the statistics panel
/// reads: no lock, no allocation, safe to touch on every frame.
#[derive(Debug, Default)]
pub(crate) struct VideoPerf {
    /// Frames discarded before the copy and the conversion.
    pub skipped_frames: AtomicU64,
    /// Frames discarded at presentation time.
    pub presentation_drops: AtomicU64,
    /// GPU→CPU copy of a hardware-decoded frame.
    pub download_ms: MsEwma,
    /// Colour conversion and scaling.
    pub convert_ms: MsEwma,
    /// HDR→SDR tone mapping, a part of `convert_ms`.
    pub tone_map_ms: MsEwma,
    /// Waiting for room in the frame queue (the presentation-lead gate).
    pub queue_wait_ms: MsEwma,
}

/// Shared state between the UI thread and the workers.
pub(crate) struct Shared {
    pub state: Mutex<PlaybackState>,
    pub info: Mutex<Option<Arc<MediaInfo>>>,
    pub duration: Mutex<f64>,
    pub clock: Clock,
    pub video_queue: Mutex<VideoQueue>,
    /// Signalled whenever the video queue gains room, so the decoder can wait
    /// for space instead of polling for it.
    pub video_ready: Condvar,
    pub perf: VideoPerf,
    pub pool: FramePool,
    pub subtitle: Mutex<Option<Arc<Subtitle>>>,
    pub external_subtitle: Mutex<Option<Arc<Subtitle>>>,
    /// Graphical cues currently on screen (embedded or external).
    pub bitmap_subtitle: Mutex<Option<Arc<BitmapSubtitle>>>,
    /// The external graphical subtitle in force, if any.
    pub external_bitmap: Mutex<Option<Arc<BitmapSubtitle>>>,
    pub audio: Mutex<Option<Arc<AudioSink>>>,
    /// Audio enhancement parameters, published by the interface and read by the
    /// device callback. The very same `Arc` is handed to the sink, so a change
    /// here is heard within one audio buffer rather than after the decode-ahead.
    pub audio_enhance: Arc<crate::dsp::EnhanceParams>,
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
    /// Bring HDR frames into SDR range on the way to the screen.
    pub hdr_tone_map: AtomicBool,
    /// Apply the Dolby Vision reshaping on the way to the screen.
    pub dv_reshape: AtomicBool,
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
            video_queue: Mutex::new(VideoQueue::with_time_budget(
                config.frame_queue_bytes,
                config.frame_queue_frames,
                config.frame_queue_seconds,
            )),
            video_ready: Condvar::new(),
            perf: VideoPerf::default(),
            pool: FramePool::new(config.buffer_pool_bytes),
            subtitle: Mutex::new(None),
            external_subtitle: Mutex::new(None),
            bitmap_subtitle: Mutex::new(None),
            external_bitmap: Mutex::new(None),
            audio: Mutex::new(None),
            // Built here, empty and bypassed: the interface publishes the persisted
            // settings as soon as it has them, and until then the callback does
            // nothing at all.
            audio_enhance: Arc::new(crate::dsp::EnhanceParams::default()),
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
            hdr_tone_map: AtomicBool::new(config.hdr_tone_map),
            dv_reshape: AtomicBool::new(config.dv_reshape),
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
        // The video worker may be parked waiting for room in a queue that this
        // seek is about to empty. Without this it would sit there until its
        // safety poll expired, and a seek would feel a poll interval late.
        self.wake_video();
    }

    /// Wake the video decoder: room has appeared in the frame queue.
    ///
    /// Called wherever frames leave the queue — the interface taking one, a
    /// seek emptying it, a flush being acted on — because the decoder waits for
    /// space rather than polling for it.
    pub fn wake_video(&self) {
        self.video_ready.notify_all();
    }

    /// Take a pending flush request for the video worker.
    pub fn take_video_flush(&self) -> Option<FlushRequest> {
        self.video_flush.lock().take()
    }

    /// Take a pending flush request for the audio worker.
    pub fn take_audio_flush(&self) -> Option<FlushRequest> {
        self.audio_flush.lock().take()
    }

    /// The output device, when the open file has one.
    ///
    /// Cloned out of the lock rather than borrowed from it. Everything that can
    /// be done with the sink — `set_paused`, `flush`, `close`, and dropping the
    /// last reference, which stops the stream — goes down into the audio driver,
    /// and holding `audio` across that would freeze every other thread that
    /// wants the device. The interface asks for it on every frame, so a driver
    /// call that takes its time would show up as a window that stops responding
    /// rather than as an error. This is the same trap the seek request slot
    /// documents in `demuxer_main`.
    fn audio_sink(&self) -> Option<Arc<AudioSink>> {
        self.audio.lock().clone()
    }

    /// `true` when an external subtitle file (text or graphical) is in force.
    ///
    /// An external side-car always wins over what is muxed into the container,
    /// and the two kinds are mutually exclusive: loading one clears the other.
    pub fn has_external_subtitle(&self) -> bool {
        self.external_subtitle.lock().is_some() || self.external_bitmap.lock().is_some()
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
        *self.shared.bitmap_subtitle.lock() = None;
        *self.shared.external_bitmap.lock() = None;
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
        // Dropped outside the lock: losing the last reference stops the stream,
        // which is a call into the audio driver.
        let previous = self.shared.audio.lock().take();
        drop(previous);
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
    ///
    /// Bounded and quick on purpose: this runs when the window closes, and a
    /// teardown that waits for the decoder backlog is exactly the "the window
    /// hangs for a moment before it goes away" the player must not have. The
    /// order matters —
    ///
    /// * the abort flag is raised first, and every worker checks it at the top
    ///   of its loop, so the packets still in the channels are abandoned rather
    ///   than decoded;
    /// * the audio device is told to stop accepting chunks, so a decoder parked
    ///   in the back-pressure wait (which a paused device never drains) returns
    ///   immediately;
    /// * only then are the threads joined.
    pub fn stop(&self) {
        self.shared.abort.store(true, Ordering::SeqCst);
        // The video worker may be parked on the frame queue waiting for room
        // that is not coming. Waking it here is what keeps a stop from waiting
        // out that worker's safety poll — the whole point of raising the flag
        // first is that the teardown does not wait for the decoder.
        self.shared.wake_video();
        if let Some(sink) = self.shared.audio_sink() {
            sink.close();
        }
        let thread = self.threads.lock().pop();
        if let Some(handle) = thread {
            join_bounded(handle);
        }
        // Drained first, joined after: `spawn_worker` takes this lock from the
        // demuxer thread, and a join is not something to hold it across.
        let workers: Vec<_> = self.shared.workers.lock().drain(..).collect();
        for handle in workers {
            join_bounded(handle);
        }
        self.shared.video_queue.lock().clear();
        self.shared.pool.clear();
        self.shared.clock.set_running(false);
        // The last reference stops the device, so it is released outside the
        // lock as well.
        let sink = self.shared.audio.lock().take();
        if let Some(sink) = sink {
            sink.flush(0.0);
            drop(sink);
        }
        self.shared.clock.set_audio(None);
        if self.shared.state().is_active() {
            self.shared.set_state(PlaybackState::Idle);
        }
    }

    /// Begin or resume playback.
    ///
    /// Pressing play on a file that has run to its end is a **replay**: the
    /// pipeline is parked on the last frame with nothing left to decode, so
    /// merely restarting the clock would let the position climb past the end of
    /// the media.
    pub fn play(&self) {
        let state = self.shared.state();
        if state == PlaybackState::Ended {
            // A live stream has no beginning to return to; leaving it ended is
            // the honest answer, and better than advancing a clock through a
            // stream that is already over.
            if self.duration() > 0.0 {
                self.restart_after_end(0.0);
            }
            return;
        }
        if !state.is_active() {
            return;
        }
        if let Some(sink) = self.shared.audio_sink() {
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
        if let Some(sink) = self.shared.audio_sink() {
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
    /// On a file that already ended, a seek is a replay from that point (see
    /// [`Engine::restart_after_end`]).
    pub fn seek(&self, position: f64) {
        if self.shared.state() == PlaybackState::Ended {
            if self.duration() > 0.0 {
                self.restart_after_end(position);
            }
            return;
        }
        let duration = *self.shared.duration.lock();
        *self.shared.seek_request.lock() = Some(clamp_position(position, duration));
    }

    /// Restart a finished file at `position`, resuming playback there.
    ///
    /// The demuxer deliberately stays alive once it reaches the end of the
    /// stream (see `workers::handle_eof`): it parks with the clock stopped and
    /// the decoders idle, and the seek requested below is what wakes it up. The
    /// clock is put back in motion here because parking stopped it, and
    /// [`Clock::seek`] flushes the sound card so no tail of the finished audio
    /// survives into the replay.
    ///
    /// Without this, `play` on a finished file only restarted the clock, which
    /// then kept climbing past the media's duration with nothing left to show.
    fn restart_after_end(&self, position: f64) {
        let target = clamp_position(position, *self.shared.duration.lock());
        self.shared.clock.seek(target);
        self.shared.clock.set_running(true);
        if let Some(sink) = self.shared.audio_sink() {
            sink.set_paused(false);
        }
        self.shared.set_state(PlaybackState::Playing);
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
        self.publish_gain();
    }

    /// Current linear volume.
    pub fn volume(&self) -> f32 {
        self.shared.volume()
    }

    /// Mute or unmute.
    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
        self.publish_gain();
    }

    /// `true` when muted.
    pub fn is_muted(&self) -> bool {
        self.shared.muted.load(Ordering::Relaxed)
    }

    /// Publish a whole set of audio enhancement parameters.
    ///
    /// The whole set travels at once, through the atomics the device callback
    /// reads — no channel, no lock, no queue: putting a control message on the
    /// packet channels is the deadlock this engine is built to avoid, and a
    /// parameter change has to be audible *now*, not after the decode-ahead.
    ///
    /// Cheap enough to call from a slider's every frame; the callback smooths
    /// towards it, so calling it forty times a second changes nothing.
    pub fn set_audio_enhance(&self, snapshot: crate::dsp::Snapshot) {
        self.shared.audio_enhance.publish(snapshot);
    }

    /// `true` once a chain has been built for the open output device.
    ///
    /// `false` means there is no device (or audio is disabled): the interface
    /// says so instead of offering controls that would do nothing.
    pub fn audio_enhance_ready(&self) -> bool {
        self.shared.audio_enhance.ready.load(Ordering::Relaxed)
    }

    /// Delay the enhancement chain adds, in milliseconds (`0.0` when bypassed).
    pub fn audio_enhance_latency_ms(&self) -> f32 {
        self.shared.audio_enhance.latency_ms()
    }

    /// Sample rate of the open output device.
    ///
    /// Anything that draws a frequency response needs it: 16 kHz simply cannot exist at a
    /// 22 kHz rate, and a curve that claimed otherwise would be lying about the sound.
    /// The conventional 48 kHz when there is no device at all — right for almost every
    /// Windows output, and better than drawing nothing.
    pub fn audio_sample_rate(&self) -> f32 {
        match self.shared.audio_sink() {
            Some(sink) => sink.sample_rate() as f32,
            None => 48_000.0,
        }
    }

    /// The gain the decoder is actually applying to the samples it produces.
    ///
    /// Volume and mute live in two places — the engine's own state and the
    /// output device — and this is the one that reaches the speakers. It is
    /// what the interface should show when it wants to say "how loud is it
    /// really", and it is what [`Engine::set_volume`] has to keep in step.
    pub fn effective_gain(&self) -> f32 {
        match self.shared.audio_sink() {
            Some(sink) => sink.effective_gain(),
            // No device (or audio disabled): the engine's own values are what
            // the producer would use.
            None => {
                if self.is_muted() {
                    0.0
                } else {
                    self.volume()
                }
            }
        }
    }

    /// Hand the current volume and mute state to the running output device.
    ///
    /// Writing the engine's atomics is not enough: the decoder worker scales
    /// every chunk with the *sink's* gain, so a volume change that is not
    /// forwarded moves a number on screen and nothing else.
    fn publish_gain(&self) {
        if let Some(sink) = self.shared.audio_sink() {
            sink.set_volume(self.shared.volume());
            sink.set_muted(self.shared.muted.load(Ordering::Relaxed));
        }
    }

    /// Select an audio stream by container index, or `None` to disable audio.
    pub fn set_audio_track(&self, index: Option<usize>) {
        let value = match index {
            Some(i) => i as i64,
            None => TRACK_OFF,
        };
        self.shared.audio_track.store(value, Ordering::Relaxed);
    }

    /// Index of the audio stream actually in use.
    ///
    /// `None` means audio is off — not that the selection is still "auto", so
    /// an interface can highlight the right row on a freshly opened file.
    pub fn audio_track(&self) -> Option<usize> {
        let selector = self.shared.audio_track.load(Ordering::Relaxed);
        let auto = self
            .info()
            .and_then(|info| info.audio.first().map(|stream| stream.index));
        resolve_selector(selector, auto)
    }

    /// Select a subtitle stream by container index; `None` turns subtitles off.
    pub fn set_subtitle_track(&self, index: Option<usize>) {
        let value = match index {
            Some(i) => i as i64,
            None => TRACK_OFF,
        };
        self.shared.subtitle_track.store(value, Ordering::Relaxed);
        // Turning embedded subtitles off must also drop the decoded cues, of
        // both kinds: text and graphical tracks share one selector.
        if index.is_none() {
            *self.shared.subtitle.lock() = None;
            *self.shared.bitmap_subtitle.lock() = None;
            let _ = self
                .shared
                .event_tx
                .send(EngineEvent::SubtitleChanged(Arc::new(Subtitle::empty())));
            let _ = self
                .shared
                .event_tx
                .send(EngineEvent::BitmapSubtitleChanged(Arc::new(
                    BitmapSubtitle::empty(),
                )));
        }
    }

    /// Hand subtitle selection back to the file's own default track.
    ///
    /// This is what "show subtitles when the file has them" means: the player
    /// does not pick a stream, it lets the container do it.
    pub fn set_subtitle_track_auto(&self) {
        self.shared
            .subtitle_track
            .store(TRACK_AUTO, Ordering::Relaxed);
    }

    /// Index of the subtitle stream actually in use.
    ///
    /// Only streams we can draw are offered: text tracks and graphical ones
    /// (PGS/VobSub/DVB), whose bitmaps are shown directly. "Auto" resolves to
    /// the file's default such track, falling back to the first one.
    pub fn subtitle_track(&self) -> Option<usize> {
        let selector = self.shared.subtitle_track.load(Ordering::Relaxed);
        let auto = self.info().and_then(|info| {
            info.subtitles
                .iter()
                .find(|stream| stream.is_default && stream.is_renderable())
                .or_else(|| info.subtitles.iter().find(|stream| stream.is_renderable()))
                .map(|stream| stream.index)
        });
        resolve_selector(selector, auto)
    }

    /// Provide an external subtitle file (or clear it with `None`).
    ///
    /// Loading a text side-car clears any external graphical one: the two are
    /// alternatives for the same slot, not layers.
    pub fn set_external_subtitle(&self, subtitle: Option<Arc<Subtitle>>) {
        *self.shared.external_bitmap.lock() = None;
        *self.shared.external_subtitle.lock() = subtitle.clone();
        if let Some(s) = subtitle {
            // The text side-car takes over the slot: hide the embedded bitmap.
            *self.shared.bitmap_subtitle.lock() = None;
            *self.shared.subtitle.lock() = Some(Arc::clone(&s));
            let _ = self.shared.event_tx.send(EngineEvent::SubtitleChanged(s));
        } else {
            *self.shared.subtitle.lock() = None;
            let _ = self
                .shared
                .event_tx
                .send(EngineEvent::SubtitleChanged(Arc::new(Subtitle::empty())));
        }
    }

    /// Provide an external graphical subtitle file (or clear it with `None`).
    ///
    /// Mirrors [`Engine::set_external_subtitle`] for bitmap tracks: it takes the
    /// external slot, drops any text side-car, and hides the embedded cues.
    pub fn set_external_bitmap_subtitle(&self, subtitle: Option<Arc<BitmapSubtitle>>) {
        *self.shared.external_subtitle.lock() = None;
        *self.shared.external_bitmap.lock() = subtitle.clone();
        match subtitle {
            Some(track) => {
                *self.shared.subtitle.lock() = None;
                *self.shared.bitmap_subtitle.lock() = Some(Arc::clone(&track));
                let _ = self
                    .shared
                    .event_tx
                    .send(EngineEvent::BitmapSubtitleChanged(track));
            }
            None => {
                *self.shared.bitmap_subtitle.lock() = None;
                let _ = self
                    .shared
                    .event_tx
                    .send(EngineEvent::BitmapSubtitleChanged(Arc::new(
                        BitmapSubtitle::empty(),
                    )));
            }
        }
    }

    /// Currently active subtitle cues (external wins over embedded).
    pub fn subtitle(&self) -> Option<Arc<Subtitle>> {
        self.shared.subtitle.lock().clone()
    }

    /// Currently active graphical subtitle cues (external wins over embedded).
    pub fn bitmap_subtitle(&self) -> Option<Arc<BitmapSubtitle>> {
        self.shared.bitmap_subtitle.lock().clone()
    }

    /// `true` when an external subtitle file (text or graphical) is loaded.
    pub fn has_external_subtitle(&self) -> bool {
        self.shared.has_external_subtitle()
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

    /// Bring HDR (PQ / HLG) frames into the range an SDR display can show.
    ///
    /// Takes effect on the next frame: the decoder re-reads the flag for each
    /// frame it converts, so flipping the switch does not need the file to be
    /// reopened.
    pub fn set_hdr_tone_map(&self, enabled: bool) {
        self.shared.hdr_tone_map.store(enabled, Ordering::Relaxed);
    }

    /// `true` when HDR frames are being tone mapped for display.
    pub fn hdr_tone_map(&self) -> bool {
        self.shared.hdr_tone_map.load(Ordering::Relaxed)
    }

    /// Turn the Dolby Vision reshaping on or off.
    ///
    /// Takes effect on the next frame, like [`Engine::set_hdr_tone_map`], and
    /// costs nothing while it is off: the converter never enters the reshaping
    /// pass at all.
    pub fn set_dv_reshape(&self, enabled: bool) {
        self.shared.dv_reshape.store(enabled, Ordering::Relaxed);
    }

    /// `true` when the Dolby Vision reshaping is being applied.
    pub fn dv_reshape(&self) -> bool {
        self.shared.dv_reshape.load(Ordering::Relaxed)
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

    /// Advance `steps` frames while paused.
    ///
    /// Stepping *backwards* is not something this engine can do: the decoder is
    /// a forward-only pipeline, and the frames it hands out are moved straight
    /// into a GPU texture by the caller, so nothing here may keep a reference to
    /// one. The interface keeps its own history of the images it uploaded and
    /// moves the playhead with [`Engine::set_playhead`] for a backward step.
    pub fn step_frame(&self, steps: i32) {
        if steps <= 0 || !self.shared.state().is_active() {
            return;
        }
        self.pause();
        self.shared.stepping.fetch_add(steps as i64, Ordering::Relaxed);
    }

    /// Move the playhead without touching the container.
    ///
    /// Used for single-frame stepping, where the picture comes from the
    /// interface's own frame history rather than from the decoder: seeking the
    /// container instead would flush the queues and lose the exact position the
    /// user is stepping through.
    pub fn set_playhead(&self, position: f64) {
        self.shared.clock.seek(position.max(0.0));
    }

    /// Total duration in seconds.
    pub fn duration(&self) -> f64 {
        *self.shared.duration.lock()
    }

    /// Current position in seconds.
    pub fn position(&self) -> f64 {
        self.within_media(self.shared.clock.now())
    }

    /// Position with the user's audio delay applied (what the OSD shows).
    pub fn display_position(&self) -> f64 {
        self.within_media((self.shared.clock.now() + self.audio_delay()).max(0.0))
    }

    /// Hold a reported position inside the media.
    ///
    /// The master clock is a *wall* clock and knows nothing about the length of
    /// the media: a seek re-anchors it forward, and nothing stops it while the
    /// demuxer works out where that seek landed or how much of the file is left
    /// after it. A click near the end of a file therefore put a position past
    /// its own duration in front of the user — "0:11" under a "0:09" total,
    /// with the progress bar pinned full — and a positive audio delay could do
    /// the same at the end of any file. Clamping here fixes every reader at
    /// once: the transport bar, the OSD and the resume bookmark all take their
    /// position from this one place.
    ///
    /// It is deliberately the *reported* position that is clamped, not the
    /// clock: `handle_eof` and the A–B loop compare the clock against the
    /// duration to decide when the file has ended, and a clock that could not
    /// pass the duration could never reach that decision. A live stream has no
    /// duration to clamp against and is left alone.
    fn within_media(&self, position: f64) -> f64 {
        let duration = *self.shared.duration.lock();
        if duration > 0.0 {
            position.min(duration)
        } else {
            position
        }
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
                // One frame left the queue: the decoder may be waiting for
                // exactly that.
                self.shared.wake_video();
                return Some(frame);
            }
            return None;
        }
        let tolerance = 0.005;
        let (frame, discarded) = self
            .shared
            .video_queue
            .lock()
            .take_ready_counted(now, tolerance);
        if discarded > 0 {
            self.shared
                .perf
                .presentation_drops
                .fetch_add(discarded, Ordering::Relaxed);
            // Room appeared in the queue: wake the decoder instead of making it
            // wait out its poll.
            self.shared.wake_video();
        }
        frame
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
            skipped_frames: self.shared.perf.skipped_frames.load(Ordering::Relaxed),
            presentation_drops: self.shared.perf.presentation_drops.load(Ordering::Relaxed),
            download_ms: self.shared.perf.download_ms.get(),
            convert_ms: self.shared.perf.convert_ms.get(),
            tone_map_ms: self.shared.perf.tone_map_ms.get(),
            queue_wait_ms: self.shared.perf.queue_wait_ms.get(),
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
        //
        // Timed because this is the one part of "close the window" that is our
        // own code rather than the window manager's: if it ever grows, the log
        // says so, and the number should stay in the low tens of milliseconds.
        let started = std::time::Instant::now();
        self.stop();
        let millis = started.elapsed().as_secs_f64() * 1000.0;
        if millis > 5.0 {
            log::info!("释放播放引擎耗时 {millis:.1} ms");
        }
    }
}

/// How long [`Engine::stop`] gives a worker to notice the abort flag.
const STOP_JOIN_TIMEOUT_MS: u64 = 400;

/// Join a worker thread, but never block the caller forever.
///
/// Workers poll the abort flag every 25–50 ms, so a healthy pipeline is inside
/// this bound by a wide margin. The bound is for the unhealthy case: a demuxer
/// parked inside a long FFmpeg call — a seek across a damaged file, a stalled
/// network read — must not be able to hold the window open, which is the one
/// thing "closing is instant" promises.
///
/// Detaching is safe: every worker holds its own `Arc` to the shared state, so
/// the thread that is left behind has nothing borrowed from the caller.
fn join_bounded(handle: std::thread::JoinHandle<()>) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(STOP_JOIN_TIMEOUT_MS);
    while !handle.is_finished() {
        if std::time::Instant::now() >= deadline {
            log::warn!(
                "工作线程未在 {STOP_JOIN_TIMEOUT_MS} ms 内退出，已放弃等待并分离该线程"
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let _ = handle.join();
}

/// Clamp a requested position into the media.
///
/// A hair of room is kept before the end: seeking exactly onto the last
/// timestamp would immediately satisfy the end-of-file condition again, so a
/// "jump to the end" would look like the seek had done nothing. A live stream
/// (`duration == 0.0`) has no upper bound to clamp against.
fn clamp_position(position: f64, duration: f64) -> f64 {
    if duration > 0.0 {
        position.clamp(0.0, (duration - 0.05).max(0.0))
    } else {
        position.max(0.0)
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

/// The stream selectors the engine understands.
///
/// `-2` means "whatever the file marks as default", which is what a freshly
/// opened file uses; `-1` means "explicitly off"; anything else is a container
/// stream index. Keeping the three apart matters for the interface: reporting
/// "auto" as "off" is what made the track pickers claim the sound was disabled
/// while it was playing perfectly well.
pub(crate) const TRACK_AUTO: i64 = -2;
/// Selector meaning "no such track".
pub(crate) const TRACK_OFF: i64 = -1;

/// Resolve a raw selector to the stream index that is actually in use.
///
/// `auto` is the index the file's own default resolves to, computed by the
/// caller from the probed stream list.
fn resolve_selector(selector: i64, auto: Option<usize>) -> Option<usize> {
    match selector {
        TRACK_OFF => None,
        value if value >= 0 => Some(value as usize),
        _ => auto,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug this guards: "auto" used to be reported as "off", so a freshly
    /// opened file showed "sound muted" while it was playing, and picking the
    /// default embedded subtitle track looked like picking nothing at all.
    #[test]
    fn auto_resolves_to_the_files_default_track() {
        assert_eq!(resolve_selector(TRACK_AUTO, Some(2)), Some(2));
        assert_eq!(
            resolve_selector(TRACK_AUTO, None),
            None,
            "a file with no such stream has nothing to select"
        );
    }

    #[test]
    fn an_explicit_index_wins_over_the_default() {
        assert_eq!(resolve_selector(5, Some(2)), Some(5));
        assert_eq!(resolve_selector(0, Some(2)), Some(0));
    }

    #[test]
    fn off_is_off_whatever_the_default_is() {
        assert_eq!(resolve_selector(TRACK_OFF, Some(2)), None);
        assert_eq!(resolve_selector(TRACK_OFF, None), None);
    }

    #[test]
    fn unknown_negative_selectors_fall_back_to_the_default() {
        assert_eq!(resolve_selector(-7, Some(1)), Some(1));
    }
}
