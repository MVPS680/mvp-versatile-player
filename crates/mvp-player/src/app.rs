//! The application object: it owns the engine, the playlist, the settings and
//! all transient UI state, and it is the only place where those four are allowed
//! to talk to each other.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use egui::{Context, TextureHandle, TextureOptions};
use mvp_core::engine::{EngineConfig, EngineEvent, MediaSource, PlaybackState};
use mvp_core::playlist::{self, Playlist, PlaylistItem};
use mvp_core::util::MediaKind;
use mvp_core::{Engine, ImageView, MediaInfo};
use mvp_platform::power::SleepBlocker;
use mvp_platform::single_instance::{AppInstance, IpcMessage};
use mvp_subtitle::Subtitle;

use crate::settings::{LaunchOverrides, Settings, SettingsStore};
use crate::state::{FrameHistory, InfoRow, Mode, Overlay, SettingsTab, ShownFrame, Toast, ToastKind, UiState};
use crate::theme::Theme;

/// Options the player was started with.
#[derive(Debug, Clone, Default)]
pub struct StartupArgs {
    /// Files and folders named on the command line.
    pub files: Vec<PathBuf>,
    /// Start in fullscreen.
    pub fullscreen: bool,
    /// Do not start playing automatically.
    pub no_autoplay: bool,
    /// Override the starting volume (`0..=200`).
    pub volume: Option<f32>,
    /// Override the starting speed.
    pub speed: Option<f64>,
    /// A subtitle file to load for the first item.
    pub subtitle: Option<PathBuf>,
    /// Suppress the audio device entirely.
    pub no_audio: bool,
    /// Print the version and exit (handled in `main`).
    pub show_version: bool,
    /// Print the help text and exit (handled in `main`).
    pub show_help: bool,
}

impl StartupArgs {
    /// Whether a file named on the command line should start playing.
    ///
    /// Always, unless this launch asked otherwise: a path handed to the player
    /// — a double-click on an associated extension, Explorer's "Open with",
    /// something typed in a shell — is an explicit request to play *that* file.
    /// The only way to open something paused is `--no-autoplay` (`--paused`),
    /// and that lasts exactly one launch: it is deliberately not a preference,
    /// because the retired "打开文件后自动播放" switch could leave the player
    /// unable to play a double-clicked file with no sign of why.
    pub fn autoplay_command_line_files(&self) -> bool {
        !self.no_autoplay
    }
}

/// Everything the player is and knows.
pub struct PlayerApp {
    /// The media engine, shared with the IPC thread.
    pub engine: Arc<Engine>,
    /// Persisted settings, with this launch's command-line overrides folded in.
    pub settings: Settings,
    /// Settings as they were on disk, before the command line was folded in.
    ///
    /// Kept so the save path can tell an untouched `--volume` from a volume the
    /// user has since moved by hand. See [`Settings::persisted`].
    pub baseline: Settings,
    /// The command-line values that belong to this launch alone.
    pub launch: LaunchOverrides,
    /// Debounced settings writer.
    pub store: SettingsStore,
    /// Visual tokens.
    pub theme: Theme,

    /// The playlist.
    pub playlist: Playlist,
    /// Still-image viewer state.
    pub image: ImageView,
    /// What the main area shows.
    pub mode: Mode,
    /// Transient interface state.
    pub ui: UiState,

    /// GPU texture holding the current video frame.
    pub texture: Option<TextureHandle>,
    /// Serial number of the frame currently uploaded.
    pub uploaded_serial: u64,
    /// Presentation timestamp of the frame currently uploaded.
    pub uploaded_pts: f64,
    /// Pixel size of the frame currently uploaded.
    pub uploaded_size: (u32, u32),
    /// Which still-image frame is on the GPU, as `(viewer serial, frame index)`.
    ///
    /// The video path has `uploaded_serial` for the same reason: a repaint is
    /// not a reason to convert and upload pixels that have not changed.
    pub uploaded_image: Option<(u64, usize)>,
    /// The exact image currently on screen, kept so a snapshot never has to ask
    /// the engine for pixels it has already given away.
    pub displayed: Option<Arc<egui::ColorImage>>,
    /// The frames already shown, which is the only place a backward step can
    /// come from. See [`ShownFrame`].
    pub frame_history: FrameHistory,
    /// The frame a backward step moved away from, so stepping forward again
    /// returns to it instead of skipping it.
    pub redo_frame: Option<ShownFrame>,

    /// The single-instance guard, held for the lifetime of the process.
    pub instance: Option<AppInstance>,
    /// Messages forwarded from other instances.
    pub ipc_rx: crossbeam_channel::Receiver<IpcMessage>,

    /// Native window handle, used for the dark title bar and taskbar hints.
    pub hwnd: isize,
    /// Keeps the window inside the screen it is on. See [`crate::display`].
    pub window_fit: crate::display::Guard,
    /// The dark-title-bar value that has been applied, if any. `None` until the
    /// window exists.
    pub titlebar_dark: Option<bool>,
    /// Keeps the display awake while playing.
    pub sleep_blocker: SleepBlocker,

    /// Size the video is being scaled to, so the engine can decode at that size.
    pub video_target: (u32, u32),

    /// Side-car subtitle found for the file being opened, held back until the
    /// file has been probed: "external subtitles win" only applies when the file
    /// has no embedded track worth showing.
    pub pending_sidecar: Option<PathBuf>,

    /// `true` when the file being opened names an audio extension.
    ///
    /// Which screen a file gets is decided by the probe, which is the
    /// authority — but the probe takes a few milliseconds, and in them an MP3
    /// would still be showing the video canvas' "正在准备画面…". Nothing can be
    /// prepared for a file that has no picture, so the extension covers that
    /// gap; see [`PlayerApp::is_audio_only`].
    pub pending_audio: bool,

    /// Wall-clock instant the process started, for the startup-time readout.
    pub process_start: Instant,
    /// Cached info rows for the current media, rebuilt only when the file changes.
    pub info_source: Option<String>,
    /// Cached duration, used to detect when the engine has resolved it.
    pub last_duration: f64,
    /// Set once the first frame has been painted.
    pub first_frame_painted: bool,
    /// Title of the window as last applied, so we only rename on change.
    pub last_title: String,
}

impl PlayerApp {
    /// Build the application, restoring the previous session.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        args: StartupArgs,
        instance: Option<AppInstance>,
        ipc_rx: crossbeam_channel::Receiver<IpcMessage>,
        process_start: Instant,
    ) -> Self {
        let mut settings = Settings::load();
        // Snapshot the document before the command line is folded in: the values
        // that came from `settings.json` are the ones that belong there.
        let baseline = settings.clone();
        let launch = LaunchOverrides {
            volume: args.volume,
            speed: args.speed.map(|speed| speed.clamp(0.25, 4.0)),
        };
        settings.apply_launch_overrides(&launch);

        let theme = Theme::default();
        theme.install(&cc.egui_ctx);

        let engine_config = EngineConfig {
            hardware_decoding: settings.hardware_decoding,
            hdr_tone_map: settings.hdr_tone_map,
            audio_enabled: !args.no_audio,
            audio_device: settings.audio_device.clone(),
            volume: settings.volume,
            speed: settings.speed,
            // The engine is told before the first file is opened; `open_engine`
            // sets it again for every open, which is what makes a file picked
            // inside the player always play.
            autoplay: args.autoplay_command_line_files(),
            ..EngineConfig::default()
        };
        let engine = Arc::new(
            Engine::new(engine_config).expect("FFmpeg 初始化失败"),
        );
        engine.set_speed(settings.speed);
        engine.set_volume(settings.volume);
        engine.set_muted(settings.muted);
        engine.set_looping(settings.repeat == playlist::RepeatMode::One);
        engine.set_audio_delay(settings.audio_delay);
        engine.set_subtitle_delay(settings.subtitle_delay);
        engine.set_hardware_decoding(settings.hardware_decoding);
        engine.set_hdr_tone_map(settings.hdr_tone_map);

        let mut playlist = Playlist::new();
        if settings.restore_playlist && !settings.playlist.is_empty() {
            playlist.set_items(settings.playlist.clone());
        }
        playlist.set_repeat(settings.repeat);
        playlist.set_shuffle(settings.shuffle);

        let hwnd = window_handle_from(cc);

        let mut app = Self {
            engine,
            settings,
            baseline,
            launch,
            store: SettingsStore::default(),
            theme,
            playlist,
            image: ImageView::new(),
            mode: Mode::Empty,
            ui: UiState::default(),
            texture: None,
            uploaded_serial: 0,
            uploaded_pts: 0.0,
            uploaded_size: (0, 0),
            uploaded_image: None,
            displayed: None,
            frame_history: FrameHistory::new(),
            redo_frame: None,
            instance,
            ipc_rx,
            hwnd,
            window_fit: crate::display::Guard::default(),
            titlebar_dark: None,
            sleep_blocker: SleepBlocker::new(false),
            video_target: (0, 0),
            pending_sidecar: None,
            pending_audio: false,
            process_start,
            info_source: None,
            last_duration: 0.0,
            first_frame_painted: false,
            last_title: String::new(),
        };

        app.ui.sidebar_visible = app.settings.sidebar_visible;
        app.ui.sidebar_tab = app.settings.sidebar_tab;
        app.ui.startup_ms = process_start.elapsed().as_secs_f32() * 1000.0;
        app.ui.ffmpeg_version = mvp_core::ffmpeg_version();
        app.ui.ffmpeg_config = mvp_core::ffmpeg_configuration();
        app.image.slideshow_interval = app.settings.slideshow_interval;
        app.image.animation_playing = app.settings.animate_images;

        // Apply the persisted window level once; sending it every frame would
        // make the window manager re-evaluate the stacking order 60 times a
        // second for no reason.
        if app.settings.always_on_top {
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                    egui::WindowLevel::AlwaysOnTop,
                ));
        }

        if !args.files.is_empty() {
            // A file the shell hands over always plays; only `--no-autoplay`
            // asks for a paused start, and only for this launch.
            app.open_paths(&args.files, true, args.autoplay_command_line_files());
        } else if app.settings.restore_playlist {
            if let Some(index) = app.settings.playlist_index {
                if index < app.playlist.len() {
                    app.playlist.set_current(Some(index));
                }
            }
        }
        if let Some(subtitle) = args.subtitle {
            app.load_subtitle_file(&subtitle);
        }
        app
    }

    // -----------------------------------------------------------------------
    // Opening media
    // -----------------------------------------------------------------------

    /// Add `paths` to the playlist (replacing it when `replace`) and open the
    /// first one.
    ///
    /// `autoplay` is how *this* open was requested. Files the player opens for
    /// the user play by default — a path on the command line, a double-click in
    /// Explorer, a drop, the file dialog, a pick in the playlist — and the only
    /// caller that may pass `false` is the start-up path, when the launch itself
    /// asked for a paused start with `--no-autoplay`. With it the entry is
    /// loaded and waits rather than not opening at all.
    pub fn open_paths(&mut self, paths: &[PathBuf], replace: bool, autoplay: bool) {
        if paths.is_empty() {
            return;
        }
        let expanded = playlist::expand_paths(paths);
        if expanded.is_empty() {
            self.toast(Toast::warning("没有找到可播放的文件").with_icon(crate::icons::Icon::Info));
            return;
        }
        if replace {
            self.playlist.clear();
        }
        let first = self.playlist.len();
        for path in &expanded {
            self.playlist.add(PlaylistItem::from_path(path));
        }
        self.playlist.set_current(Some(first));
        if let Some(dir) = expanded[0].parent() {
            self.settings.last_dir = Some(dir.to_path_buf());
        }
        self.store.mark_dirty();
        // The entry is loaded either way — with autoplay off it simply waits
        // paused rather than not opening at all.
        self.play_index_with(first, autoplay);
    }

    /// Add a network URL to the playlist and play it.
    pub fn open_url(&mut self, url: String, replace: bool) {
        let url = url.trim().to_string();
        if url.is_empty() {
            return;
        }
        if replace {
            self.playlist.clear();
        }
        let index = self.playlist.add(PlaylistItem::from_url(url));
        self.play_index(index);
    }

    /// Play the playlist entry at `index`.
    ///
    /// Selecting an entry is an instruction to play it, so this always plays:
    /// nothing a user clicks inside the player is ever ignored because of how
    /// the player was launched.
    pub fn play_index(&mut self, index: usize) {
        self.play_index_with(index, true);
    }

    /// Move to `index` and open it, starting playback when `autoplay`.
    fn play_index_with(&mut self, index: usize, autoplay: bool) {
        let Some(item) = self.playlist.items().get(index).cloned() else {
            return;
        };
        self.playlist.set_current(Some(index));
        self.settings.playlist_index = Some(index);
        self.store.mark_dirty();

        if !item.is_url {
            let path = item.path();
            self.settings.push_recent(&path);
            self.store.mark_dirty();
        }

        // Save where we were in the file we are leaving.
        self.remember_current_position();

        let source = MediaSource::new(item.source.clone());
        match source {
            MediaSource::Path(path) => self.open_local(&path, autoplay),
            MediaSource::Url(_) => self.open_engine(source, None, autoplay),
        }
    }

    /// Decide between the image viewer and the media engine for a local path.
    fn open_local(&mut self, path: &Path, autoplay: bool) {
        let kind = mvp_core::util::classify(path);
        if kind == MediaKind::Image {
            match self.image.open(path) {
                Ok(()) => {
                    self.mode = Mode::Image;
                    // A new file starts fitted: the zoom and the pan of the last
                    // one belong to that picture, not to this one.
                    self.reset_zoom();
                    self.texture = None;
                    self.displayed = None;
                    self.uploaded_serial = 0;
                    self.uploaded_pts = 0.0;
                    self.uploaded_image = None;
                    self.forget_shown_frames();
                    // The presentation clock belongs to the file that was on
                    // screen, not to this one: a first frame must not be timed
                    // against the last frame of the previous file.
                    self.ui.present.reset_timing();
                    self.engine.stop();
                    self.rebuild_info_rows_image();
                    let doc = self.image.doc.as_ref().map(|d| d.summary()).unwrap_or_default();
                    self.toast(Toast::success(doc).with_icon(crate::icons::Icon::Image));
                }
                Err(err) => {
                    self.error(format!("无法打开图片: {err}"));
                }
            }
            self.load_sidecar_subtitle(path);
            return;
        }
        self.open_engine(
            MediaSource::Path(path.to_path_buf()),
            Some(path.to_path_buf()),
            autoplay,
        );
    }

    fn open_engine(&mut self, source: MediaSource, local: Option<PathBuf>, autoplay: bool) {
        // The engine decides whether a freshly opened file runs, and it has to
        // be told *before* the file is opened — the demuxer reads this when it
        // publishes the first state.
        self.engine.set_autoplay(autoplay);
        if let Err(err) = self.engine.open(source.clone()) {
            self.error(format!("无法打开: {err}"));
            return;
        }
        // Before the probe lands, the file name is the only thing that knows
        // this file has nothing to show (see `pending_audio`).
        self.pending_audio = local
            .as_deref()
            .is_some_and(|path| mvp_core::util::classify(path) == MediaKind::Audio);
        // Restart each file at a clean slate.
        self.engine.set_speed(self.settings.speed);
        self.engine.set_volume(self.settings.volume);
        self.engine.set_muted(self.settings.muted);
        self.engine.set_audio_delay(self.settings.audio_delay);
        self.engine.set_subtitle_delay(self.settings.subtitle_delay);
        self.engine.set_external_subtitle(None);
        self.engine.set_ab_loop(None);
        // "Show subtitles when the file has them" means: let the container pick
        // its default text track. Turning them off means the decoder is not even
        // started, and nothing is lost by that because the choice is only a
        // display gate that the renderer already honours.
        if self.settings.subtitles_enabled {
            self.engine.set_subtitle_track_auto();
        } else {
            self.engine.set_subtitle_track(None);
        }
        self.mode = Mode::Media;
        self.image.close();
        self.reset_zoom();
        self.texture = None;
        self.displayed = None;
        self.uploaded_serial = 0;
        self.uploaded_pts = 0.0;
        self.uploaded_image = None;
        self.forget_shown_frames();
        self.ui.present.reset_timing();
        // A seek preview belongs to the file that was open when it was made.
        self.ui.seek_drag = None;
        self.ui.seek_hold = None;
        self.info_source = None;
        self.last_duration = 0.0;
        self.engine
            .set_target_size(self.video_target.0, self.video_target.1);

        self.pending_sidecar = None;
        if let Some(path) = local {
            self.attach_sidecar_subtitle(&path);
        }
    }

    /// Attach the same-named subtitle next to `media`, honouring the user's
    /// preference when the file also carries one.
    ///
    /// The engine always lets an external side-car win over an embedded track,
    /// so the decision has to be made *here*: either load it now, or hold it
    /// back until the probe says whether the file has a text track of its own.
    fn attach_sidecar_subtitle(&mut self, media: &Path) {
        if !self.settings.autoload_sidecar_subtitles || !self.settings.subtitles_enabled {
            return;
        }
        let Some(sidecar) = find_sidecar_subtitle(media) else {
            return;
        };
        if self.settings.prefer_external_subtitles {
            self.load_subtitle_file(&sidecar);
        } else {
            self.pending_sidecar = Some(sidecar);
        }
    }

    /// Decide what to do with a held-back side-car now that the file is probed.
    ///
    /// With "external subtitles preferred" off, a file that has text subtitles
    /// of its own is shown with those; the side-car stays on disk and can still
    /// be loaded by hand.
    fn resolve_pending_sidecar(&mut self, info: &MediaInfo) {
        let Some(sidecar) = self.pending_sidecar.take() else {
            return;
        };
        let has_embedded = info.subtitles.iter().any(|stream| stream.is_text);
        if has_embedded {
            log::info!("使用内嵌字幕，忽略同名字幕文件: {}", sidecar.display());
        } else {
            self.load_subtitle_file(&sidecar);
        }
    }

    /// Seek to the remembered position once the duration is known.
    ///
    /// The duration only arrives with the `Opened` event, so the "was this file
    /// finished?" check cannot happen any earlier.
    fn apply_resume_position(&mut self, info: &MediaInfo) {
        let key = info.path.to_string_lossy().into_owned();
        let Some(position) = self.settings.resume_position(&key, info.duration) else {
            return;
        };
        self.seek(position);
        self.toast(Toast::info(format!(
            "从 {} 继续播放",
            mvp_core::util::format_duration(position)
        )));
    }

    /// Look for `name.srt` / `name.ass` / `name.vtt` next to `media` and load it.
    fn load_sidecar_subtitle(&mut self, media: &Path) {
        if !self.settings.autoload_sidecar_subtitles || !self.settings.subtitles_enabled {
            return;
        }
        if let Some(candidate) = find_sidecar_subtitle(media) {
            self.load_subtitle_file(&candidate);
        }
    }

    /// Load a subtitle file into the engine.
    pub fn load_subtitle_file(&mut self, path: &Path) {
        match std::fs::read(path) {
            Ok(bytes) => {
                let subtitle = Subtitle::parse(&bytes);
                if subtitle.is_empty() {
                    self.toast(Toast::warning(format!(
                        "字幕文件没有可用内容: {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )));
                    return;
                }
                let count = subtitle.len();
                self.engine
                    .set_external_subtitle(Some(Arc::new(subtitle)));
                self.settings.last_subtitle_dir =
                    path.parent().map(std::path::Path::to_path_buf);
                self.store.mark_dirty();
                self.toast(Toast::success(format!(
                    "已加载字幕 · {count} 条 · {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )));
            }
            Err(err) => self.error(format!("无法读取字幕文件: {err}")),
        }
    }

    // -----------------------------------------------------------------------
    // Playlist navigation
    // -----------------------------------------------------------------------

    /// Move to the next entry. `auto` is `true` when the file ended by itself.
    pub fn next_media(&mut self, auto: bool) {
        match self.playlist.next_index(auto) {
            Some(index) => self.play_index(index),
            None => {
                if auto {
                    self.engine.pause();
                    self.toast(Toast::info("播放列表已结束"));
                }
            }
        }
    }

    /// Move to the previous entry.
    pub fn prev_media(&mut self) {
        if let Some(index) = self.playlist.prev_index() {
            self.play_index(index);
        }
    }

    /// `true` when there is an entry after the current one.
    pub fn has_next(&self) -> bool {
        self.playlist.next_index(false).is_some()
    }

    /// `true` when there is an entry before the current one.
    pub fn has_previous(&self) -> bool {
        self.playlist.prev_index().is_some()
    }

    // -----------------------------------------------------------------------
    // Frame upload
    // -----------------------------------------------------------------------

    /// Pull a decoded frame from the engine and upload it as a texture.
    ///
    /// The conversion is performed without copying pixels: the frame's `Vec<u8>`
    /// allocation is reinterpreted as `Vec<Color32>` (both are four bytes per
    /// pixel, align 1), moved into an `Arc<ColorImage>` and handed to `egui` by
    /// reference. That is only possible because the engine hands out the *only*
    /// reference to the frame — it deliberately keeps none of its own.
    pub fn update_frame_texture(&mut self, ctx: &Context) {
        if self.mode != Mode::Media {
            return;
        }
        let now = self.engine.display_position();
        let Some(frame) = self.engine.take_frame(now) else {
            return;
        };
        if frame.serial == self.uploaded_serial {
            return;
        }
        if frame.width == 0 || frame.height == 0 {
            return;
        }
        let size = [frame.width as usize, frame.height as usize];
        let expected = size[0] * size[1] * 4;
        if frame.data.len() < expected {
            return;
        }
        let serial = frame.serial;
        let pts = frame.pts;
        let dimensions = (frame.width, frame.height);

        // `cast_vec` reuses the allocation: `u8` and `Color32` have the same
        // size and alignment, so this is a pointer move rather than a copy.
        let Ok(frame) = Arc::try_unwrap(frame) else {
            // Unreachable by construction; if it ever happens, dropping the
            // frame is far better than showing nothing at all.
            debug_assert!(false, "the engine handed out a shared frame");
            return;
        };
        let mut data = frame.data;
        data.truncate(expected);
        let pixels: Vec<egui::Color32> = bytemuck::allocation::cast_vec(data);

        // Remember what is about to be replaced, *before* it is replaced: a
        // backward step can only ever show something that has already been on
        // screen, and this is the moment a frame stops being the current one.
        self.remember_shown_frame();
        // A frame from the decoder means the playhead moved on, so a frame held
        // over from a backward step is no longer "next".
        self.redo_frame = None;

        self.upload_image(
            ctx,
            egui::ColorImage {
                size,
                source_size: egui::vec2(size[0] as f32, size[1] as f32),
                pixels,
            },
        );
        self.uploaded_serial = serial;
        self.uploaded_pts = pts;
        self.uploaded_size = dimensions;
    }

    /// Upload the current image-viewer frame.
    ///
    /// A still image only changes when the *frame* changes, but this runs on
    /// every repaint — and repaints happen whenever the pointer moves, the
    /// window is resized, or the controls animate. Converting and re-uploading
    /// the same picture each time is the single largest cost in the viewer: an
    /// 8192x8192 photo is 268 MB of pixels per repaint. So the frame that is
    /// already on the GPU is remembered by identity, exactly as the video path
    /// remembers its serial, and nothing happens when it has not changed.
    pub fn update_image_texture(&mut self, ctx: &Context) {
        if self.mode != Mode::Image {
            return;
        }
        let Some(frame_index) = self.image.current_frame_index() else {
            return;
        };
        let key = (self.image.serial(), frame_index);
        if self.uploaded_image == Some(key) && self.texture.is_some() {
            return;
        }
        let Some(frame) = self.image.current_frame() else {
            return;
        };
        let Some((width, height)) = self.image.dimensions() else {
            return;
        };
        let size = [width as usize, height as usize];
        let expected = size[0] * size[1] * 4;
        if frame.data.len() < expected {
            return;
        }
        // Images *do* carry meaningful alpha, and `Color32` is premultiplied, so
        // this path has to do the real conversion rather than a reinterpret.
        let image = egui::ColorImage::from_rgba_unmultiplied(size, &frame.data[..expected]);
        self.upload_image(ctx, image);
        self.uploaded_size = (width, height);
        self.uploaded_image = Some(key);
    }

    /// Hand a finished image to the GPU and remember it for snapshots.
    ///
    /// `egui` takes the pixels by `Arc`, so retaining our own reference for the
    /// snapshot command costs a pointer, not a copy.
    fn upload_image(&mut self, ctx: &Context, image: egui::ColorImage) {
        let image = Arc::new(image);
        match &mut self.texture {
            Some(texture) => texture.set(Arc::clone(&image), TextureOptions::LINEAR),
            None => {
                self.texture = Some(ctx.load_texture(
                    "mvp-frame",
                    Arc::clone(&image),
                    TextureOptions::LINEAR,
                ));
            }
        }
        // A frame is on its way to the screen: this is the clock the presented
        // frame rate is derived from. The still-image path goes through here
        // too, and for an animation that is exactly right.
        self.ui.present.record_presented();
        self.displayed = Some(image);
    }

    // -----------------------------------------------------------------------
    // Events and housekeeping
    // -----------------------------------------------------------------------

    /// Drain messages forwarded by other instances.
    pub fn handle_ipc(&mut self) {
        while let Ok(message) = self.ipc_rx.try_recv() {
            match message {
                IpcMessage::OpenPaths(paths) => {
                    self.open_paths(&paths, true, true);
                }
                IpcMessage::Activate => {
                    mvp_platform::shell::foreground_window(self.hwnd);
                    self.ui.wake_controls(5.0);
                }
                IpcMessage::Quit => {
                    self.save_session();
                    self.ui.close_requested = true;
                }
            }
        }
    }

    /// React to engine events.
    pub fn handle_engine_events(&mut self, ctx: &Context) {
        // Bounded on purpose: a malfunctioning producer must not be able to keep
        // this loop running forever and freeze the interface.
        const MAX_EVENTS_PER_FRAME: usize = 64;
        for _ in 0..MAX_EVENTS_PER_FRAME {
            let Some(event) = self.engine.poll_event() else {
                break;
            };
            match event {
                EngineEvent::Opened(info) => {
                    self.rebuild_info_rows(&info);
                    self.engine
                        .set_target_size(self.video_target.0, self.video_target.1);
                    self.resolve_pending_sidecar(&info);
                    self.apply_resume_position(&info);
                    self.announce_dynamic_range(&info);
                }
                EngineEvent::StateChanged(_) => {}
                EngineEvent::SubtitleChanged(_) => {}
                EngineEvent::Error(message) => {
                    // A missing audio device is worth a warning, not an error
                    // banner: the video is still perfectly watchable.
                    if message.starts_with("音频输出不可用") {
                        self.toast(Toast::warning(message));
                    } else {
                        self.error(message);
                    }
                }
                EngineEvent::Ended => {
                    self.on_media_ended(ctx);
                }
            }
        }
    }

    fn on_media_ended(&mut self, ctx: &Context) {
        // A finished file should not be resumed next time, so any stored
        // position is dropped rather than saved.
        if let Some(source) = self.engine.source() {
            self.settings.clear_position(&source.as_str());
            self.store.mark_dirty();
        }
        match self.settings.end_action {
            crate::settings::EndAction::Close => {
                self.save_session();
                self.ui.close_requested = true;
            }
            crate::settings::EndAction::Hold => {
                self.engine.pause();
            }
            crate::settings::EndAction::Playlist => {
                if self.playlist.repeat() == playlist::RepeatMode::One {
                    self.play_index(self.playlist.current_index().unwrap_or(0));
                } else {
                    let next = self.playlist.next_index(true);
                    match next {
                        Some(index) => self.play_index(index),
                        None => {
                            self.engine.pause();
                            self.toast(Toast::info("播放列表已结束"));
                        }
                    }
                }
            }
        }
        ctx.request_repaint();
    }

    /// Persist the current playback position for resume.
    pub fn remember_current_position(&mut self) {
        let Some(source) = self.engine.source() else {
            return;
        };
        let key = source.as_str();
        let duration = self.engine.duration();
        let position = self.engine.position();
        if duration > 0.0 && position > 0.0 && position < duration - 5.0 {
            self.settings.store_position(&key, position);
            self.store.mark_dirty();
        }
    }

    /// Push the settings that can change at runtime into the engine.
    pub fn sync_engine(&mut self) {
        self.engine.set_volume(self.settings.volume);
        self.engine.set_muted(self.settings.muted);
        self.engine.set_speed(self.settings.speed);
        self.engine.set_audio_delay(self.settings.audio_delay);
        self.engine.set_subtitle_delay(self.settings.subtitle_delay);
        self.engine
            .set_looping(self.settings.repeat == playlist::RepeatMode::One);
        self.engine.set_hdr_tone_map(self.settings.hdr_tone_map);
        self.playlist.set_repeat(self.settings.repeat);
        self.playlist.set_shuffle(self.settings.shuffle);
    }

    /// Save the session so the next launch can restore it.
    pub fn save_session(&mut self) {
        self.settings.playlist = self.playlist.items().to_vec();
        self.settings.playlist_index = self.playlist.current_index();
        self.settings.sidebar_visible = self.ui.sidebar_visible;
        self.settings.sidebar_tab = self.ui.sidebar_tab;
        self.store.mark_dirty();
        let document = self.settings_to_persist();
        self.store.flush(&document);
    }

    /// The settings document that belongs in `settings.json`.
    ///
    /// The live document carries this launch's command-line overrides so the
    /// interface can show them; the file must not, or a single `--volume` or
    /// `--speed` would become a permanent preference.
    fn settings_to_persist(&self) -> Settings {
        self.settings.persisted(&self.launch, &self.baseline)
    }

    /// Show an error banner and a toast.
    pub fn error(&mut self, message: impl Into<String>) {
        let message = message.into();
        log::warn!("{message}");
        self.ui.error_banner = Some(message.clone());
        self.ui.toast(Toast::error(message).with_icon(crate::icons::Icon::Info));
    }

    /// Show a transient message.
    pub fn toast(&mut self, toast: Toast) {
        self.ui.toast(toast);
    }

    /// Save a snapshot of the current video frame or image.
    pub fn save_snapshot(&mut self) {
        let dir = self.settings.snapshot_dir();
        if let Err(err) = std::fs::create_dir_all(&dir) {
            self.error(format!("无法创建截图目录: {err}"));
            return;
        }
        let stamp = timestamp_for_filename();
        let path = dir.join(format!("MVP_{stamp}.png"));

        match self.write_snapshot(&path) {
            Ok(()) => {
                self.ui.last_snapshot = Some(path.clone());
                self.toast(
                    Toast::success(format!("截图已保存 · {}", path.display()))
                        .with_icon(crate::icons::Icon::Snapshot),
                );
            }
            Err(err) => self.error(format!("截图失败: {err}")),
        }
    }

    /// Write the picture currently on screen to `path` as a PNG.
    ///
    /// Both paths snapshot what the user is actually looking at, including the
    /// scaling the engine applied to fit the window — which is why this reads
    /// the uploaded image rather than asking the engine for a frame it has
    /// deliberately already handed over.
    fn write_snapshot(&self, path: &Path) -> Result<(), mvp_core::MediaError> {
        let (width, height, rgba): (u32, u32, Vec<u8>) = if self.mode == Mode::Image {
            let doc = self
                .image
                .doc
                .as_ref()
                .ok_or_else(|| mvp_core::MediaError::other("没有可保存的画面"))?;
            let frame = self
                .image
                .current_frame()
                .ok_or_else(|| mvp_core::MediaError::other("没有可保存的画面"))?;
            (doc.width, doc.height, frame.data.clone())
        } else {
            let image = self
                .displayed
                .as_ref()
                .ok_or_else(|| mvp_core::MediaError::other("还没有解码出画面"))?;
            let width = image.size[0] as u32;
            let height = image.size[1] as u32;
            // `Color32` is four bytes per pixel and `Pod`, so this is the RGBA
            // buffer the engine produced, byte for byte.
            let bytes: Vec<u8> =
                bytemuck::cast_slice::<egui::Color32, u8>(&image.pixels).to_vec();
            (width, height, bytes)
        };

        let image = image::RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| mvp_core::MediaError::other("画面数据不完整"))?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        image.save(path)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Information panel
    // -----------------------------------------------------------------------

    /// Say what the file's dynamic range is, once, when it opens.
    ///
    /// A Dolby Vision or HDR10 file shown without a word of explanation looks
    /// like a washed-out transfer. Saying what it is — and whether the player is
    /// tone mapping it — is the difference between "this player is broken" and
    /// "this player is doing what it can".
    fn announce_dynamic_range(&mut self, info: &MediaInfo) {
        let Some(video) = info.primary_video() else {
            return;
        };
        if !video.hdr.kind.is_hdr() && video.hdr.dovi.is_none() {
            return;
        }
        let label = video.hdr.label();
        if video.hdr.needs_dolby_renderer() {
            self.toast(Toast::warning(format!(
                "{label}：基底层为 IPT 编码，需要杜比视界渲染器才能正确还原，颜色可能不正确"
            )));
        } else if video.hdr.needs_tone_map() && self.settings.hdr_tone_map {
            self.toast(Toast::info(format!("{label} · 已做 HDR→SDR 色调映射")));
        } else {
            self.toast(Toast::info(label));
        }
    }

    fn rebuild_info_rows(&mut self, info: &MediaInfo) {
        self.info_source = Some(info.path.to_string_lossy().into_owned());
        self.ui.info_rows = mvp_core::info::info_rows(info)
            .into_iter()
            .map(|(label, value)| InfoRow { label, value })
            .collect();
        self.last_duration = info.duration;
    }

    fn rebuild_info_rows_image(&mut self) {
        let Some(doc) = &self.image.doc else {
            self.ui.info_rows.clear();
            return;
        };
        self.info_source = Some(doc.path.to_string_lossy().into_owned());
        self.ui.info_rows = mvp_core::image_view::info_rows(doc)
            .into_iter()
            .map(|(label, value)| InfoRow { label, value })
            .collect();
    }

    /// Update the OS window title if it changed.
    pub fn update_window_title(&mut self, ctx: &Context) {
        let title = self.window_title();
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }

    fn window_title(&self) -> String {
        let prefix = if self.settings.always_on_top {
            "[置顶] "
        } else {
            ""
        };
        match self.mode {
            Mode::Empty => format!("{prefix}MVP-Versatile-Player"),
            Mode::Image => {
                let name = self
                    .image
                    .doc
                    .as_ref()
                    .and_then(|d| d.path.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_default();
                format!("{prefix}{name} - MVP-Versatile-Player")
            }
            Mode::Media => {
                let name = self
                    .playlist
                    .current()
                    .map(|i| i.title.clone())
                    .unwrap_or_else(|| "MVP-Versatile-Player".to_string());
                let state = self.engine.state();
                let marker = if state == PlaybackState::Paused { "[暂停] " } else { "" };
                format!("{prefix}{marker}{name} - MVP-Versatile-Player")
            }
        }
    }

    /// Apply the native title bar tint, now and whenever the setting changes.
    ///
    /// Re-applied on every change rather than once at start-up: the switch says
    /// "dark title bar", and a switch that only takes effect after a restart is
    /// a fake switch. Handing the colours back to the system is a separate call
    /// because DWM remembers what it was last told.
    pub fn apply_window_chrome(&mut self) {
        if self.hwnd == 0 {
            return;
        }
        let wanted = self.settings.dark_title_bar;
        if self.titlebar_dark == Some(wanted) {
            return;
        }
        if wanted {
            mvp_platform::shell::set_dark_titlebar(self.hwnd, true);
            let t = &self.theme.tokens;
            mvp_platform::shell::set_caption_color(
                self.hwnd,
                [t.bg.r(), t.bg.g(), t.bg.b()],
                [t.text.r(), t.text.g(), t.text.b()],
                [t.border.r(), t.border.g(), t.border.b()],
            );
        } else {
            mvp_platform::shell::set_dark_titlebar(self.hwnd, false);
            mvp_platform::shell::reset_caption_color(self.hwnd);
        }
        self.titlebar_dark = Some(wanted);
    }

    /// Push the whole settings document back into the running player.
    ///
    /// Used after "reset all settings": replacing the struct is only half the
    /// job, because the volume, the window level, the sidebar and the engine's
    /// playback modes all live outside it.
    pub fn apply_settings(&mut self, ctx: &Context) {
        self.sync_engine();
        self.image.slideshow_interval = self.settings.slideshow_interval;
        self.image.animation_playing = self.settings.animate_images;
        self.ui.sidebar_visible = self.settings.sidebar_visible;
        self.ui.sidebar_tab = self.settings.sidebar_tab;
        self.titlebar_dark = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            if self.settings.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            },
        ));
        self.engine
            .set_hardware_decoding(self.settings.hardware_decoding);
        self.store.mark_dirty();
    }

    /// Toggle fullscreen and remember the previous window state.
    pub fn toggle_fullscreen(&mut self, ctx: &Context) {
        self.ui.fullscreen = !self.ui.fullscreen;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.ui.fullscreen));
        if self.ui.fullscreen {
            self.ui.wake_controls(3.0);
        }
    }

    /// Toggle always-on-top.
    pub fn toggle_always_on_top(&mut self, ctx: &Context) {
        self.set_always_on_top(ctx, !self.settings.always_on_top);
    }

    /// Put the window above (or back in line with) the others.
    ///
    /// Toggling the setting alone is not enough: the window level is a message
    /// to the window manager, and a switch that only edits a boolean looks
    /// broken until the next launch.
    pub fn set_always_on_top(&mut self, ctx: &Context, value: bool) {
        if self.settings.always_on_top == value {
            return;
        }
        self.settings.always_on_top = value;
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(if value {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        }));
        self.store.mark_dirty();
    }

    /// `true` when what is on screen is sound and nothing else.
    ///
    /// The probe is the authority as soon as it lands; before that the file
    /// name is (see [`PlayerApp::pending_audio`]). "Has no picture" is a
    /// property of the file, not of how far the pipeline has got, which is why
    /// this is asked *before* the first frame is decoded as well as after.
    pub fn is_audio_only(&self) -> bool {
        if self.mode != Mode::Media {
            return false;
        }
        match self.engine.info() {
            Some(info) => info.is_audio_only(),
            None => self.pending_audio,
        }
    }

    /// `true` when there is a picture on screen: a video frame or an image.
    ///
    /// The picture-only commands — rotate, flip, snapshot, frame stepping —
    /// ask this rather than "is anything open?", so an audio file does not
    /// offer controls that could only ever do nothing to it.
    pub fn has_picture(&self) -> bool {
        match self.mode {
            Mode::Image => true,
            Mode::Media => !self.is_audio_only(),
            Mode::Empty => false,
        }
    }

    /// Human readable "now playing" line for the OSD.
    pub fn now_playing_label(&self) -> Option<String> {
        match self.mode {
            Mode::Empty => None,
            Mode::Image => self
                .image
                .doc
                .as_ref()
                .and_then(|d| d.path.file_name().map(|n| n.to_string_lossy().into_owned())),
            Mode::Media => self.playlist.current().map(|i| i.title.clone()),
        }
    }

    /// Enable or disable the display-sleep blocker according to playback state.
    pub fn update_sleep_blocker(&mut self) {
        let wanted = self.engine.is_playing() && self.mode == Mode::Media;
        if wanted != self.sleep_blocker.is_enabled() {
            self.sleep_blocker.set(wanted);
        }
    }

    /// Compose the whole interface for this frame.
    pub fn ui(&mut self, ctx: &Context) {
        crate::ui::draw(self, ctx);
    }

    // -----------------------------------------------------------------------
    // Native file dialogs
    // -----------------------------------------------------------------------

    /// Ask the user for media files and open them.
    pub fn request_open_file(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("打开媒体文件");
        if let Some(dir) = &self.settings.last_dir {
            if dir.is_dir() {
                dialog = dialog.set_directory(dir);
            }
        }
        let filters: Vec<(&str, Vec<&str>)> = vec![
            (
                "所有支持的媒体",
                mvp_platform::assoc::all_extensions()
                    .iter()
                    .map(|e| e.trim_start_matches('.'))
                    .collect(),
            ),
            ("视频", vec!["mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "ts", "m2ts", "mpg", "mpeg", "rmvb", "3gp", "vob", "ogv"]),
            ("音频", vec!["mp3", "flac", "aac", "m4a", "ogg", "opus", "wav", "wma", "ape", "alac", "ac3", "dts"]),
            ("图片", vec!["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "avif", "jxl", "heic"]),
            ("播放列表", vec!["m3u", "m3u8", "pls", "xspf"]),
            ("所有文件", vec!["*"]),
        ];
        for (name, extensions) in filters {
            dialog = dialog.add_filter(name, &extensions);
        }
        let Some(paths) = dialog.pick_files() else {
            return;
        };
        let replace = !self.engine.state().is_active() && self.mode != Mode::Image;
        self.open_paths(&paths, replace, true);
    }

    /// Ask the user for a folder and queue everything in it.
    pub fn request_open_folder(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("打开文件夹");
        if let Some(dir) = &self.settings.last_dir {
            if dir.is_dir() {
                dialog = dialog.set_directory(dir);
            }
        }
        let Some(folder) = dialog.pick_folder() else {
            return;
        };
        self.open_paths(&[folder], true, true);
    }

    /// Ask the user for a subtitle file and attach it.
    pub fn request_open_subtitle(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("加载字幕文件")
            .add_filter("字幕", &["srt", "ass", "ssa", "vtt", "sub", "smi"]);
        if let Some(dir) = &self.settings.last_subtitle_dir {
            if dir.is_dir() {
                dialog = dialog.set_directory(dir);
            }
        } else if let Some(dir) = &self.settings.last_dir {
            if dir.is_dir() {
                dialog = dialog.set_directory(dir);
            }
        }
        let Some(path) = dialog.pick_file() else {
            return;
        };
        self.load_subtitle_file(&path);
    }

    /// Ask the user where to save the current playlist.
    pub fn request_save_playlist(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("保存播放列表")
            .add_filter("M3U 播放列表", &["m3u"])
            .set_file_name("播放列表.m3u")
            .save_file()
        else {
            return;
        };
        match self.playlist.save_m3u(&path) {
            Ok(()) => self.toast(Toast::success(format!("已保存 · {}", path.display()))),
            Err(err) => self.error(format!("保存播放列表失败: {err}")),
        }
    }

    // -----------------------------------------------------------------------
    // Small media actions shared by the menu, the toolbar and the shortcuts
    // -----------------------------------------------------------------------

    /// Jump to an absolute position.
    ///
    /// Every seek goes through here rather than straight to the engine: a jump
    /// invalidates the frames remembered for single-frame stepping, and a
    /// "previous frame" that showed something from before the jump would be
    /// worse than one that is simply unavailable. It is also the one place that
    /// can tell the transport what the user asked for, so that the bar and the
    /// timestamp show it at once instead of waiting for the demuxer to answer.
    pub fn seek(&mut self, position: f64) {
        self.forget_shown_frames();
        self.engine.seek(position);
        if self.mode.is_media() {
            // Clamped like the engine clamps it, so that a click on the very end
            // of the bar cannot put a position past the duration on screen.
            let duration = self.engine.duration();
            let asked = if duration > 0.0 {
                position.clamp(0.0, duration)
            } else {
                position.max(0.0)
            };
            self.ui.seek_hold = Some((asked, Instant::now()));
        }
    }

    /// Jump forward or backward by `delta` seconds.
    pub fn seek_relative(&mut self, delta: f64) {
        self.forget_shown_frames();
        self.engine.seek_relative(delta);
    }

    /// Lose every remembered frame: they describe a playhead being left behind.
    fn forget_shown_frames(&mut self) {
        self.frame_history.clear();
        self.redo_frame = None;
    }

    /// Remember the frame that is on screen right now.
    fn remember_shown_frame(&mut self) {
        let (Some(image), true) = (&self.displayed, self.uploaded_serial > 0) else {
            return;
        };
        self.frame_history.push(ShownFrame {
            pts: self.uploaded_pts,
            serial: self.uploaded_serial,
            image: Arc::clone(image),
        });
    }

    /// `true` when there is a frame to step back to.
    pub fn can_step_back(&self) -> bool {
        self.mode.is_media() && !self.frame_history.is_empty()
    }

    /// Show the frame before the current one.
    ///
    /// The picture comes from the interface's own history (see [`ShownFrame`]);
    /// the engine is only told to move its playhead, because a container seek
    /// would flush the decoder and lose the exact position being stepped
    /// through.
    pub fn step_back_frame(&mut self, ctx: &Context) {
        if !self.mode.is_media() {
            return;
        }
        let Some(previous) = self.frame_history.pop() else {
            return;
        };
        // Pause first: the playhead is about to move backwards, and a running
        // clock would immediately re-advance past the frame being shown.
        self.engine.pause();
        // Hold on to the frame being left so 下一帧 comes back to it.
        if let (Some(image), true) = (&self.displayed, self.uploaded_serial > 0) {
            self.redo_frame = Some(ShownFrame {
                pts: self.uploaded_pts,
                serial: self.uploaded_serial,
                image: Arc::clone(image),
            });
        }
        self.show_frame(previous, ctx);
    }

    /// Show the next frame: the one a backward step left, or the decoder's next.
    pub fn step_forward_frame(&mut self, ctx: &Context) {
        // A file with no picture has no frames to step through — and pausing it
        // would be the only thing this could still do, which is not what "下一帧"
        // means to anyone.
        if !self.mode.is_media() || self.is_audio_only() {
            return;
        }
        self.engine.pause();
        if let Some(frame) = self.redo_frame.take() {
            // Returning to where we were: the frame being left goes back on the
            // ring, so stepping back again lands on it rather than skipping it.
            self.remember_shown_frame();
            self.show_frame(frame, ctx);
            return;
        }
        // No held-over frame: the decoder's queue is the only source, and it
        // hands the frame over through the normal upload path.
        self.engine.step_frame(1);
    }

    /// Put a remembered frame back on screen and move the playhead to it.
    fn show_frame(&mut self, frame: ShownFrame, ctx: &Context) {
        let image = frame.image;
        self.uploaded_pts = frame.pts;
        self.uploaded_serial = frame.serial;
        self.uploaded_size = (image.size[0] as u32, image.size[1] as u32);
        self.engine.set_playhead(frame.pts);
        match &mut self.texture {
            Some(texture) => texture.set(Arc::clone(&image), TextureOptions::LINEAR),
            None => {
                self.texture = Some(ctx.load_texture(
                    "mvp-frame",
                    Arc::clone(&image),
                    TextureOptions::LINEAR,
                ));
            }
        }
        self.displayed = Some(image);
    }

    /// Arm or clear the A–B loop.
    pub fn toggle_ab_loop(&mut self) {
        let now = self.engine.display_position();
        match self.engine.ab_loop() {
            None => {
                self.engine.set_ab_loop(Some((now, now)));
                self.toast(Toast::info("A–B 循环：已设置起点 A，再按一次设置终点 B"));
            }
            Some((a, b)) if (b - a).abs() < 0.2 => {
                self.engine.set_ab_loop(Some((a, now)));
                self.toast(Toast::success(format!(
                    "A–B 循环：{} → {}",
                    mvp_core::util::format_duration(a),
                    mvp_core::util::format_duration(now)
                )));
            }
            Some(_) => {
                self.engine.set_ab_loop(None);
                self.toast(Toast::info("A–B 循环已取消"));
            }
        }
    }

    /// Rotate the picture 90° clockwise (video or image).
    pub fn rotate_media(&mut self) {
        if self.mode == Mode::Image {
            self.image.rotate_cw();
            return;
        }
        self.settings.rotation = (self.settings.rotation + 90).rem_euclid(360);
        self.store.mark_dirty();
        self.toast(Toast::info(format!("旋转 {}°", self.settings.rotation)));
    }

    /// Mirror the picture horizontally.
    ///
    /// Images keep their own view state — it is reset per file, which is what
    /// you want when browsing a folder — while video and audio share the
    /// persisted setting. Both routes have to be honoured, or the menu item
    /// silently does nothing in one of the two modes.
    pub fn set_flip_h(&mut self, value: bool) {
        if self.mode == Mode::Image {
            if self.image.flip_h != value {
                self.image.toggle_flip_h();
            }
            return;
        }
        self.settings.flip_h = value;
        self.store.mark_dirty();
    }

    /// Mirror the picture vertically. See [`PlayerApp::set_flip_h`].
    pub fn set_flip_v(&mut self, value: bool) {
        if self.mode == Mode::Image {
            if self.image.flip_v != value {
                self.image.toggle_flip_v();
            }
            return;
        }
        self.settings.flip_v = value;
        self.store.mark_dirty();
    }

    /// The flip state the current mode is actually using.
    pub fn flip_h(&self) -> bool {
        if self.mode == Mode::Image {
            self.image.flip_h
        } else {
            self.settings.flip_h
        }
    }

    /// The vertical flip state the current mode is actually using.
    pub fn flip_v(&self) -> bool {
        if self.mode == Mode::Image {
            self.image.flip_v
        } else {
            self.settings.flip_v
        }
    }

    /// The rotation the current mode is actually using. See
    /// [`PlayerApp::flip_h`].
    pub fn rotation(&self) -> i32 {
        if self.mode == Mode::Image {
            self.image.rotation
        } else {
            self.settings.rotation
        }
    }

    /// Open the settings window on a named page.
    ///
    /// The overlay on its own is not enough for an entry that names a page
    /// ("字幕延迟…", "音频输出设置…"): the window then opens on whichever page was
    /// visited last, and the click reads as having done nothing. Every entry that
    /// means a page goes through here, so the pair can never be half-done — which
    /// is the shape of bug this method exists to prevent.
    pub fn open_settings(&mut self, tab: SettingsTab) {
        self.ui.open_overlay(Overlay::Settings);
        self.ui.settings_tab = tab;
    }

    /// Turn subtitle display on or off.
    ///
    /// This is a display gate, not a track choice: turning subtitles off and on
    /// again has to bring back the very track that was selected, so the engine's
    /// selection is left alone (except when the decoder was never started for
    /// this file, in which case "on" means "let the container choose").
    pub fn set_subtitles_enabled(&mut self, on: bool) {
        self.settings.subtitles_enabled = on;
        if on
            && self.engine.subtitle_track().is_none()
            && self.engine.subtitle().is_none()
            && self.has_embedded_text_subtitles()
        {
            self.engine.set_subtitle_track_auto();
        }
        self.store.mark_dirty();
    }

    /// `true` when the open file carries a text subtitle track.
    fn has_embedded_text_subtitles(&self) -> bool {
        self.engine
            .info()
            .is_some_and(|info| info.subtitles.iter().any(|stream| stream.is_text))
    }

    /// Show an embedded subtitle track, dropping any external file in its way.
    ///
    /// The engine gives an external side-car priority over embedded tracks, so
    /// without removing it the click would appear to do nothing at all.
    pub fn select_embedded_subtitle(&mut self, index: usize) {
        self.engine.set_external_subtitle(None);
        // Re-selecting the *same* index is a no-op for the demuxer, so the
        // selection is cleared first to make it notice and republish.
        self.engine.set_subtitle_track(None);
        self.engine.set_subtitle_track(Some(index));
        self.settings.subtitles_enabled = true;
        self.store.mark_dirty();
    }

    /// Drop the external subtitle file.
    ///
    /// Any embedded track that was selected underneath becomes visible again,
    /// which is what "remove the side-car" should mean.
    pub fn remove_external_subtitle(&mut self) {
        if self.engine.subtitle().is_none() {
            return;
        }
        let embedded = self.engine.subtitle_track();
        self.engine.set_external_subtitle(None);
        if let Some(index) = embedded {
            self.engine.set_subtitle_track(None);
            self.engine.set_subtitle_track(Some(index));
        }
        self.toast(Toast::info("已移除外部字幕"));
    }

    /// Keep the window on the screen, and remember where the user put it.
    ///
    /// This runs on the whole session, not just at start-up, because the screen
    /// can change underneath a running player: the resolution can drop, a
    /// remote-desktop session can resize, and the window can be dragged onto a
    /// monitor with less room than it needs. The guard only reacts when the
    /// window manager reports a *different* screen, so a window the user has
    /// sized or stretched deliberately is left exactly where they left it.
    ///
    /// The cost per frame is a handful of comparisons — the viewport values are
    /// already in the input state.
    pub fn sync_display(&mut self, ctx: &Context) {
        let (inner, outer, monitor, maximized, fullscreen) = ctx.input(|i| {
            let viewport = i.viewport();
            (
                viewport.inner_rect,
                viewport.outer_rect,
                viewport.monitor_size,
                viewport.maximized,
                viewport.fullscreen,
            )
        });

        // ---- remember the geometry for the next launch --------------------
        //
        // A maximised or fullscreen window reports the *screen's* size, not the
        // size the user chose, so neither is worth storing: what has to survive
        // is the size the window restores to when it is small again.
        let maximized = maximized.unwrap_or(false);
        let fullscreen = fullscreen.unwrap_or(false);
        if !maximized && !fullscreen {
            if let Some(inner) = inner {
                let size = [inner.width(), inner.height()];
                if size[0] > 0.0 && size[1] > 0.0 && size != self.settings.window_size {
                    self.settings.window_size = size;
                    self.store.mark_dirty();
                }
            }
            if let Some(outer) = outer {
                let position = [outer.min.x, outer.min.y];
                if self.settings.window_pos != Some(position) {
                    self.settings.window_pos = Some(position);
                    self.store.mark_dirty();
                }
            }
            if self.settings.maximized {
                self.settings.maximized = false;
                self.store.mark_dirty();
            }
        } else if maximized && !fullscreen && !self.settings.maximized {
            self.settings.maximized = true;
            self.store.mark_dirty();
        }

        // ---- keep it inside the screen ------------------------------------
        if fullscreen || maximized {
            return;
        }
        let inner = inner.map(|rect| [rect.width(), rect.height()]);
        if let Some(fit) = self
            .window_fit
            .fit(inner, monitor.map(|size| [size.x, size.y]))
        {
            // The minimum has to come down first: a window cannot be resized
            // below it, and on a screen smaller than the player's own minimum
            // the resize would otherwise be silently refused.
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(fit.min.into()));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(fit.size.into()));
        }
    }

    /// Keep the fullscreen flag in step with the real window.
    ///
    /// The window manager can leave fullscreen without asking us (a system
    /// shortcut, a remote-desktop session), and the transport button and the
    /// auto-hiding controls both read this flag.
    pub fn sync_fullscreen(&mut self, ctx: &Context) {
        let reported = ctx.input(|i| i.viewport().fullscreen);
        let Some(reported) = reported else {
            return;
        };
        if self.ui.last_reported_fullscreen == Some(reported) {
            return;
        }
        self.ui.last_reported_fullscreen = Some(reported);
        if self.ui.fullscreen != reported {
            self.ui.fullscreen = reported;
            if reported {
                self.ui.wake_controls(3.0);
            }
        }
    }

    /// Zoom whatever is on the canvas, keeping `anchor` where it is.
    ///
    /// `anchor` is the point the zoom holds still, in points measured from the
    /// centre of the canvas: the wheel passes the pointer, which is what makes
    /// Ctrl+wheel land on the part of the picture the user is looking at, and
    /// the keyboard and the menu pass `None` to zoom about the middle.
    ///
    /// Returns `true` when something actually moved — at either end of the zoom
    /// range, and with nothing on the canvas at all, it does not.
    pub fn zoom_media(&mut self, factor: f32, anchor: Option<egui::Vec2>) -> bool {
        let Some(picture) = self.ui.picture else {
            return false;
        };
        let area = picture.canvas;
        let anchor = anchor.unwrap_or(egui::Vec2::ZERO);
        if self.mode.is_image() {
            let viewport = (area.width(), area.height());
            let before = self.image.effective_scale(Some(viewport));
            self.image.zoom_by(factor, Some(viewport));
            let after = self.image.effective_scale(Some(viewport));
            if before <= 0.0 {
                return false;
            }
            let applied = after / before;
            if applied == 1.0 {
                return false;
            }
            // The viewer grows the picture about the middle of the viewport;
            // move it back so that the point under the pointer is the point
            // that stays.
            let offset = egui::Vec2::new(self.image.offset.0, self.image.offset.1);
            let moved = crate::view::anchored_pan(offset, anchor, applied);
            let size = picture.rect.size() * applied;
            self.set_image_pan(moved, size, area);
        } else {
            let base = self.ui.canvas.fitted_rect(picture.rect, area);
            if !self.ui.canvas.zoom_by(factor, anchor, base, area) {
                return false;
            }
        }
        true
    }

    /// Put the image viewer's picture `pan` away from the middle of the canvas,
    /// clamped so it cannot be dragged out of sight.
    pub fn set_image_pan(&mut self, pan: egui::Vec2, size: egui::Vec2, area: egui::Rect) {
        let pan = crate::view::clamp_pan(pan, size, area);
        self.image.offset = (pan.x, pan.y);
    }

    /// "适应窗口" for whatever is on the canvas: drop the zoom and the pan.
    ///
    /// One command for both modes on purpose. The still viewer and the video
    /// canvas keep their zoom in different places — a still keeps its fit mode
    /// and its rotation with it, a video has neither — and a key that only did
    /// half of that would look broken in the other half.
    pub fn reset_zoom(&mut self) {
        self.ui.canvas.reset();
        self.image.fit = mvp_core::FitMode::Fit;
        self.image.offset = (0.0, 0.0);
    }

    /// Advance the slideshow to the next image in the playlist.
    pub fn advance_slideshow(&mut self) {
        if self.playlist.is_empty() {
            return;
        }
        if let Some(index) = self.playlist.next_index(true) {
            self.play_index(index);
        }
    }

    /// Tick the image viewer and handle slideshow transitions.
    pub fn tick_image(&mut self, ctx: &Context) {
        if self.mode != Mode::Image {
            return;
        }
        self.image.slideshow = self.settings.slideshow_active;
        self.image.slideshow_interval = self.settings.slideshow_interval;
        if self.image.tick() {
            self.advance_slideshow();
            ctx.request_repaint();
        }
    }
}

/// Find the best side-car subtitle for `media`.
///
/// Tries the most specific names first so `movie.zh.srt` beats a bare
/// `movie.srt`: a folder with several subtitle languages should start on the
/// Chinese one, which is what the language-suffix list encodes.
fn find_sidecar_subtitle(media: &Path) -> Option<PathBuf> {
    let stem = media.file_stem()?.to_string_lossy().into_owned();
    let dir = media.parent()?;
    let languages = ["zh", "chs", "cht", "sc", "tc", "eng", "en"];
    let mut candidates: Vec<PathBuf> = Vec::new();
    for lang in languages {
        for ext in ["srt", "ass", "ssa", "vtt", "sub"] {
            candidates.push(dir.join(format!("{stem}.{lang}.{ext}")));
        }
    }
    for ext in ["srt", "ass", "ssa", "vtt"] {
        candidates.push(dir.join(format!("{stem}.{ext}")));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// `YYYYMMDD_HHMMSS`, used to name snapshots so they sort chronologically.
fn timestamp_for_filename() -> String {    // `SystemTime` has no calendar formatting in std, so derive a readable
    // stamp from the UNIX epoch with a small civil-date conversion.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let time = secs % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}{month:02}{day:02}_{:02}{:02}{:02}",
        time / 3600,
        (time % 3600) / 60,
        time % 60
    )
}

/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Extract the native window handle from the eframe creation context.
fn window_handle_from(cc: &eframe::CreationContext<'_>) -> isize {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = cc.window_handle() {
            if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                return win32.hwnd.get();
            }
        }
    }
    let _ = cc;
    0
}

/// Colour used by the OSD for a toast kind.
pub fn toast_color(theme: &Theme, kind: ToastKind) -> egui::Color32 {
    match kind {
        ToastKind::Info => theme.tokens.text,
        ToastKind::Success => theme.tokens.success,
        ToastKind::Warning => theme.tokens.warning,
        ToastKind::Error => theme.tokens.danger,
    }
}

impl eframe::App for PlayerApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.handle_ipc();
        self.handle_engine_events(ctx);
        self.apply_window_chrome();
        self.sync_display(ctx);
        self.sync_fullscreen(ctx);
        self.update_sleep_blocker();
        self.sync_engine();
        self.tick_image(ctx);
        // A minimized window has no picture to update, and a frame that is
        // pulled, converted and uploaded anyway is work nobody will ever see —
        // at 4K that is tens of megabytes a frame, sixty times a second, for a
        // strip of taskbar. The engine carries on decoding on its own threads
        // and the sound keeps playing; when the window comes back the next pull
        // takes the frame that is due then, not the one from before it was
        // hidden.
        if !crate::ui::window_hidden(ctx) {
            // Timed as a pair, and timed around the *whole* hand-off rather than
            // around `TextureHandle::set`: for a still this is where the pixels
            // are converted out of RGBA, and that conversion is the expensive
            // half of showing a large image. The GPU upload itself happens
            // later, inside egui's paint, and is already covered by the render
            // timing.
            let handoff_started = std::time::Instant::now();
            self.update_frame_texture(ctx);
            self.update_image_texture(ctx);
            self.ui
                .present
                .record_handoff(handoff_started.elapsed().as_secs_f32() * 1000.0);
        }
        self.ui(ctx);
        self.update_window_title(ctx);

        if !self.first_frame_painted {
            self.first_frame_painted = true;
            self.ui.startup_ms = self.process_start.elapsed().as_secs_f32() * 1000.0;
            log::info!("首帧渲染耗时 {:.1} ms", self.ui.startup_ms);
        }

        if self.store.is_dirty() {
            let document = self.settings_to_persist();
            self.store.flush_if_due(&document);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Deliberately small: the window is already going away, and anything
        // expensive here is time the user spends staring at a frozen frame. The
        // settings write is one small file (the session was already flushed
        // within the last two seconds) and the IPC thread exits on a posted
        // message, so this stays in the low single-digit milliseconds.
        self.remember_current_position();
        self.save_session();
        if let Some(instance) = &self.instance {
            instance.shutdown();
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = self.theme.tokens.bg;
        [
            c.r() as f32 / 255.0,
            c.g() as f32 / 255.0,
            c.b() as f32 / 255.0,
            1.0,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_match_known_values() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
    }

    #[test]
    fn snapshot_file_names_are_sortable() {
        let name = timestamp_for_filename();
        assert_eq!(name.len(), 15, "YYYYMMDD_HHMMSS");
        assert!(name.starts_with("20"));
        assert_eq!(&name[8..9], "_");
    }
}
