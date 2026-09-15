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

/// A palette-free description of the picture the glass should frost.
///
/// `None` when there is nothing to frost — no picture (audio, a still image, an empty
/// window) or a picture that has been rotated or mirrored. The frost samples the
/// video texture through the rectangle the frame was drawn into, and those two
/// transformations break exactly that correspondence; a wrong blur is worse than
/// none.
pub fn frost_source(app: &PlayerApp) -> Option<(egui::TextureId, egui::Rect)> {
    let texture = app.texture.as_ref()?;
    let rect = app.ui.picture_rect?;
    if app.settings.rotation != 0 || app.settings.flip_h || app.settings.flip_v {
        return None;
    }
    Some((texture.id(), rect))
}

/// Reserve the slot the frosted backdrop goes into.
///
/// Called *before* the content is laid out: the frost is opaque, so it has to land
/// under the labels, and a floating surface only knows its rect once they exist.
pub fn frost_slot(ui: &Ui) -> egui::layers::ShapeIdx {
    ui.painter().add(egui::Shape::Noop)
}

/// The same glass for a *window* or an *area*, whose rect is only known once it has
/// been laid out.
///
/// `egui::Window` lays its title bar out inside its own frame and *outside* the `Ui`
/// the closure is handed, so the rect a closure can measure is the body — the title
/// bar is not in it. Filled from the body's rect alone, a sheet shows a strip along
/// its top with no glass on it at all: transparent, taking its colour from whatever
/// is behind. The window's own rect comes back from `show`, and filling the slot
/// afterwards is not a hack — the shape list is not consumed until the end of the
/// frame, which is exactly the mechanism egui uses for the title bar's own
/// background. The order works out too: the title bar is painted *after* the body, so
/// the material still lands under the title's text and under the body's widgets. The
/// whole sheet ends up one colour, with everything still readable on top of it.
///
/// `slot` is `None` when the container never ran its closure (a window that is
/// closed), and then there is nothing to fill.
pub fn frost_surface(
    ctx: &egui::Context,
    app: &PlayerApp,
    tokens: &crate::theme::Tokens,
    slot: Option<egui::layers::ShapeIdx>,
    layer: egui::LayerId,
    rect: Rect,
    glass: crate::ui::glass::Glass,
) {
    if let Some(slot) = slot {
        frost_into(&ctx.layer_painter(layer), app, slot, tokens, rect, glass);
    }
}

/// The frosted picture plus the material, written into a shape slot.
///
/// The order is the whole point. The frost is opaque — it replaces the surface's own
/// colour with the film's — so the bed has to go *on top of* it, not underneath: a
/// panel whose colour comes from the video shows one colour where the film is bright
/// and another where it is dark, which reads as two different panels glued together.
/// With the bed over the frost the tone stays the surface's own, and the blurred film
/// only shifts it. When there is nothing to frost, the material is still drawn: a
/// surface must never be left un-glassed just because the picture is missing.
fn frost_into(
    painter: &egui::Painter,
    app: &PlayerApp,
    slot: egui::layers::ShapeIdx,
    tokens: &crate::theme::Tokens,
    rect: Rect,
    glass: crate::ui::glass::Glass,
) {
    let mut shapes = Vec::new();
    if let Some((texture, source)) = frost_source(app) {
        shapes.extend(crate::ui::glass::frost_shapes(
            texture,
            rect,
            source,
            glass.radius,
            crate::ui::glass::FROST_RADIUS,
        ));
    }
    // The bed, the wash, the sheen and the rim, in that order, over the frost.
    shapes.extend(crate::ui::glass::shapes(tokens, rect, glass, 1.0));
    painter.set(slot, egui::Shape::Vec(shapes));
}

/// A block-level heading with a hairline underneath.
///
/// The heading uses the bold cut of the system font and the hairline uses the
/// *separator* colour rather than the border colour: the heading and the rows it
/// introduces live on one surface, and a surface border there would read as the
/// edge of a panel that is not there.
pub fn section(ui: &mut Ui, tokens: &Tokens, title: &str) {
    ui.add_space(space::XL);
    ui.label(
        RichText::new(title)
            .font(crate::theme::strong_font(font::H3))
            .color(tokens.text),
    );
    ui.add_space(space::XS);
    let rect = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [
            egui::pos2(rect.left(), rect.top()),
            egui::pos2(rect.right(), rect.top()),
        ],
        Stroke::new(1.0_f32, tokens.separator),
    );
    ui.add_space(space::SM);
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

    // The row uses whatever width it has. A forced 220 pt minimum used to push
    // the control column past the right edge of a narrow settings page, where
    // it was clipped rather than laid out — every row then ended at a different,
    // invisible x.
    let full = ui.available_width();
    // The control never takes more than its share, so a long label still has
    // room to be read; the label column gives way first, because the control is
    // what the row is for.
    let control_width = control_width.min(full * 0.6).max(0.0);
    let label_width = (full - control_width - space::MD).max(0.0);

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
        ui.add_space(space::XXS);
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

/// A segmented control: the macOS way to choose one of a few options.
///
/// Apple draws exactly one of these — a row of labels inside a rounded trough
/// with the current one on a raised pill — so the tab strip below is this
/// control under a name that says what it is used *for*.
///
/// Returns the index that was clicked. `id_source` keys the animation, so two
/// controls on one page never share a pill.
pub fn segmented(
    ui: &mut Ui,
    tokens: &Tokens,
    id_source: &str,
    items: &[&str],
    active: usize,
) -> Option<usize> {
    /// Height of the trough. 26 pt keeps a 12 pt label clear of the edges at
    /// every scaling factor the player supports.
    const HEIGHT: f32 = 26.0;
    /// Space between the trough and the pill sliding inside it.
    const INSET: f32 = 2.0;
    /// Padding on each side of a label.
    const PAD: f32 = space::LG;

    if items.is_empty() {
        return None;
    }

    let galleys: Vec<_> = items
        .iter()
        .map(|label| {
            ui.painter().layout_no_wrap(
                (*label).to_owned(),
                FontId::proportional(font::SMALL),
                tokens.text,
            )
        })
        .collect();
    // Every segment is as wide as its own label plus the same padding, so the
    // pill does not change shape as the selection moves along the row.
    let natural: Vec<f32> = galleys.iter().map(|g| g.size().x + PAD).collect();
    let total: f32 = natural.iter().sum();

    // Narrow sidebars are the normal case, not the exception: rather than let the
    // control hang off the edge of the panel, the segments give way together.
    let available = (ui.available_width() - INSET * 2.0).max(0.0);
    let scale = if total > available && total > 0.0 {
        available / total
    } else {
        1.0
    };
    let widths: Vec<f32> = natural.iter().map(|w| w * scale).collect();
    let width: f32 = widths.iter().sum();

    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width + INSET * 2.0, HEIGHT), Sense::click());
    let track = rect.shrink(INSET);
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::MD as u8), tokens.sunken);

    let mut clicked = None;
    let mut x = track.left();
    for (index, galley) in galleys.iter().enumerate() {
        let segment = Rect::from_min_size(
            egui::pos2(x, track.top()),
            Vec2::new(widths[index], track.height()),
        );
        let selected = index == active;
        let hovered = response
            .hover_pos()
            .is_some_and(|pointer| segment.contains(pointer));
        let settle = ui.ctx().animate_bool_with_time(
            ui.id().with((id_source, index, "segment")),
            selected,
            SETTLE,
        );
        if settle > 0.0 {
            // The pill fades in on the segment that was chosen and out on the one
            // that was left, so the selection reads as one object moving.
            ui.painter().rect_filled(
                segment,
                CornerRadius::same(radius::SM as u8),
                tokens.active.gamma_multiply(settle),
            );
            ui.painter().rect_stroke(
                segment,
                CornerRadius::same(radius::SM as u8),
                Stroke::new(
                    1.0_f32,
                    tokens.border_strong.gamma_multiply(settle),
                ),
                StrokeKind::Inside,
            );
        } else if hovered {
            ui.painter().rect_filled(
                segment,
                CornerRadius::same(radius::SM as u8),
                tokens.hover,
            );
        }
        ui.painter().galley(
            egui::pos2(
                segment.center().x - galley.size().x / 2.0,
                segment.center().y - galley.size().y / 2.0,
            ),
            galley.clone(),
            if selected || hovered {
                tokens.text
            } else {
                tokens.text_weak
            },
        );
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            if response.clicked() {
                clicked = Some(index);
            }
        }
        x += widths[index];
    }

    // Announced as the current choice rather than as a row of unrelated labels.
    let current = items.get(active).copied().unwrap_or_default();
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, current)
    });
    clicked
}

/// A row of tabs, drawn as a segmented control.
///
/// Kept as a name of its own so a call site can say what the tabs are for; the
/// control itself is `segmented`. Returns the index that was clicked.
pub fn tab_strip(
    ui: &mut Ui,
    tokens: &Tokens,
    tabs: &[&str],
    active: usize,
) -> Option<usize> {
    // Keyed by the first label: stable for the life of the control, and
    // different for every group of tabs in the window.
    let key = tabs.first().copied().unwrap_or("tabs");
    segmented(ui, tokens, key, tabs, active)
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
///
/// The knob is white and it *travels*: the position is what carries the state,
/// so a switch that jumped would throw away the only thing it has to say.
pub fn switch(ui: &mut Ui, tokens: &Tokens, value: &mut bool) -> Response {
    /// Track size. Apple's control is 51x31 at its largest; this is the compact
    /// one, which is what fits a 26 pt settings row.
    const TRACK: Vec2 = Vec2::new(38.0, 22.0);
    /// Knob diameter.
    const KNOB: f32 = 18.0;
    /// Gap between the knob and the edge of the track.
    const INSET: f32 = 2.0;

    let (rect, mut response) = ui.allocate_exact_size(TRACK, Sense::click());
    // Keyboard: Space and Enter toggle, like every other checkbox in the system.
    let keyboard = response.has_focus()
        && ui.input(|i| i.key_pressed(egui::Key::Space) || i.key_pressed(egui::Key::Enter));
    if response.clicked() || keyboard {
        *value = !*value;
        response.mark_changed();
    }

    let settle = ui
        .ctx()
        .animate_bool_with_time(ui.id().with("switch"), *value, SETTLE);
    ui.painter().rect_filled(
        rect,
        CornerRadius::same((TRACK.y / 2.0) as u8),
        blend(tokens.track, tokens.accent, settle),
    );

    // The travel of the knob is the animated value, not the new one — that is
    // the whole difference between a switch and a checkbox.
    let center = egui::pos2(
        egui::lerp(
            (rect.left() + KNOB / 2.0 + INSET)..=(rect.right() - KNOB / 2.0 - INSET),
            settle,
        ),
        rect.center().y,
    );
    ui.painter().circle_filled(center, KNOB / 2.0, tokens.on_accent);
    ui.painter().circle_stroke(
        center,
        KNOB / 2.0,
        Stroke::new(1.0_f32, tokens.border_strong),
    );

    paint_focus(ui, &response, rect, tokens);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let selected = *value;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, selected, "")
    });
    response
}

/// A slider drawn the way the system draws one: a thin track that thickens under
/// the pointer, a small white knob, and no numbers on the track itself.
///
/// Returns `Some(value)` only while the value is being changed, so a caller can
/// tell a real edit from a redraw — the contract `egui::Slider` used to provide.
/// Keyboard: with focus, the arrow keys nudge by one percent of the range.
pub fn slider(
    ui: &mut Ui,
    tokens: &Tokens,
    id_source: &str,
    value: f32,
    range: std::ops::RangeInclusive<f32>,
    width: f32,
) -> Option<f32> {
    /// Interaction height — far taller than the track, because a 3 pt line is
    /// not something anyone can hit.
    const HEIGHT: f32 = 24.0;
    /// Track thickness at rest, and under the pointer.
    const TRACK: f32 = 3.0;
    const TRACK_ACTIVE: f32 = 5.0;
    /// Knob radius at rest, and under the pointer.
    const KNOB: f32 = 6.0;
    const KNOB_ACTIVE: f32 = 8.0;

    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, HEIGHT), Sense::click_and_drag());
    let (start, end) = (*range.start(), *range.end());
    let span = (end - start).max(f32::MIN_POSITIVE);

    // The track stops one knob-radius short of each end, so the knob at 0 % and
    // at 100 % still sits *inside* the control rather than half outside it.
    let track = Rect::from_min_max(
        egui::pos2(rect.left() + KNOB_ACTIVE, rect.center().y),
        egui::pos2(rect.right() - KNOB_ACTIVE, rect.center().y),
    );

    let current = value;
    let mut next = value;
    if response.dragged() || response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let fraction = pointer_fraction(&track, pointer) as f32;
            next = start + fraction * span;
        }
    } else if response.has_focus() {
        let step = ui.input(|i| {
            let up = i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::ArrowUp);
            let down = i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowDown);
            (up as i32 - down as i32) as f32
        });
        if step != 0.0 {
            next = (next + step * span * 0.01).clamp(start, end);
        }
    }
    let next = next.clamp(start, end);

    let grow = ui.ctx().animate_bool_with_time(
        ui.id().with((id_source, "slider")),
        response.hovered() || response.dragged(),
        SETTLE,
    );
    let thickness = TRACK + (TRACK_ACTIVE - TRACK) * grow;
    let knob = KNOB + (KNOB_ACTIVE - KNOB) * grow;
    let radius = CornerRadius::same((thickness / 2.0) as u8);
    let bar = Rect::from_center_size(track.center(), Vec2::new(track.width(), thickness));
    let fraction = ((next - start) / span).clamp(0.0, 1.0);

    ui.painter().rect_filled(bar, radius, tokens.track);
    if fraction > 0.0 {
        ui.painter().rect_filled(
            Rect::from_min_size(bar.min, Vec2::new(bar.width() * fraction, thickness)),
            radius,
            tokens.accent,
        );
    }
    let knob_center = egui::pos2(bar.left() + bar.width() * fraction, bar.center().y);
    ui.painter().circle_filled(knob_center, knob, tokens.on_accent);
    ui.painter()
        .circle_stroke(knob_center, knob, Stroke::new(1.0_f32, tokens.border_strong));
    paint_focus(ui, &response, rect, tokens);

    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.widget_info(|| egui::WidgetInfo::slider(true, f64::from(next), ""));

    // Reported only when the value actually moved: a caller that persists on
    // every frame would otherwise write the settings file sixty times a second.
    if (next - current).abs() > f32::EPSILON {
        Some(next)
    } else {
        None
    }
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
        match slider(ui, tokens, label, *value, range, SLIDER) {
            Some(new) => {
                *value = new;
                true
            }
            None => false,
        }
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
    // The caller gives the bar its width; the floor only keeps a degenerate
    // zero-width layout from producing a zero-length drag target.
    let full = ui.available_width().max(24.0);
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
        Stroke::new(2.0_f32, tokens.accent),
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

/// The transport bar's volume slider: the same control, at toolbar size.
///
/// Shares the implementation rather than imitating it — a volume slider that
/// behaved differently from the sliders in the settings sheet is exactly the
/// kind of detail that makes one window feel like two products.
pub fn volume_slider(ui: &mut Ui, tokens: &Tokens, value: f32) -> Option<f32> {
    /// Width of the control in the transport bar.
    const WIDTH: f32 = 90.0;
    // The setting runs 0–200 %, which is what the audio path accepts.
    slider(ui, tokens, "volume", value, 0.0..=2.0, WIDTH)
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

    let mut painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("mvp_toast"),
    ));
    // The HUD is a capsule of glass over the picture. It fades as a whole — the
    // material is translucent already, so scaling its opacity is enough to keep
    // the frame behind it visible while the message leaves.
    painter.multiply_opacity(opacity);
    let radius = size.y / 2.0;
    // The frosted picture goes down first: the HUD slides over the film, and a
    // blurred copy of what is behind it is what makes the capsule read as glass
    // rather than as a hole cut in the frame.
    if let Some((texture, source)) = frost_source(app) {
        crate::ui::glass::frosted(
            &painter,
            texture,
            rect,
            source,
            radius,
            crate::ui::glass::FROST_RADIUS,
        );
    }
    crate::ui::glass::paint(
        &painter,
        tokens,
        rect,
        crate::ui::glass::Glass::float(radius),
    );
    // The state colour stays a wash *on* the glass, so a warning still reads as a
    // warning without turning the capsule into a coloured slab.
    painter.rect_filled(
        rect,
        CornerRadius::same(radius as u8),
        color.gamma_multiply(0.22),
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

// ---------------------------------------------------------------------------
// Controls in the system style
// ---------------------------------------------------------------------------
//
// Everything below is painted rather than configured, for one reason: a system
// control is defined by its *states* — rest, hover, pressed, focused, disabled —
// and `egui`'s stock widgets only expose a subset of them. Painting the five
// states in one place is what keeps a button, a switch and a slider feeling like
// they came out of the same box.

/// How long a control takes to settle from one state to the next.
const SETTLE: f32 = 0.14;

/// Mix two colours, `t` of the way from `a` to `b`.
///
/// `Color32` stores *premultiplied* channels, so they are mixed as they are: taking
/// the alpha out first would reintroduce colour the alpha has already taken away,
/// and the blend of two translucent colours would come out brighter than either of
/// them.
fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

/// Draw the keyboard-focus ring around `rect`, when `response` holds focus.
///
/// A ring rather than a colour change: a control that is already accent-filled
/// has no colour left to spend on "you are here".
fn paint_focus(ui: &Ui, response: &Response, rect: Rect, tokens: &Tokens) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(radius::MD as u8 + 2),
            Stroke::new(1.0_f32, tokens.focus_ring),
            StrokeKind::Outside,
        );
    }
}

/// Which of the two button roles a control is playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// The accent-filled button: the one action a screen is *for*.
    Primary,
    /// Everything else: grey, translucent, quiet.
    Secondary,
}

/// A button with the five system states, returning `true` when activated.
///
/// Returns a `bool` rather than an `egui::Response` because the interesting
/// question at every call site is "was this pressed" — and because activation by
/// keyboard (Enter or Space, like every other control in the interface) has to be
/// folded into the same answer, which a copied `Response` cannot express.
pub fn pill_button(
    ui: &mut Ui,
    tokens: &Tokens,
    id_source: &str,
    label: &str,
    kind: ButtonKind,
    min_width: f32,
    enabled: bool,
) -> bool {
    /// Height of a regular button. 30 pt clears the 26 pt interaction minimum
    /// with room for a 13 pt label inside it.
    const HEIGHT: f32 = 30.0;

    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(font::BODY),
        tokens.text,
    );
    let width = (galley.size().x + space::LG * 2.0).max(min_width);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, HEIGHT), Sense::click());

    let id = ui.id().with((id_source, "pill"));
    let hover = ui
        .ctx()
        .animate_bool_with_time(id, response.hovered() && enabled, SETTLE);
    let press = ui.ctx().animate_bool_with_time(
        id.with("press"),
        response.is_pointer_button_down_on() && enabled,
        0.05,
    );
    // Liquid feedback: the material compresses under the finger. It is the one
    // piece of motion Liquid Glass adds to a control, and it is what makes a press
    // feel like touching a surface rather than toggling a boolean.
    let rect = rect.shrink(press * 1.5);

    let (fill, text, glassy) = match kind {
        ButtonKind::Primary => (
            blend(
                blend(tokens.accent, tokens.accent_hover, hover),
                tokens.accent_pressed,
                press,
            ),
            tokens.on_accent,
            false,
        ),
        ButtonKind::Secondary => (
            blend(tokens.hover, tokens.active, hover.max(press)),
            tokens.text,
            true,
        ),
    };
    let fill = if enabled {
        fill
    } else {
        fill.gamma_multiply(0.5)
    };
    let text = if enabled { text } else { tokens.text_muted };

    let painter = ui.painter();
    if glassy {
        // The quiet button is a capsule of *glass*, not a grey rectangle: the
        // material is the shape, and the tint under the pointer is that material
        // catching light.
        crate::ui::glass::paint(
            painter,
            tokens,
            rect,
            crate::ui::glass::Glass::capsule(rect.height()),
        );
        if enabled && hover.max(press) > 0.0 {
            painter.rect_filled(
                rect,
                CornerRadius::same((rect.height() / 2.0) as u8),
                blend(Color32::TRANSPARENT, tokens.active, hover.max(press)),
            );
        }
    } else {
        painter.rect_filled(rect, CornerRadius::same(radius::MD as u8), fill);
    }
    painter.galley(rect.center() - galley.size() / 2.0, galley, text);

    paint_focus(ui, &response, rect, tokens);
    if enabled {
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label)
        });
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    enabled
        && (response.clicked()
            || (response.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space))))
}

/// The accent-filled button. One per screen, at most.
pub fn primary_button(ui: &mut Ui, tokens: &Tokens, label: &str, min_width: f32) -> bool {
    pill_button(ui, tokens, label, label, ButtonKind::Primary, min_width, true)
}

/// The quiet button: cancels, "open folder…", anything secondary.
pub fn secondary_button(ui: &mut Ui, tokens: &Tokens, label: &str, min_width: f32) -> bool {
    pill_button(
        ui,
        tokens,
        label,
        label,
        ButtonKind::Secondary,
        min_width,
        true,
    )
}

/// A ring spinner, for work that has no progress to report.
///
/// `phase` is the time in seconds since start-up (`ui.input(|i| i.time)`): the
/// gap in the ring is what makes the motion readable, and a spinner that does not
/// move is indistinguishable from a hang.
pub fn spinner(ui: &Ui, tokens: &Tokens, center: egui::Pos2, diameter: f32, phase: f64) {
    /// How much of the circle the moving arc covers.
    const SWEEP: f32 = std::f32::consts::FRAC_PI_2;

    let radius = (diameter / 2.0).max(2.0);
    ui.painter()
        .circle_stroke(center, radius, Stroke::new(2.0_f32, tokens.track));

    const SEGMENTS: usize = 24;
    let start = (phase * 2.5) as f32 % std::f32::consts::TAU;
    let points: Vec<egui::Pos2> = (0..=SEGMENTS)
        .map(|step| {
            let t = step as f32 / SEGMENTS as f32;
            let angle = start + t * SWEEP;
            egui::pos2(
                center.x + angle.cos() * radius,
                center.y + angle.sin() * radius,
            )
        })
        .collect();
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(2.5_f32, tokens.accent)));
    ui.ctx().request_repaint();
}

/// A placeholder row: a rounded bar with a band of light travelling along it.
///
/// The alternative — a line of text saying "loading" — tells the reader that
/// something is happening but not *what* is about to appear; a skeleton row has
/// the shape of the answer.
pub fn skeleton_row(ui: &mut Ui, tokens: &Tokens, width: f32, height: f32, phase: f64) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let radius = CornerRadius::same(radius::SM as u8);
    ui.painter().rect_filled(rect, radius, tokens.skeleton);

    // Clipped to the bar, so the highlight reads as light passing over content
    // rather than as a second object sliding behind it.
    let band = (height * 3.0).max(48.0);
    let travel = ((phase * 0.5) as f32 % 1.0) * (rect.width() + band) - band;
    let highlight = Rect::from_min_size(
        egui::pos2(rect.left() + travel, rect.top()),
        Vec2::new(band, height),
    );
    ui.painter()
        .with_clip_rect(rect)
        .rect_filled(highlight, radius, tokens.skeleton_highlight);
    ui.ctx().request_repaint();
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
