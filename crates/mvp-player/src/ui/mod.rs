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
mod minimap;
mod picture_panel;
mod screens;
mod settings_window;
mod sidebar;
mod surface;
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

    // ---- panel order ----------------------------------------------------
    //
    // Every panel has to claim its strip of the window *before* the central
    // panel asks for what is left, because that is how egui hands out space: the
    // central panel is whatever remains of the rectangle the panels declared
    // ahead of it have not taken. Painting the transport bar after the canvas
    // therefore did not reserve anything for it — the canvas was laid out at the
    // full window height and the picture was centred on a rectangle whose bottom
    // third then disappeared behind the bar. On screen that read as a video
    // sitting low in the frame with a wide black band above it and almost none
    // below, which is what "the picture is not centred" always is.
    //
    // So: top panels, then the side panel, then the bottom bar, then the canvas.
    menu::draw(app, ctx);

    // The banner belongs to the top group: it pushes the canvas down rather than
    // being drawn across the top of the picture.
    app.ui.error_banner = error_banner(app, ctx);

    // Minimized: keep the clock, and do none of the expensive work.
    //
    // This sits immediately before the screen because the canvas is where the
    // frame is pulled from the engine and uploaded to the texture — the two costs
    // that made a night in the background into a locked-up window (and into a
    // frame that queues behind thousands nobody saw). Audio carries on
    // regardless: it runs on its own thread, which is what keeps a minimized
    // player playing.
    if window_hidden(ctx) {
        app.ui.tick_toast();
        ctx.request_repaint_after(HIDDEN_POLL);
        return;
    }
    // Timed from here, not from the top of the frame: this is the half of the
    // frame the program controls — pulling the frame from the engine, uploading
    // it, and painting — and it is the half that answers "why does it feel slow".
    let render_started = std::time::Instant::now();

    // Each media kind has its own screen, and each screen owns the panel order
    // that makes it work: sidebar, then its own bottom strip, then the canvas.
    screens::draw(app, ctx);

    // The enhancement's statistics, at most ten times a second. Off unless the setting
    // is on, and then it reads the frame the upload path already holds: no extra
    // decode, no second copy of the picture.
    app.update_picture_state();

    // The floating island is the *only* transport in fullscreen and it is an
    // `Area`, not a panel: it reserves nothing, so it is painted over the canvas
    // rather than before it.
    if app.ui.fullscreen && app.ui.controls_visible() {
        screens::draw_overlay(app, ctx);
    }

    // The picture panel is a window, not an overlay: it must not dim the frame it is
    // being used to judge, so it is painted with the settings window, after the
    // picture, and it never takes the keyboard.
    picture_panel::draw(app, ctx);

    settings_window::draw(app, ctx);
    dialogs::draw(app, ctx);

    if let Some(toast) = &app.ui.toast {
        widgets::draw_toast(app, ctx, toast);
    }

    if app.ui.close_requested {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    note_frame(render_started.elapsed());
    schedule_repaint(app, ctx);
}

/// How often the interface wakes up while its window is minimized.
///
/// A minimized window still gets frames if anything asks for them, and this player
/// asks for one every 8 ms while a film is playing. Left in the background
/// overnight that is a whole night of decoding and texture uploads for a surface
/// nobody can see. Twice a second is enough to notice the window coming back.
const HIDDEN_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// `true` when the window is minimized, so nothing can be seen.
///
/// Shared with the frame loop in `PlayerApp::update`: a minimized window must
/// not pull a frame out of the engine, convert it and hand it to a texture
/// nobody can look at.
pub(crate) fn window_hidden(ctx: &Context) -> bool {
    ctx.input(|input| input.viewport().minimized.unwrap_or(false))
}

/// The half of the frame this program controls, in milliseconds.
///
/// Stored as `f32` bits in atomics because the frame loop and the statistics
/// panel are different functions and neither owns the other; there is exactly one
/// writer, so relaxed loads are enough to read a number that is only ever read for
/// display.
static LAST_FRAME_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static AVERAGE_FRAME_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static WORST_FRAME_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static SLOW_FRAMES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A frame slower than this is a stall rather than a slow frame.
const SLOW_FRAME_MS: f32 = 100.0;

/// How long the last frame's render half took, in milliseconds.
pub fn frame_ms() -> f32 {
    f32::from_bits(LAST_FRAME_MS.load(std::sync::atomic::Ordering::Relaxed))
}

/// An exponentially weighted average of the render half, in milliseconds.
pub fn average_frame_ms() -> f32 {
    f32::from_bits(AVERAGE_FRAME_MS.load(std::sync::atomic::Ordering::Relaxed))
}

/// The worst render half since the player started, in milliseconds.
pub fn worst_frame_ms() -> f32 {
    f32::from_bits(WORST_FRAME_MS.load(std::sync::atomic::Ordering::Relaxed))
}

/// Record how long the render half of one frame took.
///
/// Measurements, not guesses: "the player feels slow" is only worth acting on if
/// the numbers say where the time goes, and a run of slow frames is logged — every
/// sixtieth one, so a stalled graphics driver cannot flood the log — because that
/// is the evidence a freeze report needs.
fn note_frame(elapsed: std::time::Duration) {
    use std::sync::atomic::Ordering;

    let ms = elapsed.as_secs_f32() * 1000.0;
    LAST_FRAME_MS.store(ms.to_bits(), Ordering::Relaxed);

    let previous = average_frame_ms();
    let average = if previous <= 0.0 {
        ms
    } else {
        previous * 0.9 + ms * 0.1
    };
    AVERAGE_FRAME_MS.store(average.to_bits(), Ordering::Relaxed);

    if ms > worst_frame_ms() {
        WORST_FRAME_MS.store(ms.to_bits(), Ordering::Relaxed);
    }

    if ms >= SLOW_FRAME_MS {
        let count = SLOW_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
        if count % 60 == 0 {
            log::warn!(
                "连续 {count} 帧渲染超过 {SLOW_FRAME_MS:.0} 毫秒（最近 {ms:.0} ms，平均 {average:.0} ms）"
            );
        }
    } else {
        SLOW_FRAMES.store(0, Ordering::Relaxed);
    }
}

/// Ask for another frame at the right moment instead of spinning at 60 fps.
fn schedule_repaint(app: &PlayerApp, ctx: &Context) {
    // The same rule the frame loop follows: a hidden window is woken twice a
    // second, not sixty times, whatever the file is doing.
    if window_hidden(ctx) {
        ctx.request_repaint_after(HIDDEN_POLL);
        return;
    }
    // An open dialog must not stop the clock: playback continues behind the
    // settings window, and the end of a file still has to advance the playlist.
    match app.mode {
        Mode::Video | Mode::Audio => {
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
                // Paused, stopped or stepping: no frames are produced, but the
                // interface still animates — hover glows, the knob growing under
                // the pointer, the controls fading out. 120 ms ran all of that at
                // eight frames a second, which is what "the player feels slow"
                // is. Thirty frames a second costs nothing while nothing decodes.
                ctx.request_repaint_after(std::time::Duration::from_millis(33));
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
            (Key::P, true, _) => app.open_picture_panel(),
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
            (Key::Num0, false, _) => app.reset_zoom(),
            (Key::Num1, false, _) => app.image.zoom_original(),
            // Zoom works on whatever is on the canvas: a still and a video both
            // answer to these, which is what the menu says they do.
            (Key::Plus, false, _) | (Key::Equals, false, _) => {
                app.zoom_media(1.25, None);
            }
            (Key::Minus, false, _) => {
                app.zoom_media(0.8, None);
            }
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
///
/// A banner rather than a dialog: the message is worth reading but not worth
/// interrupting playback for, so it sits in the flow of the window and can be
/// dismissed with one click — or with Enter, because the close button is a real
/// button.
fn error_banner(app: &mut PlayerApp, ctx: &Context) -> Option<String> {
    let message = app.ui.error_banner.clone()?;
    let mut dismissed = false;
    egui::TopBottomPanel::top("mvp_error_banner")
        .frame(
            egui::Frame::new()
                .fill(app.theme.tokens.danger_soft)
                .inner_margin(egui::Margin::symmetric(
                    crate::theme::space::LG as i8,
                    crate::theme::space::SM as i8,
                ))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    app.theme.tokens.danger.gamma_multiply(0.35),
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
                    if widgets::secondary_button(ui, &app.theme.tokens, "关闭", 64.0) {
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
