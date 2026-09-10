//! The right-hand sidebar: playlist, track pickers, chapters and file details.

use egui::{Align, Context, Layout, RichText, Ui};

use crate::app::PlayerApp;
use crate::icons::Icon;
use crate::settings::SidebarTab;
use crate::state::{Mode, Overlay, Toast};
use crate::theme::{font, radius, space, Tokens};
use crate::ui::widgets;

/// Draw the docked sidebar.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    let frame = egui::Frame::new()
        .fill(tokens.panel)
        .inner_margin(egui::Margin::symmetric(space::SM as i8, space::SM as i8))
        .stroke(egui::Stroke::new(1.0, tokens.border));

    egui::SidePanel::right("mvp_sidebar")
        .frame(frame)
        .default_width(300.0)
        .width_range(240.0..=460.0)
        .resizable(true)
        .show(ctx, |ui| {
            let tabs: Vec<&str> = SidebarTab::all().iter().map(|t| t.label()).collect();
            let active = SidebarTab::all()
                .iter()
                .position(|t| *t == app.ui.sidebar_tab)
                .unwrap_or(0);
            if let Some(index) = widgets::tab_strip(ui, &tokens, &tabs, active) {
                app.ui.sidebar_tab = SidebarTab::all()[index];
                app.store.mark_dirty();
            }
            ui.add_space(space::SM);

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match app.ui.sidebar_tab {
                    SidebarTab::Playlist => playlist_tab(app, ui, &tokens),
                    SidebarTab::Tracks => tracks_tab(app, ui, &tokens),
                    SidebarTab::Chapters => chapters_tab(app, ui, &tokens),
                    SidebarTab::Info => info_tab(app, ui, &tokens),
                });
        });
}

// ---------------------------------------------------------------------------
// Playlist
// ---------------------------------------------------------------------------

fn playlist_tab(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    // ---- toolbar ---------------------------------------------------------
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        if widgets::tool_button(tokens, ui, Icon::Plus, "添加文件", true).clicked() {
            app.request_open_file();
        }
        if widgets::tool_button(tokens, ui, Icon::Folder, "添加文件夹", true).clicked() {
            app.request_open_folder();
        }
        if widgets::tool_button(tokens, ui, Icon::Link, "添加网络地址", true).clicked() {
            app.ui.url_input.clear();
            app.ui.url_focus = true;
            app.ui.open_overlay(Overlay::OpenUrl);
        }
        if widgets::tool_button(tokens, ui, Icon::Save, "保存播放列表", !app.playlist.is_empty())
            .clicked()
        {
            app.request_save_playlist();
        }
        if widgets::tool_button(tokens, ui, Icon::Clear, "清空播放列表", !app.playlist.is_empty())
            .clicked()
        {
            app.playlist.clear();
            app.engine.stop();
            app.mode = Mode::Empty;
            app.store.mark_dirty();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::tool_button(
                tokens,
                ui,
                Icon::Refresh,
                "移除不存在的文件",
                !app.playlist.is_empty(),
            )
            .clicked()
            {
                let removed = app.playlist.prune_missing();
                app.toast(Toast::info(format!("已移除 {removed} 个无效条目")));
                app.store.mark_dirty();
            }
        });
    });

    ui.add_space(space::XS);

    // ---- shuffle / repeat summary ---------------------------------------
    ui.horizontal(|ui| {
        let count = app.playlist.len();
        ui.label(
            RichText::new(format!("共 {count} 项"))
                .size(font::TINY)
                .color(tokens.text_muted),
        );
        if app.settings.shuffle {
            widgets::chip(ui, "随机", tokens.accent);
        }
        if app.settings.repeat != mvp_core::playlist::RepeatMode::Off {
            widgets::chip(ui, crate::state::repeat_label(app.settings.repeat), tokens.accent);
        }
    });

    ui.add_space(space::XS);

    if app.playlist.is_empty() {
        widgets::empty_hint(ui, tokens, "播放列表为空\n拖入文件或点击 + 添加");
        return;
    }

    // ---- entries ---------------------------------------------------------
    let current = app.playlist.current_index();
    let mut action: Option<PlaylistAction> = None;
    let items: Vec<(usize, String, bool, Option<f64>)> = app
        .playlist
        .items()
        .iter()
        .enumerate()
        .map(|(i, item)| (i, item.title.clone(), item.is_url, item.duration))
        .collect();

    let row_height = 34.0;
    for (index, title, is_url, duration) in items {
        let selected = current == Some(index);
        let (rect, response, painter) = widgets::list_row(ui, tokens, selected, row_height);

        // Playing indicator or index.
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 16.0, rect.center().y),
            egui::Vec2::splat(14.0),
        );
        if selected {
            crate::icons::draw(
                &painter,
                icon_rect,
                if app.engine.is_playing() {
                    Icon::VolumeHigh
                } else {
                    Icon::Pause
                },
                tokens.accent,
            );
        } else {
            painter.text(
                icon_rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("{}", index + 1),
                egui::FontId::proportional(font::TINY),
                tokens.text_muted,
            );
        }

        // Title.
        painter.text(
            egui::pos2(rect.left() + 30.0, rect.center().y - 6.0),
            egui::Align2::LEFT_CENTER,
            truncate(&title, 40),
            egui::FontId::proportional(font::BODY),
            if selected { tokens.text } else { tokens.text_weak },
        );

        // Secondary line: kind icon + kind + duration.
        let (kind_icon, kind) = if is_url {
            (Icon::Link, "网络串流".to_string())
        } else {
            let path = app
                .playlist
                .items()
                .get(index)
                .map(|i| i.path())
                .unwrap_or_default();
            match mvp_core::util::classify(&path) {
                mvp_core::MediaKind::Video => (Icon::Film, "视频".to_string()),
                mvp_core::MediaKind::Audio => (Icon::Music, "音频".to_string()),
                mvp_core::MediaKind::Image => (Icon::Image, "图片".to_string()),
                mvp_core::MediaKind::Playlist => (Icon::Playlist, "列表".to_string()),
                _ => (Icon::Film, "媒体".to_string()),
            }
        };
        let subtitle = match duration {
            Some(d) if d > 0.0 => format!("{kind} · {}", mvp_core::util::format_duration(d)),
            _ => kind,
        };
        crate::icons::draw(
            &painter,
            egui::Rect::from_center_size(
                egui::pos2(rect.left() + 35.0, rect.center().y + 8.0),
                egui::Vec2::splat(11.0),
            ),
            kind_icon,
            tokens.text_muted,
        );
        painter.text(
            egui::pos2(rect.left() + 44.0, rect.center().y + 8.0),
            egui::Align2::LEFT_CENTER,
            subtitle,
            egui::FontId::proportional(font::TINY),
            tokens.text_muted,
        );

        // Per-row actions on hover.
        let hovered = response.hovered();
        if hovered {
            let mut x = rect.right() - 14.0;
            {
                let (icon, hint) = (Icon::Close, "从列表移除");
                let hit = egui::Rect::from_center_size(
                    egui::pos2(x, rect.center().y),
                    egui::Vec2::splat(24.0),
                );
                crate::icons::draw(&painter, hit.shrink(5.0), icon, tokens.text_weak);
                let click = ui.interact(
                    hit,
                    egui::Id::new(("mvp_pl_remove", index)),
                    egui::Sense::click(),
                );
                if click.clicked() {
                    action = Some(PlaylistAction::Remove(index));
                } else if click.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    click.on_hover_text(hint);
                }
                x -= 26.0;
                let up = egui::Rect::from_center_size(
                    egui::pos2(x, rect.center().y),
                    egui::Vec2::splat(24.0),
                );
                crate::icons::draw(&painter, up.shrink(6.0), Icon::ChevronLeft, tokens.text_weak);
                let click = ui.interact(
                    up,
                    egui::Id::new(("mvp_pl_left", index)),
                    egui::Sense::click(),
                );
                if click.clicked() && index > 0 {
                    action = Some(PlaylistAction::Move(index, index - 1));
                }
                x -= 26.0;
                let down = egui::Rect::from_center_size(
                    egui::pos2(x, rect.center().y),
                    egui::Vec2::splat(24.0),
                );
                crate::icons::draw(&painter, down.shrink(6.0), Icon::ChevronRight, tokens.text_weak);
                let click = ui.interact(
                    down,
                    egui::Id::new(("mvp_pl_right", index)),
                    egui::Sense::click(),
                );
                if click.clicked() && index + 1 < app.playlist.len() {
                    action = Some(PlaylistAction::Move(index, index + 1));
                }
            }
        }

        if response.double_clicked() {
            action = Some(PlaylistAction::Play(index));
        } else if response.clicked() && hovered {
            action = Some(PlaylistAction::Select(index));
        }

        // Right-click context menu.
        response.context_menu(|ui| {
            if ui.button("播放").clicked() {
                action = Some(PlaylistAction::Play(index));
                ui.close();
            }
            if ui.button("在资源管理器中显示").clicked() {
                if let Some(item) = app.playlist.items().get(index) {
                    if !item.is_url {
                        mvp_platform::shell::reveal_in_explorer(&item.path());
                    }
                }
                ui.close();
            }
            if ui.button("从列表移除").clicked() {
                action = Some(PlaylistAction::Remove(index));
                ui.close();
            }
            if ui.button("清空列表").clicked() {
                action = Some(PlaylistAction::Clear);
                ui.close();
            }
        });
    }

    match action {
        Some(PlaylistAction::Play(index)) => app.play_index(index),
        Some(PlaylistAction::Select(index)) => {
            app.playlist.set_current(Some(index));
            app.store.mark_dirty();
        }
        Some(PlaylistAction::Remove(index)) => {
            app.playlist.remove(index);
            app.store.mark_dirty();
        }
        Some(PlaylistAction::Move(from, to)) => {
            app.playlist.move_item(from, to);
            app.store.mark_dirty();
        }
        Some(PlaylistAction::Clear) => {
            app.playlist.clear();
            app.store.mark_dirty();
        }
        None => {}
    }
}

enum PlaylistAction {
    Play(usize),
    Select(usize),
    Remove(usize),
    Move(usize, usize),
    Clear,
}

// ---------------------------------------------------------------------------
// Tracks
// ---------------------------------------------------------------------------

fn tracks_tab(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    if app.mode == Mode::Empty {
        widgets::empty_hint(ui, tokens, "未打开任何媒体");
        return;
    }
    if app.mode == Mode::Image {
        widgets::empty_hint(ui, tokens, "图片没有音视频轨道");
        return;
    }
    let Some(info) = app.engine.info() else {
        widgets::empty_hint(ui, tokens, "正在读取轨道信息…");
        return;
    };

    widgets::section(ui, tokens, "视频轨道");
    if info.video.is_empty() {
        widgets::empty_hint(ui, tokens, "没有视频轨道");
    } else {
        for (i, video) in info.video.iter().enumerate() {
            let selected = i == 0;
            ui.horizontal(|ui| {
                widgets::chip(ui, if selected { "正在使用" } else { "备用" }, tokens.accent);
                ui.label(
                    RichText::new(format!(
                        "{} · {}x{} · {}",
                        video.codec,
                        video.width,
                        video.height,
                        mvp_core::util::format_fps(video.fps)
                    ))
                    .size(font::SMALL)
                    .color(tokens.text),
                );
            });
        }
    }

    widgets::section(ui, tokens, "音频轨道");
    if info.audio.is_empty() {
        widgets::empty_hint(ui, tokens, "没有音频轨道");
    } else {
        let current = app.engine.audio_track();
        let mut chosen: Option<Option<usize>> = None;
        for audio in &info.audio {
            let selected = current == Some(audio.index);
            if ui
                .selectable_label(
                    selected,
                    RichText::new(format!(
                        "{} · {} Hz · {}",
                        audio.display_name(),
                        audio.sample_rate,
                        audio.codec
                    ))
                    .size(font::SMALL),
                )
                .clicked()
            {
                chosen = Some(Some(audio.index));
            }
        }
        if ui
            .selectable_label(current.is_none(), RichText::new("关闭声音").size(font::SMALL))
            .clicked()
        {
            chosen = Some(None);
        }
        if let Some(index) = chosen {
            app.engine.set_audio_track(index);
        }
    }

    widgets::section(ui, tokens, "字幕轨道");
    if info.subtitles.is_empty() {
        widgets::empty_hint(ui, tokens, "没有内嵌字幕");
    } else {
        let current = app.engine.subtitle_track();
        let mut chosen: Option<Option<usize>> = None;
        for subtitle in &info.subtitles {
            let selected = current == Some(subtitle.index);
            let suffix = if subtitle.is_text {
                ""
            } else {
                "（图形字幕，暂不支持）"
            };
            if ui
                .add_enabled_ui(subtitle.is_text, |ui| {
                    ui.selectable_label(
                        selected,
                        RichText::new(format!(
                            "{}{suffix}",
                            subtitle.display_name()
                        ))
                        .size(font::SMALL),
                    )
                })
                .inner
                .clicked()
            {
                chosen = Some(Some(subtitle.index));
            }
        }
        if ui
            .selectable_label(current.is_none(), RichText::new("关闭字幕").size(font::SMALL))
            .clicked()
        {
            chosen = Some(None);
        }
        if let Some(index) = chosen {
            app.engine.set_subtitle_track(index);
        }
    }

    if app.engine.subtitle_track().is_none() && app.engine.subtitle().is_some() {
        ui.add_space(space::SM);
        widgets::key_value(ui, tokens, "外部字幕", "已加载");
        if ui.button("移除外部字幕").clicked() {
            app.engine.set_external_subtitle(None);
        }
    }
}

// ---------------------------------------------------------------------------
// Chapters
// ---------------------------------------------------------------------------

fn chapters_tab(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let Some(info) = app.engine.info() else {
        widgets::empty_hint(ui, tokens, "未打开任何媒体");
        return;
    };
    if info.chapters.is_empty() {
        widgets::empty_hint(ui, tokens, "该文件没有章节信息");
        return;
    }
    let position = app.engine.display_position();
    let mut seek_to: Option<f64> = None;
    for (i, chapter) in info.chapters.iter().enumerate() {
        let active = position >= chapter.start && position < chapter.end;
        let (rect, response, painter) = widgets::list_row(ui, tokens, active, 30.0);
        painter.text(
            egui::pos2(rect.left() + space::SM, rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{}. {}", i + 1, chapter.title),
            egui::FontId::proportional(font::SMALL),
            if active { tokens.accent } else { tokens.text },
        );
        painter.text(
            egui::pos2(rect.right() - space::SM, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            mvp_core::util::format_duration(chapter.start),
            egui::FontId::proportional(font::TINY),
            tokens.text_muted,
        );
        if response.clicked() {
            seek_to = Some(chapter.start);
        }
    }
    if let Some(time) = seek_to {
        app.engine.seek(time);
    }
}

// ---------------------------------------------------------------------------
// Information
// ---------------------------------------------------------------------------

fn info_tab(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    if app.ui.info_rows.is_empty() {
        widgets::empty_hint(ui, tokens, "暂无媒体信息");
        return;
    }
    widgets::section(ui, tokens, "媒体信息");
    let rows: Vec<(String, String)> = app
        .ui
        .info_rows
        .iter()
        .map(|r| (r.label.clone(), r.value.clone()))
        .collect();
    for (label, value) in rows {
        widgets::key_value(ui, tokens, &label, &value);
    }

    if app.settings.show_statistics {
        widgets::section(ui, tokens, "性能统计");
        let snapshot = app.engine.snapshot_state();
        let rows = [
            ("状态", crate::state::state_label(&snapshot.state).to_string()),
            (
                "时钟来源",
                if snapshot.audio_clock {
                    "音频设备"
                } else {
                    "系统时钟"
                }
                .to_string(),
            ),
            ("已解码帧", snapshot.decoded_frames.to_string()),
            ("丢弃帧", snapshot.dropped_frames.to_string()),
            ("队列帧数", snapshot.queued_frames.to_string()),
            (
                "音频缓冲",
                format!("{:.2} 秒", snapshot.audio_queue_seconds),
            ),
            ("音频欠载", snapshot.underruns.to_string()),
            ("丢弃音频块", snapshot.dropped_audio.to_string()),
            (
                "启动耗时",
                format!("{:.0} 毫秒", app.ui.startup_ms),
            ),
        ];
        for (label, value) in rows {
            widgets::key_value(ui, tokens, label, &value);
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A small scrollbar styling hook used by the sidebar's list areas.
#[allow(dead_code)]
fn list_scroll_area() -> egui::ScrollArea {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .id_salt("mvp_list_scroll")
}

#[allow(dead_code)]
const SIDEBAR_RADIUS: f32 = radius::SM;

