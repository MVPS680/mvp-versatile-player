//! Small helpers shared by the rest of the engine: media classification and
//! human readable formatting.

use std::path::Path;

/// What kind of thing a path refers to, decided purely from its extension.
///
/// The player branches on this *before* handing anything to FFmpeg so that
/// still images (which need sub-millisecond start-up and interactive zoom) and
/// playlists take a completely different, much cheaper code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// Motion video, possibly with audio tracks.
    Video,
    /// Audio-only container or raw audio stream.
    Audio,
    /// A still or animated image.
    Image,
    /// A playlist that expands into several entries.
    Playlist,
    /// A subtitle side-car file.
    Subtitle,
    /// Unrecognised; let FFmpeg have a go at it.
    Unknown,
}

impl MediaKind {
    /// `true` for kinds the video pipeline can open.
    pub fn is_playable(self) -> bool {
        matches!(self, MediaKind::Video | MediaKind::Audio | MediaKind::Unknown)
    }

    /// `true` for kinds handled by the image viewer.
    pub fn is_image(self) -> bool {
        matches!(self, MediaKind::Image)
    }
}

/// Extensions handled by the still-image viewer, in the order they are shown
/// in the UI. Kept in sync with `mvp-platform`'s registry list.
pub const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "png", "bmp", "gif", "webp", "tif", "tiff", "ico", "tga", "dds",
    "ppm", "pgm", "pbm", "pnm", "qoi", "avif", "heic", "heif", "jxl", "exr", "hdr",
];

/// Extensions treated as playlists.
pub const PLAYLIST_EXTENSIONS: &[&str] = &["m3u", "m3u8", "pls", "xspf", "wpl"];

/// Extensions treated as subtitle side-cars.
///
/// `.sup` and `.idx` are graphical: raw PGS and a VobSub index. A binary `.sub`
/// is graphical too, but the extension is shared with MicroDVD text, so it is
/// decided by content rather than by name.
pub const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "vtt", "sub", "idx", "smi", "sup"];

/// Extensions of optical-disc images.
///
/// These are *filesystems*, not media containers: an ISO or a BDMV image has to
/// be mounted (or its contents extracted) before FFmpeg has anything to read.
/// Handing one to the demuxers anyway is what produces an endless storm of
/// decoder errors instead of a picture.
pub const DISC_IMAGE_EXTENSIONS: &[&str] = &["iso", "img", "udf", "nrg", "mdf", "mds", "ccd"];

/// `true` when the path looks like a disc image, by extension.
pub fn is_disc_image_extension(path: &Path) -> bool {
    applies_to(path, DISC_IMAGE_EXTENSIONS)
}

/// `true` when the first volume descriptors say "ISO9660 / UDF image".
///
/// A disc image carries the string `CD001` (ISO9660) or `BEA01` / `NSR02` /
/// `NSR03` (UDF) at byte offset 0x8001. Checking the content as well as the
/// extension catches images that were renamed on the way here.
pub fn looks_like_disc_image(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    const VOLUME_DESCRIPTOR_OFFSET: u64 = 0x8001;
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    if file.seek(SeekFrom::Start(VOLUME_DESCRIPTOR_OFFSET)).is_err() {
        return false;
    }
    let mut magic = [0u8; 5];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    matches!(&magic, b"CD001" | b"BEA01" | b"NSR02" | b"NSR03")
}

/// `true` when `path`'s extension is one of `list`.
fn applies_to(path: &Path, list: &[&str]) -> bool {
    match extension_of(path) {
        Some(ext) => list.contains(&ext.as_str()),
        None => false,
    }
}

/// Audio-only extensions, used to pick a nicer default window shape and to
/// decide whether a "video" area is expected at all.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "aac", "m4a", "ogg", "oga", "opus", "wav", "wma", "ac3", "eac3", "dts", "ape",
    "alac", "amr", "mka", "mp2", "mpc", "wv", "tta", "aiff", "aif", "au", "mid", "midi", "m4b",
    "spx", "caf",
];

/// Lower-cased extension of `path`, without the leading dot.
pub fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

/// Classify `path` by extension. Never touches the filesystem.
pub fn classify(path: &Path) -> MediaKind {
    let Some(ext) = extension_of(path) else {
        return MediaKind::Unknown;
    };
    classify_extension(&ext)
}

/// Classify a bare extension (with or without a leading dot).
pub fn classify_extension(ext: &str) -> MediaKind {
    let ext = ext.trim_start_matches('.').to_ascii_lowercase();
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        MediaKind::Image
    } else if PLAYLIST_EXTENSIONS.contains(&ext.as_str()) {
        MediaKind::Playlist
    } else if SUBTITLE_EXTENSIONS.contains(&ext.as_str()) {
        MediaKind::Subtitle
    } else if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        MediaKind::Audio
    } else {
        MediaKind::Video
    }
}

/// `true` when `s` looks like a network URL rather than a local path.
pub fn is_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    // Windows drive letters (`c:\`) must not be mistaken for a scheme.
    const SCHEMES: &[&str] = &[
        "http://", "https://", "rtsp://", "rtmp://", "rtmps://", "mms://", "mmsh://", "ftp://",
        "sftp://", "udp://", "tcp://", "rtp://", "srt://", "rist://", "file://", "gopher://",
        "hls://", "dash://", "concat:", "crypto:",
    ];
    SCHEMES.iter().any(|s| lower.starts_with(s))
}

/// Format a duration as `H:MM:SS` (or `MM:SS` under an hour), matching what
/// every mainstream player shows on its seek bar.
pub fn format_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".to_string();
    }
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Same as [`format_duration`] but always `HH:MM:SS`, used by the subtitle
/// track listing where a stable column width matters.
pub fn format_duration_long(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--:--".to_string();
    }
    let total = secs.round() as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// Byte count as `1.23 GB` / `456 MB` / `12.3 KB`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Bit rate as `5.2 Mb/s` or `320 kb/s`.
pub fn format_bitrate(bits_per_sec: u64) -> String {
    if bits_per_sec == 0 {
        return "—".to_string();
    }
    let kbit = bits_per_sec as f64 / 1000.0;
    if kbit >= 1000.0 {
        format!("{:.2} Mb/s", kbit / 1000.0)
    } else {
        format!("{kbit:.0} kb/s")
    }
}

/// Frame rate as `23.976 fps` / `25 fps`, trimming pointless decimals.
pub fn format_fps(fps: f64) -> String {
    if !fps.is_finite() || fps <= 0.0 {
        return "—".to_string();
    }
    if (fps - fps.round()).abs() < 0.001 {
        format!("{} fps", fps.round() as i64)
    } else {
        format!("{fps:.3} fps")
    }
}

/// Simplify `widthxheight` into the closest well known aspect ratio label.
pub fn aspect_label(width: u32, height: u32) -> String {
    if width == 0 || height == 0 {
        return "—".to_string();
    }
    let ratio = width as f64 / height as f64;
    const KNOWN: &[(f64, &str)] = &[
        (1.0, "1:1"),
        (4.0 / 3.0, "4:3"),
        (3.0 / 2.0, "3:2"),
        (16.0 / 10.0, "16:10"),
        (5.0 / 3.0, "5:3"),
        (16.0 / 9.0, "16:9"),
        (1.85, "1.85:1"),
        (2.0, "2:1"),
        (2.2, "2.2:1"),
        (2.35, "2.35:1"),
        (2.39, "2.39:1"),
        (9.0 / 16.0, "9:16"),
    ];
    for (value, label) in KNOWN {
        if (ratio - value).abs() < 0.02 {
            return (*label).to_string();
        }
    }
    // Reduce the raw ratio with a continued-fraction style approximation.
    let (mut a, mut b) = (width, height);
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    let (w, h) = (width / a, height / a);
    if w > 40 || h > 40 {
        format!("{ratio:.3}")
    } else {
        format!("{w}:{h}")
    }
}

/// Largest `width x height` box that fits `src` inside `bounds` while keeping
/// the aspect ratio. Never upscales past `src`.
pub fn fit_inside(src: (u32, u32), bounds: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = (src.0.max(1), src.1.max(1));
    let (bw, bh) = (bounds.0.max(1), bounds.1.max(1));
    let scale = f64::min(bw as f64 / sw as f64, bh as f64 / sh as f64);
    let scale = scale.min(1.0);
    (
        ((sw as f64 * scale).round() as u32).max(1),
        ((sh as f64 * scale).round() as u32).max(1),
    )
}

/// Even-sized dimensions, required by most chroma-subsampled pixel formats.
pub fn even((w, h): (u32, u32)) -> (u32, u32) {
    ((w.max(2) & !1), (h.max(2) & !1))
}

/// An exponentially weighted average of a millisecond measurement, in an
/// atomic.
///
/// The video worker measures the stages of a frame and the interface displays
/// the result, so the number has to travel between two functions that own
/// neither of the other's state. Exactly one thread writes a slot and the value
/// is only ever read to be shown, which is why relaxed loads are enough.
#[derive(Debug, Default)]
pub struct MsEwma(std::sync::atomic::AtomicU32);

impl MsEwma {
    /// An average with no samples yet.
    pub const fn new() -> Self {
        Self(std::sync::atomic::AtomicU32::new(0))
    }

    /// Fold one measurement in.
    pub fn record(&self, ms: f32) {
        use std::sync::atomic::Ordering;
        let previous = f32::from_bits(self.0.load(Ordering::Relaxed));
        let next = if previous <= 0.0 {
            ms
        } else {
            previous * 0.9 + ms * 0.1
        };
        self.0.store(next.to_bits(), Ordering::Relaxed);
    }

    /// The current average in milliseconds (`0.0` before the first sample).
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(std::sync::atomic::Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_ewma_starts_empty_then_smooths_towards_the_samples() {
        let average = MsEwma::new();
        assert_eq!(average.get(), 0.0, "no samples means no reading");
        average.record(10.0);
        assert!(
            (average.get() - 10.0).abs() < 1e-4,
            "the first sample is the reading, got {}",
            average.get()
        );
        // A second, much smaller sample must move the average *towards* it, not
        // replace it: one fast frame in a stream of slow ones is noise.
        average.record(0.0);
        let after = average.get();
        assert!((after - 9.0).abs() < 1e-3, "got {after}");
        // Repeated small samples converge.
        for _ in 0..200 {
            average.record(0.0);
        }
        assert!(average.get() < 0.5, "got {}", average.get());
    }
    use std::path::PathBuf;

    #[test]
    fn classifies_by_extension() {
        assert_eq!(classify(&PathBuf::from("a/b/c.MP4")), MediaKind::Video);
        assert_eq!(classify(&PathBuf::from("c:\\x\\y.PNG")), MediaKind::Image);
        assert_eq!(classify(&PathBuf::from("song.flac")), MediaKind::Audio);
        assert_eq!(classify(&PathBuf::from("list.m3u")), MediaKind::Playlist);
        // `.m3u8` is ambiguous: it is the M3U *and* the HLS playlist extension.
        // It classifies as a playlist here; `playlist::load` sniffs the content
        // and hands real HLS manifests to FFmpeg as a single media entry.
        assert_eq!(classify(&PathBuf::from("list.m3u8")), MediaKind::Playlist);
        assert_eq!(classify(&PathBuf::from("sub.srt")), MediaKind::Subtitle);
        assert_eq!(classify(&PathBuf::from("mystery.bin")), MediaKind::Video);
        assert_eq!(classify(&PathBuf::from("noextension")), MediaKind::Unknown);
    }

    #[test]
    fn urls_are_not_drive_letters() {
        assert!(is_url("https://example.com/a.m3u8"));
        assert!(is_url("rtsp://10.0.0.1/live"));
        assert!(!is_url("C:\\Movies\\a.mkv"));
        assert!(!is_url("\\\\server\\share\\a.mkv"));
    }

    #[test]
    fn durations_are_human_readable() {
        assert_eq!(format_duration(0.0), "0:00");
        assert_eq!(format_duration(59.6), "1:00");
        assert_eq!(format_duration(3599.0), "59:59");
        assert_eq!(format_duration(3600.0), "1:00:00");
        assert_eq!(format_duration(3661.4), "1:01:01");
        assert_eq!(format_duration(f64::NAN), "--:--");
    }

    #[test]
    fn sizes_and_bitrates() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1024 * 1024 * 3 / 2), "1.50 MB");
        assert_eq!(format_bitrate(0), "—");
        assert_eq!(format_bitrate(320_000), "320 kb/s");
        assert_eq!(format_bitrate(5_200_000), "5.20 Mb/s");
    }

    #[test]
    fn aspect_labels_cover_common_shapes() {
        assert_eq!(aspect_label(1920, 1080), "16:9");
        assert_eq!(aspect_label(640, 480), "4:3");
        assert_eq!(aspect_label(1080, 1920), "9:16");
        assert_eq!(aspect_label(0, 0), "—");
    }

    #[test]
    fn fitting_never_upscales() {
        assert_eq!(fit_inside((1920, 1080), (1280, 720)), (1280, 720));
        assert_eq!(fit_inside((640, 480), (1280, 720)), (640, 480));
        // 1080x1920 portrait inside a 1280x720 landscape box.
        assert_eq!(fit_inside((1080, 1920), (1280, 720)), (405, 720));
    }

    #[test]
    fn even_dims_are_even() {
        assert_eq!(even((1921, 1081)), (1920, 1080));
        assert_eq!(even((0, 0)), (2, 2));
    }
}
