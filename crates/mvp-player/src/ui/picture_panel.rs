//! The picture panel: the seven adjustment sliders and the quality enhancement.
//!
//! Deliberately **not** an [`Overlay`](crate::state::Overlay).
//!
//! An overlay here would be wrong twice over. It dims what is behind it, and the one
//! thing this panel must not do is change the picture it is being used to judge — a
//! scrim over the frame turns "is this too blue" into a guess. And an overlay swallows
//! the keyboard, so pausing on the frame you are about to tune would mean closing the
//! panel first. So this is an ordinary egui window, in the same layer as the settings
//! window: no dim, no input trap, shortcuts live — space still pauses while you drag.
//!
//! It is also the visible half of the feature's cheapest promise: with the sliders
//! neutral and the enhancement off, the panel says so, and that sentence is the exact
//! condition under which the renderer never enters the adjustment path at all (see
//! [`crate::picture::uniforms`]). What the user reads on screen and what the GPU does
//! are the same condition, not two opinions about it.

use egui::{Context, RichText};

use crate::app::PlayerApp;
use crate::settings::RememberScope;
use crate::theme::{font, space};
use crate::ui::surface;
use crate::ui::widgets;

/// Width of the panel, in points.
///
/// Wide enough for `slider_row`'s label, readout and slider to sit at their full
/// widths — a narrow panel squeezes the slider to its 72 pt floor and every drag
/// becomes a coarse one.
const WIDTH: f32 = 356.0;

/// Draw the panel when it is open.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    if !app.ui.picture_panel_open {
        return;
    }
    // Video only, and it closes rather than staying up: the adjustments are drawn by
    // the video canvas, so a panel over a photograph or a song would be a set of
    // sliders that do nothing — and the per-file key it would write to is the last
    // video's, not the file on screen. See `PlayerApp::picture_applies`.
    if !app.picture_applies() {
        app.ui.picture_panel_open = false;
        return;
    }
    let tokens = app.theme.tokens.clone();
    let key = app.picture_key();
    // Read through the scope, so with "按文件" the sliders start from this file's
    // remembered values and with "全局" from the globals. Write back through the same
    // door at the end of the function.
    let mut picture = app.picture_settings();
    let mut enhance = app.settings.enhance;
    // What the enhancement is doing right now, for the status note below: the same
    // value the renderer will be given.
    let state = app.picture_state.unwrap_or_default();
    let mut scope = app.settings.picture_scope;
    let mut changed = false;
    let mut reset = false;
    let mut forget = false;
    let mut open = true;

    let screen = ctx.screen_rect();
    // The panel is tall — three sections and a dozen rows — and a small or high-DPI
    // screen must not cut the last slider off, so it may take most of the height and
    // scrolls if the contents still do not fit. Nothing scrolls on an ordinary window:
    // the content is shorter than the room on offer and the bar never appears.
    const RESERVED: f32 = 160.0;
    let room = (screen.height() - RESERVED).max(260.0);
    egui::Window::new(RichText::new("画面调节").size(font::H2).strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .movable(true)
        .vscroll(true)
        .max_height(room)
        .default_size([WIDTH, 420.0_f32.min(room)])
        // Against the right edge, where the sidebar lives when it is shown: the least
        // picture is hidden for the panel that changes it.
        .default_pos(egui::pos2(
            screen.right() - WIDTH - space::LG,
            screen.top() + 64.0,
        ))
        .constrain_to(screen)
        .frame(surface::sheet_shell(
            &tokens,
            surface::sheet_margin(),
            surface::SHEET_RADIUS,
        ))
        .show(ctx, |ui| {
            ui.set_width(WIDTH - 2.0 * space::MD);

            widgets::section(ui, &tokens, "画面调节");
            changed |= widgets::slider_row(
                ui,
                &tokens,
                "亮度",
                &mut picture.brightness,
                -1.0..=1.0,
                signed,
            );
            changed |= widgets::slider_row(
                ui,
                &tokens,
                "对比度",
                &mut picture.contrast,
                -1.0..=1.0,
                signed,
            );
            changed |= widgets::slider_row(
                ui,
                &tokens,
                "饱和度",
                &mut picture.saturation,
                -1.0..=1.0,
                signed,
            );
            // Named by what the user sees rather than by the maths: a warmer picture is
            // a shift towards orange, and "−1" has to read as "cooler" or the slider
            // cannot be used without a guess.
            changed |= widgets::slider_row(
                ui,
                &tokens,
                "色温",
                &mut picture.temperature,
                -1.0..=1.0,
                |value| {
                    if value.abs() < 0.005 {
                        "0".to_string()
                    } else if value > 0.0 {
                        format!("暖 {value:.2}")
                    } else {
                        format!("冷 {:.2}", value.abs())
                    }
                },
            );
            changed |= widgets::slider_row(
                ui,
                &tokens,
                "色调",
                &mut picture.tint,
                -1.0..=1.0,
                |value| {
                    if value.abs() < 0.005 {
                        "0".to_string()
                    } else if value > 0.0 {
                        format!("品红 {value:.2}")
                    } else {
                        format!("绿 {:.2}", value.abs())
                    }
                },
            );
            changed |= widgets::slider_row(
                ui,
                &tokens,
                "锐度",
                &mut picture.sharpness,
                0.0..=1.0,
                percent,
            );
            changed |= widgets::slider_row(
                ui,
                &tokens,
                "Gamma",
                &mut picture.gamma,
                0.4..=2.2,
                |value| format!("{value:.2}"),
            );

            ui.add_space(space::SM);
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, &tokens, "重置", 72.0) {
                    reset = true;
                }
                // The state of the fast path, said out loud — and asked of the same
                // function the renderer asks, so the note and the GPU cannot disagree.
                // Neutral *and* nothing asked of the enhancement is not a "small
                // adjustment": it is the path where the frame is drawn exactly as it
                // was before this feature existed.
                let note = if crate::picture::is_active(&picture, &enhance, state) {
                    "已调整"
                } else {
                    "中性：未启用画面通道"
                };
                ui.label(RichText::new(note).size(font::SMALL).color(tokens.text_weak));
            });

            widgets::section(ui, &tokens, "记住方式");
            let active = match scope {
                RememberScope::Global => 0,
                RememberScope::PerFile => 1,
            };
            if let Some(choice) = widgets::segmented(
                ui,
                &tokens,
                "picture-panel-scope",
                &["跟随全局", "按文件"],
                active,
            ) {
                let chosen = if choice == 1 {
                    RememberScope::PerFile
                } else {
                    RememberScope::Global
                };
                if chosen != scope {
                    scope = chosen;
                    changed = true;
                }
            }
            ui.label(
                RichText::new(match scope {
                    RememberScope::Global => "所有文件共用这一组滑块。".to_string(),
                    RememberScope::PerFile => format!(
                        "每个文件记住自己的一组；没调过的文件用全局值。最多记住 {} 个文件，\
                         超出后最早的一个被替换。",
                        crate::settings::Settings::PER_FILE_PICTURE_LIMIT
                    ),
                })
                .size(font::SMALL)
                .color(tokens.text_weak),
            );
            if scope == RememberScope::PerFile {
                let remembered = app.settings.per_file_picture.len();
                ui.horizontal(|ui| {
                    if widgets::secondary_button(ui, &tokens, "清除按文件记录", 132.0) {
                        forget = true;
                    }
                    ui.label(
                        RichText::new(format!("已记住 {remembered}"))
                            .size(font::SMALL)
                            .monospace()
                            .color(tokens.text_weak),
                    );
                });
            }

            widgets::section(ui, &tokens, "画质增强");
            changed |= widgets::switch_row(
                ui,
                &tokens,
                "启用画质增强",
                &mut enhance.enabled,
                "按每帧自己的直方图做轻微校正：暗部抬升、色彩补足、边缘平滑。\
                 关闭时立即失效，不留残影。",
            );
            if enhance.enabled {
                changed |= widgets::slider_row(
                    ui,
                    &tokens,
                    "强度",
                    &mut enhance.strength,
                    0.0..=1.0,
                    percent,
                );
                changed |= widgets::switch_row(
                    ui,
                    &tokens,
                    "自动色阶",
                    &mut enhance.auto_levels,
                    "把过暗或发灰的画面拉到完整的黑白范围。",
                );
                changed |= widgets::switch_row(
                    ui,
                    &tokens,
                    "自动色彩",
                    &mut enhance.auto_colour,
                    "画面偏淡时补足饱和度；已经鲜艳的画面不再加。",
                );
                changed |= widgets::slider_row(
                    ui,
                    &tokens,
                    "降噪",
                    &mut enhance.denoise,
                    0.0..=1.0,
                    percent,
                );
                changed |= widgets::slider_row(
                    ui,
                    &tokens,
                    "去块",
                    &mut enhance.deblock,
                    0.0..=1.0,
                    percent,
                );
                changed |= widgets::slider_row(
                    ui,
                    &tokens,
                    "锐化",
                    &mut enhance.sharpening,
                    0.0..=1.0,
                    percent,
                );
                ui.label(
                    RichText::new(
                        "降噪与锐化按画面细节自适应：细节多的画面少平滑、少加锐。\
                         降噪与去块共用同一次边缘感知平滑，两者同时拉满不会更强。",
                    )
                    .size(font::SMALL)
                    .color(tokens.text_weak),
                );
            }
        });

    if reset {
        // Reset *is* `Default` (see `PictureSettings::reset`), and `Default` is
        // "untouched" by construction, so the button cannot leave behind a value that
        // the zero-channel test would call a channel.
        picture.reset();
        changed = true;
    }
    if forget {
        app.settings.clear_per_file_pictures();
        changed = true;
    }
    if changed {
        // Clamp before storing: a slider cannot leave its range, but a hand-edited
        // settings file can, and every value the renderer sees comes through here.
        picture.clamp();
        enhance.clamp();
        app.settings.picture_scope = scope;
        app.settings.enhance = enhance;
        app.settings.set_picture_for(key.as_deref(), picture);
        app.store.mark_dirty();
    }
    if !open {
        app.ui.picture_panel_open = false;
    }
}

/// A signed value with a visible sign, or a bare `0`.
///
/// `+0.30` and `-0.30` have to be told apart at a glance: a plain `0.30` beside a
/// slider whose middle is the default reads as an adjustment even at rest.
fn signed(value: f32) -> String {
    if value.abs() < 0.005 {
        "0".to_string()
    } else {
        format!("{value:+.2}")
    }
}

/// A `0.0..=1.0` value as a percentage.
fn percent(value: f32) -> String {
    format!("{:.0}%", value * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_readout_keeps_its_sign_and_a_bare_zero() {
        assert_eq!(signed(0.0), "0");
        assert_eq!(signed(-0.0), "0");
        // The dead zone of `picture::smooth` is 0.004, so nothing below five
        // thousandths is ever a visible adjustment — printing it as `+0.00` would
        // claim a change that is not on screen.
        assert_eq!(signed(0.004), "0");
        assert_eq!(signed(0.31), "+0.31");
        assert_eq!(signed(-0.31), "-0.31");
    }

    #[test]
    fn a_percentage_readout_is_whole_numbers() {
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.5), "50%");
        assert_eq!(percent(1.0), "100%");
    }

    #[test]
    // The assertion *is* about two constants, which is the whole point: it fails at the
    // moment somebody narrows the panel or widens a slider row, rather than at the moment
    // a user notices every slider has become a coarse one.
    #[allow(clippy::assertions_on_constants)]
    fn the_panel_width_holds_a_full_width_slider_row() {
        // `slider_row` stops shrinking at 72 pt and its readout column is 56 pt, so a
        // panel narrower than this silently hands every slider the floor width.
        assert!(WIDTH - 2.0 * space::MD > 56.0 + space::SM + 72.0);
    }
}
