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
            buf.clear();
            buf.resize(len, 0);
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
        }
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
        let width = src.width() as i32;
        let height = src.height() as i32;
        if width <= 0 || height <= 0 {
            return Err(MediaError::other("解码帧尺寸无效"));
        }
        let dst_width = dst_width.max(1) as i32;
        let dst_height = dst_height.max(1) as i32;
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

        let len = dst_width as usize * dst_height as usize * 4;
        let mut out = pool.acquire(len);
        let strides = [dst_width * 4, 0, 0, 0];

        // SAFETY: `self.ctx` is a live SwsContext built for exactly these
        // dimensions, formats and colour settings. `src` is a live AVFrame and
        // FFmpeg guarantees its planes stay valid for the duration of the call.
        // The destination points at `out`, which is exactly `len` bytes, and the
        // matching stride describes it.
        let written = unsafe {
            let src_frame = src.as_ptr();
            let dst_data = [
                out.as_mut_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            ];
            ffi::sws_scale(
                self.ctx,
                (*src_frame).data.as_ptr() as *const *const u8,
                (*src_frame).linesize.as_ptr(),
                0,
                height,
                dst_data.as_ptr(),
                strides.as_ptr(),
            )
        };

        if written <= 0 {
            return Err(MediaError::other("视频帧色彩转换失败"));
        }
        Ok(out)
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
        assert_eq!(b.iter().filter(|v| **v == 0).count(), 4096);
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
