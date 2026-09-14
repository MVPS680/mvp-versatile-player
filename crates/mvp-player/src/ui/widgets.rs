//! Custom widgets shared by every panel.
//!
//! `egui`'s stock widgets are usable but generic; a media player needs a seek
//! bar that previews a timestamp on hover, a volume slider that looks like part
//! of the transport bar, and list rows that highlight the way a desktop player's
//! playlist does. Everything here is drawn with the painter so it matches the
//! design tokens exactly.

use egui::{
    Align, Color32, Context, CornerRadius, FontId, Layout, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};

use crate::app::PlayerApp;
use crate::icons::{self, Icon};
use crate::state::Toast;
use crate::theme::{self, font, radius, space, Tokens};

/// A block-level heading with a hairline underneath.
pub fn section(ui: &mut Ui, tokens: &Tokens, title: &str) {
    ui.add_space(space::LG);
    ui.label(
        RichText::new(title)
            .size(font::H3)
            .strong()
            .color(tokens.text),
    );
    let rect = ui.available_rect_before_wrap();
    let y = rect.top() + 3.0;
    ui.painter().line_segment(
        [
            egui::pos2(rect.left(), y),
            egui::pos2(rect.right(), y),
        ],
        Stroke::new(1.0, tokens.border),
    );
    ui.add_space(space::MD);
}

/// One settings row: a label on the left, a control pinned to the right edge,
/// and an optional explanation on its own line underneath.
///
/// The two columns are sized from the *available* width rather than being fixed,
/// so the row stays readable in a narrow window — the label column gives way
/// first and the control can never slide underneath the text. `hint` is written
/// on a full-width line instead of living in a tooltip, because an explanation
/// nobody knows to hover is an explanation nobody reads.
///
/// `control_width` is the width reserved for the control; the closure draws it
/// inside a right-to-left layout, so a control that is narrower than the column
/// still lines up with every other row.
pub fn row(
    ui: &mut Ui,
    tokens: &Tokens,
    label: &str,
    hint: &str,
    control_width: f32,
    control: impl FnOnce(&mut Ui) -> bool,
) -> bool {
    /// Height of the label/control line itself.
    const LINE: f32 = 26.0;

    let full = ui.available_width().max(220.0);
    // The control never takes more than its share, so a long label still has
    // room to be read; the label column in turn never collapses to nothing.
    let control_width = control_width.min(full * 0.6);
    let label_width = (full - control_width - space::MD).max(90.0);

    // Reserve the whole row first and place the two columns by absolute
    // position. `allocate_ui_with_layout` sizes a region to its *content*, so
    // laying the row out with it leaves the control right after the label text
    // — every row a different, ragged x. Reserving the row explicitly is what
    // actually pins the controls into one straight column.
    let (_, rect) = ui.allocate_space(Vec2::new(full, LINE));
    let label_rect = Rect::from_min_size(rect.min, Vec2::new(label_width, LINE));
    let control_rect = Rect::from_min_size(
        egui::pos2(rect.right() - control_width, rect.top()),
        Vec2::new(control_width, LINE),
    );

    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(label_rect)
            .layout(Layout::left_to_right(Align::Center)),
        |ui| {
            ui.add(
                egui::Label::new(RichText::new(label).size(font::BODY).color(tokens.text))
                    .truncate(),
            );
        },
    );

    let changed = ui
        .scope_builder(
            egui::UiBuilder::new()
                .max_rect(control_rect)
                .layout(Layout::right_to_left(Align::Center)),
            control,
        )
        .inner;

    if !hint.is_empty() {
        ui.add_space(2.0);
        ui.label(
            RichText::new(hint)
                .size(font::TINY)
                .color(tokens.text_muted),
        );
    }
    // The air between rows is what keeps a settings page legible; without it
    // every control reads as one solid block.
    ui.add_space(space::MD);
    changed
}

/// A small rounded label, used for state pills and track kinds.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        FontId::proportional(font::TINY),
        color,
    );
    let size = galley.size() + Vec2::new(12.0, 4.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::SM as u8), color.gamma_multiply(0.2));
    ui.painter().galley(
        rect.center() - galley.size() / 2.0,
        galley,
        color,
    );
    response
}

/// A label/value row used by the information panel.
pub fn key_value(ui: &mut Ui, tokens: &Tokens, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        ui.label(
            RichText::new(label)
                .size(font::SMALL)
                .color(tokens.text_weak),
        );
        ui.label(RichText::new(value).size(font::SMALL).color(tokens.text));
    });
}

/// A horizontal tab strip with an animated underline.
pub fn tab_strip(
    ui: &mut Ui,
    tokens: &Tokens,
    tabs: &[&str],
    active: usize,
) -> Option<usize> {
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        for (index, label) in tabs.iter().enumerate() {
            let selected = index == active;
            let galley = ui.painter().layout_no_wrap(
                (*label).to_owned(),
                FontId::proportional(font::BODY),
                if selected { tokens.text } else { tokens.text_weak },
            );
            let size = Vec2::new(galley.size().x + space::MD * 2.0, 30.0);
            let (rect, response) = ui.allocate_exact_size(size, Sense::click());
            if response.hovered() && !selected {
                ui.painter()
                    .rect_filled(rect, CornerRadius::same(radius::SM as u8), tokens.hover);
            }
            ui.painter().galley(
                rect.center() - galley.size() / 2.0,
                galley,
                if selected { tokens.text } else { tokens.text_weak },
            );
            if selected {
                let underline = Rect::from_min_max(
                    egui::pos2(rect.left() + space::SM, rect.bottom() - 2.0),
                    egui::pos2(rect.right() - space::SM, rect.bottom()),
                );
                ui.painter()
                    .rect_filled(underline, CornerRadius::same(1), tokens.accent);
            }
            if response.clicked() {
                clicked = Some(index);
            }
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
    });
    clicked
}

/// An icon button styled for toolbars.
pub fn tool_button(
    app_tokens: &Tokens,
    ui: &mut Ui,
    icon: Icon,
    tooltip: &str,
    enabled: bool,
) -> Response {
    let response = ui.add_enabled_ui(enabled, |ui| {
        icons::icon_button(
            ui,
            icon,
            18.0,
            app_tokens.text_weak,
            app_tokens.text,
            app_tokens.hover,
            app_tokens.active,
        )
    });
    let inner = response.inner;
    if enabled {
        inner.clone().on_hover_text(tooltip)
    } else {
        inner
    }
}

/// The same, but rendered in the accent colour when toggled on.
pub fn toggle_tool_button(
    tokens: &Tokens,
    ui: &mut Ui,
    icon: Icon,
    tooltip: &str,
    active: bool,
    enabled: bool,
) -> Response {
    let color = if active { tokens.accent } else { tokens.text_weak };
    let hover = if active { tokens.accent_hover } else { tokens.text };
    let response = ui.add_enabled_ui(enabled, |ui| {
        icons::icon_button(
            ui,
            icon,
            18.0,
            color,
            hover,
            if active {
                tokens.accent.gamma_multiply(0.22)
            } else {
                tokens.hover
            },
            tokens.active,
        )
    });
    let inner = response.inner;
    if enabled {
        inner.clone().on_hover_text(tooltip)
    } else {
        inner
    }
}

/// A labelled switch row for the settings window.
pub fn switch_row(ui: &mut Ui, tokens: &Tokens, label: &str, value: &mut bool, hint: &str) -> bool {
    row(ui, tokens, label, hint, 40.0, |ui| {
        switch(ui, tokens, value).changed()
    })
}

/// A pill-shaped on/off switch.
pub fn switch(ui: &mut Ui, tokens: &Tokens, value: &mut bool) -> Response {
    let size = Vec2::new(38.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let radius = size.y / 2.0;
    let on = *value;
    let track = if on {
        tokens.accent
    } else {
        tokens.track
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius as u8), track);
    let knob_x = if on {
        rect.right() - radius
    } else {
        rect.left() + radius
    };
    ui.painter().circle_filled(
        egui::pos2(knob_x, rect.center().y),
        radius - 2.0,
        if on { tokens.on_accent } else { tokens.text_weak },
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A slider row for the settings window, with a value readout.
///
/// The readout sits in a fixed-width column so every slider in the page starts
/// and ends at the same x — ragged sliders are half of what makes a settings
/// page look like a pile of controls.
pub fn slider_row(
    ui: &mut Ui,
    tokens: &Tokens,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    format: impl Fn(f32) -> String,
) -> bool {
    /// Width of the value readout column.
    const READOUT: f32 = 56.0;
    /// Width of the slider itself.
    const SLIDER: f32 = 168.0;

    let text = format(*value);
    row(ui, tokens, label, "", READOUT + space::SM + SLIDER, |ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(READOUT, 20.0),
            Layout::right_to_left(Align::Center),
            |ui| {
                ui.label(
                    RichText::new(text)
                        .size(font::SMALL)
                        .monospace()
                        .color(tokens.text_weak),
                );
            },
        );
        ui.spacing_mut().slider_width = SLIDER;
        ui.add(egui::Slider::new(value, range).show_value(false))
            .changed()
    })
}

/// A dropdown row for the settings window.
pub fn combo_row<T: PartialEq + Clone>(
    ui: &mut Ui,
    tokens: &Tokens,
    label: &str,
    value: &mut T,
    options: &[(T, &'static str)],
) -> bool {
    /// Width of the dropdown.
    const WIDTH: f32 = 180.0;

    let current = options
        .iter()
        .find(|(v, _)| v == value)
        .map(|(_, l)| *l)
        .unwrap_or("—");

    row(ui, tokens, label, "", WIDTH, |ui| {
        let mut changed = false;
        egui::ComboBox::from_id_salt(label)
            .selected_text(RichText::new(current).size(font::SMALL))
            .width(WIDTH)
            .show_ui(ui, |ui| {
                for (option, text) in options {
                    if ui
                        .selectable_value(value, option.clone(), *text)
                        .changed()
                    {
                        changed = true;
                    }
                }
            });
        changed
    })
}

/// The custom seek bar.
///
/// Returns `Some(seconds)` while the user is scrubbing so the caller can show a
/// preview and defer the real seek until the drag ends.
pub fn seek_bar(
    ui: &mut Ui,
    tokens: &Tokens,
    position: f64,
    duration: f64,
    dragging: Option<f64>,
) -> SeekBarOutput {
    let height = 22.0;
    let full = ui.available_width().max(80.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(full, height), Sense::click_and_drag());
    let track_height = if response.hovered() || dragging.is_some() {
        6.0
    } else {
        4.0
    };
    let track = Rect::from_center_size(
        rect.center(),
        Vec2::new(rect.width(), track_height),
    );
    let radius = CornerRadius::same((track_height / 2.0) as u8);

    ui.painter()
        .rect_filled(track, radius, tokens.track);

    let fraction = |value: f64| -> f32 {
        if duration > 0.0 {
            (value / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    };

    let progress = match dragging {
        Some(preview) => fraction(preview),
        None => fraction(position),
    };

    if progress > 0.0 {
        let filled = Rect::from_min_size(
            track.min,
            Vec2::new(track.width() * progress, track.height()),
        );
        ui.painter().rect_filled(filled, radius, tokens.progress);
    }

    // Handle.
    let handle_x = track.left() + track.width() * progress;
    let handle_radius = if response.hovered() || dragging.is_some() {
        7.0
    } else {
        5.0
    };
    ui.painter().circle_filled(
        egui::pos2(handle_x, track.center().y),
        handle_radius,
        tokens.on_accent,
    );
    ui.painter().circle_stroke(
        egui::pos2(handle_x, track.center().y),
        handle_radius,
        Stroke::new(2.0, tokens.accent),
    );

    let mut output = SeekBarOutput {
        preview: None,
        released: None,
        response: response.clone(),
    };

    if let Some(pointer) = response.hover_pos() {
        let value = pointer_fraction(&track, pointer) * duration;
        output.preview = Some(value);
        if response.dragged() || response.clicked() {
            output.released = Some(value);
        }
    }
    if response.drag_stopped() {
        if let Some(pointer) = response.interact_pointer_pos() {
            output.released = Some(pointer_fraction(&track, pointer) * duration);
        }
    }
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    output
}

fn pointer_fraction(track: &Rect, pointer: egui::Pos2) -> f64 {
    if track.width() <= 0.0 {
        return 0.0;
    }
    ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0) as f64
}

/// What the seek bar reports this frame.
pub struct SeekBarOutput {
    /// Time under the pointer, for the hover readout.
    pub preview: Option<f64>,
    /// Time the user asked to seek to.
    pub released: Option<f64>,
    /// The underlying interaction response.
    pub response: Response,
}

/// A compact volume slider that reads as part of the transport bar.
pub fn volume_slider(ui: &mut Ui, tokens: &Tokens, value: f32) -> Option<f32> {
    let width = 90.0;
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::click_and_drag());
    let track = Rect::from_center_size(rect.center(), Vec2::new(width, 4.0));
    let radius = CornerRadius::same(2);
    ui.painter().rect_filled(track, radius, tokens.track);
    let fraction = (value / 2.0).clamp(0.0, 1.0);
    let filled = Rect::from_min_size(
        track.min,
        Vec2::new(track.width() * fraction, track.height()),
    );
    ui.painter().rect_filled(filled, radius, tokens.accent);

    let x = track.left() + track.width() * fraction;
    let knob = if response.hovered() || response.dragged() {
        6.0
    } else {
        4.5
    };
    ui.painter()
        .circle_filled(egui::pos2(x, track.center().y), knob, tokens.on_accent);

    let mut result = None;
    if response.dragged() || response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let f = ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0);
            result = Some(f * 2.0);
        }
    }
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    result
}

/// Draw the transient message near the top of the window.
pub fn draw_toast(app: &PlayerApp, ctx: &Context, toast: &Toast) {
    let opacity = toast.opacity();
    if opacity <= 0.0 {
        return;
    }
    let tokens = &app.theme.tokens;
    let color = crate::app::toast_color(&app.theme, toast.kind);

    let galley = ctx.fonts(|f| {
        f.layout_no_wrap(
            toast.text.clone(),
            FontId::proportional(font::BODY),
            tokens.text,
        )
    });
    let icon_width = if toast.icon.is_some() { 26.0 } else { 0.0 };
    let size = Vec2::new(galley.size().x + space::LG * 2.0 + icon_width, 38.0);
    let screen = ctx.screen_rect();
    let rect = Rect::from_center_size(
        egui::pos2(screen.center().x, screen.top() + 72.0),
        size,
    );

    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("mvp_toast"),
    ));
    painter.rect_filled(
        rect,
        CornerRadius::same(radius::LG as u8),
        tokens
            .elevated
            .gamma_multiply(opacity)
            .linear_multiply(1.0),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius::LG as u8),
        Stroke::new(1.0, color.gamma_multiply(opacity * 0.6)),
        StrokeKind::Inside,
    );
    if let Some(icon) = toast.icon {
        let icon_rect = Rect::from_center_size(
            egui::pos2(rect.left() + space::LG + 6.0, rect.center().y),
            Vec2::splat(16.0),
        );
        icons::draw(&painter, icon_rect, icon, color.gamma_multiply(opacity));
    }
    painter.galley(
        egui::pos2(
            rect.left() + space::LG + icon_width,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        tokens.text.gamma_multiply(opacity),
    );
    ctx.request_repaint();
}

/// A full-width list row with a hover highlight, used by the playlist.
pub fn list_row(
    ui: &mut Ui,
    tokens: &Tokens,
    selected: bool,
    height: f32,
) -> (Rect, Response, egui::Painter) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let fill = theme::row_fill(tokens, selected, response.hovered());
    if fill != Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(radius::SM as u8), fill);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let painter = ui.painter().clone();
    (rect, response, painter)
}

/// A muted "nothing here" placeholder.
pub fn empty_hint(ui: &mut Ui, tokens: &Tokens, text: &str) {
    ui.add_space(space::XL);
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new(text)
                .size(font::SMALL)
                .color(tokens.text_muted),
        );
    });
}

/// Convert accumulated wheel movement into whole steps.
///
/// A notched wheel arrives as a single large spike (egui multiplies a line by
/// its native `line_scroll_speed`, 40 points on the desktop), while a precision
/// touch-pad sends a stream of tiny deltas. Accumulating turns both into the
/// *same* number of steps per notch, and returning the leftover lets a slow
/// drag still reach the next step instead of being rounded away every frame —
/// which is what made the wheel feel unpredictable.
pub fn wheel_steps(accumulated: f32, points_per_step: f32) -> (i32, f32) {
    if points_per_step <= 0.0 || !accumulated.is_finite() {
        return (0, 0.0);
    }
    let steps = (accumulated / points_per_step).trunc();
    (steps as i32, accumulated - steps * points_per_step)
}

/// Draw the dimming gradient that sits between the picture and the controls.
///
/// The transport bar floats over the video in fullscreen, and a bright frame
/// behind a translucent bar swallows the icons. A gradient reads as part of the
/// picture in a way a hard-edged band does not, which is why this is a mesh
/// rather than a filled rectangle.
pub fn paint_scrim(painter: &egui::Painter, rect: Rect) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let mut mesh = egui::Mesh::default();
    let top = Color32::TRANSPARENT;
    let bottom = Color32::from_black_alpha(scrim_alpha(1.0));
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// Opacity of the scrim at `fraction` of its height, fully transparent at the
/// top and strongest at the bottom.
///
/// Quadratic rather than linear: most of the darkening has to be where the
/// controls are, and a linear ramp is still visibly grey halfway up the video.
pub fn scrim_alpha(fraction: f32) -> u8 {
    /// Alpha at the very bottom of the band — dark enough for white icons on a
    /// white frame, light enough to still see the picture.
    const MAX: f32 = 150.0;
    let t = fraction.clamp(0.0, 1.0);
    (MAX * t * t).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_fraction_is_clamped() {
        let track = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(100.0, 4.0));
        assert_eq!(pointer_fraction(&track, egui::pos2(-50.0, 2.0)), 0.0);
        assert_eq!(pointer_fraction(&track, egui::pos2(150.0, 2.0)), 1.0);
        assert!((pointer_fraction(&track, egui::pos2(50.0, 2.0)) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn pointer_fraction_handles_a_zero_width_track() {
        let track = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(0.0, 4.0));
        assert_eq!(pointer_fraction(&track, egui::pos2(10.0, 2.0)), 0.0);
    }

    #[test]
    fn the_scrim_gradient_is_transparent_at_the_top_and_strongest_at_the_bottom() {
        assert_eq!(scrim_alpha(0.0), 0);
        assert!(scrim_alpha(1.0) > scrim_alpha(0.5));
        assert!(scrim_alpha(0.5) > scrim_alpha(0.25));
        assert!(scrim_alpha(1.0) <= 160, "the picture must stay visible");
        // Out of range must not wrap around.
        assert_eq!(scrim_alpha(-1.0), 0);
        assert_eq!(scrim_alpha(4.0), scrim_alpha(1.0));
    }

    /// One wheel notch must always be one step, whatever the device reports.
    #[test]
    fn wheel_notches_become_whole_steps_without_losing_the_remainder() {
        // A notched wheel: a single 40-point spike is exactly one step.
        assert_eq!(wheel_steps(40.0, 40.0), (1, 0.0));
        assert_eq!(wheel_steps(-40.0, 40.0), (-1, 0.0));
        assert_eq!(wheel_steps(120.0, 40.0), (3, 0.0), "three notches, three steps");

        // A precision touch-pad: tiny deltas accumulate instead of vanishing.
        let mut leftover = 0.0;
        let mut stepped = 0;
        for _ in 0..2 {
            let (steps, left) = wheel_steps(leftover + 15.0, 40.0);
            leftover = left;
            stepped += steps;
        }
        assert_eq!(stepped, 0, "30 points is still short of a notch");
        let (steps, leftover) = wheel_steps(leftover + 15.0, 40.0);
        assert_eq!(steps, 1, "45 accumulated points is one full notch");
        assert!(leftover > 0.0 && leftover < 40.0, "the rest is carried over");

        // Nonsense in, nothing out.
        assert_eq!(wheel_steps(10.0, 0.0), (0, 0.0));
        assert_eq!(wheel_steps(f32::NAN, 40.0), (0, 0.0));
    }

    /// Every settings row must put its control at the same right-hand edge,
    /// whatever the label length or control kind — the "everything is crammed
    /// together" report was rows laid out with content-sized regions, which left
    /// each control at a different, ragged x.
    #[test]
    fn settings_rows_keep_one_straight_control_column() {
        let tokens = crate::theme::Theme::default().tokens;
        let ctx = egui::Context::default();
        let mut lefts: Vec<f32> = Vec::new();
        let mut rights: Vec<f32> = Vec::new();

        let _ = ctx.run(Default::default(), |ctx| {
            egui::Area::new(egui::Id::new("mvp_row_test")).show(ctx, |ui| {
                ui.set_max_width(706.0);
                for (label, hint, width) in [
                    ("短标签", "", 40.0f32),
                    ("一个相当长的设置项目名称", "下面还有一行解释文字", 232.0),
                    ("中等长度标签", "", 180.0),
                ] {
                    row(ui, &tokens, label, hint, width, |ui| {
                        let rect = ui.max_rect();
                        lefts.push(rect.left());
                        rights.push(rect.right());
                        false
                    });
                }
            });
        });

        assert_eq!(rights.len(), 3, "every row must have drawn a control");
        for value in &rights {
            assert!(
                (value - rights[0]).abs() < 0.5,
                "controls are not in one column: {rights:?}"
            );
        }
        assert!(
            (rights[2] - lefts[2] - 180.0).abs() < 0.5,
            "a combo row must reserve its own width"
        );
        assert!(
            lefts[1] < lefts[0],
            "the wider control must start further to the left"
        );
    }
}
