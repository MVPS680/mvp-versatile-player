//! Visual design tokens and the `egui` style the whole player is built from.
//!
//! The palette is a deliberately desaturated dark scheme: a media player is
//! watched next to its own picture, so the chrome must never compete with the
//! video. Surfaces step up in lightness (`bg` → `panel` → `elevated`) and a
//! single violet accent carries every interactive highlight, which keeps the
//! interface legible without a rainbow of status colours.

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
    pub fn install(&self, ctx: &Context) {
        install_fonts(ctx);

        let t = &self.tokens;
        let mut visuals = Visuals::dark();

        visuals.panel_fill = t.panel;
        visuals.window_fill = t.elevated;
        visuals.extreme_bg_color = t.sunken;
        visuals.faint_bg_color = t.hover;
        visuals.code_bg_color = t.sunken;
        visuals.window_stroke = Stroke::new(1.0, t.border_strong);
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
        visuals.selection.stroke = Stroke::new(1.0, t.accent_hover);
        visuals.slider_trailing_fill = true;

        let w = &mut visuals.widgets;
        w.noninteractive.bg_fill = t.panel;
        w.noninteractive.weak_bg_fill = t.panel;
        w.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, t.text_weak);
        w.noninteractive.corner_radius = CornerRadius::same(radius::MD as u8);

        w.inactive.bg_fill = t.hover;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::new(1.0, t.border);
        w.inactive.fg_stroke = Stroke::new(1.0, t.text);
        w.inactive.corner_radius = CornerRadius::same(radius::MD as u8);

        w.hovered.bg_fill = t.active;
        w.hovered.weak_bg_fill = t.active;
        w.hovered.bg_stroke = Stroke::new(1.0, t.border_strong);
        w.hovered.fg_stroke = Stroke::new(1.0, t.text);
        w.hovered.corner_radius = CornerRadius::same(radius::MD as u8);

        w.active.bg_fill = t.accent.gamma_multiply(0.85);
        w.active.weak_bg_fill = t.accent.gamma_multiply(0.85);
        w.active.bg_stroke = Stroke::new(1.0, t.accent_hover);
        w.active.fg_stroke = Stroke::new(1.0, t.on_accent);
        w.active.corner_radius = CornerRadius::same(radius::MD as u8);

        w.open.bg_fill = t.active;
        w.open.weak_bg_fill = t.active;
        w.open.bg_stroke = Stroke::new(1.0, t.border_strong);
        w.open.fg_stroke = Stroke::new(1.0, t.text);
        w.open.corner_radius = CornerRadius::same(radius::MD as u8);

        ctx.set_visuals(visuals);

        let mut style = (*ctx.style()).clone();
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

        ctx.set_style(style);
    }
}

/// Candidate CJK-capable fonts, in the order Windows ships them.
///
/// Microsoft YaHei is present on every Windows 8+ install regardless of the
/// display language, so the first entry almost always wins and the rest are
/// belt-and-braces for stripped-down images.
const CJK_FONT_CANDIDATES: &[&str] = &[
    "C:/Windows/Fonts/msyh.ttc",
    "C:/Windows/Fonts/msyh.ttf",
    "C:/Windows/Fonts/msyhl.ttc",
    "C:/Windows/Fonts/simhei.ttf",
    "C:/Windows/Fonts/Deng.ttf",
    "C:/Windows/Fonts/simsun.ttc",
    "C:/Windows/Fonts/msjh.ttc",
];

static FONTS_INSTALLED: std::sync::Once = std::sync::Once::new();

/// Load a CJK font and append it to the default family.
///
/// `egui`'s built-in fonts have no CJK coverage at all, so without this every
/// Chinese label would render as tofu boxes. Loading is done once per process.
fn install_fonts(ctx: &Context) {
    FONTS_INSTALLED.call_once(|| {
        let mut fonts = egui::FontDefinitions::default();
        for path in CJK_FONT_CANDIDATES {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let data = egui::FontData::from_owned(bytes);
            fonts.font_data.insert("cjk".to_owned(), std::sync::Arc::new(data));
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".to_owned());
            }
            log::info!("已加载中文字体: {path}");
            break;
        }
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
    fn cjk_font_candidates_all_look_like_windows_font_paths() {
        for path in CJK_FONT_CANDIDATES {
            assert!(path.starts_with("C:/Windows/Fonts/"));
            assert!(path.ends_with(".ttc") || path.ends_with(".ttf"));
        }
    }
}
