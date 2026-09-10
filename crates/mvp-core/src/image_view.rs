//! Still-image and animation viewing: decoding, orientation, and the
//! interactive view state (zoom, pan, rotate, slideshow).
//!
//! Images deliberately do **not** go through the video pipeline. A JPEG has no
//! clock, no audio and no stream to demux, and forcing it through the decoder
//! graph would cost tens of milliseconds of start-up for no benefit. Instead the
//! `image` crate decodes straight to RGBA and the viewer state below handles
//! zooming, panning and multi-frame animation.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{MediaError, Result};

/// Largest dimension a texture may have. Anything bigger is downscaled during
/// load, because GPUs reject textures beyond this and a 30000 px photo would
/// otherwise be 3.6 GB in memory.
pub const MAX_DIMENSION: u32 = 8192;

/// Hard limit on decoded pixels, applied before allocation.
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// Maximum number of animation frames we are willing to hold.
pub const MAX_ANIMATION_FRAMES: usize = 400;

/// Maximum total bytes of animation frames.
pub const MAX_ANIMATION_BYTES: usize = 512 * 1024 * 1024;

/// One frame of a still or animated image: tightly packed RGBA8.
#[derive(Debug, Clone)]
pub struct ImageFrame {
    /// Pixels, `width * height * 4` bytes.
    pub data: Vec<u8>,
    /// How long this frame is shown, in milliseconds.
    pub delay_ms: u32,
}

impl ImageFrame {
    /// Bytes occupied by the pixel data.
    pub fn byte_len(&self) -> usize {
        self.data.len()
    }
}

/// A decoded image file.
#[derive(Debug, Clone)]
pub struct ImageDoc {
    /// Where it came from.
    pub path: PathBuf,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Every frame; one entry for still images.
    pub frames: Vec<ImageFrame>,
    /// `true` when there is more than one frame.
    pub is_animated: bool,
    /// Container/format name, e.g. `PNG`.
    pub format: String,
    /// Bit depth and colour model as reported by the decoder.
    pub color_type: String,
    /// File size in bytes.
    pub file_size: u64,
    /// Orientation that was applied from EXIF, in degrees (`0`, `90`, `180`, `270`).
    pub exif_orientation: u16,
    /// `true` when the image had to be downscaled to fit [`MAX_DIMENSION`].
    pub downscaled: bool,
}

impl ImageDoc {
    /// The frame that should be visible given an animation clock.
    pub fn frame_at(&self, elapsed_ms: u32) -> &ImageFrame {
        if !self.is_animated || self.frames.len() <= 1 {
            return &self.frames[0];
        }
        let total: u32 = self.frames.iter().map(|f| f.delay_ms.max(10)).sum();
        if total == 0 {
            return &self.frames[0];
        }
        let mut t = elapsed_ms % total;
        for frame in &self.frames {
            let delay = frame.delay_ms.max(10);
            if t < delay {
                return frame;
            }
            t -= delay;
        }
        self.frames.last().unwrap_or(&self.frames[0])
    }

    /// Total animation duration in milliseconds.
    pub fn animation_duration_ms(&self) -> u32 {
        self.frames.iter().map(|f| f.delay_ms.max(10)).sum()
    }

    /// One-line description for the info panel.
    pub fn summary(&self) -> String {
        let mut parts = vec![
            format!("{}x{}", self.width, self.height),
            self.format.clone(),
        ];
        if self.is_animated {
            parts.push(format!("{} 帧", self.frames.len()));
        }
        if self.downscaled {
            parts.push("已缩放".to_string());
        }
        parts.join(" · ")
    }
}

/// How the image is fitted into the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FitMode {
    /// Scale down so the whole image is visible (never upscale).
    #[default]
    Fit,
    /// Scale up or down to fill the viewport, cropping if needed.
    Fill,
    /// 100% — one image pixel per screen pixel.
    Original,
    /// A user-chosen zoom factor.
    Custom,
}

/// Interactive state for the image viewer.
#[derive(Debug, Clone)]
pub struct ImageView {
    /// The loaded image, if any.
    pub doc: Option<ImageDoc>,
    /// Current fitting strategy.
    pub fit: FitMode,
    /// Zoom factor used when `fit == FitMode::Custom`.
    pub zoom: f32,
    /// Pan offset in screen pixels.
    pub offset: (f32, f32),
    /// Extra rotation applied by the user, in degrees clockwise.
    pub rotation: i32,
    /// Mirror horizontally.
    pub flip_h: bool,
    /// Mirror vertically.
    pub flip_v: bool,
    /// Milliseconds since the current animation frame started.
    pub animation_time_ms: u32,
    /// Last time the animation advanced.
    last_tick: Option<std::time::Instant>,
    /// Animated images advance only while this is true.
    pub animation_playing: bool,
    /// `true` when the viewer is cycling through a folder.
    pub slideshow: bool,
    /// Seconds between slides.
    pub slideshow_interval: f32,
    /// Seconds since the current slide appeared.
    pub slideshow_elapsed: f32,
}

impl Default for ImageView {
    fn default() -> Self {
        Self {
            doc: None,
            fit: FitMode::Fit,
            zoom: 1.0,
            offset: (0.0, 0.0),
            rotation: 0,
            flip_h: false,
            flip_v: false,
            animation_time_ms: 0,
            last_tick: None,
            animation_playing: true,
            slideshow: false,
            slideshow_interval: 5.0,
            slideshow_elapsed: 0.0,
        }
    }
}

impl ImageView {
    /// An empty viewer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode `path` and display it, replacing whatever was shown.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        let doc = load(path)?;
        self.doc = Some(doc);
        self.reset_view();
        Ok(())
    }

    /// Forget the current image.
    pub fn close(&mut self) {
        self.doc = None;
        self.reset_view();
    }

    /// Reset zoom, pan, rotation and the animation clock.
    pub fn reset_view(&mut self) {
        self.fit = FitMode::Fit;
        self.zoom = 1.0;
        self.offset = (0.0, 0.0);
        self.rotation = 0;
        self.flip_h = false;
        self.flip_v = false;
        self.animation_time_ms = 0;
        self.last_tick = None;
        self.slideshow_elapsed = 0.0;
    }

    /// `true` when an image is loaded.
    pub fn is_loaded(&self) -> bool {
        self.doc.is_some()
    }

    /// Pixel dimensions of the loaded image.
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        self.doc.as_ref().map(|d| (d.width, d.height))
    }

    /// Jump to 100 % zoom.
    pub fn zoom_original(&mut self) {
        self.fit = FitMode::Original;
        self.zoom = 1.0;
        self.offset = (0.0, 0.0);
    }

    /// Multiply the zoom by `factor`, switching to [`FitMode::Custom`].
    pub fn zoom_by(&mut self, factor: f32, viewport: Option<(f32, f32)>) {
        let base = match self.fit {
            FitMode::Original => 1.0,
            FitMode::Custom => self.zoom,
            _ => 1.0,
        };
        let zoom = (base * factor).clamp(0.02, 64.0);
        self.set_zoom(zoom, viewport);
    }

    /// Set an absolute zoom factor.
    pub fn set_zoom(&mut self, zoom: f32, viewport: Option<(f32, f32)>) {
        let previous = self.effective_scale(viewport);
        self.fit = FitMode::Custom;
        self.zoom = zoom.clamp(0.02, 64.0);
        let next = self.effective_scale(viewport);
        if previous > 0.0 {
            // Keep the point under the middle of the viewport stable.
            let ratio = next / previous;
            self.offset = (self.offset.0 * ratio, self.offset.1 * ratio);
        }
    }

    /// Scale that will actually be used to draw, given the viewport size.
    pub fn effective_scale(&self, viewport: Option<(f32, f32)>) -> f32 {
        let Some(doc) = &self.doc else {
            return 1.0;
        };
        let (iw, ih) = (doc.width.max(1) as f32, doc.height.max(1) as f32);
        let Some((vw, vh)) = viewport else {
            return self.zoom;
        };
        if vw <= 1.0 || vh <= 1.0 {
            return self.zoom;
        }
        match self.fit {
            FitMode::Fit => (vw / iw).min(vh / ih).min(1.0),
            FitMode::Fill => (vw / iw).max(vh / ih),
            FitMode::Original => 1.0,
            FitMode::Custom => self.zoom,
        }
    }

    /// Pan by a screen-space delta.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.offset.0 += dx;
        self.offset.1 += dy;
    }

    /// Rotate 90° clockwise.
    pub fn rotate_cw(&mut self) {
        self.rotation = (self.rotation + 90).rem_euclid(360);
        self.offset = (self.offset.1, -self.offset.0);
    }

    /// Rotate 90° counter-clockwise.
    pub fn rotate_ccw(&mut self) {
        self.rotation = (self.rotation - 90).rem_euclid(360);
        self.offset = (-self.offset.1, self.offset.0);
    }

    /// Flip horizontally.
    pub fn toggle_flip_h(&mut self) {
        self.flip_h = !self.flip_h;
    }

    /// Flip vertically.
    pub fn toggle_flip_v(&mut self) {
        self.flip_v = !self.flip_v;
    }

    /// Advance the animation and slideshow clocks.
    ///
    /// Returns `true` when the slideshow wants the next file.
    pub fn tick(&mut self) -> bool {
        let now = std::time::Instant::now();
        let dt_ms = match self.last_tick {
            Some(previous) => now.duration_since(previous).as_millis().min(1000) as u32,
            None => 0,
        };
        self.last_tick = Some(now);

        if let Some(doc) = &self.doc {
            if doc.is_animated && self.animation_playing {
                self.animation_time_ms = self
                    .animation_time_ms
                    .wrapping_add(dt_ms)
                    % doc.animation_duration_ms().max(1);
            }
        }

        if self.slideshow {
            self.slideshow_elapsed += dt_ms as f32 / 1000.0;
            if self.slideshow_elapsed >= self.slideshow_interval.max(0.5) {
                self.slideshow_elapsed = 0.0;
                return true;
            }
        }
        false
    }

    /// The frame that should currently be displayed.
    pub fn current_frame(&self) -> Option<&ImageFrame> {
        let doc = self.doc.as_ref()?;
        if doc.frames.is_empty() {
            return None;
        }
        Some(doc.frame_at(self.animation_time_ms))
    }

    /// `true` when the viewer needs continuous repaints (animated image or a
    /// running slideshow).
    pub fn wants_animation(&self) -> bool {
        self.doc.as_ref().is_some_and(|d| d.is_animated) && self.animation_playing
            || self.slideshow
    }

    /// Seconds until the next animation frame, used to schedule repaints.
    pub fn time_to_next_frame(&self) -> Option<Duration> {
        let doc = self.doc.as_ref()?;
        if !doc.is_animated || !self.animation_playing {
            return None;
        }
        let total: u32 = doc.frames.iter().map(|f| f.delay_ms.max(10)).sum();
        if total == 0 {
            return None;
        }
        let mut t = self.animation_time_ms % total;
        for frame in &doc.frames {
            let delay = frame.delay_ms.max(10);
            if t < delay {
                return Some(Duration::from_millis(u64::from(delay - t)));
            }
            t -= delay;
        }
        Some(Duration::from_millis(100))
    }
}

/// Decode an image file into RGBA frames.
pub fn load(path: &Path) -> Result<ImageDoc> {
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let extension = crate::util::extension_of(path).unwrap_or_default();
    let format_name = format_name_for(&extension);

    // Read the raw bytes once: this lets us sniff EXIF, then hand the same
    // buffer to the decoder (and to two decoders in the APNG/WebP case).
    let bytes = std::fs::read(path)?;

    let exif_orientation = if matches!(extension.as_str(), "jpg" | "jpeg" | "jpe" | "jfif") {
        read_exif_orientation(&bytes).unwrap_or(1)
    } else {
        1
    };

    let cursor = std::io::Cursor::new(&bytes);
    let reader = image::ImageReader::new(cursor)
        .with_guessed_format()
        .map_err(|e| MediaError::other(format!("无法识别图片格式: {e}")))?;
    let detected = reader
        .format()
        .map(|f| format!("{f:?}").to_uppercase())
        .unwrap_or(format_name);

    let mut frames = decode_frames(&bytes, &detected)?;
    if frames.is_empty() {
        return Err(MediaError::other("图片没有任何可显示的帧"));
    }

    let (mut width, mut height, color_type) = describe_first_frame(&bytes, &detected)?;

    // EXIF orientation 5–8 swap the axes; apply the rotation to the pixels so
    // that phone photos are not shown sideways.
    let rotated_axes = matches!(exif_orientation, 5..=8);
    if rotated_axes {
        std::mem::swap(&mut width, &mut height);
    }
    if exif_orientation != 1 {
        for frame in frames.iter_mut() {
            let source = if rotated_axes {
                // The decoded buffer still has the un-swapped dimensions.
                (height, width)
            } else {
                (width, height)
            };
            frame.data = apply_exif_orientation(&frame.data, source.0, source.1, exif_orientation);
        }
    }

    // Enforce the texture limit by downscaling rather than refusing to show.
    let mut downscaled = false;
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        let scale = (MAX_DIMENSION as f32 / width as f32).min(MAX_DIMENSION as f32 / height as f32);
        let new_width = ((width as f32 * scale).round() as u32).max(1);
        let new_height = ((height as f32 * scale).round() as u32).max(1);
        for frame in frames.iter_mut() {
            frame.data = resize_rgba(&frame.data, width, height, new_width, new_height);
        }
        width = new_width;
        height = new_height;
        downscaled = true;
    }

    let is_animated = frames.len() > 1;
    Ok(ImageDoc {
        path: path.to_path_buf(),
        width,
        height,
        frames,
        is_animated,
        format: detected,
        color_type,
        file_size,
        exif_orientation,
        downscaled,
    })
}

fn format_name_for(extension: &str) -> String {
    match extension {
        "jpg" | "jpeg" | "jpe" | "jfif" => "JPEG".to_string(),
        "png" => "PNG".to_string(),
        "gif" => "GIF".to_string(),
        "webp" => "WEBP".to_string(),
        "bmp" => "BMP".to_string(),
        "tif" | "tiff" => "TIFF".to_string(),
        "ico" => "ICO".to_string(),
        "tga" => "TGA".to_string(),
        "dds" => "DDS".to_string(),
        "qoi" => "QOI".to_string(),
        "avif" => "AVIF".to_string(),
        "heic" | "heif" => "HEIF".to_string(),
        "jxl" => "JXL".to_string(),
        "exr" => "EXR".to_string(),
        "hdr" => "HDR".to_string(),
        "pnm" | "ppm" | "pgm" | "pbm" => "PNM".to_string(),
        other => other.to_uppercase(),
    }
}

/// Decode every frame of `bytes`, choosing an animation decoder when the format
/// supports one.
fn decode_frames(bytes: &[u8], format: &str) -> Result<Vec<ImageFrame>> {
    let animated = match format {
        "GIF" => image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))
            .ok()
            .map(collect_animation),
        "PNG" => image::codecs::png::PngDecoder::new(std::io::Cursor::new(bytes))
            .ok()
            .and_then(|decoder| {
                if decoder.is_apng().unwrap_or(false) {
                    decoder.apng().ok().map(collect_animation)
                } else {
                    None
                }
            }),
        "WEBP" => image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(bytes))
            .ok()
            .and_then(|decoder| {
                if decoder.has_animation() {
                    Some(collect_animation(decoder))
                } else {
                    None
                }
            }),
        _ => None,
    };

    if let Some(frames) = animated {
        if !frames.is_empty() {
            return Ok(frames);
        }
    }

    // Static path.
    let decoded = image::load_from_memory(bytes)?;
    let (width, height) = (decoded.width(), decoded.height());
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(MediaError::other(format!(
            "图片尺寸过大 ({width}x{height})，超过 {} 像素上限",
            MAX_PIXELS
        )));
    }
    let rgba = decoded.to_rgba8();
    Ok(vec![ImageFrame {
        data: rgba.into_raw(),
        delay_ms: 0,
    }])
}

/// Drain an animation decoder into our own frame representation, enforcing the
/// frame-count and byte budgets on the way.
fn collect_animation<'a, D>(decoder: D) -> Vec<ImageFrame>
where
    D: image::AnimationDecoder<'a>,
{
    let mut out: Vec<ImageFrame> = Vec::new();
    let mut total_bytes = 0usize;
    for frame in decoder.into_frames().take(MAX_ANIMATION_FRAMES) {
        let Ok(frame) = frame else { continue };
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        // A zero denominator means "unspecified"; 100 ms is what every viewer
        // falls back to for a badly authored animation.
        let delay_ms = numerator
            .checked_div(denominator)
            .unwrap_or(100)
            .clamp(10, 10_000);
        let buffer = frame.into_buffer();
        total_bytes += buffer.as_raw().len();
        out.push(ImageFrame {
            data: buffer.into_raw(),
            delay_ms,
        });
        if total_bytes > MAX_ANIMATION_BYTES {
            log::warn!("动画帧过多，已截断");
            break;
        }
    }
    out
}

fn describe_first_frame(bytes: &[u8], format: &str) -> Result<(u32, u32, String)> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| MediaError::other(format!("无法读取图片信息: {e}")))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| MediaError::other(format!("无法读取图片尺寸: {e}")))?;
    let color = match format {
        "PNG" | "GIF" | "WEBP" | "BMP" | "TIFF" => "8 位/通道 RGBA",
        "JPEG" => "8 位/通道 YCbCr",
        _ => "未知",
    };
    Ok((width, height, color.to_string()))
}

/// Read the EXIF orientation tag (0x0112) from a JPEG byte stream.
///
/// Returns `None` when the file has no EXIF block, which is the common case for
/// images produced by anything other than a phone or camera.
pub fn read_exif_orientation(bytes: &[u8]) -> Option<u16> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break; // start of scan / end of image: no EXIF beyond this point
        }
        let length = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if length < 2 || i + 2 + length > bytes.len() {
            break;
        }
        let segment = &bytes[i + 4..i + 2 + length];
        if marker == 0xE1 && segment.len() > 6 && &segment[..6] == b"Exif\0\0" {
            return parse_tiff_orientation(&segment[6..]);
        }
        i += 2 + length;
    }
    None
}

fn parse_tiff_orientation(tiff: &[u8]) -> Option<u16> {
    if tiff.len() < 8 {
        return None;
    }
    let little = match &tiff[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let read_u16 = |offset: usize| -> Option<u16> {
        let slice = tiff.get(offset..offset + 2)?;
        Some(if little {
            u16::from_le_bytes([slice[0], slice[1]])
        } else {
            u16::from_be_bytes([slice[0], slice[1]])
        })
    };
    let read_u32 = |offset: usize| -> Option<u32> {
        let slice = tiff.get(offset..offset + 4)?;
        Some(if little {
            u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
        } else {
            u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]])
        })
    };

    let ifd_offset = read_u32(4)? as usize;
    let count = read_u16(ifd_offset)? as usize;
    for entry in 0..count {
        let base = ifd_offset + 2 + entry * 12;
        let tag = read_u16(base)?;
        if tag == 0x0112 {
            let value = read_u16(base + 8)?;
            return Some(value.clamp(1, 8));
        }
    }
    None
}

/// Rotate/mirror an RGBA buffer according to an EXIF orientation value.
///
/// `width`/`height` describe the *source* buffer; for orientations 5–8 the
/// result has the axes swapped.
pub fn apply_exif_orientation(data: &[u8], width: u32, height: u32, orientation: u16) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    if data.len() < w * h * 4 || orientation <= 1 || orientation > 8 {
        return data.to_vec();
    }
    let swap = matches!(orientation, 5..=8);
    let (out_w, out_h) = if swap { (h, w) } else { (w, h) };
    let mut out = vec![0u8; out_w * out_h * 4];

    for y in 0..h {
        for x in 0..w {
            // Destination coordinate for the source pixel `(x, y)`, following
            // the EXIF orientation table (1 = as-shot, 6 = rotate 90° CW, ...).
            let (dx, dy) = match orientation {
                2 => (w - 1 - x, y),         // mirror horizontal
                3 => (w - 1 - x, h - 1 - y), // rotate 180°
                4 => (x, h - 1 - y),         // mirror vertical
                5 => (y, x),                 // transpose
                6 => (h - 1 - y, x),         // rotate 90° CW
                7 => (h - 1 - y, w - 1 - x), // transverse
                8 => (y, w - 1 - x),         // rotate 270° CW
                _ => (x, y),
            };
            let source = (y * w + x) * 4;
            let target = (dy.min(out_h - 1) * out_w + dx.min(out_w - 1)) * 4;
            out[target..target + 4].copy_from_slice(&data[source..source + 4]);
        }
    }
    out
}

/// Fast, box-filtered RGBA downscale.
///
/// Only ever used to bring an oversized image under [`MAX_DIMENSION`], where a
/// cheap filter is preferable to a slow one.
fn resize_rgba(data: &[u8], width: u32, height: u32, new_width: u32, new_height: u32) -> Vec<u8> {
    let Some(image) = image::RgbaImage::from_raw(width, height, data.to_vec()) else {
        return data.to_vec();
    };
    let resized = image::imageops::resize(
        &image,
        new_width,
        new_height,
        image::imageops::FilterType::Triangle,
    );
    resized.into_raw()
}

/// Human readable image metadata rows for the info panel.
pub fn info_rows(doc: &ImageDoc) -> Vec<(String, String)> {
    let mut rows = vec![
        (
            "分辨率".to_string(),
            format!(
                "{}x{} ({})",
                doc.width,
                doc.height,
                crate::util::aspect_label(doc.width, doc.height)
            ),
        ),
        ("格式".to_string(), doc.format.clone()),
        ("颜色".to_string(), doc.color_type.clone()),
        ("文件大小".to_string(), crate::util::format_size(doc.file_size)),
    ];
    if doc.is_animated {
        rows.push(("帧数".to_string(), doc.frames.len().to_string()));
        rows.push((
            "动画时长".to_string(),
            format!("{:.2} 秒", doc.animation_duration_ms() as f64 / 1000.0),
        ));
    }
    if doc.exif_orientation != 1 {
        rows.push((
            "EXIF 方向".to_string(),
            format!("{}", doc.exif_orientation),
        ));
    }
    if doc.downscaled {
        rows.push((
            "提示".to_string(),
            format!("已缩小到最长边 {MAX_DIMENSION} 像素以适配显卡"),
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_with_size(w: u32, h: u32) -> ImageDoc {
        ImageDoc {
            path: PathBuf::from("test.png"),
            width: w,
            height: h,
            frames: vec![ImageFrame {
                data: vec![0; (w * h * 4) as usize],
                delay_ms: 0,
            }],
            is_animated: false,
            format: "PNG".into(),
            color_type: "RGBA".into(),
            file_size: 0,
            exif_orientation: 1,
            downscaled: false,
        }
    }

    #[test]
    fn fit_mode_never_upscales_but_fill_does() {
        let mut view = ImageView::new();
        view.doc = Some(image_with_size(800, 600));
        view.fit = FitMode::Fit;
        assert!((view.effective_scale(Some((1920.0, 1080.0))) - 1.0).abs() < 1e-6);
        assert!(
            (view.effective_scale(Some((400.0, 300.0))) - 0.5).abs() < 1e-6,
            "large images scale down"
        );
        view.fit = FitMode::Fill;
        assert!(view.effective_scale(Some((1600.0, 600.0))) > 1.0);
    }

    #[test]
    fn zoom_is_clamped() {
        let mut view = ImageView::new();
        view.doc = Some(image_with_size(100, 100));
        view.zoom_by(1000.0, Some((100.0, 100.0)));
        assert!(view.zoom <= 64.0);
        view.zoom_by(0.00001, Some((100.0, 100.0)));
        assert!(view.zoom >= 0.02);
    }

    #[test]
    fn rotation_cycles_and_swaps_the_pan_axis() {
        let mut view = ImageView::new();
        view.pan(10.0, 0.0);
        view.rotate_cw();
        assert_eq!(view.rotation, 90);
        assert_eq!(view.offset, (0.0, -10.0));
        for _ in 0..3 {
            view.rotate_cw();
        }
        assert_eq!(view.rotation, 0);
        view.rotate_ccw();
        assert_eq!(view.rotation, 270);
    }

    #[test]
    fn animation_selects_the_right_frame() {
        let doc = ImageDoc {
            path: PathBuf::from("a.gif"),
            width: 2,
            height: 2,
            frames: vec![
                ImageFrame {
                    data: vec![1; 16],
                    delay_ms: 100,
                },
                ImageFrame {
                    data: vec![2; 16],
                    delay_ms: 200,
                },
            ],
            is_animated: true,
            format: "GIF".into(),
            color_type: "RGBA".into(),
            file_size: 0,
            exif_orientation: 1,
            downscaled: false,
        };
        assert_eq!(doc.frame_at(0).data[0], 1);
        assert_eq!(doc.frame_at(150).data[0], 2);
        assert_eq!(doc.frame_at(299).data[0], 2);
        assert_eq!(doc.frame_at(300).data[0], 1, "animation wraps");
        assert_eq!(doc.animation_duration_ms(), 300);
    }

    #[test]
    fn exif_orientation_six_rotates_clockwise() {
        // 2x1 image: [A][B] rotated 90° CW becomes a 1x2 column [A][B].
        let data = vec![
            1, 0, 0, 255, // A
            2, 0, 0, 255, // B
        ];
        let out = apply_exif_orientation(&data, 2, 1, 6);
        assert_eq!(out.len(), 8);
        assert_eq!(&out[0..4], &[1, 0, 0, 255]);
        assert_eq!(&out[4..8], &[2, 0, 0, 255]);
    }

    #[test]
    fn exif_orientation_one_is_a_no_op() {
        let data = vec![9u8; 16];
        assert_eq!(apply_exif_orientation(&data, 2, 2, 1), data);
    }

    #[test]
    fn exif_parser_reads_a_synthetic_tiff_block() {
        // Little-endian TIFF with a single IFD entry for orientation = 6.
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD at offset 8
        tiff.extend_from_slice(&1u16.to_le_bytes()); // one entry
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&6u16.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        assert_eq!(parse_tiff_orientation(&tiff), Some(6));
    }

    #[test]
    fn exif_parser_rejects_garbage() {
        assert_eq!(parse_tiff_orientation(&[]), None);
        assert_eq!(read_exif_orientation(&[0, 1, 2]), None);
    }

    #[test]
    fn slideshow_reports_when_the_slide_is_over() {
        let mut view = ImageView::new();
        view.doc = Some(image_with_size(10, 10));
        view.slideshow = true;
        view.slideshow_interval = 0.0; // clamped to 0.5s internally
        view.slideshow_elapsed = 10.0;
        assert!(view.tick());
        assert_eq!(view.slideshow_elapsed, 0.0);
    }
}
