//! The central canvas: video, still images, and the empty state.

use egui::{Color32, Context, Rect, RichText, Sense, Stroke, Ui, Vec2};

use crate::app::PlayerApp;
use crate::icons::{self, Icon};
use crate::settings::AspectMode;
use crate::state::{Mode, Overlay, Toast};
use crate::theme::{font, radius, space, Tokens};

/// Draw the central area.
pub fn draw(app: &mut PlayerApp, ctx: &Context) {
    let tokens = app.theme.tokens.clone();
    let frame = egui::Frame::new().fill(tokens.letterbox);
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        match app.mode {
            Mode::Empty => empty_state(app, ui, &tokens),
            Mode::Media => media_view(app, ui, &tokens),
            Mode::Image => image_view(app, ui, &tokens, ctx),
        }
    });
}

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

fn media_view(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let area = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(area, Sense::click_and_drag());

    // The *probe* is the source of truth for the picture's geometry. Using the
    // decoded frame's size instead would feed the decode target back into
    // itself: the first frame is scaled to whatever the target happens to be,
    // and every later frame would then be scaled to fit that — collapsing the
    // whole video into a thumbnail on the first frame.
    let probed = app
        .engine
        .info()
        .and_then(|info| info.video.first().map(|v| (v.width, v.height)))
        .filter(|(w, h)| *w > 1 && *h > 1);

    // Until the probe arrives, fall back to whatever has actually been decoded
    // so the letterbox is at least the right shape.
    let source = probed
        .or_else(|| (app.uploaded_size.0 > 1).then_some(app.uploaded_size))
        .unwrap_or((16, 9));

    let rect = destination_rect(area, source, app.settings.aspect, app.settings.rotation);
    app.last_video_rect = rect;

    // Ask the engine to decode at (at most) the size we actually display. A 4K
    // file shown in a 1080p window then costs a quarter of the memory and a
    // quarter of the conversion work. Only do so once the real source size is
    // known — guessing would permanently constrain the decoder.
    if let Some(source) = probed {
        let scale = ui.ctx().pixels_per_point();
        let viewport = (
            (rect.width() * scale).round().max(16.0) as u32,
            (rect.height() * scale).round().max(16.0) as u32,
        );
        let target = mvp_core::util::even(mvp_core::util::fit_inside(source, viewport));
        if target != app.video_target {
            app.video_target = target;
            app.engine.set_target_size(target.0, target.1);
        }
    }

    match &app.texture {
        Some(texture) => {
            let tint = Color32::WHITE;
            image_transformed(
                ui.painter(),
                texture.id(),
                rect,
                app.settings.rotation,
                app.settings.flip_h,
                app.settings.flip_v,
                tint,
            );
        }
        None => {
            ui.painter().text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                "正在准备画面…",
                egui::FontId::proportional(font::BODY),
                tokens.text_muted,
            );
        }
    }

    // ---- subtitles ------------------------------------------------------
    if app.settings.subtitles_enabled {
        draw_subtitles(app, ui, &rect, tokens);
    }

    // ---- interaction ----------------------------------------------------
    handle_video_interaction(app, ui, &response, &rect);

    // ---- state banners --------------------------------------------------
    let state = app.engine.state();
    if let mvp_core::PlaybackState::Error(message) = &state {
        ui.painter().text(
            area.center() + egui::vec2(0.0, -40.0),
            egui::Align2::CENTER_CENTER,
            message,
            egui::FontId::proportional(font::BODY),
            tokens.danger,
        );
    }

    // Hover scrub preview line.
    if let Some(time) = app.ui.video_hover_time {
        let duration = app.engine.duration();
        if duration > 0.0 {
            let fraction = (time / duration).clamp(0.0, 1.0) as f32;
            let x = area.left() + area.width() * fraction;
            ui.painter().line_segment(
                [
                    egui::pos2(x, area.top()),
                    egui::pos2(x, area.bottom()),
                ],
                Stroke::new(1.0, tokens.accent.gamma_multiply(0.5)),
            );
        }
    }
}

fn handle_video_interaction(
    app: &mut PlayerApp,
    ui: &mut Ui,
    response: &egui::Response,
    rect: &Rect,
) {
    if response.double_clicked() && app.settings.double_click_fullscreen {
        app.toggle_fullscreen(ui.ctx());
        return;
    }
    if response.clicked() {
        app.ui.wake_controls(3.0);
        return;
    }
    if response.dragged() {
        app.ui.wake_controls(3.0);
        app.image.pan(response.drag_delta().x, response.drag_delta().y);
    }

    // Scroll: volume by default, seek with Ctrl.
    let (scroll, modifiers) = ui.ctx().input(|i| (i.raw_scroll_delta.y, i.modifiers));
    if scroll.abs() > 0.5 {
        if modifiers.ctrl || !app.settings.wheel_controls_volume {
            let delta = (scroll as f64) * 0.5;
            app.engine.seek_relative(delta);
        } else {
            let step = scroll / 400.0;
            app.settings.volume = (app.settings.volume + step).clamp(0.0, 2.0);
            if app.settings.volume > 0.0 {
                app.settings.muted = false;
            }
            app.store.mark_dirty();
            app.toast(Toast::info(format!(
                "音量 {}%",
                (app.settings.volume * 100.0).round() as i32
            )));
        }
    }
    let _ = rect;
}

fn draw_subtitles(app: &PlayerApp, ui: &mut Ui, rect: &Rect, tokens: &Tokens) {
    let time = app.engine.display_position() - app.settings.subtitle_delay;
    let Some(cue) = app.engine.active_subtitle(time) else {
        return;
    };
    let text = cue.text.trim();
    if text.is_empty() {
        return;
    }

    let size = app.settings.subtitle_size.max(10.0);
    let color = Color32::from_rgb(
        app.settings.subtitle_color[0],
        app.settings.subtitle_color[1],
        app.settings.subtitle_color[2],
    );
    let max_width = rect.width() * 0.92;
    let galley = ui.fonts(|f| {
        f.layout(
            text.to_owned(),
            egui::FontId::proportional(size),
            color,
            max_width,
        )
    });
    let text_size = galley.size();
    let center_x = rect.center().x;
    let base_y = rect.bottom() - app.settings.subtitle_margin - text_size.y;
    let origin = egui::pos2(center_x - text_size.x / 2.0, base_y);

    let painter = ui.painter();
    if app.settings.subtitle_outline {
        // A soft plate behind the text reads far better over bright video than
        // a hard outline, and is what modern players ship.
        let plate = Rect::from_min_size(origin, text_size).expand2(Vec2::new(12.0, 6.0));
        painter.rect_filled(
            plate,
            egui::CornerRadius::same(6),
            Color32::from_black_alpha(140),
        );
    }
    painter.galley(origin, galley, color);
    let _ = tokens;
}

/// Compute where the picture goes inside `area`.
fn destination_rect(area: Rect, source: (u32, u32), mode: AspectMode, rotation: i32) -> Rect {
    let (mut width, mut height) = (source.0.max(1) as f32, source.1.max(1) as f32);
    if rotation.rem_euclid(180) == 90 {
        std::mem::swap(&mut width, &mut height);
    }
    let ratio = mode
        .forced_ratio()
        .unwrap_or(width / height)
        .max(0.01);

    match mode {
        AspectMode::Stretch => area,
        AspectMode::Crop => {
            let area_ratio = area.width() / area.height().max(1.0);
            let (w, h) = if ratio > area_ratio {
                (area.height() * ratio, area.height())
            } else {
                (area.width(), area.width() / ratio)
            };
            Rect::from_center_size(area.center(), Vec2::new(w, h))
        }
        _ => {
            let area_ratio = area.width() / area.height().max(1.0);
            let (w, h) = if ratio > area_ratio {
                (area.width(), area.width() / ratio)
            } else {
                (area.height() * ratio, area.height())
            };
            Rect::from_center_size(area.center(), Vec2::new(w, h))
        }
    }
}

/// Draw a texture with arbitrary rotation and mirroring.
///
/// `egui`'s `Painter::image` only accepts an axis-aligned rectangle plus a UV
/// rectangle, which can mirror but not rotate; a four-vertex mesh does both.
#[allow(clippy::too_many_arguments)]
pub fn image_transformed(
    painter: &egui::Painter,
    texture: egui::TextureId,
    rect: Rect,
    rotation: i32,
    flip_h: bool,
    flip_v: bool,
    tint: Color32,
) {
    // UV corners in the order (top-left, top-right, bottom-right, bottom-left),
    // permuted according to the rotation so the image appears rotated.
    let mut uv = match rotation.rem_euclid(360) {
        90 => [
            egui::pos2(0.0, 1.0),
            egui::pos2(0.0, 0.0),
            egui::pos2(1.0, 0.0),
            egui::pos2(1.0, 1.0),
        ],
        180 => [
            egui::pos2(1.0, 1.0),
            egui::pos2(0.0, 1.0),
            egui::pos2(0.0, 0.0),
            egui::pos2(1.0, 0.0),
        ],
        270 => [
            egui::pos2(1.0, 0.0),
            egui::pos2(1.0, 1.0),
            egui::pos2(0.0, 1.0),
            egui::pos2(0.0, 0.0),
        ],
        _ => [
            egui::pos2(0.0, 0.0),
            egui::pos2(1.0, 0.0),
            egui::pos2(1.0, 1.0),
            egui::pos2(0.0, 1.0),
        ],
    };
    if flip_h {
        uv.swap(0, 1);
        uv.swap(2, 3);
    }
    if flip_v {
        uv.swap(0, 3);
        uv.swap(1, 2);
    }

    let positions = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];

    let mut mesh = egui::Mesh::with_texture(texture);
    for (position, uv) in positions.iter().zip(uv.iter()) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: *position,
            uv: *uv,
            color: tint,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

fn image_view(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens, ctx: &Context) {
    let area = ui.available_rect_before_wrap();
    let Some((width, height)) = app.image.dimensions() else {
        empty_state(app, ui, tokens);
        return;
    };
    let response = ui.allocate_rect(area, Sense::click_and_drag());
    let pointer = response.hover_pos();

    // Scroll to zoom when the pointer is over the canvas.
    if response.hovered() {
        let scroll = ctx.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.5 {
            let factor = if scroll > 0.0 { 1.12 } else { 1.0 / 1.12 };
            app.image.zoom_by(factor, Some((area.width(), area.height())));
        }
    }

    if response.dragged() {
        app.image.pan(response.drag_delta().x, response.drag_delta().y);
    }
    if response.double_clicked() && app.settings.double_click_fullscreen {
        app.toggle_fullscreen(ctx);
        return;
    }

    let scale = app.image.effective_scale(Some((area.width(), area.height())));
    let draw_size = Vec2::new(width as f32 * scale, height as f32 * scale);
    let center = area.center() + Vec2::new(app.image.offset.0, app.image.offset.1);
    let rect = Rect::from_center_size(center, draw_size);

    // Cheap checkerboard for transparent images.
    paint_checkerboard(ui.painter(), rect, tokens);

    if let Some(texture) = &app.texture {
        image_transformed(
            ui.painter(),
            texture.id(),
            rect,
            app.image.rotation,
            app.image.flip_h,
            app.image.flip_v,
            Color32::WHITE,
        );
    }

    // Zoom readout in the corner.
    let label = format!("{}%  {}x{}", (scale * 100.0).round() as i32, width, height);
    ui.painter().text(
        egui::pos2(area.left() + space::MD, area.bottom() - space::MD),
        egui::Align2::LEFT_BOTTOM,
        label,
        egui::FontId::proportional(font::TINY),
        tokens.text_weak,
    );
    let _ = pointer;

    image_toolbar(app, ui, &area, tokens);
}

/// A floating toolbar with the image-viewer controls, shown at the bottom of the
/// canvas. Images need zoom, rotate, flip and slideshow controls that the
/// transport bar simply has no room (or meaning) for.
fn image_toolbar(app: &mut PlayerApp, ui: &mut Ui, area: &Rect, tokens: &Tokens) {
    let bar_height = 40.0;
    let bar_width = 360.0f32.min(area.width() - 24.0);
    let rect = Rect::from_center_size(
        egui::pos2(area.center().x, area.bottom() - space::XL - bar_height / 2.0),
        Vec2::new(bar_width, bar_height),
    );

    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(radius::MD as u8),
        tokens.elevated.gamma_multiply(0.92),
    );
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(radius::MD as u8),
        Stroke::new(1.0, tokens.border_strong),
        egui::StrokeKind::Inside,
    );

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(Vec2::new(space::SM, 6.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let ui = &mut child;
    ui.spacing_mut().item_spacing.x = space::XS;

    if crate::ui::widgets::tool_button(tokens, ui, Icon::ZoomOut, "缩小 (-)", true).clicked() {
        app.image.zoom_by(0.8, Some((area.width(), area.height())));
    }
    if crate::ui::widgets::tool_button(tokens, ui, Icon::ZoomIn, "放大 (+)", true).clicked() {
        app.image.zoom_by(1.25, Some((area.width(), area.height())));
    }
    if crate::ui::widgets::tool_button(tokens, ui, Icon::FitToWindow, "适应窗口 (0)", true).clicked()
    {
        app.image.fit = mvp_core::FitMode::Fit;
        app.image.offset = (0.0, 0.0);
    }
    if crate::ui::widgets::tool_button(tokens, ui, Icon::RotateCcw, "逆时针旋转", true).clicked() {
        app.image.rotate_ccw();
    }
    if crate::ui::widgets::tool_button(tokens, ui, Icon::RotateCw, "顺时针旋转 (R)", true).clicked()
    {
        app.image.rotate_cw();
    }
    if crate::ui::widgets::toggle_tool_button(
        tokens,
        ui,
        Icon::FlipHorizontal,
        "水平翻转",
        app.image.flip_h,
        true,
    )
    .clicked()
    {
        app.image.toggle_flip_h();
    }
    if crate::ui::widgets::toggle_tool_button(
        tokens,
        ui,
        Icon::FlipVertical,
        "垂直翻转",
        app.image.flip_v,
        true,
    )
    .clicked()
    {
        app.image.toggle_flip_v();
    }
    if crate::ui::widgets::toggle_tool_button(
        tokens,
        ui,
        Icon::Slideshow,
        "幻灯片播放",
        app.settings.slideshow_active,
        app.playlist.len() > 1,
    )
    .clicked()
    {
        app.settings.slideshow_active = !app.settings.slideshow_active;
        app.store.mark_dirty();
        let on = app.settings.slideshow_active;
        app.toast(Toast::info(if on { "幻灯片已开启" } else { "幻灯片已关闭" }));
    }
}

fn paint_checkerboard(painter: &egui::Painter, rect: Rect, tokens: &Tokens) {
    const CELL: f32 = 12.0;
    painter.rect_filled(rect, egui::CornerRadius::ZERO, Color32::from_gray(28));
    let cols = (rect.width() / CELL).ceil() as i32;
    let rows = (rect.height() / CELL).ceil() as i32;
    // Cap the number of cells so a wildly zoomed-out image does not generate
    // tens of thousands of rectangles.
    if cols * rows > 4000 {
        return;
    }
    for row in 0..rows {
        for col in 0..cols {
            if (row + col) % 2 == 0 {
                continue;
            }
            let min = rect.min + Vec2::new(col as f32 * CELL, row as f32 * CELL);
            let cell = Rect::from_min_size(min, Vec2::splat(CELL)).intersect(rect);
            if cell.width() > 0.5 && cell.height() > 0.5 {
                painter.rect_filled(cell, egui::CornerRadius::ZERO, Color32::from_gray(36));
            }
        }
    }
    let _ = tokens;
}

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

fn empty_state(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let area = ui.available_rect_before_wrap();
    ui.scope_builder(egui::UiBuilder::new().max_rect(area.shrink(space::XL)), |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space((area.height() * 0.16).max(24.0));

            // Logo: a large accent circle with the play glyph.
            let (logo_rect, _) =
                ui.allocate_exact_size(Vec2::splat(84.0), Sense::hover());
            ui.painter().circle_filled(
                logo_rect.center(),
                42.0,
                tokens.accent.gamma_multiply(0.18),
            );
            ui.painter().circle_stroke(
                logo_rect.center(),
                42.0,
                Stroke::new(1.5, tokens.accent.gamma_multiply(0.55)),
            );
            icons::draw(
                ui.painter(),
                Rect::from_center_size(logo_rect.center(), Vec2::splat(38.0)),
                Icon::Play,
                tokens.accent,
            );

            ui.add_space(space::LG);
            ui.label(
                RichText::new("MVP-Versatile-Player")
                    .size(font::H1)
                    .strong()
                    .color(tokens.text),
            );
            ui.add_space(space::XS);
            ui.label(
                RichText::new("FFmpeg 内核 · 音视频与图片播放器")
                    .size(font::SMALL)
                    .color(tokens.text_weak),
            );

            ui.add_space(space::XL);
            ui.horizontal(|ui| {
                let width = 460.0f32.min(area.width() - 40.0);
                let side = ((ui.available_width() - width) / 2.0).max(0.0);
                ui.add_space(side);
                if ui
                    .add_sized(
                        Vec2::new(width / 2.0 - space::XS, 40.0),
                        egui::Button::new(
                            RichText::new("打开文件…").size(font::BODY),
                        )
                        .fill(tokens.accent),
                    )
                    .clicked()
                {
                    app.request_open_file();
                }
                if ui
                    .add_sized(
                        Vec2::new(width / 2.0 - space::XS, 40.0),
                        egui::Button::new(RichText::new("打开文件夹…").size(font::BODY))
                            .fill(tokens.elevated),
                    )
                    .clicked()
                {
                    app.request_open_folder();
                }
            });

            ui.add_space(space::MD);
            ui.vertical_centered(|ui| {
                if ui
                    .button(RichText::new("打开网络串流…").size(font::SMALL))
                    .clicked()
                {
                    app.ui.url_input.clear();
                    app.ui.url_focus = true;
                    app.ui.open_overlay(Overlay::OpenUrl);
                }
            });

            ui.add_space(space::MD);
            ui.label(
                RichText::new("也可以直接把文件或文件夹拖入窗口")
                    .size(font::TINY)
                    .color(tokens.text_muted),
            );

            // ---- recent files ------------------------------------------
            app.settings.prune_recent();
            if !app.settings.recent_files.is_empty() {
                ui.add_space(space::XL);
                ui.label(
                    RichText::new("最近播放")
                        .size(font::H3)
                        .color(tokens.text),
                );
                ui.add_space(space::XS);
                let entries: Vec<std::path::PathBuf> =
                    app.settings.recent_files.iter().take(6).cloned().collect();
                let mut chosen = None;
                let width = 520.0f32.min(area.width() - 40.0);
                for path in entries {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_string_lossy().into_owned());
                    let full = path.to_string_lossy().into_owned();
                    ui.scope(|ui| {
                        let side = ((ui.available_width() - width) / 2.0).max(0.0);
                        ui.add_space(side);
                        let button = ui.add_sized(
                            Vec2::new(width, 26.0),
                            egui::Button::new(
                                RichText::new(format!("· {name}"))
                                    .size(font::SMALL)
                                    .color(tokens.text_weak),
                            )
                            .frame(false),
                        );
                        if button.on_hover_text(&full).clicked() {
                            chosen = Some(path.clone());
                        }
                    });
                }
                if let Some(path) = chosen {
                    app.open_paths(&[path], true, true);
                }
            }

            if app.settings.show_statistics {
                ui.add_space(space::LG);
                ui.label(
                    RichText::new(format!(
                        "启动耗时 {:.0} 毫秒 · {}",
                        app.ui.startup_ms,
                        mvp_core::ffmpeg_version()
                    ))
                    .size(font::TINY)
                    .color(tokens.text_muted),
                );
            }
        });
    });
    let _ = radius::SM;
}

