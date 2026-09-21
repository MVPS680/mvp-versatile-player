//! The player's icon set, drawn as vectors.
//!
//! Icons are painted with `egui`'s painter rather than shipped as an icon font
//! or emoji. That keeps the binary small, renders crisply at every DPI without
//! hinting artefacts, and lets an icon inherit the exact colour of the state it
//! represents (hover, active, disabled) with no tinting tricks.
//!
//! Every glyph is authored on a 24x24 grid and scaled into whatever rectangle
//! the caller provides, so the same drawing works for a 16 pt toolbar button and
//! a 48 pt transport control.

use egui::{Color32, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2};

use crate::theme;

/// Logical grid every icon is drawn on.
const GRID: f32 = 24.0;

/// `egui`'s painter has no arc primitive, so icons that need one (speakers,
/// repeat arrows, the help mark) get it from this extension.
/// Implementing it as a trait keeps every call site reading like the primitive
/// it stands in for.
trait ArcExt {
    /// Stroke a circular arc from `range.start` to `range.end` radians,
    /// measured clockwise from the positive X axis.
    fn arc(&self, center: Pos2, radius: f32, range: std::ops::Range<f32>, stroke: Stroke);
}

impl ArcExt for Painter {
    fn arc(&self, center: Pos2, radius: f32, range: std::ops::Range<f32>, stroke: Stroke) {
        const SEGMENTS: usize = 28;
        let mut points = Vec::with_capacity(SEGMENTS + 1);
        for step in 0..=SEGMENTS {
            let t = step as f32 / SEGMENTS as f32;
            let angle = range.start + (range.end - range.start) * t;
            let (sin, cos) = angle.sin_cos();
            points.push(Pos2::new(
                center.x + cos * radius,
                center.y + sin * radius,
            ));
        }
        self.add(Shape::line(points, stroke));
    }
}

/// Maps a point on the 24x24 design grid into `rect`.
struct Canvas {
    rect: Rect,
    scale: f32,
}

impl Canvas {
    fn new(rect: Rect) -> Self {
        let scale = (rect.width().min(rect.height()) / GRID).max(0.01);
        Self { rect, scale }
    }

    fn p(&self, x: f32, y: f32) -> Pos2 {
        Pos2::new(
            self.rect.center().x + (x - GRID / 2.0) * self.scale,
            self.rect.center().y + (y - GRID / 2.0) * self.scale,
        )
    }

    fn s(&self, v: f32) -> f32 {
        v * self.scale
    }

    fn line(&self, painter: &Painter, a: (f32, f32), b: (f32, f32), stroke: Stroke) {
        painter.line_segment([self.p(a.0, a.1), self.p(b.0, b.1)], stroke);
    }

    fn poly(&self, painter: &Painter, points: &[(f32, f32)], fill: Color32) {
        let pts: Vec<Pos2> = points.iter().map(|(x, y)| self.p(*x, *y)).collect();
        painter.add(Shape::convex_polygon(pts, fill, Stroke::NONE));
    }

    fn circle(&self, painter: &Painter, center: (f32, f32), radius: f32, fill: Color32) {
        painter.circle_filled(self.p(center.0, center.1), self.s(radius), fill);
    }
}

/// Every icon the interface can draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    /// Play triangle.
    Play,
    /// Two vertical bars.
    Pause,
    /// Skip to the previous track.
    Previous,
    /// Skip to the next track.
    Next,
    /// Jump back ten seconds.
    Rewind,
    /// Jump forward ten seconds.
    Forward,
    /// Speaker with waves.
    VolumeHigh,
    /// Speaker with one wave.
    VolumeLow,
    /// Speaker with a cross.
    VolumeMute,
    /// Four corner brackets.
    Fullscreen,
    /// Four inward brackets.
    ExitFullscreen,
    /// Cog wheel.
    Settings,
    /// Three stacked lines with a play mark.
    Playlist,
    /// Folder outline.
    Folder,
    /// Picture frame with a mountain.
    Image,
    /// Camera body.
    Snapshot,
    /// Two arrows in a loop.
    Repeat,
    /// Crossed arrows.
    Shuffle,
    /// Circular arrow, repeat one.
    RepeatOne,
    /// Speech bubble with "cc".
    Subtitles,
    /// Magnifier with a plus.
    ZoomIn,
    /// Magnifier with a minus.
    ZoomOut,
    /// Arrows pointing into a frame.
    FitToWindow,
    /// Circular arrow, rotate clockwise.
    RotateCw,
    /// Circular arrow, rotate counter-clockwise.
    RotateCcw,
    /// Two mirrored triangles.
    FlipHorizontal,
    /// Two mirrored triangles, vertical.
    FlipVertical,
    /// Slideshow play.
    Slideshow,
    /// An "i" in a circle.
    Info,
    /// Question mark in a circle.
    Help,
    /// Cross.
    Close,
    /// Plus.
    Plus,
    /// Up chevron, "move earlier in the list".
    ChevronUp,
    /// Down chevron, "move later in the list".
    ChevronDown,
    /// Circular refresh arrow.
    Refresh,
    /// Link / open URL.
    Link,
    /// Film strip.
    Film,
    /// Musical note.
    Music,
    /// A broom, "clear list".
    Clear,
    /// Floppy disk, "save playlist".
    Save,
    /// A bracketed region between two points, "A–B loop".
    AbLoop,
    /// Stacked text lines, "lyrics".
    Lyrics,
    /// A small grid of squares, "thumbnail strip".
    Grid,
    /// Two lanes with a handle each, "tracks".
    Tracks,
}

/// Draw `icon` inside `rect` using `color`.
pub fn draw(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = Canvas::new(rect);
    // Stroke weight scales with the icon so a 48 pt control does not look wispy.
    let w = (c.s(2.0)).clamp(1.0, 4.0);
    let stroke = Stroke::new(w, color);
    let thin = Stroke::new((w * 0.8).max(1.0), color);

    match icon {
        Icon::Play => c.poly(
            painter,
            &[(8.0, 5.0), (19.0, 12.0), (8.0, 19.0)],
            color,
        ),
        Icon::Pause => {
            let r = egui::Rect::from_min_max(c.p(7.0, 5.0), c.p(10.5, 19.0));
            painter.rect_filled(r, 1.0, color);
            let r = egui::Rect::from_min_max(c.p(13.5, 5.0), c.p(17.0, 19.0));
            painter.rect_filled(r, 1.0, color);
        }
        Icon::Previous => {
            c.poly(painter, &[(18.0, 5.5), (18.0, 18.5), (9.0, 12.0)], color);
            let r = egui::Rect::from_min_max(c.p(5.5, 5.5), c.p(8.0, 18.5));
            painter.rect_filled(r, 1.0, color);
        }
        Icon::Next => {
            c.poly(painter, &[(6.0, 5.5), (6.0, 18.5), (15.0, 12.0)], color);
            let r = egui::Rect::from_min_max(c.p(16.0, 5.5), c.p(18.5, 18.5));
            painter.rect_filled(r, 1.0, color);
        }
        Icon::Rewind => {
            c.poly(painter, &[(11.0, 6.0), (11.0, 18.0), (3.5, 12.0)], color);
            c.poly(painter, &[(20.5, 6.0), (20.5, 18.0), (13.0, 12.0)], color);
        }
        Icon::Forward => {
            c.poly(painter, &[(3.5, 6.0), (3.5, 18.0), (11.0, 12.0)], color);
            c.poly(painter, &[(13.0, 6.0), (13.0, 18.0), (20.5, 12.0)], color);
        }
        Icon::VolumeHigh | Icon::VolumeLow | Icon::VolumeMute => {
            // Shared speaker body.
            c.poly(
                painter,
                &[(4.0, 9.5), (8.0, 9.5), (12.0, 5.5), (12.0, 18.5), (8.0, 14.5), (4.0, 14.5)],
                color,
            );
            match icon {
                Icon::VolumeHigh => {
                    painter.arc(
                        c.p(12.0, 12.0),
                        c.s(5.0),
                        -0.9..0.9,
                        Stroke::new(w, color),
                    );
                    painter.arc(
                        c.p(12.0, 12.0),
                        c.s(8.0),
                        -0.9..0.9,
                        Stroke::new(w, color),
                    );
                }
                Icon::VolumeLow => {
                    painter.arc(
                        c.p(12.0, 12.0),
                        c.s(5.0),
                        -0.9..0.9,
                        Stroke::new(w, color),
                    );
                }
                _ => {
                    c.line(painter, (15.0, 9.0), (20.5, 15.0), stroke);
                    c.line(painter, (20.5, 9.0), (15.0, 15.0), stroke);
                }
            }
        }
        Icon::Fullscreen => {
            c.line(painter, (4.0, 9.0), (4.0, 4.0), stroke);
            c.line(painter, (4.0, 4.0), (9.0, 4.0), stroke);
            c.line(painter, (15.0, 4.0), (20.0, 4.0), stroke);
            c.line(painter, (20.0, 4.0), (20.0, 9.0), stroke);
            c.line(painter, (20.0, 15.0), (20.0, 20.0), stroke);
            c.line(painter, (20.0, 20.0), (15.0, 20.0), stroke);
            c.line(painter, (9.0, 20.0), (4.0, 20.0), stroke);
            c.line(painter, (4.0, 20.0), (4.0, 15.0), stroke);
        }
        Icon::ExitFullscreen => {
            c.line(painter, (9.0, 4.0), (9.0, 9.0), stroke);
            c.line(painter, (9.0, 9.0), (4.0, 9.0), stroke);
            c.line(painter, (15.0, 4.0), (15.0, 9.0), stroke);
            c.line(painter, (15.0, 9.0), (20.0, 9.0), stroke);
            c.line(painter, (20.0, 15.0), (15.0, 15.0), stroke);
            c.line(painter, (15.0, 15.0), (15.0, 20.0), stroke);
            c.line(painter, (4.0, 15.0), (9.0, 15.0), stroke);
            c.line(painter, (9.0, 15.0), (9.0, 20.0), stroke);
        }
        Icon::Settings => {
            painter.circle_stroke(c.p(12.0, 12.0), c.s(6.5), stroke);
            painter.circle_filled(c.p(12.0, 12.0), c.s(2.4), color);
            for i in 0..8 {
                let angle = std::f32::consts::PI * 2.0 * i as f32 / 8.0;
                let (s, co) = angle.sin_cos();
                c.line(
                    painter,
                    (12.0 + co * 7.6, 12.0 + s * 7.6),
                    (12.0 + co * 10.2, 12.0 + s * 10.2),
                    stroke,
                );
            }
        }
        Icon::Playlist => {
            for y in [6.5f32, 12.0, 17.5] {
                c.line(painter, (4.0, y), (13.0, y), thin);
            }
            c.poly(painter, &[(16.5, 6.0), (21.0, 8.6), (16.5, 11.2)], color);
        }
        Icon::Folder => {
            // An outline, not a solid. The playlist toolbar puts this next to
            // `Plus`, `Link` and `Close`, all of which are line art — one filled
            // silhouette in that row is what makes a toolbar look like two icon
            // sets that happen to be sharing a shelf.
            let outline = [
                (4.0, 18.5),
                (4.0, 6.5),
                (9.7, 6.5),
                (11.9, 9.2),
                (20.0, 9.2),
                (20.0, 18.5),
                (4.0, 18.5),
            ];
            for pair in outline.windows(2) {
                c.line(painter, pair[0], pair[1], stroke);
            }
        }
        Icon::Image => {
            let r = egui::Rect::from_min_max(c.p(3.5, 5.0), c.p(20.5, 19.0));
            painter.rect_stroke(r, c.s(2.0), stroke, StrokeKind::Middle);
            c.poly(
                painter,
                &[(6.0, 16.5), (10.5, 11.0), (14.0, 15.0), (16.0, 12.8), (18.5, 16.5)],
                color,
            );
            c.circle(painter, (8.5, 9.0), 1.5, color);
        }
        Icon::Snapshot => {
            let r = egui::Rect::from_min_max(c.p(3.0, 7.0), c.p(21.0, 19.0));
            painter.rect_stroke(r, c.s(2.5), stroke, StrokeKind::Middle);
            let top = egui::Rect::from_min_max(c.p(8.5, 4.2), c.p(15.5, 7.0));
            painter.rect_filled(top, c.s(1.0), color);
            painter.circle_stroke(c.p(12.0, 13.0), c.s(3.6), stroke);
        }
        Icon::Repeat => {
            painter.arc(c.p(12.0, 12.0), c.s(7.5), 0.6..2.6, stroke);
            c.poly(painter, &[(20.0, 6.5), (20.0, 11.5), (16.0, 9.0)], color);
            painter.arc(c.p(12.0, 12.0), c.s(7.5), 3.7..5.7, stroke);
            c.poly(painter, &[(4.0, 17.5), (4.0, 12.5), (8.0, 15.0)], color);
        }
        Icon::RepeatOne => {
            painter.arc(c.p(12.0, 12.0), c.s(7.5), 0.6..2.6, stroke);
            c.poly(painter, &[(20.0, 6.5), (20.0, 11.5), (16.0, 9.0)], color);
            painter.arc(c.p(12.0, 12.0), c.s(7.5), 3.7..5.7, stroke);
            c.poly(painter, &[(4.0, 17.5), (4.0, 12.5), (8.0, 15.0)], color);
            c.line(painter, (12.0, 9.5), (12.0, 14.5), Stroke::new(w * 0.9, color));
            c.line(painter, (10.4, 11.0), (12.0, 9.5), Stroke::new(w * 0.9, color));
        }
        Icon::Shuffle => {
            c.line(painter, (4.0, 7.0), (9.0, 7.0), stroke);
            c.line(painter, (9.0, 7.0), (15.0, 17.0), stroke);
            c.line(painter, (15.0, 17.0), (20.0, 17.0), stroke);
            c.poly(painter, &[(18.5, 14.5), (21.5, 17.0), (18.5, 19.5)], color);
            c.line(painter, (4.0, 17.0), (9.0, 17.0), stroke);
            c.line(painter, (9.0, 17.0), (15.0, 7.0), stroke);
            c.line(painter, (15.0, 7.0), (20.0, 7.0), stroke);
            c.poly(painter, &[(18.5, 4.5), (21.5, 7.0), (18.5, 9.5)], color);
        }
        Icon::Subtitles => {
            let r = egui::Rect::from_min_max(c.p(3.0, 5.5), c.p(21.0, 18.5));
            painter.rect_stroke(r, c.s(2.5), stroke, StrokeKind::Middle);
            c.line(painter, (7.0, 13.5), (11.5, 13.5), thin);
            c.line(painter, (13.0, 13.5), (17.0, 13.5), thin);
            c.line(painter, (7.0, 16.0), (13.0, 16.0), thin);
            c.line(painter, (14.5, 16.0), (17.0, 16.0), thin);
        }
        Icon::ZoomIn | Icon::ZoomOut | Icon::FitToWindow => {
            painter.circle_stroke(c.p(10.5, 10.5), c.s(6.0), stroke);
            c.line(painter, (15.0, 15.0), (20.0, 20.0), stroke);
            match icon {
                Icon::ZoomIn => {
                    c.line(painter, (7.5, 10.5), (13.5, 10.5), thin);
                    c.line(painter, (10.5, 7.5), (10.5, 13.5), thin);
                }
                Icon::ZoomOut => c.line(painter, (7.5, 10.5), (13.5, 10.5), thin),
                _ => {
                    c.line(painter, (4.0, 9.0), (4.0, 4.0), thin);
                    c.line(painter, (4.0, 4.0), (9.0, 4.0), thin);
                    c.line(painter, (15.0, 20.0), (20.0, 20.0), thin);
                    c.line(painter, (20.0, 20.0), (20.0, 15.0), thin);
                }
            }
        }
        Icon::RotateCw => {
            painter.arc(c.p(12.0, 12.5), c.s(7.0), -2.4..1.2, stroke);
            c.poly(painter, &[(18.5, 3.5), (19.5, 9.0), (14.2, 6.6)], color);
        }
        Icon::RotateCcw => {
            painter.arc(c.p(12.0, 12.5), c.s(7.0), 1.9..5.5, stroke);
            c.poly(painter, &[(5.5, 3.5), (9.8, 6.6), (4.5, 9.0)], color);
        }
        Icon::FlipHorizontal => {
            c.line(painter, (12.0, 4.0), (12.0, 20.0), thin);
            c.poly(painter, &[(10.0, 7.0), (10.0, 17.0), (3.5, 12.0)], color);
            c.poly(painter, &[(14.0, 7.0), (14.0, 17.0), (20.5, 12.0)], color);
        }
        Icon::FlipVertical => {
            c.line(painter, (4.0, 12.0), (20.0, 12.0), thin);
            c.poly(painter, &[(7.0, 10.0), (17.0, 10.0), (12.0, 3.5)], color);
            c.poly(painter, &[(7.0, 14.0), (17.0, 14.0), (12.0, 20.5)], color);
        }
        Icon::Slideshow => {
            let r = egui::Rect::from_min_max(c.p(3.0, 5.0), c.p(21.0, 19.0));
            painter.rect_stroke(r, c.s(2.0), stroke, StrokeKind::Middle);
            c.poly(painter, &[(10.0, 8.5), (17.0, 12.0), (10.0, 15.5)], color);
        }
        Icon::Info | Icon::Help => {
            painter.circle_stroke(c.p(12.0, 12.0), c.s(8.5), stroke);
            if icon == Icon::Info {
                c.circle(painter, (12.0, 7.6), 1.3, color);
                c.line(painter, (12.0, 10.6), (12.0, 17.0), Stroke::new(w * 1.1, color));
            } else {
                painter.arc(c.p(12.0, 9.6), c.s(2.4), 3.4..6.0, stroke);
                c.line(painter, (12.0, 12.2), (12.0, 13.8), Stroke::new(w * 1.1, color));
                c.circle(painter, (12.0, 16.8), 1.3, color);
            }
        }
        Icon::Close => {
            c.line(painter, (6.5, 6.5), (17.5, 17.5), stroke);
            c.line(painter, (17.5, 6.5), (6.5, 17.5), stroke);
        }
        Icon::Plus => {
            c.line(painter, (6.0, 12.0), (18.0, 12.0), stroke);
            c.line(painter, (12.0, 6.0), (12.0, 18.0), stroke);
        }
        Icon::ChevronUp => {
            c.line(painter, (5.0, 15.0), (12.0, 8.0), stroke);
            c.line(painter, (12.0, 8.0), (19.0, 15.0), stroke);
        }
        Icon::ChevronDown => {
            c.line(painter, (5.0, 9.0), (12.0, 16.0), stroke);
            c.line(painter, (12.0, 16.0), (19.0, 9.0), stroke);
        }
        Icon::Refresh => {
            // A circular arrow: the arc, plus a chevron at its end laid out along
            // the arc's own tangent.
            //
            // The chevron is two strokes rather than a filled triangle, which is
            // what it used to be. At the 13 or 14 pt the toolbar draws this, that
            // triangle came out under three points across and simply vanished into
            // its own antialiasing — leaving a plain "C" that says nothing about
            // refreshing anything. Strokes that follow the tangent keep their shape
            // at every size, because their weight is the icon's, not their area.
            const FROM: f32 = 0.6;
            const TO: f32 = 5.5;
            let centre = c.p(12.0, 12.0);
            let radius = c.s(7.4);
            painter.arc(centre, radius, FROM..TO, stroke);

            let (sin, cos) = TO.sin_cos();
            let tip = Pos2::new(centre.x + cos * radius, centre.y + sin * radius);
            // The direction the sweep is travelling in when it reaches `tip`.
            let tangent = Vec2::new(-sin, cos);
            let back = tangent * -c.s(5.4);
            let side = Vec2::new(-tangent.y, tangent.x) * c.s(3.0);
            painter.line_segment([tip, tip + back + side], stroke);
            painter.line_segment([tip, tip + back - side], stroke);
        }
        Icon::Link => {
            // A globe. What was here was two open arcs facing each other with a bar
            // between them — a chain link on the design grid, and at the 11 to 14 pt
            // the interface actually draws it at, three thin arcs that touch come out
            // as an unreadable knot. A circle, a meridian and an equator are the same
            // idea at any size, and they read as "network" rather than as "metal".
            painter.circle_stroke(c.p(12.0, 12.0), c.s(8.0), stroke);
            c.line(painter, (4.0, 12.0), (20.0, 12.0), thin);
            let meridian: Vec<Pos2> = (0..=24)
                .map(|step| {
                    let angle = step as f32 / 24.0 * std::f32::consts::TAU;
                    Pos2::new(
                        c.p(12.0, 12.0).x + angle.sin() * c.s(3.2),
                        c.p(12.0, 12.0).y - angle.cos() * c.s(8.0),
                    )
                })
                .collect();
            painter.add(Shape::line(meridian, thin));
        }
        Icon::Film => {
            let r = egui::Rect::from_min_max(c.p(3.0, 4.5), c.p(21.0, 19.5));
            painter.rect_stroke(r, c.s(1.5), stroke, StrokeKind::Middle);
            for y in [7.5f32, 12.0, 16.5] {
                c.line(painter, (3.0, y), (21.0, y), thin);
            }
            c.line(painter, (7.0, 4.5), (7.0, 19.5), thin);
            c.line(painter, (17.0, 4.5), (17.0, 19.5), thin);
        }
        Icon::Music => {
            c.line(painter, (9.5, 17.0), (9.5, 6.0), Stroke::new(w, color));
            c.line(painter, (9.5, 6.5), (19.0, 4.5), Stroke::new(w, color));
            c.line(painter, (19.0, 4.5), (19.0, 15.0), Stroke::new(w, color));
            painter.circle_filled(c.p(7.3, 17.2), c.s(2.3), color);
            painter.circle_filled(c.p(16.8, 15.2), c.s(2.3), color);
        }
        Icon::Clear => {
            // A bin. What this used to draw was an arrow pointing down at a line,
            // which means "download" or "save" everywhere else in the world — on
            // the button that empties the playlist. The glyph has to say what the
            // button does, and "delete" has one widely understood shape.
            c.line(painter, (4.0, 7.2), (20.0, 7.2), thin);
            c.line(painter, (9.4, 7.2), (9.4, 4.6), thin);
            c.line(painter, (9.4, 4.6), (14.6, 4.6), thin);
            c.line(painter, (14.6, 4.6), (14.6, 7.2), thin);
            c.line(painter, (6.6, 7.2), (7.6, 19.6), thin);
            c.line(painter, (17.4, 7.2), (16.4, 19.6), thin);
            c.line(painter, (7.6, 19.6), (16.4, 19.6), thin);
            c.line(painter, (12.0, 10.2), (12.0, 16.6), thin);
        }
        Icon::Save => {
            let r = egui::Rect::from_min_max(c.p(3.5, 3.5), c.p(20.5, 20.5));
            painter.rect_stroke(r, c.s(2.0), stroke, StrokeKind::Middle);
            let slot = egui::Rect::from_min_max(c.p(8.0, 3.5), c.p(16.0, 9.5));
            painter.rect_filled(slot, 0.0, color);
            let label = egui::Rect::from_min_max(c.p(7.0, 13.0), c.p(17.0, 20.5));
            painter.rect_stroke(label, 0.0, thin, StrokeKind::Middle);
        }
        Icon::AbLoop => {
            // Two brackets around a double-headed arrow: the region between the
            // two points. The letters themselves would need glyph outlines, and
            // the shape reads at every size without them.
            c.line(painter, (7.5, 6.0), (4.5, 6.0), stroke);
            c.line(painter, (4.5, 6.0), (4.5, 18.0), stroke);
            c.line(painter, (4.5, 18.0), (7.5, 18.0), stroke);
            c.line(painter, (16.5, 6.0), (19.5, 6.0), stroke);
            c.line(painter, (19.5, 6.0), (19.5, 18.0), stroke);
            c.line(painter, (19.5, 18.0), (16.5, 18.0), stroke);
            c.line(painter, (8.0, 12.0), (16.0, 12.0), thin);
            c.poly(painter, &[(9.5, 9.6), (6.8, 12.0), (9.5, 14.4)], color);
            c.poly(painter, &[(14.5, 9.6), (17.2, 12.0), (14.5, 14.4)], color);
        }
        Icon::Lyrics => {
            // Text lines with one highlighted, the way a lyric sheet marks the
            // line being sung.
            for y in [6.0f32, 12.0, 18.0] {
                c.line(painter, (5.0, y), (19.0, y), thin);
            }
            c.line(painter, (5.0, 12.0), (14.0, 12.0), stroke);
            painter.circle_filled(c.p(20.0, 12.0), c.s(1.4), color);
        }
        Icon::Grid => {
            for x in [5.0f32, 12.0, 19.0] {
                for y in [5.0f32, 12.0, 19.0] {
                    let square =
                        egui::Rect::from_center_size(c.p(x, y), Vec2::splat(c.s(4.2)));
                    painter.rect_filled(square, c.s(1.0), color);
                }
            }
        }
        Icon::Tracks => {
            // Two lanes with a handle on each: a multitrack view. Deliberately not a
            // music note — the sidebar already uses `Music` to mean "this file is
            // audio", while this control answers *which* stream plays.
            for y in [8.5f32, 15.5] {
                let lane = egui::Rect::from_min_max(c.p(3.5, y - 2.2), c.p(20.5, y + 2.2));
                painter.rect_stroke(lane, c.s(1.4), thin, StrokeKind::Middle);
            }
            painter.circle_filled(c.p(8.5, 8.5), c.s(2.4), color);
            painter.circle_filled(c.p(15.5, 15.5), c.s(2.4), color);
        }
    }
}

/// An icon button that highlights on hover and shows a tooltip.
///
/// Returns the `Response` so callers can chain `.on_hover_text(...)` or read
/// `.clicked()`. The hit area is always at least 28x28 pt *before*
/// [`theme::button`] scales it, which keeps the controls comfortable in a dark,
/// low-contrast interface.
pub fn icon_button(
    ui: &mut Ui,
    icon: Icon,
    size: f32,
    color: Color32,
    hover_color: Color32,
    bg_hover: Color32,
    bg_active: Color32,
) -> Response {
    // The caller passes the size the design was laid out at, and this is the one place every
    // icon button in the player — the transport bar, the toolbars, the glyph menus — is
    // turned into the size on screen. Nothing else in the interface goes through it.
    let size = theme::button::of(size);
    let desired = Vec2::splat(size.max(theme::button::of(28.0)));
    let (rect, response) = ui.allocate_exact_size(desired, Sense::click());
    let visuals = ui.style().interact(&response);

    let fill = if response.is_pointer_button_down_on() {
        bg_active
    } else if response.hovered() {
        bg_hover
    } else {
        Color32::TRANSPARENT
    };
    if fill != Color32::TRANSPARENT {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(theme::radius::MD as u8),
            fill,
        );
    }

    let tint = if !ui.is_enabled() {
        visuals.fg_stroke.color.gamma_multiply(0.5)
    } else if response.hovered() {
        hover_color
    } else {
        color
    };
    let glyph_rect = Rect::from_center_size(rect.center(), Vec2::splat(size * 0.72));
    draw(ui.painter(), glyph_rect, icon, tint);

    // Hand the keyboard back after a click so the global playback shortcuts keep
    // working without the user having to click an empty area first.
    if response.clicked() {
        ui.memory_mut(|memory| memory.surrender_focus(response.id));
    }
    response
}

/// A larger, filled transport control (play/pause), used at the centre of the
/// control bar where the primary action deserves emphasis.
///
/// `diameter` is the *final* size, not a design size to be scaled: this button is not a
/// multiple of the icon buttons, and the number lives in [`theme::button::PLAY`] with the
/// reasoning for it.
pub fn primary_transport_button(
    ui: &mut Ui,
    icon: Icon,
    diameter: f32,
    bg: Color32,
    bg_hover: Color32,
    fg: Color32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::click());
    let radius = diameter * 0.5;

    let fill = if response.is_pointer_button_down_on() {
        bg.gamma_multiply(0.8)
    } else if response.hovered() {
        bg_hover
    } else {
        bg
    };
    ui.painter().circle_filled(rect.center(), radius, fill);

    // Ring on hover keeps the target readable on busy video frames.
    if response.hovered() {
        ui.painter()
            .circle_stroke(rect.center(), radius, Stroke::new(1.0_f32, bg_hover.gamma_multiply(0.6)));
    }

    let glyph = Rect::from_center_size(rect.center(), Vec2::splat(diameter * 0.58));
    draw(ui.painter(), glyph, icon, fg);
    if response.clicked() {
        ui.memory_mut(|memory| memory.surrender_focus(response.id));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_maps_the_grid_onto_the_target_rect() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::splat(48.0));
        let canvas = Canvas::new(rect);
        let centre = canvas.p(12.0, 12.0);
        assert!((centre.x - rect.center().x).abs() < 1e-3);
        assert!((centre.y - rect.center().y).abs() < 1e-3);
        let corner = canvas.p(0.0, 0.0);
        assert!((corner.x - rect.min.x).abs() < 1e-3);
        assert!((corner.y - rect.min.y).abs() < 1e-3);
    }

    #[test]
    fn canvas_uses_the_smaller_dimension_for_non_square_rects() {
        let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(64.0, 24.0));
        let canvas = Canvas::new(rect);
        // 24 pt tall => scale 1.0, so a 24-unit span covers the full height.
        assert!((canvas.s(24.0) - 24.0).abs() < 1e-3);
    }

    #[test]
    fn zero_sized_rects_do_not_divide_by_zero() {
        let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::ZERO);
        let canvas = Canvas::new(rect);
        assert!(canvas.scale.is_finite());
        assert!(canvas.scale > 0.0);
    }

    /// The size the design passes in is the size *before* [`theme::button`], and an icon
    /// button's box is what the transport-bar layout model assumes — so this is where the
    /// two have to agree. A box that grew without the model noticing is a row of controls
    /// drawn on top of each other.
    #[test]
    fn an_icon_button_box_is_scaled_by_the_button_factor() {
        let ctx = egui::Context::default();
        let mut rect = Rect::NOTHING;
        let _ = ctx.run(Default::default(), |ctx| {
            egui::Area::new(egui::Id::new("mvp_icon_size_probe")).show(ctx, |ui| {
                let response = icon_button(
                    ui,
                    Icon::Play,
                    18.0,
                    Color32::WHITE,
                    Color32::WHITE,
                    Color32::TRANSPARENT,
                    Color32::TRANSPARENT,
                );
                rect = response.rect;
            });
        });
        let expected = theme::button::of(28.0);
        assert!(
            (rect.width() - expected).abs() < 0.5 && (rect.height() - expected).abs() < 0.5,
            "an icon button measured {rect:?}, not {expected} pt square"
        );
    }

    /// The size the design passes in is the size *before* [`theme::button`], and an icon
    /// button's box is what the transport-bar layout model assumes — so this is where the two
    /// have to agree. A box that grew without the model noticing is a row of controls drawn
    /// on top of each other.
    #[test]
    fn an_icon_button_box_follows_the_button_size() {
        let ctx = egui::Context::default();
        let mut rect = Rect::NOTHING;
        let _ = ctx.run(Default::default(), |ctx| {
            egui::Area::new(egui::Id::new("mvp_icon_size_probe")).show(ctx, |ui| {
                let response = icon_button(
                    ui,
                    Icon::Play,
                    18.0,
                    Color32::WHITE,
                    Color32::WHITE,
                    Color32::TRANSPARENT,
                    Color32::TRANSPARENT,
                );
                rect = response.rect;
            });
        });
        let expected = theme::button::of(28.0);
        assert!(
            expected > 28.0,
            "the buttons are meant to be bigger than the design, not the same size"
        );
        assert!(
            (rect.width() - expected).abs() < 0.5 && (rect.height() - expected).abs() < 0.5,
            "an icon button measured {rect:?}, not {expected} pt square"
        );
    }

    /// The glyphs the transport bar's menus are drawn with have to stay in their box.
    ///
    /// They are painted, not laid out, so nothing else would catch a glyph that reaches
    /// past the rectangle it was given: it would either be clipped by the button or spill
    /// onto the control beside it, and both read as "the icon is broken" rather than as a
    /// drawing mistake.
    #[test]
    fn the_menu_glyphs_paint_inside_the_rectangle_they_are_given() {
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::new(40.0, 60.0), Vec2::splat(18.0));

        for icon in [Icon::Subtitles, Icon::Tracks, Icon::Image] {
            let output = ctx.run(Default::default(), |ctx| {
                // A painter of its own rather than an `Area`'s: an `Area` on its first
                // frame has no size yet, and its clip rectangle would drop the glyph
                // before this test could look at it — which is a fact about the harness,
                // not about the icon.
                let painter = Painter::new(
                    ctx.clone(),
                    egui::LayerId::new(
                        egui::Order::Background,
                        egui::Id::new(("mvp_icon_probe", icon)),
                    ),
                    egui::Rect::EVERYTHING,
                );
                draw(&painter, rect, icon, Color32::WHITE);
            });
            let painted: Vec<Rect> = output
                .shapes
                .iter()
                .map(|shape| shape.shape.visual_bounding_rect())
                .filter(|bounds| bounds.intersects(rect))
                .collect();
            assert!(!painted.is_empty(), "{icon:?} painted nothing at all");
            for bounds in painted {
                assert!(
                    rect.expand(1.0).contains_rect(bounds),
                    "{icon:?} painted outside its rectangle: {bounds:?} is not inside {rect:?}"
                );
            }
        }
    }
}
