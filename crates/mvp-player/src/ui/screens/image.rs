//! The image screen.
//!
//! A photograph is not a paused film. There is no timeline, no audio, no
//! subtitles — and the controls that matter are the ones that change how the
//! picture is *looked at*: zoom, rotation, mirroring, the fit mode and the
//! slideshow. The screen therefore replaces the transport bar with a filmstrip
//! of the other pictures in the playlist and keeps the picture tools on the
//! canvas itself, where the picture is.

use std::path::PathBuf;

use egui::{Color32, Context, Rect, Sense, Vec2};

use crate::app::PlayerApp;
use crate::icons::{self, Icon};
use crate::theme::{radius, space, Tokens};
use crate::ui::{canvas, sidebar, surface};

/// Height of the thumbnail strip.
const STRIP_HEIGHT: f32 = 92.0;
/// Thumbnail cell, in points.
const CELL: Vec2 = Vec2::new(112.0, 63.0);

pub(super) fn draw(app: &mut PlayerApp, ctx: &Context) {
    if app.ui.sidebar_visible {
        sidebar::draw(app, ctx);
    }
    // Fullscreen is the photograph and nothing else; the strip is a browsing
    // tool, and browsing is what windowed mode is for.
    if !app.ui.fullscreen {
        filmstrip(app, ctx);
    }
    canvas::draw_image(app, ctx);
}

/// One row of thumbnails for the still images in the playlist.
///
/// The strip is the image viewer's answer to the transport bar: the playlist is
/// not a separate page to open, it is the row of pictures under the one being
/// looked at. A non-image entry keeps its place with its kind glyph rather than
/// its thumbnail, so the order matches the real playlist exactly.
fn filmstrip(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();

    // Snapshot the entries before touching the cache: the loop below needs the
    // playlist (to draw) and the cache (to request) at the same time, and they
    // both live behind `app`.
    let entries: Vec<StripEntry> = app
        .playlist
        .items()
        .iter()
        .enumerate()
        .map(|(index, item)| {
            if item.is_url {
                return StripEntry {
                    index,
                    path: None,
                    title: item.title.clone(),
                    kind: Icon::Link,
                };
            }
            let path = item.path();
            let kind = match mvp_core::util::classify(&path) {
                mvp_core::MediaKind::Image => Icon::Image,
                mvp_core::MediaKind::Audio => Icon::Music,
                mvp_core::MediaKind::Video => Icon::Film,
                _ => Icon::Grid,
            };
            StripEntry {
                index,
                path: Some(path),
                title: item.title.clone(),
                kind,
            }
        })
        .collect();

    for entry in &entries {
        if entry.kind == Icon::Image {
            if let Some(path) = &entry.path {
                app.thumbs.request(path);
            }
        }
    }

    // Nothing to browse: a single picture needs no strip, and neither does an
    // empty playlist.
    if entries.len() <= 1 {
        return;
    }

    let current = app.playlist.current_index();
    let mut clicked: Option<usize> = None;

    egui::TopBottomPanel::bottom("mvp_filmstrip")
        .frame(surface::bar_shell(
            &tokens,
            egui::Margin::symmetric(space::SM as i8, space::XS as i8),
        ))
        .exact_height(STRIP_HEIGHT)
        .show(ctx, |ui| {
            egui::ScrollArea::horizontal()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = space::SM;
                        for entry in &entries {
                            if let Some(index) = draw_cell(app, ui, &tokens, entry, current) {
                                clicked = Some(index);
                            }
                        }
                    });
                });
        });

    if let Some(index) = clicked {
        app.play_index(index);
    }
}

/// A playlist entry reduced to what the strip needs to draw.
struct StripEntry {
    index: usize,
    path: Option<PathBuf>,
    title: String,
    kind: Icon,
}

/// Draw one thumbnail cell; return the entry's index when it is clicked.
fn draw_cell(
    app: &PlayerApp,
    ui: &mut egui::Ui,
    tokens: &Tokens,
    entry: &StripEntry,
    current: Option<usize>,
) -> Option<usize> {
    let (rect, response) = ui.allocate_exact_size(CELL, Sense::click());
    let active = current == Some(entry.index);
    let hovered = response.hovered();

    let texture = entry
        .path
        .as_deref()
        .and_then(|path| app.thumbs.get(path).cloned());

    let fill = if hovered {
        tokens.hover
    } else {
        tokens.sunken
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(radius::SM as u8), fill);

    let inner = rect.shrink(2.0);
    if let Some(texture) = texture {
        let size = texture.size_vec2();
        let scale = (inner.width() / size.x).min(inner.height() / size.y);
        let drawn = Rect::from_center_size(inner.center(), size * scale);
        ui.painter().image(
            texture.id(),
            drawn,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        icons::draw(ui.painter(), inner.shrink(inner.height() * 0.28), entry.kind, tokens.text_muted);
    }

    // The current picture is marked the way the playlist marks it: an accent
    // stroke rather than a filled row, so the thumbnail stays visible.
    if active {
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(radius::SM as u8),
            egui::Stroke::new(2.0_f32, tokens.accent),
            egui::StrokeKind::Inside,
        );
    }

    if response.clicked() {
        return Some(entry.index);
    }
    response.on_hover_text(&entry.title);
    None
}
