//! Transient interface state — everything that is *not* persisted and not part
//! of the media engine.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mvp_core::playlist::RepeatMode;
use mvp_core::PlaybackState;

use crate::icons::Icon;
use crate::settings::SidebarTab;
use crate::view::{CanvasPicture, CanvasView};

/// What the main area is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Nothing is open.
    Empty,
    /// The video/audio pipeline is driving the display.
    Media,
    /// The still-image viewer is driving the display.
    Image,
}

impl Mode {
    /// Whether the media transport controls apply.
    pub fn is_media(self) -> bool {
        matches!(self, Mode::Media)
    }

    /// Whether the image tools apply.
    pub fn is_image(self) -> bool {
        matches!(self, Mode::Image)
    }
}

/// Which page the settings window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    /// Playback behaviour and start-up.
    #[default]
    General,
    /// Video output and rendering.
    Video,
    /// Audio output and volume.
    Audio,
    /// Subtitle appearance and loading.
    Subtitles,
    /// File associations and shell integration.
    Integration,
    /// Keyboard and mouse.
    Shortcuts,
    /// Version information and FFmpeg build details.
    About,
}

impl SettingsTab {
    /// Every tab, in display order.
    pub fn all() -> &'static [SettingsTab] {
        &[
            SettingsTab::General,
            SettingsTab::Video,
            SettingsTab::Audio,
            SettingsTab::Subtitles,
            SettingsTab::Integration,
            SettingsTab::Shortcuts,
            SettingsTab::About,
        ]
    }

    /// Sidebar label.
    pub fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "常规",
            SettingsTab::Video => "视频",
            SettingsTab::Audio => "音频",
            SettingsTab::Subtitles => "字幕",
            SettingsTab::Integration => "系统集成",
            SettingsTab::Shortcuts => "快捷键",
            SettingsTab::About => "关于",
        }
    }
}

/// Severity of a transient message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    /// Neutral information.
    Info,
    /// Something worked.
    Success,
    /// Something is off but recoverable.
    Warning,
    /// Something failed.
    Error,
}

/// A short message shown near the top of the video area and then faded out.
#[derive(Debug, Clone)]
pub struct Toast {
    /// Message text.
    pub text: String,
    /// Optional leading icon.
    pub icon: Option<Icon>,
    /// Severity, which selects the colour.
    pub kind: ToastKind,
    /// When it appeared.
    pub born: Instant,
    /// How long it stays fully visible.
    pub hold: Duration,
    /// How long it takes to fade out afterwards.
    pub fade: Duration,
}

impl Toast {
    /// A neutral message.
    pub fn info(text: impl Into<String>) -> Self {
        Self::new(text, ToastKind::Info, None)
    }

    /// A success message.
    pub fn success(text: impl Into<String>) -> Self {
        Self::new(text, ToastKind::Success, None)
    }

    /// A warning message.
    pub fn warning(text: impl Into<String>) -> Self {
        Self::new(text, ToastKind::Warning, None)
    }

    /// An error message.
    pub fn error(text: impl Into<String>) -> Self {
        Self::new(text, ToastKind::Error, None)
    }

    /// A message with a leading icon.
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    fn new(text: impl Into<String>, kind: ToastKind, icon: Option<Icon>) -> Self {
        Self {
            text: text.into(),
            icon,
            kind,
            born: Instant::now(),
            hold: Duration::from_millis(1800),
            fade: Duration::from_millis(600),
        }
    }

    /// Opacity in `0.0..=1.0`, accounting for the fade-out.
    pub fn opacity(&self) -> f32 {
        let age = self.born.elapsed();
        if age <= self.hold {
            1.0
        } else {
            let over = age - self.hold;
            (1.0 - over.as_secs_f32() / self.fade.as_secs_f32()).clamp(0.0, 1.0)
        }
    }

    /// `true` once the message is completely gone.
    pub fn is_finished(&self) -> bool {
        self.born.elapsed() >= self.hold + self.fade
    }
}

/// Which overlay panel is open, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    /// No overlay.
    None,
    /// The settings window.
    Settings,
    /// The "open URL" prompt.
    OpenUrl,
    /// The about box.
    About,
    /// The keyboard shortcut reference.
    Shortcuts,
}

/// A single row in the media-information list.
#[derive(Debug, Clone)]
pub struct InfoRow {
    /// Row label.
    pub label: String,
    /// Row value.
    pub value: String,
}

/// Registry answers the integration page needs, read once per visit.
///
/// Every one of these is a registry round-trip; asking for them on each frame
/// while the settings window is open is enough I/O to make the whole interface
/// stutter.
#[derive(Debug, Clone, Default)]
pub struct AssocCache {
    /// Whether the player is advertised as a handler at all.
    pub registered: bool,
    /// Current handler for `.mp4`.
    pub video: Option<String>,
    /// Current handler for `.mp3`.
    pub audio: Option<String>,
    /// Current handler for `.png`.
    pub image: Option<String>,
}

/// Output devices, enumerated once per visit to the audio settings page.
///
/// Enumerating WASAPI endpoints is expensive enough that doing it per frame is
/// visible as a hitch.
#[derive(Debug, Clone, Default)]
pub struct AudioDeviceCache {
    /// Every output device name the host reports.
    pub devices: Vec<String>,
    /// Name of the current default device.
    pub default_name: String,
}

/// A frame the interface has already put on screen.
///
/// "Step back one frame" needs the frame before the current one, and the engine
/// cannot supply it: the decoder is a forward-only pipeline, and every frame it
/// hands out has its pixels moved straight into the GPU texture it belongs to,
/// which is why the engine deliberately keeps no reference to any of them — a
/// second reference would make that zero-copy hand-off impossible.
///
/// The interface's own uploaded images are therefore the only place a previous
/// frame can come from, and they cost a pointer rather than a copy: this shares
/// the very allocation the texture is displaying.
#[derive(Debug, Clone)]
pub struct ShownFrame {
    /// Presentation timestamp the frame was shown at.
    pub pts: f64,
    /// Serial the engine gave it.
    ///
    /// Needed so "is this a new frame?" still tells the truth after a step back:
    /// the engine's next frame carries a higher serial, and the remembered one
    /// must not be mistaken for it.
    pub serial: u64,
    /// Pixel data, shared with the texture that displays it.
    pub image: std::sync::Arc<egui::ColorImage>,
}

impl ShownFrame {
    /// Pixel bytes this frame occupies.
    pub fn byte_len(&self) -> usize {
        self.image.pixels.len() * std::mem::size_of::<egui::Color32>()
    }
}

/// A bounded ring of already-shown frames, newest last.
///
/// Bounded by both a frame count and a byte budget: a 4K frame is 33 MB, so
/// "remember the last thirty" would reserve a gigabyte to make one keystroke
/// work.
#[derive(Debug, Default)]
pub struct FrameHistory {
    frames: std::collections::VecDeque<ShownFrame>,
    bytes: usize,
}

impl FrameHistory {
    /// How many frames may be remembered regardless of size.
    pub const MAX_FRAMES: usize = 12;
    /// How many pixel bytes the ring may hold.
    pub const MAX_BYTES: usize = 128 * 1024 * 1024;

    /// An empty ring.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many frames are remembered.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// `true` when there is nothing to step back to.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Pixel bytes currently held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Remember a frame, dropping the oldest ones to stay inside the budget.
    pub fn push(&mut self, frame: ShownFrame) {
        self.bytes += frame.byte_len();
        self.frames.push_back(frame);
        // Always keep at least one frame, however large it is: a ring that can
        // step back once is still a working command, an empty one is not.
        while self.frames.len() > Self::MAX_FRAMES
            || (self.frames.len() > 1 && self.bytes > Self::MAX_BYTES)
        {
            if let Some(dropped) = self.frames.pop_front() {
                self.bytes = self.bytes.saturating_sub(dropped.byte_len());
            }
        }
    }

    /// Take the most recent frame back off the ring.
    pub fn pop(&mut self) -> Option<ShownFrame> {
        let frame = self.frames.pop_back()?;
        self.bytes = self.bytes.saturating_sub(frame.byte_len());
        Some(frame)
    }

    /// Forget everything (a new file, or a jump to another position).
    pub fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }
}

/// Transient UI state.
#[derive(Debug)]
pub struct UiState {
    /// Sidebar visibility.
    pub sidebar_visible: bool,
    /// Active sidebar tab.
    pub sidebar_tab: SidebarTab,
    /// Cached information rows for the active file.
    pub info_rows: Vec<InfoRow>,

    /// Seek preview while the user drags the bar, in seconds.
    pub seek_drag: Option<f64>,
    /// A position the user asked for, and when, held while the engine catches
    /// up with it.
    ///
    /// A seek is asynchronous: [`mvp_core::Engine::seek`] parks the request and
    /// the demuxer answers it a moment later, so for the frames in between the
    /// engine still reports the position the playhead is *leaving*. Drawing the
    /// bar from that makes a click snap the handle back and then jump forward
    /// again — which reads as "the click did nothing", and is exactly the
    /// moment a user clicks a second and a third time. Drawing it from the
    /// request instead holds the handle where the user put it. The hold is
    /// dropped as soon as the clock agrees with it ([`SEEK_SETTLED`]), or after
    /// [`SEEK_HOLD_LIMIT`] so that a seek which never lands cannot freeze the
    /// bar.
    pub seek_hold: Option<(f64, Instant)>,
    /// Wheel movement not yet turned into a volume step, in egui points.
    pub wheel_volume: f32,
    /// Wheel movement not yet turned into a seek step, in egui points.
    pub wheel_seek: f32,
    /// Zoom and pan of the *video* picture on the canvas. Images carry their own
    /// in [`mvp_core::ImageView`], and the commands that can be given in either
    /// mode ("适应窗口") reach both through [`crate::app::PlayerApp::reset_zoom`].
    pub canvas: CanvasView,
    /// Where the picture was when the canvas last drew one.
    ///
    /// Rebuilt every frame, and `None` on any frame that showed no picture: the
    /// zoom keys and the menu run before the canvas is laid out and have no
    /// rectangle of their own to measure, so they read what was drawn last —
    /// measuring the window instead would include the sidebar and the transport
    /// bar, and `+` would jump the first time it was pressed.
    pub picture: Option<CanvasPicture>,
    /// Row the user highlighted in the playlist.
    ///
    /// Deliberately *not* the playlist's "current" entry: clicking a row is a
    /// selection, not a play command, and moving the playing marker on a single
    /// click made the title bar announce a file that was not playing.
    pub playlist_selection: Option<usize>,

    /// Currently open overlay.
    pub overlay: Overlay,
    /// Active settings page.
    pub settings_tab: SettingsTab,
    /// Settings page the other caches were filled for.
    pub cached_settings_tab: Option<SettingsTab>,
    /// Cached file-association answers.
    pub assoc_cache: Option<AssocCache>,
    /// Cached output-device list.
    pub audio_device_cache: Option<AudioDeviceCache>,
    /// Text in the URL prompt.
    pub url_input: String,
    /// Whether the URL prompt should auto-focus its field.
    pub url_focus: bool,

    /// The latest toast, if any.
    pub toast: Option<Toast>,
    /// Error text shown as a banner until dismissed.
    pub error_banner: Option<String>,

    /// Controls stay visible until this instant (mouse movement refreshes it).
    pub controls_until: Instant,
    /// Last time the pointer moved, used for fullscreen auto-hide.
    pub last_pointer_move: Instant,
    /// Cursor position last frame, used to detect movement.
    pub last_pointer_pos: Option<egui::Pos2>,
    /// Fullscreen state as the player believes it to be.
    pub fullscreen: bool,
    /// The last fullscreen value the *window* reported.
    ///
    /// Compared against rather than written straight into `fullscreen`, so the
    /// optimistic value set by a toggle is not undone by the window's reply
    /// arriving a frame late.
    pub last_reported_fullscreen: Option<bool>,
    /// The window should be closed at the end of this frame.
    pub close_requested: bool,

    /// Milliseconds from process start to the first painted frame.
    pub startup_ms: f32,
    /// What actually reached the screen, as opposed to what was decoded.
    pub present: PresentStats,
    /// Cached FFmpeg version banner.
    pub ffmpeg_version: String,
    /// Cached FFmpeg build configuration.
    pub ffmpeg_config: String,
    /// Cached snapshot-saved message.
    pub last_snapshot: Option<PathBuf>,
}

/// A rolling view of the frame pipeline as the interface sees it.
///
/// The engine counts what it decodes; this counts what was *shown*, which is the
/// number that answers "is the player keeping up". The two disagreeing is the
/// whole diagnosis: many decoded frames and few presented ones means the
/// pipeline is producing pictures the screen never gets to see.
#[derive(Debug, Default)]
pub struct PresentStats {
    /// When the previous frame was presented, for the presentation interval.
    last_present: Option<Instant>,
    /// Time between *presentations*; its reciprocal is the frame rate on
    /// screen. Measured here rather than per repaint, because a repaint that
    /// shows the frame already on screen is not a presented frame.
    interval_ms: mvp_core::util::MsEwma,
    /// Time spent pulling a frame out of the engine and handing it to egui.
    handoff_ms: mvp_core::util::MsEwma,
    /// Frames presented since the player started.
    presented: u64,
}

impl PresentStats {
    /// Record what one frame's hand-off cost, in milliseconds.
    ///
    /// Called on every repaint, including the ones that found no new frame, so
    /// the number reflects the path rather than only its busiest frames.
    pub fn record_handoff(&mut self, ms: f32) {
        self.handoff_ms.record(ms);
    }

    /// Record that a new frame reached the screen.
    ///
    /// Only ever called when a frame was actually handed over: this is the
    /// presentation clock the frame rate is derived from, and counting
    /// repaints instead would report the interface's idle refresh as playback
    /// performance.
    pub fn record_presented(&mut self) {
        let now = Instant::now();
        if let Some(previous) = self.last_present.replace(now) {
            let interval = now.saturating_duration_since(previous).as_secs_f32() * 1000.0;
            // A pause, a seek or a file change produces a gap that is not a slow
            // frame; folding one in would hold the reading down for a second or
            // two of perfectly healthy playback.
            if interval < 1000.0 {
                self.interval_ms.record(interval);
            }
        }
        self.presented += 1;
    }

    /// Frames per second actually reaching the screen (`0.0` before the second
    /// frame).
    pub fn fps(&self) -> f32 {
        let interval = self.interval_ms.get();
        if interval > 0.01 {
            1000.0 / interval
        } else {
            0.0
        }
    }

    /// Average time spent handing one frame to the interface, in milliseconds.
    pub fn handoff_ms(&self) -> f32 {
        self.handoff_ms.get()
    }

    /// Frames presented so far.
    pub fn presented(&self) -> u64 {
        self.presented
    }

    /// Forget the timing history, keeping the lifetime counter.
    pub fn reset_timing(&mut self) {
        self.last_present = None;
        self.interval_ms = mvp_core::util::MsEwma::new();
        self.handoff_ms = mvp_core::util::MsEwma::new();
    }
}

/// How long a requested seek keeps the seek bar where the user put it.
///
/// Long enough to cover a seek that has to read into a container with no usable
/// index, short enough that a seek which never lands cannot leave the bar
/// showing a position the engine is not heading for.
pub const SEEK_HOLD_LIMIT: Duration = Duration::from_millis(600);

/// How close the engine's position has to be to a held request, in seconds,
/// before the hold is released.
///
/// A seek lands on a keyframe, so the clock the engine reports after one is a
/// little away from the position that was asked for; a third of a second is
/// inside "the click went where I meant" and outside the jitter of a clock that
/// is still catching up.
pub const SEEK_SETTLED: f64 = 0.30;

impl Default for UiState {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            sidebar_visible: true,
            sidebar_tab: SidebarTab::Playlist,
            info_rows: Vec::new(),
            seek_drag: None,
            seek_hold: None,
            wheel_volume: 0.0,
            wheel_seek: 0.0,
            canvas: CanvasView::new(),
            picture: None,
            playlist_selection: None,
            overlay: Overlay::None,
            settings_tab: SettingsTab::default(),
            cached_settings_tab: None,
            assoc_cache: None,
            audio_device_cache: None,
            url_input: String::new(),
            url_focus: false,
            toast: None,
            error_banner: None,
            controls_until: now,
            last_pointer_move: now,
            last_pointer_pos: None,
            fullscreen: false,
            last_reported_fullscreen: None,
            close_requested: false,
            startup_ms: 0.0,
            present: PresentStats::default(),
            ffmpeg_version: String::new(),
            ffmpeg_config: String::new(),
            last_snapshot: None,
        }
    }
}

impl UiState {
    /// Show a message, replacing any message already on screen.
    pub fn toast(&mut self, toast: Toast) {
        self.toast = Some(toast);
    }

    /// Drop the toast once it has faded.
    pub fn tick_toast(&mut self) {
        if self.toast.as_ref().is_some_and(Toast::is_finished) {
            self.toast = None;
        }
    }

    /// Keep the controls on screen for a while.
    pub fn wake_controls(&mut self, seconds: f32) {
        self.controls_until = Instant::now() + Duration::from_secs_f32(seconds.max(0.5));
    }

    /// `true` when the transport overlay should be drawn.
    pub fn controls_visible(&self) -> bool {
        Instant::now() < self.controls_until
    }

    /// Open an overlay.
    pub fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = overlay;
    }

    /// Close any overlay.
    pub fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
    }

    /// `true` when an overlay is capturing input.
    pub fn has_overlay(&self) -> bool {
        self.overlay != Overlay::None
    }
}

/// Human readable label for an engine state.
pub fn state_label(state: &PlaybackState) -> &'static str {
    match state {
        PlaybackState::Idle => "就绪",
        PlaybackState::Opening => "正在打开…",
        PlaybackState::Playing => "播放中",
        PlaybackState::Paused => "已暂停",
        PlaybackState::Ended => "播放结束",
        PlaybackState::Error(_) => "出错",
    }
}

/// Colour role for a state pill.
pub fn state_kind(state: &PlaybackState) -> ToastKind {
    match state {
        PlaybackState::Playing => ToastKind::Success,
        PlaybackState::Paused | PlaybackState::Opening => ToastKind::Info,
        PlaybackState::Ended => ToastKind::Info,
        PlaybackState::Error(_) => ToastKind::Error,
        PlaybackState::Idle => ToastKind::Info,
    }
}

/// Label for a repeat mode, in the transport bar.
pub fn repeat_label(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "不循环",
        RepeatMode::All => "列表循环",
        RepeatMode::One => "单个循环",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny stand-in for a decoded frame.
    fn shown(pts: f64, pixels: usize) -> ShownFrame {
        ShownFrame {
            pts,
            serial: pts as u64,
            image: std::sync::Arc::new(egui::ColorImage {
                size: [1, 1],
                source_size: egui::vec2(1.0, 1.0),
                pixels: vec![egui::Color32::TRANSPARENT; pixels],
            }),
        }
    }

    #[test]
    fn the_frame_ring_hands_back_the_newest_frame() {
        let mut ring = FrameHistory::new();
        assert!(ring.is_empty());
        for i in 0..5 {
            ring.push(shown(i as f64, 4));
        }
        assert_eq!(ring.len(), 5);
        assert_eq!(ring.pop().map(|f| f.pts), Some(4.0));
        assert_eq!(ring.pop().map(|f| f.pts), Some(3.0));
        assert_eq!(ring.bytes(), 3 * 4 * 4, "popped frames release their bytes");
    }

    #[test]
    fn the_frame_ring_is_bounded_by_count_and_by_bytes() {
        let mut ring = FrameHistory::new();
        for i in 0..(FrameHistory::MAX_FRAMES + 10) {
            ring.push(shown(i as f64, 4));
        }
        assert_eq!(ring.len(), FrameHistory::MAX_FRAMES);

        // A 4K frame is 33 MB; the byte budget must win over the count.
        let pixels = FrameHistory::MAX_BYTES / 4 / std::mem::size_of::<egui::Color32>() + 1;
        let mut ring = FrameHistory::new();
        for i in 0..8 {
            ring.push(shown(i as f64, pixels));
        }
        assert!(ring.bytes() <= FrameHistory::MAX_BYTES);
        assert!(
            !ring.is_empty(),
            "a single step back must stay possible even for huge frames"
        );
    }

    #[test]
    fn clearing_the_frame_ring_releases_everything() {
        let mut ring = FrameHistory::new();
        for i in 0..4 {
            ring.push(shown(i as f64, 16));
        }
        ring.clear();
        assert!(ring.is_empty());
        assert_eq!(ring.bytes(), 0);
        assert!(ring.pop().is_none());
    }

    #[test]
    fn toast_fades_and_expires() {
        let mut toast = Toast::info("hello");
        toast.hold = Duration::from_millis(0);
        toast.fade = Duration::from_millis(30);
        assert!(toast.opacity() <= 1.0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(toast.opacity(), 0.0);
        assert!(toast.is_finished());
    }

    #[test]
    fn controls_visibility_is_time_boxed() {
        let mut ui = UiState {
            controls_until: Instant::now() - Duration::from_millis(1),
            ..UiState::default()
        };
        assert!(!ui.controls_visible());
        ui.wake_controls(5.0);
        assert!(ui.controls_visible());
    }

    #[test]
    fn overlays_are_exclusive() {
        let mut ui = UiState::default();
        assert!(!ui.has_overlay());
        ui.open_overlay(Overlay::Settings);
        assert!(ui.has_overlay());
        ui.open_overlay(Overlay::About);
        assert_eq!(ui.overlay, Overlay::About);
        ui.close_overlay();
        assert_eq!(ui.overlay, Overlay::None);
    }

    #[test]
    fn settings_tabs_are_complete() {
        assert_eq!(SettingsTab::all().len(), 7);
        for tab in SettingsTab::all() {
            assert!(!tab.label().is_empty());
        }
    }

    #[test]
    fn state_labels_cover_every_variant() {
        for state in [
            PlaybackState::Idle,
            PlaybackState::Opening,
            PlaybackState::Playing,
            PlaybackState::Paused,
            PlaybackState::Ended,
            PlaybackState::Error("x".into()),
        ] {
            assert!(!state_label(&state).is_empty());
        }
    }
}
