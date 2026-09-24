//! Visual design tokens and the `egui` style the whole player is built from.
//!
//! The player ships **five palettes** and the user picks one in the settings page
//! ([`Palette`]): the original blue-on-neutral-grey it started with, and four
//! Morandi ones. A Morandi palette is a ramp of warm, low-chroma greys
//! (`bg` → `panel` → `elevated` → `sunken`), one dusty accent that carries every
//! interactive highlight, and a small set of muted semantic colours (success,
//! warning, danger) used only for state. Nothing in one is as saturated as it
//! could be, and that is the point: a media player is watched next to its own
//! picture, so the chrome stays quiet and the *picture* stays the brightest, most
//! colourful thing on screen. Their greys are warm (a hint of brown rather than
//! the blue of a system dark mode) and their translucent state fills are their
//! own white, so no cold grey appears next to them. The
//! `no_colour_in_the_palette_shouts` test at the bottom of this file is what keeps
//! that property from being eroded one "cleanup" at a time; it asks
//! `Palette::is_muted` rather than listing exceptions, so a new Morandi palette is
//! held to it the moment it is added.
//!
//! A palette is a choice of **chrome**, never of hierarchy: switching one changes
//! hues and nothing else. That is why the translucent fills are derived from each
//! palette's own white at *shared* alphas ([`Recipe`]) instead of being spelled
//! out five times — and why [`Palette::Blue`] has to keep passing every contrast
//! floor. It is exempt from the chroma ceilings alone, which is exactly what
//! "keep the original" means.
//!
//! Three rules taken from the Human Interface Guidelines hold the whole file
//! together:
//!
//! 1. **Hierarchy comes from colour, not from lines.** Surfaces are separated by
//!    a translucent hairline ([`Tokens::separator`]) rather than a grey
//!    stroke, so a separator is correct on any surface it is drawn on.
//! 2. **Everything is measured on a 4 pt grid** ([`space`]) with Apple's corner
//!    radii ([`radius`]), so unrelated panels still line up with each other.
//! 3. **The accent means "this is interactive, or this is current."** It is never
//!    decoration. Selection fills use [`Tokens::accent_soft`] so text on top of
//!    them keeps its contrast.
//!
//! Geometry, type and spacing are deliberately untouched by a palette: choosing
//! one is a recolouring, not a redesign, and the layout tests still pin the same
//! values they did before.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use egui::{
    Color32, Context, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals,
};
use serde::{Deserialize, Serialize};

/// A colour with an alpha channel, built from a hex literal at compile time.
/// A colour without an alpha channel, built from a hex literal at compile time.
const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// A colour with an alpha channel, built from a hex literal.
///
/// Not `const`: `Color32::from_rgba_unmultiplied` is not a const fn, so the
/// translucent part of the palette is built when the tokens are.
fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// A colour straight from the literal a palette is written as — `0xRRGGBB`, the
/// shape the values are read off a design in.
///
/// Palettes are long enough that a row of `rgb(0x1E, 0x1D, 0x19)`s hides the
/// differences between them; one hex number per colour keeps a palette readable as
/// the table it is.
const fn hex(value: u32) -> Color32 {
    rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

/// The colour palettes the player ships with, one at a time.
///
/// [`Palette::Blue`] is what the player looked like before this was a setting, kept
/// because that look is a legitimate preference. The rest are Morandi ones, which
/// is what the interface is designed around; [`Palette::Clay`] is the default.
///
/// Every variant is a *dark* palette. There is deliberately no light one and no
/// "follow the system": `Theme::install` documents what following the OS theme
/// used to do to this interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Palette {
    /// The original: Apple blue on neutral, faintly blue-grey surfaces.
    Blue,
    /// Warm grey with a dusty clay accent.
    #[default]
    Clay,
    /// Warm grey with a muted sage-green accent.
    Sage,
    /// Cool grey with a dusty slate-blue accent.
    Slate,
    /// Warm grey with a muted mauve accent.
    Mauve,
}

impl Palette {
    /// The marker a label carries when its palette is the default one.
    ///
    /// Only the tests read this. A `&'static str` cannot be assembled at run time, so
    /// the marker has to sit in the label literal itself — and this constant is what
    /// `every_palette_is_offered_once` compares that literal against, so the two
    /// cannot drift apart.
    #[cfg(test)]
    pub const DEFAULT_MARK: &'static str = "（默认）";

    /// Every palette, in the order the settings page offers them.
    ///
    /// The original comes first: a user who wants the old look back should not have
    /// to read four Chinese colour names to find it.
    pub fn all() -> [Palette; 5] {
        [
            Palette::Blue,
            Palette::Clay,
            Palette::Sage,
            Palette::Slate,
            Palette::Mauve,
        ]
    }

    /// The name shown in the settings page.
    ///
    /// The default carries its own marker, and `the_default_palette_says_so` keeps
    /// the marker on the palette that is actually the default.
    pub fn label(self) -> &'static str {
        match self {
            Palette::Blue => "原版蓝",
            Palette::Clay => "陶土灰（默认）",
            Palette::Sage => "灰绿",
            Palette::Slate => "雾蓝",
            Palette::Mauve => "藕紫",
        }
    }

    /// The same list in the shape `widgets::combo_row` wants.
    pub fn choices() -> [(Palette, &'static str); 5] {
        Self::all().map(|palette| (palette, palette.label()))
    }

    /// Whether the palette promises that every colour in it is muted.
    ///
    /// True of the Morandi palettes and false of [`Palette::Blue`], whose accent is
    /// a fully saturated blue — that *is* what choosing the original look means.
    /// The chromatic ceilings in `no_colour_in_the_palette_shouts` ask this instead
    /// of listing exceptions, so anything added here is held to them by default.
    ///
    /// Only the tests ask: nothing the player ships draws a verdict on its own
    /// palette, and this is here so that the answer lives next to the palettes rather
    /// than in the test that happens to need it.
    #[cfg(test)]
    pub fn is_muted(self) -> bool {
        !matches!(self, Palette::Blue)
    }

    /// The light surround the image viewer paints behind a still that does not fill
    /// the window, as RGB.
    ///
    /// Part of the palette rather than one constant, because a surround is judged
    /// against the chrome around it: the warm mat that suits the Morandi greys
    /// reads as a colour cast under the blue one, and the neutral grey the original
    /// shipped with reads as a cold frame around them. Only the *light* surround is
    /// here — black and the checkerboard are choices about judging a picture, not
    /// colours.
    pub fn light_surround(self) -> [u8; 3] {
        match self {
            Palette::Blue => [214, 214, 216],
            Palette::Clay => [216, 211, 202],
            Palette::Sage => [211, 214, 205],
            Palette::Slate => [206, 210, 216],
            Palette::Mauve => [214, 209, 215],
        }
    }
}

/// Every colour the interface uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    /// Window background, behind everything.
    pub bg: Color32,
    /// Side bars and toolbars.
    pub panel: Color32,
    /// Cards, popups and the settings window.
    pub elevated: Color32,
    /// Inset areas such as the seek bar trough.
    pub sunken: Color32,
    /// Hover fill for list rows and icon buttons.
    pub hover: Color32,
    /// Active/pressed fill.
    pub active: Color32,
    /// Hairline separators.
    pub border: Color32,
    /// Stronger separators and outlines.
    pub border_strong: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text, labels and captions.
    pub text_weak: Color32,
    /// Disabled text.
    pub text_muted: Color32,
    /// The one accent colour.
    pub accent: Color32,
    /// Accent, lightened for hover.
    pub accent_hover: Color32,
    /// Text on top of the accent.
    pub on_accent: Color32,
    /// Playback progress fill.
    pub progress: Color32,
    /// Seek bar trough.
    pub track: Color32,
    /// Positive state (volume, connected).
    pub success: Color32,
    /// Warnings (A–B loop armed, unsupported track).
    pub warning: Color32,
    /// Errors and destructive actions.
    pub danger: Color32,
    /// The video letterbox area — pure black, like a cinema.
    pub letterbox: Color32,
    /// Hairline between rows of a grouped list.
    ///
    /// A translucent white rather than a grey: the same value then reads as a
    /// separator on the panel, on a card and on a popup.
    pub separator: Color32,
    /// The accent at list-selection strength, for selected rows and pills.
    ///
    /// Kept as a translucent fill instead of a solid colour so text drawn over
    /// it keeps its own contrast.
    pub accent_soft: Color32,
    /// The accent under the pointer's finger — one step darker than [`Tokens::accent`].
    pub accent_pressed: Color32,
    /// Keyboard-focus ring, drawn around the focused control.
    pub focus_ring: Color32,
    /// Placeholder fill of a skeleton row, while its content loads.
    pub skeleton: Color32,
    /// The brighter band that travels across a skeleton row.
    pub skeleton_highlight: Color32,
    /// Error, at banner strength.
    pub danger_soft: Color32,
}

/// The colours of one palette, before the translucent fills are derived.
///
/// The fills are deliberately *not* here: they are the palette's own white
/// ([`Recipe::wash`]) at the alphas in [`alpha`], and the accent at two more. How
/// strong a hover is, how loud a selection is and how visible a focus ring is are
/// decisions about the *shape* of the interface, and they must not change when the
/// user picks a different hue — which is exactly what re-deriving them per palette
/// would have allowed.
struct Recipe {
    /// Window background, behind everything.
    bg: Color32,
    /// Side bars and toolbars.
    panel: Color32,
    /// Cards, popups and the settings window.
    elevated: Color32,
    /// Inset areas such as the seek bar trough.
    sunken: Color32,
    /// The palette's white.
    ///
    /// Every translucent fill is this colour at some alpha, so a palette keeps one
    /// temperature: the warm greys with a pure-white wash over them are a cold
    /// smudge, which is what sharing one white across the palettes would give.
    wash: Color32,
    /// Primary text.
    text: Color32,
    /// Secondary text, labels and captions.
    text_weak: Color32,
    /// Disabled text, and the settings page's explanations.
    text_muted: Color32,
    /// The one accent colour.
    accent: Color32,
    /// The accent under the pointer.
    accent_hover: Color32,
    /// The accent under the pointer's finger.
    accent_pressed: Color32,
    /// Positive state (volume, connected).
    success: Color32,
    /// Warnings (A–B loop armed, unsupported track).
    warning: Color32,
    /// Errors and destructive actions.
    danger: Color32,
}

/// How strong each translucent fill is, on every palette.
///
/// Shared numbers rather than per-palette ones — see [`Recipe`] — and how they
/// relate to each other is a property the tests at the bottom of this file hold: a
/// skeleton is quieter than the separator that replaces it, which is quieter than
/// the band that travels across it.
mod alpha {
    /// Hover fill for list rows and icon buttons.
    pub const HOVER: u8 = 0x14;
    /// Active/pressed fill.
    pub const ACTIVE: u8 = 0x24;
    /// Hairline separators.
    pub const BORDER: u8 = 0x14;
    /// Stronger separators and outlines.
    pub const BORDER_STRONG: u8 = 0x2E;
    /// Seek bar trough.
    pub const TRACK: u8 = 0x1F;
    /// Hairline between rows of a grouped list.
    pub const SEPARATOR: u8 = 0x17;
    /// Placeholder fill of a skeleton row.
    pub const SKELETON: u8 = 0x0F;
    /// The band that travels across a skeleton row.
    pub const SKELETON_HIGHLIGHT: u8 = 0x26;
    /// The accent at list-selection strength.
    pub const ACCENT_SOFT: u8 = 0x3D;
    /// Keyboard-focus ring.
    pub const ACCENT_RING: u8 = 0x8C;
    /// Error, at banner strength.
    pub const DANGER_SOFT: u8 = 0x33;
}

/// `colour` at `alpha`, for the translucent fills.
fn faint(colour: Color32, alpha: u8) -> Color32 {
    rgba(colour.r(), colour.g(), colour.b(), alpha)
}

impl Recipe {
    /// Expand into every colour the interface uses.
    fn tokens(self) -> Tokens {
        Tokens {
            bg: self.bg,
            panel: self.panel,
            elevated: self.elevated,
            sunken: self.sunken,
            hover: faint(self.wash, alpha::HOVER),
            active: faint(self.wash, alpha::ACTIVE),
            border: faint(self.wash, alpha::BORDER),
            border_strong: faint(self.wash, alpha::BORDER_STRONG),
            text: self.text,
            text_weak: self.text_weak,
            text_muted: self.text_muted,
            accent: self.accent,
            accent_hover: self.accent_hover,
            // A constant rather than a fifteenth thing each palette could get wrong:
            // every palette's accent is dark enough to carry white text, and
            // `every_palette_has_enough_contrast` measures that ratio for each of
            // them.
            on_accent: rgb(0xFF, 0xFF, 0xFF),
            // Progress is the accent: the seek bar is the loudest "here is where you
            // are" element on screen, so it speaks the interactive colour.
            progress: self.accent,
            track: faint(self.wash, alpha::TRACK),
            success: self.success,
            warning: self.warning,
            danger: self.danger,
            // The one colour no palette chooses: the black a cinema letterboxes
            // with. A letterbox that followed a palette would tint the bars around a
            // film, which is the opposite of what they are for.
            letterbox: rgb(0x00, 0x00, 0x00),
            separator: faint(self.wash, alpha::SEPARATOR),
            accent_soft: faint(self.accent, alpha::ACCENT_SOFT),
            accent_pressed: self.accent_pressed,
            focus_ring: faint(self.accent, alpha::ACCENT_RING),
            skeleton: faint(self.wash, alpha::SKELETON),
            skeleton_highlight: faint(self.wash, alpha::SKELETON_HIGHLIGHT),
            danger_soft: faint(self.danger, alpha::DANGER_SOFT),
        }
    }
}

impl Palette {
    /// The colours of this palette.
    fn recipe(self) -> Recipe {
        // The semantic trio does not change between the Morandi palettes: success,
        // warning and danger mean the same thing in all of them, so a palette is a
        // choice of chrome rather than a re-skin of state. The green is deliberately
        // *lighter* than the sage accent rather than a different hue — a success mark
        // and an interactive highlight that read as the same green are worse than two
        // greens that differ in weight.
        const MUTED: (u32, u32, u32) = (0x8FA88B, 0xC2A578, 0xB07468);
        match self {
            // The palette the player shipped with, value for value.
            Palette::Blue => Recipe {
                bg: hex(0x0D0E11),
                panel: hex(0x16181C),
                elevated: hex(0x1E2126),
                sunken: hex(0x0A0B0D),
                wash: hex(0xFFFFFF),
                text: hex(0xF5F5F7),
                text_weak: hex(0xA1A7B3),
                text_muted: hex(0x7E838D),
                accent: hex(0x0A84FF),
                accent_hover: hex(0x3D9BFF),
                accent_pressed: hex(0x0670E0),
                // The one palette that does not use the muted trio: this is the
                // system colour set it was designed with.
                success: hex(0x32D74B),
                warning: hex(0xFF9F0A),
                danger: hex(0xFF453A),
            },
            // Warm near-black, warm panel, warm card: the ramp is the same shape a
            // system dark mode's — three steps plus an inset — but the hue is pulled
            // towards brown rather than blue, which is what makes the chrome sit
            // quietly next to a picture instead of tinting it.
            Palette::Clay => Recipe {
                bg: hex(0x151412),
                panel: hex(0x1E1D19),
                elevated: hex(0x272520),
                sunken: hex(0x11100E),
                wash: hex(0xECE7DE),
                text: hex(0xECE7DE),
                text_weak: hex(0xABA59A),
                // 3.7:1 on the panel was not enough for an 11 pt caption, and the
                // settings page draws every explanation in this colour. At this value
                // it clears WCAG AA on the panel and still sits a clear step below
                // `text_weak`, so the three levels stay distinguishable.
                text_muted: hex(0x918B80),
                // Dusty clay: dark enough for white text on it and for itself to read
                // against the background, muted enough to be a Morandi tone rather
                // than a warning sign.
                accent: hex(0xA67A66),
                accent_hover: hex(0xB98E76),
                accent_pressed: hex(0x8E6A58),
                success: hex(MUTED.0),
                warning: hex(MUTED.1),
                danger: hex(MUTED.2),
            },
            // The same greys with a hint of green, and a sage accent. The green is in
            // the surfaces too: a *cool* neutral under a green accent reads as two
            // decisions rather than one.
            Palette::Sage => Recipe {
                bg: hex(0x131512),
                panel: hex(0x1B1E1A),
                elevated: hex(0x242722),
                sunken: hex(0x0F110E),
                wash: hex(0xE9EDE5),
                text: hex(0xE9EDE5),
                text_weak: hex(0xA9B0A2),
                text_muted: hex(0x8C9484),
                accent: hex(0x66836A),
                accent_hover: hex(0x79957D),
                accent_pressed: hex(0x547057),
                success: hex(MUTED.0),
                warning: hex(MUTED.1),
                danger: hex(MUTED.2),
            },
            // The one cool palette: a blue-grey ramp, but a *dusty* slate accent
            // rather than the original's saturated blue. This is the Morandi answer
            // for someone who finds the warm ones muddy.
            Palette::Slate => Recipe {
                bg: hex(0x121317),
                panel: hex(0x1A1C21),
                elevated: hex(0x23262C),
                sunken: hex(0x0E0F12),
                wash: hex(0xE7E9EE),
                text: hex(0xE7E9EE),
                text_weak: hex(0xA6ABB5),
                text_muted: hex(0x8B919C),
                accent: hex(0x6F7F9B),
                accent_hover: hex(0x8291A9),
                accent_pressed: hex(0x5E6C84),
                success: hex(MUTED.0),
                warning: hex(MUTED.1),
                danger: hex(MUTED.2),
            },
            // Warm greys with a violet cast and a mauve accent. The accent is the
            // furthest of the four from the brick-red danger, which is why it is a
            // mauve rather than a rose.
            Palette::Mauve => Recipe {
                bg: hex(0x151316),
                panel: hex(0x1D1B20),
                elevated: hex(0x26232A),
                sunken: hex(0x100F12),
                wash: hex(0xEBE7EE),
                text: hex(0xEBE7EE),
                text_weak: hex(0xACA5B2),
                text_muted: hex(0x918A97),
                accent: hex(0x9A7C94),
                accent_hover: hex(0xAD8EA6),
                accent_pressed: hex(0x83677D),
                success: hex(MUTED.0),
                warning: hex(MUTED.1),
                danger: hex(MUTED.2),
            },
        }
    }
}

impl Tokens {
    /// Every colour the interface uses, for `palette`.
    pub fn for_palette(palette: Palette) -> Self {
        palette.recipe().tokens()
    }
}

impl Default for Tokens {
    /// The default palette. Every colour is [`Tokens::for_palette`]'s answer for
    /// [`Palette::default`] — written once, so "the default palette" cannot come to
    /// mean two different things.
    fn default() -> Self {
        Self::for_palette(Palette::default())
    }
}

/// Spacing scale, in points. Everything is a multiple of four, apart from the
/// half-step used *inside* a control.
pub mod space {
    /// 2 pt — the gap between a glyph and its label, inside a control.
    pub const XXS: f32 = 2.0;
    /// 4 pt — hairline gaps.
    pub const XS: f32 = 4.0;
    /// 8 pt — the default gap between related controls.
    pub const SM: f32 = 8.0;
    /// 12 pt — gap between groups.
    pub const MD: f32 = 12.0;
    /// 16 pt — panel padding.
    pub const LG: f32 = 16.0;
    /// 24 pt — section separation.
    pub const XL: f32 = 24.0;
    /// 32 pt — the space between one page block and the next.
    pub const XXL: f32 = 32.0;
}

/// Corner radii, in points, following Apple's scale.
///
/// The steps matter more than the values: a chip, a control, a card and a sheet
/// must each look *deliberately* rounded relative to one another, because a
/// nearly-round corner next to a rounder one reads as a mistake.
pub mod radius {
    /// 5 pt — checkboxes, chips, list rows.
    pub const SM: f32 = 5.0;
    /// 8 pt — buttons, text fields, icon buttons.
    pub const MD: f32 = 8.0;
    /// 12 pt — cards, popovers, grouped lists.
    pub const LG: f32 = 12.0;
    /// 16 pt — the settings sheet and other floating windows.
    pub const XL: f32 = 16.0;
}

/// How much bigger the buttons are than the size the interface was laid out at.
///
/// Buttons **only**. The menu bar, the settings rows, the tabs, the switches, the type,
/// the spacing and every panel and dialog keep the size they were designed at; what
/// follows this factor is an icon button's box and its glyph, and the play/pause button.
///
/// Two containers are sized *as* "button plus its own padding" and therefore follow it —
/// the floating image toolbar and the height of the video transport bar. There is no room
/// for a bigger button inside a box that was measured around the smaller one.
pub mod button {
    /// The multiplier applied to an icon button's geometry.
    pub const SCALE: f32 = 1.3;

    /// `points`, at the current button size.
    ///
    /// A named call rather than `* SCALE` at each site: it is what tells a reader that a
    /// number is button geometry and not, say, a gap.
    pub const fn of(points: f32) -> f32 {
        points * SCALE
    }

    /// Diameter of the play/pause button, in points.
    ///
    /// Deliberately *not* a scaled design size. Scaling the design's own 48 pt (video bar) and
    /// 40 pt (audio bar) made the triangle inside the button — 58 % of the diameter — half
    /// again as large as the icon glyphs beside it, and in the video bar larger than the bar
    /// itself, so it was drawn clipped. 46 puts that glyph at 26.7 pt, optically the same as
    /// the icon buttons' 26.2; it is the largest value that still fits the 88 pt video bar
    /// (24 + 46 + 16 ≤ 88), and it makes the button the same size in both bars instead of the
    /// two sizes it used to have.
    pub const PLAY: f32 = 46.0;
}

/// Font sizes, in points.
///
/// `epaint` has no variable-weight faces, so the type hierarchy is carried by
/// size and colour; the one exception is headings, which use the bold cut of the
/// system font through [`strong_font`].
pub mod font {
    /// 11 pt — timestamps on the seek bar.
    pub const TINY: f32 = 11.0;
    /// 12 pt — captions and metadata rows.
    pub const SMALL: f32 = 12.0;
    /// 13 pt — the default UI size.
    pub const BODY: f32 = 13.0;
    /// 15 pt — the heading of a group of settings.
    pub const H3: f32 = 15.0;
    /// 20 pt — window and sheet titles.
    pub const H2: f32 = 20.0;
    /// 26 pt — the empty-state title.
    pub const H1: f32 = 26.0;
}

/// The player's complete style.
#[derive(Debug, Clone)]
#[derive(Default)]
pub struct Theme {
    /// Colour tokens.
    pub tokens: Tokens,
}

impl Theme {
    /// The style for `palette`.
    ///
    /// The palette is a *setting*, so a theme is not built once and kept: this is
    /// what `PlayerApp::install_palette` calls on start-up and again whenever the
    /// user picks a different one.
    pub fn of(palette: Palette) -> Self {
        Self {
            tokens: Tokens::for_palette(palette),
        }
    }

    /// Install the theme into `ctx`, including the CJK-capable font stack.
    ///
    /// Every palette is a dark one — no light counterpart, and nothing that follows
    /// the system — so this pins the theme rather than tracking it. Cheap to call
    /// again: the fonts go through a `OnceLock`, so a palette change only rewrites
    /// the style.
    pub fn install(&self, ctx: &Context) {
        install_fonts(ctx);

        let t = &self.tokens;
        let mut visuals = Visuals::dark();

        visuals.panel_fill = t.panel;
        visuals.window_fill = t.elevated;
        visuals.extreme_bg_color = t.sunken;
        visuals.faint_bg_color = t.hover;
        visuals.code_bg_color = t.sunken;
        visuals.window_stroke = Stroke::new(1.0_f32, t.border_strong);
        visuals.window_corner_radius = CornerRadius::same(radius::XL as u8);
        visuals.menu_corner_radius = CornerRadius::same(radius::MD as u8);
        visuals.window_shadow = shadow::window();
        visuals.popup_shadow = shadow::popup();
        visuals.override_text_color = Some(t.text);
        visuals.hyperlink_color = t.accent;
        visuals.selection.bg_fill = t.accent_soft;
        visuals.selection.stroke = Stroke::new(1.0_f32, t.text);
        visuals.slider_trailing_fill = true;

        let w = &mut visuals.widgets;
        w.noninteractive.bg_fill = t.panel;
        w.noninteractive.weak_bg_fill = t.panel;
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32, t.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, t.text_weak);
        w.noninteractive.corner_radius = CornerRadius::same(radius::MD as u8);

        w.inactive.bg_fill = t.hover;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::NONE;
        w.inactive.fg_stroke = Stroke::new(1.0_f32, t.text);
        w.inactive.corner_radius = CornerRadius::same(radius::MD as u8);

        w.hovered.bg_fill = t.active;
        w.hovered.weak_bg_fill = t.hover;
        w.hovered.bg_stroke = Stroke::NONE;
        w.hovered.fg_stroke = Stroke::new(1.0_f32, t.text);
        w.hovered.corner_radius = CornerRadius::same(radius::MD as u8);

        // Pressed: the accent, like a filled macOS button. `bg_fill` is what a
        // slider handle and a checkbox tick are drawn with, so this is also the
        // "engaged" colour of every control in the player.
        w.active.bg_fill = t.accent_pressed;
        w.active.weak_bg_fill = t.accent_pressed;
        w.active.bg_stroke = Stroke::NONE;
        w.active.fg_stroke = Stroke::new(1.0_f32, t.on_accent);
        w.active.corner_radius = CornerRadius::same(radius::MD as u8);

        w.open.bg_fill = t.active;
        w.open.weak_bg_fill = t.hover;
        w.open.bg_stroke = Stroke::NONE;
        w.open.fg_stroke = Stroke::new(1.0_f32, t.text);
        w.open.corner_radius = CornerRadius::same(radius::MD as u8);

        // The caret and the focus ring are the accent too: in a system where one
        // colour means "interactive", a grey caret would be the only thing on
        // screen not speaking the same language.
        visuals.text_cursor.stroke = Stroke::new(1.5_f32, t.accent);
        visuals.text_cursor.preview = false;

        // egui keeps a separate `Style` for each theme and resolves
        // `ThemePreference::System` afresh on every frame, swapping which one is
        // active. `set_visuals` and `set_style` only write to whichever slot is
        // active at the time, and at start-up — before the first input frame has
        // carried the system report — that is the dark slot. On a light-themed
        // Windows the player then ran the rest of the session on egui's stock
        // `Visuals::light()`: everything coloured by hand from `Tokens` stayed
        // dark, while every surface egui draws for us — menu popups, tooltips,
        // drop-downs, windows — took the light palette. That split is what
        // produced a white menu popup with our own light text sitting on it.
        //
        // The player has exactly one palette, so pin the theme *and* fill in
        // both slots: the pin makes the choice deterministic, and patching both
        // means a theme change can never bring the mismatch back.
        ctx.set_theme(egui::ThemePreference::Dark);
        ctx.all_styles_mut(|style| {
            style.visuals = visuals.clone();

            style.spacing.item_spacing = egui::vec2(space::SM, space::SM);
            style.spacing.button_padding = egui::vec2(space::MD, space::XS + 2.0);
            style.spacing.menu_margin = egui::Margin::same(6);
            style.spacing.indent = space::LG;
            style.spacing.slider_width = 120.0;
            style.spacing.interact_size = egui::vec2(0.0, 26.0);
            // Scroll bars: thin, always the same quiet grey, and *never on top of a
            // control*.
            //
            // A floating bar with no allocated width — which is what this was — is
            // drawn over whatever happens to be at the right-hand edge. On the
            // settings page that is the column of switches and sliders, so the bar
            // lay across their ends. `floating_allocated_width` makes the content
            // stop short of the bar, which is the whole point of reserving room for
            // it; the bar stays floating so it still fades in rather than sitting in
            // a permanent grey gutter.
            style.spacing.scroll.bar_width = 10.0;
            style.spacing.scroll.floating = true;
            style.spacing.scroll.floating_width = 5.0;
            style.spacing.scroll.floating_allocated_width = 10.0;
            style.spacing.scroll.bar_inner_margin = 2.0;
            style.visuals.striped = true;

            // Motion: long enough to be seen, short enough that a click feels
            // instant. Apple's own controls settle in about a seventh of a second.
            style.animation_time = 0.14;

            style.text_styles = [
                (TextStyle::Heading, FontId::new(font::H2, FontFamily::Proportional)),
                (TextStyle::Body, FontId::new(font::BODY, FontFamily::Proportional)),
                (TextStyle::Monospace, FontId::new(font::SMALL, FontFamily::Monospace)),
                (TextStyle::Button, FontId::new(font::BODY, FontFamily::Proportional)),
                (TextStyle::Small, FontId::new(font::SMALL, FontFamily::Proportional)),
            ]
            .into();
        });
    }
}

/// Elevation. Apple ships a handful of shadow steps rather than an arbitrary
/// blur per component, so the same three are reused everywhere: a popover lifts
/// off the page, a sheet lifts off the popover, and the floating HUD only just
/// clears the picture.
pub mod shadow {
    use egui::epaint::Shadow;
    use egui::Color32;

    /// Menus and popovers: a short, fairly dark shadow, because a menu is close to
    /// the surface it covers.
    pub fn popup() -> Shadow {
        Shadow {
            offset: [0, 8],
            blur: 20,
            spread: 0,
            color: Color32::from_black_alpha(0x2E),
        }
    }

    /// Floating windows and sheets.
    ///
    /// The one piece of polish left in the interface that costs nothing to draw: a
    /// window without a shadow does not read as floating, it reads as pasted. Offset
    /// downwards and kept light — a heavy shadow under a dark panel is a black smear,
    /// not depth.
    pub fn window() -> Shadow {
        Shadow {
            offset: [0, 16],
            blur: 32,
            spread: 0,
            color: Color32::from_black_alpha(0x1F),
        }
    }
}

/// Name of the heavier font family, when Windows has one.
pub const BOLD_FAMILY: &str = "mvp-bold";

/// Whether the bold face could be loaded at start-up.
static BOLD_FACE: AtomicBool = AtomicBool::new(false);

/// The heading face: the bold cut of the system font, or the body face when
/// Windows has no bold face to offer.
///
/// `epaint` cannot synthesise a bold weight, so headings would otherwise be
/// distinguishable only by size. Loading the real bold file — and *falling back
/// silently* if it is missing — is what makes a title look like a title without
/// making a stripped-down Windows fail to start.
pub fn strong_font(size: f32) -> FontId {
    if BOLD_FACE.load(Ordering::Relaxed) {
        FontId::new(size, FontFamily::Name(BOLD_FAMILY.into()))
    } else {
        FontId::proportional(size)
    }
}

/// Latin UI font: the face every stock Windows control is drawn with.
///
/// Segoe UI has shipped since Vista; Tahoma and Arial go back much further and
/// are only reached on an image that has had its default fonts stripped.
const UI_FONT_CANDIDATES: &[&str] = &["segoeui.ttf", "tahoma.ttf", "arial.ttf"];

/// Monospace font, used for the timestamps on the seek bar.
const MONO_FONT_CANDIDATES: &[&str] = &["consola.ttf", "cour.ttf", "lucon.ttf"];

/// CJK font, paired with the face to use inside the file.
///
/// The two collections need an explicit face: their *second* face is the "… UI"
/// cut — tighter spacing, hinted for the small sizes a UI is drawn at — which is
/// what Explorer and Settings themselves use. `simsun.ttc` is the exception, so
/// it is not generalised into a rule: its second face is NSimSun, a different
/// family rather than a UI cut, so it stays on face 0. Every index here was read
/// off the real name tables in `%SystemRoot%\Fonts`.
///
/// Microsoft YaHei is present on every Windows 8+ install regardless of the
/// display language, so the first entry almost always wins and the rest are
/// belt-and-braces for stripped-down images.
///
/// `msyhl.ttc` (YaHei Light) is deliberately absent: it is a lighter weight than
/// any Latin face here, so as a fallback it would make mixed text disagree with
/// itself in the opposite direction.
const CJK_FONT_CANDIDATES: &[(&str, u32)] = &[
    ("msyh.ttc", 1),
    ("msyh.ttf", 0),
    ("simhei.ttf", 0),
    ("Deng.ttf", 0),
    ("msjh.ttc", 1),
    ("simsun.ttc", 0),
];

/// Bold Latin face, used for headings and window titles.
///
/// Segoe UI Bold is part of the same family as [`UI_FONT_CANDIDATES`], so a
/// heading and the body under it keep identical metrics and only differ in
/// weight — which is exactly the pairing macOS gets from SF Pro.
const BOLD_FONT_CANDIDATES: &[&str] = &["segoeuib.ttf", "tahomabd.ttf", "arialbd.ttf"];

/// Bold CJK, so a Chinese heading does not drop back to the regular weight at
/// the first ideograph. `simhei.ttf` is the last resort: it is a family of its
/// own, but it is unambiguously heavy, and a heading that is heavy for the wrong
/// reason still reads as a heading.
const CJK_BOLD_FONT_CANDIDATES: &[(&str, u32)] = &[
    ("msyhbd.ttc", 0),
    ("msjhbd.ttc", 0),
    ("simhei.ttf", 0),
];

/// Directory Windows keeps its fonts in.
///
/// Read from `%SystemRoot%` rather than assuming `C:\Windows`, so a Windows
/// installed on another drive still finds its own fonts. Resolved once.
fn font_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .filter(|root| root.is_dir())
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("Fonts")
    })
}

/// One font file read off the disk, with the face index already clamped.
struct PrefetchedFont {
    path: PathBuf,
    bytes: Vec<u8>,
    face: u32,
}

/// The font files being read ahead of the first frame.
///
/// The stack is 37 MB across five files, and reading it is ~18 ms of disk I/O
/// that used to sit *after* `eframe` had finished bringing the GL context up —
/// on the critical path to the first frame. [`prefetch_fonts`] reads it on its
/// own thread, started from `main`, so the I/O overlaps the driver's own ~160 ms
/// of GL initialisation and the install finds the bytes already in memory.
static FONT_PREFETCH: OnceLock<
    crossbeam_channel::Receiver<Vec<(&'static str, PrefetchedFont)>>,
> = OnceLock::new();

/// The candidate files for one font slot, with the face to use inside each.
///
/// Built from the `*_CANDIDATES` constants so the background read and
/// [`install_fonts`] can never disagree about which files are wanted.
fn font_slots() -> [(&'static str, Vec<(&'static str, u32)>); 5] {
    let plain = |list: &[&'static str]| list.iter().map(|file| (*file, 0)).collect();
    [
        ("ui", plain(UI_FONT_CANDIDATES)),
        ("mono", plain(MONO_FONT_CANDIDATES)),
        ("cjk", CJK_FONT_CANDIDATES.to_vec()),
        ("ui_bold", plain(BOLD_FONT_CANDIDATES)),
        ("cjk_bold", CJK_BOLD_FONT_CANDIDATES.to_vec()),
    ]
}

/// Start reading the font stack on a background thread.
///
/// Called once from `main`, before `eframe::run_native`, so the read runs while
/// the window and GL context are being created. Safe to call more than once —
/// only the first call spawns.
pub fn prefetch_fonts() {
    let (tx, rx) = crossbeam_channel::bounded(1);
    if FONT_PREFETCH.set(rx).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("mvp-fonts".into())
        .spawn(move || {
            let dir = font_dir();
            let loaded: Vec<_> = font_slots()
                .iter()
                .filter_map(|(key, candidates)| {
                    read_first(dir, candidates).map(|font| (*key, font))
                })
                .collect();
            let _ = tx.send(loaded);
        });
}

/// Read the first candidate that exists, clamping the face index to the file.
fn read_first(dir: &Path, candidates: &[(&str, u32)]) -> Option<PrefetchedFont> {
    for (file, wanted) in candidates {
        let path = dir.join(file);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let face = if *wanted < face_count(&bytes) { *wanted } else { 0 };
        return Some(PrefetchedFont { path, bytes, face });
    }
    None
}

/// Take the prefetched files, waiting for the background thread if it is still
/// running. Absent when [`prefetch_fonts`] was never called (a test, or a caller
/// that installs the theme on its own); the install then reads from disk.
fn take_prefetched() -> HashMap<&'static str, PrefetchedFont> {
    FONT_PREFETCH
        .get()
        .and_then(|rx| rx.recv().ok())
        .map(|list| list.into_iter().collect())
        .unwrap_or_default()
}

/// How many faces a font file holds.
///
/// Asking for a face that is not there makes `epaint` panic while parsing the
/// file, so callers clamp their requested index against this.
fn face_count(bytes: &[u8]) -> u32 {
    // TrueType collection header: 'ttcf', a version, then the face count.
    if bytes.len() >= 12 && &bytes[0..4] == b"ttcf" {
        return u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    }
    1
}

/// Register `key`'s font from the prefetched bytes, or read the first candidate
/// that exists when the prefetch did not find one.
///
/// Returns `false` when no candidate exists at all. The caller then simply
/// leaves the font out of its family, so a missing fallback degrades to the next
/// entry in the chain instead of aborting start-up.
fn load_faces(
    fonts: &mut egui::FontDefinitions,
    key: &str,
    candidates: &[(&str, u32)],
    prefetched: Option<PrefetchedFont>,
) -> bool {
    // Clamped rather than trusted: a Windows build that ships a single-face
    // `msyh.ttc` must still start, not abort on a bad face index.
    let font = prefetched.or_else(|| read_first(font_dir(), candidates));
    let Some(font) = font else {
        let names: Vec<&str> = candidates.iter().map(|(file, _)| *file).collect();
        log::warn!("系统字体缺失，{key} 的回退链 {names:?} 全部不可用");
        return false;
    };
    let kilobytes = font.bytes.len() / 1024;
    let mut data = egui::FontData::from_owned(font.bytes);
    data.index = font.face;
    fonts
        .font_data
        .insert(key.to_owned(), std::sync::Arc::new(data));
    log::info!(
        "已加载字体 {key}: {} ({kilobytes} KB, 第 {} 个字面)",
        font.path.display(),
        font.face
    );
    true
}

static FONTS_INSTALLED: std::sync::Once = std::sync::Once::new();

/// Build the font stack the whole interface is drawn with.
///
/// Starts from an **empty** definition rather than `FontDefinitions::default()`.
/// `egui`'s bundled pairing is Ubuntu-Light for text and Hack for monospace —
/// neither is a Windows font. Mixing them with Microsoft YaHei made a single
/// line disagree with itself: the Latin ran in a light weight at Ubuntu's
/// proportions while the CJK fell back to a much heavier YaHei, so size, stroke
/// weight and baseline all shifted at every script boundary. The stack below is
/// the one the Windows shell itself uses, so the runs finally agree.
///
/// Loading happens once per process, before the first frame is laid out.
fn install_fonts(ctx: &Context) {
    FONTS_INSTALLED.call_once(|| {
        let started = std::time::Instant::now();
        let mut fonts = egui::FontDefinitions::empty();

        let mut proportional = Vec::new();
        let mut monospace = Vec::new();
        let mut bold = Vec::new();

        // `epaint` walks a family in order and takes the first font that has a
        // glyph, so this reads as a primary followed by its fallbacks: Latin
        // first, then CJK for anything the Latin face does not cover.
        //
        // The bytes come from the background read `main` started before the
        // window existed; `take_prefetched` waits for it to finish, which it has
        // by now — the GL context took longer to come up than the read.
        let mut prefetched = take_prefetched();
        for (key, candidates) in font_slots() {
            if !load_faces(&mut fonts, key, &candidates, prefetched.remove(key)) {
                continue;
            }
            match key {
                "ui" => proportional.push("ui".to_owned()),
                "mono" => monospace.push("mono".to_owned()),
                // The CJK face backs both families.
                "cjk" => {
                    proportional.push("cjk".to_owned());
                    monospace.push("cjk".to_owned());
                }
                // Headings. Registered as a family of its own rather than as a
                // replacement for the body font, because only some text — titles,
                // section headings, the one number a dialog is about — is meant to
                // be heavy.
                "ui_bold" => bold.push("ui_bold".to_owned()),
                "cjk_bold" => bold.push("cjk_bold".to_owned()),
                _ => {}
            }
        }
        if !bold.is_empty() {
            fonts
                .families
                .insert(FontFamily::Name(BOLD_FAMILY.into()), bold);
            BOLD_FACE.store(true, Ordering::Relaxed);
        }

        log::info!(
            "字体栈就绪: 文本 {proportional:?} / 等宽 {monospace:?} / 粗体 {}，耗时 {:.1} ms",
            BOLD_FACE.load(Ordering::Relaxed),
            started.elapsed().as_secs_f32() * 1000.0
        );
        if proportional.is_empty() {
            log::error!("没有可用的文本字体，界面文字将无法渲染");
        }

        fonts.families.insert(FontFamily::Proportional, proportional);
        fonts.families.insert(FontFamily::Monospace, monospace);
        ctx.set_fonts(fonts);
    });
}

/// Convenience: a selection highlight drawn behind a row.
///
/// Selected rows use the translucent accent rather than a solid fill, so the row
/// keeps whatever surface it was drawn on and the text over it keeps its
/// contrast — the same reason Apple's list selections are translucent.
pub fn row_fill(tokens: &Tokens, selected: bool, hovered: bool) -> Color32 {
    if selected {
        tokens.accent_soft
    } else if hovered {
        tokens.hover
    } else {
        Color32::TRANSPARENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance.
    fn luminance(colour: Color32) -> f32 {
        let channel = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(colour.r()) + 0.7152 * channel(colour.g()) + 0.0722 * channel(colour.b())
    }

    /// WCAG contrast ratio between two colours.
    fn contrast(a: Color32, b: Color32) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// How much colour a colour carries: `(max - min) / max`.
    ///
    /// Measured against the strongest stored channel, which is the cheap, monotone
    /// reading of "how much colour is in here".
    fn chroma(colour: Color32) -> f32 {
        let max = colour.r().max(colour.g()).max(colour.b()) as f32;
        let min = colour.r().min(colour.g()).min(colour.b()) as f32;
        if max == 0.0 {
            0.0
        } else {
            (max - min) / max
        }
    }

    /// Every palette with its tokens, so a test can state its property once.
    fn every_palette() -> Vec<(Palette, Tokens)> {
        Palette::all()
            .into_iter()
            .map(|palette| (palette, Tokens::for_palette(palette)))
            .collect()
    }

    /// A channel sum, as a stand-in for lightness in the ordering checks.
    fn lightness(colour: Color32) -> u32 {
        u32::from(colour.r()) + u32::from(colour.g()) + u32::from(colour.b())
    }

    /// The play button has to fit the transport bar it sits in, and stay the biggest control
    /// in it. The bar is 88 pt: 24 pt of seek row and gap, the button, and the 8 pt frame
    /// above and below. Scaling the button without doing this arithmetic is how it ended up
    /// drawn clipped once already.
    #[test]
    fn the_play_button_fits_the_transport_bar_and_still_leads_it() {
        // Through locals: `assert!(button::PLAY > button::of(28.0))` is a constant expression
        // and clippy says so, which is a lint rather than a test.
        let play = button::PLAY;
        let tool = button::of(28.0);
        assert!(
            play > tool,
            "the play button ({play}) must stay bigger than a tool button ({tool})"
        );
        assert!(
            24.0 + play + 2.0 * space::SM <= 88.0,
            "a {play} pt play button does not fit the 88 pt video transport bar"
        );
    }

    /// Every palette has to be *legible*, the original included: the contrast
    /// floors are what makes this interface readable, and a palette is allowed to
    /// change the hue of a colour, never its weight.
    #[test]
    fn every_palette_has_enough_contrast() {
        for (palette, t) in every_palette() {
            let name = palette.label();
            assert!(
                contrast(t.text, t.bg) > 12.0,
                "{name}: body text must be very legible"
            );
            assert!(
                contrast(t.text_weak, t.panel) > 4.5,
                "{name}: captions must pass WCAG AA"
            );
            assert!(
                contrast(t.accent, t.bg) > 3.0,
                "{name}: the accent must be visible"
            );
            assert!(
                contrast(t.on_accent, t.accent) > 3.5,
                "{name}: text on the accent has to stay readable"
            );
        }
    }

    /// The *third* level of text has to be readable too.
    ///
    /// `text_muted` carries the settings page's explanations, the timestamps in the
    /// playlist, every "共 N 项" caption and the shortcut hints in the menus — all of
    /// it 11 or 12 pt, all of it meant to be read. At its old value it sat at 3.7:1
    /// on the panel and about 2.6:1 over the settings sheet, which is not a quiet
    /// caption but an invisible one. Quiet is a *step down* from the text around it,
    /// not a step below the contrast floor.
    #[test]
    fn the_quietest_text_still_passes_wcag_aa() {
        for (palette, t) in every_palette() {
            let name = palette.label();
            assert!(
                contrast(t.text_muted, t.panel) >= 4.5,
                "{name}: the muted colour is used for 11 pt captions"
            );
            assert!(
                contrast(t.text_weak, t.panel) >= 4.5,
                "{name}: and the weak colour for captions on top of it"
            );
            // The levels stay distinguishable: a caption must not become body text.
            assert!(contrast(t.text, t.panel) > contrast(t.text_weak, t.panel));
            assert!(contrast(t.text_weak, t.panel) > contrast(t.text_muted, t.panel));
        }
    }

    #[test]
    fn surfaces_step_up_in_lightness() {
        for (palette, t) in every_palette() {
            let name = palette.label();
            assert!(
                lightness(t.sunken) < lightness(t.bg),
                "{name}: the trough has to sit below the background"
            );
            assert!(
                lightness(t.bg) < lightness(t.panel),
                "{name}: panel must sit above the background"
            );
            assert!(
                lightness(t.panel) < lightness(t.elevated),
                "{name}: elevated must sit above the panel"
            );
        }
    }

    #[test]
    fn font_candidates_are_bare_windows_font_file_names() {
        // Bare names rather than paths: `font_dir` resolves them against
        // `%SystemRoot%`, so a Windows installed on another drive still finds
        // its own fonts.
        for file in UI_FONT_CANDIDATES
            .iter()
            .copied()
            .chain(MONO_FONT_CANDIDATES.iter().copied())
            .chain(BOLD_FONT_CANDIDATES.iter().copied())
            .chain(CJK_FONT_CANDIDATES.iter().map(|(file, _)| *file))
            .chain(CJK_BOLD_FONT_CANDIDATES.iter().map(|(file, _)| *file))
        {
            assert!(
                !file.contains(['/', '\\']),
                "{file} must be a file name, not a path"
            );
            assert!(file.ends_with(".ttc") || file.ends_with(".ttf"));
        }
    }

    #[test]
    fn only_verified_collections_ask_for_a_second_face() {
        // Face 1 is the "… UI" cut for YaHei and JhengHei. `simsun.ttc` also
        // holds two faces, but its second is NSimSun — a different family, not a
        // UI cut — so it stays on face 0. Pinning the table here is what stops
        // the distinction being smoothed over by a "collections use face 1"
        // rule that would be wrong. Indices were read from the real name tables.
        for (file, face) in CJK_FONT_CANDIDATES {
            let expected = match *file {
                "msyh.ttc" | "msjh.ttc" => 1,
                _ => 0,
            };
            assert_eq!(*face, expected, "{file} carries an unexpected face index");
        }
    }

    #[test]
    fn face_count_reads_a_collection_header() {
        let mut ttc = vec![0u8; 16];
        ttc[0..4].copy_from_slice(b"ttcf");
        ttc[8..12].copy_from_slice(&2u32.to_be_bytes());
        assert_eq!(face_count(&ttc), 2);

        // A plain sfnt file has exactly one face, and a file too short to hold a
        // header must not be read past its end.
        assert_eq!(face_count(&[0, 1, 0, 0, 0, 10]), 1);
        assert_eq!(face_count(b"ttcf"), 1);
    }

    /// The steps of the radius scale must grow outwards. A chip drawn rounder
    /// than the card it sits on is the kind of detail that makes an interface
    /// feel assembled rather than designed.
    #[test]
    fn the_radius_scale_grows_outwards() {
        assert!(radius::SM < radius::MD);
        assert!(radius::MD < radius::LG);
        assert!(radius::LG < radius::XL);
        // Anything bigger than a quarter of a 26 pt control would start to read
        // as a pill and destroy the difference between the steps.
        assert!(radius::XL <= 20.0);
    }

    /// The spacing scale is the 4 pt grid, with one half-step for the gap inside
    /// a control — which is why the half-step is checked against `XS` rather than
    /// against the grid.
    #[test]
    fn the_spacing_scale_is_a_four_point_grid() {
        for step in [
            space::XS,
            space::SM,
            space::MD,
            space::LG,
            space::XL,
            space::XXL,
        ] {
            assert!(step > 0.0);
            assert!(
                (step % 4.0).abs() < f32::EPSILON,
                "{step} is off the 4 pt grid"
            );
        }
        assert!((space::XXS - space::XS / 2.0).abs() < f32::EPSILON);
        assert!(space::XXS < space::XS && space::XS < space::SM);
        assert!(space::XL < space::XXL);
    }

    /// Translucent state fills have to *be* translucent: a selection painted at
    /// full alpha hides the surface behind it, which is what made the old
    /// selection look like a different window.
    #[test]
    fn state_fills_are_translucent_and_keep_their_hue() {
        // The soft variants are the same colour as their solid counterpart, only
        // quieter. `Color32` stores *premultiplied* channels, so an opacity of
        // 61/255 scales the channels stored for a translucent colour: they have
        // to be unmultiplied before two colours can be compared meaningfully.
        let un = |c: Color32| {
            let alpha = f32::from(c.a());
            let channel = |v: u8| {
                if alpha == 0.0 {
                    0.0
                } else {
                    f32::from(v) * 255.0 / alpha
                }
            };
            (channel(c.r()), channel(c.g()), channel(c.b()))
        };
        let same_hue = |soft: Color32, solid: Color32, what: &str| {
            let (a, b) = (un(soft), un(solid));
            let close = |x: f32, y: f32| (x - y).abs() <= 2.0;
            assert!(
                close(a.0, b.0) && close(a.1, b.1) && close(a.2, b.2),
                "{what}: {a:?} is not the same hue as {b:?}"
            );
        };
        for (palette, t) in every_palette() {
            for soft in [
                t.hover,
                t.active,
                t.separator,
                t.border,
                t.border_strong,
                t.track,
                t.accent_soft,
                t.focus_ring,
                t.skeleton,
                t.skeleton_highlight,
                t.danger_soft,
            ] {
                assert!(
                    soft.a() > 0 && soft.a() < 255,
                    "{}: expected a translucent fill",
                    palette.label()
                );
            }
            same_hue(t.accent_soft, t.accent, "accent_soft");
            same_hue(t.danger_soft, t.danger, "danger_soft");
        }
    }

    /// A skeleton is a *hint* of content: visible against the surface, quieter
    /// than the separator that will replace it, and its travelling highlight has
    /// to be the brightest of the three.
    #[test]
    fn skeleton_fills_are_quieter_than_the_content_they_stand_in_for() {
        let t = Tokens::default();
        assert!(t.skeleton.a() < t.separator.a(), "the skeleton is a whisper");
        assert!(
            t.skeleton_highlight.a() > t.separator.a(),
            "the travelling band must be visible"
        );
        assert!(t.skeleton_highlight.a() < 60, "and must not flash");
    }

    /// The pressed accent is the accent family one step darker, so a press reads
    /// as the same button being held down rather than as a different control.
    #[test]
    fn the_pressed_accent_is_the_accent_one_step_darker() {
        for (palette, t) in every_palette() {
            let name = palette.label();
            assert!(
                lightness(t.accent_pressed) < lightness(t.accent),
                "{name}: a press must darken"
            );
            // Hovering must not *lose* the accent's colour: the channel it leads with
            // gets brighter, not duller. Written on the leading channel rather than on
            // blue, because a Morandi accent is not blue.
            let lead = |c: Color32| c.r().max(c.g()).max(c.b());
            assert!(
                lead(t.accent_hover) >= lead(t.accent),
                "{name}: hovering must not lose the accent's colour"
            );
            // All three states are the *same hue family*: hover and press change the
            // weight of the accent, never which channel leads it. This used to be
            // spelled `b > g > r` — "the accent must stay blue" — while the accent was
            // Apple blue; what that stood for is the property pinned here, and it is
            // what every palette has to keep, the original included.
            let family = |c: Color32| ((c.r() > c.g()) as u8) | (((c.g() > c.b()) as u8) << 1);
            let expected = family(t.accent);
            for state in [t.accent_hover, t.accent_pressed] {
                assert_eq!(
                    family(state),
                    expected,
                    "{name}: hover and press must not change the accent's hue"
                );
            }
        }
    }

    /// Nothing in a Morandi palette shouts: every colour in one is a *muted* one.
    ///
    /// This is the property that makes a palette Morandi rather than "a dark theme
    /// with a nicer blue", and it is the one that gets eroded first: a single
    /// saturated accent added later makes everything around it look dirty. Chroma is
    /// measured against the strongest stored channel — `(max - min) / max` — which is
    /// the cheap, monotone reading of "how much colour is in here". Surfaces and text
    /// are the most restrained, the accent family carries the most and still stays
    /// under 42 %, and the semantic trio is allowed a little more than the surfaces
    /// because a warning has to be recognisable as one.
    ///
    /// Which palettes the ceilings apply to is [`Palette::is_muted`]'s answer rather
    /// than a list here: the original blue palette is exempt because a saturated
    /// accent is what "the original" *is*, and a palette added later is held to them
    /// by default.
    #[test]
    fn no_colour_in_the_palette_shouts() {
        for (palette, t) in every_palette() {
            let name = palette.label();
            // The letterbox is not a colour: it is the black a cinema letterboxes
            // with, and it is that on every palette.
            assert_eq!(t.letterbox, Color32::BLACK, "{name}");
            if !palette.is_muted() {
                continue;
            }
            for (what, colour) in [
                ("bg", t.bg),
                ("panel", t.panel),
                ("elevated", t.elevated),
                ("sunken", t.sunken),
                ("text", t.text),
                ("text_weak", t.text_weak),
                ("text_muted", t.text_muted),
            ] {
                assert!(
                    chroma(colour) <= 0.25,
                    "{name}: {what} is too colourful for a Morandi palette: {:.2}",
                    chroma(colour)
                );
            }
            for (what, colour) in [
                ("accent", t.accent),
                ("accent_hover", t.accent_hover),
                ("accent_pressed", t.accent_pressed),
            ] {
                assert!(
                    chroma(colour) <= 0.42,
                    "{name}: {what} carries more colour than the accent is allowed: {:.2}",
                    chroma(colour)
                );
            }
            for (what, colour) in [
                ("success", t.success),
                ("warning", t.warning),
                ("danger", t.danger),
            ] {
                assert!(
                    chroma(colour) <= 0.45,
                    "{name}: {what} must stay muted: {:.2}",
                    chroma(colour)
                );
            }
        }
    }

    /// Switching palettes must not change the *shape* of the interface.
    ///
    /// Every translucent fill is its palette's own white at one of these alphas, so
    /// how strong a hover is, how loud a selection is and how visible a focus ring is
    /// are the same on all five. That is what makes this setting a recolouring rather
    /// than five interfaces to keep in step, and it is what the derived fills in
    /// [`Recipe`] are built on.
    #[test]
    fn every_palette_draws_its_fills_at_the_same_strength() {
        for (palette, t) in every_palette() {
            for (what, colour, wanted) in [
                ("hover", t.hover, alpha::HOVER),
                ("active", t.active, alpha::ACTIVE),
                ("border", t.border, alpha::BORDER),
                ("border_strong", t.border_strong, alpha::BORDER_STRONG),
                ("track", t.track, alpha::TRACK),
                ("separator", t.separator, alpha::SEPARATOR),
                ("skeleton", t.skeleton, alpha::SKELETON),
                (
                    "skeleton_highlight",
                    t.skeleton_highlight,
                    alpha::SKELETON_HIGHLIGHT,
                ),
                ("accent_soft", t.accent_soft, alpha::ACCENT_SOFT),
                ("focus_ring", t.focus_ring, alpha::ACCENT_RING),
                ("danger_soft", t.danger_soft, alpha::DANGER_SOFT),
            ] {
                assert_eq!(
                    colour.a(),
                    wanted,
                    "{}: {what} is not drawn at the shared strength",
                    palette.label()
                );
            }
        }
    }

    /// The palette the player shipped with, pinned value for value.
    ///
    /// [`Palette::Blue`] exists so that a user who liked the interface before this
    /// was a setting can have it back, and "the original" can only mean one thing.
    /// Changing a value here is therefore a decision to change what the original
    /// *is*, which is exactly the kind of change that should have to be made on
    /// purpose.
    #[test]
    fn the_original_palette_is_kept_verbatim() {
        let t = Tokens::for_palette(Palette::Blue);
        for (what, actual, original) in [
            ("bg", t.bg, 0x0D0E11),
            ("panel", t.panel, 0x16181C),
            ("elevated", t.elevated, 0x1E2126),
            ("sunken", t.sunken, 0x0A0B0D),
            ("text", t.text, 0xF5F5F7),
            ("text_weak", t.text_weak, 0xA1A7B3),
            ("text_muted", t.text_muted, 0x7E838D),
            ("accent", t.accent, 0x0A84FF),
            ("accent_hover", t.accent_hover, 0x3D9BFF),
            ("accent_pressed", t.accent_pressed, 0x0670E0),
            ("success", t.success, 0x32D74B),
            ("warning", t.warning, 0xFF9F0A),
            ("danger", t.danger, 0xFF453A),
        ] {
            assert_eq!(
                actual,
                hex(original),
                "the original palette's {what} is not the colour it shipped with"
            );
        }
        // Its translucent fills were a pure white, which is what `wash` carries.
        assert_eq!(t.hover, rgba(0xFF, 0xFF, 0xFF, alpha::HOVER));
        assert_eq!(t.separator, rgba(0xFF, 0xFF, 0xFF, alpha::SEPARATOR));
    }

    /// Every palette is offered exactly once, and the default says so.
    ///
    /// The settings page builds its dropdown from [`Palette::choices`], and either a
    /// duplicate or a variant missing from it is a palette the user cannot reach.
    #[test]
    fn every_palette_is_offered_once() {
        let mut labels: Vec<&str> = Vec::new();
        for (palette, label) in Palette::choices() {
            assert!(!label.trim().is_empty(), "a palette has no name");
            assert!(
                !labels.contains(&label),
                "two palettes share the name {label}"
            );
            labels.push(label);
            assert!(
                Palette::all().contains(&palette),
                "{label} is offered but is not in `Palette::all`"
            );
        }
        assert_eq!(labels.len(), Palette::all().len());

        // The marker follows the default, so `Palette::default()` can be changed
        // without leaving "（默认）" on the wrong row.
        let marked: Vec<&str> = labels
            .iter()
            .copied()
            .filter(|label| label.contains(Palette::DEFAULT_MARK))
            .collect();
        assert_eq!(
            marked,
            vec![Palette::default().label()],
            "exactly the default palette carries the marker"
        );
    }

    /// The default is a Morandi palette: what the interface is designed around is
    /// what a fresh install gets, and the original is the opt-in.
    #[test]
    fn the_default_palette_is_a_morandi_one() {
        assert!(Palette::default().is_muted());
        assert_eq!(Tokens::default(), Tokens::for_palette(Palette::default()));
    }

    /// A palette reaches egui's own widgets, not just the hand-painted ones.
    ///
    /// The bug this pins down: egui keeps one `Style` per theme and resolves
    /// `ThemePreference::System` from the OS report on every frame. `install`
    /// used to write the palette with `set_visuals`/`set_style`, which only
    /// touch the slot that is active at the time — and at start-up, before any
    /// frame has carried a report, that is always the dark slot. A light-themed
    /// Windows therefore ran the rest of the session on egui's stock light
    /// visuals: everything coloured by hand from `Tokens` stayed dark while
    /// every surface egui draws itself took the light palette. The visible
    /// symptom was a white menu popup with our own light text on top of it.
    ///
    /// Run for every palette, because the fix — pinning the theme and filling both
    /// slots — is also what makes switching palettes work at all.
    /// A palette can be installed *while* a frame is being drawn.
    ///
    /// Which is exactly what the settings row does: `widgets::combo_row` runs inside
    /// the frame that is painting the sheet, and `PlayerApp::set_palette` installs the
    /// new colours there and then. egui allows the style to be rewritten mid-frame —
    /// the frame being built keeps what it started with — so what has to hold is that
    /// nothing panics and that the *next* frame is the new palette.
    #[test]
    fn a_palette_can_be_installed_while_a_frame_is_being_drawn() {
        let ctx = Context::default();
        Theme::of(Palette::Blue).install(&ctx);
        let mauve = Tokens::for_palette(Palette::Mauve);

        let during = Theme::of(Palette::Mauve);
        let _ = ctx.run(Default::default(), |ctx| {
            during.install(ctx);
        });

        let _ = ctx.run(Default::default(), |ctx| {
            let visuals = &ctx.style().visuals;
            assert_eq!(
                visuals.panel_fill, mauve.panel,
                "the frame after a mid-frame install has to be the new palette"
            );
            assert_eq!(visuals.window_fill, mauve.elevated);
            assert_eq!(visuals.override_text_color, Some(mauve.text));
        });
    }

    #[test]
    fn a_light_system_theme_cannot_replace_the_palette() {
        let input = egui::RawInput {
            system_theme: Some(egui::Theme::Light),
            ..Default::default()
        };
        for (palette, tokens) in every_palette() {
            let theme = Theme::of(palette);
            let ctx = Context::default();
            theme.install(&ctx);

            let _ = ctx.run(input.clone(), |ctx| {
                assert_eq!(
                    ctx.theme(),
                    egui::Theme::Dark,
                    "the player is dark-only and must not follow the OS theme"
                );
                let visuals = &ctx.style().visuals;
                assert_eq!(
                    visuals.panel_fill,
                    tokens.panel,
                    "{}: panels must keep the player's palette",
                    palette.label()
                );
                assert_eq!(
                    visuals.window_fill,
                    tokens.elevated,
                    "{}: windows and menu popups must keep the player's palette",
                    palette.label()
                );
                assert_eq!(
                    visuals.override_text_color,
                    Some(tokens.text),
                    "{}: text must stay legible against those surfaces",
                    palette.label()
                );
            });
        }
    }
}
