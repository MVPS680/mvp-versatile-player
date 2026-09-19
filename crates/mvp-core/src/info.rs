//! Media probing: everything the UI needs to describe a file, gathered
//! *without* decoding a single frame.
//!
//! Opening a container and reading its headers costs well under a millisecond
//! for local files, so [`probe`] is safe to call from the UI thread when a file
//! is dropped, keeping the "media info" panel instant.

use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ffmpeg_next as ffmpeg;
use ffmpeg::format::stream::Disposition;

use crate::error::{MediaError, Result};
use crate::hdr::HdrInfo;
use crate::util::{self, MediaKind};

/// One video stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoStreamInfo {
    /// Index inside the container.
    pub index: usize,
    /// Short codec name, e.g. `h264`.
    pub codec: String,
    /// Long codec name, e.g. `H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10`.
    pub codec_long: String,
    /// Coded width in pixels.
    pub width: u32,
    /// Coded height in pixels.
    pub height: u32,
    /// Nominal frame rate; `0.0` when unknown (common for raw streams).
    pub fps: f64,
    /// Declared bit rate in bits per second, `0` when unknown.
    pub bit_rate: u64,
    /// Pixel format name, e.g. `yuv420p`.
    pub pixel_format: String,
    /// Codec profile name, e.g. `High`.
    pub profile: String,
    /// Codec level, `0` when absent.
    pub level: i32,
    /// Number of frames when the container knows it, otherwise `0`.
    pub frames: i64,
    /// Display rotation in degrees taken from stream metadata (`rotate` tag).
    pub rotation: i32,
    /// Dynamic range: transfer function, primaries and the Dolby Vision
    /// configuration record when the container declares one.
    pub hdr: HdrInfo,
}

/// One audio stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioStreamInfo {
    /// Index inside the container.
    pub index: usize,
    /// Short codec name, e.g. `aac`.
    pub codec: String,
    /// Long codec name.
    pub codec_long: String,
    /// Channel count.
    pub channels: u16,
    /// Layout description, e.g. `stereo` or `5.1`.
    pub channel_layout: String,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Declared bit rate in bits per second, `0` when unknown.
    pub bit_rate: u64,
    /// ISO-639 language tag when present.
    pub language: Option<String>,
    /// Human readable track title when present.
    pub title: Option<String>,
    /// `true` when FFmpeg flags this as the container's default track.
    pub is_default: bool,
}

impl AudioStreamInfo {
    /// `English (eng)` / `Track 2`, used as a fallback menu label.
    pub fn display_name(&self) -> String {
        match (&self.title, &self.language) {
            (Some(t), Some(l)) => format!("{t} [{l}]"),
            (Some(t), None) => t.clone(),
            (None, Some(l)) => format!("{} [{l}]", crate::info::language_name(l)),
            (None, None) => format!("{} · {} ch", self.codec, self.channels),
        }
    }
}

/// One subtitle stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubtitleStreamInfo {
    /// Index inside the container.
    pub index: usize,
    /// Short codec name, e.g. `subrip`.
    pub codec: String,
    /// Long codec name.
    pub codec_long: String,
    /// ISO-639 language tag when present.
    pub language: Option<String>,
    /// Human readable track title when present.
    pub title: Option<String>,
    /// `true` for text subtitle codecs we can render as text.
    pub is_text: bool,
    /// `true` for graphical subtitle codecs (PGS/VobSub/DVB) we can render as a
    /// bitmap overlay.
    pub is_bitmap: bool,
    /// `true` when FFmpeg flags this as the container's default track.
    pub is_default: bool,
    /// `true` for tracks that are forced (only shown for foreign dialogue).
    pub is_forced: bool,
}

impl SubtitleStreamInfo {
    /// `true` when the player can actually draw this track.
    ///
    /// Text tracks are rendered as text; graphical ones are decoded to bitmaps
    /// and drawn as an overlay. Anything else (for example a teletext codec
    /// with no bitmap decoder) is listed but cannot be selected.
    pub fn is_renderable(&self) -> bool {
        self.is_text || self.is_bitmap
    }

    /// Menu label for this track.
    pub fn display_name(&self) -> String {
        match (&self.title, &self.language) {
            (Some(t), Some(l)) => format!("{t} [{l}]"),
            (Some(t), None) => t.clone(),
            (None, Some(l)) => crate::info::language_name(l).to_string(),
            (None, None) => format!("轨道 #{}", self.index),
        }
    }
}

/// A chapter marker.
#[derive(Debug, Clone, PartialEq)]
pub struct ChapterInfo {
    /// Chapter title, falling back to `Chapter N`.
    pub title: String,
    /// Start time in seconds.
    pub start: f64,
    /// End time in seconds.
    pub end: f64,
}

/// Complete description of one media file.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    /// Path or URL that was probed.
    pub path: PathBuf,
    /// Container short name, e.g. `matroska,webm`.
    pub format_name: String,
    /// Container long name, e.g. `Matroska / WebM`.
    pub format_long_name: String,
    /// Total duration in seconds; `0.0` when unknown (live streams).
    pub duration: f64,
    /// Overall bit rate in bits per second; `0` when unknown.
    pub bit_rate: u64,
    /// File size in bytes; `0` for network streams.
    pub size: u64,
    /// Presentation start offset in seconds.
    pub start_time: f64,
    /// Container level metadata (title, artist, encoder, ...).
    pub metadata: Vec<(String, String)>,
    /// Video streams, usually exactly one.
    pub video: Vec<VideoStreamInfo>,
    /// Audio streams.
    pub audio: Vec<AudioStreamInfo>,
    /// Subtitle streams.
    pub subtitles: Vec<SubtitleStreamInfo>,
    /// Chapter markers.
    pub chapters: Vec<ChapterInfo>,
    /// How the player should treat this file.
    pub kind: MediaKind,
    /// `true` when the file is a single still image (never advance time).
    pub is_still_image: bool,
}

impl MediaInfo {
    /// The first video stream, when there is one.
    pub fn primary_video(&self) -> Option<&VideoStreamInfo> {
        self.video.first()
    }

    /// The first audio stream, when there is one.
    pub fn primary_audio(&self) -> Option<&AudioStreamInfo> {
        self.audio.first()
    }

    /// A short one-line summary, e.g. `1920x1080 · 23.976 fps · h264`.
    pub fn short_summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(v) = self.primary_video() {
            parts.push(format!("{}x{}", v.width, v.height));
            if v.fps > 0.0 {
                parts.push(util::format_fps(v.fps));
            }
            parts.push(v.codec.clone());
        }
        if let Some(a) = self.primary_audio() {
            parts.push(format!("{} {} ch", a.codec, a.channels));
        }
        if parts.is_empty() {
            self.format_long_name.clone()
        } else {
            parts.join(" · ")
        }
    }

    /// Whether this file carries moving video.
    pub fn has_video(&self) -> bool {
        !self.video.is_empty() && !self.is_still_image
    }

    /// Whether this file carries any audio.
    pub fn has_audio(&self) -> bool {
        !self.audio.is_empty()
    }

    /// Whether this file has nothing but sound: audio, and nothing to show.
    ///
    /// A still image has no video stream either, but it does have a picture —
    /// which is why the test goes through [`MediaInfo::has_video`] rather than
    /// looking at the stream list, and why an image is never "audio only".
    pub fn is_audio_only(&self) -> bool {
        self.has_audio() && !self.has_video()
    }

    /// A container level metadata tag, looked up without regard to case.
    ///
    /// FFmpeg normalises the well-known names to lower case, but a third-party
    /// muxer is free to write `Artist`, and some write an empty string rather
    /// than leaving the tag out — so the lookup ignores case and treats blank
    /// as absent.
    pub fn tag(&self, name: &str) -> Option<&str> {
        tag_value(&self.metadata, name)
    }
}

/// The implementation of [`MediaInfo::tag`], free-standing so it can be tested
/// without building a whole [`MediaInfo`].
fn tag_value<'a>(tags: &'a [(String, String)], name: &str) -> Option<&'a str> {
    tags.iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

/// Best effort native name for an ISO-639-1/2 code, falling back to the code
/// itself so unusual tags still show up as *something* readable.
pub fn language_name(code: &str) -> String {
    let lowered = code.to_ascii_lowercase();
    let name = match lowered.as_str() {
        "zh" | "chi" | "zho" | "chs" | "cht" => "中文",
        "en" | "eng" => "English",
        "ja" | "jpn" => "日本語",
        "ko" | "kor" => "한국어",
        "fr" | "fra" | "fre" => "Français",
        "de" | "deu" | "ger" => "Deutsch",
        "es" | "spa" => "Español",
        "it" | "ita" => "Italiano",
        "pt" | "por" => "Português",
        "ru" | "rus" => "Русский",
        "ar" | "ara" => "العربية",
        "hi" | "hin" => "हिन्दी",
        "th" | "tha" => "ไทย",
        "vi" | "vie" => "Tiếng Việt",
        "und" => "未标注",
        other => other,
    };
    name.to_string()
}

/// Container format names produced by FFmpeg's image demuxers.
const IMAGE_FORMATS: &[&str] = &[
    "image2",
    "png_pipe",
    "jpeg_pipe",
    "jpegls_pipe",
    "bmp_pipe",
    "gif",
    "webp_pipe",
    "tiff_pipe",
    "ico",
    "qoi_pipe",
    "avif",
];

/// Probe `path` (a local file or a URL) and describe it.
///
/// This never decodes frames, so it stays fast even for very large files.
pub fn probe(path: &Path) -> Result<MediaInfo> {
    probe_with_kind(path, util::classify(path))
}

/// Like [`probe`], but with the [`MediaKind`] supplied by the caller (used when
/// the extension is unknown but the content has already been sniffed).
pub fn probe_with_kind(path: &Path, kind: MediaKind) -> Result<MediaInfo> {
    reject_disc_image(path)?;
    let ictx = ffmpeg::format::input(&path)?;
    Ok(describe(&ictx, path, kind))
}

/// Refuse a disc image with an explanation instead of handing it to FFmpeg.
///
/// An ISO/UDF image is a filesystem, not a container: the demuxers cannot make
/// sense of it, and what they do instead is emit decoder errors forever while no
/// picture ever arrives.
fn reject_disc_image(path: &Path) -> Result<()> {
    if util::is_disc_image_extension(path) || util::looks_like_disc_image(path) {
        return Err(MediaError::other(format!(
            "这是一个光盘镜像，不是媒体文件: {}。\n\
             请右键选择「装载」挂载后再打开里面的视频，或先解压出其中的 .m2ts / .mkv / .mp4。",
            path.display()
        )));
    }
    Ok(())
}

/// Describe an already-open container.
///
/// The engine uses this instead of [`probe`] so that opening a file touches the
/// disk exactly once; it needs no decoding and no seeking, so it is cheap enough
/// to run before the first frame is shown.
pub fn describe(ictx: &ffmpeg::format::context::Input, path: &Path, kind: MediaKind) -> MediaInfo {
    let format_name = ictx.format().name().to_string();
    let format_long_name = ictx.format().description().to_string();

    let duration = {
        let raw = ictx.duration();
        if raw > 0 {
            raw as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
        } else {
            0.0
        }
    };
    let start_time = {
        let raw = unsafe { (*ictx.as_ptr()).start_time };
        if raw > 0 {
            raw as f64 / f64::from(ffmpeg::ffi::AV_TIME_BASE)
        } else {
            0.0
        }
    };

    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut subtitles = Vec::new();

    for stream in ictx.streams() {
        let params = stream.parameters();
        // SAFETY: `params` borrows the live `AVCodecParameters` owned by the
        // stream, which outlives this block; we only read plain scalars.
        let raw = unsafe { &*params.as_ptr() };
        let codec_id = stream.parameters().id();
        let codec = codec_name(codec_id);
        let codec_long = codec_long_name(codec_id);
        let bit_rate = params.bit_rate().max(0) as u64;
        let meta = stream.metadata();
        let language = meta.get("language").map(|s| s.to_ascii_lowercase());
        let title = meta.get("title").map(str::to_string);

        match params.medium() {
            ffmpeg::media::Type::Video => {
                let fps = rational_to_f64(stream.avg_frame_rate());
                let fps = if fps > 0.0 {
                    fps
                } else {
                    rational_to_f64(stream.rate())
                };
                let rotation = meta
                    .get("rotate")
                    .and_then(|r| r.trim().parse::<i32>().ok())
                    .unwrap_or(0);
                video.push(VideoStreamInfo {
                    index: stream.index(),
                    codec,
                    codec_long,
                    width: raw.width.max(0) as u32,
                    height: raw.height.max(0) as u32,
                    fps,
                    bit_rate,
                    pixel_format: pixel_format_name(raw.format),
                    profile: video_profile_name(codec_id, raw.profile),
                    level: raw.level,
                    frames: stream.frames(),
                    rotation,
                    // SAFETY: `params` belongs to the live stream and the call
                    // only reads plain fields and FFmpeg-owned side data.
                    hdr: unsafe { HdrInfo::from_codec_parameters(params.as_ptr()) },
                });
            }
            ffmpeg::media::Type::Audio => {
                audio.push(AudioStreamInfo {
                    index: stream.index(),
                    codec,
                    codec_long,
                    channels: raw.ch_layout.nb_channels.max(0) as u16,
                    channel_layout: channel_layout_name(&raw.ch_layout),
                    sample_rate: raw.sample_rate.max(0) as u32,
                    bit_rate,
                    language,
                    title,
                    is_default: stream.disposition().contains(Disposition::DEFAULT),
                });
            }
            ffmpeg::media::Type::Subtitle => {
                subtitles.push(SubtitleStreamInfo {
                    index: stream.index(),
                    codec,
                    codec_long,
                    language,
                    title,
                    is_text: is_text_subtitle_codec(codec_id),
                    is_bitmap: is_bitmap_subtitle_codec(codec_id),
                    is_default: stream.disposition().contains(Disposition::DEFAULT),
                    is_forced: stream.disposition().contains(Disposition::FORCED),
                });
            }
            _ => {}
        }
    }

    let mut chapters = Vec::new();
    for (i, chapter) in ictx.chapters().enumerate() {
        let tb = chapter.time_base();
        let start = chapter.start() as f64 * f64::from(tb);
        let end = chapter.end() as f64 * f64::from(tb);
        let title = chapter
            .metadata()
            .get("title")
            .map(str::to_string)
            .unwrap_or_else(|| format!("章节 {}", i + 1));
        chapters.push(ChapterInfo {
            title,
            start: if start.is_finite() { start } else { 0.0 },
            end: if end.is_finite() { end } else { 0.0 },
        });
    }

    let mut metadata: Vec<(String, String)> = ictx
        .metadata()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    metadata.sort_by(|a, b| a.0.cmp(&b.0));

    // A still image is either classed as one by extension, or arrives in one of
    // FFmpeg's image demuxers with exactly one video frame and no audio.
    let is_still_image = kind.is_image()
        || (IMAGE_FORMATS.contains(&format_name.as_str())
            && audio.is_empty()
            && video.len() == 1
            && video[0].fps <= 1.0);

    MediaInfo {
        path: path.to_path_buf(),
        format_name,
        format_long_name,
        duration,
        bit_rate: ictx.bit_rate().max(0) as u64,
        size,
        start_time,
        metadata,
        video,
        audio,
        subtitles,
        chapters,
        kind,
        is_still_image,
    }
}

/// Short FFmpeg codec name for an ID, e.g. `h264`.
pub fn codec_name(id: ffmpeg::codec::Id) -> String {
    // SAFETY: `avcodec_get_name` always returns a valid static C string.
    unsafe {
        let ptr = ffmpeg::ffi::avcodec_get_name(id.into());
        if ptr.is_null() {
            return "unknown".to_string();
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

/// Long FFmpeg codec description for an ID, falling back to the short name.
pub fn codec_long_name(id: ffmpeg::codec::Id) -> String {
    ffmpeg::codec::decoder::find(id)
        .map(|c| c.description().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| codec_name(id))
}

/// `true` when FFmpeg can give us subtitle *text* for this codec (as opposed to
/// bitmap subtitles such as DVD/PGS which would need OCR).
///
/// The answer comes straight from FFmpeg's codec descriptor table, so new text
/// subtitle codecs are picked up automatically.
pub fn is_text_subtitle_codec(id: ffmpeg::codec::Id) -> bool {
    // SAFETY: `avcodec_descriptor_get` returns a pointer into a static table
    // (or NULL) and never retains the argument.
    unsafe {
        let desc = ffmpeg::ffi::avcodec_descriptor_get(id.into());
        if desc.is_null() {
            return false;
        }
        ((*desc).props & ffmpeg::ffi::AV_CODEC_PROP_TEXT_SUB) != 0
    }
}

/// `true` when FFmpeg decodes this codec into a bitmap we can draw directly.
///
/// PGS, VobSub, DVB and XSUB live here. No OCR is involved: the decoded
/// rectangles are painted over the picture as they are.
pub fn is_bitmap_subtitle_codec(id: ffmpeg::codec::Id) -> bool {
    // SAFETY: `avcodec_descriptor_get` returns a pointer into a static table
    // (or NULL) and never retains the argument.
    unsafe {
        let desc = ffmpeg::ffi::avcodec_descriptor_get(id.into());
        if desc.is_null() {
            return false;
        }
        ((*desc).props & ffmpeg::ffi::AV_CODEC_PROP_BITMAP_SUB) != 0
    }
}

fn rational_to_f64(r: ffmpeg::Rational) -> f64 {
    let num = r.numerator();
    let den = r.denominator();
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

fn pixel_format_name(format: i32) -> String {
    // SAFETY: `AVPixelFormat` is a fieldless C enum with `int` representation, so
    // any `i32` is a valid bit pattern for it, and `av_get_pix_fmt_name` answers
    // NULL for values it does not know.
    let pixel_format: ffmpeg::ffi::AVPixelFormat = unsafe { std::mem::transmute(format) };
    unsafe {
        let ptr = ffmpeg::ffi::av_get_pix_fmt_name(pixel_format);
        if ptr.is_null() {
            return format!("unknown({format})");
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

fn channel_layout_name(layout: &ffmpeg::ffi::AVChannelLayout) -> String {
    let mut buf = [0i8; 128];
    // SAFETY: `buf` is a valid 128 byte writable buffer and `layout` is a live
    // channel layout; the call is documented to be safe with any layout.
    let written = unsafe {
        ffmpeg::ffi::av_channel_layout_describe(layout, buf.as_mut_ptr(), buf.len())
    };
    if written < 0 {
        return format!("{} ch", layout.nb_channels);
    }
    // SAFETY: on success FFmpeg has written a NUL terminated string into `buf`.
    let described = unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    if described.is_empty() {
        format!("{} ch", layout.nb_channels)
    } else {
        described
    }
}

fn video_profile_name(codec_id: ffmpeg::codec::Id, profile: i32) -> String {
    if profile < 0 {
        return String::new();
    }
    // SAFETY: `avcodec_profile_name` returns a static string or NULL.
    unsafe {
        let ptr = ffmpeg::ffi::avcodec_profile_name(codec_id.into(), profile);
        if ptr.is_null() {
            profile.to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}

/// Guess a sensible default track index for a stream list.
pub fn default_index<T, F>(items: &[T], is_default: F) -> Option<usize>
where
    F: Fn(&T) -> bool,
{
    items
        .iter()
        .position(is_default)
        .or(if items.is_empty() { None } else { Some(0) })
}

/// Format a probe result as a list of `(label, value)` rows for the info panel.
pub fn info_rows(info: &MediaInfo) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    rows.push(("容器格式".into(), info.format_long_name.clone()));
    if info.size > 0 {
        rows.push(("文件大小".into(), util::format_size(info.size)));
    }
    if info.duration > 0.0 {
        rows.push((
            "时长".into(),
            format!(
                "{} ({:.3} 秒)",
                util::format_duration(info.duration),
                info.duration
            ),
        ));
    }
    if info.bit_rate > 0 {
        rows.push(("总比特率".into(), util::format_bitrate(info.bit_rate)));
    }
    if info.start_time > 0.0 {
        rows.push(("起始时间".into(), format!("{:.3} 秒", info.start_time)));
    }
    for (i, v) in info.video.iter().enumerate() {
        let prefix = if info.video.len() > 1 {
            format!("视频 #{i} ")
        } else {
            "视频 ".to_string()
        };
        rows.push((
            format!("{prefix}编码"),
            format!("{} ({})", v.codec_long, v.codec),
        ));
        rows.push((
            format!("{prefix}分辨率"),
            format!(
                "{}x{} ({})",
                v.width,
                v.height,
                util::aspect_label(v.width, v.height)
            ),
        ));
        if v.fps > 0.0 {
            rows.push((format!("{prefix}帧率"), util::format_fps(v.fps)));
        }
        if v.bit_rate > 0 {
            rows.push((format!("{prefix}比特率"), util::format_bitrate(v.bit_rate)));
        }
        rows.push((format!("{prefix}像素格式"), v.pixel_format.clone()));
        if v.hdr.kind.is_hdr() || v.hdr.dovi.is_some() {
            rows.push((format!("{prefix}动态范围"), v.hdr.label()));
        }
        if v.hdr.needs_dolby_renderer() {
            rows.push((
                format!("{prefix}提示"),
                "杜比视界 Profile 5 使用 IPT 编码的基底层，本播放器无法还原杜比视界的映射，\
                 颜色可能不正确"
                    .to_string(),
            ));
        }
        if v.frames > 0 {
            rows.push((format!("{prefix}总帧数"), v.frames.to_string()));
        }
        if v.rotation != 0 {
            rows.push((format!("{prefix}旋转"), format!("{}°", v.rotation)));
        }
    }
    for (i, a) in info.audio.iter().enumerate() {
        let prefix = if info.audio.len() > 1 {
            format!("音频 #{i} ")
        } else {
            "音频 ".to_string()
        };
        rows.push((
            format!("{prefix}编码"),
            format!("{} ({})", a.codec_long, a.codec),
        ));
        rows.push((
            format!("{prefix}声道"),
            format!("{} ({})", a.channels, a.channel_layout),
        ));
        rows.push((format!("{prefix}采样率"), format!("{} Hz", a.sample_rate)));
        if a.bit_rate > 0 {
            rows.push((format!("{prefix}比特率"), util::format_bitrate(a.bit_rate)));
        }
    }
    for (i, s) in info.subtitles.iter().enumerate() {
        rows.push((
            format!("字幕 #{}", i + 1),
            format!("{} ({})", s.codec_long, s.display_name()),
        ));
    }
    for (k, v) in &info.metadata {
        rows.push((format!("元数据 · {k}"), v.clone()));
    }
    rows
}

/// Human readable elapsed timer from a `Duration` (used by the OSD).
pub fn format_elapsed(d: Duration) -> String {
    format!("{:.1}s", d.as_secs_f64())
}

/// Reject a path that cannot possibly be opened, producing a clear message
/// instead of an opaque FFmpeg error.
pub fn validate_input(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(MediaError::other("路径为空"));
    }
    if util::is_url(&path.to_string_lossy()) {
        return Ok(());
    }
    if !path.exists() {
        return Err(MediaError::other(format!("文件不存在: {}", path.display())));
    }
    if path.is_dir() {
        return Err(MediaError::other(format!(
            "这是一个文件夹: {}",
            path.display()
        )));
    }
    reject_disc_image(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_names_cover_the_common_cases() {
        assert_eq!(language_name("eng"), "English");
        assert_eq!(language_name("ZH"), "中文");
        assert_eq!(language_name("und"), "未标注");
        assert_eq!(language_name("xyz"), "xyz");
    }

    #[test]
    fn validate_input_reports_missing_files() {
        let err = validate_input(Path::new("C:\\definitely\\not\\here.mkv")).unwrap_err();
        assert!(err.to_string().contains("文件不存在"));
        assert!(validate_input(Path::new("https://example.com/a.m3u8")).is_ok());
    }

    #[test]
    fn default_index_prefers_flagged_items() {
        let items = [false, false, true];
        assert_eq!(default_index(&items, |v| *v), Some(2));
        let none: [bool; 0] = [];
        assert_eq!(default_index(&none, |v| *v), None);
        let plain = [false, false];
        assert_eq!(default_index(&plain, |v| *v), Some(0));
    }

    #[test]
    fn tags_are_found_whatever_their_case() {
        let tags = vec![
            ("ARTIST".to_string(), " 周杰伦 ".to_string()),
            ("album".to_string(), "十一月的萧邦".to_string()),
            ("comment".to_string(), "   ".to_string()),
        ];
        assert_eq!(tag_value(&tags, "artist"), Some("周杰伦"));
        assert_eq!(tag_value(&tags, "Album"), Some("十一月的萧邦"));
        // A tag that is there but empty means "not set", not "".
        assert_eq!(tag_value(&tags, "COMMENT"), None);
        assert_eq!(tag_value(&tags, "title"), None);
    }
}
