//! Persisted user settings.
//!
//! Everything lives in a single JSON document under
//! `%APPDATA%\MVP-Versatile-Player\settings.json`. A JSON file (rather than the
//! registry) keeps the settings inspectable, trivially resettable by deleting one
//! file, and portable between machines.
//!
//! The file is only written when something actually changed, and writes go
//! through a temporary file + rename so a crash mid-save cannot corrupt the
//! user's configuration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mvp_core::playlist::{PlaylistItem, RepeatMode};
use mvp_platform::assoc::FileKinds;
use serde::{Deserialize, Serialize};

/// Product name, used for the settings folder and the registry ProgID.
pub const APP_NAME: &str = "MVP-Versatile-Player";

/// Identifier used for the single-instance mutex and the AppUserModelID.
pub const APP_ID: &str = "mvp-versatile-player";

/// How the video is stretched into the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AspectMode {
    /// Keep the source ratio, letterbox the rest.
    #[default]
    Fit,
    /// Keep the ratio, crop to fill the viewport.
    Crop,
    /// Stretch to the viewport, ignoring the ratio.
    Stretch,
    /// Always 16:9.
    Ratio16x9,
    /// Always 4:3.
    Ratio4x3,
    /// Deprecated: was meant to force the *coded* ratio even when the container
    /// declares something else, but the prober exposes no separate display
    /// aspect ratio, so it behaved exactly like [`AspectMode::Fit`]. It is kept
    /// only so an existing `settings.json` still parses; [`Settings::load`]
    /// migrates it to `Fit` and it no longer appears in any menu.
    Source,
}

impl AspectMode {
    /// Label for the video menu.
    pub fn label(self) -> &'static str {
        match self {
            AspectMode::Fit => "适应窗口",
            AspectMode::Crop => "裁剪填充",
            AspectMode::Stretch => "拉伸填充",
            AspectMode::Ratio16x9 => "16:9",
            AspectMode::Ratio4x3 => "4:3",
            AspectMode::Source => "原始比例",
        }
    }

    /// All modes, in menu order.
    ///
    /// [`AspectMode::Source`] is deliberately absent: it was indistinguishable
    /// from `Fit` and offering two menu entries that do the same thing is worse
    /// than offering one.
    pub fn all() -> &'static [AspectMode] {
        &[
            AspectMode::Fit,
            AspectMode::Crop,
            AspectMode::Stretch,
            AspectMode::Ratio16x9,
            AspectMode::Ratio4x3,
        ]
    }

    /// Ratio override, or `None` to derive it from the frame.
    pub fn forced_ratio(self) -> Option<f32> {
        match self {
            AspectMode::Ratio16x9 => Some(16.0 / 9.0),
            AspectMode::Ratio4x3 => Some(4.0 / 3.0),
            _ => None,
        }
    }
}

/// Which panel the sidebar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SidebarTab {
    /// The playlist.
    #[default]
    Playlist,
    /// Audio and subtitle track pickers.
    Tracks,
    /// Chapters.
    Chapters,
    /// Technical details of the current file.
    Info,
}

impl SidebarTab {
    /// Label for the tab strip.
    pub fn label(self) -> &'static str {
        match self {
            SidebarTab::Playlist => "播放列表",
            SidebarTab::Tracks => "轨道",
            SidebarTab::Chapters => "章节",
            SidebarTab::Info => "信息",
        }
    }

    /// Every tab, in display order.
    pub fn all() -> &'static [SidebarTab] {
        &[
            SidebarTab::Playlist,
            SidebarTab::Tracks,
            SidebarTab::Chapters,
            SidebarTab::Info,
        ]
    }
}

/// What to do when the current file ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EndAction {
    /// Follow the playlist's repeat mode.
    #[default]
    Playlist,
    /// Close the player.
    Close,
    /// Do nothing, stay on the last frame.
    Hold,
}

impl EndAction {
    /// The label shown for this action, in the menu and in the settings page.
    pub fn label(self) -> &'static str {
        match self {
            EndAction::Playlist => "按播放列表继续",
            EndAction::Hold => "停留在最后一帧",
            EndAction::Close => "关闭播放器",
        }
    }

    /// Every choice, in the order the interfaces offer them.
    ///
    /// One list rather than two copies: the tools menu used to spell the three
    /// labels out again inside a submenu of its own, which is how the menu and the
    /// settings page could quietly drift apart.
    pub fn choices() -> [(EndAction, &'static str); 3] {
        [EndAction::Playlist, EndAction::Hold, EndAction::Close].map(|action| (action, action.label()))
    }
}

/// Everything the player remembers between runs.
///
/// Keys that no longer exist are ignored rather than rejected, which is how a
/// retired preference — `autoplay`, which used to leave the player unable to
/// play a double-clicked file — is dropped instead of breaking the document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // ---- audio -----------------------------------------------------------
    /// Linear volume, `0.0..=2.0`.
    pub volume: f32,
    /// Mute flag.
    pub muted: bool,
    /// Output device name; `None` uses the system default.
    pub audio_device: Option<String>,
    /// Audio delay in seconds relative to the video.
    pub audio_delay: f64,

    // ---- playback --------------------------------------------------------
    /// Playback rate.
    pub speed: f64,
    /// Repeat mode.
    pub repeat: RepeatMode,
    /// Shuffle the playlist.
    pub shuffle: bool,
    /// Resume the previous position when reopening a file.
    pub remember_position: bool,
    /// Only resume when at least this much of the file was watched.
    pub resume_min_seconds: f64,
    /// What to do at the end of the last file.
    pub end_action: EndAction,

    // ---- video -----------------------------------------------------------
    /// Try hardware decoding.
    pub hardware_decoding: bool,
    /// Bring HDR (PQ / HLG) and Dolby Vision frames into the range an SDR
    /// display can show.
    pub hdr_tone_map: bool,
    /// How the frame is fitted to the window.
    pub aspect: AspectMode,
    /// Extra rotation in degrees (0/90/180/270).
    pub rotation: i32,
    /// Mirror horizontally.
    pub flip_h: bool,
    /// Mirror vertically.
    pub flip_v: bool,
    /// Draw a dimming gradient behind the controls so they read over video.
    pub control_scrim: bool,
    /// Show the bird's-eye view in the corner of the canvas while the picture is
    /// zoomed in past the edges of the canvas.
    pub minimap: bool,
    /// Auto-hide the controls in fullscreen after this many seconds (0 = never).
    pub hide_controls_after: f32,

    // ---- subtitles -------------------------------------------------------
    /// Show subtitles when the file has them.
    pub subtitles_enabled: bool,
    /// Font size in points, relative to the window height.
    pub subtitle_size: f32,
    /// Text colour.
    pub subtitle_color: [u8; 3],
    /// Draw a shadow/outline behind the text.
    pub subtitle_outline: bool,
    /// Distance from the bottom of the video, in points.
    pub subtitle_margin: f32,
    /// Subtitle delay in seconds.
    pub subtitle_delay: f64,
    /// Prefer an external side-car file over embedded subtitles.
    pub prefer_external_subtitles: bool,
    /// Automatically load `movie.srt` next to `movie.mkv`.
    pub autoload_sidecar_subtitles: bool,
    /// Directory of the last side-car subtitle.
    pub last_subtitle_dir: Option<PathBuf>,

    // ---- images ----------------------------------------------------------
    /// Seconds each slide is shown in the slideshow.
    pub slideshow_interval: f32,
    /// The slideshow is running.
    pub slideshow_active: bool,
    /// Animated images loop.
    pub animate_images: bool,

    // ---- shell integration ----------------------------------------------
    /// Media families to register with Explorer.
    pub file_kinds: FileKinds,
    /// Also add "open with MVP" to the right-click menu.
    pub context_menu: bool,
    /// Also try to make MVP the *default* handler on register.
    pub set_as_default: bool,

    // ---- window ----------------------------------------------------------
    /// Window size in points.
    pub window_size: [f32; 2],
    /// Window position, when the user moved it.
    pub window_pos: Option<[f32; 2]>,
    /// Maximised on start.
    pub maximized: bool,
    /// Fullscreen on start.
    pub start_fullscreen: bool,
    /// Keep the window above others.
    pub always_on_top: bool,
    /// Tint the native title bar to match the theme.
    pub dark_title_bar: bool,
    /// Sidebar visibility.
    pub sidebar_visible: bool,
    /// Active sidebar tab.
    pub sidebar_tab: SidebarTab,
    /// Show the performance counters in the sidebar.
    pub show_statistics: bool,

    // ---- history ---------------------------------------------------------
    /// Recently opened files, newest first.
    pub recent_files: Vec<PathBuf>,
    /// How many recent files to keep.
    pub recent_limit: usize,
    /// Last directory used by a file dialog.
    pub last_dir: Option<PathBuf>,
    /// Where snapshots are written.
    pub snapshot_dir: Option<PathBuf>,
    /// Saved positions, keyed by path, for resume-where-you-left-off.
    pub resume_positions: HashMap<String, f64>,
    /// The playlist is restored on start.
    pub restore_playlist: bool,
    /// The playlist as it was when the player closed.
    pub playlist: Vec<PlaylistItem>,
    /// Index that was playing.
    pub playlist_index: Option<usize>,

    // ---- shortcuts -------------------------------------------------------
    /// Small seek step in seconds.
    pub seek_step: f64,
    /// Large seek step in seconds.
    pub seek_step_large: f64,
    /// Mouse wheel adjusts the volume instead of seeking.
    pub wheel_controls_volume: bool,
    /// Double-clicking the video toggles fullscreen.
    pub double_click_fullscreen: bool,

    /// Schema version, so a future migration can detect old files.
    pub version: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
            audio_device: None,
            audio_delay: 0.0,

            speed: 1.0,
            repeat: RepeatMode::Off,
            shuffle: false,
            remember_position: true,
            resume_min_seconds: 15.0,
            end_action: EndAction::Playlist,

            hardware_decoding: true,
            hdr_tone_map: true,
            aspect: AspectMode::Fit,
            rotation: 0,
            flip_h: false,
            flip_v: false,
            control_scrim: true,
            minimap: true,
            hide_controls_after: 3.0,

            subtitles_enabled: true,
            subtitle_size: 22.0,
            subtitle_color: [255, 255, 255],
            subtitle_outline: true,
            subtitle_margin: 28.0,
            subtitle_delay: 0.0,
            prefer_external_subtitles: true,
            autoload_sidecar_subtitles: true,
            last_subtitle_dir: None,

            slideshow_interval: 5.0,
            slideshow_active: false,
            animate_images: true,

            file_kinds: FileKinds::default(),
            context_menu: true,
            set_as_default: false,

            window_size: [1280.0, 780.0],
            window_pos: None,
            maximized: false,
            start_fullscreen: false,
            always_on_top: false,
            dark_title_bar: true,
            sidebar_visible: true,
            sidebar_tab: SidebarTab::Playlist,
            show_statistics: false,

            recent_files: Vec::new(),
            recent_limit: 15,
            last_dir: None,
            snapshot_dir: None,
            resume_positions: HashMap::new(),
            restore_playlist: true,
            playlist: Vec::new(),
            playlist_index: None,

            seek_step: 5.0,
            seek_step_large: 30.0,
            wheel_controls_volume: true,
            double_click_fullscreen: true,

            version: SCHEMA_VERSION,
        }
    }
}

/// Bumped whenever the on-disk shape changes in a way that needs migration.
pub const SCHEMA_VERSION: u32 = 1;

impl Settings {
    /// Directory holding the settings, cache and logs.
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(APP_NAME)
    }

    /// Absolute path of the settings file.
    pub fn path() -> PathBuf {
        Self::config_dir().join("settings.json")
    }

    /// Where snapshots go by default: `Pictures\MVP Snapshots`.
    pub fn default_snapshot_dir() -> PathBuf {
        dirs::picture_dir()
            .unwrap_or_else(Self::config_dir)
            .join("MVP Snapshots")
    }

    /// Effective snapshot directory.
    pub fn snapshot_dir(&self) -> PathBuf {
        self.snapshot_dir
            .clone()
            .unwrap_or_else(Self::default_snapshot_dir)
    }

    /// Load from disk, falling back to defaults for anything missing or broken.
    ///
    /// A corrupt settings file is renamed rather than deleted, so the user can
    /// still recover it, and the player always starts.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(mut settings) => {
                settings.migrate();
                settings
            }
            Err(err) => {
                log::warn!("设置文件损坏，已重置: {err}");
                let backup = path.with_extension("json.broken");
                let _ = std::fs::rename(&path, backup);
                Self::default()
            }
        }
    }

    /// Bring a document written by an older build up to the current shape.
    ///
    /// Values that no longer exist are folded into their replacement instead of
    /// being dropped, because dropping a variant makes serde reject the whole
    /// file and silently reset every other setting with it.
    fn migrate(&mut self) {
        if self.aspect == AspectMode::Source {
            self.aspect = AspectMode::Fit;
        }
    }

    /// Write to disk atomically.
    pub fn save(&self) -> std::io::Result<()> {
        let dir = Self::config_dir();
        std::fs::create_dir_all(&dir)?;
        let path = Self::path();
        let temp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&temp, text)?;
        // `rename` replaces the destination on Windows only when it does not
        // already exist, so remove it first; the window where neither file
        // exists is a few microseconds and the defaults are always usable.
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&temp, &path)
    }

    /// Delete the settings file and return fresh defaults.
    pub fn reset() -> Self {
        let path = Self::path();
        let _ = std::fs::remove_file(&path);
        Self::default()
    }

    /// Record `path` at the top of the recent list.
    pub fn push_recent(&mut self, path: &Path) {
        if mvp_core::util::is_url(&path.to_string_lossy()) {
            return;
        }
        self.recent_files.retain(|p| p != path);
        self.recent_files.insert(0, path.to_path_buf());
        self.recent_files.truncate(self.recent_limit.max(1));
    }

    /// Drop recent entries whose file no longer exists.
    pub fn prune_recent(&mut self) {
        self.recent_files.retain(|p| p.exists());
    }

    /// Remember where the user stopped watching `key`.
    pub fn store_position(&mut self, key: &str, seconds: f64) {
        if !self.remember_position {
            return;
        }
        if seconds < self.resume_min_seconds {
            self.resume_positions.remove(key);
        } else {
            self.resume_positions.insert(key.to_string(), seconds);
        }
        // Bound the map so it cannot grow forever.
        if self.resume_positions.len() > 500 {
            let keys: Vec<String> = self.resume_positions.keys().take(100).cloned().collect();
            for key in keys {
                self.resume_positions.remove(&key);
            }
        }
    }

    /// Position to resume `key` from, if it is worth resuming.
    pub fn resume_position(&self, key: &str, duration: f64) -> Option<f64> {
        if !self.remember_position {
            return None;
        }
        let saved = *self.resume_positions.get(key)?;
        if saved < self.resume_min_seconds {
            return None;
        }
        // Do not resume into the credits.
        if duration > 0.0 && saved > duration - self.resume_min_seconds.max(10.0) {
            return None;
        }
        Some(saved)
    }

    /// Forget the stored position for `key`.
    pub fn clear_position(&mut self, key: &str) {
        self.resume_positions.remove(key);
    }

    /// Fold this launch's command-line overrides into the live document.
    ///
    /// The overrides have to reach the document: the transport bar shows
    /// `volume`, the speed menu shows `speed`, and `sync_engine` pushes both at
    /// the engine every frame, so a value that only lived in the engine would be
    /// overwritten by the stored one a frame later. [`Settings::persisted`] is
    /// what keeps them out of the file.
    pub fn apply_launch_overrides(&mut self, overrides: &LaunchOverrides) {
        if let Some(volume) = overrides.volume {
            self.volume = volume;
        }
        if let Some(speed) = overrides.speed {
            self.speed = speed;
        }
    }

    /// The document that should be written to `settings.json`.
    ///
    /// `--volume` and `--speed` describe one launch; writing them back would
    /// turn a single `--volume 10` into a player that is quiet for good. An
    /// override the user never touched during the session is therefore replaced
    /// by the value that was already on disk, while one they moved by hand —
    /// with the slider, the wheel, the speed menu — is a real change and is
    /// kept.
    pub fn persisted(&self, overrides: &LaunchOverrides, baseline: &Settings) -> Settings {
        let mut disk = self.clone();
        if let Some(volume) = overrides.volume {
            if (disk.volume - volume).abs() < 1e-6 {
                disk.volume = baseline.volume;
            }
        }
        if let Some(speed) = overrides.speed {
            if (disk.speed - speed).abs() < 1e-9 {
                disk.speed = baseline.speed;
            }
        }
        disk
    }
}

/// Command-line values that describe one launch rather than a preference.
///
/// `--volume 30 --speed 2.0 movie.mkv` says what *this* run should start with.
/// None of it may survive the session — the same rule that keeps `--no-autoplay`
/// (see [`crate::app::StartupArgs::autoplay_command_line_files`]) and `-f` from
/// rewriting the user's saved configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LaunchOverrides {
    /// `--volume`, normalised to `0.0..=2.0`.
    pub volume: Option<f32>,
    /// `--speed`, in `0.25..=4.0`.
    pub speed: Option<f64>,
}

/// Debounced settings writer.
///
/// Dragging the volume slider changes the value sixty times a second; writing
/// the file each time would hammer the disk. This coalesces changes and flushes
/// at most once every `FLUSH_INTERVAL`.
#[derive(Debug)]
pub struct SettingsStore {
    dirty: bool,
    last_flush: Instant,
    interval: Duration,
}

impl Default for SettingsStore {
    fn default() -> Self {
        Self {
            dirty: false,
            last_flush: Instant::now(),
            interval: Duration::from_secs(2),
        }
    }
}

impl SettingsStore {
    /// Mark the settings as needing a write.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// `true` when there are unsaved changes.
    ///
    /// The interface asks before it builds the document that goes to disk, so a
    /// frame that changed nothing pays nothing for the [launch
    /// overrides](Settings::persisted) that have to be folded back out.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Write if dirty and enough time has passed. Returns `true` when a write
    /// happened.
    pub fn flush_if_due(&mut self, settings: &Settings) -> bool {
        if !self.dirty || self.last_flush.elapsed() < self.interval {
            return false;
        }
        self.flush(settings);
        true
    }

    /// Write immediately, regardless of the timer.
    pub fn flush(&mut self, settings: &Settings) {
        if !self.dirty {
            return;
        }
        match settings.save() {
            Ok(()) => {
                self.dirty = false;
                self.last_flush = Instant::now();
            }
            Err(err) => log::warn!("无法保存设置: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The choices the menu and the settings page offer must be one list.
    ///
    /// Both used to spell the labels out themselves, which is how a setting ends
    /// up described one way in the menu and another way in the settings window —
    /// and how the menu came to hide them behind a submenu that never opened.
    #[test]
    fn the_end_action_choices_cover_every_variant() {
        use std::collections::HashSet;

        let choices = EndAction::choices();
        assert_eq!(choices.len(), 3, "there are exactly three end actions");

        let mut labels: HashSet<&str> = HashSet::new();
        let mut actions: Vec<EndAction> = Vec::new();
        for (action, label) in choices {
            assert!(!label.trim().is_empty(), "{action:?} has no label");
            assert!(labels.insert(label), "two choices share the label {label:?}");
            assert!(!actions.contains(&action), "{action:?} is offered twice");
            actions.push(action);
        }
        for action in [EndAction::Playlist, EndAction::Hold, EndAction::Close] {
            assert!(actions.contains(&action), "{action:?} is missing");
            assert_eq!(
                Some(action.label()),
                choices
                    .iter()
                    .find(|(candidate, _)| *candidate == action)
                    .map(|(_, label)| *label),
                "the label of {action:?} disagrees with the list"
            );
        }
    }

    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert_eq!(s.volume, 1.0);
        assert_eq!(s.speed, 1.0);
        assert!(s.hardware_decoding);
        assert!(s.subtitles_enabled);
        assert_eq!(s.recent_limit, 15);
        assert!(s.file_kinds.video && s.file_kinds.audio && s.file_kinds.image);
    }

    #[test]
    fn settings_round_trip_through_json() {
        let mut s = Settings {
            volume: 0.42,
            repeat: RepeatMode::All,
            aspect: AspectMode::Crop,
            subtitle_color: [10, 20, 30],
            ..Settings::default()
        };
        s.recent_files.push(PathBuf::from("C:/movie.mkv"));
        let text = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.volume, 0.42);
        assert_eq!(back.repeat, RepeatMode::All);
        assert_eq!(back.aspect, AspectMode::Crop);
        assert_eq!(back.subtitle_color, [10, 20, 30]);
        assert_eq!(back.recent_files.len(), 1);
    }

    #[test]
    fn unknown_and_missing_fields_fall_back_to_defaults() {
        // A file written by an older version must still load.
        let json = r#"{"volume":0.5}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.volume, 0.5);
        assert_eq!(s.repeat, RepeatMode::Off, "missing fields use defaults");
    }

    #[test]
    fn recent_files_deduplicate_and_bound() {
        let mut s = Settings {
            recent_limit: 3,
            ..Settings::default()
        };
        for i in 0..5 {
            s.push_recent(Path::new(&format!("C:/f{i}.mkv")));
        }
        assert_eq!(s.recent_files.len(), 3);
        assert_eq!(s.recent_files[0], PathBuf::from("C:/f4.mkv"));
        // Re-adding an existing entry moves it to the front without growing.
        s.push_recent(Path::new("C:/f2.mkv"));
        assert_eq!(s.recent_files.len(), 3);
        assert_eq!(s.recent_files[0], PathBuf::from("C:/f2.mkv"));
    }

    #[test]
    fn urls_are_not_kept_in_the_recent_list() {
        let mut s = Settings::default();
        s.push_recent(Path::new("https://example.com/a.m3u8"));
        assert!(s.recent_files.is_empty());
    }

    #[test]
    fn resume_positions_respect_the_thresholds() {
        let mut s = Settings {
            resume_min_seconds: 15.0,
            ..Settings::default()
        };
        s.store_position("movie", 5.0);
        assert_eq!(s.resume_position("movie", 3600.0), None, "too early to resume");
        s.store_position("movie", 120.0);
        assert_eq!(s.resume_position("movie", 3600.0), Some(120.0));
        // Near the end we start over instead of resuming into the credits.
        assert_eq!(s.resume_position("movie", 130.0), None);
        s.clear_position("movie");
        assert_eq!(s.resume_position("movie", 3600.0), None);
    }

    #[test]
    fn resume_is_disabled_when_the_user_asked_for_it() {
        let mut s = Settings {
            remember_position: false,
            ..Settings::default()
        };
        s.store_position("movie", 600.0);
        assert!(s.resume_positions.is_empty());
    }

    #[test]
    fn aspect_modes_map_to_ratios() {
        assert_eq!(AspectMode::Fit.forced_ratio(), None);
        assert!((AspectMode::Ratio16x9.forced_ratio().unwrap() - 16.0 / 9.0).abs() < 1e-6);
        assert_eq!(AspectMode::all().len(), 5);
        assert_eq!(SidebarTab::all().len(), 4);
    }

    /// Two menu entries that do the same thing are worse than one: `Source`
    /// produced exactly the same rectangle as `Fit`, so it is no longer offered.
    #[test]
    fn the_duplicate_source_aspect_is_not_offered() {
        assert!(!AspectMode::all().contains(&AspectMode::Source));
        // An old settings file that still names it must load, not reset.
        let json = r#"{"aspect":"Source","volume":0.5}"#;
        let mut s: Settings = serde_json::from_str(json).unwrap();
        s.migrate();
        assert_eq!(s.aspect, AspectMode::Fit);
        assert_eq!(s.volume, 0.5, "the rest of the file survives the migration");
    }

    #[test]
    fn settings_store_only_writes_when_dirty() {
        let mut store = SettingsStore::default();
        let s = Settings::default();
        assert!(!store.flush_if_due(&s), "nothing to do when clean");
        store.mark_dirty();
        assert!(store.is_dirty());
        // The interval has not elapsed, so nothing is written yet. (Writing
        // here would touch the user's real settings file, which is why the test
        // stops at the decision and not at the write.)
        assert!(!store.flush_if_due(&s));
    }

    /// `--volume` and `--speed` are start-up values, not preferences.
    ///
    /// The bug this guards: they used to be folded straight into the document
    /// that is written back, so one launch with `--volume 10` left the player
    /// quiet for good — the same trap that let a one-off `--no-autoplay` turn
    /// "double-click a video and watch it" into "double-click a video and press
    /// play" for every later launch.
    #[test]
    fn launch_overrides_do_not_become_preferences() {
        let baseline = Settings {
            volume: 0.8,
            speed: 1.0,
            ..Settings::default()
        };
        let overrides = LaunchOverrides {
            volume: Some(0.1),
            speed: Some(2.0),
        };
        let mut live = baseline.clone();
        live.apply_launch_overrides(&overrides);
        assert_eq!(live.volume, 0.1, "the session must start at the given volume");
        assert_eq!(live.speed, 2.0);

        let disk = live.persisted(&overrides, &baseline);
        assert_eq!(
            disk.volume, baseline.volume,
            "an untouched --volume must not be written back"
        );
        assert_eq!(
            disk.speed, baseline.speed,
            "an untouched --speed must not be written back"
        );

        // A value the user moved during the session is theirs to keep: the
        // override explains where it started, not what it has to end at.
        live.volume = 0.5;
        let disk = live.persisted(&overrides, &baseline);
        assert_eq!(disk.volume, 0.5);
        assert_eq!(
            disk.speed, baseline.speed,
            "the other override is still just a start-up value"
        );
    }

    /// A settings file written by the version that had the switch still loads.
    ///
    /// The preference is gone rather than honoured: it is what made a
    /// double-clicked file wait for a play button, and the only way to ask for
    /// a paused start is `--no-autoplay`, which lasts exactly one launch.
    #[test]
    fn a_retired_autoplay_preference_is_dropped() {
        let json = r#"{"autoplay":false,"volume":0.5}"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.volume, 0.5, "the rest of the file survives");
        let saved = serde_json::to_string(&settings).unwrap();
        assert!(
            !saved.contains("autoplay"),
            "the retired key must not come back: {saved}"
        );
    }
}
