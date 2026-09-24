//! High dynamic range: what a file carries, and how to show it on a display
//! that expects standard dynamic range.
//!
//! Two separate problems live here.
//!
//! 1. **Knowing what the file is.** Dolby Vision (and HDR10/HLG) content is
//!    coded with the PQ or HLG transfer function into BT.2020 primaries, and a
//!    Dolby Vision container additionally declares a *configuration record*
//!    (profile, level, whether an enhancement layer is present). Playing such a
//!    file as if it were BT.709 gamma content is what makes it look grey and
//!    washed out; a Profile 5 file is worse, because its base layer is coded in
//!    IPT and only something that undoes the Dolby Vision mapping can show it.
//! 2. **Showing it anyway.** [`ToneMapper`] brings the highlights back into
//!    range so the picture looks like the grade rather than like a faded copy.
//!
//! The Dolby Vision half of that first problem — what a configuration record
//! actually means, the reference picture unit a frame carries, and what the
//! player is going to do about either — is [`crate::dolby`]. This module reads
//! the container's record, decides how much to map, and maps it.
//!
//! The mapping is deliberately approximate and cheap: a per-pixel
//! luminance-preserving curve plus the BT.2020 → BT.709 matrix, applied to the
//! RGBA buffer that has already been converted and scaled for display. Doing it
//! on the GPU with a custom shader would be both faster and more accurate, and
//! that is the next step; what is here is honest about being a compromise.

use ffmpeg_next as ffmpeg;
use ffmpeg::ffi;

/// The transfer function the video stream is coded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HdrKind {
    /// Standard dynamic range.
    #[default]
    Sdr,
    /// Perceptual Quantiser (BT.2100 PQ / SMPTE ST 2084). HDR10 and every
    /// Dolby Vision profile use this.
    Pq,
    /// Hybrid Log-Gamma (BT.2100 HLG).
    Hlg,
}

impl HdrKind {
    /// `true` when the picture needs tone mapping to look right on an SDR
    /// display.
    pub fn is_hdr(self) -> bool {
        !matches!(self, HdrKind::Sdr)
    }

    /// Short label for the information panel.
    pub fn label(self) -> &'static str {
        match self {
            HdrKind::Sdr => "SDR",
            HdrKind::Pq => "HDR10 / PQ",
            HdrKind::Hlg => "HLG",
        }
    }
}

/// The Dolby Vision configuration record a container declares.
///
/// FFmpeg hands this over as a nine-byte side-data block on the video stream;
/// the layout is documented in `libavutil/dovi_meta.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoviConfig {
    /// Major version of the bitstream specification.
    pub version_major: u8,
    /// Minor version.
    pub version_minor: u8,
    /// Dolby Vision profile (5, 7, 8, ...).
    pub profile: u8,
    /// Dolby Vision level.
    pub level: u8,
    /// The stream carries reference picture units (dynamic metadata).
    pub rpu_present: bool,
    /// A separate enhancement layer is present.
    pub el_present: bool,
    /// The base layer is present / usable on its own.
    pub bl_present: bool,
    /// `0` none, `1` HDR10, `2` SDR, `4` HLG, `6` Blu-ray HDR10.
    pub bl_compatibility_id: u8,
}

impl DoviConfig {
    /// Parse the nine-byte record, rejecting anything shorter.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 9 {
            return None;
        }
        Some(DoviConfig {
            version_major: bytes[0],
            version_minor: bytes[1],
            profile: bytes[2],
            level: bytes[3],
            rpu_present: bytes[4] != 0,
            el_present: bytes[5] != 0,
            bl_present: bytes[6] != 0,
            bl_compatibility_id: bytes[7],
        })
    }

    /// `true` for Profile 5, whose base layer is coded in IPT (ICtCp): it can
    /// only be rendered correctly by a decoder that undoes the Dolby Vision
    /// mapping, which libavcodec does not do.
    pub fn needs_dolby_renderer(&self) -> bool {
        self.profile == 5
    }

    /// The transfer function the *base layer* is coded with, given the
    /// transfer function the container tagged it with.
    ///
    /// The compatibility id is Dolby Vision's own statement about the base
    /// layer, and it beats the container's tags for the same reason the record
    /// beats them everywhere else: a Dolby Vision elementary stream frequently
    /// carries no tags of its own. Getting this wrong is not cosmetic — an
    /// HLG-compatible base layer (id `4`) run through the PQ curve of
    /// [`HdrKind::Pq`] is exactly the "washed out Dolby Vision file" this
    /// module exists to avoid.
    ///
    /// The two ids that name no transfer function fall back to `tagged` rather
    /// than overruling it, and only a container that said nothing at all is
    /// given the PQ curve the Dolby Vision specification codes its own base
    /// layers with.
    pub fn base_layer_kind(&self, tagged: HdrKind) -> HdrKind {
        match self.bl_compatibility_id {
            // HDR10: `1` is the streaming flavour, `6` the Blu-ray one.
            1 | 6 => HdrKind::Pq,
            // HLG.
            4 => HdrKind::Hlg,
            // `2` says the base layer is an SDR picture; `0` says it has no
            // compatibility layer at all (Profile 5, which is IPT-PQ).
            2 => tagged,
            _ => {
                if tagged.is_hdr() {
                    tagged
                } else {
                    HdrKind::Pq
                }
            }
        }
    }

    /// Human readable summary, e.g. `Profile 7（双层 · HDR10 兼容）`.
    pub fn label(&self) -> String {
        let layer = match (self.el_present, self.rpu_present) {
            (true, _) => "双层 MEL/FEL",
            (false, true) => "单层 · 动态元数据",
            (false, false) => "单层",
        };
        let compatibility = match self.bl_compatibility_id {
            1 => "HDR10 兼容",
            2 => "SDR 兼容",
            4 => "HLG 兼容",
            6 => "蓝光 HDR10 兼容",
            _ => "无兼容层",
        };
        format!(
            "Profile {}（{} · {}，Level {}）",
            self.profile, layer, compatibility, self.level
        )
    }
}

/// Everything the renderer needs to know about a stream's dynamic range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HdrInfo {
    /// Transfer function the stream is coded with.
    pub kind: HdrKind,
    /// Coded in BT.2020 primaries, which HDR10 and Dolby Vision always are.
    pub bt2020: bool,
    /// Dolby Vision configuration record, when the container declares one.
    pub dovi: Option<DoviConfig>,
}

impl HdrInfo {
    /// Read the dynamic-range fields off a decoded frame.
    pub fn from_frame(frame: &ffmpeg::frame::Video) -> Self {
        // SAFETY: the frame is alive for the duration of the call and both
        // fields are plain C enums copied out by value.
        unsafe {
            let ptr = frame.as_ptr();
            Self::from_parts((*ptr).color_trc, (*ptr).color_primaries)
        }
    }

    /// Read them off a stream's codec parameters, including the Dolby Vision
    /// configuration record the demuxer exported as coded side data.
    ///
    /// # Safety
    ///
    /// `params` must point at a live `AVCodecParameters`.
    pub unsafe fn from_codec_parameters(params: *const ffi::AVCodecParameters) -> Self {
        if params.is_null() {
            return Self::default();
        }
        // SAFETY: the caller guarantees a live pointer; every read below is a
        // plain scalar or a bounded walk of the side-data array FFmpeg owns.
        unsafe {
            let raw = &*params;
            let mut info = Self::from_parts(raw.color_trc, raw.color_primaries);
            if !raw.coded_side_data.is_null() && raw.nb_coded_side_data > 0 {
                for i in 0..raw.nb_coded_side_data as isize {
                    let entry = &*raw.coded_side_data.offset(i);
                    if entry.type_ != ffi::AVPacketSideDataType::AV_PKT_DATA_DOVI_CONF
                        || entry.data.is_null()
                    {
                        continue;
                    }
                    let bytes = std::slice::from_raw_parts(entry.data, entry.size);
                    if let Some(config) = DoviConfig::parse(bytes) {
                        info.dovi = Some(config);
                        // The record, not the container's tags, says what the
                        // base layer is coded with — and that is HLG for an
                        // HLG-compatible layer, not PQ.
                        info.kind = config.base_layer_kind(info.kind);
                        info.bt2020 = true;
                        break;
                    }
                }
            }
            info
        }
    }

    fn from_parts(
        transfer: ffi::AVColorTransferCharacteristic,
        primaries: ffi::AVColorPrimaries,
    ) -> Self {
        use ffi::AVColorTransferCharacteristic as Trc;
        let kind = match transfer {
            Trc::AVCOL_TRC_SMPTE2084 => HdrKind::Pq,
            Trc::AVCOL_TRC_ARIB_STD_B67 => HdrKind::Hlg,
            _ => HdrKind::Sdr,
        };
        let bt2020 = primaries == ffi::AVColorPrimaries::AVCOL_PRI_BT2020;
        HdrInfo {
            kind,
            bt2020,
            dovi: None,
        }
    }

    /// `true` when the picture has to be tone mapped to look right on an SDR
    /// display.
    pub fn needs_tone_map(&self) -> bool {
        self.kind.is_hdr()
    }

    /// `true` for the one case the player cannot show correctly at all: a
    /// Dolby Vision Profile 5 base layer, which is coded in IPT.
    pub fn needs_dolby_renderer(&self) -> bool {
        self.dovi.map(|d| d.needs_dolby_renderer()).unwrap_or(false)
    }

    /// Summary for the information panel, e.g.
    /// `杜比视界 Profile 7（双层 MEL/FEL · HDR10 兼容，Level 6） · BT.2020 · PQ`.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        match &self.dovi {
            Some(config) => parts.push(format!("杜比视界 {}", config.label())),
            None => parts.push(self.kind.label().to_string()),
        }
        if self.dovi.is_some() {
            parts.push(self.kind.label().to_string());
        }
        if self.bt2020 {
            parts.push("BT.2020".to_string());
        }
        parts.join(" · ")
    }
}

// The Dolby Vision reference picture unit — the levels FFmpeg attaches to a
// decoded frame, the colour metadata and the reshaping curves that come with
// them — lives in [`crate::dolby`], which is also where the player's policy for
// such a file is decided. What is left here is the container's record, the
// decision of how much to map, and the mapping itself.

/// Linear luminance the mapper treats as SDR reference white (ITU-R BT.2408).
///
/// Content at or below this level passes through untouched, which is what makes
/// a "HDR10-compatible" grade look the way its colourist intended on an SDR
/// screen instead of like a washed-out copy.
pub const SDR_WHITE_NITS: f32 = 203.0;

/// Upper end of the luminance table, in nits.
///
/// The PQ curve spans 10000 nits and highlights really do reach four digits, so
/// the table has to cover all of it: a table that stopped at 1000 nits would
/// hand a 4000-nit highlight the *gain* of a 1000-nit one, which brightens it
/// instead of compressing it — straight to clipped white.
const GAIN_LUT_MAX_NITS: f32 = 10_000.0;

/// Where the soft shoulder starts, in units of SDR reference white
/// (0.75 × 203 nits ≈ 152 nits).
///
/// Starting the roll-off *below* reference white is what reserves the top of
/// the SDR range for HDR highlights: if 203 nits already mapped to code 255,
/// everything above it could only clip.
const TONE_KNEE: f32 = 0.75;

/// Shoulder softness, in the same units as [`TONE_KNEE`].
///
/// Chosen so that a 1000-nit highlight lands at ~94% of white: bright, clearly
/// separated from reference white, but not clipped.
const SHOULDER: f32 = 3.0;

/// Where a scene's own peak lands in the display range, when the mapper is
/// aimed at that peak.
///
/// Just short of white: the brightest sample of the scene stays visibly the
/// brightest thing on screen, and nothing above it can reach the code ceiling.
const TARGET_PEAK: f32 = 0.95;

/// Shoulder softness that puts `peak_nits` at [`TARGET_PEAK`].
///
/// The knee stays where it is, so only the shoulder is re-solved; what moves is
/// the material above the knee. A scene that never exceeds 400 nits then keeps
/// more of the SDR range for its own highlights instead of being compressed by
/// a curve sized for 10 000-nit material. [`SHOULDER`] is what a frame without
/// dynamic metadata gets, and it is what this returns for a peak so low the
/// shoulder is meaningless.
fn shoulder_for_peak(peak_nits: f32) -> f32 {
    let x = (peak_nits / SDR_WHITE_NITS).max(0.0);
    if x <= TONE_KNEE + 0.05 {
        return 0.05;
    }
    // tone_curve(peak) = TARGET_PEAK solved for the shoulder:
    // TARGET = KNEE + (1 - KNEE) * (1 - exp(-(x - KNEE) / shoulder)).
    let ratio = ((1.0 - TARGET_PEAK) / (1.0 - TONE_KNEE)).clamp(1e-4, 0.999);
    ((x - TONE_KNEE) / -ratio.ln()).clamp(0.05, 100.0)
}

/// BT.2020 → BT.709 primaries, for linear light with a D65 white point.
///
/// Without this, a BT.2020 picture shown on an sRGB display comes out
/// oversaturated and hue-shifted.
const BT2020_TO_BT709: [[f32; 3]; 3] = [
    [1.6605, -0.5876, -0.0728],
    [-0.1246, 1.1329, -0.0083],
    [-0.0182, -0.1006, 1.1187],
];

/// Brings HDR frames back into the range an SDR display can show.
///
/// The mapping is folded into three small tables, so a frame costs one pass of
/// table lookups and a handful of multiplies per pixel — the reason this is
/// affordable on the CPU at all. It is an approximation: the tone curve is
/// applied to luminance and the result scales the colour, which preserves hue
/// but not exact chroma, and the gamut conversion is a plain matrix with no
/// perceptual intent. A GPU shader is the right place for the exact version.
pub struct ToneMapper {
    info: HdrInfo,
    /// 8-bit code value → linear light, in nits.
    to_linear: [f32; 256],
    /// Linear luminance (nits, up to [`GAIN_LUT_MAX_NITS`]) → the factor that
    /// brings it into range while keeping the hue.
    gain: Vec<f32>,
    /// Linear display light (`0..=1`) → 8-bit code value.
    encode: [u8; 1025],
    /// Shoulder softness the `gain` table was built with.
    shoulder: f32,
    /// The peak the mapper was aimed at, when it came from dynamic metadata.
    scene_peak: Option<f32>,
}

impl ToneMapper {
    /// Build the fixed tables for `info`.
    ///
    /// The tables depend only on the transfer function and the primaries, so a
    /// single mapper serves a whole file. This is what a frame with no dynamic
    /// metadata gets.
    pub fn new(info: HdrInfo) -> Self {
        Self::with_scene_peak(info, None)
    }

    /// Build the tables for `info`, aiming the highlight roll-off at a Dolby
    /// Vision scene whose brightest sample is `scene_peak_nits`.
    ///
    /// The knee stays fixed and only the shoulder moves: a scene that never
    /// exceeds 400 nits keeps more of the SDR range for its own highlights
    /// instead of being compressed by a curve sized for 10 000-nit material.
    /// Material above the knee therefore adapts from scene to scene, which is
    /// the point of dynamic metadata. `None` means the fixed [`SHOULDER`],
    /// which is what a plain HDR10 frame gets.
    pub fn with_scene_peak(info: HdrInfo, scene_peak_nits: Option<f32>) -> Self {
        let shoulder = scene_peak_nits.map(shoulder_for_peak).unwrap_or(SHOULDER);
        let mut to_linear = [0.0f32; 256];
        for (code, slot) in to_linear.iter_mut().enumerate() {
            let e = code as f32 / 255.0;
            *slot = match info.kind {
                HdrKind::Pq => pq_to_nits(e),
                HdrKind::Hlg => hlg_to_nits(e),
                // Not used for SDR, but keeping it consistent means the tables
                // are never nonsense if a caller builds one anyway.
                HdrKind::Sdr => e,
            };
        }

        let steps = 1024usize;
        let mut gain = Vec::with_capacity(steps + 1);
        for i in 0..=steps {
            let nits = GAIN_LUT_MAX_NITS * i as f32 / steps as f32;
            gain.push(if nits > 0.0 {
                tone_curve(nits, shoulder) / nits
            } else {
                1.0 / SDR_WHITE_NITS
            });
        }

        let mut encode = [0u8; 1025];
        let last = encode.len() - 1;
        for (i, slot) in encode.iter_mut().enumerate() {
            *slot = srgb_encode(i as f32 / last as f32);
        }

        Self {
            info,
            to_linear,
            gain,
            encode,
            shoulder,
            scene_peak: scene_peak_nits,
        }
    }

    /// The peak, in nits, this mapper was aimed at, when it was built from
    /// Dolby Vision dynamic metadata.
    pub fn scene_peak_nits(&self) -> Option<f32> {
        self.scene_peak
    }

    /// Shoulder softness the highlight roll-off was built with.
    ///
    /// A plain HDR10 mapper reports [`SHOULDER`]; a Dolby Vision scene reports
    /// the value [`shoulder_for_peak`] derived from its own peak.
    pub fn shoulder(&self) -> f32 {
        self.shoulder
    }

    /// The dynamic range this mapper was built for.
    pub fn info(&self) -> HdrInfo {
        self.info
    }

    /// `true` when [`Self::apply`] would do nothing.
    pub fn is_identity(&self) -> bool {
        !self.info.needs_tone_map()
    }

    /// Tone map a packed RGBA8 buffer in place; the alpha byte is untouched.
    pub fn apply(&self, rgba: &mut [u8]) {
        if self.is_identity() {
            return;
        }
        for pixel in rgba.chunks_exact_mut(4) {
            let mut r = self.to_linear[pixel[0] as usize];
            let mut g = self.to_linear[pixel[1] as usize];
            let mut b = self.to_linear[pixel[2] as usize];
            if self.info.bt2020 {
                let m = &BT2020_TO_BT709;
                let (nr, ng, nb) = (
                    m[0][0] * r + m[0][1] * g + m[0][2] * b,
                    m[1][0] * r + m[1][1] * g + m[1][2] * b,
                    m[2][0] * r + m[2][1] * g + m[2][2] * b,
                );
                // The matrix can push a colour outside the target gamut; that
                // lobe is clipped rather than wrapped around.
                r = nr.max(0.0);
                g = ng.max(0.0);
                b = nb.max(0.0);
            }
            let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            let factor = self.gain_at(luminance);
            pixel[0] = self.encode_linear(r * factor);
            pixel[1] = self.encode_linear(g * factor);
            pixel[2] = self.encode_linear(b * factor);
        }
    }

    fn gain_at(&self, nits: f32) -> f32 {
        if !nits.is_finite() || nits <= 0.0 {
            return self.gain[0];
        }
        let last = self.gain.len() - 1;
        let index = ((nits / GAIN_LUT_MAX_NITS) * last as f32) as usize;
        self.gain[index.min(last)]
    }

    fn encode_linear(&self, linear: f32) -> u8 {
        let last = self.encode.len() - 1;
        let index = (linear.max(0.0) * last as f32) as usize;
        self.encode[index.min(last)]
    }
}

/// PQ (SMPTE ST 2084) code value in `0..=1` → absolute light, in nits.
pub(crate) fn pq_to_nits(e: f32) -> f32 {
    const M1: f32 = 0.159_301_757_812_5;
    const M2: f32 = 78.843_75;
    const C1: f32 = 0.835_937_5;
    const C2: f32 = 18.851_562_5;
    const C3: f32 = 18.687_5;
    let e = e.clamp(0.0, 1.0).powf(1.0 / M2);
    let numerator = (e - C1).max(0.0);
    let denominator = C2 - C3 * e;
    if denominator <= 0.0 {
        return 0.0;
    }
    10_000.0 * (numerator / denominator).powf(1.0 / M1)
}

/// Full scale of the 12-bit PQ code value a Dolby Vision RPU level is coded in.
pub const PQ_12BIT_MAX: f32 = 4095.0;

/// A 12-bit PQ code value, as a Dolby Vision RPU codes luminance → nits.
///
/// The RPU's levels are code values rather than nits, and this is the one place
/// the conversion is written down so the tone mapper and the information panel
/// cannot disagree about it.
pub fn pq_code_to_nits(code: u16) -> f32 {
    pq_to_nits(f32::from(code) / PQ_12BIT_MAX)
}

/// HLG (BT.2100) code value in `0..=1` → light on a 1000-nit display, in nits.
fn hlg_to_nits(e: f32) -> f32 {
    const A: f32 = 0.178_832_77;
    const B: f32 = 0.284_668_92;
    const C: f32 = 0.559_910_73;
    let e = e.clamp(0.0, 1.0);
    let scene = if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - C) / A).exp() + B) / 12.0
    };
    // OOTF: a 1000-nit HLG display applies a system gamma of 1.2.
    1000.0 * scene.max(0.0).powf(1.2)
}

/// Linear luminance in nits → display-referred factor, `1.0` being white.
///
/// Identity below the knee, then a soft shoulder that approaches — but never
/// reaches — white: highlights keep their separation instead of clipping to a
/// flat plate the way a hard clamp would. The shoulder is per-scene for Dolby
/// Vision content; see [`shoulder_for_peak`].
fn tone_curve(nits: f32, shoulder: f32) -> f32 {
    let x = (nits / SDR_WHITE_NITS).max(0.0);
    if x <= TONE_KNEE {
        return x;
    }
    let over = x - TONE_KNEE;
    TONE_KNEE + (1.0 - TONE_KNEE) * (1.0 - (-over / shoulder.max(1e-4)).exp())
}

/// Linear display light (`0..=1`) → 8-bit sRGB / BT.709 code value.
fn srgb_encode(linear: f32) -> u8 {
    let l = linear.clamp(0.0, 1.0);
    let encoded = if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes `ffprobe` reports for a Profile 7 (FEL) test clip.
    const FEL_RECORD: [u8; 9] = [1, 0, 7, 6, 1, 1, 0, 6, 0];

    #[test]
    fn a_dolby_vision_record_is_parsed_byte_for_byte() {
        let config = DoviConfig::parse(&FEL_RECORD).expect("the record parses");
        assert_eq!(config.version_major, 1);
        assert_eq!(config.profile, 7);
        assert_eq!(config.level, 6);
        assert!(config.rpu_present);
        assert!(config.el_present);
        assert!(!config.bl_present);
        assert_eq!(config.bl_compatibility_id, 6);
        assert!(!config.needs_dolby_renderer());
        assert!(config.label().contains("Profile 7"), "{}", config.label());
        assert!(
            DoviConfig::parse(&FEL_RECORD[..8]).is_none(),
            "a truncated record must be rejected rather than read past the end"
        );
    }

    #[test]
    fn profile_five_is_the_one_that_needs_a_dolby_renderer() {
        let mut record = FEL_RECORD;
        record[2] = 5;
        let config = DoviConfig::parse(&record).unwrap();
        assert!(config.needs_dolby_renderer());
        assert!(config.label().contains("Profile 5"));
    }

    #[test]
    fn pq_starts_at_black_and_ends_at_ten_thousand_nits() {
        assert_eq!(pq_to_nits(0.0), 0.0);
        let peak = pq_to_nits(1.0);
        assert!((9_500.0..=10_000.0).contains(&peak), "PQ peak is {peak}");
        let half = pq_to_nits(0.5);
        assert!((85.0..=100.0).contains(&half), "PQ 0.5 is {half} nits");
    }

    #[test]
    fn hlg_ends_at_a_thousand_nits() {
        assert_eq!(hlg_to_nits(0.0), 0.0);
        let peak = hlg_to_nits(1.0);
        assert!((900.0..=1_100.0).contains(&peak), "HLG peak is {peak}");
    }

    #[test]
    fn the_tone_curve_never_folds_back() {
        let mut previous = tone_curve(0.0, SHOULDER);
        for step in 1..=2000 {
            let nits = step as f32 * 5.0;
            let value = tone_curve(nits, SHOULDER);
            assert!(value >= previous, "the curve went backwards at {nits} nits");
            assert!(value <= 1.0, "the curve overshot white at {nits} nits");
            previous = value;
        }
        // Reference white (203 nits) keeps its separation from the highlights
        // above it, and a 1000-nit highlight stays inside the code range.
        let white = tone_curve(SDR_WHITE_NITS, SHOULDER);
        assert!((0.70..0.85).contains(&white), "203 nits maps to {white}");
        let highlight = tone_curve(1000.0, SHOULDER);
        assert!(
            highlight > white && highlight < 1.0,
            "a 1000-nit highlight maps to {highlight}"
        );
        assert!(
            tone_curve(10_000.0, SHOULDER) <= 1.0,
            "nothing may exceed white"
        );
    }

    #[test]
    fn a_scene_peak_aims_the_shoulder_at_the_top_of_the_range() {
        // Every peak the RPU can report is put just short of white, while the
        // mid-tones only shift within the band the fixed curve already uses.
        for peak in [300.0f32, 1_000.0, 4_000.0, 10_000.0] {
            let shoulder = shoulder_for_peak(peak);
            let landed = tone_curve(peak, shoulder);
            assert!(
                (TARGET_PEAK - 0.02..=TARGET_PEAK + 0.02).contains(&landed),
                "a {peak}-nit peak landed at {landed}"
            );
            // Mid-tones adapt with the scene, but only gently: reference white
            // stays inside the band the fixed curve already leaves it in.
            let white = tone_curve(SDR_WHITE_NITS, shoulder);
            assert!(
                (0.70..=0.90).contains(&white),
                "reference white moved to {white} for a {peak}-nit scene"
            );
        }
        // A dimmer scene is not compressed as hard as the fixed curve would be:
        // that is the whole point of reading level 1.
        let dim = shoulder_for_peak(400.0);
        assert!(tone_curve(400.0, dim) > tone_curve(400.0, SHOULDER));
    }

    #[test]
    fn a_scene_peak_reaches_the_mapper() {
        let info = HdrInfo {
            kind: HdrKind::Pq,
            bt2020: true,
            dovi: None,
        };
        let plain = ToneMapper::new(info);
        assert_eq!(plain.shoulder(), SHOULDER);
        assert!(plain.scene_peak_nits().is_none());

        let aimed = ToneMapper::with_scene_peak(info, Some(600.0));
        assert_eq!(aimed.scene_peak_nits(), Some(600.0));
        assert!(aimed.shoulder() < plain.shoulder());
    }

    #[test]
    fn a_dolby_vision_record_names_the_base_layer_transfer() {
        let record = |compatibility: u8| DoviConfig {
            version_major: 1,
            version_minor: 0,
            profile: 8,
            level: 9,
            rpu_present: true,
            el_present: false,
            bl_present: true,
            bl_compatibility_id: compatibility,
        };
        // The compatibility id beats a container that said nothing, and it beats
        // one that said the wrong thing: an HLG base layer is HLG.
        assert_eq!(record(4).base_layer_kind(HdrKind::Sdr), HdrKind::Hlg);
        assert_eq!(record(4).base_layer_kind(HdrKind::Pq), HdrKind::Hlg);
        assert_eq!(record(1).base_layer_kind(HdrKind::Sdr), HdrKind::Pq);
        assert_eq!(record(6).base_layer_kind(HdrKind::Sdr), HdrKind::Pq);
        // `2` (SDR) and `0` (DV only) name no transfer function, so the tags
        // stand — except that a DV-only base layer with no tags at all is the
        // PQ-coded signal the specification defines.
        assert_eq!(record(2).base_layer_kind(HdrKind::Hlg), HdrKind::Hlg);
        assert_eq!(record(0).base_layer_kind(HdrKind::Hlg), HdrKind::Hlg);
        assert_eq!(record(0).base_layer_kind(HdrKind::Sdr), HdrKind::Pq);
    }

    #[test]
    fn sdr_frames_are_left_alone() {
        let mapper = ToneMapper::new(HdrInfo::default());
        assert!(mapper.is_identity());
        let mut pixels = vec![10, 20, 30, 255, 200, 100, 50, 255];
        let before = pixels.clone();
        mapper.apply(&mut pixels);
        assert_eq!(pixels, before, "an SDR frame must not be touched");
    }

    #[test]
    fn tone_mapping_a_pq_frame_keeps_the_picture_readable() {
        let mapper = ToneMapper::new(HdrInfo {
            kind: HdrKind::Pq,
            bt2020: true,
            dovi: None,
        });
        assert!(!mapper.is_identity());

        // Black, a PQ grey near reference white, and a 1345-nit highlight.
        let mut pixels = vec![0, 0, 0, 255, 130, 130, 130, 255, 200, 200, 200, 255];
        let alpha: Vec<u8> = pixels.iter().skip(3).step_by(4).copied().collect();
        mapper.apply(&mut pixels);

        assert_eq!(&pixels[0..3], &[0, 0, 0], "black must stay black");
        assert_eq!(
            pixels.iter().skip(3).step_by(4).copied().collect::<Vec<_>>(),
            alpha,
            "alpha is not the tone mapper's business"
        );
        let grey = |i: usize| pixels[i * 4];
        assert!(grey(0) <= grey(1), "greys inverted");
        assert!(grey(1) <= grey(2), "greys inverted");
        assert!(grey(2) > 200, "the highlight collapsed to {}", grey(2));
        assert!(grey(2) < 255, "the highlight clipped to white");
        assert!(grey(1) < grey(2), "highlight and reference white are equal");
    }

    #[test]
    fn tone_mapping_a_4k_frame_is_affordable() {
        // Not a benchmark: a guard. If this ever takes hundreds of
        // milliseconds, the player has stopped being able to show HDR content
        // in real time and the mapping has to move to the GPU.
        let mapper = ToneMapper::new(HdrInfo {
            kind: HdrKind::Pq,
            bt2020: true,
            dovi: None,
        });
        for (label, pixels) in [("1080p", 1920 * 1080), ("4K", 3840 * 2160)] {
            let mut frame = vec![128u8; pixels * 4];
            let started = std::time::Instant::now();
            mapper.apply(&mut frame);
            let millis = started.elapsed().as_secs_f64() * 1000.0;
            eprintln!("tone mapping {label}: {millis:.1} ms per frame");
            assert!(
                millis < 400.0,
                "{label} tone mapping took {millis:.1} ms, which cannot keep up"
            );
        }
    }
}

