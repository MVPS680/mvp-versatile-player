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
    /// The current file's subtitle track shown as scrolling lyrics.
    ///
    /// Only offered by the audio screen: on a film the same cues belong on the
    /// picture, where they already are.
    Lyrics,
    /// A photograph's parameters: size, format, colour, EXIF orientation.
    Exif,
}

impl SidebarTab {
    /// Label for the tab strip.
    pub fn label(self) -> &'static str {
        match self {
            SidebarTab::Playlist => "播放列表",
            SidebarTab::Tracks => "轨道",
            SidebarTab::Chapters => "章节",
            SidebarTab::Info => "信息",
            SidebarTab::Lyrics => "歌词",
            SidebarTab::Exif => "图片信息",
        }
    }

    /// Every tab, in display order. Kept for the settings document and tests;
    /// each screen narrows this to the pages that mean something to it.
    pub fn all() -> &'static [SidebarTab] {
        &[
            SidebarTab::Playlist,
            SidebarTab::Tracks,
            SidebarTab::Chapters,
            SidebarTab::Info,
            SidebarTab::Lyrics,
            SidebarTab::Exif,
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

/// How the picture adjustments are remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RememberScope {
    /// One set of sliders for every file.
    #[default]
    Global,
    /// A set per file, falling back to the globals for a file nobody has tuned.
    PerFile,
}

/// The seven sliders of the picture adjustment panel.
///
/// Every field's default is the value that means "hands off", so the reset button is
/// `*self = Self::default()` and a settings file written before this feature existed
/// keeps showing the picture exactly as it did.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PictureSettings {
    /// Overall lightness. `0.0` is untouched.
    pub brightness: f32,
    /// Distance from mid grey. `0.0` is untouched.
    pub contrast: f32,
    /// Colour strength. `0.0` is untouched.
    pub saturation: f32,
    /// Blue ↔ orange white balance. `0.0` is untouched.
    pub temperature: f32,
    /// Green ↔ magenta. `0.0` is untouched.
    pub tint: f32,
    /// Edge enhancement. `0.0` is off.
    pub sharpness: f32,
    /// Mid-tone curve. `1.0` is untouched.
    pub gamma: f32,
}

impl Default for PictureSettings {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            temperature: 0.0,
            tint: 0.0,
            sharpness: 0.0,
            gamma: 1.0,
        }
    }
}

impl PictureSettings {
    /// `true` when every slider is where it started.
    ///
    /// This is the zero-channel test: a neutral picture with the enhancement off
    /// means the renderer takes the path it took before this feature existed, so
    /// "turning it off changes nothing on screen" is a property of the code rather
    /// than of the numbers happening to look alike.
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }

    /// Put every slider back. This is the reset button, and nothing else.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Pull every slider into the range the shader assumes.
    ///
    /// `settings.json` is a text file people edit by hand, and a wild value there
    /// would reach the shader as a uniform: a `NaN` gamma is a black picture, and a
    /// brightness of 1e9 is a white one.
    pub fn clamp(&mut self) {
        let unit = |v: f32| v.clamp(-1.0, 1.0);
        self.brightness = unit(self.brightness);
        self.contrast = unit(self.contrast);
        self.saturation = unit(self.saturation);
        self.temperature = unit(self.temperature);
        self.tint = unit(self.tint);
        self.sharpness = self.sharpness.clamp(0.0, 2.0);
        self.gamma = self.gamma.clamp(0.5, 2.5);
        if !self.gamma.is_finite() {
            self.gamma = 1.0;
        }
    }

    /// `true` when every field is a real number — the guard before anything reaches
    /// a uniform.
    pub fn is_finite(&self) -> bool {
        [
            self.brightness,
            self.contrast,
            self.saturation,
            self.temperature,
            self.tint,
            self.sharpness,
            self.gamma,
        ]
        .iter()
        .all(|v| v.is_finite())
    }
}

/// Real-time picture enhancement: one switch, one strength, and what it may do.
///
/// Off by default. A player that quietly changes the picture the first time it is
/// started is a player whose output nobody can judge, and this is the feature whose
/// whole job is to change the picture.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnhanceSettings {
    /// The master switch. Off means the enhancement is not applied at all.
    pub enabled: bool,
    /// How far each sub-effect is allowed to go, `0.0..=1.0`.
    pub strength: f32,
    /// Automatic black/white point and shadow lift, from the frame's own histogram.
    pub auto_levels: bool,
    /// Pull the colour up when the frame is undersaturated.
    pub auto_colour: bool,
    /// Edge-aware smoothing, `0.0..=1.0`. Light by design — see the module docs.
    pub denoise: f32,
    /// Block-edge smoothing, `0.0..=1.0`.
    pub deblock: f32,
    /// Adaptive sharpening of edges that survive the smoothing, `0.0..=1.0`.
    pub sharpening: f32,
}

impl Default for EnhanceSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            strength: 0.6,
            auto_levels: true,
            auto_colour: true,
            denoise: 0.0,
            deblock: 0.0,
            sharpening: 0.3,
        }
    }
}

impl EnhanceSettings {
    /// Pull the strength sliders into range and drop the ones that are not numbers.
    pub fn clamp(&mut self) {
        self.strength = self.strength.clamp(0.0, 1.0);
        self.denoise = self.denoise.clamp(0.0, 1.0);
        self.deblock = self.deblock.clamp(0.0, 1.0);
        self.sharpening = self.sharpening.clamp(0.0, 1.0);
    }
}

/// What is painted behind a still image.
///
/// A photograph that does not fill the window sits on *something*, and what that
/// something is changes how the picture reads: a dark surround for normal
/// viewing, pure black for judging contrast, a light surround for print, and the
/// checkerboard when the file has transparency to reveal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ImageBackground {
    /// The window background — the quiet default.
    #[default]
    Dark,
    /// Pure black, for judging contrast the way a grading suite does.
    Black,
    /// Light grey, the surround a print is judged against.
    Light,
    /// A transparency checkerboard.
    Checkerboard,
}

impl ImageBackground {
    /// Label for the toolbar menu.
    pub fn label(self) -> &'static str {
        match self {
            ImageBackground::Dark => "深色",
            ImageBackground::Black => "纯黑",
            ImageBackground::Light => "浅色",
            ImageBackground::Checkerboard => "棋盘格",
        }
    }

    /// Every choice, in menu order.
    pub fn all() -> &'static [ImageBackground] {
        &[
            ImageBackground::Dark,
            ImageBackground::Black,
            ImageBackground::Light,
            ImageBackground::Checkerboard,
        ]
    }

}

/// An RGB fill for the canvas, for everything but the checkerboard (which draws
/// itself) and the default (which leaves the panel's own background alone).
pub fn background_fill(background: ImageBackground) -> Option<[u8; 3]> {
    match background {
        ImageBackground::Dark => None,
        ImageBackground::Black => Some([0, 0, 0]),
        ImageBackground::Light => Some([214, 214, 216]),
        ImageBackground::Checkerboard => None,
    }
}

/// Everything the player remembers between runs.
///
/// Keys that no longer exist are ignored rather than rejected, which is how a
/// retired preference — `autoplay`, which used to leave the player unable to
/// play a double-clicked file — is dropped instead of breaking the document.
/// A named equaliser curve.
///
/// The bands themselves are fixed — the frequencies live in `mvp_core::dsp::EQ_FREQS`,
/// so the interface, the presets and the DSP cannot drift apart — and a preset is
/// nothing but ten gain values in dB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EqPreset {
    /// Everything at 0 dB.
    #[default]
    Flat,
    /// Bass and low mids up, treble slightly down: warmth for small speakers.
    Warm,
    /// Bass down, presence and air up.
    Bright,
    /// Mids and presence up: film dialogue and speech.
    Voice,
    /// A scoop in the low mids with bass and presence lifted.
    Rock,
    /// Sub-bass and bass up hardest, everything else untouched.
    Bass,
    /// Presence and air up, bass untouched.
    Treble,
    /// A gentle mid scoop with both ends lifted.
    Pop,
    /// The user's own gains. Selecting it changes nothing; moving any band
    /// switches the preset to it.
    Custom,
}

impl EqPreset {
    /// Every preset, in menu order.
    pub fn all() -> &'static [EqPreset] {
        &[
            EqPreset::Flat,
            EqPreset::Warm,
            EqPreset::Bright,
            EqPreset::Voice,
            EqPreset::Rock,
            EqPreset::Bass,
            EqPreset::Treble,
            EqPreset::Pop,
            EqPreset::Custom,
        ]
    }

    /// Label for the preset menu.
    pub fn label(self) -> &'static str {
        match self {
            EqPreset::Flat => "平直",
            EqPreset::Warm => "温暖",
            EqPreset::Bright => "明亮",
            EqPreset::Voice => "人声",
            EqPreset::Rock => "摇滚",
            EqPreset::Bass => "低音增强",
            EqPreset::Treble => "高音增强",
            EqPreset::Pop => "流行",
            EqPreset::Custom => "自定义",
        }
    }

    /// Band gains in dB, one per [`mvp_core::dsp::EQ_BANDS`] band.
    pub fn gains(self) -> [f32; mvp_core::dsp::EQ_BANDS] {
        match self {
            EqPreset::Flat | EqPreset::Custom => [0.0; mvp_core::dsp::EQ_BANDS],
            EqPreset::Warm => [3.0, 2.5, 1.5, 0.5, 0.0, -0.5, -1.0, -1.0, -0.5, 0.0],
            EqPreset::Bright => [-2.0, -1.5, -0.5, 0.0, 0.5, 1.0, 2.0, 3.0, 3.5, 3.0],
            EqPreset::Voice => [-3.0, -2.0, 0.0, 1.5, 2.5, 3.0, 3.0, 2.0, 1.0, 0.0],
            EqPreset::Rock => [3.0, 2.0, 0.0, -1.0, -1.5, 0.0, 2.0, 3.0, 3.0, 2.5],
            EqPreset::Bass => [6.0, 5.0, 3.5, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            EqPreset::Treble => [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.5, 4.0, 4.5, 4.0],
            EqPreset::Pop => [-1.0, 0.0, 1.5, 3.0, 2.0, 0.0, -0.5, -0.5, 1.0, 2.0],
        }
    }

    /// The preset a set of gains equals, or [`EqPreset::Custom`] when it matches none.
    pub fn of(gains: [f32; mvp_core::dsp::EQ_BANDS]) -> EqPreset {
        for preset in Self::all() {
            if *preset == EqPreset::Custom {
                continue;
            }
            let matches = preset
                .gains()
                .iter()
                .zip(gains.iter())
                .all(|(a, b)| (a - b).abs() < 0.05);
            if matches {
                return *preset;
            }
        }
        EqPreset::Custom
    }
}

/// Audio enhancement: the ten-band equaliser and the effects after it.
///
/// One flat document rather than a list of effects: the DSP chain is a fixed order
/// (see `mvp_core::dsp::enhance`), so the settings that configure it should be just
/// as fixed, and a stored file cannot ask for an order that does not exist.
///
/// The defaults are the *neutral* ones, twice over: [`Self::enabled`] is `false` and
/// every control is at its do-nothing position. Switching the feature on without
/// touching a control therefore sounds exactly like it did before the feature
/// existed, which is what the DSP's bypass guarantees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioEnhanceSettings {
    /// Master switch. Off means the chain is not in the pipeline at all.
    pub enabled: bool,
    /// The named curve the band gains came from.
    pub preset: EqPreset,
    /// Band gains in dB, `-12.0..=12.0`.
    pub bands: [f32; mvp_core::dsp::EQ_BANDS],
    /// Q of the peaking bands: how wide each one is.
    pub bandwidth: f32,
    /// Low-shelf bass in dB.
    pub bass_db: f32,
    /// Second-harmonic content for the bass, `0.0..=1.0`.
    pub bass_harmonics: f32,
    /// High-shelf clarity in dB.
    pub clarity_db: f32,
    /// Transient emphasis, `0.0..=1.0`.
    pub clarity_transient: f32,
    /// Normalisation target in dBFS; `-60.0` is "off".
    pub loudness_db: f32,
    /// How fast the normaliser follows, in seconds.
    pub loudness_speed: f32,
    /// True-peak ceiling in dBFS, `-6.0..=0.0`.
    pub limiter_ceiling_db: f32,
    /// Stereo width, `1.0` is untouched.
    pub width: f32,
    /// Cross-feed amount, `0.0..=1.0`.
    pub room: f32,
}

impl Default for AudioEnhanceSettings {
    fn default() -> Self {
        let snapshot = mvp_core::dsp::Snapshot::default();
        Self {
            enabled: false,
            preset: EqPreset::default(),
            bands: snapshot.eq_gain,
            bandwidth: snapshot.eq_q,
            bass_db: snapshot.bass_db,
            bass_harmonics: snapshot.bass_harmonics,
            clarity_db: snapshot.clarity_db,
            clarity_transient: snapshot.clarity_transient,
            loudness_db: snapshot.loudness_target_db,
            loudness_speed: snapshot.loudness_speed_s,
            limiter_ceiling_db: snapshot.limiter_ceiling_db,
            width: snapshot.spatial_width,
            room: snapshot.spatial_room,
        }
    }
}

impl AudioEnhanceSettings {
    /// The parameter set the DSP reads.
    ///
    /// The single translation point between the document and the engine: the
    /// interface never builds a `Snapshot` itself, and the stored shape is free to
    /// stay comfortable (a named preset, ten gains) while the DSP gets a flat,
    /// clamped block it can read from a real-time thread.
    pub fn snapshot(&self) -> mvp_core::dsp::Snapshot {
        let mut snapshot = mvp_core::dsp::Snapshot {
            bypass: !self.enabled,
            eq_gain: self.bands,
            eq_q: self.bandwidth,
            bass_db: self.bass_db,
            bass_harmonics: self.bass_harmonics,
            clarity_db: self.clarity_db,
            clarity_transient: self.clarity_transient,
            loudness_target_db: self.loudness_db,
            loudness_speed_s: self.loudness_speed,
            limiter_ceiling_db: self.limiter_ceiling_db,
            spatial_width: self.width,
            spatial_room: self.room,
        };
        // The file is editable by hand, so the clamp happens here rather than in the
        // callback: a wild number must become a legal filter, not a NaN.
        snapshot.clamp();
        snapshot
    }

    /// `true` when every control is at its neutral position.
    pub fn is_neutral(&self) -> bool {
        self.snapshot().is_idle()
    }

    /// Set one band. The preset follows the gains: moving a band away from a named
    /// curve switches the label to 自定义, and moving it back finds the curve again.
    pub fn set_band(&mut self, index: usize, gain: f32) {
        if let Some(slot) = self.bands.get_mut(index) {
            *slot = gain.clamp(-12.0, 12.0);
        }
        self.preset = EqPreset::of(self.bands);
    }

    /// Select a preset, replacing the band gains with its curve.
    ///
    /// [`EqPreset::Custom`] is deliberately a no-op on the gains: it is a label for
    /// what the user already set, not a curve of its own.
    pub fn apply_preset(&mut self, preset: EqPreset) {
        self.preset = preset;
        if preset != EqPreset::Custom {
            self.bands = preset.gains();
        }
    }

    /// Everything back to neutral, leaving the master switch where it is.
    pub fn reset(&mut self) {
        let enabled = self.enabled;
        *self = Self { enabled, ..Self::default() };
    }
}

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
    /// The equaliser and the effects after the volume stage.
    pub audio_enhance: AudioEnhanceSettings,

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

    // ---- picture adjustment ---------------------------------------------
    /// The picture adjustment sliders, as the global default.
    pub picture: PictureSettings,
    /// Whether those sliders are remembered globally or per file.
    pub picture_scope: RememberScope,
    /// Per-file overrides, most recently used first.
    ///
    /// Bounded on purpose and trimmed when written: a settings file that grows
    /// without limit is one that eventually fails to load, which is the same
    /// reasoning `recent_files` is capped with.
    pub per_file_picture: Vec<(String, PictureSettings)>,
    /// Real-time picture enhancement.
    pub enhance: EnhanceSettings,

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
    /// What is painted behind a still that does not fill the window.
    pub image_background: ImageBackground,

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
    /// Bake the picture adjustments into a saved snapshot.
    ///
    /// Off by default, and only ever about video: a snapshot has always been the
    /// frame the engine produced, and that stays the answer for anyone who has not
    /// asked for the other one. See [`crate::app::PlayerApp::snapshot_needs_render`].
    pub snapshot_includes_picture: bool,
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
            audio_enhance: AudioEnhanceSettings::default(),

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
            picture: PictureSettings::default(),
            picture_scope: RememberScope::default(),
            per_file_picture: Vec::new(),
            enhance: EnhanceSettings::default(),

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
            image_background: ImageBackground::default(),

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
            snapshot_includes_picture: false,
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

    /// How many per-file picture sets are kept before the oldest is dropped.
    pub const PER_FILE_PICTURE_LIMIT: usize = 200;

    /// The picture sliders that apply to `key` right now.
    ///
    /// `key` identifies one file and is the same key the resume position uses, so a
    /// file that is remembered for playback is remembered for its picture too. A
    /// file nobody has tuned falls back to the globals — which is what makes "per
    /// file" safe to switch on for a library that already exists.
    pub fn picture_for(&self, key: Option<&str>) -> PictureSettings {
        match (self.picture_scope, key) {
            (RememberScope::PerFile, Some(key)) => self
                .per_file_picture
                .iter()
                .find(|(stored, _)| stored == key)
                .map(|(_, picture)| *picture)
                .unwrap_or(self.picture),
            _ => self.picture,
        }
    }

    /// Store `picture` where the current scope says it belongs.
    ///
    /// Per-file entries move to the front on every write, so the cap evicts the
    /// file that has gone longest without being tuned rather than the one that has
    /// been in the library longest.
    pub fn set_picture_for(&mut self, key: Option<&str>, picture: PictureSettings) {
        if let (RememberScope::PerFile, Some(key)) = (self.picture_scope, key) {
            self.per_file_picture.retain(|(stored, _)| stored != key);
            self.per_file_picture.insert(0, (key.to_owned(), picture));
            self.per_file_picture.truncate(Self::PER_FILE_PICTURE_LIMIT);
            return;
        }
        self.picture = picture;
    }

    /// Forget every per-file picture, keeping the globals.
    pub fn clear_per_file_pictures(&mut self) {
        self.per_file_picture.clear();
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
        // The sidebar has grown past the playlist's original four pages: the
        // audio screen adds lyrics and the image screen adds picture details.
        assert_eq!(SidebarTab::all().len(), 6);
        for tab in SidebarTab::all() {
            assert!(!tab.label().is_empty());
        }
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

    #[test]
    fn the_audio_enhance_defaults_are_neutral_and_off() {
        let s = Settings::default();
        assert!(!s.audio_enhance.enabled, "the feature ships off");
        assert!(
            s.audio_enhance.is_neutral(),
            "every control starts at its do-nothing position"
        );
        assert_eq!(s.audio_enhance.preset, EqPreset::Flat);
        // And the snapshot says bypassed, which is what makes the default bit-exact:
        // switching the feature on and touching nothing must not change a sample.
        assert!(s.audio_enhance.snapshot().bypass);
    }

    #[test]
    fn moving_a_band_leaves_the_named_preset_behind() {
        let mut s = AudioEnhanceSettings::default();
        s.apply_preset(EqPreset::Rock);
        assert_eq!(s.preset, EqPreset::Rock);
        assert_eq!(s.bands, EqPreset::Rock.gains());

        s.set_band(4, EqPreset::Rock.gains()[4] + 3.0);
        assert_eq!(s.preset, EqPreset::Custom, "a hand-moved band is no longer 摇滚");

        // And putting it back finds the curve again.
        s.set_band(4, EqPreset::Rock.gains()[4]);
        assert_eq!(s.preset, EqPreset::Rock);
    }

    #[test]
    fn selecting_custom_keeps_the_gains_the_user_set() {
        let mut s = AudioEnhanceSettings::default();
        s.set_band(0, 4.0);
        let gains = s.bands;
        s.apply_preset(EqPreset::Custom);
        assert_eq!(s.bands, gains, "自定义 is a label, not a curve of its own");
    }

    #[test]
    fn the_snapshot_is_what_the_dsp_reads() {
        let mut s = AudioEnhanceSettings {
            enabled: true,
            bass_db: 5.0,
            loudness_db: -18.0,
            ..AudioEnhanceSettings::default()
        };
        s.set_band(9, 2.0);

        let snapshot = s.snapshot();
        assert!(!snapshot.bypass, "the master switch becomes the bypass flag");
        assert_eq!(snapshot.bass_db, 5.0);
        assert_eq!(snapshot.loudness_target_db, -18.0);
        assert_eq!(snapshot.eq_gain[9], 2.0);
        assert!(!snapshot.is_idle());
    }

    #[test]
    fn a_hand_edited_audio_enhance_block_is_clamped_on_the_way_to_the_engine() {
        // The settings file is editable by hand, and this is the last point before a wild
        // number becomes a filter coefficient.
        let mut s = AudioEnhanceSettings {
            enabled: true,
            bass_db: 999.0,
            width: f32::INFINITY,
            ..AudioEnhanceSettings::default()
        };
        s.bands = [f32::NAN; mvp_core::dsp::EQ_BANDS];

        let snapshot = s.snapshot();
        assert!(snapshot.eq_gain.iter().all(|gain| gain.is_finite()));
        assert_eq!(snapshot.bass_db, 12.0);
        assert!(snapshot.spatial_width.is_finite());
    }

    #[test]
    fn the_audio_enhance_block_round_trips_through_json() {
        let mut s = AudioEnhanceSettings {
            enabled: true,
            preset: EqPreset::Voice,
            bass_db: 3.0,
            room: 0.25,
            ..AudioEnhanceSettings::default()
        };
        s.apply_preset(EqPreset::Voice);

        let text = serde_json::to_string(&s).unwrap();
        let back: AudioEnhanceSettings = serde_json::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn an_old_settings_file_without_the_audio_enhance_block_still_loads() {
        // Every settings file written before this feature existed has no such key, and
        // `#[serde(default)]` has to produce the neutral, switched-off block.
        let json = r#"{"volume":0.5}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.volume, 0.5);
        assert!(!s.audio_enhance.enabled);
        assert!(s.audio_enhance.is_neutral());
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

    /// The picture sliders remember where they belong: globally, or per file with the
    /// globals as the fallback for a file nobody has tuned.
    #[test]
    fn picture_memory_follows_the_scope() {
        let mut settings = Settings::default();
        let tuned = PictureSettings {
            brightness: 0.5,
            ..Default::default()
        };

        // Global writes one place and answers for every file.
        settings.set_picture_for(Some("a.mkv"), tuned);
        assert_eq!(settings.picture_for(Some("a.mkv")), tuned);
        assert_eq!(settings.picture_for(Some("b.mkv")), tuned);
        assert!(
            settings.per_file_picture.is_empty(),
            "a global write does not touch the per-file list"
        );

        // Per file answers for the tuned file, and falls back for anything else.
        settings.picture_scope = RememberScope::PerFile;
        settings.picture = PictureSettings::default();
        settings.set_picture_for(Some("a.mkv"), tuned);
        assert_eq!(settings.picture_for(Some("a.mkv")), tuned);
        assert_eq!(
            settings.picture_for(Some("b.mkv")),
            PictureSettings::default(),
            "an untuned file falls back to the globals"
        );
        assert_eq!(
            settings.picture_for(None),
            PictureSettings::default(),
            "with no file open there is nothing to override"
        );

        settings.clear_per_file_pictures();
        assert!(settings.per_file_picture.is_empty());
        assert_eq!(settings.picture_for(Some("a.mkv")), settings.picture);
    }

    /// The per-file list is bounded, and it drops the file that has gone longest
    /// without being tuned — a settings file that grows without limit is one that
    /// eventually fails to load.
    #[test]
    fn per_file_picture_memory_is_bounded_and_evicts_the_oldest() {
        let mut settings = Settings {
            picture_scope: RememberScope::PerFile,
            ..Default::default()
        };
        let tuned = PictureSettings {
            contrast: 0.25,
            ..Default::default()
        };
        for index in 0..(Settings::PER_FILE_PICTURE_LIMIT + 10) {
            settings.set_picture_for(Some(&format!("file-{index}.mkv")), tuned);
        }
        assert_eq!(
            settings.per_file_picture.len(),
            Settings::PER_FILE_PICTURE_LIMIT,
            "the list is capped"
        );
        assert_eq!(
            settings.per_file_picture[0].0, "file-209.mkv",
            "the most recent write is first"
        );
        assert!(
            !settings
                .per_file_picture
                .iter()
                .any(|(key, _)| key == "file-0.mkv"),
            "the first file to be tuned is the first to go"
        );

        // Re-tuning a file that is already there moves it to the front rather than
        // adding a second entry for it.
        settings.set_picture_for(Some("file-209.mkv"), tuned);
        assert_eq!(settings.per_file_picture.len(), Settings::PER_FILE_PICTURE_LIMIT);
        assert_eq!(
            settings
                .per_file_picture
                .iter()
                .filter(|(key, _)| key == "file-209.mkv")
                .count(),
            1,
            "no duplicates"
        );
    }

    /// The reset button is `reset()`, and it has to give back exactly the defaults —
    /// including `gamma`, whose default is the identity rather than zero.
    #[test]
    fn the_reset_button_gives_back_the_defaults() {
        let mut picture = PictureSettings {
            brightness: -0.4,
            gamma: 1.8,
            sharpness: 1.2,
            ..Default::default()
        };
        assert!(!picture.is_neutral());
        picture.reset();
        assert!(picture.is_neutral());
        assert_eq!(picture.gamma, 1.0, "gamma's default is 1.0, not 0.0");
    }

    #[test]
    fn the_enhancement_sliders_are_clamped_on_the_way_in() {
        let mut enhance = EnhanceSettings {
            strength: 4.0,
            denoise: -3.0,
            deblock: 0.5,
            ..Default::default()
        };
        enhance.clamp();
        assert_eq!(enhance.strength, 1.0);
        assert_eq!(enhance.denoise, 0.0);
        assert_eq!(enhance.deblock, 0.5, "a value already in range is left alone");
        assert!(
            !enhance.enabled,
            "the enhancement starts switched off, so the picture starts untouched"
        );
    }

    /// A hand-edited settings file that mentions only the slider the user cares about
    /// must not cost them everything else.
    ///
    /// This block is the one part of the document people are invited to edit, so
    /// `{"picture":{"brightness":0.5}}` is a perfectly reasonable thing to write. Without
    /// `#[serde(default)]` on the block itself serde rejects the *whole* document over the
    /// missing neighbours: the player announces 设置文件损坏, every preference silently
    /// goes back to its default, and the slider the user set does nothing. That is not a
    /// theory — it is what the first end-to-end capture of this feature measured, as a
    /// "changed" run coming back byte-identical to the baseline.
    #[test]
    fn a_partial_picture_block_keeps_the_rest_of_the_document() {
        let json = r#"{
            "volume": 0.4,
            "picture_scope": "PerFile",
            "picture": { "brightness": 0.5 },
            "enhance": { "enabled": true },
            "per_file_picture": [["a.mkv", { "sharpness": 0.7 }]]
        }"#;
        let settings: Settings = serde_json::from_str(json).expect("a partial block still parses");
        assert_eq!(settings.volume, 0.4, "the rest of the document survived");
        assert_eq!(settings.picture.brightness, 0.5);
        assert_eq!(settings.picture.gamma, 1.0, "gamma falls back to the identity");
        assert_eq!(settings.picture.saturation, 0.0);

        let expected = EnhanceSettings {
            enabled: true,
            ..Default::default()
        };
        assert_eq!(
            settings.enhance, expected,
            "an unmentioned switch keeps its default rather than taking the block down"
        );
        assert_eq!(
            settings.picture_for(Some("a.mkv")).sharpness,
            0.7,
            "a per-file entry is partial too"
        );
    }

    /// The switch that decides whether a snapshot is the frame the engine produced
    /// or the picture as it is on screen.
    #[test]
    fn a_snapshot_keeps_the_raw_frame_unless_the_setting_asks_for_the_other() {
        assert!(
            !Settings::default().snapshot_includes_picture,
            "off, so a snapshot stays what it has always been"
        );

        // A document written before this existed has no such field; `serde(default)`
        // fills it in and the user's snapshots do not change.
        let old: Settings = serde_json::from_str(r#"{"volume": 0.4}"#).expect("parses");
        assert!(!old.snapshot_includes_picture);

        let asked: Settings =
            serde_json::from_str(r#"{"snapshot_includes_picture": true}"#).expect("parses");
        assert!(asked.snapshot_includes_picture);
    }
}
