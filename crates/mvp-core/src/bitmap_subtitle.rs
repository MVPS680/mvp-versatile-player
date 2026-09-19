//! Graphical (bitmap) subtitle support: PGS, VobSub and DVB.
//!
//! Unlike the text formats handled by `mvp-subtitle`, a graphical subtitle is a
//! picture: FFmpeg decodes it into an `AVSubtitleRect` holding an 8-bit palette
//! index plane plus a palette. Showing it therefore needs no OCR — the decoder
//! has already done the hard part — but it does need a place to carry the pixels
//! through the engine and a way to put them on screen, which is what this module
//! provides.
//!
//! ## Why the pixels are stored paletted
//!
//! A single 1080p subtitle line is roughly 100 KB as palette indices and four
//! times that as RGBA. A feature film can carry a couple of thousand of them, so
//! keeping them as indices and expanding to RGBA only for the cue on screen is
//! what keeps a graphical track from costing hundreds of megabytes.
//!
//! ## Time
//!
//! The cue model mirrors [`mvp_subtitle::Cue`]: a half-open `[start, end)` range
//! in seconds from the beginning of the media. PGS frames frequently carry no
//! duration of their own (`end_display_time` is zero), so
//! [`normalize`] closes such a cue at the start of the cue that follows it.

use std::path::Path;

use ffmpeg_next as ffmpeg;

use crate::error::{MediaError, Result};

/// One bitmap rectangle inside a cue, stored as an 8-bit palette index plane.
#[derive(Debug, Clone, PartialEq)]
pub struct BitmapRect {
    /// Left edge in the subtitle's own coordinate space.
    pub x: i32,
    /// Top edge in the subtitle's own coordinate space.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// One palette index per pixel, row-major, `width * height` bytes.
    pub indices: Vec<u8>,
    /// Palette entries as RGBA.
    pub palette: Vec<[u8; 4]>,
}

impl BitmapRect {
    /// Expand the index plane into straight (un-premultiplied) RGBA8.
    pub fn rgba(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.indices.len() * 4];
        for (pixel, &index) in self.indices.iter().enumerate() {
            let color = self
                .palette
                .get(index as usize)
                .copied()
                .unwrap_or([0, 0, 0, 0]);
            out[pixel * 4..pixel * 4 + 4].copy_from_slice(&color);
        }
        out
    }
}

/// One graphical subtitle cue: a half-open time range plus its rectangles.
#[derive(Debug, Clone, PartialEq)]
pub struct BitmapCue {
    /// Start time in seconds from the beginning of the media.
    pub start: f64,
    /// End time in seconds from the beginning of the media.
    pub end: f64,
    /// The rectangles that make up the picture.
    pub rects: Vec<BitmapRect>,
}

impl BitmapCue {
    /// Returns `true` when `t` falls inside `[start, end)`.
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }
}

/// A decoded graphical subtitle track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BitmapSubtitle {
    /// Cues, always sorted by [`BitmapCue::start`].
    pub cues: Vec<BitmapCue>,
    /// The coordinate space the rectangles are expressed in, when the source
    /// states one. PGS coordinates are already in video pixels and leave this
    /// `None`; VobSub carries the DVD frame size here.
    pub canvas: Option<(u32, u32)>,
}

impl BitmapSubtitle {
    /// An empty track that is safe to hand to the rest of the app.
    pub fn empty() -> Self {
        Self::default()
    }

    /// `true` when the track holds no cues.
    pub fn is_empty(&self) -> bool {
        self.cues.is_empty()
    }

    /// Number of cues in the track.
    pub fn len(&self) -> usize {
        self.cues.len()
    }

    /// The cue that should be visible at `t`.
    ///
    /// The active cue with the greatest `start` wins, matching the text model:
    /// when two overlap, the later declaration is the one a viewer expects.
    pub fn active_at(&self, t: f64) -> Option<&BitmapCue> {
        let mut best: Option<usize> = None;
        for (i, cue) in self.cues.iter().enumerate() {
            if cue.contains(t) {
                match best {
                    Some(j) if self.cues[j].start > cue.start => {}
                    _ => best = Some(i),
                }
            }
        }
        best.map(|i| &self.cues[i])
    }

    /// Every cue active at `t`, in file order.
    pub fn active_all(&self, t: f64) -> Vec<&BitmapCue> {
        self.cues.iter().filter(|cue| cue.contains(t)).collect()
    }

    /// Re-sort the cues by start time. Cheap after a seek-time rebuild.
    pub fn sort(&mut self) {
        self.cues
            .sort_by(|a, b| a.start.total_cmp(&b.start));
    }
}

/// Close cues that have no duration of their own and remove empty ones.
///
/// PGS and DVB frequently report `end_display_time == 0`, meaning "until the
/// next composition". A cue whose end does not exceed its start is therefore
/// closed at the next cue's start, and a trailing one gets a short default so it
/// is not invisible for its whole life.
pub fn normalize(cues: &mut Vec<BitmapCue>) {
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    let count = cues.len();
    for i in 0..count {
        if cues[i].end > cues[i].start {
            continue;
        }
        cues[i].end = cues
            .get(i + 1)
            .map(|next| next.start)
            .filter(|&start| start > cues[i].start)
            .unwrap_or(cues[i].start + 4.0);
    }
    cues.retain(|cue| !cue.rects.is_empty() && cue.end > cue.start);
}

/// Keep only the cues that can still be shown or are about to be: everything
/// that ends before `floor` is dropped. Used by the demuxer to bound the memory
/// a feature-length graphical track costs.
pub fn prune(cues: &mut Vec<BitmapCue>, floor: f64) {
    cues.retain(|cue| cue.end >= floor);
}

/// Total palette-index bytes held by the cues, for a size cap.
pub fn byte_size(cues: &[BitmapCue]) -> usize {
    cues.iter()
        .flat_map(|cue| &cue.rects)
        .map(|rect| rect.indices.len())
        .sum()
}

/// `true` when a side-car subtitle path holds a bitmap subtitle.
///
/// `.sup` is raw PGS and `.idx` is a VobSub index (the `.sub` sits beside it);
/// both are unambiguous. `.sub` is the awkward one — MicroDVD text and binary
/// VobSub/DVB share the extension — so the content is sniffed: a text `.sub`
/// starts with `{` or a digit, a binary one starts with a pack header or the
/// DVB sync byte.
pub fn is_graphic_subtitle(path: &Path) -> bool {
    match crate::util::extension_of(path).as_deref() {
        Some("sup") | Some("idx") => true,
        Some("sub") => {
            let mut head = [0u8; 16];
            let read = std::fs::File::open(path)
                .and_then(|mut file| {
                    use std::io::Read;
                    file.read(&mut head)
                })
                .unwrap_or(0);
            !looks_like_text_subtitle(&head[..read])
        }
        _ => false,
    }
}

/// `true` when the first bytes of a `.sub` file look like MicroDVD text.
fn looks_like_text_subtitle(head: &[u8]) -> bool {
    let head = head
        .iter()
        .position(|&b| b != 0)
        .map(|start| &head[start..])
        .unwrap_or(head);
    match head.first() {
        Some(b'{') => true,
        Some(c) if c.is_ascii_digit() => true,
        // A UTF-8 BOM or the leading whitespace of a text file.
        Some(0xEF) | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// FFmpeg decoding
// ---------------------------------------------------------------------------

/// Owned copy of a stream's codec parameters, safe to move to another thread.
pub(crate) fn own_parameters<'a>(stream: &ffmpeg::Stream<'a>) -> ffmpeg::codec::Parameters {
    let mut params = ffmpeg::codec::Parameters::new();
    // SAFETY: `params` is a freshly allocated AVCodecParameters and the source
    // pointer belongs to a live stream of a live format context. The copy makes
    // the destination fully independent, including its extradata.
    unsafe {
        ffmpeg::ffi::avcodec_parameters_copy(params.as_mut_ptr(), stream.parameters().as_ptr());
    }
    params
}

/// Decode one subtitle packet into a graphical cue.
///
/// Returns `None` when the packet is a composition update with no picture (a
/// PGS "clear"), which carries no rectangles. The `AVSubtitle` is always
/// released: `ffmpeg-next`'s wrapper has no `Drop`, and the decoder allocates a
/// palette and an index plane for every bitmap it produces.
pub(crate) fn decode_packet(
    decoder: &mut ffmpeg::decoder::Subtitle,
    packet: &ffmpeg::Packet,
    time_base: f64,
    start_offset: f64,
) -> Option<BitmapCue> {
    let mut subtitle = ffmpeg::Subtitle::new();
    let got = matches!(decoder.decode(packet, &mut subtitle), Ok(true));
    let cue = if got {
        cue_from_subtitle(&subtitle, packet.pts(), time_base, start_offset)
    } else {
        None
    };
    free_subtitle(&mut subtitle);
    cue
}

/// Convert a decoded `AVSubtitle` into our cue model.
pub(crate) fn cue_from_subtitle(
    subtitle: &ffmpeg::Subtitle,
    packet_pts: Option<i64>,
    time_base: f64,
    start_offset: f64,
) -> Option<BitmapCue> {
    let base = subtitle.pts().or(packet_pts)? as f64 * time_base - start_offset;
    let start = base + subtitle.start() as f64 / 1000.0;
    let end = base + subtitle.end() as f64 / 1000.0;

    let mut rects = Vec::new();
    for rect in subtitle.rects() {
        if let ffmpeg::subtitle::Rect::Bitmap(bitmap) = rect {
            if let Some(rect) = bitmap_rect(&bitmap) {
                rects.push(rect);
            }
        }
    }
    // A composition with no rectangles is kept as an empty cue: it is a PGS
    // "clear", and its timestamp is what closes whatever was on screen. The
    // external decoder drops these after normalising; the demuxer uses them.
    Some(BitmapCue { start, end, rects })
}

/// Copy one `AVSubtitleRect` bitmap into an owned, paletted rectangle.
fn bitmap_rect(bitmap: &ffmpeg::subtitle::Bitmap) -> Option<BitmapRect> {
    let width = bitmap.width();
    let height = bitmap.height();
    if width == 0 || height == 0 {
        return None;
    }
    let colors = bitmap.colors().min(256);

    // SAFETY: `bitmap.as_ptr()` is the live `AVSubtitleRect` the decoder wrote,
    // and every pointer read below (`data`, `linesize`) belongs to it for as
    // long as the `AVSubtitle` is alive, which is the caller's contract. The
    // index plane is copied out row by row, honouring a negative stride, before
    // the subtitle is freed.
    let (indices, palette) = unsafe {
        let rect = bitmap.as_ptr();
        let data = (*rect).data;
        if data[0].is_null() {
            return None;
        }

        let stride = (*rect).linesize[0];
        let step = stride.unsigned_abs() as usize;
        let row_bytes = width as usize;
        let mut indices = vec![0u8; row_bytes * height as usize];
        let base = data[0] as *const u8;
        for row in 0..height as usize {
            let source = if stride < 0 {
                base.add(step * (height as usize - 1 - row))
            } else {
                base.add(step * row)
            };
            let row_slice = std::slice::from_raw_parts(source, row_bytes);
            indices[row * row_bytes..(row + 1) * row_bytes].copy_from_slice(row_slice);
        }

        let mut palette = vec![[0u8; 4]; 256];
        let palette_ptr = data[1];
        if palette_ptr.is_null() || colors == 0 {
            // A bitmap with no usable palette renders as an opaque white mask,
            // which is at least visible rather than silently transparent.
            palette.fill([255, 255, 255, 255]);
        } else {
            // `AVPALETTE` is `AV_PIX_FMT_RGB32`: a native-endian u32 laid out
            // as 0xAARRGGBB, so four bytes per entry in memory.
            for (i, slot) in palette.iter_mut().enumerate().take(colors) {
                let ptr = palette_ptr.add(i * 4);
                let bytes = [*ptr, *ptr.add(1), *ptr.add(2), *ptr.add(3)];
                let value = u32::from_ne_bytes(bytes);
                *slot = [
                    ((value >> 16) & 0xFF) as u8,
                    ((value >> 8) & 0xFF) as u8,
                    (value & 0xFF) as u8,
                    ((value >> 24) & 0xFF) as u8,
                ];
            }
        }
        (indices, palette)
    };

    Some(BitmapRect {
        x: bitmap.x() as i32,
        y: bitmap.y() as i32,
        width,
        height,
        indices,
        palette,
    })
}

/// Release the rectangles an `AVSubtitle` owns.
pub(crate) fn free_subtitle(subtitle: &mut ffmpeg::Subtitle) {
    // SAFETY: every `AVSubtitle` handed to `avsubtitle_free` is either zeroed
    // (no rectangles) or was filled by `avcodec_decode_subtitle2`, which is the
    // exact producer `avsubtitle_free` matches. FFmpeg zeroes the struct as it
    // frees, so a second call would also be harmless.
    unsafe { ffmpeg::ffi::avsubtitle_free(subtitle.as_mut_ptr()) };
}

/// The pixel size a stream declares, when it declares one.
pub(crate) fn stream_size<'a>(stream: &ffmpeg::Stream<'a>) -> Option<(u32, u32)> {
    // SAFETY: the pointer belongs to a live stream; width and height are plain
    // integer fields of AVCodecParameters.
    unsafe {
        let params = stream.parameters();
        let width = (*params.as_ptr()).width;
        let height = (*params.as_ptr()).height;
        if width > 0 && height > 0 {
            Some((width as u32, height as u32))
        } else {
            None
        }
    }
}

/// Upper bound on palette-index bytes decoded from one external file.
const EXTERNAL_BITMAP_LIMIT: usize = 512 * 1024 * 1024;

/// A VobSub `.sub` is only half of the pair: its timestamps and palette live in
/// the `.idx` beside it, and FFmpeg's demuxer needs the index. Picking either
/// file therefore opens the index when one is there.
fn resolve_vobsub_index(path: &Path) -> std::path::PathBuf {
    if crate::util::extension_of(path).as_deref() == Some("sub") {
        let index = path.with_extension("idx");
        if index.is_file() {
            return index;
        }
    }
    path.to_path_buf()
}

/// Decode an external graphical subtitle file into a full track.
///
/// `.sup` (PGS) and `.idx` (VobSub, whose `.sub` is found beside it) are opened
/// straight by FFmpeg's own demuxers. The whole file is decoded up front: the
/// cues are needed for random access on a seek, and a graphical stream's whole
/// point is that it can be decoded without a decoder state that survives between
/// cues.
pub fn decode_external(path: &Path) -> Result<BitmapSubtitle> {
    crate::init()?;
    let path = resolve_vobsub_index(path);
    let mut ictx = ffmpeg::format::input(&path)
        .map_err(|err| MediaError::Other(format!("无法打开图形字幕文件: {err}")))?;

    let (index, time_base, start_offset, canvas) = {
        let stream = ictx
            .streams()
            .find(|stream| stream.parameters().medium() == ffmpeg::media::Type::Subtitle)
            .ok_or_else(|| {
                MediaError::Other(format!("图形字幕文件没有字幕流: {}", path.display()))
            })?;
        let time_base = crate::engine::rational_to_f64(stream.time_base());
        let start = stream.start_time();
        let start_offset = if start > 0 && start != i64::MIN {
            start as f64 * time_base
        } else {
            0.0
        };
        (stream.index(), time_base, start_offset, stream_size(&stream))
    };

    let params = {
        let stream = ictx
            .streams()
            .find(|stream| stream.index() == index)
            .ok_or_else(|| MediaError::MissingStream("字幕".into()))?;
        own_parameters(&stream)
    };
    let codec = ffmpeg::codec::decoder::find(params.id())
        .ok_or_else(|| MediaError::unsupported("系统中的图形字幕解码器不可用"))?;
    let context = ffmpeg::codec::context::Context::from_parameters(params)?;
    let mut decoder = context.decoder().open_as(codec)?.subtitle()?;

    let mut cues: Vec<BitmapCue> = Vec::new();
    let mut decoded_bytes = 0usize;
    for (stream, packet) in ictx.packets() {
        if stream.index() != index {
            continue;
        }
        if let Some(cue) = decode_packet(&mut decoder, &packet, time_base, start_offset) {
            // A feature film's graphical track is a few hundred megabytes
            // paletted; anything far beyond that is a malformed or hostile file
            // and is refused rather than allowed to exhaust memory.
            decoded_bytes += byte_size(std::slice::from_ref(&cue));
            if decoded_bytes > EXTERNAL_BITMAP_LIMIT {
                return Err(MediaError::Other(format!(
                    "图形字幕解码后超过 {} MB，已放弃",
                    EXTERNAL_BITMAP_LIMIT / (1024 * 1024)
                )));
            }
            cues.push(cue);
        }
    }
    normalize(&mut cues);
    Ok(BitmapSubtitle { cues, canvas })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(width: u32, height: u32, indices: Vec<u8>) -> BitmapRect {
        BitmapRect {
            x: 0,
            y: 0,
            width,
            height,
            indices,
            palette: vec![[255, 255, 255, 255]; 256],
        }
    }

    fn cue(start: f64, end: f64) -> BitmapCue {
        BitmapCue {
            start,
            end,
            rects: vec![rect(1, 1, vec![0])],
        }
    }

    #[test]
    fn active_at_uses_a_half_open_range_and_prefers_the_later_cue() {
        let track = BitmapSubtitle {
            cues: vec![cue(0.0, 1.0), cue(0.5, 2.0)],
            canvas: None,
        };
        assert!(track.active_at(1.0).is_some());
        // The later cue wins where the two overlap.
        assert_eq!(track.active_at(1.5).unwrap().start, 0.5);
        assert!(track.active_at(2.0).is_none());
        assert!(track.active_at(f64::NAN).is_none());
    }

    #[test]
    fn a_paletted_rect_expands_to_rgba() {
        let mut rect = rect(2, 1, vec![1, 2]);
        rect.palette[1] = [10, 20, 30, 40];
        rect.palette[2] = [50, 60, 70, 80];
        assert_eq!(rect.rgba(), vec![10, 20, 30, 40, 50, 60, 70, 80]);
    }

    #[test]
    fn normalize_closes_cues_without_a_duration_at_the_next_start() {
        let mut cues = vec![
            BitmapCue {
                start: 0.0,
                end: 0.0,
                rects: vec![rect(1, 1, vec![0])],
            },
            BitmapCue {
                start: 3.0,
                end: 4.0,
                rects: vec![rect(1, 1, vec![0])],
            },
        ];
        normalize(&mut cues);
        assert_eq!(cues[0].end, 3.0);
        assert_eq!(cues[1].end, 4.0);
    }

    #[test]
    fn normalize_drops_cues_with_no_rectangles() {
        let mut cues = vec![BitmapCue {
            start: 0.0,
            end: 1.0,
            rects: Vec::new(),
        }];
        normalize(&mut cues);
        assert!(cues.is_empty());
    }

    #[test]
    fn a_binary_sub_is_told_apart_from_a_text_one() {
        assert!(looks_like_text_subtitle(b"{100}{200}hello"));
        assert!(looks_like_text_subtitle(b"  {5}{6}hello"));
        assert!(!looks_like_text_subtitle(&[0x00, 0x00, 0x01, 0xBA, 0x44]));
    }

    /// The extraction path reads the raw `AVSubtitleRect` fields, which cannot
    /// be exercised without a real PGS/VobSub sample. This builds the same
    /// structure the decoder produces by hand, so the palette layout and the
    /// index plane copy are covered even when no sample is available.
    #[test]
    fn a_decoded_bitmap_subtitle_becomes_an_owned_cue() {
        let mut subtitle = ffmpeg::Subtitle::new();
        {
            let mut rect = subtitle.add_rect(ffmpeg::subtitle::Type::Bitmap);
            let bitmap = match &mut rect {
                ffmpeg::subtitle::RectMut::Bitmap(bitmap) => bitmap,
                _ => unreachable!("add_rect was asked for a bitmap"),
            };
            bitmap.set_x(10);
            bitmap.set_y(20);
            bitmap.set_width(2);
            bitmap.set_height(2);
            bitmap.set_colors(3);

            let indices = [1u8, 2, 0, 1];
            // 0xAARRGGBB, so entry 1 is r=0x10 g=0x20 b=0x30 a=0xFF.
            let palette: [u32; 3] = [0x0000_0000, 0xFF10_2030, 0x8040_5060];
            // SAFETY: allocate with `av_malloc` so the matching `avsubtitle_free`
            // below can release the planes exactly as it would after a decode.
            unsafe {
                let plane = ffmpeg::ffi::av_malloc(indices.len()) as *mut u8;
                std::ptr::copy_nonoverlapping(indices.as_ptr(), plane, indices.len());
                let pal = ffmpeg::ffi::av_malloc(palette.len() * 4) as *mut u8;
                for (i, value) in palette.iter().enumerate() {
                    let bytes = value.to_ne_bytes();
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), pal.add(i * 4), 4);
                }
                let ptr = rect.as_mut_ptr();
                (*ptr).data[0] = plane;
                (*ptr).linesize[0] = 2;
                (*ptr).data[1] = pal;
            }
        }
        // 90 kHz time base, first second.
        subtitle.set_pts(Some(90_000));

        let cue = cue_from_subtitle(&subtitle, None, 1.0 / 90_000.0, 0.0).expect("a cue");
        assert_eq!(cue.start, 1.0);
        assert_eq!(cue.rects.len(), 1);
        let rect = &cue.rects[0];
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (10, 20, 2, 2));
        assert_eq!(rect.indices, vec![1, 2, 0, 1]);
        assert_eq!(rect.palette[1], [0x10, 0x20, 0x30, 0xFF]);
        assert_eq!(rect.palette[2], [0x40, 0x50, 0x60, 0x80]);

        free_subtitle(&mut subtitle);
    }
}
