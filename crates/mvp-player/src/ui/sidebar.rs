//! The right-hand sidebar: playlist, track pickers, chapters and file details.

use egui::{Align, Context, Layout, RichText, Ui};

use crate::app::PlayerApp;
use crate::icons::Icon;
use crate::settings::SidebarTab;
use crate::state::{Mode, Overlay, Toast};
use crate::theme::{font, space, Tokens};
use crate::ui::surface;
use crate::ui::widgets;

/// Draw the docked sidebar.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    // An opaque panel: the hairline along its left edge — the one facing the picture
    // — is egui's own panel separator line.
    let margin = egui::Margin::symmetric(space::SM as i8, space::SM as i8);
    let frame = surface::bar_shell(&tokens, margin);

    // The sidebar is a third of the interface on a wide screen and a nuisance
    // on a narrow one: a fixed 460 pt ceiling would leave a 720 pt window with
    // no picture left at all, so both its starting width and its limits follow
    // the window.
    let metrics = crate::layout::Metrics::of(ctx);
    let (min_width, max_width) = metrics.sidebar_width_range();

    egui::SidePanel::right("mvp_sidebar")
        .frame(frame)
        .default_width(metrics.sidebar_default_width())
        .width_range(min_width..=max_width)
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
    /// Space the hover actions (remove, move up, move down) occupy at the right
    /// end of a row, so a title is never painted underneath them.
    const ACTIONS_WIDTH: f32 = 84.0;
    /// Where the row's text column starts, and how big the kind glyph above it is.
    ///
    /// Every x in a row is derived from these two, so the index, the title and the
    /// kind caption line up down the whole list instead of drifting by the pixel
    /// or two each version: the kind glyph is aligned with the *first* character
    /// of the title, which is what makes the two lines read as one entry.
    const TEXT_X: f32 = 30.0;
    const SUBTITLE_ICON: f32 = 11.0;

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
            app.ui.playlist_selection = None;
            app.engine.stop();
            app.mode = Mode::Empty;
            app.store.mark_dirty();
        }
        // Six buttons already fill the sidebar at its narrowest, so the prune
        // button only appears when there is really room for it. Dropping it is
        // what keeps the toolbar aligned: a `right_to_left` layout given less
        // space than it needs draws its contents over what came before it, and
        // the prune button has a menu entry to fall back on.
        if ui.available_width() >= 34.0 {
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
                    app.ui.playlist_selection = None;
                    app.toast(Toast::info(format!("已移除 {removed} 个无效条目")));
                    app.store.mark_dirty();
                }
            });
        }
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
    let selection = app.ui.playlist_selection;
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
        // "Playing" and "selected" are two different things: a single click
        // moves the highlight, and only a double click (or the context menu)
        // starts the file. Conflating them made the window title announce a
        // file that was not the one playing.
        let playing = current == Some(index);
        let highlighted = selection.map_or(playing, |selected| selected == index);
        let (rect, response, painter) = widgets::list_row(ui, tokens, highlighted, row_height);

        // Playing indicator or index.
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 16.0, rect.center().y),
            egui::Vec2::splat(14.0),
        );
        if playing {
            // A play mark, in the accent, for the entry the player has open.
            //
            // This used to be a *pause* mark whenever the file was running, which
            // turned a state indicator into something that looked like a button —
            // and then changed its mind the moment playback paused. The row is not
            // clickable there, so it must not promise an action: it says "this is
            // the one that is open", and it says it the same way whatever the
            // transport is doing.
            crate::icons::draw(&painter, icon_rect, Icon::Play, tokens.accent);
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
        //
        // Painted, not laid out, so the ellipsis has to be produced here rather
        // than by a `Label`: the line is measured against the room the row really
        // has and shortened with a `…`, and the clip rectangle is only a backstop
        // for the one case measurement cannot cover — a glyph wider than the whole
        // column. `clipped_line` is what makes this exact for CJK too: the previous
        // version divided the column by a guessed nine points per character, so a
        // Chinese title lost its tail long before it had to.
        let title_room = ((rect.right() - ACTIONS_WIDTH) - (rect.left() + TEXT_X)).max(8.0);
        let title_color = if highlighted {
            tokens.text
        } else {
            tokens.text_weak
        };
        let title_galley = widgets::clipped_line(
            ui,
            &title,
            egui::FontId::proportional(font::BODY),
            title_color,
            title_room,
        );
        let title_offset = title_galley.size().y / 2.0;
        painter.with_clip_rect(rect.intersect(painter.clip_rect())).galley(
            egui::pos2(rect.left() + TEXT_X, rect.center().y - 6.0 - title_offset),
            title_galley,
            title_color,
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
                egui::pos2(rect.left() + TEXT_X + SUBTITLE_ICON / 2.0, rect.center().y + 8.0),
                egui::Vec2::splat(SUBTITLE_ICON),
            ),
            kind_icon,
            tokens.text_muted,
        );
        painter.with_clip_rect(rect.intersect(painter.clip_rect())).text(
            egui::pos2(
                rect.left() + TEXT_X + SUBTITLE_ICON + space::XS,
                rect.center().y + 8.0,
            ),
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
                crate::icons::draw(&painter, up.shrink(6.0), Icon::ChevronUp, tokens.text_weak);
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
                crate::icons::draw(&painter, down.shrink(6.0), Icon::ChevronDown, tokens.text_weak);
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
        // Selection only: moving the playlist's "current" entry here would make
        // the player claim it is playing a file it never opened.
        Some(PlaylistAction::Select(index)) => {
            app.ui.playlist_selection = Some(index);
        }
        Some(PlaylistAction::Remove(index)) => {
            app.playlist.remove(index);
            app.ui.playlist_selection = None;
            app.store.mark_dirty();
        }
        Some(PlaylistAction::Move(from, to)) => {
            app.playlist.move_item(from, to);
            // The indices shifted, so the highlight no longer means the row the
            // user picked.
            app.ui.playlist_selection = None;
            app.store.mark_dirty();
        }
        Some(PlaylistAction::Clear) => {
            app.playlist.clear();
            app.ui.playlist_selection = None;
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
        // A skeleton with the shape of the answer, rather than a line of text
                // saying that an answer is coming: the rows below are where the tracks
                // themselves will appear.
                for index in 0..3 {
                    widgets::skeleton_row(
                        ui,
                        tokens,
                        ui.available_width(),
                        14.0,
                        ui.input(|i| i.time) + f64::from(index) * 0.22,
                    );
                    ui.add_space(space::SM);
                }
        return;
    };

    widgets::section(ui, tokens, "视频轨道");
    if info.video.is_empty() {
        widgets::empty_hint(ui, tokens, "没有视频轨道");
    } else {
        // Informational only: the engine decodes the first video stream and has
        // no way to switch, so the rows must not look like a picker.
        for (i, video) in info.video.iter().enumerate() {
            let primary = i == 0;
            ui.horizontal(|ui| {
                widgets::chip(ui, if primary { "正在使用" } else { "未使用" }, tokens.accent);
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
        if info.video.len() > 1 {
            ui.label(
                RichText::new("暂不支持切换视频轨道，播放时始终使用第一条视频流")
                    .size(font::TINY)
                    .color(tokens.text_muted),
            );
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
            // Display can be off while a track stays selected, so the row has to
            // be highlighted only when it is both chosen *and* shown.
            let selected = app.settings.subtitles_enabled && current == Some(subtitle.index);
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
            .selectable_label(
                !app.settings.subtitles_enabled || current.is_none(),
                RichText::new("关闭字幕").size(font::SMALL),
            )
            .clicked()
        {
            chosen = Some(None);
        }
        match chosen {
            // Picking an embedded track has to push any external file out of the
            // way, otherwise the click would change nothing at all.
            Some(Some(index)) => app.select_embedded_subtitle(index),
            Some(None) => app.set_subtitles_enabled(false),
            None => {}
        }
    }

    if app.engine.subtitle_track().is_none() && app.engine.subtitle().is_some() {
        ui.add_space(space::SM);
        widgets::key_value(ui, tokens, "外部字幕", "已加载");
        if ui.button("移除外部字幕").clicked() {
            app.remove_external_subtitle();
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
        app.seek(time);
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
                // What the sound card is actually being fed: the interface's
                // own volume can differ from it while muted, and showing only
                // the slider's value is how a volume control that reached
                // nothing stayed "working" for so long.
                "输出增益",
                format!(
                    "{:.0}%{}",
                    app.engine.effective_gain() * 100.0,
                    if app.settings.muted { "（静音）" } else { "" }
                ),
            ),
            (
                // How far back single-frame stepping can go, and what that
                // history costs in memory — it is a byte-budgeted ring, so the
                // number is not always 12.
                "可回退帧",
                format!(
                    "{} 帧 · {:.0} MB",
                    app.frame_history.len(),
                    app.frame_history.bytes() as f64 / (1024.0 * 1024.0)
                ),
            ),
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
