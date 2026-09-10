//! Transient interface state — everything that is *not* persisted and not part
//! of the media engine.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mvp_core::playlist::RepeatMode;
use mvp_core::PlaybackState;

use crate::icons::Icon;
use crate::settings::SidebarTab;

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
    /// Index being renamed inline.
    pub renaming: Option<usize>,

    /// Currently open overlay.
    pub overlay: Overlay,
    /// Active settings page.
    pub settings_tab: SettingsTab,
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
    /// The window should be closed at the end of this frame.
    pub close_requested: bool,

    /// Milliseconds from process start to the first painted frame.
    pub startup_ms: f32,
    /// Cached FFmpeg version banner.
    pub ffmpeg_version: String,
    /// Cached FFmpeg build configuration.
    pub ffmpeg_config: String,
    /// Cached snapshot-saved message.
    pub last_snapshot: Option<PathBuf>,
    /// Set while the user is scrubbing with the mouse over the video.
    pub video_hover_time: Option<f64>,
}

impl Default for UiState {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            sidebar_visible: true,
            sidebar_tab: SidebarTab::Playlist,
            info_rows: Vec::new(),
            seek_drag: None,
            renaming: None,
            overlay: Overlay::None,
            settings_tab: SettingsTab::default(),
            url_input: String::new(),
            url_focus: false,
            toast: None,
            error_banner: None,
            controls_until: now,
            last_pointer_move: now,
            last_pointer_pos: None,
            fullscreen: false,
            close_requested: false,
            startup_ms: 0.0,
            ffmpeg_version: String::new(),
            ffmpeg_config: String::new(),
            last_snapshot: None,
            video_hover_time: None,
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
