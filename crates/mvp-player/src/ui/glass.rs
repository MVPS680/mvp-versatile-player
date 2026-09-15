//! Liquid Glass: the translucent material the chrome is made of.
//!
//! Apple's Liquid Glass is a *material*, not a colour. It borrows the light of
//! whatever sits behind it, gathers a rim of that light around its own edge, shows
//! its thickness as a bevel just inside that rim, floats on a soft halo, and
//! answers the pointer with a highlight that travels across the surface.
//!
//! **What this module can and cannot do.** A `glow`-backed `egui` never sees the
//! framebuffer, so the backdrop cannot be sampled, which means it cannot be
//! blurred or refracted: there is no true `BackdropFilter` here, and pretending
//! otherwise would be a lie in the comments. What *is* reproducible is everything
//! the eye reads as glass at a glance, drawn from a few painter primitives:
//!
//! 1. a translucent fill, graded from lighter at the top to denser at the bottom
//!    (the backdrop stays visible through it — that is what makes it glass rather
//!    than a dark panel),
//! 2. a bright rim on the lit edge, a dim one opposite it, and a hot spot in the
//!    middle of the lit edge where the light source reflects,
//! 3. an inner bevel just inside that rim, where the material has thickness,
//! 4. a halo gathered around the shape, so it floats instead of sitting on the
//!    picture,
//! 5. a specular pool that follows the pointer, which is what makes the surface
//!    feel like it is answering rather than merely blending.
//!
//! Cost is a handful of shapes per surface: no extra dependency, no shader, and no
//! per-frame allocation beyond a few vertices.

use egui::{Color32, CornerRadius, Frame, Margin, Painter, Pos2, Rect, Shape, Stroke, Ui, Vec2};

use crate::theme::Tokens;

/// How much of a surface is glass, which decides its density and its rims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thickness {
    /// Docked chrome: the menu bar, the transport bar, the sidebar. It sits over
    /// the window's own background, so it can afford to be the densest.
    Chrome,
    /// Floating surfaces over the picture: sheets, dialogs, the HUD.
    Float,
    /// Small capsules: buttons, the segmented control, a hovered list row.
    Pill,
}

/// Which edges of a shape catch the light.
///
/// A flush bar is only *cut* on the side that faces content — a bright line along
/// the top of a menu bar would sit on the window frame and read as a mistake —
/// while a floating sheet is cut on all four sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rim {
    /// The top edge.
    pub top: bool,
    /// The bottom edge.
    pub bottom: bool,
    /// The left edge.
    pub left: bool,
    /// The right edge.
    pub right: bool,
}

impl Rim {
    /// All four edges: floating surfaces.
    pub const ALL: Rim = Rim {
        top: true,
        bottom: true,
        left: true,
        right: true,
    };
    /// Only the edge facing the content, for a bar docked below the picture.
    pub const TOP: Rim = Rim {
        top: true,
        bottom: false,
        left: false,
        right: false,
    };
    /// Only the edge facing the content, for a bar docked above it.
    pub const BOTTOM: Rim = Rim {
        top: false,
        bottom: true,
        left: false,
        right: false,
    };
    /// Only the edge facing the picture, for a sidebar on the right.
    pub const LEFT: Rim = Rim {
        top: false,
        bottom: false,
        left: true,
        right: false,
    };
}

/// A glass surface: how thick it is, how round, and which edges are lit.
#[derive(Debug, Clone, Copy)]
pub struct Glass {
    /// Density and rim brightness.
    pub thickness: Thickness,
    /// Corner radius of the shape.
    pub radius: f32,
    /// Lit edges.
    pub rim: Rim,
    /// Whether the material answers the pointer with a specular pool.
    pub interactive: bool,
}

impl Glass {
    /// A docked bar: dense, square-cornered, lit on `rim`.
    pub fn chrome(rim: Rim) -> Self {
        Self {
            thickness: Thickness::Chrome,
            radius: 0.0,
            rim,
            interactive: false,
        }
    }

    /// A floating surface: sheets, dialogs, the HUD.
    pub fn float(radius: f32) -> Self {
        Self {
            thickness: Thickness::Float,
            radius,
            rim: Rim::ALL,
            interactive: true,
        }
    }

    /// A small capsule: buttons and pills.
    pub fn pill(radius: f32) -> Self {
        Self {
            thickness: Thickness::Pill,
            radius,
            rim: Rim::ALL,
            interactive: false,
        }
    }

    /// The same shape, as a capsule for a control of the given height.
    pub fn capsule(height: f32) -> Self {
        Self::pill(height / 2.0)
    }

    /// Turn the pointer response on or off.
    pub fn interactive(mut self, on: bool) -> Self {
        self.interactive = on;
        self
    }
}

/// The two ends of the fill gradient, for a given thickness of glass.
///
/// Both are translucent — a glass surface that hides its backdrop is a panel — and
/// the bottom is the denser end, because the light comes from above.
fn fill_ends(thickness: Thickness, tokens: &Tokens) -> (Color32, Color32) {
    let (base, top, bottom) = match thickness {
        Thickness::Chrome => (tokens.panel, 0xC4, 0xDE),
        Thickness::Float => (tokens.elevated, 0xBE, 0xDA),
        Thickness::Pill => (tokens.elevated, 0xAE, 0xCC),
    };
    let mixed =
        |alpha: u8| Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha);
    (mixed(top), mixed(bottom))
}

/// The three rim alphas of a thickness: lit edge, shadowed edge, hot spot.
///
/// The hot spot is the brightest of the three even though it is the shortest
/// line: it is the light source itself reflecting, and a reflection that is
/// fainter than the edge it sits on reads as a smudge.
fn rim_alphas(thickness: Thickness) -> (u8, u8, u8) {
    match thickness {
        Thickness::Chrome => (0x3A, 0x0E, 0x6A),
        Thickness::Float => (0x4A, 0x12, 0x7C),
        Thickness::Pill => (0x56, 0x14, 0x7E),
    }
}

/// The halo: how far each ring reaches beyond the shape, and how strong it is.
///
/// A cast shadow would be wrong here — glass does not darken what is under it, it
/// gathers light around its edge — so the halo is white and very faint.
const HALO: &[(f32, u8)] = &[(1.0, 0x12), (2.5, 0x0C), (5.0, 0x07), (8.0, 0x04)];

/// How many rings follow the pointer.
const POOL_RINGS: usize = 5;

/// Peak alpha of one ring, before the falloff is applied.
const POOL_PEAK: f32 = 0.075;

/// Radius of the specular pool for a surface of `size`, in points.
fn pool_reach(size: Vec2) -> f32 {
    (size.x.min(size.y) * 0.9).clamp(48.0, 220.0)
}

/// The rings of the specular pool, largest and faintest first.
///
/// The rings are drawn in this order so their alphas accumulate into a soft
/// falloff — cheaper and smoother than a radial gradient mesh. Returned as
/// `(radius, alpha)` so the falloff can be checked without a painter.
fn pool_rings(reach: f32, rings: usize) -> Vec<(f32, u8)> {
    let rings = rings.max(1);
    (0..rings)
        .rev()
        .map(|step| {
            let t = (step + 1) as f32 / rings as f32;
            let alpha = (255.0 * POOL_PEAK * t * t).round() as u8;
            (reach * t, alpha)
        })
        .collect()
}

/// The rect a frame's material has to cover when it is painted *before* content.
///
/// `Frame::show` hands the closure an inner rect and the material is painted from
/// inside that closure, so the margin has to be added back to reach the edge of
/// the surface. `max_rect` is the right source here: a panel knows its full rect
/// before anything has been laid out in it.
pub fn surface_rect(ui: &Ui, margin: Margin) -> Rect {
    ui.max_rect()
        .expand2(Vec2::new(margin.left as f32, margin.top as f32))
}

/// The rect a frame's material has to cover when it is painted *after* content.
///
/// Used by floating surfaces (areas and windows), whose `Ui` is given an unbounded
/// `max_rect` and only reports its real size — `min_rect` — once its content has
/// been laid out.
pub fn content_rect(ui: &Ui, margin: Margin) -> Rect {
    ui.min_rect()
        .expand2(Vec2::new(margin.left as f32, margin.top as f32))
}

/// The shell of a docked bar.
///
/// The frame carries no fill and no stroke: the bar is glass, and the material is
/// painted by [`paint`] once the bar's rect is known. An opaque fill here would
/// hide the very backdrop the glass is supposed to borrow.
pub fn chrome_shell(margin: Margin) -> Frame {
    Frame::new().inner_margin(margin)
}

/// The corner radius of a floating sheet: rounder than a card, because a sheet
/// with a Liquid Glass rim reads as thicker the rounder its corners are.
pub const SHEET_RADIUS: f32 = 20.0;

/// The inner margin of a floating sheet, matching the reach of its rim.
pub fn sheet_margin() -> Margin {
    Margin::same(crate::theme::space::LG as i8)
}

/// The shell of a floating window.
///
/// `egui::Window` paints its own frame, so the base fill, the corner radius, the
/// shadow and a baseline rim have to come from the frame; everything that belongs
/// *over* the content — the graded veil, the bevel, the lit rim and the pointer's
/// highlight — is added afterwards by [`paint_overlay_ui`], using the rect the
/// window reports.
pub fn window_shell(tokens: &Tokens, margin: Margin, radius: f32) -> Frame {
    let (lit, _, _) = rim_alphas(Thickness::Float);
    Frame::new()
        .fill(base_fill(tokens, Thickness::Float))
        .corner_radius(CornerRadius::same(radius.clamp(0.0, 255.0) as u8))
        .stroke(Stroke::new(
            1.0_f32,
            Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, lit),
        ))
        .inner_margin(margin)
        .shadow(crate::theme::shadow::window())
}

/// The colour a `Frame` should use as the *base* under the material, for the
/// surfaces whose frame is drawn by `egui` itself (windows and areas).
///
/// It is the top end of the gradient: the frame's fill is painted behind the
/// content, and [`paint_overlay_ui`] then grades it downwards on top.
pub fn base_fill(tokens: &Tokens, thickness: Thickness) -> Color32 {
    fill_ends(thickness, tokens).0
}

/// The material, as shapes, for a caller that has to paint it *behind* something.
///
/// `Painter::add` paints immediately and therefore on top; a caller that only
/// learns a widget's rect *after* the widget has been drawn — the sliding
/// highlight of a menu bar is exactly that — reserves a `ShapeIdx` first and fills
/// it in afterwards with `Painter::set`. That is only possible with shapes in
/// hand, which is why the material is built before it is painted.
///
/// `opacity` scales every colour's alpha, so a material can fade in or out
/// without changing its geometry.
pub fn shapes(
    tokens: &Tokens,
    rect: Rect,
    glass: Glass,
    pointer: Option<Pos2>,
    base: bool,
    opacity: f32,
) -> Vec<Shape> {
    let mut out = Vec::new();
    if rect.width() <= 0.0 || rect.height() <= 0.0 || opacity <= 0.0 {
        return out;
    }
    let alpha = opacity.clamp(0.0, 1.0);
    let radius = CornerRadius::same(glass.radius.clamp(0.0, 255.0) as u8);

    halo_shapes(&mut out, rect, radius, alpha);
    if base {
        fill_shapes(&mut out, tokens, rect, radius, glass.thickness, alpha);
    } else {
        veil_shapes(&mut out, rect, radius, alpha);
    }
    bevel_shapes(&mut out, rect, radius, glass.thickness, alpha);
    if glass.interactive {
        pool_shapes(&mut out, rect, pointer, alpha);
    }
    rim_shapes(&mut out, rect, radius, glass, alpha);
    out
}

/// Paint the material over `rect`.
///
/// `base` selects which phase of the material to draw: with `true` the fill is
/// part of it (the caller has laid down no background of its own, which is the
/// case for a panel), with `false` only the parts that belong *over* the content
/// are drawn (the caller's frame has already painted the base).
pub fn paint(
    painter: &Painter,
    tokens: &Tokens,
    rect: Rect,
    glass: Glass,
    pointer: Option<Pos2>,
    base: bool,
) {
    for shape in shapes(tokens, rect, glass, pointer, base, 1.0) {
        painter.add(shape);
    }
}

/// Paint the material into a shape slot reserved earlier, so it lands *behind*
/// whatever was drawn in between.
///
/// `at` is the index `Painter::add(Shape::Noop)` returned before the widgets were
/// laid out.
pub fn paint_behind(
    ui: &Ui,
    at: egui::layers::ShapeIdx,
    tokens: &Tokens,
    rect: Rect,
    glass: Glass,
    pointer: Option<Pos2>,
    base: bool,
    opacity: f32,
) {
    let material = shapes(tokens, rect, glass, pointer, base, opacity);
    ui.painter().set(at, Shape::Vec(material));
}

/// Paint the whole material on a `Ui`, borrowing its pointer position.
///
/// For surfaces the caller lays out itself — the docked bars and the sidebar.
pub fn paint_ui(ui: &Ui, tokens: &Tokens, rect: Rect, glass: Glass) {
    let pointer = ui.ctx().pointer_hover_pos();
    paint(ui.painter(), tokens, rect, glass, pointer, true);
}

/// Paint only the parts of the material that belong *over* the content.
///
/// For floating surfaces the caller's `Frame` has already painted the base fill
/// behind the content; everything here is a surface effect — the graded veil, the
/// rim, the bevel and the pointer's highlight — so painting it last is not a
/// compromise but the correct order: light lies on top of what it falls on.
pub fn paint_overlay_ui(ui: &Ui, tokens: &Tokens, rect: Rect, glass: Glass) {
    let pointer = ui.ctx().pointer_hover_pos();
    paint(ui.painter(), tokens, rect, glass, pointer, false);
}

/// The graded veil over a surface whose base has already been painted.
///
/// A thin darkening towards the bottom: glass is denser where it is thickest, and
/// a veil is the only way to show that without hiding the content. It also helps
/// legibility, because the bottom of a bar is where its labels sit.
fn veil_shapes(out: &mut Vec<Shape>, rect: Rect, radius: CornerRadius, opacity: f32) {
    let cap = (radius.nw as f32).min(rect.height() / 2.0);
    let clear = Color32::from_rgba_unmultiplied(0x00, 0x00, 0x00, 0x00);
    let dense = Color32::from_rgba_unmultiplied(0x00, 0x00, 0x00, 0x1E).gamma_multiply(opacity);
    let body = if cap > 0.5 {
        Rect::from_min_max(
            egui::pos2(rect.min.x, rect.min.y + cap),
            egui::pos2(rect.max.x, rect.max.y - cap),
        )
    } else {
        rect
    };
    out.push(Shape::mesh(gradient_mesh(body, clear, dense)));
}

/// The light gathered around the shape, so it floats above the picture.
fn halo_shapes(out: &mut Vec<Shape>, rect: Rect, radius: CornerRadius, opacity: f32) {
    for (reach, alpha) in HALO {
        let alpha = (*alpha as f32 * opacity).round() as u8;
        if alpha == 0 {
            continue;
        }
        out.push(Shape::rect_filled(
            rect.expand(*reach),
            radius,
            Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, alpha),
        ));
    }
}

/// The graded fill: two rounded caps of the end colours with a mesh between them.
///
/// A plain gradient quad would have square corners, so the caps are drawn as part
/// of the shape and the mesh only covers the straight band in the middle, where
/// its square corners are hidden inside the rounded shape.
fn fill_shapes(
    out: &mut Vec<Shape>,
    tokens: &Tokens,
    rect: Rect,
    radius: CornerRadius,
    thickness: Thickness,
    opacity: f32,
) {
    let (top, bottom) = fill_ends(thickness, tokens);
    let top = top.gamma_multiply(opacity);
    let bottom = bottom.gamma_multiply(opacity);
    let cap = (radius.nw as f32).min(rect.height() / 2.0);
    if cap <= 0.5 {
        out.push(Shape::mesh(gradient_mesh(rect, top, bottom)));
        return;
    }
    let upper = Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + cap));
    let lower = Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - cap), rect.max);
    let middle = Rect::from_min_max(
        egui::pos2(rect.min.x, rect.min.y + cap),
        egui::pos2(rect.max.x, rect.max.y - cap),
    );
    out.push(Shape::rect_filled(upper, radius, top));
    out.push(Shape::rect_filled(lower, radius, bottom));
    out.push(Shape::mesh(gradient_mesh(middle, top, bottom)));
}

/// A vertical two-stop gradient as a single quad.
fn gradient_mesh(rect: Rect, top: Color32, bottom: Color32) -> egui::Mesh {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    mesh
}

/// The bevel: the inside of the rim, where the material shows its thickness.
///
/// Only on the lit side — a bevel all the way round would read as a second border.
fn bevel_shapes(
    out: &mut Vec<Shape>,
    rect: Rect,
    radius: CornerRadius,
    thickness: Thickness,
    opacity: f32,
) {
    let (lit, _, _) = rim_alphas(thickness);
    let alpha = ((lit / 3) as f32 * opacity).round() as u8;
    if alpha == 0 {
        return;
    }
    let inset = rect.shrink(1.5);
    let r = (radius.nw as f32 - 1.5).max(0.0);
    out.push(Shape::line(
        edge_path(inset, r, Edge::Top),
        Stroke::new(
            1.0_f32,
            Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, alpha),
        ),
    ));
}

/// The specular pool under the pointer: the surface answering the hand.
///
/// There is no clip rectangle to hold the highlight inside the shape — the pool
/// is built as circles — so it is kept in by geometry instead: it shrinks and
/// fades as the pointer approaches an edge, which is what a real highlight does
/// as the surface curves away from it.
fn pool_shapes(out: &mut Vec<Shape>, rect: Rect, pointer: Option<Pos2>, opacity: f32) {
    let Some(pointer) = pointer else {
        return;
    };
    if !rect.contains(pointer) {
        return;
    }
    let nearest = (pointer.x - rect.left())
        .min(rect.right() - pointer.x)
        .min(pointer.y - rect.top())
        .min(rect.bottom() - pointer.y)
        .max(0.0);
    let reach = pool_reach(rect.size()).min(nearest + 8.0);
    let fade = (nearest / (rect.size().min_elem() * 0.5).max(1.0)).clamp(0.25, 1.0);
    for (radius, alpha) in pool_rings(reach, POOL_RINGS) {
        let alpha = (f32::from(alpha) * opacity * fade).round() as u8;
        if alpha == 0 {
            continue;
        }
        out.push(Shape::circle_filled(
            pointer,
            radius,
            Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, alpha),
        ));
    }
}

/// The rim: the lit edge, the hot spot on it, and the dim edge opposite.
fn rim_shapes(
    out: &mut Vec<Shape>,
    rect: Rect,
    radius: CornerRadius,
    glass: Glass,
    opacity: f32,
) {
    let (lit, dim, hot) = rim_alphas(glass.thickness);
    let width = 1.0_f32;
    let r = radius.nw as f32;

    // Each lit edge is a path along that side of the rounded rectangle, corners
    // included — built as a path rather than as a clipped stroke, because a shape
    // carries no clip rectangle of its own.
    {
        let mut edge = |edge: Edge, alpha: u8| {
            let alpha = (f32::from(alpha) * opacity).round() as u8;
            if alpha == 0 {
                return;
            }
            out.push(Shape::line(
                edge_path(rect, r, edge),
                Stroke::new(
                    width,
                    Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, alpha),
                ),
            ));
        };
        if glass.rim.top {
            edge(Edge::Top, lit);
        }
        if glass.rim.bottom {
            edge(Edge::Bottom, dim);
        }
        if glass.rim.left {
            edge(Edge::Left, lit / 2);
        }
        if glass.rim.right {
            edge(Edge::Right, dim);
        }
    }

    // The hot spot: the middle of the lit edge, where the light source reflects.
    // A short brighter line is what turns a border into a reflection.
    if glass.rim.top && rect.width() > 24.0 {
        let alpha = (f32::from(hot) * opacity).round() as u8;
        if alpha > 0 {
            let span = rect.width() * 0.34;
            out.push(Shape::rect_filled(
                Rect::from_min_max(
                    egui::pos2(rect.center().x - span / 2.0, rect.top() + width / 2.0),
                    egui::pos2(rect.center().x + span / 2.0, rect.top() + width * 1.5),
                ),
                CornerRadius::same(1),
                Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, alpha),
            ));
        }
    }
}

/// One side of a rounded rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    /// The side the light comes from.
    Top,
    /// The side in shadow.
    Bottom,
    /// The left side.
    Left,
    /// The right side.
    Right,
}

/// Points along one side of a rounded rectangle, corners included.
///
/// Angles are measured clockwise from the positive X axis in screen space, where
/// Y grows downwards: `0` is East, `π/2` is South.
fn edge_path(rect: Rect, radius: f32, edge: Edge) -> Vec<Pos2> {
    /// Segments per corner arc. Eight reads as round at the sizes the chrome uses
    /// and keeps the shape count of a whole bar in the dozens.
    const ARC: usize = 8;

    use std::f32::consts::PI;
    let r = radius.clamp(0.0, rect.width().min(rect.height()) / 2.0);
    let arc = |center: Pos2, from: f32, to: f32| -> Vec<Pos2> {
        (0..=ARC)
            .map(|step| {
                let t = step as f32 / ARC as f32;
                let angle = from + (to - from) * t;
                Pos2::new(center.x + angle.cos() * r, center.y + angle.sin() * r)
            })
            .collect()
    };

    let (tl, tr, br, bl) = (
        egui::pos2(rect.left() + r, rect.top() + r),
        egui::pos2(rect.right() - r, rect.top() + r),
        egui::pos2(rect.right() - r, rect.bottom() - r),
        egui::pos2(rect.left() + r, rect.bottom() - r),
    );
    let mut points = Vec::with_capacity(ARC * 2 + 3);
    match edge {
        Edge::Top => {
            points.extend(arc(tl, PI, 1.5 * PI));
            points.push(egui::pos2(rect.right() - r, rect.top()));
            points.extend(arc(tr, 1.5 * PI, 2.0 * PI));
        }
        Edge::Bottom => {
            points.extend(arc(br, 0.0, 0.5 * PI));
            points.push(egui::pos2(rect.left() + r, rect.bottom()));
            points.extend(arc(bl, 0.5 * PI, PI));
        }
        Edge::Left => {
            points.extend(arc(tl, 1.5 * PI, PI));
            points.push(egui::pos2(rect.left(), rect.bottom() - r));
            points.extend(arc(bl, PI, 0.5 * PI));
        }
        Edge::Right => {
            points.extend(arc(tr, 1.5 * PI, 2.0 * PI));
            points.push(egui::pos2(rect.right(), rect.bottom() - r));
            points.extend(arc(br, 0.0, 0.5 * PI));
        }
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> Tokens {
        Tokens::default()
    }

    /// Glass has to stay see-through: the moment a fill reaches full opacity it is
    /// a panel again, and the picture stops glowing through the chrome.
    #[test]
    fn the_fill_never_hides_the_backdrop() {
        for thickness in [Thickness::Chrome, Thickness::Float, Thickness::Pill] {
            let (top, bottom) = fill_ends(thickness, &tokens());
            assert!(top.a() > 0 && top.a() < 255, "{thickness:?} top is opaque");
            assert!(
                bottom.a() > 0 && bottom.a() < 255,
                "{thickness:?} bottom is opaque"
            );
            // Light from above: the bottom of a piece of glass is the denser end.
            assert!(
                bottom.a() > top.a(),
                "{thickness:?} is not graded from light to dense"
            );
        }
    }

    /// Floating glass can afford to be lighter than docked chrome, because it has
    /// the picture behind it to borrow from.
    #[test]
    fn floating_glass_is_lighter_than_docked_chrome() {
        let tokens = tokens();
        let (chrome_top, _) = fill_ends(Thickness::Chrome, &tokens);
        let (float_top, _) = fill_ends(Thickness::Float, &tokens);
        assert!(float_top.a() < chrome_top.a());
    }

    /// The lit edge must outshine the shadowed one and the hot spot must be the
    /// brightest thing on the rim; otherwise the material reads as a flat border
    /// lit from nowhere.
    #[test]
    fn the_rim_is_lit_from_above() {
        for thickness in [Thickness::Chrome, Thickness::Float, Thickness::Pill] {
            let (lit, dim, hot) = rim_alphas(thickness);
            assert!(dim < lit, "{thickness:?} has no lit/shadowed contrast");
            assert!(lit <= hot, "{thickness:?} has no hot spot");
            assert!(hot < 128, "{thickness:?} rim would be a white outline");
        }
    }

    /// The specular pool fades outwards. Each ring is a *shell*, so what the eye
    /// sees at a distance from the pointer is the sum of every ring that reaches
    /// that far — which is why the test accumulates them instead of comparing the
    /// rings one against the next.
    #[test]
    fn the_specular_pool_fades_outwards() {
        let rings = pool_rings(120.0, 5);
        assert_eq!(rings.len(), 5);
        // Largest first, so the shells are laid down from the outside in.
        for pair in rings.windows(2) {
            assert!(
                pair[0].0 > pair[1].0,
                "rings must be drawn largest first: {rings:?}"
            );
        }
        let accumulated = |at: f32| -> u32 {
            rings
                .iter()
                .filter(|(radius, _)| *radius > at)
                .map(|(_, alpha)| u32::from(*alpha))
                .sum()
        };
        for pair in [0.0_f32, 24.0, 48.0, 72.0, 96.0].windows(2) {
            assert!(
                accumulated(pair[0]) > accumulated(pair[1]),
                "the pool must fade outwards: {rings:?}"
            );
        }
        // A sheen, never a spotlight: even directly under the pointer the pool
        // stays under a quarter of full white.
        assert!(accumulated(0.0) < 64, "{rings:?}");
    }

    /// The pool of a large surface has to stay proportional, and a tiny one still
    /// gets a visible highlight.
    #[test]
    fn the_pool_keeps_a_sane_radius() {
        assert!(pool_reach(Vec2::new(4.0, 4.0)) >= 48.0);
        assert!(pool_reach(Vec2::new(4000.0, 2000.0)) <= 220.0);
        // Degenerate input must not produce NaN or a negative radius.
        assert!(pool_rings(0.0, 0).iter().all(|(radius, _)| *radius >= 0.0));
    }

    /// A capsule is round by definition, so the shape and the material have to
    /// agree on the radius — a "capsule" with square corners is a rectangle.
    #[test]
    fn a_capsule_is_half_a_control_tall() {
        let glass = Glass::capsule(30.0);
        assert_eq!(glass.radius, 15.0);
        assert_eq!(glass.thickness, Thickness::Pill);
        assert!(glass.rim.top && glass.rim.bottom && glass.rim.left && glass.rim.right);
    }
}
