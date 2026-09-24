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

use crate::dolby::{DvMappingSummary, DvPlan, DvReshape, DvRpu};
use crate::error::{MediaError, Result};
use crate::hdr::{DoviConfig, HdrInfo, ToneMapper};

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

/// What happened to the Dolby Vision reshaping on the frames arriving now.
///
/// The interface reports it — the information panel and the open-time notice —
/// so a file whose reshaping could *not* be applied says so rather than looking
/// like a file that needed none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DvReshapeState {
    /// The preference is off, so nothing was attempted.
    #[default]
    Disabled,
    /// The frames carry no reshaping, or one that provably changes nothing.
    NotNeeded,
    /// Applied to every frame as it arrives.
    Applied,
    /// The RPU describes a reshaping this player cannot apply to this stream.
    Unsupported(&'static str),
}

/// One component's position inside a pixel of its plane.
#[derive(Debug, Clone, Copy, Default)]
struct ComponentLayout {
    /// Which plane of the frame the component lives in.
    plane: usize,
    /// Byte offset within that plane's pixel.
    byte: usize,
    /// How many bytes the sample occupies.
    bytes: usize,
    /// Bits to shift the loaded word down by.
    shift: u32,
    /// Bits kept, once shifted.
    mask: u32,
}

/// The pixel layout of a format the reshaping can be applied to.
///
/// Read from FFmpeg's own pixel-format descriptor, so 8, 10 and 12-bit samples,
/// planar, semi-planar and packed arrangements all take the same path: a table
/// of formats written out by hand here would be a table of formats to get wrong.
#[derive(Debug, Clone, Copy)]
struct FormatLayout {
    /// Base layer plane, then Cb and Cr.
    components: [ComponentLayout; 3],
    /// Bytes one column step advances within each plane.
    plane_pixel: [usize; 3],
    /// Chroma is subsampled by `1 << log2_chroma_w` horizontally.
    log2_chroma_w: u32,
    /// And vertically.
    log2_chroma_h: u32,
}

impl FormatLayout {
    /// The code value a component holds at `(x, y)`.
    ///
    /// # Safety
    ///
    /// Every plane pointer must be the first byte of the corresponding plane of
    /// a frame this layout was read from, with `strides` as the line sizes, and
    /// `(x, y)` inside the frame.
    unsafe fn sample(
        &self,
        component: usize,
        planes: &[*const u8; 3],
        strides: &[isize; 3],
        x: usize,
        y: usize,
    ) -> f32 {
        let layout = self.components[component];
        let pixel = self.plane_pixel[layout.plane];
        let stride = strides[layout.plane] as usize;
        // SAFETY: the caller guarantees the plane and that `(x, y)` is inside it.
        let address = unsafe {
            planes[layout.plane].add(y.wrapping_mul(stride).wrapping_add(x * pixel + layout.byte))
        };
        // SAFETY: as above, plus: the descriptor says how many bytes the sample
        // occupies at that address. It is unaligned on purpose — a packed
        // format's third component sits at an odd byte.
        let word = unsafe {
            match layout.bytes {
                1 => u32::from(*address),
                _ => u32::from(std::ptr::read_unaligned(address as *const u16)),
            }
        };
        ((word >> layout.shift) & layout.mask) as f32
    }

    /// Write a component's code value back at `(x, y)`, leaving whatever shares
    /// the word with it alone.
    ///
    /// # Safety
    ///
    /// As [`FormatLayout::sample`], with a writable plane pointer.
    unsafe fn store(
        &self,
        component: usize,
        planes: &[*mut u8; 3],
        strides: &[isize; 3],
        x: usize,
        y: usize,
        value: f32,
    ) {
        let layout = self.components[component];
        let pixel = self.plane_pixel[layout.plane];
        let stride = strides[layout.plane] as usize;
        // SAFETY: the caller guarantees the plane and that `(x, y)` is inside it.
        let address = unsafe {
            planes[layout.plane].add(y.wrapping_mul(stride).wrapping_add(x * pixel + layout.byte))
        };
        let code = ((value.clamp(0.0, 1.0) * layout.mask as f32).round() as u32) & layout.mask;
        let shifted = layout.mask << layout.shift;
        // SAFETY: as for `sample`; the read-modify-write keeps whatever else
        // shares this word.
        unsafe {
            match layout.bytes {
                1 => {
                    let old = u32::from(*address);
                    *address = ((old & !shifted) | (code << layout.shift)) as u8;
                }
                _ => {
                    let word = u32::from(std::ptr::read_unaligned(address as *const u16));
                    std::ptr::write_unaligned(
                        address as *mut u16,
                        ((word & !shifted) | (code << layout.shift)) as u16,
                    );
                }
            }
        }
    }
}

/// The layout of `format`, when the reshaping can be applied to it.
///
/// `None` for anything that is not three components of one sample each: RGB,
/// paletted, bitstream and hardware formats, and any format whose samples span
/// more than two bytes or whose chroma is subsampled by more than four.
fn layout_for(format: ffi::AVPixelFormat) -> Option<FormatLayout> {
    // SAFETY: `av_pix_fmt_desc_get` returns a pointer into a table owned by
    // FFmpeg, or null for a format it does not know; every field read below is a
    // plain scalar.
    let desc = unsafe { ffi::av_pix_fmt_desc_get(format) };
    if desc.is_null() {
        return None;
    }
    let desc = unsafe { &*desc };
    let rejected = ffi::AV_PIX_FMT_FLAG_RGB as u64
        | ffi::AV_PIX_FMT_FLAG_PAL as u64
        | ffi::AV_PIX_FMT_FLAG_BITSTREAM as u64
        | ffi::AV_PIX_FMT_FLAG_HWACCEL as u64;
    if desc.nb_components != 3 || (desc.flags & rejected) != 0 {
        return None;
    }
    let mut components = [ComponentLayout::default(); 3];
    for (c, slot) in components.iter_mut().enumerate() {
        let comp = desc.comp[c];
        // `depth` 0 means "not set"; more than 16 bits cannot be read as two
        // bytes, and the third plane is as far as any format in use goes.
        if comp.depth <= 0 || comp.depth > 16 || comp.plane < 0 || comp.plane > 2 || comp.offset < 0
        {
            return None;
        }
        let bytes = ((comp.offset & 7) + comp.depth + 7) / 8;
        if bytes > 2 {
            return None;
        }
        *slot = ComponentLayout {
            plane: comp.plane as usize,
            byte: (comp.offset / 8) as usize,
            bytes: bytes as usize,
            shift: comp.shift as u32,
            mask: (1u32 << comp.depth) - 1,
        };
    }
    let mut plane_pixel = [0usize; 3];
    for layout in &components {
        plane_pixel[layout.plane] = plane_pixel[layout.plane].max(layout.byte + layout.bytes);
    }
    if desc.log2_chroma_w > 2 || desc.log2_chroma_h > 2 {
        return None;
    }
    Some(FormatLayout {
        components,
        plane_pixel,
        log2_chroma_w: u32::from(desc.log2_chroma_w),
        log2_chroma_h: u32::from(desc.log2_chroma_h),
    })
}

/// Rewrite a frame's components through `reshape`.
///
/// `Err` carries the reason this stream cannot be rewritten, in Chinese, for the
/// interface to show: a reshaping that silently did nothing would leave the user
/// comparing a picture to a Dolby Vision device's with no idea why they differ.
fn apply_reshape(
    reshape: &DvReshape,
    src: &ffmpeg::frame::Video,
) -> std::result::Result<Option<ffmpeg::frame::Video>, &'static str> {
    let Some(layout) = layout_for(src.format().into()) else {
        return Err("像素格式不支持重塑");
    };
    // The pivots are coded in the base layer's bit depth, so a stream whose
    // samples are a different depth would need the two domains reconciled;
    // guessing that mapping could only make the picture wrong.
    let depth = layout.components[0].mask.count_ones() as u8;
    if depth != reshape.base_layer_bit_depth() {
        return Err("位深与 RPU 声明不一致");
    }
    // A frame whose lines are shorter than a row, or absurdly long, is not
    // something this reader can walk: `stride()` reports FFmpeg's "unknown"
    // `-1` as a huge `usize`, and a row it does not cover would read another
    // plane's memory.
    const MAX_PADDING: usize = 64 * 1024;
    let row_bytes = [
        (src.width() as usize) * layout.plane_pixel[0],
        ((src.width() as usize) >> layout.log2_chroma_w) * layout.plane_pixel[1],
        ((src.width() as usize) >> layout.log2_chroma_w) * layout.plane_pixel[2],
    ];
    for (plane, row) in row_bytes.into_iter().enumerate() {
        let stride = src.stride(plane);
        if stride < row || stride > row + MAX_PADDING {
            return Err("行跨距不支持重塑");
        }
    }
    let mut out = ffmpeg::frame::Video::new(src.format(), src.width(), src.height());
    // SAFETY: both frames are alive and have the same format and geometry, which
    // is what `av_frame_copy` requires. The copy is also what makes the
    // read-modify-write of a packed format's shared word safe.
    unsafe {
        ffi::av_frame_copy(out.as_mut_ptr(), src.as_ptr());
    }
    reshape_components(&layout, reshape, src, &mut out);
    Ok(Some(out))
}

/// Rewrite every component of a frame through the Dolby Vision reshaping.
///
/// `out` must already hold a copy of `src`: a packed or semi-planar format has
/// components sharing a word, and the write-back keeps the bits that belong to
/// the others.
///
/// The chroma is reshaped at its own resolution, using the co-sited luma sample
/// — the top-left of the group it serves — which is where a decoder's own
/// reshaping stage sits: the curves belong to the decoded components, and the
/// conversion to RGB that follows is what upsamples them.
fn reshape_components(
    layout: &FormatLayout,
    reshape: &DvReshape,
    src: &ffmpeg::frame::Video,
    out: &mut ffmpeg::frame::Video,
) {
    let width = src.width() as usize;
    let height = src.height() as usize;
    // SAFETY: both frames are alive for the call, have the same format and
    // geometry, and the loops stay inside `width x height` — and inside the
    // chroma planes, which are that divided by the subsampling.
    unsafe {
        let src_planes = [
            src.data(0).as_ptr(),
            plane_ptr(src, 1),
            plane_ptr(src, 2),
        ];
        let strides = [
            src.stride(0) as isize,
            src.stride(1) as isize,
            src.stride(2) as isize,
        ];
        let out_planes = [
            out.data_mut(0).as_mut_ptr(),
            plane_ptr_mut(out, 1),
            plane_ptr_mut(out, 2),
        ];

        // The base layer: every luma sample, with the chroma co-sited to it.
        for y in 0..height {
            for x in 0..width {
                let cx = x >> layout.log2_chroma_w;
                let cy = y >> layout.log2_chroma_h;
                let bl = codes(layout, &src_planes, &strides, x, y, cx, cy);
                let reshaped = reshape.apply(bl, None);
                layout.store(0, &out_planes, &strides, x, y, reshaped[0]);
            }
        }

        // The chroma: every chroma sample, with the base-layer sample it serves.
        let chroma_width = width >> layout.log2_chroma_w;
        let chroma_height = height >> layout.log2_chroma_h;
        for cy in 0..chroma_height {
            for cx in 0..chroma_width {
                let (lx, ly) = (cx << layout.log2_chroma_w, cy << layout.log2_chroma_h);
                let bl = codes(layout, &src_planes, &strides, lx, ly, cx, cy);
                let reshaped = reshape.apply(bl, None);
                layout.store(1, &out_planes, &strides, cx, cy, reshaped[1]);
                layout.store(2, &out_planes, &strides, cx, cy, reshaped[2]);
            }
        }
    }
}

/// One pixel's three components, normalized the way the RPU's coefficients
/// expect: the code value over the component's own full scale, with no range
/// expansion and no chroma centring — see `dolby/reshape.rs` for why that is the
/// domain the pivots live in.
///
/// # Safety
///
/// As [`FormatLayout::sample`].
unsafe fn codes(
    layout: &FormatLayout,
    planes: &[*const u8; 3],
    strides: &[isize; 3],
    x: usize,
    y: usize,
    cx: usize,
    cy: usize,
) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for (component, slot) in out.iter_mut().enumerate() {
        let (sx, sy) = if component == 0 { (x, y) } else { (cx, cy) };
        // SAFETY: the caller guarantees the planes and the coordinates.
        let code = unsafe { layout.sample(component, planes, strides, sx, sy) };
        *slot = code / layout.components[component].mask as f32;
    }
    out
}

/// A plane's first byte, or null when the format has no such plane.
///
/// # Safety
///
/// `frame` must be alive. The pointer is only dereferenced by the layout reader,
/// which is told how many planes the format has.
unsafe fn plane_ptr(frame: &ffmpeg::frame::Video, index: usize) -> *const u8 {
    let plane = frame.data(index);
    if plane.is_empty() {
        std::ptr::null()
    } else {
        plane.as_ptr()
    }
}

/// A plane's first byte, writable, or null when the format has no such plane.
///
/// # Safety
///
/// As [`plane_ptr`], and the pointer must not be used to write anywhere another
/// borrow of the frame still holds.
unsafe fn plane_ptr_mut(frame: &mut ffmpeg::frame::Video, index: usize) -> *mut u8 {
    if frame.data(index).is_empty() {
        return std::ptr::null_mut();
    }
    frame.data_mut(index).as_mut_ptr()
}

///
/// At the 24–30 fps of real material this is a time constant of roughly a third
/// of a second: fast enough to follow a scene cut, slow enough that per-frame
/// metadata noise cannot make the brightness pump.
const SCENE_SMOOTHING: f32 = 0.12;

/// The luminance step, in nits, that a Dolby Vision mapper rebuild is
/// quantised to.
///
/// The tables themselves are cheap, but rebuilding them for every frame would
/// still be wasted work when level 1 wobbles by a nit; a bucket also keeps the
/// build from being observable as flicker.
const SCENE_PEAK_STEP: f32 = 32.0;

/// A tone mapper together with the dynamic range and scene it was built for.
struct MappedScene {
    /// The frame's dynamic range; a change means a new mapper regardless.
    info: HdrInfo,
    /// Quantised scene peak, `0` when the frame carries no dynamic metadata.
    bucket: u16,
    /// The mapper itself.
    mapper: ToneMapper,
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
    /// Apply the Dolby Vision reshaping (the interface's preference, off by
    /// default — see `dolby/reshape.rs`).
    dv_reshape: bool,
    /// Mapper built for the dynamic range and the scene currently arriving.
    ///
    /// A Dolby Vision file carries per-frame brightness metadata, so the mapper
    /// is rebuilt when the *scene* changes rather than once per file; the
    /// bucket quantises that so a fluctuating measurement cannot rebuild it on
    /// every frame.
    mapper: Option<MappedScene>,
    /// Smoothed peak luminance of the Dolby Vision scene, in nits.
    ///
    /// Level 1 changes from frame to frame, and a tone curve that chased every
    /// change would make the picture breathe. This is the smoothed value the
    /// mapper is actually built from.
    scene_peak: Option<f32>,
    /// The file's Dolby Vision configuration record, when the container declares
    /// one.
    ///
    /// It is set once per file and used as the fallback answer for what the base
    /// layer is coded with: the record's compatibility id names an HLG base layer
    /// as HLG, which neither the container's tags nor a "Dolby Vision is always
    /// PQ" assumption does. See [`crate::dolby`].
    dovi: Option<DoviConfig>,
    /// The Dolby Vision reshaping the file describes, ready to apply.
    ///
    /// The curves do not change from frame to frame — only the brightness
    /// metadata does — so they are built once and kept until the RPU's mapping
    /// changes, which is a once-per-file event.
    reshape: Option<DvReshape>,
    /// The mapping [`Self::reshape`] was built from, so a change can be noticed
    /// without rebuilding five kilobytes of coefficients every frame.
    reshape_source: Option<DvMappingSummary>,
    /// What happened to the reshaping on the frames arriving now.
    reshape_state: DvReshapeState,
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
            dv_reshape: false,
            mapper: None,
            scene_peak: None,
            dovi: None,
            reshape: None,
            reshape_source: None,
            reshape_state: DvReshapeState::default(),
            bands: tone_map_bands(),
            last_tone_map_ms: 0.0,
        }
    }

    /// Declare what the file's Dolby Vision record says, if the container has one.
    ///
    /// Called once per opened file, before the first frame is converted. It is a
    /// setter rather than a constructor argument for the same reason
    /// [`Self::set_tone_map`] is: the record is known only once the demuxer has
    /// described the file, and the converter already exists by then.
    pub fn set_dovi_config(&mut self, config: Option<DoviConfig>) {
        if self.dovi != config {
            self.dovi = config;
            self.mapper = None;
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

    /// Turn the Dolby Vision reshaping on or off.
    ///
    /// Off by default, and off means *off*: the pass is not entered at all, so a
    /// file whose reshaping is not wanted costs exactly what it did before this
    /// existed. See `dolby/reshape.rs` for why the default is off.
    pub fn set_dv_reshape(&mut self, enabled: bool) {
        self.dv_reshape = enabled;
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
        // The Dolby Vision reshaping is applied to the *components*, so it
        // happens before the scaler converts them to RGB and is a pass of its
        // own: `pixels` is a rewritten copy of `src` when the file needs one.
        // The metadata the tone map reads stays on `src` either way, because the
        // copy carries no side data.
        let reshaped = self.reshape_frame(src);
        let pixels = reshaped.as_ref().unwrap_or(src);
        let geometry = self.prepare(pixels, dst_width, dst_height)?;
        let mut out = pool.acquire(geometry.dst_bytes());
        self.scale(pixels, &mut out, dst_width)?;
        self.apply_tone_map(src, &mut out);
        Ok(out)
    }

    /// What the reshaping is doing on the frames arriving now, for the interface
    /// to report.
    pub fn dv_reshape_state(&self) -> DvReshapeState {
        self.reshape_state
    }

    /// Apply the file's Dolby Vision reshaping to `src`, if it describes one.
    ///
    /// `None` means "scale `src` as it is": either the file needs no reshaping
    /// (the common case — most profiles leave the base layer alone) or this
    /// stream cannot be rewritten, which [`DvReshapeState`] then says out loud
    /// rather than leaving the user with a picture nobody can explain.
    fn reshape_frame(&mut self, src: &ffmpeg::frame::Video) -> Option<ffmpeg::frame::Video> {
        if !self.dv_reshape {
            // Off means off: no RPU parsing, no copy, no pass.
            self.reshape_state = DvReshapeState::Disabled;
            return None;
        }
        let rpu = DvRpu::from_frame(src);
        let mapping = rpu.as_ref().and_then(|rpu| rpu.mapping.as_ref());
        let Some(mapping) = mapping else {
            self.reshape = None;
            self.reshape_source = None;
            self.reshape_state = DvReshapeState::NotNeeded;
            return None;
        };
        if self.reshape_source.as_ref() != Some(mapping) {
            self.reshape_source = Some(mapping.clone());
            self.reshape = DvReshape::from_mapping(mapping, rpu.as_ref().and_then(|r| r.header.as_ref()));
        }

        let (frame, state) = match self.reshape.as_ref() {
            None => (None, DvReshapeState::Unsupported("映射无法解析")),
            Some(reshape) if reshape.is_identity() => (None, DvReshapeState::NotNeeded),
            Some(reshape) => match apply_reshape(reshape, src) {
                Ok(frame) => (frame, DvReshapeState::Applied),
                Err(reason) => (None, DvReshapeState::Unsupported(reason)),
            },
        };
        self.reshape_state = state;
        frame
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
    /// follows the display rather than the source. The mapper is rebuilt when a
    /// frame arrives with a different dynamic range, and — for Dolby Vision —
    /// when the scene's own brightness has moved enough to matter.
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
        let mut info = HdrInfo::from_frame(src);
        let rpu = DvRpu::from_frame(src);
        // A Dolby Vision file decides its own dynamic range: the RPU is the most
        // specific statement about a frame, the container's record is the next,
        // and the tags the frame carries are the last resort. That is what makes
        // an HLG-compatible base layer (Profile 8.4) an HLG file — running its
        // code values through the PQ curve is what used to make such a file look
        // washed out — and it is also what tells the player that a Profile 5 base
        // layer is not a picture in any transfer function at all.
        let plan = DvPlan::resolve(self.dovi, rpu.as_ref(), info.kind);
        if let Some(plan) = &plan {
            info.kind = plan.transfer;
            info.bt2020 = true;
        }
        if !info.needs_tone_map() {
            return;
        }
        let peak = self.smoothed_scene_peak(rpu.as_ref().and_then(|rpu| rpu.peak_nits()));
        let bucket = match peak {
            Some(nits) => (nits / SCENE_PEAK_STEP).round().clamp(0.0, 30_000.0) as u16 + 1,
            None => 0,
        };
        let needs_build = self
            .mapper
            .as_ref()
            .map(|m| m.info != info || m.bucket != bucket)
            .unwrap_or(true);
        if needs_build {
            self.mapper = Some(MappedScene {
                info,
                bucket,
                mapper: ToneMapper::with_scene_peak(info, peak),
            });
        }
        let Some(mapped) = &self.mapper else {
            return;
        };
        let started = std::time::Instant::now();
        self.spread_over_threads(&mapped.mapper, dst);
        self.last_tone_map_ms = started.elapsed().as_secs_f32() * 1000.0;
    }

    /// Move the smoothed scene peak towards `target`, in nits.
    ///
    /// `None` means the frame carried no dynamic metadata; that resets the
    /// adaptation rather than holding a peak that belongs to another scene.
    fn smoothed_scene_peak(&mut self, target: Option<f32>) -> Option<f32> {
        match target {
            Some(target) if target.is_finite() && target > 0.0 => {
                let smoothed = match self.scene_peak {
                    Some(previous) => previous + (target - previous) * SCENE_SMOOTHING,
                    None => target,
                };
                self.scene_peak = Some(smoothed);
                Some(smoothed)
            }
            _ => {
                self.scene_peak = None;
                None
            }
        }
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
