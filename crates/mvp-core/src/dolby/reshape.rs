//! The Dolby Vision reshaping curve, evaluated on the CPU.
//!
//! A Dolby Vision base layer is not the master: it is the master pushed through
//! a *reshaping* the encoder chose, and the RPU carries the inverse of it — a
//! piecewise polynomial for the luma, an MMR fit for the chroma, and optionally
//! the parameters of the non-linear inverse quantisation that a Profile 7
//! enhancement layer's residual is coded with. Applying it is what makes the
//! picture look like the grade rather than like the base layer: measured on the
//! real Profile 8.4 sample this machine has (a phone recording, so no
//! enhancement layer at all) skipping it moves the mean luma by 16% and the peak
//! by 20%, at 29 dB PSNR against the same frame with it applied.
//!
//! # Where this comes from, and what has been checked
//!
//! The curve shapes and the coefficient scaling are the ones libplacebo
//! implements (`src/shaders/colorspace.c`: `pl_shader_dovi_reshape`,
//! `sh_dovi_compose_nlq`), and its public `pl_dovi_metadata` documents what each
//! number means. libplacebo is the implementation mpv renders Dolby Vision with,
//! which makes it the closest thing to a reference this project can check
//! against — and the ffmpeg build this repository links has the `libplacebo`
//! filter, so the same frame *can* be rendered by both and compared
//! (`tmp/dv-compare.ps1`, and `examples/dv_reshape_dump.rs` for this side).
//!
//! That comparison, on the one Dolby Vision sample this machine has (a Profile
//! 8.4 phone recording: a six-piece luma polynomial and MMR chroma), says:
//!
//! * **The luma curve agrees.** Applying it alone changes the picture at 28.5 dB
//!   PSNR; libplacebo's own `apply_dolbyvision` changes it at 29.4 dB, and the
//!   mean luma moves by the same order. The curve evaluation, the piece
//!   selection and the pivot normalization are therefore exercising the same
//!   maths.
//! * **The chroma curve does not agree yet.** Adding it on top moves the mean
//!   luma much further than libplacebo's does — so the MMR stage and this
//!   implementation disagree somewhere in the input domain, the siting or the
//!   clamp. Its evaluation *is* faithful at the neutral point (evaluating the
//!   sample's own coefficients at a neutral chroma returns 0.4909 against 0.5),
//!   which is why the rest of it is not simply wrong; it is unverified.
//!
//! Hence: **the whole reshaping is off by default** (`dv_reshape` in the settings
//! document, and in `Rgbaconverter::set_dv_reshape`). Shipping a colour
//! transform whose chroma half has not been shown to match a reference is
//! exactly what the Dolby Vision module docs say this project does not do —
//! turning it on is the user's decision, made with that stated in the settings
//! window and in the release notes.
//!
//! # What is *not* done here
//!
//! The enhancement layer's residual is composed by [`DvReshape::apply`] when a
//! frame is handed to it. Feeding it one is the engine's job, and it is the one
//! part of this pipeline that no Dolby Vision file on this machine exercises:
//! every sample at hand is single-layer (`el_present` is false), so the NLQ path
//! is covered by unit tests built from the reference's own formulas and by
//! nothing else. See the module docs of `dolby.rs`.
//!
//! # The signal this operates on
//!
//! The three components are the base layer's own coded values — Y, Cb and Cr —
//! normalized by `2^bl_bit_depth - 1`, *without* a limited-range expansion and
//! *without* centring the chroma. That is not an approximation: the RPU's pivots
//! live in that domain (a 10-bit sample's pivots run from 63 to 940, the
//! limited-range black and white codes), and both the range and the offset
//! belong to the YCbCr→RGB step that follows, which the scaler already applies.
//!
//! # What is *not* done here
//!
//! The enhancement layer's residual is composed by [`DvReshape::apply`] when a
//! frame is handed to it. Feeding it one is the engine's job, and it is the one
//! part of this pipeline that no Dolby Vision file on this machine exercises —
//! see the module docs of `dolby.rs` for the state of that.

use super::{DvCurve, DvMappingSummary, DvNlq, DvNlqParams, DvRpuHeader, DV_MAX_PIECES};

/// One component's curve, in the units it is evaluated in.
#[derive(Debug, Clone, PartialEq)]
struct Curve {
    /// Lower and upper pivot, normalized: the range the output is clamped to.
    lo: f32,
    hi: f32,
    /// Interior pivots: the piece index is the last one that is not above the
    /// signal.
    interior: [f32; DV_MAX_PIECES],
    /// How many interior pivots are actually used.
    interior_len: usize,
    /// One entry per piece.
    pieces: [Piece; DV_MAX_PIECES],
}

/// One piece of a component's curve.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Piece {
    /// `c0 + c1·x + c2·x²`.
    Polynomial { coef: [f32; 3] },
    /// A multivariate fit over the three components.
    Mmr {
        /// `1`, `2` or `3`.
        order: u8,
        /// The constant term.
        constant: f32,
        /// One row of seven weights per order.
        weights: [[f32; 7]; 3],
    },
}

impl Piece {
    /// `true` when the piece leaves its input alone.
    fn is_identity(self) -> bool {
        match self {
            Piece::Polynomial { coef } => {
                coef[0].abs() < 1e-6 && (coef[1] - 1.0).abs() < 1e-6 && coef[2].abs() < 1e-6
            }
            Piece::Mmr { .. } => false,
        }
    }
}

impl Curve {
    /// A curve that maps every signal to itself, so a component the RPU did not
    /// describe is left alone.
    fn identity() -> Self {
        Curve {
            lo: 0.0,
            hi: 1.0,
            interior: [0.0; DV_MAX_PIECES],
            interior_len: 0,
            pieces: [Piece::Polynomial {
                coef: [0.0, 1.0, 0.0],
            }; DV_MAX_PIECES],
        }
    }
}

/// The non-linear inverse quantisation a Profile 7 enhancement layer is coded
/// with, normalized the way libplacebo's `pl_dovi_metadata` documents it:
///
/// ```text
/// offset    = nlq_offset / (2^el_bit_depth - 1)
/// slope     = (2^el_bit_depth - 1) · linear_deadzone_slope / 2^coef_log2_denom
/// threshold = (linear_deadzone_threshold - linear_deadzone_slope / 2) / 2^coef_log2_denom
/// ```
///
/// The two folded factors are the point of the shape: the `(2^eld - 1)` scales
/// the residual into the normalized domain, and the half-step turns the
/// quantiser's rounding into an offset.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Nlq {
    offset: [f32; 3],
    slope: [f32; 3],
    threshold: [f32; 3],
}

impl Nlq {
    /// The residual one enhancement-layer pixel contributes.
    ///
    /// `sign(0) == 0` is not a detail: it is the specification's carve-out for
    /// an exactly neutral residual, which must contribute nothing rather than a
    /// step of `threshold`.
    fn residual(&self, el: [f32; 3]) -> [f32; 3] {
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            let centered = el[c] - self.offset[c];
            out[c] = centered.signum() * (centered.abs() * self.slope[c] + self.threshold[c]);
        }
        out
    }
}

/// Normalize the RPU's NLQ parameters, as documented on [`Nlq`].
fn nlq_from(params: &[DvNlqParams; 3], header: &DvRpuHeader) -> Nlq {
    let denom = 2f32.powi(i32::from(header.coef_log2_denom.min(62)));
    let el_max = ((1u32 << header.el_bit_depth.clamp(8, 16)) - 1) as f32;
    let mut nlq = Nlq {
        offset: [0.0; 3],
        slope: [0.0; 3],
        threshold: [0.0; 3],
    };
    for (c, raw) in params.iter().enumerate() {
        nlq.offset[c] = f32::from(raw.nlq_offset) / el_max;
        nlq.slope[c] = el_max * raw.linear_deadzone_slope as f32 / denom;
        nlq.threshold[c] =
            (raw.linear_deadzone_threshold as f32 - raw.linear_deadzone_slope as f32 / 2.0) / denom;
    }
    nlq
}

/// The reshaping a Dolby Vision file needs, ready to be applied to pixels.
///
/// Built once per file — the curves do not change between frames, only the
/// brightness metadata does — applied once per frame, and only when there is
/// something to do: [`DvReshape::is_identity`] is what keeps a file whose curves
/// are all identity off the pixel path entirely.
#[derive(Debug, Clone, PartialEq)]
pub struct DvReshape {
    curves: [Curve; 3],
    /// The enhancement-layer residual, when the RPU carries the parameters for
    /// one (`nlq_active`).
    nlq: Option<Nlq>,
    /// The bit depth the RPU's pivots are expressed in.
    bl_bit_depth: u8,
    /// How many components the RPU actually describes.
    components: u8,
}

impl DvReshape {
    /// Read the reshaping out of an RPU's mapping.
    ///
    /// `None` when the RPU carries no mapping, or a piece this player does not
    /// know how to evaluate — refusing is the only honest answer to a curve that
    /// cannot be applied, because the alternative is a picture nobody can
    /// predict.
    pub fn from_mapping(mapping: &DvMappingSummary, header: Option<&DvRpuHeader>) -> Option<Self> {
        let header = header.copied().unwrap_or_default();
        // The coefficients are fixed point with this denominator (FFmpeg's
        // header: "denominator exponent of the fixed-point coefficients").
        let denom = 2f32.powi(i32::from(header.coef_log2_denom.min(62)));
        let bl_max = ((1u32 << header.bl_bit_depth.clamp(8, 16)) - 1) as f32;
        let mut curves = [Curve::identity(), Curve::identity(), Curve::identity()];
        let mut components = 0;
        for (c, curve) in curves.iter_mut().enumerate() {
            let raw: &DvCurve = &mapping.curves[c];
            if raw.num_pivots < 2 {
                continue;
            }
            components += 1;
            let last = usize::from(raw.num_pivots.min((DV_MAX_PIECES + 1) as u8)) - 1;
            let pivot = |i: usize| f32::from(raw.pivots[i]) / bl_max;
            curve.lo = pivot(0);
            curve.hi = pivot(last);
            curve.interior_len = last.saturating_sub(1).min(DV_MAX_PIECES);
            for i in 0..curve.interior_len {
                curve.interior[i] = pivot(i + 1);
            }
            for i in 0..last.min(DV_MAX_PIECES) {
                curve.pieces[i] = match raw.method[i] {
                    // Polynomial: `x^0`, `x^1`, `x^2`, in the order FFmpeg's
                    // header lists them and libplacebo evaluates them.
                    0 => Piece::Polynomial {
                        coef: [
                            raw.poly_coef[i][0] as f32 / denom,
                            raw.poly_coef[i][1] as f32 / denom,
                            raw.poly_coef[i][2] as f32 / denom,
                        ],
                    },
                    // MMR: up to three orders, seven weights each.
                    1 => {
                        let order = raw.mmr_order[i].min(3);
                        let mut weights = [[0.0f32; 7]; 3];
                        for (row, coef) in weights
                            .iter_mut()
                            .zip(raw.mmr_coef[i].iter())
                            .take(usize::from(order))
                        {
                            for (slot, value) in row.iter_mut().zip(coef.iter()) {
                                *slot = *value as f32 / denom;
                            }
                        }
                        Piece::Mmr {
                            order,
                            constant: raw.mmr_constant[i] as f32 / denom,
                            weights,
                        }
                    }
                    other => {
                        log::warn!("杜比视界映射方式 {other} 未知，未应用重塑");
                        return None;
                    }
                };
            }
        }
        let nlq =
            (mapping.nlq == DvNlq::LinearDeadzone).then(|| nlq_from(&mapping.nlq_params, &header));
        Some(DvReshape {
            curves,
            nlq,
            bl_bit_depth: header.bl_bit_depth,
            components,
        })
    }

    /// `true` when applying this would not change a single pixel.
    ///
    /// A file whose curves are identity — one polynomial piece, `0 + x` — is
    /// left on the existing path rather than paying for a full-frame pass that
    /// provably does nothing.
    pub fn is_identity(&self) -> bool {
        self.components == 0
            || (self.nlq.is_none()
                && self
                    .curves
                    .iter()
                    .all(|curve| curve.interior_len == 0 && curve.pieces[0].is_identity()))
    }

    /// `true` when the RPU carries the parameters for an enhancement layer's
    /// residual.
    pub fn nlq_active(&self) -> bool {
        self.nlq.is_some()
    }

    /// The bit depth the RPU's pivots are expressed in, which is what the pixels
    /// have to be normalized by for the two to agree.
    pub fn base_layer_bit_depth(&self) -> u8 {
        self.bl_bit_depth
    }

    /// Apply the reshaping to one pixel's components.
    ///
    /// `bl` are the base layer's components normalized by
    /// `2^base_layer_bit_depth - 1`; `el` is the enhancement layer's, in its own
    /// normalization, when the frame has one. The result is in the base layer's
    /// domain, ready to be written back as code values.
    pub fn apply(&self, bl: [f32; 3], el: Option<[f32; 3]>) -> [f32; 3] {
        let sig = [
            bl[0].clamp(0.0, 1.0),
            bl[1].clamp(0.0, 1.0),
            bl[2].clamp(0.0, 1.0),
        ];
        let mut out = [
            self.apply_component(0, sig),
            self.apply_component(1, sig),
            self.apply_component(2, sig),
        ];
        // The residual is composed *after* the reshaping, which is the order
        // libplacebo emits: the curves describe the base layer, and the
        // enhancement layer contributes its dequantised residual on top.
        if let (Some(nlq), Some(el)) = (&self.nlq, el) {
            for (component, add) in out.iter_mut().zip(nlq.residual(el)) {
                *component += add;
            }
        }
        out
    }

    /// One component's curve, evaluated at the shared (clamped) signal.
    fn apply_component(&self, component: usize, sig: [f32; 3]) -> f32 {
        let curve = &self.curves[component];
        let s = sig[component];
        // The piece is the last interior pivot that is not above the signal:
        // the selection libplacebo's branch tree makes.
        let mut index = 0;
        for i in 0..curve.interior_len {
            if s >= curve.interior[i] {
                index = i + 1;
            }
        }
        let value = match curve.pieces[index.min(DV_MAX_PIECES - 1)] {
            Piece::Polynomial { coef } => (coef[2] * s + coef[1]) * s + coef[0],
            Piece::Mmr {
                order,
                constant,
                weights,
            } => mmr(sig, order, constant, &weights),
        };
        value.clamp(curve.lo, curve.hi)
    }
}

/// The multivariate fit, term for term as libplacebo evaluates it.
///
/// The basis is built from the three components `x`, `y`, `z`: `[x, y, z]` and
/// `[x²y, y²z, z²x, x²yz]` for the first order, and each higher order raises
/// those by one more power of the signal — which is why each order carries seven
/// weights rather than six.
fn mmr(sig: [f32; 3], order: u8, constant: f32, weights: &[[f32; 7]; 3]) -> f32 {
    let [x, y, z] = sig;
    let cross = [x * x * y, y * y * z, z * z * x, x * x * y * z];
    let mut value = constant;
    for (k, term) in [x, y, z].into_iter().enumerate() {
        value += weights[0][k] * term;
    }
    for (k, term) in cross.into_iter().enumerate() {
        value += weights[0][3 + k] * term;
    }
    if order >= 2 {
        for (k, term) in [x * x, y * y, z * z].into_iter().enumerate() {
            value += weights[1][k] * term;
        }
        for (k, term) in cross.into_iter().enumerate() {
            value += weights[1][3 + k] * term * term;
        }
    }
    if order >= 3 {
        for (k, term) in [x * x * x, y * y * y, z * z * z].into_iter().enumerate() {
            value += weights[2][k] * term;
        }
        for (k, term) in cross.into_iter().enumerate() {
            value += weights[2][3 + k] * term * term * term;
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample's scaling: a 10-bit base layer, a 10-bit enhancement layer,
    /// and `2^23` as the coefficient denominator.
    fn header() -> DvRpuHeader {
        DvRpuHeader {
            bl_bit_depth: 10,
            el_bit_depth: 10,
            coef_log2_denom: 23,
            ..DvRpuHeader::default()
        }
    }

    /// The fixed-point denominator of [`header`], as an integer.
    const SCALE: i64 = 1 << 23;

    /// An RPU mapping that describes nothing: every component is left alone.
    fn identity_mapping() -> DvMappingSummary {
        DvMappingSummary::default()
    }

    /// A luma curve made of `pieces`, each `(method, coefficients)`, with the
    /// pivots given for the whole curve.
    fn curve_mapping(pivots: [u16; 9], pieces: &[(u8, [i64; 3])]) -> DvMappingSummary {
        let mut mapping = identity_mapping();
        let mut methods = [0u8; DV_MAX_PIECES];
        let mut coefs = [[0i64; 3]; DV_MAX_PIECES];
        for (i, (method, coef)) in pieces.iter().enumerate() {
            methods[i] = *method;
            coefs[i] = *coef;
        }
        mapping.curves[0] = DvCurve {
            num_pivots: pieces.len() as u8 + 1,
            pivots,
            method: methods,
            poly_coef: coefs,
            ..DvCurve::default()
        };
        mapping
    }

    #[test]
    fn an_identity_mapping_is_recognized_and_costs_nothing() {
        let reshape = DvReshape::from_mapping(&identity_mapping(), Some(&header())).expect("built");
        assert!(reshape.is_identity(), "no curve at all is not a reshape");
        assert!(!reshape.nlq_active());

        // And a curve that is `0 + 1·x` is the same statement written out.
        let flat = curve_mapping([64, 940, 0, 0, 0, 0, 0, 0, 0], &[(0, [0, SCALE, 0])]);
        let reshape = DvReshape::from_mapping(&flat, Some(&header())).expect("built");
        assert!(reshape.is_identity(), "0 + x is not a reshape");
    }

    #[test]
    fn a_polynomial_curve_maps_its_pieces_and_clamps_at_the_pivots() {
        // Two pieces: identity below 512, doubling above it.
        let pivots = [64, 512, 940, 0, 0, 0, 0, 0, 0];
        let mapping = curve_mapping(pivots, &[(0, [0, SCALE, 0]), (0, [0, 2 * SCALE, 0])]);
        let reshape = DvReshape::from_mapping(&mapping, Some(&header())).expect("built");
        assert!(!reshape.is_identity());

        // Below the interior pivot: the first piece, which is identity.
        let low = reshape.apply([200.0 / 1023.0, 0.5, 0.5], None);
        assert!((low[0] - 200.0 / 1023.0).abs() < 1e-4, "{low:?}");
        // Above it: doubled — and the doubling pushes past the last pivot, so
        // the clamp is what decides.
        let high = reshape.apply([800.0 / 1023.0, 0.5, 0.5], None);
        assert!((high[0] - 940.0 / 1023.0).abs() < 1e-4, "{high:?}");
        // Below the first pivot: held at it rather than leaving the domain.
        let under = reshape.apply([0.0, 0.0, 0.0], None);
        assert!((under[0] - 64.0 / 1023.0).abs() < 1e-6, "{under:?}");
    }

    #[test]
    fn the_piece_is_picked_by_the_last_interior_pivot_not_above_the_signal() {
        // Three pieces with distinguishable coefficients: 1x, 2x, 3x.
        let pivots = [0, 300, 600, 1023, 0, 0, 0, 0, 0];
        let mapping = curve_mapping(
            pivots,
            &[
                (0, [0, SCALE, 0]),
                (0, [0, 2 * SCALE, 0]),
                (0, [0, 3 * SCALE, 0]),
            ],
        );
        let reshape = DvReshape::from_mapping(&mapping, Some(&header())).expect("built");
        // 0.1 is below the first interior pivot → 1x.
        let first = reshape.apply([0.1, 0.0, 0.0], None);
        assert!((first[0] - 0.1).abs() < 1e-4, "{first:?}");
        // 0.4 sits between them → 2x.
        let second = reshape.apply([0.4, 0.0, 0.0], None);
        assert!((second[0] - 0.8).abs() < 1e-4, "{second:?}");
        // 0.7 is above both → 3x, clamped to the last pivot.
        let third = reshape.apply([0.7, 0.0, 0.0], None);
        assert!((third[0] - 1.0).abs() < 1e-4, "{third:?}");
    }

    #[test]
    fn an_mmr_piece_evaluates_its_basis_terms() {
        let mut mapping = identity_mapping();
        mapping.curves[0] = DvCurve {
            num_pivots: 2,
            pivots: [0, 1023, 0, 0, 0, 0, 0, 0, 0],
            method: [1; DV_MAX_PIECES],
            mmr_order: [1; DV_MAX_PIECES],
            mmr_constant: [SCALE / 20; DV_MAX_PIECES],
            // 0.1·x + 0.2·y + 0.3·z + 0.05·(x²y)
            mmr_coef: [[[SCALE / 10, SCALE / 5, 3 * SCALE / 10, SCALE / 20, 0, 0, 0]; 3];
                DV_MAX_PIECES],
            ..DvCurve::default()
        };
        let reshape = DvReshape::from_mapping(&mapping, Some(&header())).expect("built");

        let sig = [0.25f32, 0.5, 0.25];
        let expected = 0.05 + 0.1 * 0.25 + 0.2 * 0.5 + 0.3 * 0.25 + 0.05 * (0.25 * 0.25 * 0.5);
        let out = reshape.apply(sig, None);
        assert!((out[0] - expected).abs() < 1e-4, "{out:?} vs {expected}");
        // The components the RPU did not describe pass through.
        assert!(
            (out[1] - 0.5).abs() < 1e-4 && (out[2] - 0.25).abs() < 1e-4,
            "{out:?}"
        );

        // The same terms at full scale: the fit is linear in them, so the whole
        // sum is what the weights say — 0.05 + 0.1 + 0.2 + 0.3 + 0.05.
        let over = reshape.apply([1.0, 1.0, 1.0], None);
        assert!((over[0] - 0.7).abs() < 1e-4, "{over:?}");
    }

    #[test]
    fn the_nlq_residual_is_signed_and_skips_an_exactly_neutral_sample() {
        let nlq = Nlq {
            offset: [0.5, 0.5, 0.5],
            slope: [0.5, 0.5, 0.5],
            threshold: [0.0, 0.0, 0.0],
        };
        // Exactly neutral: contributing nothing is the specification's carve-out
        // (a signed zero has a zero sign, not a negative one).
        assert_eq!(nlq.residual([0.5, 0.5, 0.5]), [0.0, 0.0, 0.0]);
        let up = nlq.residual([0.75, 0.5, 0.5]);
        assert!((up[0] - 0.125).abs() < 1e-6, "{up:?}");
        let down = nlq.residual([0.25, 0.5, 0.5]);
        assert!((down[0] + 0.125).abs() < 1e-6, "{down:?}");
    }

    #[test]
    fn the_nlq_factors_are_folded_the_way_the_reference_documents() {
        let params = [
            DvNlqParams {
                nlq_offset: 512,
                vdr_in_max: 0,
                linear_deadzone_slope: 1 << 20,
                linear_deadzone_threshold: 1 << 22,
            },
            DvNlqParams::default(),
            DvNlqParams::default(),
        ];
        let nlq = nlq_from(&params, &header());
        assert!((nlq.offset[0] - 512.0 / 1023.0).abs() < 1e-6, "{nlq:?}");
        let slope = 1023.0 * (1u32 << 20) as f32 / SCALE as f32;
        assert!((nlq.slope[0] - slope).abs() < 1e-3, "{nlq:?}");
        let threshold = ((1u32 << 22) as f32 - (1u32 << 20) as f32 / 2.0) / SCALE as f32;
        assert!((nlq.threshold[0] - threshold).abs() < 1e-6, "{nlq:?}");
    }

    #[test]
    fn an_enhancement_layer_residual_reaches_the_output_only_when_one_is_given() {
        // An identity luma curve plus an active NLQ: the only thing the reshape
        // can do is compose the residual.
        let mut mapping = curve_mapping([64, 940, 0, 0, 0, 0, 0, 0, 0], &[(0, [0, SCALE, 0])]);
        mapping.nlq = DvNlq::LinearDeadzone;
        mapping.nlq_params[0] = DvNlqParams {
            nlq_offset: 512,
            vdr_in_max: 0,
            linear_deadzone_slope: 1 << 21,
            linear_deadzone_threshold: 0,
        };
        let reshape = DvReshape::from_mapping(&mapping, Some(&header())).expect("built");
        assert!(reshape.nlq_active());
        assert!(!reshape.is_identity(), "an active NLQ is work to do");

        let bl = [0.5f32, 0.5, 0.5];
        let without = reshape.apply(bl, None);
        assert!((without[0] - 0.5).abs() < 1e-4, "{without:?}");
        // An enhancement layer above its neutral lifts the picture, and one
        // below it lowers it.
        let up = reshape.apply(bl, Some([0.6, 0.5, 0.5]));
        assert!(up[0] > bl[0], "{up:?}");
        let down = reshape.apply(bl, Some([0.4, 0.5, 0.5]));
        assert!(down[0] < bl[0], "{down:?}");
    }

    #[test]
    fn a_mapping_this_player_cannot_evaluate_is_refused_rather_than_guessed() {
        let mut mapping = identity_mapping();
        mapping.curves[0].num_pivots = 2;
        mapping.curves[0].method[0] = 7;
        assert!(
            DvReshape::from_mapping(&mapping, Some(&header())).is_none(),
            "an unknown mapping method must not be applied"
        );
    }
}
