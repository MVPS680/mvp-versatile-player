//! The audio screen.
//!
//! A file with no picture is not a video with a missing frame. The screen is
//! built around what an album actually offers: a cover-like record, the title
//! and the technical facts, a queue on the right, the lyrics under the record
//! when the file carries a subtitle track, and a transport with a large volume
//! control and a device picker rather than aspect and rotation controls that
//! could only ever do nothing here.

use egui::Context;

use crate::app::PlayerApp;
use crate::ui::{canvas, sidebar, transport};

pub(super) fn draw(app: &mut PlayerApp, ctx: &Context) {
    if app.ui.sidebar_visible {
        sidebar::draw(app, ctx);
    }
    if !app.ui.fullscreen {
        transport::draw_audio(app, ctx);
    }
    canvas::draw_audio(app, ctx);
}
