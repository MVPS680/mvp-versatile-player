//! The main menu bar: 文件 / 播放 / 视频 / 音频 / 字幕 / 工具 / 帮助.
//!
//! A menu bar is not the most modern pattern, but it is what users of VLC,
//! PotPlayer and MPC-HC reach for, it exposes every command without hunting for
//! an icon, and it documents the keyboard shortcut next to each action.

use egui::containers::menu::{MenuBar, MenuButton};
use egui::{Align, Context, Layout, RichText, Ui};

use crate::app::PlayerApp;
use crate::settings::{AspectMode, EndAction, Settings, SidebarTab};
use crate::state::{Overlay, Toast};
use crate::theme::{font, space, Tokens};

/// Render the menu bar.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    let frame = egui::Frame::new()
        .fill(tokens.panel)
        .inner_margin(egui::Margin::symmetric(space::SM as i8, 2))
        .stroke(egui::Stroke::new(1.0, tokens.border));

    egui::TopBottomPanel::top("mvp_menu_bar")
        .frame(frame)
        .exact_height(32.0)
        .show(ctx, |ui| {
            MenuBar::new().ui(ui, |ui| {
                file_menu(app, ui, &tokens);
                playback_menu(app, ui, &tokens);
                video_menu(app, ui, &tokens);
                audio_menu(app, ui, &tokens);
                subtitle_menu(app, ui, &tokens);
                tools_menu(app, ui, &tokens);
                help_menu(app, ui, &tokens);

                // Right-aligned status: the current file and playback state.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let state = app.engine.state();
                    let label = crate::state::state_label(&state);
                    let color = match crate::state::state_kind(&state) {
                        crate::state::ToastKind::Success => tokens.success,
                        crate::state::ToastKind::Error => tokens.danger,
                        _ => tokens.text_weak,
                    };
                    ui.label(RichText::new(label).size(font::SMALL).color(color));
                    if let Some(title) = app.now_playing_label() {
                        ui.label(
                            RichText::new(truncate(&title, 48))
                                .size(font::SMALL)
                                .color(tokens.text_weak),
                        );
                    }
                });
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
        if item(ui, tokens, "截图并保存", "S", app.mode != crate::state::Mode::Empty) {
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
    MenuButton::new("最近打开").ui(ui, |ui| {
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
        if item(ui, tokens, "下一帧", ".", active) {
            app.step_forward_frame(ui.ctx());
        }
        separator(ui, tokens);
        // Speed submenu.
        MenuButton::new("播放速度").ui(ui, |ui| {
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
        MenuButton::new("循环模式").ui(ui, |ui| {
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
        let active = app.mode.is_media() || app.mode.is_image();
        MenuButton::new("画面比例").ui(ui, |ui| {
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
        if item(ui, tokens, "适应窗口", "0", app.mode.is_image()) {
            app.image.fit = mvp_core::FitMode::Fit;
            app.image.offset = (0.0, 0.0);
        }
        if item(ui, tokens, "原始大小 100%", "1", app.mode.is_image()) {
            app.image.zoom_original();
        }
        if item(ui, tokens, "放大", "+", app.mode.is_image()) {
            app.image.zoom_by(1.25, None);
        }
        if item(ui, tokens, "缩小", "-", app.mode.is_image()) {
            app.image.zoom_by(0.8, None);
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
        MenuButton::new("音频延迟").ui(ui, |ui| {
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
        if item(ui, tokens, "音频输出设置…", "", true) {
            app.ui.open_overlay(Overlay::Settings);
            app.ui.settings_tab = crate::state::SettingsTab::Audio;
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
        MenuButton::new("字幕延迟").ui(ui, |ui| {
            for delta in [-2.0f64, -1.0, -0.5, -0.1, 0.0, 0.1, 0.5, 1.0, 2.0] {
                let label = if delta == 0.0 {
                    "0 秒".to_string()
                } else {
                    format!("{delta:+.1} 秒")
                };
                if ui
                    .selectable_label(
                        (app.settings.subtitle_delay - delta).abs() < 1e-6,
                        RichText::new(label).size(font::SMALL),
                    )
                    .clicked()
                {
                    app.settings.subtitle_delay = delta;
                    app.store.mark_dirty();
                    ui.close();
                }
            }
        });
        separator(ui, tokens);
        if item(ui, tokens, "字幕样式设置…", "", true) {
            app.ui.open_overlay(Overlay::Settings);
            app.ui.settings_tab = crate::state::SettingsTab::Subtitles;
        }
    });
}

fn tools_menu(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    MenuButton::new("工具").ui(ui, |ui| {
        if item(ui, tokens, "显示/隐藏侧边栏", "Ctrl+L", true) {
            app.ui.sidebar_visible = !app.ui.sidebar_visible;
            app.store.mark_dirty();
        }
        MenuButton::new("侧边栏标签页").ui(ui, |ui| {
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
            app.ui.open_overlay(Overlay::Settings);
            app.ui.settings_tab = crate::state::SettingsTab::Integration;
        }
        separator(ui, tokens);
        MenuButton::new("播放结束行为").ui(ui, |ui| {
            for (action, label) in [
                (EndAction::Playlist, "按播放列表继续"),
                (EndAction::Hold, "停留在最后一帧"),
                (EndAction::Close, "关闭播放器"),
            ] {
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
        .line_segment([rect.left_top(), rect.right_top()], egui::Stroke::new(1.0, tokens.border));
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
}
