//! Visual design tokens and the `egui` style the whole player is built from.
//!
//! The palette is a deliberately desaturated dark scheme: a media player is
//! watched next to its own picture, so the chrome must never compete with the
//! video. Surfaces step up in lightness (`bg` → `panel` → `elevated`) and a
//! single violet accent carries every interactive highlight, which keeps the
//! interface legible without a rainbow of status colours.

use std::path::{Path, PathBuf};

use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

/// A colour with an alpha channel, built from a hex literal at compile time.
const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
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
}

impl Default for Tokens {
    fn default() -> Self {
        Self {
            bg: rgb(0x0E, 0x10, 0x14),
            panel: rgb(0x14, 0x17, 0x1D),
            elevated: rgb(0x1A, 0x1E, 0x26),
            sunken: rgb(0x10, 0x13, 0x1A),
            hover: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x14),
            active: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x24),
            border: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x14),
            border_strong: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x28),
            text: rgb(0xE7, 0xEA, 0xF0),
            text_weak: rgb(0x9B, 0xA4, 0xB4),
            text_muted: rgb(0x5C, 0x64, 0x72),
            accent: rgb(0x7C, 0x5C, 0xFF),
            accent_hover: rgb(0x8F, 0x74, 0xFF),
            on_accent: rgb(0xFF, 0xFF, 0xFF),
            progress: rgb(0x7C, 0x5C, 0xFF),
            track: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x1E),
            success: rgb(0x35, 0xC4, 0x6B),
            warning: rgb(0xF0, 0xA9, 0x3B),
            danger: rgb(0xF0, 0x50, 0x6E),
            letterbox: rgb(0x00, 0x00, 0x00),
        }
    }
}

/// Spacing scale, in points. Everything is a multiple of four.
pub mod space {
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
}

/// Corner radii, in points.
pub mod radius {
    /// 4 pt — checkboxes, small chips.
    pub const SM: f32 = 4.0;
    /// 6 pt — buttons and text fields.
    pub const MD: f32 = 6.0;
    /// 10 pt — cards and popups.
    pub const LG: f32 = 10.0;
    /// 14 pt — the settings window.
    pub const XL: f32 = 14.0;
}

/// Font sizes, in points.
pub mod font {
    /// 11 pt — timestamps on the seek bar.
    pub const TINY: f32 = 11.0;
    /// 12 pt — captions and metadata rows.
    pub const SMALL: f32 = 12.0;
    /// 13 pt — the default UI size.
    pub const BODY: f32 = 13.0;
    /// 15 pt — section headings.
    pub const H3: f32 = 15.0;
    /// 18 pt — window headings.
    pub const H2: f32 = 18.0;
    /// 24 pt — the empty-state title.
    pub const H1: f32 = 24.0;
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
        visuals.window_shadow = egui::epaint::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(140),
        };
        visuals.popup_shadow = egui::epaint::Shadow {
            offset: [0, 4],
            blur: 12,
            spread: 0,
            color: Color32::from_black_alpha(120),
        };
        visuals.override_text_color = Some(t.text);
        visuals.hyperlink_color = t.accent;
        visuals.selection.bg_fill = t.accent.gamma_multiply(0.55);
        visuals.selection.stroke = Stroke::new(1.0_f32, t.accent_hover);
        visuals.slider_trailing_fill = true;

        let w = &mut visuals.widgets;
        w.noninteractive.bg_fill = t.panel;
        w.noninteractive.weak_bg_fill = t.panel;
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32, t.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, t.text_weak);
        w.noninteractive.corner_radius = CornerRadius::same(radius::MD as u8);

        w.inactive.bg_fill = t.hover;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::new(1.0_f32, t.border);
        w.inactive.fg_stroke = Stroke::new(1.0_f32, t.text);
        w.inactive.corner_radius = CornerRadius::same(radius::MD as u8);

        w.hovered.bg_fill = t.active;
        w.hovered.weak_bg_fill = t.active;
        w.hovered.bg_stroke = Stroke::new(1.0_f32, t.border_strong);
        w.hovered.fg_stroke = Stroke::new(1.0_f32, t.text);
        w.hovered.corner_radius = CornerRadius::same(radius::MD as u8);

        w.active.bg_fill = t.accent.gamma_multiply(0.85);
        w.active.weak_bg_fill = t.accent.gamma_multiply(0.85);
        w.active.bg_stroke = Stroke::new(1.0_f32, t.accent_hover);
        w.active.fg_stroke = Stroke::new(1.0_f32, t.on_accent);
        w.active.corner_radius = CornerRadius::same(radius::MD as u8);

        w.open.bg_fill = t.active;
        w.open.weak_bg_fill = t.active;
        w.open.bg_stroke = Stroke::new(1.0_f32, t.border_strong);
        w.open.fg_stroke = Stroke::new(1.0_f32, t.text);
        w.open.corner_radius = CornerRadius::same(radius::MD as u8);

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
            style.spacing.menu_margin = egui::Margin::same(space::XS as i8);
            style.spacing.indent = space::LG;
            style.spacing.slider_width = 120.0;
            style.spacing.interact_size = egui::vec2(0.0, 26.0);
            style.spacing.scroll.bar_width = 8.0;
            style.spacing.scroll.floating = true;
            style.visuals.striped = true;

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

        log::info!(
            "字体栈就绪: 文本 {proportional:?} / 等宽 {monospace:?}，耗时 {:.1} ms",
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

/// Convenience: a rounded selection highlight drawn behind a row.
pub fn row_fill(tokens: &Tokens, selected: bool, hovered: bool) -> Color32 {
    if selected {
        tokens.accent.gamma_multiply(0.28)
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
            .chain(CJK_FONT_CANDIDATES.iter().map(|(file, _)| *file))
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
