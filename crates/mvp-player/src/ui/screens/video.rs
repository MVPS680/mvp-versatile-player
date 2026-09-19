//! The video screen.
//!
//! A film is the one case where the picture is the whole point, so the screen is
//! deliberately sparse: the playlist and track pages on the right, a timeline
//! with chapter marks and a transport under it, and the largest possible
//! letterboxed frame in between. Everything the video path can *do* to the
//! picture — aspect, rotation, mirroring, A–B loop, track choice — is on the
//! transport bar, because hunting through menus while watching is the wrong
//! interaction.

use egui::Context;

use crate::app::PlayerApp;
use crate::ui::{canvas, sidebar, transport};

pub(super) fn draw(app: &mut PlayerApp, ctx: &Context) {
    if app.ui.sidebar_visible {
        sidebar::draw(app, ctx);
    }
    if !app.ui.fullscreen {
        transport::draw_video(app, ctx);
    }
    canvas::draw_video(app, ctx);
}
