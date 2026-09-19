//! The bird's-eye view in the corner of the canvas.
//!
//! Zooming a picture is only useful if the part that is off screen can be found
//! again, and the wheel gives no clue where the view has gone: the canvas shows
//! a detail with no edges to hold on to. This is the answer every image tool
//! settled on years ago (Photoshop's navigator, a camera's rear screen): a small
//! copy of the whole picture with the part on screen marked on it, in a corner
//! where it covers as little of the detail as possible — and, because it is a
//! map, it can be dragged to travel.
//!
//! It is drawn only while the picture is bigger than the canvas. A map that
//! always covers the whole of itself is a rectangle of decoration over the
//! film, and the one moment the picture is not cropped — "适应窗口" — is the one
//! moment it has nothing to say.

use egui::{Color32, CornerRadius, Rect, Stroke, StrokeKind, Vec2};

use crate::app::PlayerApp;
use crate::gl::picture_pass::Adjusted;
use crate::theme::{radius, space, Tokens};
use crate::ui::canvas::image_transformed;
use crate::ui::surface;
use crate::view;

/// Fraction of the canvas' width the map takes.
const WIDTH_FRACTION: f32 = 0.20;
/// Bounds on that width, in points. The floor keeps a wide panorama legible;
/// the ceiling stops a 4K window from carrying a second window.
const MIN_WIDTH: f32 = 96.0;
const MAX_WIDTH: f32 = 200.0;
/// Most of the canvas' height the map may take, which is what a portrait
/// picture runs into first.
const HEIGHT_FRACTION: f32 = 0.32;
/// Room between the map and the corner of the canvas.
const MARGIN: f32 = space::MD;
/// Room inside the map's own frame, around the picture.
const INSET: f32 = 6.0;
/// Alpha of the veil over the parts of the map that are off screen.
const VEIL_ALPHA: u8 = 130;
/// Below this the canvas is small enough that the map would be an obstruction:
/// a fraction of a small canvas is still a large part of it.
const MIN_CANVAS: Vec2 = Vec2::new(220.0, 160.0);

/// Where the map should be drawn, or `None` when it should not be.
///
/// `picture` is the whole picture as it is drawn now, and `area` the canvas it
/// is drawn in.
pub fn target(app: &PlayerApp, area: Rect, picture: Rect) -> Option<Rect> {
    if !app.settings.minimap {
        return None;
    }
    // Fullscreen with the controls away is "just the film, please": a map is
    // chrome like any other, and it goes away with the rest of it.
    if app.ui.fullscreen && !app.ui.controls_visible() {
        return None;
    }
    if !view::is_cropped(picture, area) {
        return None;
    }
    layout(area, picture)
}

/// The rectangle the map takes in the corner, for a picture of this shape.
///
/// Sized from the picture's own proportions, so that the small copy has the
/// same shape as the thing it is a copy of — a map that letterboxes the picture
/// a second time makes the frame indicator meaningless.
fn layout(area: Rect, picture: Rect) -> Option<Rect> {
    if area.width() < MIN_CANVAS.x || area.height() < MIN_CANVAS.y {
        return None;
    }
    let aspect = (picture.width() / picture.height().max(1.0)).max(0.05);
    let mut width = (area.width() * WIDTH_FRACTION).clamp(MIN_WIDTH, MAX_WIDTH);
    let mut height = width / aspect;
    let tallest = area.height() * HEIGHT_FRACTION;
    if height > tallest {
        height = tallest;
        width = height * aspect;
    }
    let sheet = Vec2::new(width, height) + Vec2::splat(2.0 * INSET);
    let corner = egui::pos2(area.right() - MARGIN, area.bottom() - MARGIN);
    Some(Rect::from_min_max(corner - sheet, corner))
}

/// The point of the picture (`0..=1` on both axes) that a press at `position`
/// asks to see in the middle of the canvas, or `None` when the press is not on
/// the map.
///
/// The map does not take the interaction itself. It is drawn on top of the
/// canvas, but the canvas is the widget that owns the pointer — it is registered
/// first and it covers everything the map covers — and asking egui to arbitrate
/// between two overlapping widgets is asking a question whose answer is a frame
/// late and a layer deep. The canvas therefore asks *this* question instead, in
/// the one place that handles presses, and gets an answer it can act on at once.
pub fn point_on_map(sheet: Rect, position: egui::Pos2) -> Option<Vec2> {
    let inner = sheet.shrink(INSET);
    if !sheet.contains(position) {
        return None;
    }
    let size = inner.size();
    if size.x < 1.0 || size.y < 1.0 {
        return None;
    }
    Some(Vec2::new(
        ((position.x - inner.left()) / size.x).clamp(0.0, 1.0),
        ((position.y - inner.top()) / size.y).clamp(0.0, 1.0),
    ))
}

/// Paint the map: the whole picture in miniature, with the part the canvas is
/// showing marked on it.
///
/// `adjusted` is the picture adjustment pass, and it is a **parameter** rather than
/// something this module asks `app` for: the map is drawn for a still image as well,
/// and a photograph is not adjustable. When this asked the app directly, opening an
/// image with the sliders off their defaults coloured the miniature while the
/// picture beside it stayed as its author left it.
///
/// It is not interactive — see [`point_on_map`] for the press that steers it.
pub fn draw(
    app: &PlayerApp,
    ui: &egui::Ui,
    area: Rect,
    picture: Rect,
    sheet: Rect,
    tokens: &Tokens,
    adjusted: Option<Adjusted>,
) {
    let inner = sheet.shrink(INSET);
    let hovered = ui
        .ctx()
        .pointer_hover_pos()
        .is_some_and(|position| sheet.contains(position));

    surface::paint_sheet(ui.painter(), tokens, sheet, radius::SM);

    // The whole picture, in miniature. Drawn with the same rotation and the same
    // mirrors as the canvas uses, because a map that disagrees with the territory
    // about which way up it is makes the marker below it a lie.
    if let Some(texture) = &app.texture {
        image_transformed(
            ui.painter(),
            texture.id(),
            inner,
            app.rotation(),
            app.flip_h(),
            app.flip_v(),
            Color32::WHITE,
            // The same pass the canvas passes in, so the miniature and the picture
            // cannot disagree about colour any more than they disagree about
            // rotation — and so that "only video is adjustable" is decided in one
            // place, by the canvas, instead of twice with two different answers.
            adjusted,
        );
    }

    // What the canvas is showing, in the picture's own coordinates.
    let shown = shown_part(area, picture);
    let window = Rect::from_min_max(
        inner.min + shown.min.to_vec2() * inner.size(),
        inner.min + shown.max.to_vec2() * inner.size(),
    );

    // Everything that is *not* on screen is veiled, so the marker reads as a
    // window onto the map rather than as one more rectangle on it.
    let painter = ui.painter();
    if shown.min != egui::pos2(0.0, 0.0) || shown.max != egui::pos2(1.0, 1.0) {
        let veil = Color32::from_black_alpha(VEIL_ALPHA);
        let zero = CornerRadius::ZERO;
        for band in [
            Rect::from_min_max(inner.min, egui::pos2(inner.right(), window.top())),
            Rect::from_min_max(egui::pos2(inner.left(), window.bottom()), inner.max),
            Rect::from_min_max(
                egui::pos2(inner.left(), window.top()),
                egui::pos2(window.left(), window.bottom()),
            ),
            Rect::from_min_max(
                egui::pos2(window.right(), window.top()),
                egui::pos2(inner.right(), window.bottom()),
            ),
        ] {
            if band.width() > 0.0 && band.height() > 0.0 {
                painter.rect_filled(band, zero, veil);
            }
        }
    }

    painter.rect_stroke(
        window,
        CornerRadius::ZERO,
        Stroke::new(
            1.5_f32,
            tokens
                .accent
                .gamma_multiply(if hovered { 1.0 } else { 0.75 }),
        ),
        StrokeKind::Inside,
    );
}

/// The part of `picture` that `area` is showing, in picture coordinates
/// (`0..=1` on both axes).
///
/// The picture is always the *whole* picture — the rectangle it is drawn into is
/// what the canvas crops, not what it letterboxes — so this is a plain divide of
/// the two rectangles, with both ends held inside the picture.
pub fn shown_part(area: Rect, picture: Rect) -> Rect {
    let size = picture.size().max(Vec2::splat(1.0));
    let x = |value: f32| ((value - picture.left()) / size.x).clamp(0.0, 1.0);
    let y = |value: f32| ((value - picture.top()) / size.y).clamp(0.0, 1.0);
    Rect::from_min_max(
        egui::pos2(x(area.left()), y(area.top())),
        egui::pos2(x(area.right()), y(area.bottom())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas() -> Rect {
        Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(1000.0, 600.0))
    }

    /// The map is sized to the picture, and it stays in the corner it was put in.
    #[test]
    fn the_map_takes_its_shape_from_the_picture_and_its_place_from_the_canvas() {
        let area = canvas();
        let picture = Rect::from_center_size(area.center(), Vec2::new(2000.0, 1125.0));
        let sheet = layout(area, picture).expect("a 1000x600 canvas has room for a map");
        assert_eq!(sheet.right(), area.right() - MARGIN);
        assert_eq!(sheet.bottom(), area.bottom() - MARGIN);
        let frame = sheet.shrink(INSET);
        assert!(
            (frame.width() / frame.height() - 16.0 / 9.0).abs() < 0.01,
            "the map has the picture's proportions, not the canvas'"
        );
    }

    /// A portrait picture runs into the height limit before the width one, and
    /// has to come back narrower to keep its shape.
    #[test]
    fn a_tall_picture_is_shortened_rather_than_squashed() {
        let area = canvas();
        let picture = Rect::from_center_size(area.center(), Vec2::new(600.0, 2400.0));
        let frame = layout(area, picture).expect("room").shrink(INSET);
        assert!((frame.width() / frame.height() - 0.25).abs() < 0.01);
        assert!(frame.height() <= area.height() * HEIGHT_FRACTION + 0.01);
    }

    /// A canvas with no room for one gets none: the map must never be the
    /// largest thing on screen.
    #[test]
    fn a_small_canvas_gets_no_map() {
        let small = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(180.0, 120.0));
        let picture = Rect::from_center_size(small.center(), Vec2::new(360.0, 120.0));
        assert_eq!(layout(small, picture), None);
    }

    #[test]
    fn the_shown_part_is_the_whole_picture_when_nothing_is_cropped() {
        let area = canvas();
        let picture = Rect::from_center_size(area.center(), Vec2::new(800.0, 450.0));
        assert_eq!(
            shown_part(area, picture),
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
        );
    }

    #[test]
    fn the_shown_part_measures_the_crop() {
        let area = canvas();
        // Four times the canvas, centred: the middle quarter is on screen.
        let picture = Rect::from_center_size(area.center(), Vec2::new(4000.0, 2400.0));
        let shown = shown_part(area, picture);
        assert!((shown.min.x - 0.375).abs() < 1e-4);
        assert!((shown.min.y - 0.375).abs() < 1e-4);
        assert!((shown.max.x - 0.625).abs() < 1e-4);
        assert!((shown.max.y - 0.625).abs() < 1e-4);
    }

    /// A press on the map has to translate into the picture under it, and a press
    /// beside it has to translate into nothing at all.
    #[test]
    fn a_press_on_the_map_names_the_picture_point_under_it() {
        let area = canvas();
        let picture = Rect::from_center_size(area.center(), Vec2::new(2000.0, 1500.0));
        let sheet = layout(area, picture).expect("room");
        let inner = sheet.shrink(INSET);

        assert_eq!(point_on_map(sheet, inner.center()), Some(Vec2::splat(0.5)));
        assert_eq!(point_on_map(sheet, inner.min), Some(Vec2::ZERO));
        assert_eq!(point_on_map(sheet, inner.max), Some(Vec2::splat(1.0)));
        assert_eq!(point_on_map(sheet, area.center()), None, "not on the map");
        // The frame around the picture counts as the map: a press there travels
        // to the nearest edge rather than doing nothing.
        assert_eq!(point_on_map(sheet, sheet.min), Some(Vec2::ZERO));
    }
}
