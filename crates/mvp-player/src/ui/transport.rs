//! The transport bar: seek bar, timestamps and every playback control.

use egui::containers::menu::MenuButton;
use egui::{Align, Context, Layout, RichText, Ui};

use crate::app::PlayerApp;
use crate::icons::Icon;
use crate::state::{Overlay, Toast};
use crate::theme::{font, space, Tokens};
use crate::ui::glass;
use crate::ui::widgets;

/// The docked transport bar under the video.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    // Liquid Glass: the frame carries no fill of its own, because the material is
    // painted below and it has to be able to see through to the window behind it.
    // The rim is cut on the top edge — the one facing the picture.
    let margin = egui::Margin::symmetric(space::MD as i8, space::SM as i8);
    let material = glass::Glass::chrome(glass::Rim::TOP);

    egui::TopBottomPanel::bottom("mvp_transport")
        .frame(glass::chrome_shell(margin))
        .exact_height(84.0)
        .show(ctx, |ui| {
            // Behind the content: the fill and the rim belong under the controls.
            glass::paint_ui(ui, &tokens, glass::surface_rect(ui, margin), material);
            seek_row(app, ui, &tokens);
            ui.add_space(2.0);
            button_row(app, ui, &tokens);
        });
}

/// A floating bar drawn over the video in fullscreen.
pub fn draw_overlay(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    // The floating bar follows the screen rather than a fixed 720 pt: on a
    // narrow display it has to shrink, not hang off both edges of the picture.
    let width = crate::layout::Metrics::of(ctx).transport_overlay_width();
    egui::Area::new(egui::Id::new("mvp_transport_overlay"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            // The floating bar is the one surface with the picture behind it, so it
            // is a sheet of glass — lit on all four edges, with a highlight that
            // follows the pointer across it. Its radius is one step smaller than a
            // sheet's: a wide, short bar wants less rounding than a tall panel.
            const OVERLAY_RADIUS: f32 = crate::theme::radius::LG;
            let margin = egui::Margin::symmetric(space::LG as i8, space::SM as i8);
            let material = glass::Glass::float(OVERLAY_RADIUS);
            egui::Frame::new()
                .fill(glass::base_fill(&tokens, glass::Thickness::Float))
                .corner_radius(egui::CornerRadius::same(OVERLAY_RADIUS as u8))
                .inner_margin(margin)
                .show(ui, |ui| {
                    ui.set_width(width);
                    seek_row(app, ui, &tokens);
                    ui.add_space(2.0);
                    button_row(app, ui, &tokens);
                    // Over the content: the veil, the rim and the light, painted
                    // last because that is the order light arrives in.
                    glass::paint_overlay_ui(
                        ui,
                        &tokens,
                        glass::content_rect(ui, margin),
                        material,
                    );
                });
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
    let shown = preview.unwrap_or(position);

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
            widgets::seek_bar(ui, tokens, position, duration, preview)
        });

        let bar = output.inner;
        if let Some(value) = bar.preview {
            if active {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                app.ui.video_hover_time = Some(value);
            }
        } else {
            app.ui.video_hover_time = None;
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

/// Transport buttons, volume, speed and window controls.
///
/// The bar is one row of fixed-size controls, and `egui` clips a row that does
/// not fit rather than wrapping it — which is what used to pile the right-hand
/// buttons on top of the left-hand ones. So the width the row needs is worked
/// out first (see [`crate::layout::transport_budget`]) and the optional controls
/// are given up until what remains fits. The controls that are never given up —
/// previous, rewind, play, forward, next, mute, fullscreen, settings and the
/// sidebar toggle — fit the narrowest window the player opens at.
fn button_row(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
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
            40.0,
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
            let (speed_icon, _) =
                ui.allocate_exact_size(egui::vec2(18.0, 20.0), egui::Sense::hover());
            crate::icons::draw(
                ui.painter(),
                speed_icon.shrink(2.0),
                Icon::Speed,
                if (app.settings.speed - 1.0).abs() > 1e-3 {
                    tokens.accent
                } else {
                    tokens.text_weak
                },
            );
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
                ui.separator();
                if ui
                    .button(RichText::new("恢复正常速度").size(font::SMALL))
                    .clicked()
                {
                    app.settings.speed = 1.0;
                    app.store.mark_dirty();
                    ui.close();
                }
            });
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
            let subtitle_on = app.settings.subtitles_enabled && app.engine.subtitle().is_some();
            if budget.subtitles
                && widgets::toggle_tool_button(
                    tokens,
                    ui,
                    Icon::Subtitles,
                    "字幕开关 (V) · 加载字幕 (G)",
                    subtitle_on,
                    true,
                )
                .clicked()
            {
                app.set_subtitles_enabled(!app.settings.subtitles_enabled);
                app.toast(Toast::info(if app.settings.subtitles_enabled {
                    "字幕已开启"
                } else {
                    "字幕已关闭"
                }));
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
