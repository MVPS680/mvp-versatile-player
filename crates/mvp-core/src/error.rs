//! Error types for the media engine.

/// Result alias used throughout `mvp-core`.
pub type Result<T> = std::result::Result<T, MediaError>;

/// Everything that can go wrong while opening, decoding or rendering media.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// An error reported by the underlying FFmpeg libraries.
    #[error("ffmpeg error: {0}")]
    Ffmpeg(#[from] ffmpeg_next::Error),

    /// Filesystem / stream I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// The container or codec is understood but not supported by this build.
    #[error("unsupported media: {0}")]
    Unsupported(String),

    /// The file has no stream of the kind that was requested.
    #[error("missing stream: {0}")]
    MissingStream(String),

    /// No usable audio output device, or the device refused the format.
    #[error("audio device error: {0}")]
    AudioDevice(String),

    /// Image decoding failure (the `image` crate).
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),

    /// The operation was cancelled because playback was stopped or re-opened.
    #[error("operation cancelled")]
    Cancelled,

    /// Catch-all with a human readable message.
    #[error("{0}")]
    Other(String),
}

impl MediaError {
    /// Convenience constructor for [`MediaError::Other`].
    pub fn other(msg: impl Into<String>) -> Self {
        MediaError::Other(msg.into())
    }

    /// Convenience constructor for [`MediaError::Unsupported`].
    pub fn unsupported(msg: impl Into<String>) -> Self {
        MediaError::Unsupported(msg.into())
    }
}
