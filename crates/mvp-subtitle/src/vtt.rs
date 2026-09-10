//! WebVTT (`.vtt`) parsing.
//!
//! Handles the `WEBVTT` header, skips `NOTE`, `STYLE` and `REGION` blocks, reads
//! optional cue identifiers and accepts both `HH:MM:SS.mmm` and `MM:SS.mmm`
//! timings. Cue settings after the end timestamp are read for `align:` (mapped
//! to [`Align`]) and `position:` (used as a tie-breaker when `align:` is
//! missing); `line:` is recognised syntactically but not consumed, because
//! [`CueStyle`] has no vertical-line field.

use crate::model::{Align, Cue, CueStyle, Parsed};
use crate::text::{decode_entities, finish_text, strip_angle_tags};
use crate::time::parse_timecode;

/// Parse WebVTT text into a title and cues.
pub fn parse(input: &str) -> Parsed {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let body = normalized.strip_prefix('\u{feff}').unwrap_or(&normalized);

    let mut title = None;
    let mut cues = Vec::new();

    for block in split_blocks(body) {
        let first = block[0].trim();
        if is_header_line(first) {
            let rest = first[6..].trim().trim_start_matches('-').trim();
            if !rest.is_empty() {
                title = Some(rest.to_string());
            }
            continue;
        }
        if is_skippable_block(first) {
            continue;
        }

        // A cue block is either `timing` + text, or `identifier` + `timing` + text.
        let (timing_index, timing_line) = if first.contains("-->") {
            (0usize, first)
        } else if block.len() > 1 && block[1].contains("-->") {
            (1usize, block[1].trim())
        } else {
            continue;
        };
        let Some((start, end, align)) = parse_timing_line(timing_line) else {
            continue;
        };

        let raw = block[timing_index + 1..].join("\n");
        let text = clean_text(&raw);
        if text.is_empty() {
            continue;
        }
        cues.push(Cue {
            start,
            end,
            text,
            style: CueStyle {
                align,
                ..CueStyle::default()
            },
        });
    }

    Parsed { title, cues }
}

/// Split a WebVTT document into blocks on blank lines.
fn split_blocks(body: &str) -> Vec<Vec<&str>> {
    let mut blocks = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in body.split('\n') {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

/// `WEBVTT`, `WEBVTT - Title`, `webvtt`.
fn is_header_line(line: &str) -> bool {
    line.get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("WEBVTT"))
}

/// `NOTE`, `STYLE` and `REGION` blocks carry no cues.
fn is_skippable_block(first_line: &str) -> bool {
    let upper = first_line.to_ascii_uppercase();
    upper == "NOTE"
        || upper.starts_with("NOTE ")
        || upper.starts_with("NOTE\t")
        || upper == "STYLE"
        || upper == "REGION"
}

/// Parse `00:00:01.000 --> 00:00:04.000 align:start position:10%`.
fn parse_timing_line(line: &str) -> Option<(f64, f64, Align)> {
    let (left, right) = line.split_once("-->")?;
    let start = parse_timecode(left.trim())?;

    let mut tokens = right.split_whitespace();
    let end = parse_timecode(tokens.next()?)?;

    let mut align: Option<Align> = None;
    let mut position: Option<f64> = None;
    for token in tokens {
        if let Some(value) = token.strip_prefix("align:") {
            align = align_from_setting(value);
        } else if let Some(value) = token.strip_prefix("position:") {
            position = value
                .trim_end_matches('%')
                .parse::<f64>()
                .ok()
                .filter(|percent| percent.is_finite());
        }
    }

    let align = align.or_else(|| position.and_then(align_from_position));
    Some((start, end, align.unwrap_or_default()))
}

/// `align:start|center|end` (plus the `left`/`right` aliases) -> [`Align`].
fn align_from_setting(value: &str) -> Option<Align> {
    match value.trim().to_ascii_lowercase().as_str() {
        "start" | "left" => Some(Align::BottomLeft),
        "center" | "middle" => Some(Align::BottomCenter),
        "end" | "right" => Some(Align::BottomRight),
        _ => None,
    }
}

/// Guess an anchor from a `position:N%` setting when `align:` is absent.
fn align_from_position(percent: f64) -> Option<Align> {
    if !(0.0..=100.0).contains(&percent) {
        return None;
    }
    Some(if percent < 33.0 {
        Align::BottomLeft
    } else if percent > 66.0 {
        Align::BottomRight
    } else {
        Align::BottomCenter
    })
}

/// Strip WebVTT markup and resolve the entities the format allows.
fn clean_text(raw: &str) -> String {
    finish_text(&decode_entities(&strip_angle_tags(raw)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_variants() {
        assert!(is_header_line("WEBVTT"));
        assert!(is_header_line("webvtt - something"));
        assert!(!is_header_line("WEBVT"));
        // A multi-byte first character must not panic on the 6-byte slice; in
        // `abc😀` byte index 6 falls inside the emoji.
        assert!(!is_header_line("你好世界你好世界"));
        assert!(!is_header_line("abc😀def"));
    }

    #[test]
    fn settings_map_to_alignment() {
        assert_eq!(align_from_setting("start"), Some(Align::BottomLeft));
        assert_eq!(align_from_setting("end"), Some(Align::BottomRight));
        assert_eq!(align_from_setting("center"), Some(Align::BottomCenter));
        assert_eq!(align_from_setting("wat"), None);
        assert_eq!(align_from_position(5.0), Some(Align::BottomLeft));
        assert_eq!(align_from_position(50.0), Some(Align::BottomCenter));
        assert_eq!(align_from_position(95.0), Some(Align::BottomRight));
    }

    #[test]
    fn short_timings_and_settings() {
        let parsed = parse_timing_line("01:02.500 --> 01:04.000 align:end line:0 position:90%");
        assert_eq!(parsed, Some((62.5, 64.0, Align::BottomRight)));
        assert!(parse_timing_line("nope").is_none());
    }

    #[test]
    fn note_and_style_blocks_are_skipped() {
        let vtt = "WEBVTT\n\nNOTE a note\nwith two lines\n\nSTYLE\n::cue { color: red }\n\n00:00:01.000 --> 00:00:02.000\nhi\n";
        let parsed = parse(vtt);
        assert_eq!(parsed.cues.len(), 1);
        assert_eq!(parsed.cues[0].text, "hi");
    }

    #[test]
    fn tags_and_entities_are_cleaned() {
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n<i>a</i> &amp; <v Bob>b</v>\n";
        let parsed = parse(vtt);
        assert_eq!(parsed.cues[0].text, "a & b");
    }
}
