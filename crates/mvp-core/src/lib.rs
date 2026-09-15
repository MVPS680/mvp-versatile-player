//! # mvp-core
//!
//! The media engine behind **MVP-Versatile-Player**: direct FFmpeg bindings for
//! demuxing, decoding, colour conversion and resampling, plus an image viewer
//! and a playlist model.
//!
//! ## Design notes
//!
//! * **No side-car process.** FFmpeg is linked directly, so there is no
//!   `ffmpeg.exe` to spawn and no pipe to prime before the first frame appears.
//! * **Nothing heavy happens at start-up.** [`Engine::new`] only initialises
//!   FFmpeg's tables; the audio device is opened and the decode threads are
//!   spawned on the first [`Engine::open`].
//! * **Bounded by construction.** Every queue is bounded by bytes as well as by
//!   count, so a 4K file cannot reserve gigabytes of decoded frames.
//! * **Panic-proof.** Each worker thread catches panics and turns them into an
//!   error state, because damaged files are a fact of life.

pub mod audio;
pub mod dsp;
pub mod engine;
pub mod error;
pub mod hdr;
pub mod image_view;
pub mod info;
pub mod playlist;
pub mod util;
pub mod video;

pub use engine::{
    Clock, Engine, EngineConfig, EngineEvent, EngineSnapshot, MediaSource, PlaybackState,
};
pub use error::{MediaError, Result};
pub use hdr::{DoviConfig, HdrInfo, HdrKind, ToneMapper};
pub use image_view::{FitMode, ImageDoc, ImageFrame, ImageView};
pub use info::{AudioStreamInfo, ChapterInfo, MediaInfo, SubtitleStreamInfo, VideoStreamInfo};
pub use playlist::{Playlist, PlaylistItem, RepeatMode};
pub use util::MediaKind;
pub use video::{FramePool, RgbaConverter, VideoFrame};

/// The subtitle parser, re-exported so callers need only depend on `mvp-core`.
pub use mvp_subtitle as subtitle;

use std::sync::OnceLock;

static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Initialise FFmpeg exactly once per process.
///
/// This registers the codecs, muxers and demuxers and is cheap (a few hundred
/// microseconds), but it is still deferred until something actually needs it so
/// that a start-up with a command-line error stays instant.
pub fn init() -> Result<()> {
    let result = INIT.get_or_init(|| {
        ffmpeg_next::init().map_err(|e| e.to_string())?;
        // FFmpeg is chatty by default; the player surfaces real problems through
        // its own error state, so only genuine errors reach the log.
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
        Ok(())
    });
    match result {
        Ok(()) => Ok(()),
        Err(message) => Err(MediaError::Other(format!("FFmpeg 初始化失败: {message}"))),
    }
}

/// Version banner for the about dialog, e.g. `libavcodec 63.1`.
pub fn ffmpeg_version() -> String {
    let _ = init();
    let (fmaj, fmin, fmicro) = split_version(ffmpeg_next::format::version());
    let (cmaj, cmin, cmicro) = split_version(ffmpeg_next::codec::version());
    let (umaj, umin, umicro) = split_version(ffmpeg_next::util::version());
    format!(
        "libavformat {fmaj}.{fmin}.{fmicro} · libavcodec {cmaj}.{cmin}.{cmicro} · libavutil {umaj}.{umin}.{umicro}"
    )
}

/// FFmpeg packs its version as `major << 16 | minor << 8 | micro`.
fn split_version(packed: u32) -> (u32, u32, u32) {
    (
        (packed >> 16) & 0xFF,
        (packed >> 8) & 0xFF,
        packed & 0xFF,
    )
}

/// Build configuration of the linked FFmpeg, for the about dialog.
pub fn ffmpeg_configuration() -> String {
    let _ = init();
    ffmpeg_next::format::configuration().to_string()
}

/// `true` when the linked FFmpeg was built under the GPL, which the about
/// dialog must disclose.
pub fn ffmpeg_is_gpl() -> bool {
    let _ = init();
    ffmpeg_next::format::license().contains("GPL")
}
