//! Dolby Vision: what a reference picture unit says about a frame, and what this
//! player does with it.
//!
//! # What a Dolby Vision file carries
//!
//! Two things, and they arrive by different routes.
//!
//! * The container declares a **configuration record** — profile, level, whether
//!   an enhancement layer and a reference picture unit (RPU) are present, and a
//!   *compatibility id* that says what the base layer is: an HDR10 picture, an
//!   SDR one, an HLG one, or nothing a plain decoder could show. That record is
//!   [`DoviConfig`] and it is read from the stream, once per file.
//! * Every frame carries an **RPU**. Since FFmpeg 7.1 the HEVC decoder parses it
//!   and attaches it as `AV_FRAME_DATA_DOVI_METADATA` on the decoded frame,
//!   which is what makes this module possible at all: before that a player had
//!   to find the RPU NAL units and decode the bitstream syntax itself. An RPU
//!   holds the *header* (bit depths, whether the enhancement layer is used for
//!   residuals), the *mapping* (the piecewise polynomial and MMR reshaping
//!   curves plus non-linear inverse quantisation), the *colour metadata* (the
//!   matrices Dolby Vision defines for its own IPT-PQ space, the EOTF the signal
//!   is coded with, and the master's luminance window) and the
//!   display-management extension blocks — levels 1, 2, 5 and 6 here.
//!
//! # What is done with it
//!
//! * **The base layer's transfer function is taken from Dolby Vision, not from
//!   the container.** The compatibility id is authoritative and it is *not*
//!   always PQ: a file whose base layer is HLG-compatible (`4`) is an HLG file,
//!   and running its code values through the PQ curve turns a graded picture
//!   into a washed-out one. See [`DoviConfig::base_layer_kind`].
//! * **The tone curve is aimed at the scene.** Level 1 gives each frame's
//!   blackest, average and brightest sample; the mapper is built for that range,
//!   smoothed over a third of a second so a fluctuating measurement cannot make
//!   the picture breathe (see `video.rs`).
//! * **Everything else is read, reported, and left alone** — see below.
//!
//! # The reshaping
//!
//! The base layer is not the master: it is the master pushed through a
//! *reshaping* the encoder chose, and the RPU describes how to undo it — a
//! piecewise polynomial for the luma, an MMR fit for the chroma, and the
//! parameters of the residual a Profile 7 enhancement layer contributes.
//! [`crate::dolby::reshape`] implements that, and it is **off by default**:
//! the luma half checks out against libplacebo, the chroma half does not yet,
//! and the module docs there record exactly what was measured. The setting
//! (`dv_reshape`) exists so that turning it on is a decision rather than a
//! surprise, and so that the next round of work has an A/B harness.
//!
//! # What is deliberately not done
//!
//! Each of these was considered and rejected for a reason, so that nobody has to
//! re-discover it:
//!
//! * **The enhancement layer is not decoded.** Profile 7 keeps the extra
//!   highlight detail in a second video stream (dual-track) or in the same
//!   bitstream as extra NAL units (single-track dual-layer), and the residual it
//!   carries needs the RPU's non-linear inverse quantisation to be composed —
//!   [`crate::dolby::reshape`] implements and unit-tests that composition, but
//!   nothing feeds it a frame: the demuxer opens one video stream, and *no
//!   Dolby Vision file on this machine has an enhancement layer at all*
//!   (`el_present` is false in every sample), so the acquisition path could be
//!   written but not verified. That is the next step, and it needs material.
//! * **Profile 5 is not converted.** Its base layer is coded in IPT (ICtCp)
//!   rather than in BT.2020 RGB, and only an inverse of the Dolby Vision mapping
//!   restores it; the RPU's own matrices for that are read here
//!   ([`DvColorMetadata::rgb_to_lms`]) but the transform is not run, for the same
//!   reason as the reshaping: unverified colour maths is worse than an honest
//!   approximation. The file plays, it is tone mapped, and the player says the
//!   colours may be wrong.
//! * **No metadata reaches the display.** Handing an RPU to a Dolby Vision
//!   display is a bitstream the operating system, the driver and the sink all
//!   have to agree to carry. On Windows that agreement belongs to DXGI advanced
//!   colour and to the system media stack; an OpenGL window has no way to join
//!   in. What the player *can* do — and what its "no tone mapping" setting does
//!   — is leave the base layer's own signal untouched, so that whatever comes
//!   after the window, if it understands PQ or HLG, receives the original.
//!
//! # Seeing what a file carries
//!
//! ```text
//! cargo run -p mvp-core --example dv_probe -- <file>
//! ```
//!
//! [`probe_rpus`] is the reader behind it and the opt-in `dolby_vision`
//! integration test uses the same reader: a real Dolby Vision sample is far too
//! large to keep in `testdata/`, so those tests take one from the environment.

use std::path::Path;

use ffmpeg::ffi;
use ffmpeg_next as ffmpeg;

use crate::error::{MediaError, Result};
use crate::hdr::{pq_code_to_nits, DoviConfig, HdrKind};

mod reshape;

pub use reshape::DvReshape;

/// `AV_DOVI_MAX_EXT_BLOCKS`, from `libavutil/dovi_meta.h`.
pub const DV_MAX_EXT_BLOCKS: usize = 32;

/// `AV_DOVI_MAX_PIECES`: the most pieces one reshaping curve can have.
pub const DV_MAX_PIECES: usize = 8;

// ---------------------------------------------------------------------------
// The C layout, mirrored
// ---------------------------------------------------------------------------
//
// `libavutil/dovi_meta.h` is not in the bindings `ffmpeg-sys-next` generates and
// its accessors are `static inline`, so the layout is mirrored by hand — the same
// thing `hdr.rs` does for the container's record. Every offset the code reads is
// asserted against the header's own field order in the tests below, and every
// read is bounded by the side data's size, so a mistake shows up as a failed test
// or a rejected frame rather than as bytes read out of somebody else's memory.

/// `AVDOVIRpuDataHeader`: the RPU's own header.
///
/// `bl_bit_depth` is the base layer's bit depth, `vdr_bit_depth` the full-range
/// signal the mapping describes, and `disable_residual_flag` says the
/// enhancement layer carries no residual — that the base layer really is the
/// whole picture.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DvRpuHeader {
    /// The RPU's type, as the bitstream codes it. Informative: the sample this
    /// was developed against carries `2`, and no meaning is assumed for it.
    pub rpu_type: u8,
    /// Bitstream format of the RPU itself. Informative.
    pub rpu_format: u16,
    /// Version of the RPU's own specification.
    pub vdr_rpu_profile: u8,
    /// Level of the RPU's own specification.
    pub vdr_rpu_level: u8,
    /// The base layer used an explicit chroma resampling filter.
    pub chroma_resampling_explicit_filter_flag: u8,
    /// How the coefficients are coded before libavcodec converts them to fixed
    /// point. Informative.
    pub coef_data_type: u8,
    /// Denominator exponent of the fixed-point coefficients (`2^n`).
    pub coef_log2_denom: u8,
    /// Whether the RPU's coefficients are normalised.
    pub vdr_rpu_normalized_idc: u8,
    /// The base layer is full range.
    pub bl_video_full_range_flag: u8,
    /// Base layer bit depth, `[8, 16]`.
    pub bl_bit_depth: u8,
    /// Enhancement layer bit depth, `[8, 16]`.
    pub el_bit_depth: u8,
    /// Bit depth of the full-range signal the mapping describes, `[8, 16]`.
    pub vdr_bit_depth: u8,
    /// Spatial resampling filter in use.
    pub spatial_resampling_filter_flag: u8,
    /// The enhancement layer's own spatial resampling filter.
    pub el_spatial_resampling_filter_flag: u8,
    /// `1` when the enhancement layer carries no residual.
    pub disable_residual_flag: u8,
    /// Extended base-layer inverse mapping indicator.
    pub ext_mapping_idc_0_4: u8,
    /// Reserved.
    pub ext_mapping_idc_5_7: u8,
}

/// `AVRational`, as the colour metadata codes its matrices.
///
/// They are exact ratios rather than numbers, which is why they are read as
/// ratios and converted only when something needs a value.
pub type DvRatio = ffi::AVRational;

/// `AVDOVIColorMetadata`: what the RPU says the signal *is*.
///
/// The matrices are Dolby Vision's own, and FFmpeg's header is explicit that they
/// are to be used instead of anything the container's colour tags say.
/// `rgb_to_lms` is applied after PQ linearisation, and its output belongs to a
/// BT.2020 LMS→RGB matrix (Hunt-Pointer-Estevez, no crosstalk — the definition of
/// the ICtCp space).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DvColorMetadata {
    /// Display-management metadata id.
    pub dm_metadata_id: u8,
    /// The scene changes at this frame; a renderer may reset its adaptation.
    pub scene_refresh_flag: u8,
    /// YCbCr → Dolby Vision's own space, before PQ linearisation.
    pub ycc_to_rgb_matrix: [DvRatio; 9],
    /// Offset of the neutral value in that conversion.
    pub ycc_to_rgb_offset: [DvRatio; 3],
    /// From that space to LMS, after PQ linearisation.
    pub rgb_to_lms_matrix: [DvRatio; 9],
    /// Transfer function the signal is coded with: `1` PQ, `2` HLG, `0` BT.1886,
    /// anything else unspecified.
    pub signal_eotf: u16,
    /// First parameter of that transfer function; PQ and HLG do not use it.
    pub signal_eotf_param0: u16,
    /// Second parameter.
    pub signal_eotf_param1: u16,
    /// Third parameter.
    pub signal_eotf_param2: u32,
    /// Bit depth of the signal the RPU describes.
    pub signal_bit_depth: u8,
    /// `0` YCbCr, `1` IPT (ICtCp).
    pub signal_color_space: u8,
    /// Chroma format of that signal.
    pub signal_chroma_format: u8,
    /// Full-range flag, `[0, 3]`.
    pub signal_full_range_flag: u8,
    /// Darkest code the master contains, as 12-bit PQ.
    pub source_min_pq: u16,
    /// Brightest code the master contains, as 12-bit PQ.
    pub source_max_pq: u16,
    /// Diagonal of the mastering display, in inches.
    pub source_diagonal: u16,
}

impl DvColorMetadata {
    /// The YCbCr → IPT/RGB matrix, as values.
    pub fn ycc_to_rgb(&self) -> [[f32; 3]; 3] {
        to_matrix(&self.ycc_to_rgb_matrix)
    }

    /// The matrix from that space to LMS, applied after PQ linearisation.
    pub fn rgb_to_lms(&self) -> [[f32; 3]; 3] {
        to_matrix(&self.rgb_to_lms_matrix)
    }

    /// Offset of the neutral value in the YCbCr conversion, as values.
    pub fn ycc_offset(&self) -> [f32; 3] {
        to_vector(&self.ycc_to_rgb_offset)
    }
}

/// One `AVRational` as a value. A zero denominator reads as `0.0` rather than as
/// an infinity every caller would have to guard against.
fn ratio(value: DvRatio) -> f32 {
    if value.den == 0 {
        0.0
    } else {
        value.num as f32 / value.den as f32
    }
}

/// Nine `AVRational`s, row-major, as a 3×3 matrix.
fn to_matrix(values: &[DvRatio; 9]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = ratio(values[i * 3 + j]);
        }
    }
    out
}

/// Three `AVRational`s as a vector.
fn to_vector(values: &[DvRatio; 3]) -> [f32; 3] {
    [ratio(values[0]), ratio(values[1]), ratio(values[2])]
}

/// `AVDOVIReshapingCurve`: the mapping of one component, piece by piece.
///
/// The pieces span the value ranges between two adjacent pivots, and each piece
/// is either a polynomial or an MMR fit. The coefficients are fixed point with
/// `2^coef_log2_denom` as the denominator (the header's `coef_log2_denom`), which
/// is why they are `i64` here and why nothing is evaluated: what this type is
/// used for is knowing *whether* a reshaping is present, and of which kind.
#[repr(C)]
pub struct DvReshapingCurve {
    /// Number of pivots, `[2, 9]`: two pivots are one piece.
    pub num_pivots: u8,
    /// Ascending pivot values, in base-layer code values.
    pub pivots: [u16; DV_MAX_PIECES + 1],
    /// `0` polynomial, `1` MMR, per piece.
    pub mapping_idc: [std::os::raw::c_int; DV_MAX_PIECES],
    /// Polynomial order per piece, `[1, 2]`.
    pub poly_order: [u8; DV_MAX_PIECES],
    /// Polynomial coefficients per piece: `x^0`, `x^1`, `x^2`.
    pub poly_coef: [[i64; 3]; DV_MAX_PIECES],
    /// MMR order per piece, `[1, 3]`.
    pub mmr_order: [u8; DV_MAX_PIECES],
    /// MMR constant term per piece.
    pub mmr_constant: [i64; DV_MAX_PIECES],
    /// MMR coefficients per piece: `[order - 1][7]`.
    pub mmr_coef: [[[i64; 7]; 3]; DV_MAX_PIECES],
}

/// `AVDOVINLQParams`: the non-linear inverse quantisation of one component.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DvNlqParams {
    /// Offset applied before the inverse quantisation.
    pub nlq_offset: u16,
    /// Largest value the full-range signal can take.
    pub vdr_in_max: u64,
    /// Slope of the linear dead zone (`AV_DOVI_NLQ_LINEAR_DZ`).
    pub linear_deadzone_slope: u64,
    /// Threshold of the linear dead zone.
    pub linear_deadzone_threshold: u64,
}

/// `AVDOVIDataMapping`: how the base layer is mapped back to the full range.
#[repr(C)]
pub struct DvDataMapping {
    /// Which full-range signal id this mapping produces.
    pub vdr_rpu_id: u8,
    /// Colour space the mapping is described in.
    pub mapping_color_space: u8,
    /// Chroma format the mapping is described in.
    pub mapping_chroma_format_idc: u8,
    /// One curve per component: luma first, then the two chroma components.
    pub curves: [DvReshapingCurve; 3],
    /// `AV_DOVI_NLQ_NONE` (`-1`) or `AV_DOVI_NLQ_LINEAR_DZ` (`0`).
    pub nlq_method_idc: std::os::raw::c_int,
    /// Horizontal partitions the mapping is described over.
    pub num_x_partitions: u32,
    /// Vertical partitions.
    pub num_y_partitions: u32,
    /// The non-linear inverse quantisation of each component.
    pub nlq: [DvNlqParams; 3],
    /// Pivots of the non-linear inverse quantisation.
    pub nlq_pivots: [u16; 2],
}

/// The two mapping methods a piece of a reshaping curve can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DvMappingMethod {
    /// A piecewise polynomial: a curve, described by its coefficients.
    Polynomial {
        /// Highest power the piece uses, `[1, 2]`.
        order: u8,
        /// How many pieces the component's curve has.
        pieces: u8,
    },
    /// Multivariate multiple regression, over the base layer *and* the
    /// enhancement layer's residuals.
    Mmr {
        /// Order of the fit, `[1, 3]`.
        order: u8,
        /// How many pieces the component's curve has.
        pieces: u8,
    },
    /// The component is not mapped.
    #[default]
    None,
}

impl DvMappingMethod {
    /// `true` for anything that is not an identity mapping.
    pub fn is_mapping(self) -> bool {
        !matches!(self, DvMappingMethod::None)
    }

    /// How many pieces the component's curve has; `0` for an unmapped component.
    pub fn pieces(self) -> u8 {
        match self {
            DvMappingMethod::Polynomial { pieces, .. } | DvMappingMethod::Mmr { pieces, .. } => {
                pieces
            }
            DvMappingMethod::None => 0,
        }
    }

    /// `true` when the component needs the enhancement layer's residuals, which
    /// is the one thing a single-stream player cannot supply.
    pub fn is_mmr(self) -> bool {
        matches!(self, DvMappingMethod::Mmr { .. })
    }
}

/// `signal_color_space`: the space the base layer's pixels are coded in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DvColorSpace {
    /// YCbCr — how every profile but 5 codes its base layer.
    #[default]
    Yuv,
    /// IPT (ICtCp) — how Profile 5 codes its base layer, and why that profile
    /// cannot be shown correctly without an inverse of the Dolby Vision mapping.
    Ipt,
}

impl DvColorSpace {
    /// `1` is IPT; anything else is read as YCbCr, which is what the samples and
    /// the specification agree on for every other profile.
    fn from_idc(idc: u8) -> Self {
        if idc == 1 {
            DvColorSpace::Ipt
        } else {
            DvColorSpace::Yuv
        }
    }

    /// Short label for the information panel.
    pub fn label(self) -> &'static str {
        match self {
            DvColorSpace::Yuv => "YCbCr",
            DvColorSpace::Ipt => "IPT (ICtCp)",
        }
    }
}

/// What the RPU says the decoded pixels are.
///
/// This is the part of the metadata that is *authoritative*: the container's
/// colour tags are frequently absent on a Dolby Vision stream, and where they are
/// present they describe the base layer as the encoder tagged it rather than as
/// the colourist graded it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DvSignal {
    /// The transfer function the signal is coded with, when the RPU names one.
    /// `None` means the RPU did not say, and the container's record decides.
    pub transfer: Option<HdrKind>,
    /// Which space the base layer's pixels are in.
    pub color_space: DvColorSpace,
    /// Bit depth of the signal described.
    pub bit_depth: u8,
    /// The signal is full range.
    pub full_range: bool,
    /// Darkest code the master contains, in nits, when the RPU says.
    pub source_min_nits: Option<f32>,
    /// Brightest code the master contains, in nits, when the RPU says.
    pub source_peak_nits: Option<f32>,
    /// Diagonal of the mastering display, in inches, when the RPU says.
    pub source_diagonal_inches: Option<u16>,
}

impl DvSignal {
    /// Read the signal description out of the colour metadata.
    fn from_color(color: &DvColorMetadata) -> Self {
        DvSignal {
            transfer: transfer_from_eotf(color.signal_eotf),
            color_space: DvColorSpace::from_idc(color.signal_color_space),
            bit_depth: color.signal_bit_depth,
            full_range: color.signal_full_range_flag != 0,
            source_min_nits: (color.source_min_pq > 0)
                .then(|| pq_code_to_nits(color.source_min_pq)),
            source_peak_nits: (color.source_max_pq > 0)
                .then(|| pq_code_to_nits(color.source_max_pq)),
            source_diagonal_inches: (color.source_diagonal > 0).then_some(color.source_diagonal),
        }
    }

    /// One line for the information panel: what the pixels are and what the
    /// master's range was.
    pub fn describe(&self) -> String {
        let mut parts = vec![self.color_space.label().to_string()];
        if let Some(transfer) = self.transfer {
            parts.push(transfer.label().to_string());
        }
        if self.bit_depth > 0 {
            parts.push(format!("{} 位", self.bit_depth));
        }
        if let Some(peak) = self.source_peak_nits {
            parts.push(format!(
                "母版 {:.0}–{:.0} 尼特",
                self.source_min_nits.unwrap_or(0.0),
                peak
            ));
        }
        parts.join(" · ")
    }
}

/// `signal_eotf`: what the RPU says the signal's transfer function is.
///
/// Anything the enumeration does not name is *not* guessed at: the samples that
/// were checked carry `0xFFFF` here, which is plainly a "not stated" rather than a
/// transfer function, and the container's record answers for those files.
fn transfer_from_eotf(eotf: u16) -> Option<HdrKind> {
    match eotf {
        // BT.1886.
        0 => Some(HdrKind::Sdr),
        // PQ (SMPTE ST 2084).
        1 => Some(HdrKind::Pq),
        // HLG.
        2 => Some(HdrKind::Hlg),
        _ => None,
    }
}

/// A frame's Dolby Vision reference picture unit, as far as this player reads it.
///
/// Everything is optional on purpose: each part of the record is reached through
/// an offset FFmpeg wrote into the metadata, and a record that puts one of them
/// outside its own buffer is refused part by part. A missing part is a fact about
/// the file — or about a build of FFmpeg that did not fill it in — rather than
/// something to guess around.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DvRpu {
    /// The RPU's own header.
    pub header: Option<DvRpuHeader>,
    /// What the RPU says the signal is.
    pub color: Option<DvColorMetadata>,
    /// The reshaping the base layer would need to be shown as it was graded.
    pub mapping: Option<DvMappingSummary>,
    /// Level 1: this frame's brightness range.
    pub level1: Option<DoviLevel1>,
    /// The first level 2 block: the trims for one target display.
    pub level2: Option<DoviLevel2>,
    /// How many level 2 blocks the frame carries, one per target display.
    pub level2_count: u8,
    /// Level 5: the active area.
    pub level5: Option<DoviLevel5>,
    /// Level 6: the static mastering metadata.
    pub level6: Option<DoviLevel6>,
}

impl DvRpu {
    /// Read the reference picture unit off a decoded frame, if it has one.
    ///
    /// `AV_FRAME_DATA_DOVI_METADATA` is what the HEVC decoder attaches when the
    /// stream carries an RPU; a frame without one — the enhancement layer of a
    /// dual-layer file, or any file without Dolby Vision — has no such side data
    /// and yields `None`.
    pub fn from_frame(frame: &ffmpeg::frame::Video) -> Option<Self> {
        // SAFETY: the frame is alive for the call. `side_data` is an array of
        // `nb_side_data` pointers FFmpeg owns; every entry is only read, the
        // pointer array is indexed in bounds, and each candidate is null-checked
        // before it is followed.
        unsafe {
            let raw = frame.as_ptr();
            if raw.is_null() || (*raw).side_data.is_null() || (*raw).nb_side_data <= 0 {
                return None;
            }
            for index in 0..(*raw).nb_side_data as isize {
                let side = *(*raw).side_data.offset(index);
                if side.is_null()
                    || (*side).type_ != ffi::AVFrameSideDataType::AV_FRAME_DATA_DOVI_METADATA
                    || (*side).data.is_null()
                    || (*side).size < std::mem::size_of::<DvMetadata>()
                {
                    continue;
                }
                if let Some(rpu) = Self::parse((*side).data as *const DvMetadata, (*side).size) {
                    return Some(rpu);
                }
            }
            None
        }
    }

    /// Walk an `AVDOVIMetadata` that lives in `size` bytes.
    ///
    /// # Safety
    ///
    /// `meta` must point at the start of a readable `AVDOVIMetadata` and `size`
    /// must be the number of bytes readable from it — FFmpeg's side data gives
    /// exactly that, and it is what every offset below is checked against. A
    /// record that points outside its own buffer is refused rather than followed:
    /// a damaged or hostile file must not be able to make the player read
    /// somebody else's allocation.
    unsafe fn parse(meta: *const DvMetadata, size: usize) -> Option<Self> {
        // SAFETY: the caller guarantees a live pointer, and the side data's size
        // is at least `size_of::<DvMetadata>()` — `from_frame` checks that.
        let meta = unsafe { &*meta };
        let base = meta as *const DvMetadata as *const u8;
        // A zero offset means "this part is not here"; an offset whose part does
        // not fit inside the buffer means the same thing, only worse.
        let fits = |offset: usize, length: usize| {
            offset != 0 && offset.checked_add(length).is_some_and(|end| end <= size)
        };

        let mut rpu = DvRpu::default();
        if fits(meta.header_offset, std::mem::size_of::<DvRpuHeader>()) {
            // SAFETY: the range was just checked against the buffer's size.
            rpu.header = Some(unsafe { *(base.add(meta.header_offset) as *const DvRpuHeader) });
        }
        if fits(meta.color_offset, std::mem::size_of::<DvColorMetadata>()) {
            // SAFETY: as above.
            rpu.color = Some(unsafe { *(base.add(meta.color_offset) as *const DvColorMetadata) });
        }
        // SAFETY: as above. The mapping is read through a reference rather than
        // copied: it holds three reshaping curves, some five kilobytes of
        // coefficients this player never evaluates in place.
        if fits(meta.mapping_offset, std::mem::size_of::<DvDataMapping>()) {
            let mapping = unsafe { &*(base.add(meta.mapping_offset) as *const DvDataMapping) };
            let mut curves = Box::new([DvCurve::default(); 3]);
            for (component, curve) in curves.iter_mut().enumerate() {
                let raw = &mapping.curves[component];
                curve.num_pivots = raw.num_pivots;
                curve.pivots = raw.pivots;
                for piece in 0..DV_MAX_PIECES {
                    curve.method[piece] = raw.mapping_idc[piece].clamp(0, u8::MAX as i32) as u8;
                    curve.poly_order[piece] = raw.poly_order[piece];
                    curve.poly_coef[piece] = raw.poly_coef[piece];
                    curve.mmr_order[piece] = raw.mmr_order[piece];
                    curve.mmr_constant[piece] = raw.mmr_constant[piece];
                    curve.mmr_coef[piece] = raw.mmr_coef[piece];
                }
            }
            rpu.mapping = Some(DvMappingSummary {
                color_space: mapping.mapping_color_space,
                chroma_format: mapping.mapping_chroma_format_idc,
                luma: curve_method(&mapping.curves[0]),
                chroma: [
                    curve_method(&mapping.curves[1]),
                    curve_method(&mapping.curves[2]),
                ],
                nlq: DvNlq::from_idc(mapping.nlq_method_idc),
                pivots: [
                    mapping.curves[0].num_pivots,
                    mapping.curves[1].num_pivots,
                    mapping.curves[2].num_pivots,
                ],
                curves,
                nlq_params: mapping.nlq,
            });
        }

        let count = meta.num_ext_blocks.clamp(0, DV_MAX_EXT_BLOCKS as i32) as usize;
        if meta.ext_block_offset != 0 && meta.ext_block_size >= std::mem::size_of::<DvDmData>() {
            for index in 0..count {
                let start = meta.ext_block_offset + meta.ext_block_size * index;
                if !start
                    .checked_add(std::mem::size_of::<DvDmData>())
                    .is_some_and(|end| end <= size)
                {
                    break;
                }
                // SAFETY: the block lies inside the buffer, as just checked, and
                // the level byte at its start selects which union member to read.
                let block = unsafe { &*(base.add(start) as *const DvDmData) };
                match block.level {
                    1 if rpu.level1.is_none() => rpu.level1 = Some(unsafe { block.payload.level1 }),
                    2 => {
                        rpu.level2_count = rpu.level2_count.saturating_add(1);
                        if rpu.level2.is_none() {
                            rpu.level2 = Some(unsafe { block.payload.level2 });
                        }
                    }
                    5 if rpu.level5.is_none() => rpu.level5 = Some(unsafe { block.payload.level5 }),
                    6 if rpu.level6.is_none() => rpu.level6 = Some(unsafe { block.payload.level6 }),
                    _ => {}
                }
            }
        }

        let nothing = rpu.level1.is_none()
            && rpu.level2.is_none()
            && rpu.level5.is_none()
            && rpu.level6.is_none()
            && rpu.header.is_none()
            && rpu.color.is_none()
            && rpu.mapping.is_none();
        if nothing {
            // Nothing usable was in there. Reporting an empty RPU would only make
            // the caller believe a frame carries metadata it does not.
            return None;
        }
        Some(rpu)
    }

    /// Decode this frame's brightness range from level 1.
    pub fn scene(&self) -> Option<DoviScene> {
        let level = self.level1?;
        Some(DoviScene {
            min_nits: pq_code_to_nits(level.min_pq),
            avg_nits: pq_code_to_nits(level.avg_pq),
            max_nits: pq_code_to_nits(level.max_pq),
        })
    }

    /// `true` when this frame really carries per-frame metadata, as opposed to
    /// only the static level-6 block.
    pub fn has_dynamic_metadata(&self) -> bool {
        self.level1.is_some()
    }

    /// What the RPU says the decoded pixels are.
    pub fn signal(&self) -> Option<DvSignal> {
        self.color.as_ref().map(DvSignal::from_color)
    }

    /// The brightest sample this RPU names, in nits.
    ///
    /// The frame's own level-1 measurement first — that is what the colourist saw
    /// while grading the shot — and the master's window from the colour metadata
    /// when the frame carries no level 1 at all.
    pub fn peak_nits(&self) -> Option<f32> {
        self.scene()
            .map(|scene| scene.max_nits)
            .or_else(|| self.signal().and_then(|signal| signal.source_peak_nits))
    }

    /// Level 5, the active area in base-layer pixels.
    pub fn active_area(&self) -> Option<DoviLevel5> {
        self.level5
    }

    /// `true` when the RPU describes a mapping the base layer needs in order to
    /// look the way it was graded.
    ///
    /// This is a deliberately conservative reading. A component counts as mapped
    /// when its curve has more than the two pivots of a single piece, when it
    /// needs the enhancement layer's residuals (MMR), or when a non-linear
    /// inverse quantisation is in use. A one-piece polynomial is not counted:
    /// that is how the bitstream codes a component it left alone, and a reading
    /// that could not tell the two apart would report a reshaping on every Dolby
    /// Vision file and be ignored.
    pub fn reshapes(&self) -> bool {
        let Some(mapping) = &self.mapping else {
            return false;
        };
        if mapping.nlq != DvNlq::None {
            return true;
        }
        mapping.luma.pieces() > 1
            || mapping.luma.is_mmr()
            || mapping.chroma.iter().any(|c| c.pieces() > 1 || c.is_mmr())
    }
}

/// What kind of picture a Dolby Vision file's base layer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DvRender {
    /// The base layer is a complete picture on its own: every Profile 8 file, and
    /// the HDR10- or HLG-compatible base layer of a Profile 7 one. It is shown
    /// under its own transfer function, tone mapped or not.
    BaseLayer,
    /// Profile 5: the base layer is coded in IPT, so it is *not* a picture a
    /// BT.2020 decoder can show correctly. The player says so.
    IptBaseLayer,
    /// Neither a record nor an RPU: there is nothing to say about this file.
    Unknown,
}

impl DvRender {
    /// Short Chinese label for the interface.
    pub fn label(self) -> &'static str {
        match self {
            DvRender::BaseLayer => "单层基底层",
            DvRender::IptBaseLayer => "IPT 基底层",
            DvRender::Unknown => "未知",
        }
    }
}

/// What the player is going to do with one Dolby Vision file.
///
/// This is the single place the decision is made — the converter asks it per
/// frame, the information panel and the open-time notice ask it per file — so
/// that the notice, the panel and the renderer cannot end up with three opinions
/// about the same file. The same rule `picture.rs` follows for the picture
/// adjustments.
#[derive(Debug, Clone, PartialEq)]
pub struct DvPlan {
    /// The container's record, when there is one.
    pub config: Option<DoviConfig>,
    /// What the frame's RPU said, when a frame was at hand.
    pub signal: Option<DvSignal>,
    /// The transfer function the base layer is coded with.
    pub transfer: HdrKind,
    /// What kind of picture the base layer is.
    pub render: DvRender,
    /// The RPU carries a reshaping this player does not apply.
    pub reshaping_unapplied: bool,
}

impl DvPlan {
    /// Decide what to do, from whatever is known.
    ///
    /// `config` is the container's record (available as soon as the file opens),
    /// `rpu` the current frame's metadata (available only while decoding), and
    /// `tagged` the transfer function the container or the frame claims. `None`
    /// means the file is not Dolby Vision at all, which is the caller's cue to
    /// leave the frame alone.
    pub fn resolve(
        config: Option<DoviConfig>,
        rpu: Option<&DvRpu>,
        tagged: HdrKind,
    ) -> Option<Self> {
        if config.is_none() && rpu.is_none() {
            return None;
        }
        let signal = rpu.and_then(|rpu| rpu.signal());
        // The RPU is the most specific statement about a frame, the record is the
        // next, and the tags the stream itself carries are the last resort — a
        // Dolby Vision stream frequently ships without them, and one that carries
        // none at all is the PQ-coded signal the specification defines.
        let transfer = signal
            .as_ref()
            .and_then(|signal| signal.transfer)
            .or_else(|| config.map(|config| config.base_layer_kind(tagged)))
            .unwrap_or(if tagged.is_hdr() { tagged } else { HdrKind::Pq });
        // Profile 5 is the one profile whose base layer is not a picture in a
        // transfer function the player could show: it is IPT.
        let ipt = signal.is_some_and(|signal| signal.color_space == DvColorSpace::Ipt)
            || config.is_some_and(|config| config.needs_dolby_renderer());
        let render = if ipt {
            DvRender::IptBaseLayer
        } else {
            DvRender::BaseLayer
        };
        Some(DvPlan {
            config,
            signal,
            transfer,
            render,
            reshaping_unapplied: rpu.is_some_and(|rpu| rpu.reshapes()),
        })
    }

    /// `true` when the container declares a second video stream.
    pub fn enhancement_layer(&self) -> bool {
        self.config.map(|config| config.el_present).unwrap_or(false)
    }

    /// One Chinese line: what the file is and what the player is doing with it.
    ///
    /// `tone_mapped` is whether the frame is being brought into SDR range, which
    /// only the caller knows.
    pub fn summary(&self, tone_mapped: bool) -> String {
        let what = match &self.config {
            Some(config) => format!("杜比视界 {}", config.label()),
            None => "杜比视界（容器未声明配置记录）".to_string(),
        };
        let doing = match self.render {
            DvRender::IptBaseLayer => "基底层为 IPT 编码，无法正确还原".to_string(),
            DvRender::BaseLayer if tone_mapped => {
                format!("基底层按 {} 色调映射到 SDR", self.transfer.label())
            }
            DvRender::BaseLayer => format!("{} 原片直通（未做色调映射）", self.transfer.label()),
            DvRender::Unknown => "无可用信息".to_string(),
        };
        format!("{what} · {doing}")
    }

    /// What the player cannot do for this file, one Chinese sentence per entry.
    ///
    /// Empty when there is nothing to warn about, which is the common case for a
    /// single-layer Profile 8 file.
    pub fn caveats(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.render == DvRender::IptBaseLayer {
            out.push(
                "基底层为 IPT（ICtCp）编码，只有杜比视界渲染器能正确还原，颜色可能不正确"
                    .to_string(),
            );
        }
        if self.enhancement_layer() {
            out.push(
                "文件带增强层（Profile 7 双层 MEL/FEL），本播放器只解码基底层，增强层的细节不会合成"
                    .to_string(),
            );
        }
        if self.reshaping_unapplied {
            out.push(
                "RPU 带非线性重塑（多项式 / MMR）：默认不应用，可在设置 → 视频 → \
                 「杜比视界重塑」里开启（亮度曲线已与 libplacebo 对照验证，色度曲线尚未）"
                    .to_string(),
            );
        }
        out
    }
}

/// Decode the first frames of a file's primary video stream and return the Dolby
/// Vision RPUs they carry.
///
/// This is for the callers that cannot reach a decoded frame any other way:
/// `examples/dv_probe.rs`, which prints what a file declares, and the opt-in
/// `dolby_vision` integration test. It is a *reader* — it opens its own decoder,
/// decodes at most `max_frames` frames and keeps nothing but the metadata.
pub fn probe_rpus(path: &Path, max_frames: usize) -> Result<Vec<DvRpu>> {
    crate::init()?;
    let mut ictx = ffmpeg::format::input(path)?;
    let index = ictx
        .streams()
        .best(ffmpeg::media::Type::Video)
        .map(|stream| stream.index())
        .ok_or_else(|| MediaError::MissingStream("视频".into()))?;
    let params = {
        let stream = ictx
            .streams()
            .find(|stream| stream.index() == index)
            .ok_or_else(|| MediaError::MissingStream("视频".into()))?;
        crate::bitmap_subtitle::own_parameters(&stream)
    };
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(params)?
        .decoder()
        .video()?;

    let mut frame = ffmpeg::frame::Video::empty();
    let mut rpus: Vec<DvRpu> = Vec::new();
    let mut decoded = 0usize;
    for (stream, packet) in ictx.packets() {
        if stream.index() != index {
            continue;
        }
        // A decoder that refuses a packet has either been flushed already or met
        // something it cannot decode. Either way the metadata of the frames it
        // did produce is still worth returning rather than failing the probe.
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        while decoder.receive_frame(&mut frame).is_ok() {
            if let Some(rpu) = DvRpu::from_frame(&frame) {
                rpus.push(rpu);
            }
            decoded += 1;
            if decoded >= max_frames {
                return Ok(rpus);
            }
        }
    }
    Ok(rpus)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    /// The record a nine-byte container block parses into.
    fn record(profile: u8, compatibility: u8, el: bool) -> DoviConfig {
        DoviConfig {
            version_major: 1,
            version_minor: 0,
            profile,
            level: 6,
            rpu_present: true,
            el_present: el,
            bl_present: true,
            bl_compatibility_id: compatibility,
        }
    }

    /// A ratio, as `AVRational` codes it.
    fn ratio_value(num: i32, den: i32) -> DvRatio {
        DvRatio { num, den }
    }

    #[test]
    fn the_mirrored_layout_matches_the_c_header() {
        // `libavutil/dovi_meta.h`, field by field. A wrong offset here would not
        // fail to compile — it would read neighbouring bytes — so every one the
        // reader depends on is pinned. Alignment comes from the C rules: a
        // `size_t` is eight bytes, an `AVRational` four, and the reshaping
        // curves are eight because their coefficients are `int64_t`.
        assert_eq!(size_of::<DvMetadata>(), 48);
        assert_eq!(offset_of!(DvMetadata, ext_block_offset), 24);
        assert_eq!(offset_of!(DvMetadata, ext_block_size), 32);

        assert_eq!(size_of::<DvRpuHeader>(), 20);
        assert_eq!(offset_of!(DvRpuHeader, rpu_format), 2);
        assert_eq!(offset_of!(DvRpuHeader, bl_bit_depth), 11);
        assert_eq!(offset_of!(DvRpuHeader, disable_residual_flag), 16);
        assert_eq!(offset_of!(DvRpuHeader, ext_mapping_idc_5_7), 18);

        assert_eq!(size_of::<DvColorMetadata>(), 196);
        assert_eq!(offset_of!(DvColorMetadata, ycc_to_rgb_matrix), 4);
        assert_eq!(offset_of!(DvColorMetadata, ycc_to_rgb_offset), 76);
        assert_eq!(offset_of!(DvColorMetadata, rgb_to_lms_matrix), 100);
        assert_eq!(offset_of!(DvColorMetadata, signal_eotf), 172);
        assert_eq!(offset_of!(DvColorMetadata, signal_bit_depth), 184);
        assert_eq!(offset_of!(DvColorMetadata, source_min_pq), 188);
        assert_eq!(offset_of!(DvColorMetadata, source_max_pq), 190);
        assert_eq!(offset_of!(DvColorMetadata, source_diagonal), 192);

        assert_eq!(size_of::<DvReshapingCurve>(), 1672);
        assert_eq!(offset_of!(DvReshapingCurve, pivots), 2);
        assert_eq!(offset_of!(DvReshapingCurve, mapping_idc), 20);
        assert_eq!(offset_of!(DvReshapingCurve, poly_order), 52);
        assert_eq!(offset_of!(DvReshapingCurve, poly_coef), 64);
        assert_eq!(offset_of!(DvReshapingCurve, mmr_order), 256);
        assert_eq!(offset_of!(DvReshapingCurve, mmr_constant), 264);
        assert_eq!(offset_of!(DvReshapingCurve, mmr_coef), 328);

        assert_eq!(size_of::<DvNlqParams>(), 32);
        assert_eq!(size_of::<DvDataMapping>(), 5144);
        assert_eq!(offset_of!(DvDataMapping, curves), 8);
        assert_eq!(offset_of!(DvDataMapping, nlq_method_idc), 5024);
        assert_eq!(offset_of!(DvDataMapping, nlq), 5040);
        assert_eq!(offset_of!(DvDataMapping, nlq_pivots), 5136);

        // The extension block's union is forty bytes wide because levels 9 and 10
        // embed an `AVColorPrimariesDesc`, which is `AVRational`s; that is what
        // puts the payload four bytes after the level byte.
        assert_eq!(size_of::<DvDmPayload>(), 40);
        assert_eq!(size_of::<DvDmData>(), 44);
        assert_eq!(offset_of!(DvDmData, payload), 4);
    }

    /// A hand-built `AVDOVIMetadata`, so the offset walk can be tested without a
    /// Dolby Vision file — the real samples are far too large to keep in the
    /// repository. The field order is the C one, which is what gives every
    /// interior part the offset the reader expects.
    #[repr(C)]
    struct FakeMeta {
        meta: DvMetadata,
        header: DvRpuHeader,
        mapping: DvDataMapping,
        color: DvColorMetadata,
        blocks: [DvDmData; 3],
    }

    /// A mapping shaped like the one a real Profile 8.4 sample carries: a
    /// six-piece polynomial for luma and an MMR fit for the chroma.
    fn reshaping_mapping() -> DvDataMapping {
        // SAFETY: every field is an integer, so all-zero is a valid value.
        let mut mapping: DvDataMapping = unsafe { std::mem::zeroed() };
        mapping.nlq_method_idc = -1;
        mapping.curves[0].num_pivots = 7;
        mapping.curves[0].mapping_idc[0] = 0;
        mapping.curves[0].poly_order[0] = 2;
        mapping.curves[1].num_pivots = 2;
        mapping.curves[1].mapping_idc[0] = 1;
        mapping.curves[1].mmr_order[0] = 3;
        mapping
    }

    /// A mapping that leaves the base layer alone: one polynomial piece per
    /// component, which is what the bitstream writes when nothing was reshaped.
    fn identity_mapping() -> DvDataMapping {
        // SAFETY: as above.
        let mut mapping: DvDataMapping = unsafe { std::mem::zeroed() };
        mapping.nlq_method_idc = -1;
        for curve in mapping.curves.iter_mut() {
            curve.num_pivots = 2;
            curve.mapping_idc[0] = 0;
            curve.poly_order[0] = 1;
        }
        mapping
    }

    /// The colour metadata of the sample that was measured: an HLG base layer,
    /// twelve bits of signal, and a master running from 62 to 3079 as 12-bit PQ.
    fn color_metadata() -> DvColorMetadata {
        // SAFETY: as above.
        let mut color: DvColorMetadata = unsafe { std::mem::zeroed() };
        color.signal_eotf = 2;
        color.signal_bit_depth = 12;
        color.signal_full_range_flag = 1;
        color.source_min_pq = 62;
        color.source_max_pq = 3079;
        color.source_diagonal = 42;
        color.ycc_to_rgb_matrix[0] = ratio_value(9574, 8192);
        color.rgb_to_lms_matrix[8] = ratio_value(15962, 16384);
        color
    }

    /// The RPU header of the same sample.
    fn header() -> DvRpuHeader {
        DvRpuHeader {
            rpu_type: 2,
            rpu_format: 18,
            vdr_rpu_profile: 1,
            vdr_rpu_level: 0,
            chroma_resampling_explicit_filter_flag: 0,
            coef_data_type: 0,
            coef_log2_denom: 23,
            vdr_rpu_normalized_idc: 1,
            bl_video_full_range_flag: 0,
            bl_bit_depth: 10,
            el_bit_depth: 10,
            vdr_bit_depth: 12,
            spatial_resampling_filter_flag: 0,
            el_spatial_resampling_filter_flag: 0,
            disable_residual_flag: 1,
            ext_mapping_idc_0_4: 0,
            ext_mapping_idc_5_7: 0,
        }
    }

    /// Level 1: one frame's brightness range.
    fn block_level1(min_pq: u16, max_pq: u16, avg_pq: u16) -> DvDmData {
        DvDmData {
            level: 1,
            payload: DvDmPayload {
                level1: DoviLevel1 {
                    min_pq,
                    max_pq,
                    avg_pq,
                },
            },
        }
    }

    /// Level 2: the trims for one target display.
    fn block_level2(target_max_pq: u16) -> DvDmData {
        DvDmData {
            level: 2,
            payload: DvDmPayload {
                level2: DoviLevel2 {
                    target_max_pq,
                    ..DoviLevel2::default()
                },
            },
        }
    }

    /// Level 5: the active area.
    fn block_level5(left: u16, top: u16) -> DvDmData {
        DvDmData {
            level: 5,
            payload: DvDmPayload {
                level5: DoviLevel5 {
                    left_offset: left,
                    right_offset: 0,
                    top_offset: top,
                    bottom_offset: 0,
                },
            },
        }
    }

    /// Level 6: the static mastering metadata.
    fn block_level6(max_luminance: u16) -> DvDmData {
        DvDmData {
            level: 6,
            payload: DvDmPayload {
                level6: DoviLevel6 {
                    max_luminance,
                    min_luminance: 1,
                    max_cll: 900,
                    max_fall: 400,
                },
            },
        }
    }

    /// Build a metadata blob whose offsets are taken from its own layout.
    fn fake_metadata(
        mapping: DvDataMapping,
        color: DvColorMetadata,
        blocks: [DvDmData; 3],
    ) -> FakeMeta {
        FakeMeta {
            meta: DvMetadata {
                header_offset: offset_of!(FakeMeta, header),
                mapping_offset: offset_of!(FakeMeta, mapping),
                color_offset: offset_of!(FakeMeta, color),
                ext_block_offset: offset_of!(FakeMeta, blocks),
                ext_block_size: size_of::<DvDmData>(),
                num_ext_blocks: blocks.len() as i32,
            },
            header: header(),
            mapping,
            color,
            blocks,
        }
    }

    /// Parse a hand-built blob exactly as a decoded frame's side data is parsed.
    fn parse_fake(fake: &FakeMeta) -> Option<DvRpu> {
        // SAFETY: `fake` is live and fully initialised, the offsets were taken
        // from its own layout and `size` is its own size, so every read the
        // parser is allowed to make stays inside it.
        unsafe { DvRpu::parse(&fake.meta, size_of::<FakeMeta>()) }
    }

    #[test]
    fn the_rpu_is_walked_from_the_metadata_offsets() {
        let fake = fake_metadata(
            reshaping_mapping(),
            color_metadata(),
            [
                block_level1(0, 3000, 2000),
                block_level2(2081),
                block_level6(1000),
            ],
        );
        let rpu = parse_fake(&fake).expect("the parts are found");

        assert_eq!(rpu.header, Some(header()));
        assert_eq!(rpu.color, Some(color_metadata()));
        assert_eq!(
            rpu.level1,
            Some(DoviLevel1 {
                min_pq: 0,
                max_pq: 3000,
                avg_pq: 2000,
            })
        );
        assert_eq!(rpu.level2.map(|level| level.target_max_pq), Some(2081));
        assert_eq!(rpu.level2_count, 1);
        assert_eq!(rpu.level6.map(|level| level.max_luminance), Some(1000));
        assert!(rpu.has_dynamic_metadata());

        let scene = rpu.scene().expect("level 1 decodes to luminance");
        assert!(scene.min_nits <= scene.avg_nits && scene.avg_nits <= scene.max_nits);
        // PQ code 3000/4095 is a little under 1000 nits.
        assert!(
            (400.0..=1_000.0).contains(&scene.max_nits),
            "peak decoded to {} nits",
            scene.max_nits
        );
    }

    #[test]
    fn the_active_area_is_read_from_level_five() {
        let fake = fake_metadata(
            identity_mapping(),
            color_metadata(),
            [
                block_level5(0, 132),
                block_level1(0, 3000, 2000),
                block_level6(1000),
            ],
        );
        let rpu = parse_fake(&fake).expect("the parts are found");
        assert_eq!(
            rpu.active_area()
                .map(|area| (area.left_offset, area.top_offset)),
            Some((0, 132))
        );
    }

    #[test]
    fn a_metadata_whose_offsets_leave_its_buffer_is_refused() {
        let mut fake = fake_metadata(
            identity_mapping(),
            color_metadata(),
            [
                block_level1(0, 3000, 2000),
                block_level2(0),
                block_level6(1000),
            ],
        );
        // Every part claims to live far past the end of the buffer, which is what
        // a damaged or hostile file would do to make the reader wander into the
        // next allocation.
        fake.meta.header_offset = 1 << 20;
        fake.meta.mapping_offset = 1 << 20;
        fake.meta.color_offset = 1 << 20;
        fake.meta.ext_block_offset = 1 << 20;
        assert!(
            parse_fake(&fake).is_none(),
            "an offset outside the side data must not be followed"
        );

        // The same when the *size* is what is wrong: the record is complete, but
        // the caller says only the leading fields are readable.
        let mut fake = fake_metadata(
            identity_mapping(),
            color_metadata(),
            [
                block_level1(0, 3000, 2000),
                block_level2(0),
                block_level6(1000),
            ],
        );
        fake.meta.ext_block_offset = 0;
        // SAFETY: as above; the size is deliberately smaller than the layout, so
        // the parser must refuse the parts it cannot reach.
        assert!(
            unsafe { DvRpu::parse(&fake.meta, size_of::<DvMetadata>() + 1) }.is_none(),
            "a part that does not fit in the buffer must not be read"
        );
    }

    #[test]
    fn the_signal_is_read_from_the_colour_metadata() {
        let fake = fake_metadata(
            identity_mapping(),
            color_metadata(),
            [
                block_level1(0, 3000, 2000),
                block_level2(0),
                block_level6(1000),
            ],
        );
        let rpu = parse_fake(&fake).expect("the parts are found");
        let signal = rpu.signal().expect("the colour metadata is read");

        assert_eq!(signal.transfer, Some(HdrKind::Hlg));
        assert_eq!(signal.color_space, DvColorSpace::Yuv);
        assert_eq!(signal.bit_depth, 12);
        assert!(signal.full_range);
        assert_eq!(signal.source_diagonal_inches, Some(42));
        assert!(
            (900.0..=1100.0).contains(&signal.source_peak_nits.unwrap()),
            "12-bit PQ 3079 is about 1000 nits, not {:?}",
            signal.source_peak_nits
        );
        assert!(
            signal.source_min_nits.unwrap() < 1.0,
            "the master's black is {:?} nits",
            signal.source_min_nits
        );
        assert!(signal.describe().contains("HLG"), "{}", signal.describe());
        assert!(signal.describe().contains("母版"), "{}", signal.describe());
    }

    #[test]
    fn the_dolby_vision_matrices_are_read_as_ratios() {
        let color = color_metadata();
        assert!((color.ycc_to_rgb()[0][0] - 9574.0 / 8192.0).abs() < 1e-6);
        assert!((color.rgb_to_lms()[2][2] - 15962.0 / 16384.0).abs() < 1e-6);
        // A cell the bitstream left at zero over zero is a zero, not an infinity
        // every caller would have to guard against.
        assert_eq!(color.ycc_offset(), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn a_reshaping_is_reported_only_when_one_is_there() {
        let blocks = [
            block_level1(0, 3000, 2000),
            block_level2(0),
            block_level6(1000),
        ];
        let reshaped = parse_fake(&fake_metadata(
            reshaping_mapping(),
            color_metadata(),
            blocks,
        ))
        .expect("the parts are found");
        assert!(reshaped.reshapes(), "six pieces and an MMR chroma");

        let flat = parse_fake(&fake_metadata(identity_mapping(), color_metadata(), blocks))
            .expect("the parts are found");
        assert!(
            !flat.reshapes(),
            "one polynomial piece per component is how the bitstream writes \\
             'nothing was reshaped'"
        );

        // A non-linear inverse quantisation counts on its own, even with the
        // curves left alone.
        let mut quantised = identity_mapping();
        quantised.nlq_method_idc = 0;
        let quantised = parse_fake(&fake_metadata(quantised, color_metadata(), blocks))
            .expect("the parts are found");
        assert!(quantised.reshapes());
        assert_eq!(
            quantised.mapping.map(|m| m.nlq),
            Some(DvNlq::LinearDeadzone)
        );
    }

    #[test]
    fn the_plan_follows_the_record_and_the_frame() {
        // Nothing Dolby Vision about the file: the caller is told nothing, which
        // is its cue to leave the frame alone.
        assert!(DvPlan::resolve(None, None, HdrKind::Sdr).is_none());

        // Profile 8.4: an HLG-compatible base layer, single layer, and the frame
        // carries a reshaping the player will not apply.
        let hlg = DvPlan::resolve(Some(record(8, 4, false)), None, HdrKind::Hlg)
            .expect("a record is enough");
        assert_eq!(hlg.transfer, HdrKind::Hlg);
        assert_eq!(hlg.render, DvRender::BaseLayer);
        assert!(!hlg.enhancement_layer());
        assert!(hlg.caveats().is_empty(), "{:?}", hlg.caveats());

        let with_rpu = parse_fake(&fake_metadata(
            reshaping_mapping(),
            color_metadata(),
            [
                block_level1(0, 3000, 2000),
                block_level2(0),
                block_level6(1000),
            ],
        ))
        .expect("the parts are found");
        let plan = DvPlan::resolve(Some(record(8, 4, false)), Some(&with_rpu), HdrKind::Sdr)
            .expect("a record is enough");
        assert_eq!(plan.transfer, HdrKind::Hlg, "the RPU says HLG");
        assert!(plan.reshaping_unapplied);
        assert_eq!(plan.caveats().len(), 1, "{:?}", plan.caveats());

        // The same frame with no record to consult, and the RPU *not* naming a
        // transfer function (the sample that was measured carries `0xFFFF` there):
        // the tags the stream itself carries are the last answer, and only a
        // stream that carries none at all falls back to the PQ the specification
        // codes its own base layers with.
        let mut unstated = color_metadata();
        unstated.signal_eotf = 0xFFFF;
        let bare = parse_fake(&fake_metadata(
            identity_mapping(),
            unstated,
            [
                block_level1(0, 3000, 2000),
                block_level2(0),
                block_level6(1000),
            ],
        ))
        .expect("the parts are found");
        assert!(
            bare.signal().expect("colour metadata").transfer.is_none(),
            "0xFFFF is not a transfer function"
        );
        let no_record = DvPlan::resolve(None, Some(&bare), HdrKind::Hlg).expect("an RPU is enough");
        assert_eq!(no_record.transfer, HdrKind::Hlg);
        let untagged = DvPlan::resolve(None, Some(&bare), HdrKind::Sdr).expect("an RPU is enough");
        assert_eq!(untagged.transfer, HdrKind::Pq);

        // Profile 7 on Blu-ray: an HDR10 base layer and a second video stream.
        let dual = DvPlan::resolve(Some(record(7, 6, true)), None, HdrKind::Sdr)
            .expect("a record is enough");
        assert_eq!(dual.transfer, HdrKind::Pq);
        assert!(dual.enhancement_layer());
        assert_eq!(dual.caveats().len(), 1, "{:?}", dual.caveats());

        // Profile 5: the base layer is IPT, which is the one case the player
        // cannot show correctly at all.
        let ipt = DvPlan::resolve(Some(record(5, 0, false)), None, HdrKind::Sdr)
            .expect("a record is enough");
        assert_eq!(ipt.render, DvRender::IptBaseLayer);
        assert_eq!(ipt.transfer, HdrKind::Pq, "a DV-only base layer is PQ");
        assert_eq!(ipt.caveats().len(), 1, "{:?}", ipt.caveats());
    }

    #[test]
    fn the_summary_says_what_the_player_is_doing() {
        let plan = DvPlan::resolve(Some(record(8, 4, false)), None, HdrKind::Hlg)
            .expect("a record is enough");
        let mapped = plan.summary(true);
        assert!(mapped.contains("Profile 8"), "{mapped}");
        assert!(mapped.contains("HLG"), "{mapped}");
        assert!(mapped.contains("色调映射"), "{mapped}");

        let passthrough = plan.summary(false);
        assert!(passthrough.contains("原片直通"), "{passthrough}");
    }
}

/// `AVDOVINLQMethod`: the non-linear inverse quantisation in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DvNlq {
    /// The signal is not quantised non-linearly.
    #[default]
    None,
    /// A linear dead zone above the pivots (`AV_DOVI_NLQ_LINEAR_DZ`).
    LinearDeadzone,
}

impl DvNlq {
    /// `AV_DOVI_NLQ_NONE` is `-1` and `AV_DOVI_NLQ_LINEAR_DZ` is `0`; anything
    /// else is a method this build of FFmpeg does not know, which is reported as
    /// "not none" rather than as an identity.
    fn from_idc(idc: std::os::raw::c_int) -> Self {
        if idc == -1 {
            DvNlq::None
        } else {
            DvNlq::LinearDeadzone
        }
    }
}

/// One component's curve, as the bitstream codes it.
///
/// This is the raw material the reshaping is built from — see
/// [`crate::dolby::reshape`], which owns the normalization and the evaluation.
/// It is kept as integers on purpose: the values are fixed point over
/// `2^coef_log2_denom`, and nothing here is meaningful as a float.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DvCurve {
    /// Number of pivots, `[2, 9]`: two pivots are one piece.
    pub num_pivots: u8,
    /// Ascending pivot values, in base-layer code values.
    pub pivots: [u16; DV_MAX_PIECES + 1],
    /// `0` polynomial, `1` MMR, per piece.
    pub method: [u8; DV_MAX_PIECES],
    /// Polynomial order per piece, `[1, 2]`.
    pub poly_order: [u8; DV_MAX_PIECES],
    /// Polynomial coefficients per piece: `x^0`, `x^1`, `x^2`.
    pub poly_coef: [[i64; 3]; DV_MAX_PIECES],
    /// MMR order per piece, `[1, 3]`.
    pub mmr_order: [u8; DV_MAX_PIECES],
    /// MMR constant term per piece.
    pub mmr_constant: [i64; DV_MAX_PIECES],
    /// MMR coefficients per piece: `[order - 1][7]`.
    pub mmr_coef: [[[i64; 7]; 3]; DV_MAX_PIECES],
}

/// What a frame's mapping says, in the form the rest of the player reads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DvMappingSummary {
    /// Colour space the mapping is described in.
    pub color_space: u8,
    /// Chroma format the mapping is described in, when the mapping says.
    pub chroma_format: u8,
    /// The luma component's method.
    pub luma: DvMappingMethod,
    /// The two chroma components' methods.
    pub chroma: [DvMappingMethod; 2],
    /// The non-linear inverse quantisation in use.
    pub nlq: DvNlq,
    /// Number of pivots per component, as the bitstream codes it.
    pub pivots: [u8; 3],
    /// The curves themselves, for the reshaping pass. Boxed because they are
    /// several kilobytes of coefficients, and this summary is copied per frame.
    pub curves: Box<[DvCurve; 3]>,
    /// The non-linear inverse quantisation of each component, as coded.
    pub nlq_params: [DvNlqParams; 3],
}

impl DvMappingSummary {
    /// `true` when the mapping describes at least one component.
    pub fn describes_a_curve(&self) -> bool {
        self.curves.iter().any(|curve| curve.num_pivots >= 2)
    }
}

/// Read one component's method out of its curve.
fn curve_method(curve: &DvReshapingCurve) -> DvMappingMethod {
    let pieces = curve.num_pivots.clamp(2, (DV_MAX_PIECES + 1) as u8) - 1;
    match curve.mapping_idc[0] {
        0 => DvMappingMethod::Polynomial {
            order: curve.poly_order[0],
            pieces,
        },
        1 => DvMappingMethod::Mmr {
            order: curve.mmr_order[0],
            pieces,
        },
        _ => DvMappingMethod::None,
    }
}

/// Level 1 of a Dolby Vision RPU: how bright one frame is.
///
/// The three fields are 12-bit PQ code values, as the bitstream defines them —
/// not nits. [`DvRpu::scene`] turns them into luminance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct DoviLevel1 {
    /// Darkest sample of the frame.
    pub min_pq: u16,
    /// Brightest sample of the frame.
    pub max_pq: u16,
    /// Average brightness of the frame.
    pub avg_pq: u16,
}

/// Level 2 of a Dolby Vision RPU: the colourist's trims for one target display.
///
/// A Dolby Vision file carries one of these per target the grade was checked on
/// (a 100-nit SDR television, a 600-nit one, and so on). They are reported rather
/// than applied: a trim is a change to the *display management* of the signal,
/// and applying one without the rest of that algorithm would be a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct DoviLevel2 {
    /// The target this trim is for, as 12-bit PQ.
    pub target_max_pq: u16,
    /// Trim on the slope of the tone curve.
    pub trim_slope: u16,
    /// Trim on the black offset.
    pub trim_offset: u16,
    /// Trim on the gamma of the tone curve.
    pub trim_power: u16,
    /// Weight of the chroma trim.
    pub trim_chroma_weight: u16,
    /// Gain applied to saturation.
    pub trim_saturation_gain: u16,
    /// Weight of this trim in a mid-tone adjustment.
    pub ms_weight: i16,
}

/// Level 5 of a Dolby Vision RPU: the part of the picture that is really there.
///
/// The offsets are in base-layer pixels and describe the letterbox a display can
/// use to fit the picture; a player that letterboxes by itself does not need
/// them, so this is reported and not acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct DoviLevel5 {
    /// Left edge of the active area.
    pub left_offset: u16,
    /// Right edge.
    pub right_offset: u16,
    /// Top edge.
    pub top_offset: u16,
    /// Bottom edge.
    pub bottom_offset: u16,
}

/// Level 6 of a Dolby Vision RPU: the static HDR10 mastering metadata.
///
/// Kept exactly as the bitstream codes it. This type deliberately exposes the
/// raw fields rather than inventing a unit: the renderer only uses the RPU's own
/// code values, which are unambiguous, and guessing a scale here could only make
/// the picture wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct DoviLevel6 {
    /// Mastering-display maximum luminance, as coded.
    pub max_luminance: u16,
    /// Mastering-display minimum luminance, as coded.
    pub min_luminance: u16,
    /// Maximum content light level, as coded.
    pub max_cll: u16,
    /// Maximum frame-average light level, as coded.
    pub max_fall: u16,
}

/// The luminance range of one Dolby Vision frame, in nits, decoded from RPU
/// level 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DoviScene {
    /// Darkest sample of the frame, in nits.
    pub min_nits: f32,
    /// Average brightness of the frame, in nits.
    pub avg_nits: f32,
    /// Brightest sample of the frame, in nits.
    pub max_nits: f32,
}

/// The leading fields of `AVDOVIMetadata`, which say where each part of the
/// record lives. FFmpeg's own accessors are `static inline`, so the layout is
/// mirrored here; only these documented, stable offsets are read.
#[repr(C)]
struct DvMetadata {
    header_offset: usize,
    mapping_offset: usize,
    color_offset: usize,
    ext_block_offset: usize,
    ext_block_size: usize,
    num_ext_blocks: std::os::raw::c_int,
}

/// One `AVDOVIDmData` extension block: a level byte followed by a union whose
/// member the level selects.
#[repr(C)]
#[derive(Clone, Copy)]
struct DvDmData {
    level: u8,
    payload: DvDmPayload,
}

/// The union of `AVDOVIDmData`.
///
/// `_abi` is not read. It is there because the union's *alignment* is what places
/// the payload after the level byte in C: levels 9 and 10 embed an
/// `AVColorPrimariesDesc`, which is `AVRational`s, so the union is four-byte
/// aligned and forty bytes wide, and the payload sits at offset four. Mirroring
/// that size and alignment is what keeps this type's field offsets identical to
/// the real one's.
#[repr(C)]
#[derive(Clone, Copy)]
union DvDmPayload {
    level1: DoviLevel1,
    level2: DoviLevel2,
    level5: DoviLevel5,
    level6: DoviLevel6,
    _abi: [u32; 10],
}
