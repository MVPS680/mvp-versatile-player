//! The main menu bar: 文件 / 播放 / 视频 / 音频 / 字幕 / 工具 / 帮助.
//!
//! A menu bar is not the most modern pattern, but it is what users of VLC,
//! PotPlayer and MPC-HC reach for, it exposes every command without hunting for
//! an icon, and it documents the keyboard shortcut next to each action.
//!
//! **Titles use `MenuButton`; anything nested uses `SubMenuButton`.** The two are
//! visually identical and take the same arguments, which is what made this a
//! silent trap: `MenuButton` is for the bar itself, and inside a menu it merely
//! opens a popup carrying the *enclosing* menu's close-on-click behaviour — so the
//! parent menu shut on the very frame the submenu appeared, and a nested entry
//! could not be reached at all (no submenu ever stayed open, in the report's
//! words, "点了没反应"). `SubMenuButton` is the one that registers an open
//! submenu in egui's `MenuState`, ignores nearby clicks and opens on hover. Every
//! nested list here was written with `MenuButton` once; do not go back.

use egui::containers::menu::{MenuBar, MenuButton, SubMenuButton};
use egui::{Align, Context, Layout, RichText, Ui};

use crate::app::PlayerApp;
use crate::settings::{AspectMode, EndAction, Settings, SidebarTab};
use crate::state::{Overlay, SettingsTab, Toast};
use crate::theme::{font, space, Tokens};
use crate::ui::surface;

/// Render the menu bar.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    // An opaque panel. The bar is flush with the window frame, so the only edge that
    // meets anything is the bottom one — which is egui's own panel separator line.
    let margin = egui::Margin::symmetric(space::SM as i8, 2);
    let frame = surface::bar_shell(&tokens, margin);

    egui::TopBottomPanel::top("mvp_menu_bar")
        .frame(frame)
        .exact_height(32.0)
        .show(ctx, |ui| {
            // A title lights up on its own, exactly the way a button in the transport
            // bar does: nothing at rest, `tokens.hover` under the pointer, `tokens.active`
            // while the mouse is down, radius MD, and no animation of any kind.
            //
            // The bar used to slide a single stretched pill between the titles —
            // Liquid Glass as *motion* — and it read as a stray object travelling
            // through the bar rather than as the titles being touched. egui already
            // gives a menu title the first three of those states (its theme entries
            // are `tokens.hover` for both hover and open, so an open menu holds its
            // fill while the pointer is down inside it); only the *press* needs a word
            // here, because the theme paints a pressed control in the accent, and a
            // menu title is not a primary action.
            {
                let active = &mut ui.style_mut().visuals.widgets.active;
                active.weak_bg_fill = tokens.active;
                active.bg_fill = tokens.active;
                active.fg_stroke = egui::Stroke::new(1.0_f32, tokens.text);
            }

            MenuBar::new().ui(ui, |ui| {
                file_menu(app, ui, &tokens);
                playback_menu(app, ui, &tokens);
                video_menu(app, ui, &tokens);
                audio_menu(app, ui, &tokens);
                subtitle_menu(app, ui, &tokens);
                tools_menu(app, ui, &tokens);
                help_menu(app, ui, &tokens);

                // Right-aligned status: the playback state, then the file's name to
                // its left.
                //
                // Every width here is *measured* rather than guessed. A
                // `right_to_left` layout given less space than it needs draws its
                // contents *over* what came before it — which is how a long file name
                // ended up printed across the menus and the playback controls. So the
                // name is drawn only when the state label leaves room for it, it is
                // capped in width *and* in characters, and the full name stays
                // available on hover.
                let free = ui.available_width();
                if free >= 180.0 {
                    let state = app.engine.state();
                    let label = crate::state::state_label(&state);
                    let color = match crate::state::state_kind(&state) {
                        crate::state::ToastKind::Success => tokens.success,
                        crate::state::ToastKind::Error => tokens.danger,
                        _ => tokens.text_weak,
                    };
                    let label_width = ui
                        .painter()
                        .layout_no_wrap(
                            label.to_owned(),
                            egui::FontId::proportional(font::SMALL),
                            color,
                        )
                        .size()
                        .x;

                    /// Air kept between the name and the menus.
                    const GAP: f32 = 24.0;
                    /// The widest the name may get.
                    const MAX_NAME: f32 = 220.0;
                    /// Below this there is no room worth using, so the name is dropped.
                    const MIN_NAME: f32 = 72.0;
                    /// …and the longest, counted in characters rather than points.
                    const MAX_CHARS: usize = 26;

                    let room = free - label_width - space::SM - GAP;
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(label).size(font::SMALL).color(color));
                        if room >= MIN_NAME {
                            if let Some(title) = app.now_playing_label() {
                                ui.add_sized(
                                    egui::vec2(room.min(MAX_NAME), 18.0),
                                    egui::Label::new(
                                        RichText::new(truncate(&title, MAX_CHARS))
                                            .size(font::SMALL)
                                            .color(tokens.text_weak),
                                    )
                                    .truncate()
                                    .selectable(false),
                                )
                                .on_hover_text(title);
                            }
                        }
                    });
                }
            });
        });
}

fn file_menu(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    MenuButton::new("文件").ui(ui, |ui| {
        if item(ui, tokens, "打开文件…", "Ctrl+O", true) {
            app.request_open_file();
        }
        if item(ui, tokens, "打开文件夹…", "Ctrl+Shift+O", true) {
            app.request_open_folder();
        }
        if item(ui, tokens, "打开网络串流…", "Ctrl+U", true) {
            app.ui.url_input.clear();
            app.ui.url_focus = true;
            app.ui.open_overlay(Overlay::OpenUrl);
        }
        separator(ui, tokens);
        if item(ui, tokens, "加载字幕文件…", "G", app.mode.is_media()) {
            app.request_open_subtitle();
        }
        if item(ui, tokens, "保存播放列表…", "", !app.playlist.is_empty()) {
            app.request_save_playlist();
        }
        separator(ui, tokens);
        submenu_recent(app, ui, tokens);
        separator(ui, tokens);
        // A snapshot is of a *picture*, so it goes with the picture: the audio
        // screen has none, and greyed out is a better answer than an error.
        if item(ui, tokens, "截图并保存", "S", app.has_picture()) {
            app.save_snapshot();
        }
        if item(ui, tokens, "打开截图目录", "", true) {
            let dir = app.settings.snapshot_dir();
            let _ = std::fs::create_dir_all(&dir);
            mvp_platform::shell::reveal_in_explorer(&dir);
        }
        separator(ui, tokens);
        if item(ui, tokens, "退出", "Ctrl+W", true) {
            app.save_session();
            app.ui.close_requested = true;
        }
    });
}

fn submenu_recent(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    SubMenuButton::new("最近打开").ui(ui, |ui| {
        app.settings.prune_recent();
        if app.settings.recent_files.is_empty() {
            ui.add_enabled(
                false,
                egui::Button::new(RichText::new("（空）").size(font::SMALL)),
            );
            return;
        }
        let entries = app.settings.recent_files.clone();
        let mut chosen: Option<std::path::PathBuf> = None;
        let mut cleared = false;
        for path in entries {
            let label = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            let response = ui.add(
                egui::Button::new(RichText::new(truncate(&label, 56)).size(font::SMALL))
                    .frame(false),
            );
            if response.on_hover_text(path.to_string_lossy()).clicked() {
                chosen = Some(path);
                ui.close();
            }
        }
        separator(ui, tokens);
        if item(ui, tokens, "清空最近记录", "", true) {
            cleared = true;
        }
        if let Some(path) = chosen {
            app.open_paths(&[path], true, true);
        }
        if cleared {
            app.settings.recent_files.clear();
            app.store.mark_dirty();
        }
    });
}

fn playback_menu(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    MenuButton::new("播放").ui(ui, |ui| {
        let playing = app.engine.is_playing();
        let active = app.mode.is_media();
        // At the end of a file "play" means "replay", and saying so is the whole
        // difference between a user expecting playback to continue and one who
        // knows it starts over.
        let ended = app.engine.state() == mvp_core::PlaybackState::Ended;
        let label = if playing {
            "暂停"
        } else if ended {
            "重播"
        } else {
            "播放"
        };
        if item(ui, tokens, label, "空格", active) {
            app.engine.toggle_pause();
        }
        if item(ui, tokens, "停止", "", active) {
            app.engine.stop();
            app.mode = crate::state::Mode::Empty;
        }
        separator(ui, tokens);
        if item(ui, tokens, "上一项", "P", app.has_previous()) {
            app.prev_media();
        }
        if item(ui, tokens, "下一项", "N", app.has_next()) {
            app.next_media(false);
        }
        separator(ui, tokens);
        if item(ui, tokens, "快退 5 秒", "←", active) {
            app.seek_relative(-app.settings.seek_step);
        }
        if item(ui, tokens, "快进 5 秒", "→", active) {
            app.seek_relative(app.settings.seek_step);
        }
        if item(ui, tokens, "快退 30 秒", "Shift+←", active) {
            app.seek_relative(-app.settings.seek_step_large);
        }
        if item(ui, tokens, "快进 30 秒", "Shift+→", active) {
            app.seek_relative(app.settings.seek_step_large);
        }
        separator(ui, tokens);
        if item(ui, tokens, "上一帧", ",", app.can_step_back()) {
            app.step_back_frame(ui.ctx());
        }
        // Stepping forward needs something to step *through*: `active` alone
        // would offer it on the audio screen, where a step can only pause.
        if item(ui, tokens, "下一帧", ".", active && app.has_picture()) {
            app.step_forward_frame(ui.ctx());
        }
        separator(ui, tokens);
        // Speed submenu.
        SubMenuButton::new("播放速度").ui(ui, |ui| {
            for speed in [0.25f64, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0] {
                let selected = (app.settings.speed - speed).abs() < 1e-3;
                if ui
                    .selectable_label(
                        selected,
                        RichText::new(format!("{speed:.2}x")).size(font::SMALL),
                    )
                    .clicked()
                {
                    app.settings.speed = speed;
                    app.store.mark_dirty();
                    ui.close();
                }
            }
        });
        SubMenuButton::new("循环模式").ui(ui, |ui| {
            use mvp_core::playlist::RepeatMode;
            for (mode, label) in [
                (RepeatMode::Off, "不循环"),
                (RepeatMode::All, "列表循环"),
                (RepeatMode::One, "单个循环"),
            ] {
                if ui
                    .selectable_label(
                        app.settings.repeat == mode,
                        RichText::new(label).size(font::SMALL),
                    )
                    .clicked()
                {
                    app.settings.repeat = mode;
                    app.store.mark_dirty();
                    ui.close();
                }
            }
        });
        let mut shuffle = app.settings.shuffle;
        if ui
            .checkbox(&mut shuffle, RichText::new("随机播放").size(font::SMALL))
            .changed()
        {
            app.settings.shuffle = shuffle;
            app.store.mark_dirty();
        }
        separator(ui, tokens);
        let ab = app.engine.ab_loop();
        let label = match ab {
            Some((a, b)) if (b - a).abs() > 0.2 => format!(
                "取消 A–B 循环 ({} → {})",
                mvp_core::util::format_duration(a),
                mvp_core::util::format_duration(b)
            ),
            Some(_) => "设置 B 点".to_string(),
            None => "设置 A–B 循环".to_string(),
        };
        if item(ui, tokens, &label, "B", active) {
            app.toggle_ab_loop();
        }
        if ab.is_some() && item(ui, tokens, "跳转到 A 点", "", active) {
            if let Some((a, _)) = ab {
                app.seek(a);
            }
        }
    });
}

fn video_menu(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    MenuButton::new("视频").ui(ui, |ui| {
        // The picture commands apply to a video frame or to an image, and the
        // audio screen has neither — what is left in this menu is the encoding
        // and the window, which are just as real there.
        let active = app.has_picture();
        SubMenuButton::new("画面比例").ui(ui, |ui| {
            for mode in AspectMode::all() {
                if ui
                    .selectable_label(
                        app.settings.aspect == *mode,
                        RichText::new(mode.label()).size(font::SMALL),
                    )
                    .clicked()
                {
                    app.settings.aspect = *mode;
                    app.store.mark_dirty();
                    ui.close();
                }
            }
        });
        // Offered even when there is nothing to adjust — a greyed-out entry reads as "this
        // player does not have that feature", and the click answers for itself with the
        // same sentence the shortcut gives. The panel still refuses to open for a
        // photograph or a song; see `PlayerApp::open_picture_panel`.
        if item(ui, tokens, "画面调节…", "Ctrl+P", true) {
            app.open_picture_panel();
            ui.close();
        }
        if item(ui, tokens, "顺时针旋转 90°", "R", active) {
            app.rotate_media();
        }
        let mut flip_h = app.flip_h();
        if ui
            .checkbox(&mut flip_h, RichText::new("水平翻转").size(font::SMALL))
            .changed()
        {
            app.set_flip_h(flip_h);
        }
        let mut flip_v = app.flip_v();
        if ui
            .checkbox(&mut flip_v, RichText::new("垂直翻转").size(font::SMALL))
            .changed()
        {
            app.set_flip_v(flip_v);
        }
        separator(ui, tokens);
        // Zoom and pan apply to whatever is on the canvas — a still and a video
        // both — so these four are enabled whenever there is a picture, which is
        // exactly when `ui.picture` was written by the canvas.
        let picture = app.ui.picture.is_some();
        if item(ui, tokens, "适应窗口", "0", picture) {
            app.reset_zoom();
        }
        if item(ui, tokens, "原始大小 100%", "1", app.mode.is_image()) {
            app.image.zoom_original();
        }
        if item(ui, tokens, "放大", "+", picture) {
            app.zoom_media(1.25, None);
        }
        if item(ui, tokens, "缩小", "-", picture) {
            app.zoom_media(0.8, None);
        }
        separator(ui, tokens);
        if item(ui, tokens, "幻灯片播放", "", app.mode.is_image()) {
            app.settings.slideshow_active = !app.settings.slideshow_active;
            app.store.mark_dirty();
            let on = app.settings.slideshow_active;
            app.toast(Toast::info(if on { "幻灯片已开启" } else { "幻灯片已关闭" }));
        }
        separator(ui, tokens);
        let mut hw = app.settings.hardware_decoding;
        if ui
            .checkbox(&mut hw, RichText::new("硬件解码加速").size(font::SMALL))
            .on_hover_text("关闭后将完全使用 CPU 软解，适用于花屏或不兼容的显卡驱动")
            .changed()
        {
            app.settings.hardware_decoding = hw;
            app.store.mark_dirty();
            app.toast(Toast::info(if hw {
                "硬件解码已开启（重新打开文件后生效）"
            } else {
                "已切换为软件解码（重新打开文件后生效）"
            }));
        }
        separator(ui, tokens);
        if item(ui, tokens, "全屏", "F", true) {
            app.toggle_fullscreen(ui.ctx());
        }
        let mut top = app.settings.always_on_top;
        if ui
            .checkbox(&mut top, RichText::new("窗口置顶").size(font::SMALL))
            .changed()
        {
            app.set_always_on_top(ui.ctx(), top);
        }
    });
}

fn audio_menu(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    MenuButton::new("音频").ui(ui, |ui| {
        let info = app.engine.info();
        let tracks: Vec<(usize, String)> = info
            .as_ref()
            .map(|i| {
                i.audio
                    .iter()
                    .enumerate()
                    .map(|(n, a)| (a.index, format!("{} · {}", n + 1, a.display_name())))
                    .collect()
            })
            .unwrap_or_default();

        let current = app.engine.audio_track();
        if tracks.is_empty() {
            ui.add_enabled(
                false,
                egui::Button::new(RichText::new("（无音频轨道）").size(font::SMALL)),
            );
        } else {
            if ui
                .selectable_label(
                    current.is_none(),
                    RichText::new("关闭声音").size(font::SMALL),
                )
                .clicked()
            {
                app.engine.set_audio_track(None);
                ui.close();
            }
            for (index, label) in &tracks {
                let selected = current == Some(*index);
                if ui
                    .selectable_label(selected, RichText::new(label).size(font::SMALL))
                    .clicked()
                {
                    app.engine.set_audio_track(Some(*index));
                    ui.close();
                }
            }
        }
        separator(ui, tokens);
        if item(ui, tokens, "增大音量", "↑", true) {
            app.settings.volume = (app.settings.volume + 0.05).min(2.0);
            app.settings.muted = false;
            app.store.mark_dirty();
        }
        if item(ui, tokens, "减小音量", "↓", true) {
            app.settings.volume = (app.settings.volume - 0.05).max(0.0);
            app.store.mark_dirty();
        }
        if item(
            ui,
            tokens,
            if app.settings.muted { "取消静音" } else { "静音" },
            "M",
            true,
        ) {
            app.settings.muted = !app.settings.muted;
            app.store.mark_dirty();
        }
        separator(ui, tokens);
        SubMenuButton::new("音频延迟").ui(ui, |ui| {
            for delta in [-1.0f64, -0.5, -0.1, 0.0, 0.1, 0.5, 1.0] {
                let label = if delta == 0.0 {
                    "0 秒（同步）".to_string()
                } else {
                    format!("{delta:+.2} 秒")
                };
                if ui
                    .selectable_label(
                        (app.settings.audio_delay - delta).abs() < 1e-6,
                        RichText::new(label).size(font::SMALL),
                    )
                    .clicked()
                {
                    app.settings.audio_delay = delta;
                    app.store.mark_dirty();
                    ui.close();
                }
            }
        });
        separator(ui, tokens);
        // Not a settings page, on purpose: an equaliser is adjusted while you listen, so
        // this opens a panel of its own and playback keeps running. Never greyed out —
        // the parameters belong to the output device rather than to the file — and the
        // panel says plainly when there is no device for the chain to run on.
        if item(ui, tokens, "音效增强…", "", true) {
            app.open_audio_enhance();
            ui.close();
        }
        if item(ui, tokens, "音频输出设置…", "", true) {
            app.open_settings(SettingsTab::Audio);
        }
    });
}

fn subtitle_menu(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    MenuButton::new("字幕").ui(ui, |ui| {
        let info = app.engine.info();
        let tracks: Vec<(usize, String)> = info
            .as_ref()
            .map(|i| {
                i.subtitles
                    .iter()
                    .map(|s| (s.index, s.display_name()))
                    .collect()
            })
            .unwrap_or_default();
        let current = app.engine.subtitle_track();
        let external = app.engine.subtitle().is_some() && current.is_none();

        if ui
            .selectable_label(
                !app.settings.subtitles_enabled,
                RichText::new("关闭字幕").size(font::SMALL),
            )
            .clicked()
        {
            app.set_subtitles_enabled(false);
            ui.close();
        }
        if !tracks.is_empty() {
            separator(ui, tokens);
            for (index, label) in &tracks {
                let selected = app.settings.subtitles_enabled && current == Some(*index);
                if ui
                    .selectable_label(selected, RichText::new(label).size(font::SMALL))
                    .clicked()
                {
                    app.select_embedded_subtitle(*index);
                    ui.close();
                }
            }
        } else if !external {
            ui.add_enabled(
                false,
                egui::Button::new(RichText::new("（无内嵌字幕）").size(font::SMALL)),
            );
        }
        separator(ui, tokens);
        if item(ui, tokens, "加载字幕文件…", "G", true) {
            app.request_open_subtitle();
        }
        if item(ui, tokens, "移除外部字幕", "", external) {
            app.remove_external_subtitle();
        }
        separator(ui, tokens);
        // A jump to the page, not the preset submenu that used to sit here. A
        // nested `MenuButton` never opens in this menu bar (egui identifies an open
        // submenu by the auto id of its own button, and it closed again the instant
        // it opened — see the note on the end-of-playback heading in the tools
        // menu), so all nine presets were unreachable from the UI. They were also
        // never the whole story: the page carries a -10…+10 s slider, and it is the
        // only place the delay can be set to anything a preset did not cover. The
        // value on the right keeps the menu answering "what is it now?".
        if item(
            ui,
            tokens,
            "字幕延迟…",
            &delay_hint(app.settings.subtitle_delay),
            true,
        ) {
            app.open_settings(SettingsTab::Subtitles);
        }
        if item(ui, tokens, "字幕样式设置…", "", true) {
            app.open_settings(SettingsTab::Subtitles);
        }
    });
}

fn tools_menu(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    MenuButton::new("工具").ui(ui, |ui| {
        if item(ui, tokens, "显示/隐藏侧边栏", "Ctrl+L", true) {
            app.ui.sidebar_visible = !app.ui.sidebar_visible;
            app.store.mark_dirty();
        }
        SubMenuButton::new("侧边栏标签页").ui(ui, |ui| {
            for tab in SidebarTab::all() {
                if ui
                    .selectable_label(
                        app.ui.sidebar_tab == *tab,
                        RichText::new(tab.label()).size(font::SMALL),
                    )
                    .clicked()
                {
                    app.ui.sidebar_tab = *tab;
                    app.ui.sidebar_visible = true;
                    app.store.mark_dirty();
                    ui.close();
                }
            }
        });
        let mut stats = app.settings.show_statistics;
        if ui
            .checkbox(&mut stats, RichText::new("显示性能统计").size(font::SMALL))
            .changed()
        {
            app.settings.show_statistics = stats;
            app.store.mark_dirty();
        }
        separator(ui, tokens);
        if item(ui, tokens, "设置…", "Ctrl+,", true) {
            app.ui.open_overlay(Overlay::Settings);
        }
        if item(ui, tokens, "文件关联…", "", true) {
            app.open_settings(SettingsTab::Integration);
        }
        separator(ui, tokens);
        // A `SubMenuButton`, not a `MenuButton`. The two look alike in a menu, but
        // only the first is a submenu: `MenuButton` merely opens a `Popup::menu`
        // with the enclosing menu's close behaviour, so clicking it closed the whole
        // menu on the same frame the submenu appeared — and the entry (like every
        // other nested list in the bar) could not be reached at all. `SubMenuButton`
        // is the one that tracks an open submenu in `MenuState` and opens on hover.
        SubMenuButton::new("播放结束行为").ui(ui, |ui| {
            for (action, label) in EndAction::choices() {
                if ui
                    .selectable_label(
                        app.settings.end_action == action,
                        RichText::new(label).size(font::SMALL),
                    )
                    .clicked()
                {
                    app.settings.end_action = action;
                    app.store.mark_dirty();
                    ui.close();
                }
            }
        });
        ui.add_space(space::XS);
        if item(ui, tokens, "重置所有设置…", "", true) {
            app.settings = Settings::reset();
            // Replacing the document is only half of it: the values that reach
            // the engine, the window and the sidebar have to be re-applied too,
            // or the reset would only show up after a restart.
            app.apply_settings(ui.ctx());
            app.toast(Toast::info("设置已重置为默认值"));
        }
    });
}

fn help_menu(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    MenuButton::new("帮助").ui(ui, |ui| {
        if item(ui, tokens, "键盘快捷键", "F1", true) {
            app.ui.open_overlay(Overlay::Shortcuts);
        }
        if item(ui, tokens, "关于 MVP-Versatile-Player", "", true) {
            app.ui.open_overlay(Overlay::About);
        }
        separator(ui, tokens);
        if item(ui, tokens, "打开设置文件夹", "", true) {
            let dir = Settings::config_dir();
            let _ = std::fs::create_dir_all(&dir);
            mvp_platform::shell::reveal_in_explorer(&dir);
        }
        if item(ui, tokens, "查看运行日志", "", true) {
            let log = crate::log_file_path();
            if log.exists() {
                let _ = mvp_platform::shell::open_in_default_app(&log);
            } else {
                mvp_platform::shell::reveal_in_explorer(&Settings::config_dir());
            }
        }
        if item(ui, tokens, "项目主页", "", true) {
            let _ = mvp_platform::shell::open_url(
                "https://github.com/mvp-versatile-player/mvp-versatile-player",
            );
        }
    });
}

/// Right-hand readout for the "字幕延迟…" entry.
///
/// `0` reads as "0 秒" rather than "+0.00 秒": it is the value that means "off",
/// and a sign on it says nothing. Everything else uses the same two decimals the
/// settings slider prints, so the menu and the page never disagree about what is
/// set.
fn delay_hint(seconds: f64) -> String {
    if seconds.abs() < 5e-3 {
        "0 秒".to_string()
    } else {
        format!("{seconds:+.2} 秒")
    }
}

/// One menu entry with an optional shortcut hint on the right.
fn item(ui: &mut Ui, tokens: &Tokens, label: &str, shortcut: &str, enabled: bool) -> bool {
    let mut clicked = false;
    ui.add_enabled_ui(enabled, |ui| {
        let width = ui.available_width().max(190.0);
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::click());
        if response.hovered() && enabled {
            ui.painter().rect_filled(
                rect,
                egui::CornerRadius::same(4),
                tokens.hover,
            );
        }
        let text_color = if enabled { tokens.text } else { tokens.text_muted };
        ui.painter().text(
            egui::pos2(rect.left() + space::SM, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(font::BODY),
            text_color,
        );
        if !shortcut.is_empty() {
            ui.painter().text(
                egui::pos2(rect.right() - space::SM, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                shortcut,
                egui::FontId::proportional(font::TINY),
                tokens.text_muted,
            );
        }
        if response.clicked() && enabled {
            clicked = true;
            ui.close();
        }
    });
    clicked
}

fn separator(ui: &mut Ui, tokens: &Tokens) {
    ui.add_space(space::XS);
    let width = ui.available_width().max(190.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    ui.painter()
        .line_segment([rect.left_top(), rect.right_top()], egui::Stroke::new(1.0_f32, tokens.border));
    ui.add_space(space::XS);
}

/// Clip a string to `max` characters, adding an ellipsis when shortened.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("movie.mkv", 20), "movie.mkv");
    }

    #[test]
    fn truncate_adds_an_ellipsis() {
        let text = truncate("abcdefghij", 5);
        assert_eq!(text.chars().count(), 5);
        assert!(text.ends_with('…'));
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        let text = truncate("中文文件名测试", 4);
        assert_eq!(text.chars().count(), 4);
    }

    /// The menu's delay readout and the settings slider's readout are the same
    /// number in the same shape — two decimals, except that zero has no sign.
    #[test]
    fn the_delay_readout_matches_what_the_slider_prints() {
        assert_eq!(delay_hint(0.0), "0 秒");
        assert_eq!(delay_hint(-0.004), "0 秒");
        assert_eq!(delay_hint(0.5), "+0.50 秒");
        assert_eq!(delay_hint(-1.25), "-1.25 秒");
    }
}
