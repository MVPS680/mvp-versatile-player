//! Advanced SubStation Alpha / Sub Station Alpha (`.ass`, `.ssa`) parsing.
//!
//! The interesting parts of ASS are the style table and the inline override
//! language, so this module does three things:
//!
//! * it reads `Title` from `[Script Info]`;
//! * it builds the named style table from `[V4+ Styles]` (or the legacy
//!   `[V4 Styles]`), keyed by the columns its own `Format:` line declares;
//! * it renders every `Dialogue:` line's `Text` field to plain text, honouring
//!   the overrides the cue model can represent (`\b`, `\i`, `\u`, `\s`,
//!   `\c`/`\1c`, `\an`, `\a`, `\pos`, `\r`, `\p`) and dropping everything else,
//!   including `\p1` vector-drawing payloads.
//!
//! `[Events]` columns are read from that section's `Format:` line, so files that
//! put `Text` first or reorder the other columns parse correctly. `Comment:`,
//! `Picture:`, `Sound:`, `Movie:` and `Command:` lines are ignored.

use std::collections::HashMap;

use crate::model::{Align, Cue, CueStyle, Parsed};
use crate::text::finish_text;
use crate::time::parse_timecode;

/// A style declared in an ASS/SSA `[V4+ Styles]` / `[V4 Styles]` table.
#[derive(Debug, Clone, PartialEq)]
pub struct AssStyle {
    /// Style name, as referenced by the `Style` column of a `Dialogue:` line.
    pub name: String,
    /// The attributes this crate's cue model can carry.
    pub cue: CueStyle,
    /// Declared font size in points, retained for callers that want it even
    /// though [`CueStyle`] has no font-size field.
    pub font_size: Option<f64>,
}

/// A `Dialogue:` line before its style has been resolved.
struct RawDialogue {
    start: f64,
    end: f64,
    style_name: String,
    text: String,
    margin_v: Option<i32>,
}

/// Parse ASS/SSA text into a title and cues.
pub fn parse(input: &str) -> Parsed {
    let document = Document::parse(input);
    let styles: HashMap<&str, &AssStyle> = document
        .styles
        .iter()
        .map(|style| (style.name.as_str(), style))
        .collect();

    let mut cues = Vec::with_capacity(document.dialogues.len());
    for dialogue in &document.dialogues {
        let mut base = styles
            .get(dialogue.style_name.as_str())
            .map(|style| style.cue.clone())
            .unwrap_or_default();
        // The per-event MarginV wins over the style, and an inline `\pos` wins
        // over both because it is applied while the text is rendered.
        if dialogue.margin_v.is_some() {
            base.margin_v = dialogue.margin_v;
        }
        let (text, style) = render_text(&dialogue.text, &base);
        if text.is_empty() {
            continue;
        }
        cues.push(Cue {
            start: dialogue.start,
            end: dialogue.end,
            text,
            style,
        });
    }

    Parsed {
        title: document.title,
        cues,
    }
}

/// Parse only the named style table of an ASS/SSA document, in file order.
pub fn parse_styles(input: &str) -> Vec<AssStyle> {
    Document::parse(input).styles
}

/// Everything the module extracts from one ASS/SSA document.
struct Document {
    title: Option<String>,
    styles: Vec<AssStyle>,
    dialogues: Vec<RawDialogue>,
}

impl Document {
    fn parse(input: &str) -> Self {
        let mut document = Document {
            title: None,
            styles: Vec::new(),
            dialogues: Vec::new(),
        };
        let mut section = String::new();
        let mut style_columns: Vec<String> = Vec::new();
        let mut event_columns: Vec<String> = Vec::new();

        for raw_line in input.lines() {
            let line = raw_line.trim_start_matches('\u{feff}').trim();
            if line.is_empty() {
                continue;
            }
            if let Some(name) = section_name(line) {
                section = name;
                style_columns.clear();
                event_columns.clear();
                continue;
            }
            let Some((key, value)) = split_key_value(line) else {
                continue;
            };
            let key = key.to_ascii_lowercase();
            match section.as_str() {
                "script info" => {
                    if key == "title" {
                        let value = value.trim();
                        if !value.is_empty() {
                            document.title = Some(value.to_string());
                        }
                    }
                }
                "v4+ styles" | "v4 styles" => match key.as_str() {
                    "format" => style_columns = split_list(value),
                    "style" => {
                        if let Some(style) = parse_style_line(&style_columns, value) {
                            document.styles.push(style);
                        }
                    }
                    _ => {}
                },
                "events" => match key.as_str() {
                    "format" => event_columns = split_list(value),
                    // Comment:, Picture:, Sound:, Movie: and Command: lines all
                    // fall through here and are intentionally dropped.
                    "dialogue" => {
                        if let Some(dialogue) = parse_dialogue_line(value, &event_columns) {
                            document.dialogues.push(dialogue);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        document
    }
}

/// `[Script Info]` -> `Some("script info")`.
fn section_name(line: &str) -> Option<String> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let inner = inner.trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_ascii_lowercase())
    }
}

/// `Title: Foo` -> `("Title", "Foo")`.
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    Some((key.trim(), value.trim()))
}

/// Split a `Format:` line into trimmed, lower-case-free column names.
fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Look a column up by name, case-insensitively.
fn column<'a>(columns: &[String], values: &'a [String], name: &str) -> Option<&'a str> {
    let index = columns
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(name))?;
    values.get(index).map(String::as_str)
}

/// The event column order used when a file forgets its `Format:` line.
fn default_event_columns() -> Vec<String> {
    [
        "Layer", "Start", "End", "Style", "Name", "MarginL", "MarginR", "MarginV", "Effect", "Text",
    ]
    .iter()
    .map(|name| (*name).to_string())
    .collect()
}

/// Split a `Style:` or `Dialogue:` payload into exactly `columns.len()` fields.
///
/// The `Text` column is allowed to contain commas: everything between it and the
/// columns that follow it is re-joined, which keeps reordered `Format:` lines
/// (including ones where `Text` is not last) working.
fn split_record(value: &str, columns: &[String]) -> Vec<String> {
    let count = columns.len();
    if count == 0 {
        return Vec::new();
    }
    let parts: Vec<&str> = value.split(',').collect();

    if let Some(text_index) = columns
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case("text"))
    {
        if text_index < count && parts.len() > count {
            let trailing = count - text_index - 1;
            let end = parts.len() - trailing;
            let mut out = Vec::with_capacity(count);
            out.extend(
                parts[..text_index]
                    .iter()
                    .map(|part| part.trim().to_string()),
            );
            out.push(parts[text_index..end].join(","));
            out.extend(parts[end..].iter().map(|part| part.trim().to_string()));
            return out;
        }
    }

    let mut out: Vec<String> = parts.iter().map(|part| part.trim().to_string()).collect();
    out.resize(count, String::new());
    out.truncate(count);
    out
}

/// Parse one `Style:` line against the style table's column order.
fn parse_style_line(columns: &[String], value: &str) -> Option<AssStyle> {
    if columns.is_empty() {
        return None;
    }
    let values = split_record(value, columns);
    let name = column(columns, &values, "name")?.trim().to_string();
    if name.is_empty() {
        return None;
    }

    let cue = CueStyle {
        bold: column(columns, &values, "bold").is_some_and(style_flag),
        italic: column(columns, &values, "italic").is_some_and(style_flag),
        underline: column(columns, &values, "underline").is_some_and(style_flag),
        strikeout: column(columns, &values, "strikeout").is_some_and(style_flag),
        color: column(columns, &values, "primarycolour").and_then(parse_ass_color),
        align: column(columns, &values, "alignment")
            .and_then(align_from_number)
            .unwrap_or_default(),
        margin_v: column(columns, &values, "marginv").and_then(parse_nonzero_i32),
    };

    let font_size = column(columns, &values, "fontsize")
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|size| size.is_finite() && *size > 0.0);

    Some(AssStyle {
        name,
        cue,
        font_size,
    })
}

/// Parse one `Dialogue:` line against the event column order.
fn parse_dialogue_line(value: &str, columns: &[String]) -> Option<RawDialogue> {
    let owned;
    let columns = if columns.is_empty() {
        owned = default_event_columns();
        &owned[..]
    } else {
        columns
    };
    let values = split_record(value, columns);

    let start = parse_timecode(column(columns, &values, "start")?)?;
    let end = parse_timecode(column(columns, &values, "end")?)?;
    let style_name = column(columns, &values, "style")
        .unwrap_or_default()
        .trim()
        .to_string();
    let text = column(columns, &values, "text")
        .unwrap_or_default()
        .to_string();
    let margin_v = column(columns, &values, "marginv").and_then(parse_nonzero_i32);

    Some(RawDialogue {
        start,
        end,
        style_name,
        text,
        margin_v,
    })
}

/// Parse an ASS colour argument such as `&H00FF00&` or `&H0000FF&`.
///
/// ASS stores colours as `&HAABBGGRR&`, so the low byte is red: `&H0000FF&` is
/// pure red. A missing alpha byte and a missing trailing `&` are both accepted.
fn parse_ass_color(arg: &str) -> Option<[u8; 3]> {
    let trimmed = arg.trim();
    let hex = trimmed
        .strip_prefix("&H")
        .or_else(|| trimmed.strip_prefix("&h"))
        .or_else(|| trimmed.strip_prefix('H'))
        .or_else(|| trimmed.strip_prefix('h'))
        .unwrap_or(trimmed);
    let hex = hex.trim_end_matches('&').trim();
    if hex.is_empty() || hex.len() > 8 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    Some([
        (value & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        ((value >> 16) & 0xFF) as u8,
    ])
}

/// `Alignment` column / `\an` argument: the numeric-keypad layout, 1..=9.
///
/// Values 10 and 11 are accepted as the legacy SSA middle-centre/middle-right
/// aliases; see the crate documentation for that ambiguity.
fn align_from_number(value: &str) -> Option<Align> {
    let n: i32 = value.trim().parse().ok()?;
    Some(match n {
        1 => Align::BottomLeft,
        2 => Align::BottomCenter,
        3 => Align::BottomRight,
        4 => Align::MiddleLeft,
        5 => Align::MiddleCenter,
        6 => Align::MiddleRight,
        7 => Align::TopLeft,
        8 => Align::TopCenter,
        9 => Align::TopRight,
        10 => Align::MiddleCenter,
        11 => Align::MiddleRight,
        _ => return None,
    })
}

/// The legacy SSA `\a` argument: horizontal in bits 0-1, top in bit 2, middle in
/// bit 3.
fn align_from_legacy(value: &str) -> Option<Align> {
    let n: i32 = value.trim().parse().ok()?;
    if !(1..=11).contains(&n) {
        return None;
    }
    let horizontal = n & 3;
    let left = horizontal == 1;
    let right = horizontal == 3;
    Some(if n & 8 != 0 {
        match (left, right) {
            (true, _) => Align::MiddleLeft,
            (_, true) => Align::MiddleRight,
            _ => Align::MiddleCenter,
        }
    } else if n & 4 != 0 {
        match (left, right) {
            (true, _) => Align::TopLeft,
            (_, true) => Align::TopRight,
            _ => Align::TopCenter,
        }
    } else {
        match (left, right) {
            (true, _) => Align::BottomLeft,
            (_, true) => Align::BottomRight,
            _ => Align::BottomCenter,
        }
    })
}

/// A style-table flag: `-1`, `1`, `true` mean on, `0` and an empty field mean off.
fn style_flag(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    match value.parse::<i32>() {
        Ok(n) => n != 0,
        Err(_) => value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes"),
    }
}

/// An override flag: `\b1`/`\b0`/`\b700`, or a bare `\b`, which means "on".
fn tag_flag(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    match value.parse::<i32>() {
        Ok(weight) => weight != 0,
        // `\bfoo` is not a weight but the tag is present, so treat it as "on"
        // rather than silently dropping the emphasis.
        Err(_) => true,
    }
}

/// Parse an `i32`, treating `0` (ASS's "unset") as absent.
fn parse_nonzero_i32(value: &str) -> Option<i32> {
    value.trim().parse::<i32>().ok().filter(|v| *v != 0)
}

/// Split an override segment such as `1c&H0000FF&` into its numeric prefix, its
/// lower-case tag name and its argument.
fn split_tag(segment: &str) -> (&str, String, &str) {
    let digits_end = segment
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(segment.len());
    let rest = &segment[digits_end..];
    let name_end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    (
        &segment[..digits_end],
        rest[..name_end].to_ascii_lowercase(),
        &rest[name_end..],
    )
}

/// Render an ASS `Text` field to plain text, returning the resulting style too.
///
/// `{...}` blocks are consumed as overrides, `\N` becomes a real newline, `\n`
/// becomes a space and `\h` a non-breaking space. While a `\p1` drawing block is
/// open, all literal text is discarded.
fn render_text(raw: &str, base: &CueStyle) -> (String, CueStyle) {
    let mut style = base.clone();
    let mut drawing = false;
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();

    while let Some(c) = chars.next() {
        match c {
            '{' => {
                let mut block = String::new();
                for inner in chars.by_ref() {
                    if inner == '}' {
                        break;
                    }
                    block.push(inner);
                }
                apply_override_block(&block, base, &mut style, &mut drawing);
            }
            '\\' => match chars.next() {
                Some('N') => {
                    if !drawing {
                        out.push('\n');
                    }
                }
                Some('n') => {
                    if !drawing {
                        out.push(' ');
                    }
                }
                Some('h') => {
                    if !drawing {
                        out.push('\u{a0}');
                    }
                }
                Some(other) => {
                    if !drawing {
                        out.push('\\');
                        out.push(other);
                    }
                }
                None => {
                    if !drawing {
                        out.push('\\');
                    }
                }
            },
            _ => {
                if !drawing {
                    out.push(c);
                }
            }
        }
    }

    (finish_text(&out), style)
}

/// Apply every `\tag` inside one `{...}` override block.
fn apply_override_block(block: &str, base: &CueStyle, style: &mut CueStyle, drawing: &mut bool) {
    for segment in block.split('\\') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        let (prefix, name, arg) = split_tag(segment);
        match name.as_str() {
            "b" => style.bold = tag_flag(arg),
            "i" => style.italic = tag_flag(arg),
            "u" => style.underline = tag_flag(arg),
            "s" => style.strikeout = tag_flag(arg),
            "an" => {
                if let Some(align) = align_from_number(arg) {
                    style.align = align;
                }
            }
            "a" if prefix.is_empty() => {
                if let Some(align) = align_from_legacy(arg) {
                    style.align = align;
                }
            }
            "pos" => {
                if let Some(margin_v) = parse_position_margin(arg) {
                    style.margin_v = Some(margin_v);
                }
            }
            // `\p1` opens a vector-drawing block whose payload is not text;
            // `\p0` closes it.
            "p" => {
                let level = arg.trim().parse::<i32>().unwrap_or(0);
                *drawing = level > 0;
            }
            // `\r` restores the line's own style. A named reset target
            // (`\rOtherStyle`) is approximated by the same restore.
            "r" => {
                let was_drawing = *drawing;
                *style = base.clone();
                *drawing = was_drawing;
            }
            "c" if prefix.is_empty() || prefix == "1" => {
                if let Some(color) = parse_ass_color(arg) {
                    style.color = Some(color);
                }
            }
            _ => {}
        }
    }
}

/// `\pos(320,50)` -> the vertical component, used as the margin override.
fn parse_position_margin(arg: &str) -> Option<i32> {
    let inner = arg.trim().trim_start_matches('(').trim_end_matches(')');
    let (_, y) = inner.split_once(',')?;
    let y: f64 = y.trim().parse().ok()?;
    if !y.is_finite() {
        return None;
    }
    Some(y.round() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "[Script Info]\nTitle: Demo\nScriptType: v4.00+\n\n[V4+ Styles]\n\
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n\
Style: Top,Arial,24,&H0000FF00,&H000000FF,&H00000000,&H00000000,-1,-1,0,0,100,100,0,0,1,2,0,8,10,10,0,1\n";

    #[test]
    fn script_info_and_styles() {
        let styles = parse_styles(HEADER);
        assert_eq!(styles.len(), 2);
        assert_eq!(styles[0].name, "Default");
        assert_eq!(styles[0].font_size, Some(20.0));
        assert!(!styles[0].cue.bold);
        assert_eq!(styles[0].cue.color, Some([255, 255, 255]));
        assert_eq!(styles[0].cue.margin_v, Some(10));
        assert_eq!(styles[1].name, "Top");
        assert!(styles[1].cue.bold);
        assert!(styles[1].cue.italic);
        assert_eq!(styles[1].cue.color, Some([0, 255, 0]));
        assert_eq!(styles[1].cue.align, Align::TopCenter);
        assert_eq!(styles[1].cue.margin_v, None);
    }

    #[test]
    fn colours_are_bgr() {
        assert_eq!(parse_ass_color("&H0000FF&"), Some([255, 0, 0]));
        assert_eq!(parse_ass_color("&HFF0000&"), Some([0, 0, 255]));
        assert_eq!(parse_ass_color("&H00FFFFFF"), Some([255, 255, 255]));
        assert_eq!(parse_ass_color("nonsense"), None);
    }

    #[test]
    fn tag_splitting() {
        assert_eq!(split_tag("b1"), ("", "b".to_string(), "1"));
        assert_eq!(split_tag("1c&HFF&"), ("1", "c".to_string(), "&HFF&"));
        assert_eq!(split_tag("pos(1,2)"), ("", "pos".to_string(), "(1,2)"));
        assert_eq!(split_tag(""), ("", String::new(), ""));
    }

    #[test]
    fn drawing_blocks_are_dropped() {
        let (text, _) = render_text(r"{\p1}m 0 0 l 100 0{\p0}Caption", &CueStyle::default());
        assert_eq!(text, "Caption");
    }

    #[test]
    fn overrides_change_style_and_reset() {
        let base = CueStyle {
            bold: false,
            ..CueStyle::default()
        };
        let (text, style) = render_text(r"{\b1\i1\an8\c&H0000FF&}Hi", &base);
        assert_eq!(text, "Hi");
        assert!(style.bold && style.italic);
        assert_eq!(style.align, Align::TopCenter);
        assert_eq!(style.color, Some([255, 0, 0]));

        let (_, reset) = render_text(r"{\b1}bold{\r}plain", &base);
        assert!(!reset.bold);
    }

    #[test]
    fn escapes_become_whitespace() {
        let (text, _) = render_text(r"one\Ntwo\nthree\htail", &CueStyle::default());
        assert_eq!(text, "one\ntwo three\u{a0}tail");
    }
}
