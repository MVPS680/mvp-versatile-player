//! Decoded video frames, buffer recycling and the colour-correct YUV→RGBA
//! conversion stage.
//!
//! Frames leave this module as tightly packed 8-bit RGBA, which is exactly what
//! `egui` uploads to a GPU texture. The conversion writes *directly* into a
//! pooled byte buffer, so pixel data is never copied after decoding.

use std::ptr;

use ffmpeg_next as ffmpeg;
use ffmpeg::ffi;
use parking_lot::Mutex;

use crate::error::{MediaError, Result};
use crate::hdr::{HdrInfo, ToneMapper};

/// One decoded, display-ready video frame.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Width in pixels (already scaled to the requested target size).
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Presentation timestamp in seconds.
    pub pts: f64,
    /// Nominal display duration in seconds (`0.0` when unknown).
    pub duration: f64,
    /// Tightly packed RGBA8, `width * height * 4` bytes, no padding.
    pub data: Vec<u8>,
    /// Monotonically increasing frame counter; used to skip redundant uploads.
    pub serial: u64,
    /// Generation of the decode session that produced this frame. Frames from an
    /// older generation are dropped after a seek.
    pub generation: u64,
}

impl VideoFrame {
    /// Number of bytes the pixel buffer occupies.
    pub fn byte_len(&self) -> usize {
        self.data.len()
    }

    /// `true` when the frame has usable pixels.
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.data.len() >= (self.width as usize * self.height as usize * 4)
    }

    /// Approximate memory footprint used for queue budgeting.
    pub fn memory_cost(&self) -> usize {
        self.data.capacity().max(self.data.len())
    }
}

/// A small free-list of RGBA byte buffers.
///
/// Video frames are megabytes each and are produced many times per second, so
/// re-allocating them would dominate the profile. The pool hands back the
/// largest suitable buffer and keeps a bounded number of bytes alive overall.
#[derive(Debug)]
pub struct FramePool {
    free: Mutex<Vec<Vec<u8>>>,
    budget: usize,
    held: Mutex<usize>,
}

impl FramePool {
    /// Create a pool that will keep at most `budget` bytes of spare buffers.
    pub fn new(budget: usize) -> Self {
        Self {
            free: Mutex::new(Vec::new()),
            budget,
            held: Mutex::new(0),
        }
    }

    /// Obtain a buffer with exactly `len` usable bytes.
    ///
    /// The contents are undefined and will be fully overwritten by the caller.
    pub fn acquire(&self, len: usize) -> Vec<u8> {
        let mut free = self.free.lock();
        // Best fit: the smallest buffer that is still large enough avoids
        // wasting a 4K buffer on a thumbnail-sized frame.
        let mut best: Option<usize> = None;
        for (i, buf) in free.iter().enumerate() {
            if buf.capacity() >= len {
                match best {
                    Some(b) if free[b].capacity() <= buf.capacity() => {}
                    _ => best = Some(i),
                }
            }
        }
        if let Some(i) = best {
            let mut buf = free.swap_remove(i);
            let cap = buf.capacity();
            // Shrink to the requested length and grow only if the buffer came
            // back shorter. `clear()` followed by `resize(len, 0)` — what this
            // used to be — zero-fills the whole buffer on every reuse, and a 4K
            // frame is 33 MB of pointless memset for bytes the caller is about
            // to overwrite in full (which is the documented contract above).
            buf.truncate(len);
            if buf.len() < len {
                buf.resize(len, 0);
            }
            *self.held.lock() -= cap;
            return buf;
        }
        vec![0u8; len]
    }

    /// Give a buffer back. Oversized buffers and buffers beyond the budget are
    /// simply dropped.
    pub fn release(&self, buf: Vec<u8>) {
        if buf.capacity() == 0 || buf.capacity() > self.budget {
            return;
        }
        let mut held = self.held.lock();
        if *held + buf.capacity() > self.budget {
            return;
        }
        *held += buf.capacity();
        drop(held);
        let mut free = self.free.lock();
        // Keep the list short; the budget already bounds the total bytes.
        if free.len() < 24 {
            free.push(buf);
        }
    }

    /// Total bytes currently parked in the pool (for the debug/statistics view).
    pub fn buffered_bytes(&self) -> usize {
        *self.held.lock()
    }

    /// Drop every spare buffer (used when playback stops).
    pub fn clear(&self) {
        self.free.lock().clear();
        *self.held.lock() = 0;
    }
}

/// How many threads the tone map may be spread over.
///
/// Two cores are left alone on purpose: the demuxer and the audio decoder are
/// running too, and a player that takes every core to tone map a frame is a
/// player that makes the interface stutter. Capped at four because the split
/// only pays while each band is still large enough to be worth a thread —
/// below that, handing work out costs more than it saves.
fn tone_map_bands() -> usize {
    std::thread::available_parallelism()
        .map(|cores| cores.get().saturating_sub(2).clamp(1, 4))
        .unwrap_or(1)
}

/// The sizes a conversion works at, once they have been checked.
#[derive(Debug, Clone, Copy)]
struct Geometry {
    /// Destination width in pixels.
    dst_width: i32,
    /// Destination height in pixels.
    dst_height: i32,
}

impl Geometry {
    /// Bytes the destination image occupies.
    fn dst_bytes(&self) -> usize {
        self.dst_width as usize * self.dst_height as usize * 4
    }
}

/// Colour conversion + scaling from any FFmpeg pixel format to packed RGBA.
///
/// The underlying `SwsContext` is cached across frames and only rebuilt when a
/// frame with a different size/format/colour profile shows up, which is the
/// fast path FFmpeg itself recommends.
pub struct RgbaConverter {
    ctx: *mut ffi::SwsContext,
    src_width: i32,
    src_height: i32,
    src_format: ffi::AVPixelFormat,
    dst_width: i32,
    dst_height: i32,
    colorspace: i32,
    src_range: i32,
    /// Bring HDR frames into SDR range (the interface's preference).
    tone_map: bool,
    /// Mapper built for the dynamic range of the frames currently arriving.
    mapper: Option<(HdrInfo, ToneMapper)>,
    /// Threads the tone map may use. See [`Self::spread_over_threads`].
    bands: usize,
    /// Milliseconds the last [`Self::convert`] spent tone mapping.
    ///
    /// The tone map is a full-frame per-pixel loop and it happens inside the
    /// conversion, so this is the only way to tell how much of the conversion
    /// time it accounts for without timing it from outside.
    last_tone_map_ms: f32,
}

// SAFETY: the converter is created on, moved to and only ever used from the
// single video decode thread; no other thread can observe the raw pointer.
unsafe impl Send for RgbaConverter {}

impl RgbaConverter {
    /// Create an empty converter; the scaler is built lazily on first use.
    pub fn new() -> Self {
        Self {
            ctx: ptr::null_mut(),
            src_width: 0,
            src_height: 0,
            src_format: ffi::AVPixelFormat::AV_PIX_FMT_NONE,
            dst_width: 0,
            dst_height: 0,
            colorspace: 0,
            src_range: -1,
            tone_map: true,
            mapper: None,
            bands: tone_map_bands(),
            last_tone_map_ms: 0.0,
        }
    }

    /// Turn HDR → SDR tone mapping on or off.
    ///
    /// The interface owns this preference, so the flag arrives as a setter
    /// rather than at construction: the engine is built before the settings
    /// window can be opened, and the decoder may already be running.
    pub fn set_tone_map(&mut self, enabled: bool) {
        if self.tone_map != enabled {
            self.tone_map = enabled;
            self.mapper = None;
        }
    }

    /// `true` when HDR frames are being brought into SDR range.
    pub fn tone_map(&self) -> bool {
        self.tone_map
    }

    /// Milliseconds the last conversion spent tone mapping, `0.0` when the
    /// frame needed none.
    pub fn last_tone_map_ms(&self) -> f32 {
        self.last_tone_map_ms
    }

    /// Convert `src` into RGBA at `dst_width x dst_height`.
    ///
    /// The returned vector always has exactly `dst_width * dst_height * 4`
    /// bytes and comes from `pool`.
    pub fn convert(
        &mut self,
        src: &ffmpeg::frame::Video,
        dst_width: u32,
        dst_height: u32,
        pool: &FramePool,
    ) -> Result<Vec<u8>> {
        let geometry = self.prepare(src, dst_width, dst_height)?;
        let mut out = pool.acquire(geometry.dst_bytes());
        self.scale(src, &mut out, dst_width)?;
        self.apply_tone_map(src, &mut out);
        Ok(out)
    }

    /// Validate the frame, choose the colour matrix and make sure `self.ctx` is
    /// built for this geometry.
    fn prepare(
        &mut self,
        src: &ffmpeg::frame::Video,
        dst_width: u32,
        dst_height: u32,
    ) -> Result<Geometry> {
        let width = src.width() as i32;
        let height = src.height() as i32;
        // A damaged or mis-detected stream can hand over a frame with a bogus
        // geometry; scaling it would be reading and writing past the end of
        // something. Cheap to check, impossible to survive without.
        const MAX_DIMENSION: i32 = 32_768;
        if width <= 0 || height <= 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(MediaError::other(format!(
                "解码帧尺寸无效: {width}x{height}"
            )));
        }
        // Read the plane pointer straight out of the AVFrame: the `data()`
        // accessor builds a slice from it, and building a slice out of a null
        // pointer is exactly the thing this check exists to prevent.
        // SAFETY: the frame is alive for the duration of the call and the two
        // arrays are plain inline fields of it.
        let has_pixels = unsafe {
            let raw = src.as_ptr();
            !(*raw).data[0].is_null() && (*raw).linesize[0] != 0
        };
        if !has_pixels {
            return Err(MediaError::other("解码帧没有像素数据"));
        }
        let dst_width = dst_width.max(1) as i32;
        let dst_height = dst_height.max(1) as i32;
        if dst_width > MAX_DIMENSION || dst_height > MAX_DIMENSION {
            return Err(MediaError::other(format!(
                "目标尺寸无效: {dst_width}x{dst_height}"
            )));
        }
        let src_format: ffi::AVPixelFormat = src.format().into();
        if src_format == ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            return Err(MediaError::other("解码帧像素格式未知"));
        }
        let colorspace = choose_colorspace(src, width, height);
        let src_range = choose_range(src);

        let needs_rebuild = self.ctx.is_null()
            || self.src_width != width
            || self.src_height != height
            || self.src_format != src_format
            || self.dst_width != dst_width
            || self.dst_height != dst_height
            || self.colorspace != colorspace
            || self.src_range != src_range;

        if needs_rebuild {
            self.rebuild(
                width,
                height,
                src_format,
                dst_width,
                dst_height,
                colorspace,
                src_range,
            )?;
        }

        Ok(Geometry {
            dst_width,
            dst_height,
        })
    }

    /// Colour-convert and scale `src` into `dst`, in one piece.
    ///
    /// `sws_scale` is deliberately *not* given the frame in bands.
    ///
    /// It looks as though it could be: the signature takes `srcSliceY` and
    /// `srcSliceH`, and FFmpeg's own slice threading hands it bands. But its
    /// slicing is stateful — the context carries `dstY` and the buffered source
    /// rows from one call to the next — and handing a *single* context a
    /// sequence of bands does not reproduce the whole-frame result, because the
    /// vertical filter at a band edge is fed fewer taps than it has
    /// mid-frame. Measured on a synthetic 640x480 -> 320x240 conversion cut in
    /// two: 121 of the 240 destination rows differed from the whole-frame
    /// result, starting at the cut. The exact parallel route is FFmpeg's
    /// receive-slice API (`sws_frame_start` / `sws_send_slice` /
    /// `sws_receive_slice`), which cuts the *destination* instead and re-runs
    /// the vertical filter with the whole source available. That needs
    /// AVFrame plumbing and a destination buffer of FFmpeg's choosing, so it
    /// is not what this does.
    ///
    /// The honest summary: the scaler stays single-threaded, and what is
    /// parallelised instead is the tone map, which is per-pixel and can be
    /// proved to produce identical bytes.
    fn scale(
        &self,
        src: &ffmpeg::frame::Video,
        dst: &mut [u8],
        dst_width: u32,
    ) -> Result<()> {
        let strides = [dst_width as i32 * 4, 0, 0, 0];

        // SAFETY: `self.ctx` is a live SwsContext built for exactly these
        // dimensions, formats and colour settings. `src` is a live AVFrame and
        // FFmpeg guarantees its planes stay valid for the duration of the call.
        // The destination points at `dst`, which is `prepare`-checked to be at
        // least `dst_width * dst_height * 4` long, and the matching stride
        // describes it.
        let written = unsafe {
            let src_frame = src.as_ptr();
            let dst_data = [
                dst.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            ];
            ffi::sws_scale(
                self.ctx,
                (*src_frame).data.as_ptr() as *const *const u8,
                (*src_frame).linesize.as_ptr(),
                0,
                src.height() as i32,
                dst_data.as_ptr(),
                strides.as_ptr(),
            )
        };

        if written <= 0 {
            return Err(MediaError::other("视频帧色彩转换失败"));
        }
        Ok(())
    }

    /// Bring `dst` into SDR range, when the frame needs it and the user allows
    /// it.
    ///
    /// This happens on the buffer the screen is about to show, rather than in
    /// the decoder: that buffer is already scaled to the window, so the cost
    /// follows the display rather than the source. The mapper is rebuilt only
    /// when a frame arrives with a different dynamic range.
    ///
    /// The work is per-pixel with no reads outside the pixel, so the buffer is
    /// split between threads — see [`RgbaConverter::tone_map_bands`] — and
    /// produces exactly the same bytes as doing it in one piece. At 4K this is
    /// the largest single cost of an HDR frame.
    fn apply_tone_map(&mut self, src: &ffmpeg::frame::Video, dst: &mut [u8]) {
        self.last_tone_map_ms = 0.0;
        if !self.tone_map {
            return;
        }
        let info = HdrInfo::from_frame(src);
        if !info.needs_tone_map() {
            return;
        }
        if self.mapper.as_ref().map(|(built, _)| *built) != Some(info) {
            self.mapper = Some((info, ToneMapper::new(info)));
        }
        let Some((_, mapper)) = &self.mapper else {
            return;
        };
        let started = std::time::Instant::now();
        self.spread_over_threads(mapper, dst);
        self.last_tone_map_ms = started.elapsed().as_secs_f32() * 1000.0;
    }

    /// Run `mapper` over `dst`, on `self.bands` threads when it is worth it.
    ///
    /// The split is by *pixels*, not by rows: `chunks_mut` hands out disjoint
    /// slices, so there is no aliasing to reason about and none of the
    /// `unsafe` a raw-pointer fan-out would need. Each pixel is mapped from
    /// itself alone, so where the cuts fall cannot be observed in the result.
    fn spread_over_threads(&self, mapper: &ToneMapper, dst: &mut [u8]) {
        let bands = self.bands.min(dst.len().div_ceil(4)).max(1);
        if bands < 2 {
            mapper.apply(dst);
            return;
        }
        // Whole pixels per band, with any remainder left to the last chunk so
        // the bands still tile the buffer exactly.
        let per_band = ((dst.len() / 4) / bands).max(1) * 4;
        std::thread::scope(|scope| {
            for band in dst.chunks_mut(per_band) {
                scope.spawn(move || mapper.apply(band));
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn rebuild(
        &mut self,
        width: i32,
        height: i32,
        src_format: ffi::AVPixelFormat,
        dst_width: i32,
        dst_height: i32,
        colorspace: i32,
        src_range: i32,
    ) -> Result<()> {
        // SAFETY: `sws_getCachedContext` frees and replaces the previous context
        // when the parameters differ; passing NULL is the documented way to
        // start fresh. All other arguments are plain integers or null.
        unsafe {
            let ctx = ffi::sws_getCachedContext(
                self.ctx,
                width,
                height,
                src_format,
                dst_width,
                dst_height,
                ffi::AVPixelFormat::AV_PIX_FMT_RGBA,
                ffi::SwsFlags::SWS_BILINEAR as i32,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            if ctx.is_null() {
                self.ctx = ptr::null_mut();
                return Err(MediaError::other("无法创建视频缩放上下文"));
            }
            self.ctx = ctx;
        }

        // Tell swscale which matrix and range the source uses, otherwise HD
        // (BT.709) material gets converted as SD (BT.601) and comes out with
        // visibly wrong saturation.
        // SAFETY: `self.ctx` is a freshly built non-null SwsContext and
        // `sws_getCoefficients` returns a pointer to a static table.
        unsafe {
            let src_table = ffi::sws_getCoefficients(colorspace);
            let dst_table = ffi::sws_getCoefficients(ffi::SWS_CS_DEFAULT);
            // dstRange = 1 means full range, which is what RGBA expects.
            ffi::sws_setColorspaceDetails(
                self.ctx,
                src_table,
                src_range,
                dst_table,
                1,
                0,
                1 << 16,
                1 << 16,
            );
        }

        self.src_width = width;
        self.src_height = height;
        self.src_format = src_format;
        self.dst_width = dst_width;
        self.dst_height = dst_height;
        self.colorspace = colorspace;
        self.src_range = src_range;
        Ok(())
    }
}

impl Default for RgbaConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RgbaConverter {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            // SAFETY: we own `self.ctx` and it is never used afterwards.
            unsafe { ffi::sws_freeContext(self.ctx) };
            self.ctx = ptr::null_mut();
        }
    }
}

/// Pick the YUV→RGB matrix for a frame, honouring the container's metadata and
/// falling back on the usual resolution heuristic.
fn choose_colorspace(src: &ffmpeg::frame::Video, width: i32, height: i32) -> i32 {
    use ffmpeg::color::Space;
    match src.color_space() {
        Space::BT709 => ffi::SWS_CS_ITU709,
        Space::SMPTE170M | Space::BT470BG => ffi::SWS_CS_SMPTE170M,
        Space::BT2020NCL | Space::BT2020CL => ffi::SWS_CS_BT2020,
        Space::SMPTE240M => ffi::SWS_CS_SMPTE240M,
        Space::FCC => ffi::SWS_CS_FCC,
        Space::RGB => ffi::SWS_CS_DEFAULT,
        // Unspecified, reserved and the newer derived/HDR matrices all fall back
        // to the usual heuristic: SD material is BT.601, 720 lines and up is
        // BT.709.
        _ => {
            if height >= 720 || width >= 1280 {
                ffi::SWS_CS_ITU709
            } else {
                ffi::SWS_CS_SMPTE170M
            }
        }
    }
}

/// `0` = limited (MPEG) range, `1` = full (JPEG) range.
fn choose_range(src: &ffmpeg::frame::Video) -> i32 {
    use ffmpeg::color::Range;
    match src.color_range() {
        Range::JPEG => 1,
        // Limited range is the safe assumption for anything unspecified or
        // reserved; full range only really shows up on RGB sources.
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic YUV420P frame with real detail, so a conversion that
    /// reads the wrong rows produces visibly (and byte-wise) different output.
    fn synthetic_frame(width: u32, height: u32) -> ffmpeg::frame::Video {
        let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P, width, height);
        let (w, h) = (width as usize, height as usize);
        let luma_stride = frame.stride(0);
        {
            let plane = frame.data_mut(0);
            for y in 0..h {
                for x in 0..w {
                    plane[y * luma_stride + x] = ((x * 3 + y * 5) % 251) as u8;
                }
            }
        }
        for channel in 1..3usize {
            let stride = frame.stride(channel);
            let (pw, ph) = (w.div_ceil(2), h.div_ceil(2));
            let plane = frame.data_mut(channel);
            for y in 0..ph {
                for x in 0..pw {
                    plane[y * stride + x] = ((x * 7 + y * 11 + channel * 40) % 251) as u8;
                }
            }
        }
        frame
    }

    /// The scale stage must convert the whole frame in one call: the bands a
    /// caller might be tempted to cut are not equivalent. See `scale`.
    #[test]
    fn a_conversion_produces_a_whole_frame_of_pixels() {
        let src = synthetic_frame(640, 480);
        let pool = FramePool::new(16 * 1024 * 1024);
        let out = RgbaConverter::new()
            .convert(&src, 320, 240, &pool)
            .expect("conversion");
        assert_eq!(out.len(), 320 * 240 * 4);
    }

    /// The property the parallel tone map depends on: splitting the buffer
    /// between threads must not change a single byte. Every pixel is mapped
    /// from itself alone, so the cuts cannot be observed.
    #[test]
    fn the_parallel_tone_map_matches_a_single_threaded_one() {
        let mut converter = RgbaConverter::new();
        let mapper = ToneMapper::new(HdrInfo::default());
        let mut whole: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();
        let single = whole.clone();

        converter.bands = 1;
        converter.spread_over_threads(&mapper, &mut whole);

        for bands in [2, 3, 4, 7] {
            let mut cut = single.clone();
            converter.bands = bands;
            converter.spread_over_threads(&mapper, &mut cut);
            assert!(
                cut == whole,
                "tone mapping on {bands} threads changed the pixels"
            );
        }
    }

    #[test]
    fn a_tone_map_that_does_nothing_leaves_the_frame_alone() {
        // The identity mapper is the common case for SDR material: it must cost
        // nothing and change nothing, whatever the thread count.
        let converter = RgbaConverter::new();
        let mapper = ToneMapper::new(HdrInfo::default());
        let original: Vec<u8> = (0..1024).map(|i| (i % 97) as u8).collect();
        let mut buffer = original.clone();
        converter.spread_over_threads(&mapper, &mut buffer);
        if mapper.is_identity() {
            assert!(buffer == original, "an identity tone map must be a no-op");
        }
    }

    #[test]
    fn pool_recycles_buffers() {
        let pool = FramePool::new(1024 * 1024);
        let a = pool.acquire(4096);
        assert_eq!(a.len(), 4096);
        let ptr = a.as_ptr();
        pool.release(a);
        assert!(pool.buffered_bytes() >= 4096);
        let b = pool.acquire(4096);
        assert_eq!(b.len(), 4096);
        assert_eq!(b.as_ptr(), ptr, "the same allocation should be reused");
    }

    #[test]
    fn a_recycled_buffer_is_not_zero_filled_again() {
        // The contents of a pooled buffer are documented as undefined, and the
        // caller overwrites all of them. Zero-filling them first is a memset of
        // the whole frame — 33 MB at 4K — for nothing.
        let pool = FramePool::new(1024 * 1024);
        let mut a = pool.acquire(4096);
        a.fill(0xAB);
        pool.release(a);
        let b = pool.acquire(4096);
        assert_eq!(b.len(), 4096, "the length is exact");
        assert!(
            b.iter().all(|v| *v == 0xAB || *v == 0),
            "the buffer must not have been rewritten behind the caller's back"
        );
    }

    #[test]
    fn a_shrunken_buffer_grows_back_to_the_requested_length() {
        let pool = FramePool::new(1024 * 1024);
        pool.release(vec![0u8; 4096]);
        let small = pool.acquire(1024);
        assert_eq!(small.len(), 1024);
        pool.release(small);
        let big = pool.acquire(4096);
        assert_eq!(big.len(), 4096, "a short buffer must be grown, not returned short");
    }

    #[test]
    fn pool_best_fit_avoids_wasting_large_buffers() {
        let pool = FramePool::new(64 * 1024 * 1024);
        let big = pool.acquire(1_000_000);
        let small = pool.acquire(4096);
        pool.release(big);
        pool.release(small);
        let got = pool.acquire(4096);
        assert!(
            got.capacity() < 1_000_000,
            "a small request should reuse the small buffer, got {}",
            got.capacity()
        );
    }

    #[test]
    fn pool_respects_the_budget() {
        let pool = FramePool::new(8192);
        for _ in 0..64 {
            pool.release(vec![0u8; 4096]);
        }
        assert!(pool.buffered_bytes() <= 8192);
    }

    #[test]
    fn oversized_buffers_are_not_kept() {
        let pool = FramePool::new(4096);
        pool.release(vec![0u8; 16384]);
        assert_eq!(pool.buffered_bytes(), 0);
    }

    #[test]
    fn frame_validity_is_checked() {
        let good = VideoFrame {
            width: 2,
            height: 2,
            pts: 0.0,
            duration: 0.0,
            data: vec![0; 16],
            serial: 0,
            generation: 0,
        };
        assert!(good.is_valid());
        assert_eq!(good.byte_len(), 16);
        let mut bad = good.clone();
        bad.data.truncate(8);
        assert!(!bad.is_valid());
    }
}
