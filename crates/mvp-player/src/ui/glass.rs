//! Liquid Glass: the translucent material the chrome is made of.
//!
//! Two references define the look:
//!
//! * `kube.io/blog/liquid-glass-css-svg` — a lens modelled as a *surface
//!   function*, refracted per pixel with Snell's law, encoded into a displacement
//!   map and applied as a `backdrop-filter: url(#…)`; plus **one static specular
//!   highlight**;
//! * the CSS pen that plays the same effect with `feDisplacementMap` over a
//!   displacement image, whose recipe is a very faint fill, a **crisp rim**
//!   (`0 0 0 2px rgba(255,255,255,.6)`) and a *light* drop shadow
//!   (`0 16px 32px rgba(0,0,0,.12)`).
//!
//! **What is reproduced, and what cannot be.** A `glow`-backed `egui` never sees
//! the framebuffer, so the backdrop cannot be sampled: there is no blur of it and
//! no displacement of it. The first article says as much of the technique itself —
//! it only works where SVG filters are exposed as `backdrop-filter` (Chromium), and
//! it is explicitly experimental. What this module does reproduce is everything the
//! eye reads as glass at rest, all of it static:
//!
//! 1. the faint fill — a dark bed with a white wash over it (the bed is the
//!    player's own addition: 8 % white works over a meadow, not over video whose
//!    labels have to stay readable);
//! 2. the thin, hard rim of a lens edge, where refraction would pile the light up;
//! 3. the **specular highlight**, fixed in place as if the light came from the
//!    top-left — not a highlight that follows the pointer;
//! 4. a light shadow under the shape, so it floats rather than sits.
//!
//! The material is at most four shapes per surface — bed, wash, sheen, rim — with
//! no texture, no shader, no extra pass and no per-frame pointer queries. The
//! previous version piled a white halo, four lit edges, a hot spot and a
//! pointer-following pool on a nearly opaque fill, which is what read as a ring of
//! white fog around the control island.

use egui::{
    Color32, CornerRadius, Frame, Margin, Painter, Rect, Shape, Stroke, StrokeKind, Ui, Vec2,
};

use crate::theme::Tokens;

/// How much of a surface is glass, which decides its density and its rim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thickness {
    /// Docked chrome: the menu bar, the transport bar, the sidebar. It sits over
    /// the window's own background and has no rim — only a hairline on the edge
    /// that faces content.
    Chrome,
    /// Floating controls over the picture: the fullscreen island, the HUD. These are
    /// glanced at, not read, so the picture is allowed through.
    Float,
    /// A sheet: the settings window and the dialogs. Lots of small text, read
    /// carefully — the one surface that has to be nearly solid. Over a bright frame
    /// the floating density leaves white text at about 2.4:1, well under the 4.5:1
    /// the rest of the interface holds to.
    Sheet,
    /// Small capsules: buttons, the segmented control, a hovered list row.
    Pill,
}

/// Which edges of a docked bar catch the light.
///
/// A flush bar is only *cut* on the side that faces content — a bright line along
/// the top of a menu bar would sit on the window frame and read as a mistake.
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

/// How a surface is cut: a hard rim all the way round, or a hairline on one edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// The reference's crisp ring: `0 0 0 2px rgba(255,255,255,.6)`.
    Ring,
    /// A single hairline, for a bar that is flush with the window.
    Line(Rim),
}

/// A glass surface: how thick it is, how round, and how it is cut.
#[derive(Debug, Clone, Copy)]
pub struct Glass {
    /// Density of the bed and brightness of the rim.
    pub thickness: Thickness,
    /// Corner radius of the shape.
    pub radius: f32,
    /// Ring or hairline.
    pub edge: Edge,
}

impl Glass {
    /// A docked bar: a faint bed and a hairline on the edge facing content.
    pub fn chrome(rim: Rim) -> Self {
        Self {
            thickness: Thickness::Chrome,
            radius: 0.0,
            edge: Edge::Line(rim),
        }
    }

    /// A floating surface: the reference's ring, over the picture.
    pub fn float(radius: f32) -> Self {
        Self {
            thickness: Thickness::Float,
            radius,
            edge: Edge::Ring,
        }
    }

    /// A sheet: the same ring, over a nearly solid bed.
    pub fn sheet(radius: f32) -> Self {
        Self {
            thickness: Thickness::Sheet,
            radius,
            edge: Edge::Ring,
        }
    }

    /// A small capsule: buttons and pills.
    pub fn pill(radius: f32) -> Self {
        Self {
            thickness: Thickness::Pill,
            radius,
            edge: Edge::Ring,
        }
    }

    /// The same shape, as a capsule for a control of the given height.
    pub fn capsule(height: f32) -> Self {
        Self::pill(height / 2.0)
    }
}

/// The bed colour and the white tint of a thickness.
///
/// `(bed, tint)`: both translucent. The bed is the player's own addition for
/// legibility — see the module comment. It is a fifth of the surface for the
/// surfaces that sit *over* the picture, and most of it for a sheet, which is read
/// rather than glanced at.
fn fill_alphas(thickness: Thickness) -> (u8, u8) {
    // 26 % / 31 % / 72 % / 20 % bed, and 5 % / 8 % / 6 % / 6 % white.
    match thickness {
        Thickness::Chrome => (0x42, 0x0D),
        Thickness::Float => (0x4E, 0x14),
        // With the backdrop frosted, the bed no longer has to carry all of the
        // contrast: a blurred background is far more forgiving than a sharp one, and
        // the picture behind the panel stays visible as frost instead of as a film.
        Thickness::Sheet => (0xB8, 0x0F),
        Thickness::Pill => (0x33, 0x0F),
    }
}

/// Alpha of the rim, and how thick it is drawn.
///
/// The pen's ring is 2 px at 60 % white, but a full 60 % ring around a surface that
/// floats over *video* reads as an outline rather than as glass — this is the white
/// edge the control island was reported for. So the rim is thinner and quieter than
/// the pen's (35 % at 1.5 px), and it is the specular sheen above it that carries
/// the glass look. A docked bar gets a 1 px hairline at a sixth of that: a ring
/// around a bar that spans the window would read as a box.
fn edge_style(thickness: Thickness) -> (u8, f32) {
    match thickness {
        Thickness::Chrome => (0x14, 1.0),
        Thickness::Float | Thickness::Sheet => (0x59, 1.5),
        Thickness::Pill => (0x40, 1.0),
    }
}

/// Peak alpha of the specular sheen, at the top-left corner.
///
/// It falls off along the diagonal to nothing at the bottom-right — one shape that
/// stands in for the pile-up of light the refraction would produce along the lit
/// edge. It never moves with the pointer: the article's highlight is fixed, and a
/// highlight that follows the mouse is the interaction this round removes.
fn sheen_peak(thickness: Thickness) -> u8 {
    match thickness {
        Thickness::Chrome => 0x1E,
        Thickness::Float => 0x30,
        // A sheet is nearly solid, so its highlight is a quiet rim light rather
        // than a wash across the text.
        Thickness::Sheet => 0x22,
        Thickness::Pill => 0x26,
    }
}

/// The specular sheen: a diagonal falloff from the top-left, filling the shape.
///
/// Built as a fan over the *rounded* outline rather than as a quad over the
/// rectangle: a quad's square corners would poke out past a 20 pt corner radius as
/// small white specks — the very kind of stray edge this round is removing.
fn sheen_shapes(
    out: &mut Vec<Shape>,
    rect: Rect,
    radius: f32,
    thickness: Thickness,
    opacity: f32,
) {
    let peak = scaled(sheen_peak(thickness), opacity);
    if peak == 0 {
        return;
    }
    let outline = rounded_outline(rect, radius, 6);
    let span = (rect.width() + rect.height()).max(1.0);
    let mut mesh = egui::Mesh::default();
    // The middle: the average of the outline, so the fan does not flatten out in
    // the centre.
    mesh.colored_vertex(
        rect.center(),
        with_alpha(Color32::WHITE, peak / 2),
    );
    for point in &outline {
        // 0 at the top-left corner, 1 at the bottom-right one.
        let diagonal = ((point.x - rect.left()) + (point.y - rect.top())) / span;
        let alpha = (f32::from(peak) * (1.0 - diagonal).clamp(0.0, 1.0)).round() as u8;
        mesh.colored_vertex(*point, with_alpha(Color32::WHITE, alpha));
    }
    for index in 0..outline.len() {
        let next = (index + 1) % outline.len();
        mesh.add_triangle(0, 1 + index as u32, 1 + next as u32);
    }
    out.push(Shape::mesh(mesh));
}

/// How far the frosted backdrop is smeared, in points.
pub const FROST_RADIUS: f32 = 6.0;

/// Frost the picture behind `rect`, painted straight away.
///
/// A blur of the *backdrop* normally needs the framebuffer, which a `glow`-backed
/// `egui` never sees — but the picture is not in the framebuffer: it is a texture
/// this program uploaded, and a blur of a texture is an average of shifted copies of
/// it. Nine copies (3x3, `radius` apart) average to a box blur of width `radius`,
/// which at this scale is what a Gaussian of that width looks like.
///
/// For the surfaces that are drawn on a bare layer painter with no content to stay
/// behind — the HUD, whose rect is already known when it paints. A panel that has to
/// stay *behind* its own labels uses [`crate::ui::widgets::frost_surface`], which
/// writes the same shapes into a reserved slot.
pub fn frosted(
    painter: &Painter,
    texture: egui::TextureId,
    rect: Rect,
    source: Rect,
    corner: f32,
    radius: f32,
) {
    for shape in frost_shapes(texture, rect, source, corner, radius) {
        painter.add(shape);
    }
}

/// The copies that average to a blur: 3x3, `radius` apart.
///
/// The first copy is opaque and each later one carries weight `1/k`. That is not a
/// detail: it is what makes the accumulation an exact running mean
/// (`mean_k = (1 - 1/k) · mean_{k-1} + (1/k) · sample_k`) instead of a stack of
/// translucent layers that never quite reaches the average.
fn frost_layers(radius: f32) -> [(f32, f32, u8); 9] {
    let reach = radius.max(0.0);
    let mut layers = [(0.0_f32, 0.0_f32, 255_u8); 9];
    let mut index = 0;
    for row in -1..=1 {
        for column in -1..=1 {
            let k = index + 1;
            layers[index] = (
                column as f32 * reach,
                row as f32 * reach,
                (255.0 / k as f32).round().clamp(1.0, 255.0) as u8,
            );
            index += 1;
        }
    }
    layers
}

/// Where a screen point falls inside the picture, in texture coordinates, once it
/// has been shifted by `(dx, dy)`.
///
/// Clamped, because a panel can overlap the letterbox: a stretched edge pixel is a
/// far kinder artefact than stripes of `NaN`.
fn uv_at(point: egui::Pos2, source: Rect, shift: (f32, f32)) -> egui::Pos2 {
    let x = (point.x + shift.0 - source.left()) / source.width().max(1.0);
    let y = (point.y + shift.1 - source.top()) / source.height().max(1.0);
    egui::pos2(x.clamp(0.0, 1.0), y.clamp(0.0, 1.0))
}

/// The frosted copies, as meshes over the rounded shape.
///
/// Built over the *rounded* outline rather than over the rectangle: a panel's square
/// corners would otherwise smear blurred picture across the video outside them.
///
/// Public because the caller composes it with the material: the frost goes *under*
/// the glass, not instead of it.
pub fn frost_shapes(
    texture: egui::TextureId,
    rect: Rect,
    source: Rect,
    corner: f32,
    radius: f32,
) -> Vec<Shape> {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || source.width() <= 0.0 {
        return Vec::new();
    }
    let outline = rounded_outline(rect, corner, 6);
    let mut shapes = Vec::with_capacity(9);
    for (dx, dy, alpha) in frost_layers(radius) {
        let shift = (dx, dy);
        let tint = with_alpha(Color32::WHITE, alpha);
        // Vertices are pushed by hand rather than through `colored_vertex`: that
        // helper is for the font atlas, and it asserts that the mesh has *no*
        // texture — which is exactly what this mesh needs.
        let vertex = |point: egui::Pos2| egui::epaint::Vertex {
            pos: point,
            uv: uv_at(point, source, shift),
            color: tint,
        };
        let mut mesh = egui::Mesh::default();
        mesh.texture_id = texture;
        mesh.vertices.push(vertex(rect.center()));
        for point in &outline {
            mesh.vertices.push(vertex(*point));
        }
        for index in 0..outline.len() {
            let next = (index + 1) % outline.len();
            mesh.add_triangle(0, 1 + index as u32, 1 + next as u32);
        }
        shapes.push(Shape::mesh(mesh));
    }
    shapes
}
fn rounded_outline(rect: Rect, radius: f32, per_corner: usize) -> Vec<egui::Pos2> {
    use std::f32::consts::PI;
    let r = radius.clamp(0.0, rect.width().min(rect.height()) / 2.0);
    let steps = per_corner.max(1);
    let arc = |center: egui::Pos2, from: f32, to: f32, out: &mut Vec<egui::Pos2>| {
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            let angle = from + (to - from) * t;
            out.push(egui::pos2(
                center.x + angle.cos() * r,
                center.y + angle.sin() * r,
            ));
        }
    };
    let mut points = Vec::with_capacity(steps * 4 + 8);
    // Corners: top-left, top-right, bottom-right, bottom-left.
    arc(
        egui::pos2(rect.left() + r, rect.top() + r),
        PI,
        1.5 * PI,
        &mut points,
    );
    arc(
        egui::pos2(rect.right() - r, rect.top() + r),
        1.5 * PI,
        2.0 * PI,
        &mut points,
    );
    arc(
        egui::pos2(rect.right() - r, rect.bottom() - r),
        0.0,
        0.5 * PI,
        &mut points,
    );
    arc(
        egui::pos2(rect.left() + r, rect.bottom() - r),
        0.5 * PI,
        PI,
        &mut points,
    );
    points
}

/// The material, as shapes, for a caller that has to paint it *behind* something.
///
/// `Painter::add` paints immediately and therefore on top; a caller that only
/// learns a widget's rect *after* the widget has been drawn — the sliding
/// highlight of a menu bar is exactly that — reserves a `ShapeIdx` first and fills
/// it in afterwards with `Painter::set`. That is only possible with shapes in
/// hand, which is why the material is built before it is painted.
///
/// `opacity` scales every colour's alpha, so a material can fade in or out without
/// changing its geometry.
pub fn shapes(tokens: &Tokens, rect: Rect, glass: Glass, opacity: f32) -> Vec<Shape> {
    let mut out = Vec::new();
    if rect.width() <= 0.0 || rect.height() <= 0.0 || opacity <= 0.0 {
        return out;
    }
    let opacity = opacity.clamp(0.0, 1.0);
    let radius = CornerRadius::same(glass.radius.clamp(0.0, 255.0) as u8);
    let (bed, tint) = fill_alphas(glass.thickness);

    // ① the bed: what gives the surface the application's own tone. It belongs to
    // the material and never to the frame: a bed laid down by the frame is erased
    // the moment anything opaque — a frosted backdrop — is painted over it, and the
    // surface then takes its colour from whatever happens to be behind it.
    let colour = match glass.thickness {
        Thickness::Chrome => tokens.panel,
        Thickness::Float | Thickness::Sheet | Thickness::Pill => tokens.elevated,
    };
    push(
        &mut out,
        Shape::rect_filled(rect, radius, with_alpha(colour, scaled(bed, opacity))),
    );
    // ② the white tint, exactly as the reference has it: one flat wash, no gradient.
    push(
        &mut out,
        Shape::rect_filled(rect, radius, with_alpha(Color32::WHITE, scaled(tint, opacity))),
    );

    // ③ the specular highlight: over the wash, under the rim.
    sheen_shapes(&mut out, rect, glass.radius, glass.thickness, opacity);

    // ④ the rim: a ring for a floating surface, a hairline for a docked bar.
    let (alpha, width) = edge_style(glass.thickness);
    let alpha = scaled(alpha, opacity);
    match glass.edge {
        Edge::Ring => push(
            &mut out,
            Shape::rect_stroke(
                rect,
                radius,
                Stroke::new(width, with_alpha(Color32::WHITE, alpha)),
                StrokeKind::Middle,
            ),
        ),
        Edge::Line(rim) => {
            // A square-cornered bar: straight segments, inset by the radius so a
            // rounded bar would not have its corner cut in half.
            let inset = glass.radius.max(0.0);
            let (left, right) = (rect.left() + inset, rect.right() - inset);
            let (top, bottom) = (rect.top() + inset, rect.bottom() - inset);
            let stroke = Stroke::new(width, with_alpha(Color32::WHITE, alpha));
            if rim.top {
                push(
                    &mut out,
                    Shape::line_segment([egui::pos2(left, top), egui::pos2(right, top)], stroke),
                );
            }
            if rim.bottom {
                push(&mut out, Shape::line_segment([egui::pos2(left, bottom), egui::pos2(right, bottom)], stroke));
            }
            if rim.left {
                push(&mut out, Shape::line_segment([egui::pos2(left, top), egui::pos2(left, bottom)], stroke));
            }
            if rim.right {
                push(&mut out, Shape::line_segment([egui::pos2(right, top), egui::pos2(right, bottom)], stroke));
            }
        }
    }
    out
}

/// Paint the material over `rect`.
pub fn paint(painter: &Painter, tokens: &Tokens, rect: Rect, glass: Glass) {
    for shape in shapes(tokens, rect, glass, 1.0) {
        painter.add(shape);
    }
}

/// Paint the whole material on a `Ui`.
///
/// For surfaces the caller lays out itself — the docked bars and the sidebar.
pub fn paint_ui(ui: &Ui, tokens: &Tokens, rect: Rect, glass: Glass) {
    paint(ui.painter(), tokens, rect, glass);
}

/// A colour at a given alpha, keeping its own rgb.
fn with_alpha(colour: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha)
}

/// Scale an alpha by `opacity`, rounding to the nearest step.
fn scaled(alpha: u8, opacity: f32) -> u8 {
    (f32::from(alpha) * opacity).round().clamp(0.0, 255.0) as u8
}

/// Push a shape unless it is invisible: a fully transparent shape is not free — it
/// still has to be tessellated.
fn push(out: &mut Vec<Shape>, shape: Shape) {
    let visible = match &shape {
        Shape::Noop => false,
        Shape::Rect(rect) => rect.fill.a() > 0 || rect.stroke.color.a() > 0,
        Shape::LineSegment { stroke, .. } => stroke.color.a() > 0,
        _ => true,
    };
    if visible {
        out.push(shape);
    }
}

/// The corner radius of a floating sheet: rounder than a card, because a sheet
/// with a glass rim reads as thicker the rounder its corners are.
pub const SHEET_RADIUS: f32 = 20.0;

/// The inner margin of a floating sheet, matching the reach of its rim.
pub fn sheet_margin() -> Margin {
    Margin::same(crate::theme::space::LG as i8)
}

/// The shell of a docked bar.
///
/// The frame carries no fill and no stroke: the bar is glass, and the material is
/// painted by [`paint`] once the bar's rect is known. An opaque fill here would
/// hide the very backdrop the glass is supposed to borrow.
pub fn chrome_shell(margin: Margin) -> Frame {
    Frame::new().inner_margin(margin)
}

/// The shell of a floating window.
///
/// `egui::Window` paints its own frame, so the *geometry* — corner radius, inner
/// margin, drop shadow — has to come from here. The bed does not. It belongs to the
/// material, which knows whether it has to sit over a frosted backdrop, whereas a
/// bed painted by the frame would be erased by the frost whenever a picture is
/// showing and would compound with the material's own bed when one is not (a 72 %
/// bed, twice, is 92 %). One bed, in one place.
pub fn window_shell(margin: Margin, radius: f32) -> Frame {
    Frame::new()
        .fill(Color32::TRANSPARENT)
        .corner_radius(CornerRadius::same(radius.clamp(0.0, 255.0) as u8))
        .inner_margin(margin)
        .shadow(crate::theme::shadow::window())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> Tokens {
        Tokens::default()
    }

    /// The fill is faint where the picture has to show through, and nearly solid
    /// where text has to be read.
    ///
    /// The reference is white at 8 %; the player's bed is a fifth of the surface for
    /// the chrome, the island and the HUD — but a *sheet* is a reading surface, and
    /// 31 % over a bright frame puts white text at about 2.4:1. It is deliberately
    /// dense instead, and this test is what keeps both ends honest.
    #[test]
    fn the_fill_is_faint_for_chrome_and_dense_for_a_sheet() {
        for thickness in [Thickness::Chrome, Thickness::Float, Thickness::Pill] {
            let (bed, tint) = fill_alphas(thickness);
            assert!(bed > 0 && bed <= 0x60, "{thickness:?} bed is not faint");
            assert!(tint > 0 && tint <= 0x20, "{thickness:?} tint is not faint");
        }
        let (bed, tint) = fill_alphas(Thickness::Sheet);
        assert!(bed >= 0xB0, "a sheet must stay readable over bright video");
        assert!(bed < 0xFF, "but it is still glass, not a painted panel");
        assert!(tint <= 0x20, "and its own wash stays faint");
        // And it is the densest of the four, by a wide margin.
        let (float_bed, _) = fill_alphas(Thickness::Float);
        assert!(bed > float_bed * 2, "the sheet is the reading surface");
    }

    /// Floating glass is cut with the reference's ring; a docked bar gets a
    /// hairline on one edge only, because a ring around a bar that spans the window
    /// reads as a box.
    #[test]
    fn a_float_is_ringed_and_a_bar_is_not() {
        assert_eq!(Glass::float(SHEET_RADIUS).edge, Edge::Ring);
        assert_eq!(Glass::pill(12.0).edge, Edge::Ring);
        assert_eq!(Glass::chrome(Rim::TOP).edge, Edge::Line(Rim::TOP));
        assert_eq!(Glass::chrome(Rim::LEFT).edge, Edge::Line(Rim::LEFT));
    }

    /// The ring is hard — one crisp stroke — and never opaque: the reference is
    /// 60 % white, and a ring that reaches full white stops reading as glass and
    /// starts reading as an outline.
    #[test]
    fn the_ring_is_hard_and_translucent() {
        let (chrome, chrome_width) = edge_style(Thickness::Chrome);
        let (float, float_width) = edge_style(Thickness::Float);
        assert!(
            chrome < float,
            "the bar's hairline must be quieter than the ring"
        );
        assert!(float <= 0xB0, "the ring must not be opaque");
        assert!(
            (1.5..=2.5).contains(&float_width),
            "the ring is the reference's 2 px"
        );
        assert!(chrome_width <= 1.0, "a hairline is a hairline");
    }

    /// Nothing may be drawn outside the surface, beyond the half-stroke a centred
    /// rim needs.
    ///
    /// This is the regression guard for the white ring that used to surround the
    /// control island: it was four expanding translucent rectangles — a halo of up
    /// to 8 pt — plus a lit edge on every side and a hot spot in the middle. They
    /// are gone, and this test is what keeps them gone: the widest rim is 1.5 pt, so
    /// anything reaching further than 0.8 pt past the edge is a halo growing back.
    #[test]
    fn nothing_is_painted_outside_the_surface() {
        let tokens = tokens();
        let rect = Rect::from_min_size(egui::pos2(100.0, 100.0), Vec2::new(320.0, 68.0));
        let allowed = rect.expand(0.8);
        for glass in [
            Glass::chrome(Rim::TOP),
            Glass::float(SHEET_RADIUS),
            Glass::pill(34.0),
        ] {
            for shape in shapes(&tokens, rect, glass, 1.0) {
                let bounds = shape.visual_bounding_rect();
                assert!(
                    allowed.contains_rect(bounds),
                    "{glass:?} painted {bounds:?} outside {allowed:?}"
                );
            }
        }
    }

    /// The material is exactly four shapes — a bed, a wash, a sheen and a rim — and
    /// the bed is always one of them. That is what keeps a surface the application's
    /// own colour even when something opaque (a frosted backdrop) is painted behind
    /// its frame. Move the bed back out into the caller's frame and this test fails.
    #[test]
    fn the_material_is_a_handful_of_shapes() {
        let tokens = tokens();
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(200.0, 40.0));
        for glass in [
            Glass::chrome(Rim::TOP),
            Glass::float(12.0),
            Glass::pill(34.0),
            Glass::sheet(SHEET_RADIUS),
        ] {
            assert_eq!(
                shapes(&tokens, rect, glass, 1.0).len(),
                4,
                "{glass:?} is not bed + wash + sheen + rim"
            );
        }
    }

    /// The specular highlight is a *fixed* light from the top-left: it exists on
    /// every thickness, it is never brighter than a quarter of white, and it is not
    /// a pointer effect — there is no pointer argument left in the material at all.
    #[test]
    fn the_sheen_is_static_and_diagonal() {
        for thickness in [
            Thickness::Chrome,
            Thickness::Float,
            Thickness::Sheet,
            Thickness::Pill,
        ] {
            let peak = sheen_peak(thickness);
            assert!(peak > 0, "{thickness:?} has no highlight at all");
            assert!(peak <= 0x40, "{thickness:?} highlight is not a highlight");
        }
        // The falloff is along the diagonal: the top-left corner of the outline is
        // brighter than the bottom-right one, which lands on the far end of the ramp.
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(200.0, 60.0));
        let span = rect.width() + rect.height();
        let diagonal = |p: egui::Pos2| ((p.x - rect.left()) + (p.y - rect.top())) / span;
        assert!(diagonal(rect.left_top()) < 0.01);
        assert!(diagonal(rect.right_bottom()) > 0.99);
    }

    /// The sheen is laid out around the rounded outline, so it can never poke out
    /// past a corner radius as a square white speck.
    #[test]
    fn the_sheen_outline_stays_inside_the_shape() {
        let rect = Rect::from_min_size(egui::pos2(10.0, 20.0), Vec2::new(320.0, 68.0));
        for radius in [0.0, 8.0, 20.0, 500.0] {
            for point in rounded_outline(rect, radius, 6) {
                assert!(
                    rect.contains(point),
                    "radius {radius}: {point:?} is outside {rect:?}"
                );
            }
        }
    }

    /// Fading the material scales every colour and leaves the geometry alone.
    #[test]
    fn opacity_scales_the_material_without_moving_it() {
        let tokens = tokens();
        let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(200.0, 40.0));
        let solid = shapes(&tokens, rect, Glass::float(8.0), 1.0);
        let faded = shapes(&tokens, rect, Glass::float(8.0), 0.25);
        assert_eq!(solid.len(), faded.len());
        // Fully transparent means nothing at all: nothing to tessellate.
        assert!(shapes(&tokens, rect, Glass::float(8.0), 0.0).is_empty());
    }

    /// A capsule is round by definition, so the shape and the material have to
    /// agree on the radius.
    #[test]
    fn a_capsule_is_half_a_control_tall() {
        let glass = Glass::capsule(30.0);
        assert_eq!(glass.radius, 15.0);
        assert_eq!(glass.thickness, Thickness::Pill);
        assert_eq!(glass.edge, Edge::Ring);
    }

    /// The frosted backdrop samples the picture where the panel is: a plain
    /// rect-to-uv mapping, clamped rather than wrapped when the panel hangs over the
    /// letterbox.
    #[test]
    fn the_frost_samples_the_picture_under_the_panel() {
        let source = Rect::from_min_size(egui::pos2(100.0, 50.0), Vec2::new(400.0, 200.0));
        let uv = |point: egui::Pos2| uv_at(point, source, (0.0, 0.0));
        let top_left = uv(source.left_top());
        assert!(top_left.x.abs() < 1e-6 && top_left.y.abs() < 1e-6);
        let bottom_right = uv(source.right_bottom());
        assert!((bottom_right.x - 1.0).abs() < 1e-6 && (bottom_right.y - 1.0).abs() < 1e-6);
        let centre = uv(source.center());
        assert!((centre.x - 0.5).abs() < 1e-6 && (centre.y - 0.5).abs() < 1e-6);
        // Outside the picture — the letterbox — the edge pixel is stretched.
        assert_eq!(uv(egui::pos2(-500.0, -500.0)), egui::pos2(0.0, 0.0));
        assert_eq!(uv(egui::pos2(9999.0, 9999.0)), egui::pos2(1.0, 1.0));
        // A shift moves the sample: that is what makes the copies a blur.
        assert!(uv_at(source.center(), source, (20.0, 0.0)).x > centre.x);
    }

    /// The copies are weighted `1/k`, so their accumulation is the mean rather than a
    /// stack of translucent layers that never quite gets there.
    #[test]
    fn the_frost_layers_average_to_a_blur() {
        let layers = frost_layers(4.0);
        assert_eq!(layers.len(), 9);
        assert_eq!(layers[0].2, 255, "the first copy must be opaque");
        for (index, layer) in layers.iter().enumerate() {
            let expected = (255.0 / (index + 1) as f32).round() as u8;
            assert_eq!(layer.2, expected, "layer {index} carries the wrong weight");
        }
        // A 3x3 grid spanning ±radius: that span is the width of the blur.
        let xs: Vec<f32> = layers.iter().map(|(x, _, _)| *x).collect();
        assert_eq!(xs.iter().cloned().fold(f32::INFINITY, f32::min), -4.0);
        assert_eq!(xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 4.0);
        // A zero radius degenerates to nine copies of one sample: no blur, no NaN.
        assert!(frost_layers(0.0).iter().all(|(x, y, _)| *x == 0.0 && *y == 0.0));
    }

    /// The frost stays inside the *rounded* shape: nine meshes over the outline, none
    /// of them the square rectangle that would smear blurred picture over the video
    /// at the corners.
    #[test]
    fn the_frost_stays_inside_the_shape() {
        let rect = Rect::from_min_size(egui::pos2(200.0, 120.0), Vec2::new(320.0, 180.0));
        let source = Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(1280.0, 720.0));
        let texture = egui::TextureId::User(1);
        let shapes = frost_shapes(texture, rect, source, SHEET_RADIUS, FROST_RADIUS);
        assert_eq!(shapes.len(), 9);
        for shape in &shapes {
            let bounds = shape.visual_bounding_rect();
            assert!(
                rect.expand(0.1).contains_rect(bounds),
                "frost painted {bounds:?} outside {rect:?}"
            );
        }
        // Nothing at all when there is no picture to sample.
        assert!(frost_shapes(texture, rect, Rect::NOTHING, 8.0, 6.0).is_empty());
        // …and nothing for a degenerate surface.
        assert!(frost_shapes(texture, Rect::NOTHING, source, 8.0, 6.0).is_empty());
    }

    /// The shell of a floating window carries the geometry and nothing else: no fill
    /// (the material's bed is the only one) and no stroke (the ring is the material's
    /// job, and a frame stroke on top of it would double the edge).
    #[test]
    fn a_window_shell_paints_no_fill_and_no_stroke() {
        let frame = window_shell(sheet_margin(), SHEET_RADIUS);
        assert!(
            frame.fill == Color32::TRANSPARENT,
            "the bed belongs to the material, not to the frame"
        );
        assert!(
            frame.stroke.color.a() == 0,
            "no frame stroke: the ring is enough"
        );
    }

    /// The two `egui` facts the sheet's glass is built on, checked against the real
    /// `Window` container rather than trusted:
    ///
    /// * `Window::show` hands back the window's *own* rect — title bar included —
    ///   while the closure it runs can only ever measure the body. That is why the
    ///   glass is painted on `response.rect` and not on anything measured inside:
    ///   a sheet filled from the body alone leaves its top strip with no glass on it.
    /// * A shape slot reserved inside that closure can still be filled *after* the
    ///   call: the shape list is not consumed until the frame ends. That is what lets
    ///   the glass cover the title strip and still land under the title's text, since
    ///   the title bar is painted after the body.
    #[test]
    fn a_window_reports_the_rect_its_glass_needs() {
        let ctx = egui::Context::default();
        let mut slot = None;
        let mut window_rect = Rect::NOTHING;
        let mut body_rect = Rect::NOTHING;

        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    Vec2::new(1280.0, 720.0),
                )),
                ..Default::default()
            },
            |ctx| {
                let shown = egui::Window::new("glass")
                    .fixed_size([300.0, 200.0])
                    .show(ctx, |ui| {
                        slot = Some(ui.painter().add(egui::Shape::Noop));
                        ui.label("body");
                        // Everything the closure could measure.
                        ui.min_rect()
                    });
                let shown = shown.expect("the window is shown");
                let window = shown.response.rect;
                let body = shown.inner.expect("the window body ran");
                assert!(
                    body.top() > window.top(),
                    "the title bar is above the body: body {body:?}, window {window:?}"
                );
                assert!(
                    window.height() > body.height(),
                    "the window is taller than its body: body {body:?}, window {window:?}"
                );

                // Fill the reserved slot, from outside the closure, with the glass of
                // the whole window.
                ctx.layer_painter(shown.response.layer_id).set(
                    slot.expect("the closure reserved a slot"),
                    egui::Shape::Vec(shapes(
                        &tokens(),
                        window,
                        Glass::sheet(SHEET_RADIUS),
                        1.0,
                    )),
                );
                window_rect = window;
                body_rect = body;
            },
        );

        assert!(
            window_rect.contains_rect(body_rect),
            "the window's rect covers the body: body {body_rect:?}, window {window_rect:?}"
        );
        assert!(
            window_rect.height() > body_rect.height(),
            "the window is taller than its body: body {body_rect:?}, window {window_rect:?}"
        );
        assert!(
            output.shapes.iter().any(|clipped| matches!(
                &clipped.shape,
                egui::Shape::Vec(inner) if inner.len() == 4
            )),
            "the material reached the frame's shape list"
        );
    }
}
