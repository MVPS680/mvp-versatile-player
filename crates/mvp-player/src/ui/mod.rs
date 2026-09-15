//! The interface: layout, shared widgets, keyboard handling and overlays.
//!
//! Layout follows the convention every desktop media player has converged on,
//! because familiarity *is* usability here:
//!
//! ```text
//!  ┌──────────────────────────────────────────────────────────┐
//!  │ 文件  播放  视频  音频  字幕  工具  帮助                     │  menu bar
//!  ├───────────────────────────────────┬──────────────────────┤
//!  │                                   │                      │
//!  │            video / image          │      sidebar         │
//!  │                                   │  playlist / tracks   │
//!  ├───────────────────────────────────┴──────────────────────┤
//!  │ 00:12:04  ─────────●───────────  02:31:55   ⏮ ⏯ ⏭  🔊 ▣  │  transport
//!  └──────────────────────────────────────────────────────────┘
//!  ```

mod canvas;
mod dialogs;
mod menu;
mod settings_window;
mod sidebar;
mod transport;
mod widgets;

use egui::{Context, Key, Modifiers};

use crate::app::PlayerApp;
use crate::state::{Mode, Overlay};

/// How often the audio screen asks for a frame while it plays.
///
/// 30 frames a second is smooth enough for a clock and a progress ring, and it
/// is a third of what the video path falls back to when no frame is due.
const AUDIO_FRAME_SECONDS: f64 = 1.0 / 30.0;

/// How often the interface asks for a frame while a file is still opening.
///
/// Nothing on screen moves then — the point is only to be there when it starts
/// to, which is within a few tens of milliseconds for a local file.
const OPENING_POLL_MS: u64 = 100;

/// Paint one frame of the whole interface.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    handle_dropped_files(app, ctx);
    handle_keyboard(app, ctx);
    handle_pointer_idle(app, ctx);
    app.ui.tick_toast();

    menu::draw(app, ctx);

    // The sidebar takes its width from the central panel, so it is laid out
    // first. In fullscreen it still docks to the right, over the picture.
    if app.ui.sidebar_visible {
        sidebar::draw(app, ctx);
    }

    canvas::draw(app, ctx);

    if !app.ui.fullscreen {
        transport::draw(app, ctx);
    } else if app.ui.controls_visible() {
        transport::draw_overlay(app, ctx);
    }

    app.ui.error_banner = error_banner(app, ctx);

    settings_window::draw(app, ctx);
    dialogs::draw(app, ctx);

    if let Some(toast) = &app.ui.toast {
        widgets::draw_toast(app, ctx, toast);
    }

    if app.ui.close_requested {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    schedule_repaint(app, ctx);
}

/// Ask for another frame at the right moment instead of spinning at 60 fps.
fn schedule_repaint(app: &PlayerApp, ctx: &Context) {
    // An open dialog must not stop the clock: playback continues behind the
    // settings window, and the end of a file still has to advance the playlist.
    match app.mode {
        Mode::Media => {
            if app.engine.state() == mvp_core::PlaybackState::Opening {
                // A file that is still opening has produced no frame *and* no
                // event yet — the `Opened` event is what the audio screen takes
                // its title and its length from, and the event is only ever
                // read while a frame is being drawn. Nothing else asks for a
                // frame in the meantime, so the interface has to keep asking,
                // or it stays on 「正在打开…」 with a clock reading 0:00 until
                // something else happens to move the window.
                ctx.request_repaint_after(std::time::Duration::from_millis(OPENING_POLL_MS));
            } else if app.engine.is_playing() {
                // An audio file has no frames to keep up with. Its screen moves
                // only as fast as the clock, the ring and the seek bar do, so it
                // asks for a third of the frames a picture needs — the video
                // path's "no frame due yet" fallback is 8 ms, and repainting at
                // that rate for a file that will never produce a frame is a
                // wake-up every 8 ms for the whole of the album.
                let delay = if app.is_audio_only() {
                    AUDIO_FRAME_SECONDS
                } else {
                    app.engine.time_until_next_frame().unwrap_or(0.008)
                };
                let delay = delay.clamp(0.001, 0.05);
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(delay));
            } else if app.engine.state().is_active() {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
        Mode::Image => {
            if app.image.wants_animation() {
                let delay = app
                    .image
                    .time_to_next_frame()
                    .unwrap_or(std::time::Duration::from_millis(33));
                ctx.request_repaint_after(delay);
            }
        }
        Mode::Empty => {}
    }
}

/// Accept files dropped onto the window.
fn handle_dropped_files(app: &mut PlayerApp, ctx: &Context) {
    let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .filter_map(|f| f.path.clone())
            .collect()
    });
    if dropped.is_empty() {
        return;
    }
    let replace = !app.engine.state().is_active();
    app.open_paths(&dropped, replace, true);
    ctx.request_repaint();
}

/// Keep the controls on screen while the pointer is moving, and hide them in
/// fullscreen once it has been still for a while.
fn handle_pointer_idle(app: &mut PlayerApp, ctx: &Context) {
    let pointer = ctx.input(|i| i.pointer.hover_pos());
    if pointer != app.ui.last_pointer_pos {
        app.ui.last_pointer_pos = pointer;
        app.ui.last_pointer_move = std::time::Instant::now();
        if app.ui.fullscreen {
            app.ui.wake_controls(app.settings.hide_controls_after.max(1.0));
        }
    }
}

/// Keyboard shortcuts.
///
/// Text fields win: if any widget wants keyboard input, the global shortcuts step
/// aside so typing a URL works normally.
fn handle_keyboard(app: &mut PlayerApp, ctx: &Context) {
    if ctx.wants_keyboard_input() {
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            app.ui.close_overlay();
        }
        return;
    }

    // Collect the key presses first: `ctx.input` takes a lock we must not hold
    // while mutating the app.
    let keys: Vec<(Key, Modifiers)> = ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => Some((*key, *modifiers)),
                _ => None,
            })
            .collect()
    });

    // A widget that currently holds focus owns the keyboard: without this, a
    // focused slider would move *and* change the volume when an arrow key is
    // pressed. Our own buttons hand focus back after a click, so the common
    // "click play, then hit space" flow still works.
    let owns_keyboard = ctx.memory(|m| m.focused()).is_some();

    for (key, modifiers) in keys {
        let ctrl = modifiers.ctrl || modifiers.command;
        let shift = modifiers.shift;
        app.ui.wake_controls(3.0);

        // Escape and fullscreen always work; everything else defers to whatever
        // has focus.
        let always = matches!(key, Key::Escape | Key::F1);
        let fullscreen_key = key == Key::F && !ctrl;
        if owns_keyboard && !always && !fullscreen_key {
            continue;
        }

        match (key, ctrl, shift) {
            (Key::Space, false, _) => app.engine.toggle_pause(),
            (Key::K, false, _) => app.engine.toggle_pause(),
            (Key::Escape, _, _) => {
                if app.ui.has_overlay() {
                    app.ui.close_overlay();
                } else if app.ui.fullscreen {
                    app.toggle_fullscreen(ctx);
                }
            }
            (Key::F, false, _) => app.toggle_fullscreen(ctx),
            (Key::Enter, false, _) if modifiers.alt => app.toggle_fullscreen(ctx),
            (Key::ArrowLeft, false, false) => app.seek_relative(-app.settings.seek_step),
            (Key::ArrowRight, false, false) => app.seek_relative(app.settings.seek_step),
            (Key::ArrowLeft, false, true) => {
                app.seek_relative(-app.settings.seek_step_large)
            }
            (Key::ArrowRight, false, true) => {
                app.seek_relative(app.settings.seek_step_large)
            }
            (Key::ArrowUp, false, _) => {
                app.settings.volume = (app.settings.volume + 0.05).min(2.0);
                app.settings.muted = false;
                app.store.mark_dirty();
                app.toast(crate::state::Toast::info(format!(
                    "音量 {}%",
                    (app.settings.volume * 100.0).round() as i32
                )));
            }
            (Key::ArrowDown, false, _) => {
                app.settings.volume = (app.settings.volume - 0.05).max(0.0);
                app.store.mark_dirty();
                app.toast(crate::state::Toast::info(format!(
                    "音量 {}%",
                    (app.settings.volume * 100.0).round() as i32
                )));
            }
            (Key::M, false, _) => {
                app.settings.muted = !app.settings.muted;
                app.store.mark_dirty();
                app.toast(crate::state::Toast::info(if app.settings.muted {
                    "静音"
                } else {
                    "取消静音"
                }));
            }
            (Key::S, false, _) => app.save_snapshot(),
            (Key::N, false, _) | (Key::PageDown, false, _) => app.next_media(false),
            (Key::P, false, _) | (Key::PageUp, false, _) => app.prev_media(),
            (Key::O, true, false) => app.request_open_file(),
            (Key::O, true, true) => app.request_open_folder(),
            (Key::U, true, _) => {
                app.ui.url_input.clear();
                app.ui.url_focus = true;
                app.ui.open_overlay(Overlay::OpenUrl);
            }
            (Key::Comma, true, _) => app.ui.open_overlay(Overlay::Settings),
            (Key::L, true, _) => {
                app.ui.sidebar_visible = !app.ui.sidebar_visible;
                app.store.mark_dirty();
            }
            (Key::OpenBracket, false, _) => {
                let speed = (app.settings.speed - 0.1).clamp(0.25, 4.0);
                app.settings.speed = speed;
                app.store.mark_dirty();
                app.toast(crate::state::Toast::info(format!("速度 {speed:.2}x")));
            }
            (Key::CloseBracket, false, _) => {
                let speed = (app.settings.speed + 0.1).clamp(0.25, 4.0);
                app.settings.speed = speed;
                app.store.mark_dirty();
                app.toast(crate::state::Toast::info(format!("速度 {speed:.2}x")));
            }
            (Key::Backslash, false, _) => {
                app.settings.speed = 1.0;
                app.store.mark_dirty();
                app.toast(crate::state::Toast::info("速度 1.00x"));
            }
            (Key::Z, false, _) => {
                let modes = crate::settings::AspectMode::all();
                let current = modes
                    .iter()
                    .position(|m| *m == app.settings.aspect)
                    .unwrap_or(0);
                app.settings.aspect = modes[(current + 1) % modes.len()];
                app.store.mark_dirty();
                app.toast(crate::state::Toast::info(format!(
                    "画面比例 · {}",
                    app.settings.aspect.label()
                )));
            }
            (Key::Comma, false, _) => app.step_back_frame(ctx),
            (Key::Period, false, _) => app.step_forward_frame(ctx),
            (Key::V, false, _) => {
                let on = !app.settings.subtitles_enabled;
                app.set_subtitles_enabled(on);
                app.toast(crate::state::Toast::info(if on {
                    "字幕已开启"
                } else {
                    "字幕已关闭"
                }));
            }
            (Key::C, false, _) => {
                app.settings.repeat = app.settings.repeat.next();
                app.store.mark_dirty();
                app.toast(crate::state::Toast::info(format!(
                    "循环模式 · {}",
                    crate::state::repeat_label(app.settings.repeat)
                )));
            }
            (Key::H, false, _) => {
                app.settings.shuffle = !app.settings.shuffle;
                app.store.mark_dirty();
                app.toast(crate::state::Toast::info(if app.settings.shuffle {
                    "随机播放已开启"
                } else {
                    "随机播放已关闭"
                }));
            }
            (Key::B, false, _) => app.toggle_ab_loop(),
            (Key::A, false, _) => app.toggle_always_on_top(ctx),
            (Key::T, false, _) => {
                app.ui.sidebar_visible = !app.ui.sidebar_visible;
                app.store.mark_dirty();
            }
            (Key::G, false, _) => app.request_open_subtitle(),
            (Key::R, false, _) => app.rotate_media(),
            (Key::Num0, false, _) => {
                app.image.fit = mvp_core::FitMode::Fit;
                app.image.offset = (0.0, 0.0);
            }
            (Key::Num1, false, _) => app.image.zoom_original(),
            (Key::Plus, false, _) | (Key::Equals, false, _) => app.zoom_image(1.25, ctx),
            (Key::Minus, false, _) => app.zoom_image(0.8, ctx),
            (Key::W, true, _) => {
                app.save_session();
                app.ui.close_requested = true;
            }
            (Key::F1, false, _) => app.ui.open_overlay(Overlay::Shortcuts),
            _ => {}
        }
    }
    app.sync_engine();
}

/// The dismissible error strip under the menu bar.
fn error_banner(app: &mut PlayerApp, ctx: &Context) -> Option<String> {
    let message = app.ui.error_banner.clone()?;
    let mut dismissed = false;
    egui::TopBottomPanel::top("mvp_error_banner")
        .frame(
            egui::Frame::new()
                .fill(app.theme.tokens.danger.gamma_multiply(0.18))
                .inner_margin(egui::Margin::symmetric(12, 6))
                .stroke(egui::Stroke::new(
                    1.0,
                    app.theme.tokens.danger.gamma_multiply(0.5),
                )),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                crate::icons::draw(
                    ui.painter(),
                    egui::Rect::from_center_size(
                        ui.cursor().min + egui::vec2(9.0, ui.available_height() / 2.0),
                        egui::Vec2::splat(16.0),
                    ),
                    crate::icons::Icon::Info,
                    app.theme.tokens.danger,
                );
                ui.add_space(22.0);
                ui.label(
                    egui::RichText::new(&message)
                        .color(app.theme.tokens.text)
                        .size(crate::theme::font::SMALL),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(egui::Button::new("关闭").frame(false))
                        .clicked()
                    {
                        dismissed = true;
                    }
                });
            });
        });
    if dismissed {
        None
    } else {
        Some(message)
    }
}
