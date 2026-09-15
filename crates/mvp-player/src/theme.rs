//! Visual design tokens and the `egui` style the whole player is built from.
//!
//! The palette follows Apple's dark system appearance: a ramp of layered greys
//! (`bg` → `panel` → `elevated` → `sunken`), one blue accent that carries every
//! interactive highlight, and a small set of semantic colours (success, warning,
//! danger) that are only ever used for state. A media player is watched next to
//! its own picture, so the chrome stays quiet and the *picture* stays the
//! brightest thing on screen.
//!
//! Three rules taken from the Human Interface Guidelines hold the whole file
//! together:
//!
//! 1. **Hierarchy comes from colour, not from lines.** Surfaces are separated by
//!    a translucent white hairline ([`Tokens::separator`]) rather than a grey
//!    stroke, so a separator is correct on any surface it is drawn on.
//! 2. **Everything is measured on a 4 pt grid** ([`space`]) with Apple's corner
//!    radii ([`radius`]), so unrelated panels still line up with each other.
//! 3. **The accent means "this is interactive, or this is current."** It is never
//!    decoration. Selection fills use [`Tokens::accent_soft`] so text on top of
//!    them keeps its contrast.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use egui::{
    Color32, Context, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals,
};

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

/// Every colour the interface uses.
#[derive(Debug, Clone)]
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

impl Default for Tokens {
    fn default() -> Self {
        Self {
            bg: rgb(0x0D, 0x0E, 0x11),
            panel: rgb(0x16, 0x18, 0x1C),
            elevated: rgb(0x1E, 0x21, 0x26),
            sunken: rgb(0x0A, 0x0B, 0x0D),
            hover: rgba(0xFF, 0xFF, 0xFF, 0x14),
            active: rgba(0xFF, 0xFF, 0xFF, 0x24),
            border: rgba(0xFF, 0xFF, 0xFF, 0x14),
            border_strong: rgba(0xFF, 0xFF, 0xFF, 0x2E),
            text: rgb(0xF5, 0xF5, 0xF7),
            text_weak: rgb(0xA1, 0xA7, 0xB3),
            text_muted: rgb(0x70, 0x75, 0x7F),
            accent: rgb(0x0A, 0x84, 0xFF),
            accent_hover: rgb(0x3D, 0x9B, 0xFF),
            on_accent: rgb(0xFF, 0xFF, 0xFF),
            progress: rgb(0x0A, 0x84, 0xFF),
            track: rgba(0xFF, 0xFF, 0xFF, 0x1F),
            success: rgb(0x32, 0xD7, 0x4B),
            warning: rgb(0xFF, 0x9F, 0x0A),
            danger: rgb(0xFF, 0x45, 0x3A),
            letterbox: rgb(0x00, 0x00, 0x00),
            separator: rgba(0xFF, 0xFF, 0xFF, 0x17),
            accent_soft: rgba(0x0A, 0x84, 0xFF, 0x3D),
            accent_pressed: rgb(0x06, 0x70, 0xE0),
            focus_ring: rgba(0x0A, 0x84, 0xFF, 0x8C),
            skeleton: rgba(0xFF, 0xFF, 0xFF, 0x0F),
            skeleton_highlight: rgba(0xFF, 0xFF, 0xFF, 0x26),
            danger_soft: rgba(0xFF, 0x45, 0x3A, 0x33),
        }
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
    /// Install the theme into `ctx`, including the CJK-capable font stack.
    ///
    /// The player is dark-only — one palette, no light counterpart — so this
    /// pins the theme instead of following the system.
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
            style.spacing.scroll.bar_width = 9.0;
            style.spacing.scroll.floating = true;
            style.spacing.scroll.floating_width = 4.0;
            style.spacing.scroll.bar_inner_margin = 4.0;
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
    /// The reference implementation's `0 16px 32px rgba(0,0,0,.12)`: a *light*
    /// shadow. The glass is supposed to float, not to sit in a black smear — the
    /// previous value was half-again as dark and is the other half of why the
    /// control island looked heavy.
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

/// Register the first single-face candidate that loads, under `key`.
///
/// Returns `false` when none of them exist. The caller then simply leaves the
/// font out of its family, so a missing fallback degrades to the next entry in
/// the chain instead of aborting start-up.
fn load_font(fonts: &mut egui::FontDefinitions, key: &str, candidates: &[&str]) -> bool {
    let faces: Vec<(&str, u32)> = candidates.iter().map(|file| (*file, 0)).collect();
    load_faces(fonts, key, &faces)
}

/// [`load_font`], for files that may hold more than one face.
fn load_faces(fonts: &mut egui::FontDefinitions, key: &str, candidates: &[(&str, u32)]) -> bool {
    let dir = font_dir();
    for (file, wanted) in candidates {
        let path = dir.join(file);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        // Clamped rather than trusted: a Windows build that ships a single-face
        // `msyh.ttc` must still start, not abort on a bad face index.
        let face = if *wanted < face_count(&bytes) { *wanted } else { 0 };
        let kilobytes = bytes.len() / 1024;
        let mut data = egui::FontData::from_owned(bytes);
        data.index = face;
        fonts
            .font_data
            .insert(key.to_owned(), std::sync::Arc::new(data));
        log::info!(
            "已加载字体 {key}: {} ({kilobytes} KB, 第 {face} 个字面)",
            path.display()
        );
        return true;
    }
    let names: Vec<&str> = candidates.iter().map(|(file, _)| *file).collect();
    log::warn!("系统字体缺失，{key} 的回退链 {names:?} 全部不可用");
    false
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

        // `epaint` walks a family in order and takes the first font that has a
        // glyph, so this reads as a primary followed by its fallbacks: Latin
        // first, then CJK for anything the Latin face does not cover.
        if load_font(&mut fonts, "ui", UI_FONT_CANDIDATES) {
            proportional.push("ui".to_owned());
        }
        if load_font(&mut fonts, "mono", MONO_FONT_CANDIDATES) {
            monospace.push("mono".to_owned());
        }
        if load_faces(&mut fonts, "cjk", CJK_FONT_CANDIDATES) {
            proportional.push("cjk".to_owned());
            monospace.push("cjk".to_owned());
        }

        // Headings. Registered as a family of its own rather than as a replacement
        // for the body font, because only some text — titles, section headings,
        // the one number a dialog is about — is meant to be heavy.
        let mut bold = Vec::new();
        if load_font(&mut fonts, "ui_bold", BOLD_FONT_CANDIDATES) {
            bold.push("ui_bold".to_owned());
        }
        if load_faces(&mut fonts, "cjk_bold", CJK_BOLD_FONT_CANDIDATES) {
            bold.push("cjk_bold".to_owned());
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

    #[test]
    fn accent_has_enough_contrast_against_the_surfaces() {
        let t = Tokens::default();
        // Relative luminance, sRGB approximation: the accessible pair must be
        // far enough apart that the accent reads as a distinct element.
        let lum = |c: Color32| {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        };
        let ratio = |a: Color32, b: Color32| {
            let (la, lb) = (lum(a), lum(b));
            let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
            (hi + 0.05) / (lo + 0.05)
        };
        assert!(ratio(t.text, t.bg) > 12.0, "body text must be very legible");
        assert!(ratio(t.text_weak, t.panel) > 4.5, "captions must pass WCAG AA");
        assert!(ratio(t.accent, t.bg) > 3.0, "the accent must be visible");
        assert!(ratio(t.on_accent, t.accent) > 3.5);
    }

    #[test]
    fn surfaces_step_up_in_lightness() {
        let t = Tokens::default();
        let l = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
        assert!(l(t.bg) < l(t.panel), "panel must sit above the background");
        assert!(l(t.panel) < l(t.elevated), "elevated must sit above the panel");
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
        let t = Tokens::default();
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
            assert!(soft.a() > 0 && soft.a() < 255, "expected a translucent fill");
        }
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
        same_hue(t.accent_soft, t.accent, "accent_soft");
        same_hue(t.danger_soft, t.danger, "danger_soft");
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
        let t = Tokens::default();
        let lum = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
        assert!(lum(t.accent_pressed) < lum(t.accent), "a press must darken");
        assert!(
            t.accent_hover.b() >= t.accent.b(),
            "hovering must not lose the blue"
        );
        // All three states are blues: the blue channel is the strongest in each,
        // so hover and press change the weight of the accent, never its hue.
        for state in [t.accent, t.accent_hover, t.accent_pressed] {
            assert!(
                state.b() > state.g() && state.g() > state.r(),
                "the accent must stay blue"
            );
        }
    }

    #[test]
    fn a_light_system_theme_cannot_replace_the_palette() {
        // The bug this pins down: egui keeps one `Style` per theme and resolves
        // `ThemePreference::System` from the OS report on every frame. `install`
        // used to write the palette with `set_visuals`/`set_style`, which only
        // touch the slot that is active at the time — and at start-up, before any
        // frame has carried a report, that is always the dark slot. A light-themed
        // Windows therefore ran the rest of the session on egui's stock light
        // visuals: everything coloured by hand from `Tokens` stayed dark while
        // every surface egui draws itself took the light palette. The visible
        // symptom was a white menu popup with our own light text on top of it.
        let theme = Theme::default();
        let ctx = Context::default();
        theme.install(&ctx);

        let input = egui::RawInput {
            system_theme: Some(egui::Theme::Light),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            assert_eq!(
                ctx.theme(),
                egui::Theme::Dark,
                "the player is dark-only and must not follow the OS theme"
            );
            let visuals = &ctx.style().visuals;
            assert_eq!(
                visuals.panel_fill, theme.tokens.panel,
                "panels must keep the player's palette"
            );
            assert_eq!(
                visuals.window_fill, theme.tokens.elevated,
                "windows and menu popups must keep the player's palette"
            );
            assert_eq!(
                visuals.override_text_color,
                Some(theme.tokens.text),
                "text must stay legible against those surfaces"
            );
        });
    }
}
