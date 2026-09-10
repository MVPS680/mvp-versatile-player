//! MicroDVD (`.sub`) parsing.
//!
//! MicroDVD stores frame numbers instead of timestamps: `{start}{end}text`, with
//! `|` as the line separator. Frames are converted with a frame rate, which is
//! either supplied by the caller or read from the optional `{1}{1}<fps>`
//! declaration that some files carry on their first line.
//!
//! Because the frame rate is a guess, [`Subtitle::parse`](crate::Subtitle::parse)
//! uses [`DEFAULT_FPS`]; use [`parse_microdvd`] when the real frame rate is known.

use crate::model::{Cue, CueStyle, Parsed, Subtitle};
use crate::text::finish_text;

/// Frame rate assumed when a MicroDVD file does not declare one.
pub const DEFAULT_FPS: f64 = 23.976;

/// Lower bound of a plausible declared frame rate.
const MIN_FPS: f64 = 1.0;
/// Upper bound of a plausible declared frame rate.
const MAX_FPS: f64 = 240.0;

/// Sanitise a caller-supplied frame rate, falling back to [`DEFAULT_FPS`].
fn effective_fps(fps: f64) -> f64 {
    if fps.is_finite() && (MIN_FPS..=MAX_FPS).contains(&fps) {
        fps
    } else {
        DEFAULT_FPS
    }
}

/// Returns `true` when `line` starts with a `{frame}{frame}` pair.
pub(crate) fn is_microdvd_line(line: &str) -> bool {
    parse_entry(line).is_some()
}

/// Split `{start}{end}text` into its frame numbers and text.
fn parse_entry(line: &str) -> Option<(i64, i64, &str)> {
    let rest = line.trim_start();
    let start_brace = rest.strip_prefix('{')?;
    let (start, rest) = start_brace.split_once('}')?;
    let rest = rest.strip_prefix('{')?;
    let (end, text) = rest.split_once('}')?;
    let start = start.trim().parse::<i64>().ok()?;
    let end = end.trim().parse::<i64>().ok()?;
    Some((start, end, text))
}

/// Parse MicroDVD text at `fps` frames per second.
pub fn parse(input: &str, fps: f64) -> Parsed {
    let mut fps = effective_fps(fps);
    let mut cues: Vec<Cue> = Vec::new();
    let mut first_entry = true;

    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((start_frame, end_frame, text)) = parse_entry(line) else {
            continue;
        };
        // The optional `{1}{1}23.976` header declares the frame rate.
        if first_entry {
            first_entry = false;
            if start_frame == 1 && end_frame == 1 {
                if let Ok(declared) = text.trim().parse::<f64>() {
                    if declared.is_finite() && (MIN_FPS..=MAX_FPS).contains(&declared) {
                        fps = declared;
                        continue;
                    }
                }
            }
        }

        let (text, style) = render_text(text);
        if text.is_empty() {
            continue;
        }
        cues.push(Cue {
            start: start_frame as f64 / fps,
            end: end_frame as f64 / fps,
            text,
            style,
        });
    }

    Parsed { title: None, cues }
}

/// Decode MicroDVD bytes at `fps` and wrap the result as a [`Subtitle`].
///
/// The encoding is auto-detected; MicroDVD has no title.
pub fn parse_microdvd(bytes: &[u8], fps: f64) -> Subtitle {
    let (text, encoding) = crate::encoding::decode_text(bytes, crate::encoding::EncodingHint::Auto);
    let parsed = parse(&text, fps);
    let mut cues = parsed.cues;
    crate::model::sort_cues(&mut cues);
    Subtitle {
        format: crate::model::SubFormat::MicroDvd,
        title: None,
        cues,
        encoding,
    }
}

/// Render MicroDVD text: `|` becomes a newline and `{y:..}` / `{c:$..}` blocks
/// become style flags. Any other `{...}` block is dropped.
fn render_text(raw: &str) -> (String, CueStyle) {
    let mut style = CueStyle::default();
    let mut text = String::with_capacity(raw.len());
    let mut chars = raw.chars();

    while let Some(c) = chars.next() {
        match c {
            '|' => text.push('\n'),
            '{' => {
                let mut block = String::new();
                for inner in chars.by_ref() {
                    if inner == '}' {
                        break;
                    }
                    block.push(inner);
                }
                apply_block(&block, &mut style);
            }
            _ => text.push(c),
        }
    }

    (finish_text(&text), style)
}

/// Apply one MicroDVD `{...}` block: `y:` styles and `c:$` colours.
fn apply_block(block: &str, style: &mut CueStyle) {
    let block = block.trim();
    let lower = block.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("y:") {
        for flag in rest.split([':', ' ']) {
            match flag.trim() {
                "b" => style.bold = true,
                "i" => style.italic = true,
                "u" => style.underline = true,
                "s" => style.strikeout = true,
                _ => {}
            }
        }
        return;
    }
    if let Some(rest) = lower.strip_prefix("c:$") {
        // MicroDVD writes colours as `$BBGGRR`, the same byte order as ASS.
        let hex = rest.trim_end_matches('}').trim();
        if hex.len() <= 8 && !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(value) = u32::from_str_radix(hex, 16) {
                style.color = Some([
                    (value & 0xFF) as u8,
                    ((value >> 8) & 0xFF) as u8,
                    ((value >> 16) & 0xFF) as u8,
                ]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_pairs_are_recognised() {
        assert_eq!(parse_entry("{1}{2}hello"), Some((1, 2, "hello")));
        assert_eq!(parse_entry("{100}{200}"), Some((100, 200, "")));
        assert_eq!(parse_entry("1 00:00:01,000 --> 00:00:02,000"), None);
        assert!(is_microdvd_line("{1}{1}23.976"));
    }

    #[test]
    fn frames_convert_to_seconds() {
        let parsed = parse("{1}{1}23.976\n{100}{200}hello|world\n", 23.976);
        assert_eq!(parsed.cues.len(), 1);
        assert!((parsed.cues[0].start - 100.0 / 23.976).abs() < 1e-9);
        assert_eq!(parsed.cues[0].text, "hello\nworld");
    }

    #[test]
    fn declared_fps_wins_and_style_blocks_apply() {
        let parsed = parse("{1}{1}25.0\n{25}{50}{y:i}{c:$0000FF}styled\n", 23.976);
        assert_eq!(parsed.cues.len(), 1);
        assert_eq!(parsed.cues[0].start, 1.0);
        assert_eq!(parsed.cues[0].end, 2.0);
        assert_eq!(parsed.cues[0].text, "styled");
        assert!(parsed.cues[0].style.italic);
        assert_eq!(parsed.cues[0].style.color, Some([255, 0, 0]));
    }

    #[test]
    fn garbage_fps_falls_back_to_the_default() {
        assert_eq!(effective_fps(0.0), DEFAULT_FPS);
        assert_eq!(effective_fps(f64::NAN), DEFAULT_FPS);
        assert_eq!(effective_fps(30.0), 30.0);
    }
}
