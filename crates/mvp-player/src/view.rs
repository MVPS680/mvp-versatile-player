//! Where the picture goes on the canvas.
//!
//! A video is *fitted* to the canvas — [`crate::ui::canvas`] decides that
//! rectangle from the source size, the aspect setting and the rotation — and this
//! module is what the user is allowed to do to that answer afterwards: scale it
//! about a point, and slide it around inside the canvas. Zooming a picture that
//! cannot be moved again is only half a feature, because the first notch of the
//! wheel puts the interesting part off screen.
//!
//! The image viewer keeps its own state ([`ImageView`]: fit mode, rotation,
//! flips and pan) because a still needs all of that and a video needs none of it;
//! the arithmetic below is shared so that a drag stops at the same edge in both
//! modes, and so that the bird's-eye view steers both with the same line of code.

use egui::{Rect, Vec2};
use mvp_core::ImageView;

/// Smallest zoom factor. A tenth of the fitted size is a stamp, which is as far
/// out as this is useful.
pub const MIN_ZOOM: f32 = 0.1;

/// Largest zoom factor. Sixteen times the fitted size of a 1080p video in a
/// 1080p window is roughly a hundred source pixels across the window.
pub const MAX_ZOOM: f32 = 16.0;

/// The picture as the canvas last drew it.
///
/// Written by the canvas while it draws, and read by everything that has to
/// reason about the picture without being inside it: the zoom keys and the menu
/// (which run before the canvas is laid out, and have no rectangle to measure),
/// and the bird's-eye view (which is in the corner, not over the picture).
///
/// The rectangle is the *current* one — zoom and pan included — because that is
/// what the size of the picture on screen is; dividing it by the zoom gives the
/// fitted rectangle back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasPicture {
    /// The picture's rectangle on screen.
    pub rect: Rect,
    /// The canvas that rectangle was drawn into.
    pub canvas: Rect,
}

/// Zoom and pan applied to the picture on the canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasView {
    /// Scale relative to "fitted": `1.0` is exactly what `适应窗口` gives.
    zoom: f32,
    /// Offset of the picture's centre from the canvas' centre, in points.
    pan: Vec2,
}

impl Default for CanvasView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: Vec2::ZERO,
        }
    }
}

impl CanvasView {
    /// A view that shows the picture exactly as it was fitted.
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` while nothing has been zoomed or dragged.
    pub fn is_fitted(&self) -> bool {
        self.zoom == 1.0 && self.pan == Vec2::ZERO
    }

    /// Back to "适应窗口".
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The size the picture has when it is *not* zoomed, given the size it has
    /// now: the canvas knows the rectangle it drew and nothing else.
    pub fn fitted_size(&self, current: Vec2) -> Vec2 {
        current / self.zoom.max(0.001)
    }

    /// The fitted rectangle, given the one currently on screen.
    pub fn fitted_rect(&self, current: Rect, area: Rect) -> Rect {
        Rect::from_center_size(area.center(), self.fitted_size(current.size()))
    }

    /// The rectangle the picture occupies inside `area`, given the rectangle it
    /// has when it is fitted.
    pub fn rect(&self, base: Rect, area: Rect) -> Rect {
        Rect::from_center_size(area.center() + self.pan, base.size() * self.zoom)
    }

    /// Zoom by `factor`, keeping the picture point under `anchor` where it is.
    ///
    /// `anchor` is in canvas-relative points measured from the centre of the
    /// canvas — the pointer, or zero for "about the middle". Returns `true` when
    /// something moved.
    pub fn zoom_by(&mut self, factor: f32, anchor: Vec2, base: Rect, area: Rect) -> bool {
        let zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let applied = zoom / self.zoom;
        if !applied.is_finite() || applied == 1.0 {
            return false;
        }
        self.zoom = zoom;
        self.pan = anchored_pan(self.pan, anchor, applied);
        self.clamp(base, area);
        true
    }

    /// Drag the picture by a screen-space delta, stopping at the canvas' edge.
    pub fn pan_by(&mut self, delta: Vec2, base: Rect, area: Rect) {
        self.pan += delta;
        self.clamp(base, area);
    }

    /// Put the picture's centre `pan` from the canvas' centre.
    pub fn set_pan(&mut self, pan: Vec2, base: Rect, area: Rect) {
        self.pan = pan;
        self.clamp(base, area);
    }

    /// Keep the picture inside the canvas.
    fn clamp(&mut self, base: Rect, area: Rect) {
        self.pan = clamp_pan(self.pan, base.size() * self.zoom, area);
    }
}

/// The pan that keeps the picture point under `anchor` where it is while the
/// picture grows or shrinks by `factor`.
///
/// A zoom that anchors on the pointer is the difference between finding the
/// detail you were looking at and losing it: the picture is scaled about the
/// middle of the canvas, and this moves it back so that the point under the
/// cursor stays under the cursor. With `anchor` at zero it is the plain
/// centre-anchored zoom, which is what the keyboard asks for.
pub fn anchored_pan(pan: Vec2, anchor: Vec2, factor: f32) -> Vec2 {
    pan - (anchor - pan) * (factor - 1.0)
}

/// The pan that puts picture point `q` (`0..=1` on both axes) in the middle of
/// the canvas. This is what a click on the bird's-eye view means.
pub fn pan_for_centre(q: Vec2, size: Vec2) -> Vec2 {
    (Vec2::splat(0.5) - q) * size
}

/// Hold `pan` so that the picture cannot be dragged away from the canvas.
///
/// A picture bigger than the canvas may be moved until its own edge meets the
/// canvas' edge and no further, so there is never a band of empty canvas beside
/// it; one smaller than the canvas has nowhere to go and is centred. Without
/// this a flick of the wheel leaves a photograph parked in a corner of the
/// window, and the only way back is a command the eye is not looking for.
pub fn clamp_pan(pan: Vec2, size: Vec2, area: Rect) -> Vec2 {
    let room = ((size - area.size()) / 2.0).max(Vec2::ZERO);
    Vec2::new(pan.x.clamp(-room.x, room.x), pan.y.clamp(-room.y, room.y))
}

/// `true` when the picture is bigger than the canvas, so part of it is off
/// screen and there is something for a bird's-eye view to point at.
pub fn is_cropped(picture: Rect, area: Rect) -> bool {
    picture.width() > area.width() + 0.5 || picture.height() > area.height() + 0.5
}

/// Where the image viewer's picture goes inside `area`.
///
/// The one place the still's on-screen rectangle is worked out — the canvas
/// draws it, the zoom keys anchor on it, the drag clamps against it and the
/// bird's-eye view measures against it, and four copies of this arithmetic would
/// drift apart.
pub fn image_rect(image: &ImageView, area: Rect) -> Option<Rect> {
    let (width, height) = image.dimensions()?;
    let scale = image.effective_scale(Some((area.width(), area.height())));
    let size = Vec2::new(width as f32 * scale, height as f32 * scale);
    let offset = Vec2::new(image.offset.0, image.offset.1);
    Some(Rect::from_center_size(area.center() + offset, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1000x600 canvas with its top-left at the origin.
    fn area() -> Rect {
        Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(1000.0, 600.0))
    }

    /// A picture that fills the canvas when it is fitted — the case where a zoom
    /// has somewhere to go in both axes. Centred on the canvas, which is where
    /// the canvas puts every picture it fits.
    fn base_16_9() -> Rect {
        Rect::from_center_size(area().center(), Vec2::new(1000.0, 562.5))
    }

    #[test]
    fn a_fresh_view_draws_the_picture_where_it_was_fitted() {
        let view = CanvasView::new();
        assert!(view.is_fitted());
        assert_eq!(view.rect(base_16_9(), area()), base_16_9());
    }

    #[test]
    fn zoom_is_relative_to_the_fitted_size_and_bounded() {
        let mut view = CanvasView::new();
        let (base, area) = (base_16_9(), area());
        assert!(view.zoom_by(2.0, Vec2::ZERO, base, area));
        assert_eq!(view.rect(base, area).size(), base.size() * 2.0);

        view.zoom_by(1000.0, Vec2::ZERO, base, area);
        assert_eq!(
            view.rect(base, area).size(),
            base.size() * MAX_ZOOM,
            "a zoom stops at the far end of the range"
        );
        view.zoom_by(0.00001, Vec2::ZERO, base, area);
        assert_eq!(
            view.rect(base, area).size(),
            base.size() * MIN_ZOOM,
            "and at the near end"
        );
    }

    /// The point the wheel is over is the point that has to stay. This is the
    /// whole reason the zoom takes an anchor at all.
    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let mut view = CanvasView::new();
        let (base, area) = (base_16_9(), area());
        let pointer = Vec2::new(300.0, 0.0);

        // The picture point under the pointer before the zoom, in the picture's
        // own coordinates: 0..=1 across the rectangle on screen.
        let before = view.rect(base, area);
        let q = (area.center() + pointer - before.min) / before.size();

        view.zoom_by(2.0, pointer, base, area);

        let after = view.rect(base, area);
        let landed = after.min + q * after.size() - area.center();
        assert!(
            (landed - pointer).length() < 0.01,
            "the point under the pointer moved to {landed:?}"
        );
    }

    #[test]
    fn a_zoom_smaller_than_the_canvas_stays_centred() {
        let mut view = CanvasView::new();
        let (base, area) = (base_16_9(), area());
        view.zoom_by(0.5, Vec2::new(300.0, 0.0), base, area);
        assert_eq!(
            view.rect(base, area).center(),
            area.center(),
            "there is nowhere to pan to"
        );
    }

    #[test]
    fn a_zoomed_picture_cannot_be_dragged_off_the_canvas() {
        let mut view = CanvasView::new();
        let (base, area) = (base_16_9(), area());
        view.zoom_by(2.0, Vec2::ZERO, base, area);

        // Far past the edge in both directions, and then back the other way.
        view.pan_by(Vec2::new(4000.0, 4000.0), base, area);
        let rect = view.rect(base, area);
        assert_eq!(rect.left(), area.left());
        assert_eq!(rect.top(), area.top(), "no empty canvas above the picture");

        view.pan_by(Vec2::new(-4000.0, -4000.0), base, area);
        let rect = view.rect(base, area);
        assert_eq!(rect.right(), area.right());
        assert_eq!(rect.bottom(), area.bottom());
    }

    #[test]
    fn panning_inside_the_canvas_is_left_alone() {
        let mut view = CanvasView::new();
        let (base, area) = (base_16_9(), area());
        view.zoom_by(2.0, Vec2::ZERO, base, area);
        view.pan_by(Vec2::new(120.0, -80.0), base, area);
        assert_eq!(
            view.rect(base, area).center(),
            area.center() + Vec2::new(120.0, -80.0)
        );
    }

    #[test]
    fn a_picture_that_fits_is_not_cropped_and_a_zoomed_one_is() {
        let (base, area) = (base_16_9(), area());
        assert!(!is_cropped(base, area));
        assert!(is_cropped(
            Rect::from_center_size(area.center(), base.size() * 1.5),
            area
        ));
    }

    #[test]
    fn the_fitted_rectangle_comes_back_out_of_a_zoom() {
        let mut view = CanvasView::new();
        let (base, area) = (base_16_9(), area());
        view.zoom_by(3.0, Vec2::new(200.0, 40.0), base, area);
        let current = view.rect(base, area);
        let recovered = view.fitted_rect(current, area);
        assert!((recovered.size() - base.size()).length() < 0.01);
    }

    #[test]
    fn the_centre_of_the_map_is_the_centre_of_the_picture() {
        let size = Vec2::new(800.0, 600.0);
        assert_eq!(pan_for_centre(Vec2::splat(0.5), size), Vec2::ZERO);
        assert_eq!(pan_for_centre(Vec2::ZERO, size), Vec2::new(400.0, 300.0));
        assert_eq!(
            pan_for_centre(Vec2::splat(1.0), size),
            Vec2::new(-400.0, -300.0)
        );
    }

    #[test]
    fn a_clamped_pan_never_leaves_a_gap() {
        let size = Vec2::new(800.0, 600.0);
        // A picture smaller than the canvas is pinned to the middle.
        assert_eq!(clamp_pan(Vec2::new(50.0, 50.0), size, area()), Vec2::ZERO);
        // A bigger one stops with its edge on the canvas' edge.
        let big = Vec2::new(2000.0, 1200.0);
        assert_eq!(
            clamp_pan(Vec2::new(500.0, -400.0), big, area()),
            Vec2::new(500.0, -300.0)
        );
    }
}
