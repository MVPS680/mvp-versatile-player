//! The central canvas: video, still images, and the empty state.

use egui::{Color32, Context, Rect, RichText, Sense, Stroke, Ui, Vec2};

use crate::app::PlayerApp;
use crate::icons::{self, Icon};
use crate::settings::AspectMode;
use crate::state::{Mode, Overlay, Toast};
use crate::theme::{font, radius, space, Tokens};
use crate::ui::surface;
use crate::ui::widgets;

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
// Video, audio and the canvas they share
// ---------------------------------------------------------------------------

/// Draw whatever the engine has open: a picture, or the audio screen.
fn media_view(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let area = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(area, Sense::click_and_drag());

    // A file that is nothing but sound has no picture to letterbox, so the
    // canvas becomes a screen of its own instead of holding "正在准备画面…" up
    // for as long as the file plays.
    let audio = app.is_audio_only();

    // ---- control-bar scrim ----------------------------------------------
    // A dimming gradient at the bottom of the picture, so the controls read
    // over bright video. In windowed mode the transport bar is docked right
    // below it; in fullscreen the bar floats over the picture and only appears
    // with the controls, so the scrim follows it. The audio screen has no
    // picture to dim, and its own colours are already the dark ones.
    if !audio && app.settings.control_scrim && (!app.ui.fullscreen || app.ui.controls_visible()) {
        let height = if app.ui.fullscreen { 120.0 } else { 96.0 };
        let band = Rect::from_min_max(
            egui::pos2(area.left(), (area.bottom() - height).max(area.top())),
            egui::pos2(area.right(), area.bottom()),
        );
        widgets::paint_scrim(ui.painter(), band);
    }

    // Where the picture went, or `None` when there is no picture at all.
    let picture = if audio {
        audio_view(app, ui, tokens, area);
        None
    } else {
        Some(video_view(app, ui, tokens, area))
    };

    // ---- subtitles ------------------------------------------------------
    // Over the picture — or, with no picture to be over, along the bottom of
    // the audio screen, which is what a subtitle track is still good for there
    // (a lyrics file, most of the time).
    if app.settings.subtitles_enabled {
        let anchor = picture.unwrap_or(area);
        draw_subtitles(app, ui, &anchor, tokens);
    }

    // ---- interaction ----------------------------------------------------
    handle_video_interaction(app, ui, &response);

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

    // The hover scrub line used to be drawn here: a one pixel accent stroke down
    // the whole picture, following the pointer over the seek bar and marking where
    // a click would land. Over a film it read as a stray hairline travelling with
    // the cursor rather than as help, and the seek bar already reports the time
    // under the pointer, so it is gone. `video_hover_time` stays: the timestamp
    // readout in the transport bar is what the viewer actually needs.
}

/// Draw the video frame, and report the rectangle it was given.
///
/// The rectangle is what the rest of the canvas is measured against — the
/// subtitles, the scrub line, the decode target — so it is handed back rather
/// than recomputed by everyone who needs it.
fn video_view(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens, area: Rect) -> Rect {
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
            // A spinner rather than a static line of text: "preparing" is a state
            // that ends by itself, and a ring whose gap moves is the difference
            // between "working" and "stuck".
            let center = area.center();
            widgets::spinner(
                ui,
                tokens,
                center - egui::vec2(0.0, 24.0),
                28.0,
                ui.input(|i| i.time),
            );
            ui.painter().text(
                center,
                egui::Align2::CENTER_CENTER,
                "正在准备画面…",
                egui::FontId::proportional(font::BODY),
                tokens.text_muted,
            );
        }
    }

    // The subtitles, the scrub line, the state banner and the pointer handling
    // all live in `media_view` now: they need the picture's rectangle, not the
    // canvas, and this function is only responsible for putting the frame on
    // screen and reporting where it landed.
    rect
}

/// Clicks, drags and the wheel on the video canvas and the audio screen.
///
/// Nothing here moves the picture. A video is fitted to the canvas — that is what
/// `destination_rect` decides, from the source size and the aspect setting — so a drag
/// over it used to do nothing to the video at all while quietly writing to the *image*
/// viewer's pan, which is a different mode's state: dragging a *still* is the viewer's
/// job, and it has its own handling for it. What a drag does here is wake the
/// controls, which is what a click does.
fn handle_video_interaction(app: &mut PlayerApp, ui: &mut Ui, response: &egui::Response) {
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
    }

    // Scroll: volume by default, seek with Ctrl.
    //
    // One notch is one step, whatever the device reports: a notched wheel sends
    // a single 40-point spike per notch while a precision touch-pad sends a
    // stream of small deltas, so the movement is accumulated and spent in whole
    // steps. Feeding the raw delta straight into the volume (as this used to)
    // made a notch worth 10% and a touch-pad worth nothing at all.
    //
    // Only while the pointer is over the picture and no dialog is open: egui's
    // scroll areas consume the *smoothed* delta and leave the raw one alone, so
    // without this guard scrolling the settings list would quietly change the
    // volume at the same time.
    let (scroll, modifiers) = ui.ctx().input(|i| (i.raw_scroll_delta.y, i.modifiers));
    if scroll != 0.0 && response.hovered() && !app.ui.has_overlay() {
        if modifiers.ctrl || !app.settings.wheel_controls_volume {
            app.ui.wheel_volume = 0.0;
            app.ui.wheel_seek += scroll;
            let (steps, leftover) = widgets::wheel_steps(app.ui.wheel_seek, WHEEL_POINTS_PER_NOTCH);
            app.ui.wheel_seek = leftover;
            if steps != 0 {
                app.seek_relative(steps as f64 * app.settings.seek_step);
            }
        } else {
            app.ui.wheel_seek = 0.0;
            app.ui.wheel_volume += scroll;
            let (steps, leftover) =
                widgets::wheel_steps(app.ui.wheel_volume, WHEEL_POINTS_PER_NOTCH);
            app.ui.wheel_volume = leftover;
            if steps != 0 {
                let volume = app.settings.volume + steps as f32 * WHEEL_VOLUME_STEP;
                let clamped = volume.clamp(0.0, 2.0);
                // At either end the leftover must go, or the accumulator keeps
                // banking steps the user cannot see and the next scroll in the
                // other direction jumps.
                if clamped != volume {
                    app.ui.wheel_volume = 0.0;
                }
                app.settings.volume = clamped;
                if clamped > 0.0 {
                    app.settings.muted = false;
                }
                app.store.mark_dirty();
                app.toast(Toast::info(format!(
                    "音量 {}%",
                    (clamped * 100.0).round() as i32
                )));
            }
        }
    }
}

/// Wheel points that make one step, matching egui's native `line_scroll_speed`
/// so that one physical notch is exactly one step.
const WHEEL_POINTS_PER_NOTCH: f32 = 40.0;

/// Volume change per wheel notch — the same 5 % the arrow keys use.
const WHEEL_VOLUME_STEP: f32 = 0.05;

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
// Audio
// ---------------------------------------------------------------------------

/// Room kept between the record's rim and the ring around it.
const AUDIO_RING_INSET: f32 = 10.0;
/// Gap between the rim and the ring itself, inside that room.
const AUDIO_RING_GAP: f32 = 5.0;
/// Stroke width of the ring.
const AUDIO_RING_WIDTH: f32 = 3.0;
/// Where the ring's grooves sit, as fractions of the radius.
const AUDIO_GROOVES: [f32; 3] = [0.60, 0.72, 0.84];
/// Radius of the label at the centre of the record, as a fraction of the whole.
const AUDIO_LABEL: f32 = 0.34;
/// How far the glow around the record reaches, as a multiple of its own radius.
const AUDIO_GLOW_SPREAD: f32 = 1.20;
/// Alpha of that glow where it leaves the record.
const AUDIO_GLOW_ALPHA: f32 = 0.20;
/// Widest the sleeve's type is allowed to be, as a fraction of the canvas.
const AUDIO_TEXT_WIDTH: f32 = 0.86;

/// The screen an audio-only file gets.
///
/// A file with no picture used to be drawn by the video canvas, where it sat on
/// 「正在准备画面…」 from the first frame to the last: there was nothing to
/// prepare, and nothing to show. What such a file does have is sound, so the
/// canvas shows what the sleeve of a record shows — what is playing, who made
/// it, how it is encoded and how far through it is — and the playhead becomes a
/// shape on it (the ring around the record) rather than only a number under it.
///
/// Everything is painted rather than laid out as widgets: the screen is one
/// column of centred blocks whose sizes all come from [`crate::layout`], and a
/// label that wraps, clips or re-measures itself is exactly what would break it.
fn audio_view(app: &PlayerApp, ui: &mut Ui, tokens: &Tokens, area: Rect) {
    let screen = crate::layout::AudioScreen::fit(area.width(), area.height());
    let info = app.engine.info();

    // The title is the file's own answer to "what is this?" when it has one;
    // the file name is the fallback, and it is a good one — an untagged MP3
    // called 夜曲.mp3 is still called 夜曲.
    let title = info
        .as_ref()
        .and_then(|info| info.tag("title"))
        .map(str::to_owned)
        .or_else(|| app.playlist.current().map(|item| item.title.clone()))
        .unwrap_or_else(|| "未知标题".to_string());
    let caption = sleeve_caption(info.as_deref());
    let facts = if screen.facts {
        info.as_deref().map(encoding_line).unwrap_or_default()
    } else {
        String::new()
    };
    let clock = elapsed_line(app);

    // One galley per block, laid out before anything is painted: the blocks are
    // stacked by what they actually measure, and every line is cut to the room
    // there is with an ellipsis, because a title that runs off the edge of the
    // canvas has nowhere to run to.
    let width = (area.width() * AUDIO_TEXT_WIDTH).max(80.0);
    let caption_font = egui::FontId::proportional(screen.caption);
    let blocks: [(egui::FontId, Color32, String); 4] = [
        (egui::FontId::proportional(screen.title), tokens.text, title),
        (caption_font.clone(), tokens.text_weak, caption),
        (caption_font, tokens.text_muted, facts),
        (egui::FontId::monospace(screen.clock), tokens.text, clock),
    ];
    let galleys: Vec<(std::sync::Arc<egui::Galley>, Color32)> = blocks
        .into_iter()
        .filter(|(_, _, text)| !text.is_empty())
        .map(|(font, color, text)| (one_line(ui, &text, font, color, width), color))
        .collect();

    // Where the column starts: centred on what it actually measures, and never
    // taller than the height the layout sized it for — that arithmetic rounds
    // up (see `layout::LINE_HEIGHT`), so it is a ceiling the measurement stays
    // under rather than the number to place the poster by.
    let text_height: f32 = galleys.iter().map(|(galley, _)| galley.size().y).sum();
    let measured = screen.artwork + text_height + screen.gap * galleys.len() as f32;
    let total = measured.min(screen.height());
    let center_x = area.center().x;
    let mut y = (area.center().y - total / 2.0).max(area.top());

    let artwork = Rect::from_min_size(
        egui::pos2(center_x - screen.artwork / 2.0, y),
        Vec2::splat(screen.artwork),
    );
    draw_record(app, ui.painter(), artwork, tokens);
    y += screen.artwork;

    for (galley, color) in &galleys {
        y += screen.gap;
        let size = galley.size();
        ui.painter().galley(
            egui::pos2(center_x - size.x / 2.0, y),
            galley.clone(),
            *color,
        );
        y += size.y;
    }
}

/// Layout a single line of text, cut with an ellipsis when it does not fit.
fn one_line(
    ui: &Ui,
    text: &str,
    font: egui::FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts(|fonts| fonts.layout_job(job))
}

/// The "who and what" line under the title: the tags, or the container.
///
/// A file with no tags at all still gets an answer — "MP3 (MPEG audio layer 3)"
/// at least says what kind of thing is playing — and one whose probe has not
/// landed yet gets nothing rather than a guess.
fn sleeve_caption(info: Option<&mvp_core::MediaInfo>) -> String {
    let Some(info) = info else {
        return String::new();
    };
    let artist = info.tag("artist").or_else(|| info.tag("album_artist"));
    match (artist, info.tag("album")) {
        (Some(artist), Some(album)) => format!("{artist} — {album}"),
        (Some(artist), None) => artist.to_string(),
        (None, Some(album)) => album.to_string(),
        (None, None) => info.format_long_name.clone(),
    }
}

/// The technical line: what the sound is, in the terms a file has for it.
fn encoding_line(info: &mvp_core::MediaInfo) -> String {
    let Some(audio) = info.primary_audio() else {
        return String::new();
    };
    let mut parts = vec![audio.codec.to_uppercase()];
    if audio.sample_rate > 0 {
        parts.push(format!("{} Hz", audio.sample_rate));
    }
    if audio.channels > 0 {
        parts.push(format!("{} 声道", audio.channels));
    }
    // The stream's own bit rate when it declares one — a VBR MP3 usually does
    // not, and the container's average is then the honest number.
    let bit_rate = if audio.bit_rate > 0 {
        audio.bit_rate
    } else {
        info.bit_rate
    };
    if bit_rate > 0 {
        parts.push(mvp_core::util::format_bitrate(bit_rate));
    }
    parts.join(" · ")
}

/// The clock under the record: how far in, and how long the file is.
///
/// A stream has no duration to show — the engine reports `0.0` until (and
/// unless) the container says otherwise — and inventing one would be worse than
/// the single number.
fn elapsed_line(app: &PlayerApp) -> String {
    let position = mvp_core::util::format_duration(app.engine.display_position());
    let duration = app.engine.duration();
    if duration > 0.0 {
        format!("{position} / {}", mvp_core::util::format_duration(duration))
    } else {
        position
    }
}

/// Paint the record, with the playhead as a ring around it.
///
/// The grooves are decoration. The ring is not: it is the one place on this
/// screen where the position is a shape rather than a number, which is what
/// makes "nearly over" readable from across the room.
fn draw_record(app: &PlayerApp, painter: &egui::Painter, rect: Rect, tokens: &Tokens) {
    let center = rect.center();
    let radius = (rect.width().min(rect.height()) / 2.0 - AUDIO_RING_INSET).max(4.0);

    // A glow, as a single mesh: one ring of vertices on the record's own edge and
    // another at the far edge of the halo, with the alpha carried down from one to
    // the other. `egui` has no radial-gradient primitive, and what this replaces
    // was four translucent discs at 4 % steps — four *hard* circles a few points
    // apart, each with its own visible rim. That is a set of rings, not a glow.
    paint_halo(
        painter,
        center,
        radius,
        radius * AUDIO_GLOW_SPREAD,
        tokens.accent,
    );

    // The record itself: a dark disc, its rim, and the grooves a record has.
    painter.circle_filled(center, radius, tokens.elevated);
    painter.circle_stroke(center, radius, Stroke::new(1.0_f32, tokens.border_strong));
    for groove in AUDIO_GROOVES {
        painter.circle_stroke(center, radius * groove, Stroke::new(1.0_f32, tokens.border));
    }
    painter.circle_filled(center, radius * AUDIO_LABEL, tokens.accent);
    icons::draw(
        painter,
        Rect::from_center_size(center, Vec2::splat(radius * AUDIO_LABEL)),
        Icon::Music,
        tokens.on_accent,
    );

    // The playhead, drawn last so it reads over the halo.
    let duration = app.engine.duration();
    if duration > 0.0 {
        let played = (app.engine.display_position() / duration).clamp(0.0, 1.0) as f32;
        let ring = radius + AUDIO_RING_GAP;
        painter.circle_stroke(center, ring, Stroke::new(AUDIO_RING_WIDTH, tokens.track));
        if played > 0.0 {
            // Clockwise from the top, which is where a clock starts.
            let color = if app.engine.is_playing() {
                tokens.progress
            } else {
                tokens.accent.gamma_multiply(0.55)
            };
            stroke_arc(
                painter,
                center,
                ring,
                -std::f32::consts::FRAC_PI_2,
                -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * played,
                Stroke::new(AUDIO_RING_WIDTH, color),
            );
        }
    }
}

/// A soft radial glow, from `inner` (at `alpha`) out to `outer` (at nothing).
///
/// Built as an annulus mesh rather than as a stack of discs, because a stack of
/// discs is a stack of edges: five of them at 4 % radius steps read as five rings.
/// Two rings of vertices interpolate the alpha across the whole band instead, so
/// the falloff is continuous — a stack of discs is a stack of edges, and five of them
/// at 4 % radius steps read as five rings.
fn paint_halo(
    painter: &egui::Painter,
    center: egui::Pos2,
    inner: f32,
    outer: f32,
    color: Color32,
) {
    /// Segments around the circle.
    const SEGMENTS: usize = 48;

    if outer <= inner || !inner.is_finite() || !outer.is_finite() {
        return;
    }
    let mut mesh = egui::Mesh::default();
    let mut ring = |radius: f32, alpha: f32| {
        for step in 0..=SEGMENTS {
            let angle = step as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let (sin, cos) = angle.sin_cos();
            mesh.colored_vertex(
                egui::pos2(center.x + cos * radius, center.y + sin * radius),
                color.gamma_multiply(alpha),
            );
        }
    };
    ring(inner, AUDIO_GLOW_ALPHA);
    ring(outer, 0.0);
    let stride = (SEGMENTS + 1) as u32;
    for index in 0..SEGMENTS as u32 {
        mesh.add_triangle(index, index + stride, index + 1);
        mesh.add_triangle(index + 1, index + stride, index + 1 + stride);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Stroke an arc of a circle.
///
/// `egui`'s painter has no arc primitive (`icons` has a private one over `Shape`,
/// which is not reachable from here), and the ring around the record is the one
/// shape on this screen that is genuinely an arc.
fn stroke_arc(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    from: f32,
    to: f32,
    stroke: Stroke,
) {
    /// Segments in a full turn; an arc takes its share of them.
    const SEGMENTS: f32 = 96.0;
    let sweep = to - from;
    let steps = ((SEGMENTS * sweep.abs() / std::f32::consts::TAU).ceil() as usize).max(2);
    let points: Vec<egui::Pos2> = (0..=steps)
        .map(|step| {
            let angle = from + sweep * (step as f32 / steps as f32);
            let (sin, cos) = angle.sin_cos();
            egui::pos2(center.x + cos * radius, center.y + sin * radius)
        })
        .collect();
    painter.add(egui::Shape::line(points, stroke));
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
    // The bar is narrower than its design only when the canvas really is that
    // narrow; the floor stops a tiny window from producing a negative width.
    let bar_width = (area.width() - 24.0).clamp(160.0, 360.0);
    let rect = Rect::from_center_size(
        egui::pos2(area.center().x, area.bottom() - space::XL - bar_height / 2.0),
        Vec2::new(bar_width, bar_height),
    );

    // The same surface as every other floating panel in the player: an opaque
    // `elevated` fill, one hairline, one corner radius. This bar used to be a rectangle
    // with a hard 1 pt outline drawn inside it, which is the single most unfinished
    // edge a dark interface can have, and then briefly a sheet of glass — which meant
    // re-blurring the photograph behind it every frame to draw seven buttons.
    surface::paint_sheet(ui.painter(), tokens, rect, radius::MD);

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
    /// Cell size in points, before it is snapped to whole pixels.
    const CELL: f32 = 12.0;

    // Both the grid and its origin are snapped to whole *device* pixels first. A
    // checkerboard whose cell edges fall between pixels gets a grey seam along
    // every edge, and on a transparent PNG — which is the only thing this is drawn
    // for — that reads as a fine mesh of scratches ruled over the picture.
    let scale = painter.ctx().pixels_per_point().max(1.0);
    let snap = |value: f32| (value * scale).round() / scale;
    let cell = snap(CELL).max(1.0);
    let rect = Rect::from_min_max(
        egui::pos2(snap(rect.min.x), snap(rect.min.y)),
        egui::pos2(snap(rect.max.x), snap(rect.max.y)),
    );

    painter.rect_filled(rect, egui::CornerRadius::ZERO, Color32::from_gray(28));
    let cols = (rect.width() / cell).ceil() as i32;
    let rows = (rect.height() / cell).ceil() as i32;
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
            let min = rect.min + Vec2::new(col as f32 * cell, row as f32 * cell);
            let square = Rect::from_min_size(min, Vec2::splat(cell)).intersect(rect);
            if square.width() > 0.5 && square.height() > 0.5 {
                painter.rect_filled(square, egui::CornerRadius::ZERO, Color32::from_gray(36));
            }
        }
    }
    let _ = tokens;
}

// ---------------------------------------------------------------------------
// Empty state
// ---------------------------------------------------------------------------

/// The player's welcome screen: shown until something is open.
///
/// Deliberately **not** a `ScrollArea`. The canvas is where a picture goes; a column
/// that can be scrolled inside it reads as a web page rather than as a player, and the
/// wheel over the canvas belongs to the volume (and to seeks with Ctrl held) — a
/// scroll area in front of it would swallow both. What the scrolling was standing in
/// for is done properly here instead: every block and every gap takes its size from
/// the room actually available, so the column *fits* the canvas on a short window
/// rather than sliding around in it.
fn empty_state(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    empty_blocks(app, ui, tokens);
}

/// The blocks in the welcome column, top to bottom.
fn empty_blocks(app: &mut PlayerApp, ui: &mut Ui, tokens: &Tokens) {
    let area = ui.available_rect_before_wrap();
    // The column is inset by the same margin on both sides, and the two centred
    // blocks take their width from what is left of it.
    let room = (area.width() - 2.0 * space::XL).max(1.0);
    let metrics = crate::layout::Metrics::new(area.width(), area.height());
    let content = metrics.content_width(room, 460.0, 180.0);
    let recent_width = metrics.content_width(room, 520.0, 180.0);
    // Two steps down in size, so the column can give room back without ever being
    // clipped: the logo shrinks, the gaps close up, and the recent list — the one
    // block that is a convenience rather than the screen's purpose — goes away
    // entirely before the buttons would.
    let short = area.height() < 620.0;
    let tiny = area.height() < 440.0;
    let logo = match (tiny, short) {
        (true, _) => 44.0,
        (false, true) => 60.0,
        (false, false) => 84.0,
    };
    let gap_before_buttons = if short { space::LG } else { space::XL };
    let gap_after_buttons = if short { space::SM } else { space::MD };

    ui.scope_builder(egui::UiBuilder::new().max_rect(area.shrink(space::XL)), |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space((area.height() * 0.16).clamp(8.0, 96.0));

            // Logo: a large accent circle with the play glyph. It shrinks with
            // the window so the column keeps its proportions on a small screen.
            let (logo_rect, _) = ui.allocate_exact_size(Vec2::splat(logo), Sense::hover());
            let radius = logo / 2.0;
            ui.painter().circle_filled(
                logo_rect.center(),
                radius,
                tokens.accent.gamma_multiply(0.18),
            );
            ui.painter().circle_stroke(
                logo_rect.center(),
                radius,
                Stroke::new(1.5_f32, tokens.accent.gamma_multiply(0.55)),
            );
            icons::draw(
                ui.painter(),
                Rect::from_center_size(logo_rect.center(), Vec2::splat(logo * 0.45)),
                Icon::Play,
                tokens.accent,
            );

            ui.add_space(space::LG);
            ui.label(
                RichText::new("MVP-Versatile-Player")
                    .font(crate::theme::strong_font(font::H1))
                    .color(tokens.text),
            );
            ui.add_space(space::XS);
            ui.label(
                RichText::new("FFmpeg 内核 · 音视频与图片播放器")
                    .size(font::SMALL)
                    .color(tokens.text_weak),
            );

            ui.add_space(gap_before_buttons);
            ui.horizontal(|ui| {
                let width = content;
                let half = (width / 2.0 - space::XS).max(48.0);
                let side = ((ui.available_width() - width) / 2.0).max(0.0);
                ui.add_space(side);
                // One accent button, one quiet one. The accent marks the action
                // the screen exists for; nothing else on the page wears it.
                if widgets::primary_button(ui, tokens, "打开文件…", half) {
                    app.request_open_file();
                }
                if widgets::secondary_button(ui, tokens, "打开文件夹…", half) {
                    app.request_open_folder();
                }
            });

            ui.add_space(gap_after_buttons);
            ui.vertical_centered(|ui| {
                if widgets::secondary_button(ui, tokens, "打开网络串流…", 0.0) {
                    app.ui.url_input.clear();
                    app.ui.url_focus = true;
                    app.ui.open_overlay(Overlay::OpenUrl);
                }
            });

            ui.add_space(gap_after_buttons);
            ui.label(
                RichText::new("也可以直接把文件或文件夹拖入窗口")
                    .size(font::TINY)
                    .color(tokens.text_muted),
            );

            // ---- recent files ------------------------------------------
            app.settings.prune_recent();
            if !app.settings.recent_files.is_empty() && !tiny {
                ui.add_space(if short { space::LG } else { space::XXL });
                ui.label(
                    RichText::new("最近播放")
                        .font(crate::theme::strong_font(if short {
                            font::BODY
                        } else {
                            font::H3
                        }))
                        .color(tokens.text),
                );
                ui.add_space(space::XS);
                let entries: Vec<std::path::PathBuf> = app
                    .settings
                    .recent_files
                    .iter()
                    .take(if short { 3 } else { 6 })
                    .cloned()
                    .collect();
                let mut chosen = None;
                let width = recent_width;
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
}

