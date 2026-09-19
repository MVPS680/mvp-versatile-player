//! OpenGL resources the player owns on top of what egui draws for it.
//!
//! One module so far: the picture adjustment pass. It exists as a module rather than
//! as part of `ui` because it is the only place that talks to the driver directly,
//! and keeping it in one place is what makes "the interface never draws the video"
//! checkable by reading a directory listing.

pub mod picture_pass;
