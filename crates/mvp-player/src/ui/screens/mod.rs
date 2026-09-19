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
