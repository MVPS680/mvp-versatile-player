//! The transport bar: seek bar, timestamps and every playback control.

use egui::containers::menu::MenuButton;
use egui::{Align, Context, Layout, Rect, RichText, Ui};

use crate::app::PlayerApp;
use crate::icons::Icon;
use crate::state::{Overlay, SettingsTab, Toast};
use crate::theme::{font, radius, space, Tokens};
use crate::ui::surface;
use crate::ui::widgets;

/// Height of the video transport bar, in points.
///
/// The seek row and the 2 pt gap under it are 24 pt, the play button is the tallest control
/// in the row below it, and the panel frame keeps 8 pt of air above and below both. That is
/// 24 + 46 + 16 = 86 for the current play button, so the height the bar was designed at
/// still holds — with 2 pt to spare. `theme::button::PLAY` can move a little inside it; past
/// 48 pt it cannot.
const VIDEO_BAR_HEIGHT: f32 = 88.0;

/// The docked transport bar under the video.
pub fn draw_video(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    // An opaque panel. The hairline that separates it from the picture is egui's own
    // panel separator line, drawn on the edge that faces content.
    let margin = egui::Margin::symmetric(space::MD as i8, space::SM as i8);

    egui::TopBottomPanel::bottom("mvp_transport_video")
        .frame(surface::bar_shell(&tokens, margin))
        .exact_height(VIDEO_BAR_HEIGHT)
        .show(ctx, |ui| {
            seek_row(app, ui, &tokens);
            ui.add_space(2.0);
            video_button_row(app, ui, &tokens);
        });
}

/// A floating bar drawn over the video in fullscreen.
pub fn draw_video_overlay(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    // The floating bar follows the screen rather than a fixed 720 pt: on a
    // narrow display it has to shrink, not hang off both edges of the picture.
    let width = crate::layout::Metrics::of(ctx).transport_overlay_width();
    // The island is the one transport surface drawn *over* the picture rather than
    // above it, so it is an opaque sheet: it has to stay readable over a white frame.
    // Its radius is one step smaller than a sheet's — a wide, short bar wants less
    // rounding than a tall panel.
    const OVERLAY_RADIUS: f32 = crate::theme::radius::LG;
    let margin = egui::Margin::symmetric(space::LG as i8, space::SM as i8);
    egui::Area::new(egui::Id::new("mvp_transport_overlay"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
        // `Middle`, not `Foreground`. egui resolves a click layer by layer from
        // the top, so a `Foreground` island wins every overlapping pixel against
        // the settings window and the dialogs — which are plain `Window`s, and a
        // `Window` lives in `Middle` (see egui's own note on `Window::order`, which
        // suggests `Foreground` for *windows* that must stay on top, i.e. the
        // opposite of what this used to do). The island has to be above the
        // picture, which is in the `Background` layer, and below the windows; the
        // toast keeps its `Foreground` painter, so it stays above both.
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            surface::sheet_shell(&tokens, margin, OVERLAY_RADIUS).show(ui, |ui| {
                ui.set_width(width);
                seek_row(app, ui, &tokens);
                ui.add_space(2.0);
                video_button_row(app, ui, &tokens);
            });
        });
}

/// The docked transport bar for the audio screen.
///
/// The same timeline as the video bar, but the control row is built around
/// listening rather than watching: a wide volume slider, a speed menu, an
/// output-device picker and a lyrics toggle. Aspect, rotation, snapshots and
/// single-frame stepping are not offered at all, because there is no picture
/// for any of them to act on.
pub fn draw_audio(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    let margin = egui::Margin::symmetric(space::MD as i8, space::SM as i8);

    egui::TopBottomPanel::bottom("mvp_transport_audio")
        .frame(surface::bar_shell(&tokens, margin))
        // Unchanged: the audio bar's tallest control is an icon button, and 96 pt already
        // has room for one of those plus the seek row.
        .exact_height(96.0)
        .show(ctx, |ui| {
            seek_row(app, ui, &tokens);
            ui.add_space(2.0);
            audio_button_row(app, ui, &tokens);
        });
}

/// The floating audio transport in fullscreen.
pub fn draw_audio_overlay(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    let width = crate::layout::Metrics::of(ctx).transport_overlay_width();
    let margin = egui::Margin::symmetric(space::LG as i8, space::SM as i8);
    egui::Area::new(egui::Id::new("mvp_transport_audio_overlay"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            surface::sheet_shell(&tokens, margin, crate::theme::radius::LG).show(ui, |ui| {
                ui.set_width(width);
                seek_row(app, ui, &tokens);
                ui.add_space(2.0);
                audio_button_row(app, ui, &tokens);
            });
        });
}

/// The audio screen's control row.
fn audio_button_row(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let playing = app.engine.is_playing();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;

        // ---- transport -----------------------------------------------------
        if widgets::tool_button(tokens, ui, Icon::Previous, "上一曲 (P)", app.has_previous()).clicked()
        {
            app.prev_media();
        }
        if widgets::tool_button(tokens, ui, Icon::Rewind, "后退 10 秒 (←)", true).clicked() {
            app.seek_relative(-10.0);
        }
        let ended = app.engine.state() == mvp_core::PlaybackState::Ended;
        let (icon, tip) = if playing {
            (Icon::Pause, "暂停 (空格)")
        } else if ended {
            (Icon::Play, "重播 (空格)")
        } else {
            (Icon::Play, "播放 (空格)")
        };
        if crate::icons::primary_transport_button(
            ui,
            icon,
            crate::theme::button::PLAY,
            tokens.accent,
            tokens.accent_hover,
            tokens.on_accent,
        )
        .on_hover_text(tip)
        .clicked()
        {
            app.engine.toggle_pause();
        }
        if widgets::tool_button(tokens, ui, Icon::Forward, "前进 10 秒 (→)", true).clicked() {
            app.seek_relative(10.0);
        }
        if widgets::tool_button(tokens, ui, Icon::Next, "下一曲 (N)", app.has_next()).clicked() {
            app.next_media(false);
        }

        ui.add_space(space::MD);

        // ---- volume --------------------------------------------------------
        let muted = app.settings.muted || app.settings.volume <= 0.0;
        let volume_icon = if muted {
            Icon::VolumeMute
        } else if app.settings.volume < 0.5 {
            Icon::VolumeLow
        } else {
            Icon::VolumeHigh
        };
        if widgets::tool_button(
            tokens,
            ui,
            volume_icon,
            if muted { "取消静音 (M)" } else { "静音 (M)" },
            true,
        )
        .clicked()
        {
            app.settings.muted = !app.settings.muted;
            app.store.mark_dirty();
        }
        // The slider is the first optional control and the last to be given up:
        // on an audio screen it is not a convenience, it is the point.
        if ui.available_width() > 320.0 {
            if let Some(value) = widgets::volume_slider(ui, tokens, app.settings.volume) {
                app.settings.volume = value;
                if value > 0.0 {
                    app.settings.muted = false;
                }
                app.store.mark_dirty();
            }
        }
        if ui.available_width() > 300.0 {
            let percent = (app.settings.volume * 100.0).round() as i32;
            ui.add_sized(
                egui::vec2(38.0, 20.0),
                egui::Label::new(
                    RichText::new(format!("{percent}%"))
                        .size(font::TINY)
                        .color(tokens.text_weak),
                )
                .selectable(false),
            );
        }

        // ---- speed ---------------------------------------------------------
        if ui.available_width() > 300.0 {
            speed_menu(app, ui, tokens);
        }

        // ---- right-hand controls -------------------------------------------
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = space::SM;

            if widgets::tool_button(
                tokens,
                ui,
                if app.ui.fullscreen {
                    Icon::ExitFullscreen
                } else {
                    Icon::Fullscreen
                },
                "全屏 (F)",
                true,
            )
            .clicked()
            {
                app.toggle_fullscreen(ui.ctx());
            }
            if widgets::tool_button(tokens, ui, Icon::Settings, "设置 (Ctrl+,)", true).clicked() {
                app.ui.open_overlay(Overlay::Settings);
            }
            if widgets::toggle_tool_button(
                tokens,
                ui,
                Icon::Playlist,
                "播放队列 (Ctrl+L)",
                app.ui.sidebar_visible,
                true,
            )
            .clicked()
            {
                app.ui.sidebar_visible = !app.ui.sidebar_visible;
                app.store.mark_dirty();
            }

            // The rest are optional: they are given up on a narrow window in
            // this order, so the picture-less screen still centres its record.
            if ui.available_width() > 110.0 {
                let lyrics_on = app.settings.subtitles_enabled && app.engine.subtitle().is_some();
                if widgets::toggle_tool_button(
                    tokens,
                    ui,
                    Icon::Lyrics,
                    "歌词 / 字幕 (V)",
                    lyrics_on,
                    true,
                )
                .clicked()
                {
                    app.set_subtitles_enabled(!app.settings.subtitles_enabled);
                    app.toast(Toast::info(if app.settings.subtitles_enabled {
                        "歌词已开启"
                    } else {
                        "歌词已关闭"
                    }));
                }
            }
            if ui.available_width() > 110.0 {
                device_menu(app, ui, tokens);
            }
            if ui.available_width() > 110.0
                && widgets::toggle_tool_button(
                    tokens,
                    ui,
                    Icon::AbLoop,
                    "A–B 循环 (B)",
                    app.engine.ab_loop().is_some(),
                    true,
                )
                .clicked()
            {
                app.toggle_ab_loop();
            }
            if ui.available_width() > 110.0
                && widgets::toggle_tool_button(
                    tokens,
                    ui,
                    Icon::Shuffle,
                    "随机播放 (H)",
                    app.settings.shuffle,
                    true,
                )
                .clicked()
            {
                app.settings.shuffle = !app.settings.shuffle;
                app.store.mark_dirty();
            }
            if ui.available_width() > 110.0
                && widgets::toggle_tool_button(
                    tokens,
                    ui,
                    if app.settings.repeat == mvp_core::playlist::RepeatMode::One {
                        Icon::RepeatOne
                    } else {
                        Icon::Repeat
                    },
                    "循环模式 (C)",
                    app.settings.repeat != mvp_core::playlist::RepeatMode::Off,
                    true,
                )
                .clicked()
            {
                app.settings.repeat = app.settings.repeat.next();
                app.store.mark_dirty();
                app.toast(Toast::info(format!(
                    "循环模式 · {}",
                    crate::state::repeat_label(app.settings.repeat)
                )));
            }
        });
    });
}

/// The speed picker, shared by the audio row and the video row.
///
/// The button carries its own label — the speed in `1.00x` form — and nothing
/// else. A speedometer glyph used to be drawn just before it, on a rectangle
/// that took no clicks of its own (`Sense::hover`): it was pure decoration on a
/// control that already says what it is, so it is gone rather than bound to the
/// menu. Every icon left on the bar is a button.
fn speed_menu(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let speed_text = format!("{:.2}x", app.settings.speed);
    MenuButton::new(
        RichText::new(speed_text.as_str())
            .size(font::SMALL)
            .color(if (app.settings.speed - 1.0).abs() > 1e-3 {
                tokens.accent
            } else {
                tokens.text_weak
            }),
    )
    .ui(ui, |ui| {
        ui.set_width(150.0);
        for speed in [0.25f64, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0] {
            let selected = (app.settings.speed - speed).abs() < 1e-3;
            if ui
                .selectable_label(selected, RichText::new(format!("{speed:.2}x")).size(font::SMALL))
                .clicked()
            {
                app.settings.speed = speed;
                app.store.mark_dirty();
                ui.close();
            }
        }
        ui.separator();
        if ui.button(RichText::new("恢复正常速度").size(font::SMALL)).clicked() {
            app.settings.speed = 1.0;
            app.store.mark_dirty();
            ui.close();
        }
    });
}

/// The audio output picker.
///
/// The device is opened once, when the engine first starts, and never re-opened
/// — that is what keeps start-up latency low and avoids an audible click on
/// every file change — so a change here is stored and takes effect on the next
/// start. The menu says so rather than pretending otherwise.
fn device_menu(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    MenuButton::new(RichText::new("输出").size(font::SMALL)).ui(ui, |ui| {
        ui.set_min_width(240.0);
        let devices = mvp_core::audio::output_device_names();
        let default_name = mvp_core::audio::default_output_device_name()
            .unwrap_or_else(|| "未命名设备".to_string());
        if ui
            .selectable_label(
                app.settings.audio_device.is_none(),
                RichText::new(format!("系统默认（{default_name}）")).size(font::SMALL),
            )
            .clicked()
        {
            app.settings.audio_device = None;
            app.store.mark_dirty();
            ui.close();
        }
        for device in &devices {
            let selected = app.settings.audio_device.as_deref() == Some(device.as_str());
            if ui
                .selectable_label(selected, RichText::new(device).size(font::SMALL))
                .clicked()
            {
                app.settings.audio_device = Some(device.clone());
                app.store.mark_dirty();
                ui.close();
            }
        }
        ui.separator();
        ui.label(
            RichText::new("切换输出设备将在下次启动后生效")
                .size(font::TINY)
                .color(tokens.text_muted),
        );
    });
}

/// Timestamp + seek bar + duration.
///
/// The two timestamps are *measured* rather than assumed: the seek bar takes
/// exactly the room that is left over, and when there is not enough of it the
/// duration is the first thing to go. A seek bar too short to aim at is worse
/// than a missing "02:31:55", and guessing at the width — the old code reserved
/// a flat 110 pt — is what pushed the duration off the edge of a narrow window.
fn seek_row(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    /// The shortest a seek bar can be and still be draggable.
    const MIN_BAR: f32 = 96.0;
    /// Height of the timestamp labels.
    const LABEL_HEIGHT: f32 = 20.0;

    let active = app.mode.is_media();
    let duration = app.engine.duration();
    let position = app.engine.display_position();
    let preview = app.ui.seek_drag;
    // Chapters are cloned once: the bar draws a tick for each and the hover
    // readout names the one under the pointer, and asking the engine twice per
    // frame for the same list is work with no purpose.
    let chapters = app.chapters();
    let marks: Vec<f64> = chapters.iter().map(|chapter| chapter.start).collect();
    let ab_loop = app.engine.ab_loop();
    // The bar leads the clock: see [`crate::state::UiState::seek_hold`] for why.
    let shown = shown_position(app, position, preview);

    let current = mvp_core::util::format_duration(shown);
    let total = if duration > 0.0 {
        mvp_core::util::format_duration(duration)
    } else {
        "--:--".to_string()
    };
    let font_id = egui::FontId::monospace(font::SMALL);
    let measure = |text: &str| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font_id.clone(), tokens.text)
            .size()
            .x
    };
    let current_width = measure(&current).max(1.0);
    let total_width = measure(&total).max(1.0);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        ui.add_sized(
            egui::vec2(current_width, LABEL_HEIGHT),
            egui::Label::new(
                RichText::new(current.as_str())
                    .size(font::SMALL)
                    .monospace()
                    .color(if preview.is_some() {
                        tokens.accent
                    } else {
                        tokens.text
                    }),
            )
            .selectable(false),
        );

        let room = ui.available_width();
        let with_total = room - total_width - space::SM;
        let show_total = with_total >= MIN_BAR;
        let bar_width = (if show_total { with_total } else { room }).max(16.0);

        let output = ui.allocate_ui(egui::vec2(bar_width, 22.0), |ui| {
            ui.set_width(bar_width);
            widgets::seek_bar(ui, tokens, position, duration, preview, &marks, ab_loop)
        });

        let bar = output.inner;
        // The pointer over the bar is holding its handle.
        if active && bar.preview.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        // Hover readout: what is under the pointer, named by the chapter it
        // falls inside when the file has chapters. Drawn over the bar rather
        // than beside it, so the eye does not have to leave the track.
        if duration > 0.0 {
            if let (Some(hover), Some(value)) = (bar.response.hover_pos(), bar.preview) {
                let label = match chapters
                    .iter()
                    .find(|chapter| value >= chapter.start && value < chapter.end)
                {
                    Some(chapter) => format!(
                        "{} · {}",
                        mvp_core::util::format_duration(value),
                        chapter.title
                    ),
                    None => mvp_core::util::format_duration(value),
                };
                // The readout sits *above* the bar, which is taller than the strip
                // the transport panel reserved for it — and the canvas, laid out
                // after the panel, paints over everything outside that strip. A
                // `Foreground` layer painter escapes the panel's clip rect and
                // draws over the picture, the way a tooltip does, instead of being
                // cropped away to nothing.
                let painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("mvp_seek_preview"),
                ));
                let galley = painter.layout_no_wrap(
                    label,
                    egui::FontId::proportional(font::TINY),
                    tokens.text,
                );
                let pad = egui::vec2(10.0, 5.0);
                let size = galley.size() + pad * 2.0;
                let rect = bar.response.rect;
                let x = (hover.x - size.x / 2.0).clamp(rect.left(), (rect.right() - size.x).max(rect.left()));
                let bubble = Rect::from_min_size(
                    egui::pos2(x, rect.top() - size.y - 6.0),
                    size,
                );
                painter.rect_filled(
                    bubble,
                    egui::CornerRadius::same(radius::SM as u8),
                    tokens.elevated,
                );
                painter.rect_stroke(
                    bubble,
                    egui::CornerRadius::same(radius::SM as u8),
                    egui::Stroke::new(1.0_f32, tokens.border_strong),
                    egui::StrokeKind::Inside,
                );
                painter.galley(bubble.min + pad, galley, tokens.text);
            }
        }

        // While dragging, only preview; seek once on release so a slow disk is
        // not hit dozens of times per second.
        if let Some(value) = bar.released {
            if active {
                if bar.response.dragged() {
                    app.ui.seek_drag = Some(value);
                } else {
                    app.seek(value);
                    app.ui.seek_drag = None;
                }
            }
        }
        if bar.response.drag_stopped() {
            if let Some(value) = app.ui.seek_drag.take() {
                app.seek(value);
            }
        }

        if show_total {
            ui.add_sized(
                egui::vec2(total_width, LABEL_HEIGHT),
                egui::Label::new(
                    RichText::new(total.as_str())
                        .size(font::SMALL)
                        .monospace()
                        .color(tokens.text_weak),
                )
                .selectable(false),
            );
        }
    });
}

/// What the timestamps and the bar should show this frame.
///
/// The user's request outranks the engine's clock while it is outstanding. A
/// click is answered by the demuxer a moment later, and in that moment the clock
/// still reads the position the playhead is leaving; showing *that* is what made
/// a click look like it had been ignored — and made the bar and the timestamp
/// disagree with the pointer that had just set them.
///
/// A drag outranks everything: while the pointer is down, the pointer is the
/// truth. A readout that started leading the pointer mid-drag would be a bar the
/// user cannot aim with.
fn shown_position(app: &mut PlayerApp, engine: f64, drag: Option<f64>) -> f64 {
    if let Some(value) = drag {
        return value;
    }
    let Some((target, asked_at)) = app.ui.seek_hold else {
        return engine;
    };
    if (engine - target).abs() <= crate::state::SEEK_SETTLED
        || asked_at.elapsed() >= crate::state::SEEK_HOLD_LIMIT
    {
        app.ui.seek_hold = None;
        return engine;
    }
    target
}

/// Transport buttons, volume, speed and window controls.
///
/// The bar is one row of fixed-size controls, and `egui` clips a row that does
/// not fit rather than wrapping it — which is what used to pile the right-hand
/// buttons on top of the left-hand ones. So the width the row needs is worked
/// out first (see [`crate::layout::transport_budget`]) and the optional controls
/// are given up until what remains fits. The controls that are never given up —
/// previous, rewind, play, forward, next, mute, fullscreen, settings and the
/// sidebar toggle — fit the narrowest window the player opens at.
fn video_button_row(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let active = app.mode.is_media();
    let playing = app.engine.is_playing();

    // The speed menu is a text button, so its width depends on the font; it is
    // measured rather than estimated, because the budget has to be exact for
    // the two halves of the bar not to meet in the middle.
    let speed_text = format!("{:.2}x", app.settings.speed);
    let speed_label = ui
        .painter()
        .layout_no_wrap(
            speed_text.clone(),
            egui::FontId::proportional(font::SMALL),
            tokens.text,
        )
        .size()
        .x;
    let budget = crate::layout::transport_budget(ui.available_width(), speed_label);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;

        // ---- playlist navigation -----------------------------------------
        if widgets::tool_button(tokens, ui, Icon::Previous, "上一项 (P)", app.has_previous())
            .clicked()
        {
            app.prev_media();
        }
        if widgets::tool_button(
            tokens,
            ui,
            Icon::Rewind,
            "后退 5 秒 (←)",
            active,
        )
        .clicked()
        {
            app.seek_relative(-app.settings.seek_step);
        }

        // ---- play / pause -------------------------------------------------
        // A finished file has nothing to resume, so the button replays it from
        // the start; the tooltip is what tells the user that is what it does.
        let ended = app.engine.state() == mvp_core::PlaybackState::Ended;
        let (icon, tip) = if playing {
            (Icon::Pause, "暂停 (空格)")
        } else if ended {
            (Icon::Play, "重播 (空格)")
        } else {
            (Icon::Play, "播放 (空格)")
        };
        if crate::icons::primary_transport_button(
            ui,
            icon,
            crate::theme::button::PLAY,
            tokens.accent,
            tokens.accent_hover,
            tokens.on_accent,
        )
        .on_hover_text(tip)
        .clicked()
        {
            if active {
                app.engine.toggle_pause();
            } else if let Some(index) = app.playlist.current_index() {
                app.play_index(index);
            } else {
                app.request_open_file();
            }
        }

        if widgets::tool_button(tokens, ui, Icon::Forward, "前进 5 秒 (→)", active).clicked() {
            app.seek_relative(app.settings.seek_step);
        }
        if widgets::tool_button(tokens, ui, Icon::Next, "下一项 (N)", app.has_next()).clicked() {
            app.next_media(false);
        }

        ui.add_space(space::MD);

        // ---- volume --------------------------------------------------------
        let muted = app.settings.muted || app.settings.volume <= 0.0;
        let volume_icon = if muted {
            Icon::VolumeMute
        } else if app.settings.volume < 0.5 {
            Icon::VolumeLow
        } else {
            Icon::VolumeHigh
        };
        if widgets::tool_button(
            tokens,
            ui,
            volume_icon,
            if muted {
                "取消静音 (M)"
            } else {
                "静音 (M)"
            },
            true,
        )
        .clicked()
        {
            app.settings.muted = !app.settings.muted;
            app.store.mark_dirty();
        }
        if budget.volume_slider {
            if let Some(value) = widgets::volume_slider(ui, tokens, app.settings.volume) {
                app.settings.volume = value;
                if value > 0.0 {
                    app.settings.muted = false;
                }
                app.store.mark_dirty();
            }
        }
        if budget.volume_percent {
            let percent = (app.settings.volume * 100.0).round() as i32;
            // A readout, not a button: the speaker icon right next to it is the
            // mute control, and making the number clickable silently muted the
            // player for anyone who clicked the text to select it.
            ui.add_sized(
                egui::vec2(38.0, 20.0),
                egui::Label::new(
                    RichText::new(format!("{percent}%"))
                        .size(font::TINY)
                        .color(tokens.text_weak),
                )
                .selectable(false),
            );
        }

        ui.add_space(space::SM);

        // ---- speed ---------------------------------------------------------
        // A menu button rather than a hand-rolled popup: it dismisses on an
        // outside click and on Escape by itself.
        if budget.speed {
            speed_menu(app, ui, tokens);
        }

        // ---- right-hand controls -------------------------------------------
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = space::SM;

            if widgets::tool_button(
                tokens,
                ui,
                if app.ui.fullscreen {
                    Icon::ExitFullscreen
                } else {
                    Icon::Fullscreen
                },
                "全屏 (F)",
                true,
            )
            .clicked()
            {
                app.toggle_fullscreen(ui.ctx());
            }

            if widgets::tool_button(tokens, ui, Icon::Settings, "设置 (Ctrl+,)", true).clicked() {
                app.ui.open_overlay(Overlay::Settings);
            }

            if budget.snapshot
                && widgets::tool_button(tokens, ui, Icon::Snapshot, "截图 (S)", app.has_picture())
                    .clicked()
            {
                app.save_snapshot();
            }

            // ---- repeat / shuffle -------------------------------------------
            if budget.repeat
                && widgets::toggle_tool_button(
                    tokens,
                    ui,
                    if app.settings.repeat == mvp_core::playlist::RepeatMode::One {
                        Icon::RepeatOne
                    } else {
                        Icon::Repeat
                    },
                    "循环模式 (C)",
                    app.settings.repeat != mvp_core::playlist::RepeatMode::Off,
                    true,
                )
                .clicked()
            {
                app.settings.repeat = app.settings.repeat.next();
                app.store.mark_dirty();
                app.toast(Toast::info(format!(
                    "循环模式 · {}",
                    crate::state::repeat_label(app.settings.repeat)
                )));
            }
            if budget.shuffle
                && widgets::toggle_tool_button(
                    tokens,
                    ui,
                    Icon::Shuffle,
                    "随机播放 (H)",
                    app.settings.shuffle,
                    true,
                )
                .clicked()
            {
                app.settings.shuffle = !app.settings.shuffle;
                app.store.mark_dirty();
            }

            // ---- subtitles ---------------------------------------------------
            // A menu, not the on/off switch that used to stand here. A switch could only
            // say "on" or "off", while what a viewer changes mid-film is *which*
            // subtitle — and 「轨道」 next to it was already the shape for that. The glyph
            // keeps the state the switch carried: it is tinted while cues are showing.
            if budget.subtitles {
                let showing =
                    app.settings.subtitles_enabled && app.engine.subtitle().is_some();
                widgets::icon_menu_button(
                    tokens,
                    ui,
                    Icon::Subtitles,
                    18.0,
                    "字幕 · 选择轨道 / 关闭 / 加载文件 (V · G)",
                    showing,
                    |ui| subtitle_menu(app, ui),
                );
            }

            // ---- quick track pickers -----------------------------------------
            // The sidebar's track page does the same thing, but reaching it
            // means leaving the picture; these menus are for the one thing a
            // viewer actually changes mid-film.
            //
            // All three of them are glyphs rather than words: two Chinese characters
            // next to a row of 28 pt icon controls read as a different size of control
            // entirely, and the bar runs out of width long before it runs out of glyphs.
            // An earlier attempt at this was an icon drawn in its own rectangle *beside*
            // a text button, which took no clicks; here the rectangle *is* the button, so
            // the whole target opens the menu.
            if budget.tracks {
                widgets::icon_menu_button(
                    tokens,
                    ui,
                    Icon::Tracks,
                    18.0,
                    "轨道 · 选择音轨",
                    false,
                    |ui| track_menu(app, ui),
                );
            }

            // ---- fit / rotate / mirror ---------------------------------------
            if budget.pan_scan {
                widgets::icon_menu_button(
                    tokens,
                    ui,
                    Icon::Image,
                    18.0,
                    "画面 · 适应 / 旋转 / 镜像",
                    false,
                    |ui| pan_scan_menu(app, ui),
                );
            }

            // ---- A–B loop ----------------------------------------------------
            if budget.ab_loop
                && widgets::toggle_tool_button(
                    tokens,
                    ui,
                    Icon::AbLoop,
                    "A–B 循环 (B)",
                    app.engine.ab_loop().is_some(),
                    true,
                )
                .clicked()
            {
                app.toggle_ab_loop();
            }

            // ---- sidebar -----------------------------------------------------
            if widgets::toggle_tool_button(
                tokens,
                ui,
                Icon::Playlist,
                "播放列表 (Ctrl+L)",
                app.ui.sidebar_visible,
                true,
            )
            .clicked()
            {
                app.ui.sidebar_visible = !app.ui.sidebar_visible;
                app.store.mark_dirty();
            }
        });
    });
}

/// The audio track picker behind the video bar's glyph menu.
///
/// Subtitles used to be listed here too, under a second heading. They have a menu of
/// their own now — the control that used to be a plain on/off switch — and two lists of
/// the same tracks in one bar is how a viewer ends up changing the wrong one.
fn track_menu(app: &mut PlayerApp, ui: &mut Ui) {
    ui.set_min_width(220.0);
    let info = app.media_info();

    ui.label(
        RichText::new("音频")
            .size(font::TINY)
            .color(app.theme.tokens.text_muted),
    );
    match &info {
        Some(info) if !info.audio.is_empty() => {
            let current = app.engine.audio_track();
            for audio in &info.audio {
                let selected = current == Some(audio.index);
                if ui
                    .selectable_label(
                        selected,
                        RichText::new(format!(
                            "{} · {} Hz",
                            audio.display_name(),
                            audio.sample_rate
                        ))
                        .size(font::SMALL),
                    )
                    .clicked()
                {
                    app.engine.set_audio_track(Some(audio.index));
                    ui.close();
                }
            }
        }
        Some(_) => {
            ui.label(RichText::new("没有音频轨道").size(font::SMALL));
        }
        None => {
            ui.label(RichText::new("正在读取…").size(font::SMALL));
        }
    }
    if ui
        .selectable_label(
            app.engine.audio_track().is_none(),
            RichText::new("关闭声音").size(font::SMALL),
        )
        .clicked()
    {
        app.engine.set_audio_track(None);
        ui.close();
    }
}

/// The subtitle picker behind the video bar's glyph menu.
///
/// This replaced a plain on/off switch. Its two states are still here — the first entry
/// is the "off" the switch used to be, and the glyph in the bar is tinted while cues are
/// showing — but the things a viewer actually reaches for mid-film (a different track, a
/// side-car file, the delay) now live together instead of being split between a switch
/// and a menu.
fn subtitle_menu(app: &mut PlayerApp, ui: &mut Ui) {
    ui.set_min_width(200.0);
    let info = app.media_info();
    let current = app.engine.subtitle_track();
    let external = app.engine.has_external_subtitle();

    if ui
        .selectable_label(
            !app.settings.subtitles_enabled,
            RichText::new("关闭字幕").size(font::SMALL),
        )
        .clicked()
    {
        app.set_subtitles_enabled(false);
        ui.close();
        return;
    }

    match &info {
        Some(info) if !info.subtitles.is_empty() => {
            for subtitle in &info.subtitles {
                let selected = app.settings.subtitles_enabled && current == Some(subtitle.index);
                let suffix = if subtitle.is_renderable() {
                    ""
                } else {
                    "（暂不支持）"
                };
                ui.add_enabled_ui(subtitle.is_renderable(), |ui| {
                    if ui
                        .selectable_label(
                            selected,
                            RichText::new(format!("{}{suffix}", subtitle.display_name()))
                                .size(font::SMALL),
                        )
                        .clicked()
                    {
                        app.select_embedded_subtitle(subtitle.index);
                        ui.close();
                    }
                });
            }
        }
        // An external file is a track as far as the viewer is concerned, so an empty
        // embedded list is only worth a line when there is nothing else showing either.
        Some(_) if !external => {
            ui.add_enabled(
                false,
                egui::Button::new(RichText::new("（没有内嵌字幕）").size(font::SMALL)),
            );
        }
        _ => {}
    }

    ui.separator();
    if ui
        .button(RichText::new("加载字幕文件…").size(font::SMALL))
        .clicked()
    {
        app.request_open_subtitle();
        ui.close();
    }
    if ui
        .add_enabled(
            external,
            egui::Button::new(RichText::new("移除外部字幕").size(font::SMALL)),
        )
        .clicked()
    {
        app.remove_external_subtitle();
        ui.close();
    }
    // In fullscreen there is no menu bar, and this is the only way to the delay or the
    // style page without leaving the picture.
    if ui
        .button(RichText::new("字幕延迟与样式…").size(font::SMALL))
        .clicked()
    {
        app.open_settings(SettingsTab::Subtitles);
        ui.close();
    }
}

/// The aspect / rotation / mirroring menu behind the video bar's 「画面」 menu.
fn pan_scan_menu(app: &mut PlayerApp, ui: &mut Ui) {
    use crate::settings::AspectMode;

    ui.set_min_width(170.0);
    // First in the list, because it is the one entry here that is not about geometry: the
    // menu is already where people look for "make the picture look different".
    if ui
        .button(RichText::new("画面调节…").size(font::SMALL))
        .clicked()
    {
        app.open_picture_panel();
        ui.close();
    }
    ui.separator();
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
    ui.separator();
    if ui.button(RichText::new("顺时针旋转 90°").size(font::SMALL)).clicked() {
        app.settings.rotation = (app.settings.rotation + 90).rem_euclid(360);
        app.store.mark_dirty();
        ui.close();
    }
    if ui.button(RichText::new("逆时针旋转 90°").size(font::SMALL)).clicked() {
        app.settings.rotation = (app.settings.rotation - 90).rem_euclid(360);
        app.store.mark_dirty();
        ui.close();
    }
    if ui.button(RichText::new("重置旋转").size(font::SMALL)).clicked() {
        app.settings.rotation = 0;
        app.store.mark_dirty();
        ui.close();
    }
    ui.separator();
    let flip_h = app.flip_h();
    if ui
        .button(RichText::new(if flip_h { "取消水平翻转" } else { "水平翻转" }).size(font::SMALL))
        .clicked()
    {
        app.set_flip_h(!flip_h);
        ui.close();
    }
    let flip_v = app.flip_v();
    if ui
        .button(RichText::new(if flip_v { "取消垂直翻转" } else { "垂直翻转" }).size(font::SMALL))
        .clicked()
    {
        app.set_flip_v(!flip_v);
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The play button has to fit inside the bar it is drawn in.
    ///
    /// The bar is an `exact_height` panel, so anything taller than its content rectangle is
    /// clipped — and a clipped play button is exactly what one sized without doing this
    /// arithmetic looks like (the design's 48 pt scaled to 62 pt inside this 88 pt bar was
    /// drawn cut off top and bottom). This builds the real frame, the real seek row and the
    /// real button, and asks egui where the button and the panel's clip rectangle ended up.
    #[test]
    fn the_play_button_is_not_clipped_by_the_transport_bar() {
        let ctx = egui::Context::default();
        let tokens = Tokens::default();
        let mut button = Rect::NOTHING;
        let mut clip = Rect::NOTHING;

        for frame in 0..2 {
            // Built in one expression rather than assigned onto `default()`, which clippy
            // flags (and which the tests elsewhere in the crate already do).
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 720.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::TopBottomPanel::bottom("mvp_probe_transport")
                    .frame(surface::bar_shell(&tokens, egui::Margin::symmetric(12, 8)))
                    .exact_height(VIDEO_BAR_HEIGHT)
                    .show(ctx, |ui| {
                        // The seek row and the 2 pt gap under it, exactly as `draw_video`
                        // lays them out.
                        ui.allocate_space(egui::vec2(ui.available_width(), 24.0));
                        let response = crate::icons::primary_transport_button(
                            ui,
                            Icon::Play,
                            crate::theme::button::PLAY,
                            tokens.accent,
                            tokens.accent_hover,
                            tokens.on_accent,
                        );
                        if frame == 1 {
                            button = response.rect;
                            clip = ui.clip_rect();
                        }
                    });
            });
        }

        assert!(button.height() > 0.0, "the probe drew no button");
        assert!(
            clip.contains_rect(button),
            "the play button ({button:?}) sticks out of the bar's clip rectangle ({clip:?})"
        );
    }
}
