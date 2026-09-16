//! The opaque surfaces the chrome is made of.
//!
//! Every surface in the player is a flat, fully opaque fill — [`Tokens::bg`],
//! [`Tokens::panel`], [`Tokens::elevated`] or [`Tokens::sunken`] — plus at most one
//! hairline along the edge that faces content. There is no translucency, no blur, no
//! frost, no rim and no sheen anywhere in the interface, and this module is the one
//! place that decides so.
//!
//! # What was here, and why it is not
//!
//! This module was `glass.rs`, and it drew a Liquid Glass material: a translucent
//! bed, a white wash, a specular sheen built as a mesh fan over a rounded outline,
//! and a hard rim — on top of a *frosted backdrop*, which was a blur of the video
//! texture faked by averaging twenty-five shifted, semi-transparent copies of it.
//! Twenty-five full-size textured meshes, per floating surface, per frame.
//!
//! Three things killed it:
//!
//! 1. **It costs frames.** A settings sheet is roughly 900x700 points; twenty-five
//!    copies of it is fifteen million pixels of blending every frame, on a machine
//!    that may well be driving a 4K panel from integrated graphics — to draw a page
//!    of checkboxes.
//! 2. **It was never the point.** The player exists to show a picture, and the
//!    picture is supposed to be the brightest thing on screen. Chrome that borrows
//!    the film's colours is decoration.
//! 3. **It made text worse, not better.** A translucent panel takes its contrast
//!    from whatever is behind it, so a bright frame behind a sheet is exactly the
//!    case where the labels on it stop being readable. A flat `elevated` fill reads
//!    the same over a white frame, a black frame and no frame at all.
//!
//! An opaque surface is also what makes the whole layout simpler: nothing has to be
//! reserved ahead of the content any more, because there is nothing opaque to paint
//! *underneath* it, and a `Frame` can carry the fill itself.

use egui::{CornerRadius, Frame, Margin, Painter, Rect, Stroke, StrokeKind};

use crate::theme::{shadow, space, Tokens};

/// Corner radius of a floating sheet — the settings window and the dialogs.
///
/// Rounder than a card, because a window that floats should read as sitting on top
/// of the picture rather than as cut out of it.
pub const SHEET_RADIUS: f32 = 20.0;

/// Inner margin of a floating sheet, matching the reach of its rim.
pub fn sheet_margin() -> Margin {
    Margin::same(space::LG as i8)
}

/// The shell of a docked bar: an opaque `panel` fill and nothing else.
///
/// The hairline that separates a bar from the picture is egui's own panel separator
/// line — drawn on the edge that faces content, and coloured here by
/// `Visuals::widgets.noninteractive.bg_stroke` (see [`crate::theme::Theme::install`]).
/// So there is no stroke to add: a frame stroke would draw all four edges and box the
/// bar in, and a hand-drawn one would have to be told which edge to use, which the
/// panel already knows.
pub fn bar_shell(tokens: &Tokens, margin: Margin) -> Frame {
    Frame::new().fill(tokens.panel).inner_margin(margin)
}

/// The shell of a floating window: fill, corner, inner margin and the one shadow.
pub fn sheet_shell(tokens: &Tokens, margin: Margin, radius: f32) -> Frame {
    Frame::new()
        .fill(tokens.elevated)
        .corner_radius(CornerRadius::same(radius.clamp(0.0, 255.0) as u8))
        .inner_margin(margin)
        .shadow(shadow::window())
}

/// Paint a floating surface straight onto a painter.
///
/// For the two surfaces whose rect is known before they are drawn — the HUD capsule
/// and the image viewer's toolbar — rather than laid out by a `Frame`. `radius` is
/// half the height for a capsule.
pub fn paint_sheet(painter: &Painter, tokens: &Tokens, rect: Rect, radius: f32) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let corner = CornerRadius::same(radius.clamp(0.0, 255.0) as u8);
    painter.rect_filled(rect, corner, tokens.elevated);
    painter.rect_stroke(
        rect,
        corner,
        Stroke::new(1.0_f32, tokens.border_strong),
        StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> Tokens {
        Tokens::default()
    }

    /// The whole point of the module: nothing here is translucent, so nothing here
    /// has to be blended against whatever happens to be behind it — and a bright
    /// frame behind a panel can no longer make the panel's own text unreadable.
    #[test]
    fn every_surface_is_opaque() {
        let t = tokens();
        assert_eq!(bar_shell(&t, Margin::same(8)).fill.a(), 255, "a bar");
        assert_eq!(
            sheet_shell(&t, sheet_margin(), SHEET_RADIUS).fill.a(),
            255,
            "a sheet"
        );
        for (what, colour) in [
            ("bg", t.bg),
            ("panel", t.panel),
            ("elevated", t.elevated),
            ("letterbox", t.letterbox),
        ] {
            assert_eq!(colour.a(), 255, "{what} must be a solid colour");
        }
    }

    /// A sheet has to sit *above* a bar, and a bar above the window, or a settings
    /// window reads as a toolbar that happens to be in the middle of the screen.
    #[test]
    fn the_surfaces_step_up_in_lightness() {
        let t = tokens();
        let sum = |c: egui::Color32| u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b());
        assert!(sum(t.panel) > sum(t.bg));
        assert!(sum(t.elevated) > sum(t.panel));
    }

    /// The shells carry a fill and geometry and nothing else. A frame stroke would
    /// draw all four edges; the bar's separator belongs to egui, which knows which
    /// edge faces content.
    #[test]
    fn a_bar_shell_carries_a_fill_and_no_stroke() {
        let t = tokens();
        let frame = bar_shell(&t, Margin::same(space::SM as i8));
        assert_eq!(frame.fill, t.panel);
        assert_eq!(frame.stroke.color.a(), 0, "no frame stroke");
        assert_eq!(frame.inner_margin.top, space::SM as i8);
    }

    /// A sheet keeps the drop shadow that tells the eye it floats — the one piece of
    /// the material that is neither translucent nor expensive.
    #[test]
    fn a_sheet_keeps_its_shadow() {
        let t = tokens();
        let frame = sheet_shell(&t, sheet_margin(), SHEET_RADIUS);
        assert_eq!(frame.fill, t.elevated);
        assert!(frame.shadow.blur > 0, "a sheet must still float");
        assert_eq!(frame.corner_radius.nw, SHEET_RADIUS as u8);
    }

    /// Painting a sheet must survive the rects the layout can actually hand it: a
    /// zero-size one (a collapsed area), a radius larger than half the height (a
    /// capsule), and ridiculous values that a caller could pass by accident.
    #[test]
    fn painting_a_sheet_is_total() {
        let t = tokens();
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::debug());
            for rect in [
                Rect::NOTHING,
                Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(320.0, 68.0)),
                Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1.0, 400.0)),
            ] {
                for radius in [0.0, 12.0, 34.0, 900.0, -5.0, f32::NAN] {
                    paint_sheet(&painter, &t, rect, radius);
                }
            }
        });
    }
}
