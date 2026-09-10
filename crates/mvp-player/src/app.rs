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

use crate::settings::{Settings, SettingsStore};
use crate::state::{InfoRow, Mode, Toast, ToastKind, UiState};
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

/// Everything the player is and knows.
pub struct PlayerApp {
    /// The media engine, shared with the IPC thread.
    pub engine: Arc<Engine>,
    /// Persisted settings.
    pub settings: Settings,
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
    /// Pixel size of the frame currently uploaded.
    pub uploaded_size: (u32, u32),
    /// The exact image currently on screen, kept so a snapshot never has to ask
    /// the engine for pixels it has already given away.
    pub displayed: Option<Arc<egui::ColorImage>>,

    /// The single-instance guard, held for the lifetime of the process.
    pub instance: Option<AppInstance>,
    /// Messages forwarded from other instances.
    pub ipc_rx: crossbeam_channel::Receiver<IpcMessage>,

    /// Native window handle, used for the dark title bar and taskbar hints.
    pub hwnd: isize,
    /// Whether the dark title bar has been applied yet.
    pub titlebar_applied: bool,
    /// Keeps the display awake while playing.
    pub sleep_blocker: SleepBlocker,

    /// Size the video is being scaled to, so the engine can decode at that size.
    pub video_target: (u32, u32),
    /// Pixel rect the video occupies this frame, used for aspect-correct seeking.
    pub last_video_rect: egui::Rect,

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
        if let Some(volume) = args.volume {
            settings.volume = volume;
        }
        if let Some(speed) = args.speed {
            settings.speed = speed.clamp(0.25, 4.0);
        }
        if args.fullscreen {
            settings.start_fullscreen = true;
        }
        if args.no_autoplay {
            settings.autoplay = false;
        }

        let theme = Theme::default();
        theme.install(&cc.egui_ctx);

        let engine_config = EngineConfig {
            hardware_decoding: settings.hardware_decoding,
            audio_enabled: !args.no_audio,
            audio_device: settings.audio_device.clone(),
            volume: settings.volume,
            speed: settings.speed,
            autoplay: settings.autoplay,
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
            store: SettingsStore::default(),
            theme,
            playlist,
            image: ImageView::new(),
            mode: Mode::Empty,
            ui: UiState::default(),
            texture: None,
            uploaded_serial: 0,
            uploaded_size: (0, 0),
            displayed: None,
            instance,
            ipc_rx,
            hwnd,
            titlebar_applied: false,
            sleep_blocker: SleepBlocker::new(false),
            video_target: (0, 0),
            last_video_rect: egui::Rect::NOTHING,
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
            app.open_paths(&args.files, true, app.settings.autoplay);
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
    /// `autoplay` is the user's "start playing when a file is opened"
    /// preference. It only ever applies here, to a file the player was told to
    /// open; anything the user picks *inside* the player (double-clicking a
    /// playlist entry, next/previous, the end of a file) is an explicit "play
    /// this" and always plays.
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
    /// Selecting an entry is an instruction to play it, so this never consults
    /// the autoplay preference: a user who turned that off asked for files
    /// opened *for* them to wait, not for their own clicks to be ignored.
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
                    self.texture = None;
                    self.displayed = None;
                    self.uploaded_serial = 0;
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
        // Restart each file at a clean slate.
        self.engine.set_speed(self.settings.speed);
        self.engine.set_volume(self.settings.volume);
        self.engine.set_muted(self.settings.muted);
        self.engine.set_audio_delay(self.settings.audio_delay);
        self.engine.set_subtitle_delay(self.settings.subtitle_delay);
        self.engine.set_subtitle_track(None);
        self.engine.set_external_subtitle(None);
        self.engine.set_ab_loop(None);
        self.mode = Mode::Media;
        self.image.close();
        self.texture = None;
        self.displayed = None;
        self.uploaded_serial = 0;
        self.info_source = None;
        self.last_duration = 0.0;
        self.engine
            .set_target_size(self.video_target.0, self.video_target.1);

        if let Some(path) = local {
            self.load_sidecar_subtitle(&path);
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
        self.engine.seek(position);
        self.toast(Toast::info(format!(
            "从 {} 继续播放",
            mvp_core::util::format_duration(position)
        )));
    }

    /// Look for `name.srt` / `name.ass` / `name.vtt` next to `media`.
    fn load_sidecar_subtitle(&mut self, media: &Path) {
        if !self.settings.autoload_sidecar_subtitles || !self.settings.subtitles_enabled {
            return;
        }
        let Some(stem) = media.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            return;
        };
        let Some(dir) = media.parent() else { return };
        // Try the most specific names first so `movie.zh.srt` beats `movie.srt`.
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
        for candidate in candidates {
            if candidate.is_file() {
                self.load_subtitle_file(&candidate);
                return;
            }
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

        self.upload_image(
            ctx,
            egui::ColorImage {
                size,
                source_size: egui::vec2(size[0] as f32, size[1] as f32),
                pixels,
            },
        );
        self.uploaded_serial = serial;
        self.uploaded_size = dimensions;
    }

    /// Upload the current image-viewer frame.
    pub fn update_image_texture(&mut self, ctx: &Context) {
        if self.mode != Mode::Image {
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
                    self.apply_resume_position(&info);
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
        self.playlist.set_repeat(self.settings.repeat);
        self.playlist.set_shuffle(self.settings.shuffle);
    }

    /// Save the session so the next launch can restore it.
    pub fn save_session(&mut self) {
        debug_assert!(self.store.is_dirty());
        self.settings.playlist = self.playlist.items().to_vec();
        self.settings.playlist_index = self.playlist.current_index();
        self.settings.sidebar_visible = self.ui.sidebar_visible;
        self.settings.sidebar_tab = self.ui.sidebar_tab;
        self.store.mark_dirty();
        self.store.flush(&self.settings);
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

    /// Apply the native title bar tint once the window exists.
    pub fn apply_window_chrome(&mut self) {
        if self.titlebar_applied || self.hwnd == 0 {
            return;
        }
        if self.settings.dark_title_bar {
            mvp_platform::shell::set_dark_titlebar(self.hwnd, true);
            let t = &self.theme.tokens;
            mvp_platform::shell::set_caption_color(
                self.hwnd,
                [t.bg.r(), t.bg.g(), t.bg.b()],
                [t.text.r(), t.text.g(), t.text.b()],
                [t.border.r(), t.border.g(), t.border.b()],
            );
        }
        self.titlebar_applied = true;
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
        self.settings.always_on_top = !self.settings.always_on_top;
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            if self.settings.always_on_top {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            },
        ));
        self.store.mark_dirty();
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

    /// Zoom the image viewer, keeping the pointer anchored if it is over the
    /// canvas.
    pub fn zoom_image(&mut self, factor: f32, ctx: &Context) {
        let viewport = ctx.input(|i| i.screen_rect());
        self.image
            .zoom_by(factor, Some((viewport.width(), viewport.height())));
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
        self.update_sleep_blocker();
        self.sync_engine();
        self.tick_image(ctx);
        self.update_frame_texture(ctx);
        self.update_image_texture(ctx);
        self.ui(ctx);
        self.update_window_title(ctx);

        if !self.first_frame_painted {
            self.first_frame_painted = true;
            self.ui.startup_ms = self.process_start.elapsed().as_secs_f32() * 1000.0;
            log::info!("首帧渲染耗时 {:.1} ms", self.ui.startup_ms);
        }

        self.store.flush_if_due(&self.settings);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
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
