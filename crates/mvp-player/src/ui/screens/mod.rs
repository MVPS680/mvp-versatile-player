//! One screen per kind of rich media.
//!
//! The player shows three very different things — a film, a piece of music and
//! a photograph — and they do not deserve the same window. A film wants the
//! largest possible picture with a timeline under it; an album wants a cover, a
//! queue and lyrics; a photograph wants the frame, a filmstrip and its own
//! parameters. Before this module every one of them was the same canvas with a
//! flag inside it, which is why they all read as the same screen.
//!
//! Each submodule owns the panel order of its screen. The order is a contract
//! (see the top of `ui/mod.rs`): the sidebar claims its width first, the bottom
//! strip claims its height next, and the canvas is whatever is left. A screen
//! that drew its canvas first would centre the picture on a rectangle that the
//! bar then covers.

use egui::Context;

use crate::app::PlayerApp;
use crate::state::Mode;

use super::{canvas, sidebar, transport};

mod audio;
mod image;
mod video;

/// Paint the whole content of the current screen.
pub(super) fn draw(app: &mut PlayerApp, ctx: &Context) {
    match app.mode {
        // Nothing open yet: the welcome column, with the playlist beside it.
        Mode::Empty => {
            sidebar::draw(app, ctx);
            canvas::draw_empty(app, ctx);
        }
        Mode::Video => video::draw(app, ctx),
        Mode::Audio => audio::draw(app, ctx),
        Mode::Image => image::draw(app, ctx),
    }
}

/// Paint the picture area alone: the first, cheap frame.
///
/// The window `eframe` creates stays hidden until a frame is painted, so a
/// cheaper first frame is a window that appears sooner. This deliberately skips
/// the text-heavy chrome — the menu, the sidebar and the transport — which the
/// caller requests on the very next frame. The picture is what a media player
/// should lead with, and at this point in start-up it is the only thing there is
/// to show.
pub(super) fn draw_canvas_only(app: &mut PlayerApp, ctx: &Context) {
    match app.mode {
        Mode::Video => canvas::draw_video(app, ctx),
        Mode::Audio => canvas::draw_audio(app, ctx),
        Mode::Image => canvas::draw_image(app, ctx),
        // Nothing open yet: there is no picture to lead with, so the window
        // background alone, which is also the cheapest possible first frame.
        Mode::Empty => {}
    }
}

/// The floating transport, drawn over the picture in fullscreen.
///
/// Images have no timeline, so fullscreen is the picture and nothing else —
/// which is exactly what fullscreen is for.
pub(super) fn draw_overlay(app: &mut PlayerApp, ctx: &Context) {
    match app.mode {
        Mode::Video => transport::draw_video_overlay(app, ctx),
        Mode::Audio => transport::draw_audio_overlay(app, ctx),
        _ => {}
    }
}
