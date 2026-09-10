//! SubRip (`.srt`) parsing.
//!
//! The parser is built to survive real files rather than to validate them. It
//! tolerates:
//!
//! * `\r\n` and `\n` (and bare `\r`) line endings;
//! * a missing or non-numeric index line;
//! * a missing hours field (`MM:SS,mmm`) or more than two hour digits;
//! * `,` or `.` as the millisecond separator;
//! * a UTF-8 BOM;
//! * blank lines inside a cue;
//! * trailing garbage after a cue;
//! * a last cue with no trailing newline.
//!
//! A block whose timing line cannot be parsed is skipped, never fatal: a
//! partially parsed file beats no subtitles at all.

use crate::model::{Cue, CueStyle, Parsed};
use crate::text::{convert_newline_escapes, finish_text, strip_angle_tags, strip_brace_blocks};
use crate::time::parse_timecode;

/// Parse SubRip text into cues.
///
/// `input` is expected to be already decoded; use [`crate::Subtitle::parse`] to
/// handle bytes, encoding detection and format sniffing together.
pub fn parse(input: &str) -> Parsed {
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");
    let body = normalized.strip_prefix('\u{feff}').unwrap_or(&normalized);
    let lines: Vec<&str> = body.split('\n').collect();

    let mut cues: Vec<Cue> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let head = lines[i].trim();
        if head.is_empty() {
            i += 1;
            continue;
        }

        // A block starts either at its timing line or at an index line directly
        // above one. Anything else is stray text and gets dropped.
        let timing_idx = if is_index_line(head) {
            match lines.get(i + 1) {
                Some(next) if next.contains("-->") => i + 1,
                _ => {
                    i += 1;
                    continue;
                }
            }
        } else if head.contains("-->") {
            i
        } else {
            i += 1;
            continue;
        };

        let Some((start, end)) = parse_timing_line(lines[timing_idx].trim()) else {
            // Malformed timing: skip this block's header and keep scanning.
            i = timing_idx + 1;
            continue;
        };

        let mut raw = String::new();
        let mut wrote_line = false;
        let mut k = timing_idx + 1;
        while k < lines.len() {
            let text_line = lines[k].trim();
            if text_line.contains("-->") {
                break;
            }
            if text_line.is_empty() {
                // A blank line normally ends the cue, but some tools emit one in
                // the middle of a cue's text; only stop when what follows really
                // looks like the next block. A bare number is an index line,
                // which is how a malformed block gets skipped instead of being
                // swallowed into the cue above it.
                let mut n = k;
                while n < lines.len() && lines[n].trim().is_empty() {
                    n += 1;
                }
                let next_starts_cue = match lines.get(n) {
                    Some(next) => {
                        let next = next.trim();
                        next.contains("-->") || is_index_line(next)
                    }
                    None => true,
                };
                if next_starts_cue {
                    k = n;
                    break;
                }
                if wrote_line {
                    raw.push('\n');
                }
                k = n;
                continue;
            }
            if is_index_line(text_line) && lines.get(k + 1).is_some_and(|l| l.contains("-->")) {
                break;
            }
            if wrote_line {
                raw.push('\n');
            }
            raw.push_str(lines[k].trim_end());
            wrote_line = true;
            k += 1;
        }

        let text = clean_text(&raw);
        if !text.is_empty() {
            cues.push(Cue {
                start,
                end,
                text,
                style: CueStyle::default(),
            });
        }
        i = k.max(timing_idx + 1);
    }

    Parsed { title: None, cues }
}

/// Returns `true` for a SubRip cue index line (digits only).
fn is_index_line(line: &str) -> bool {
    !line.is_empty() && line.chars().all(|c| c.is_ascii_digit())
}

/// Parse a timing line into a `(start, end)` pair of seconds, ignoring SRT's
/// legacy `X1:.. Y1:..` coordinate suffix if one is present.
fn parse_timing_line(line: &str) -> Option<(f64, f64)> {
    let (left, right) = line.split_once("-->")?;
    // The left side is usually a bare timestamp but may carry leading junk; the
    // right side may carry SRT's old `X1:.. Y1:..` position fields.
    let start_token = left.split_whitespace().last()?;
    let end_token = right.split_whitespace().next()?;
    let start = parse_timecode(start_token)?;
    let end = parse_timecode(end_token)?;
    Some((start, end))
}

/// Strip SRT markup from a cue's already-assembled text.
///
/// Inline tags (`<i>`, `<font ...>`) and override blocks (`{\an8}`) are removed
/// but deliberately do **not** set [`CueStyle`] flags; only ASS carries styling
/// that this crate models. `\N`/`\n` become real newlines.
fn clean_text(raw: &str) -> String {
    let without_blocks = strip_brace_blocks(raw);
    let without_tags = strip_angle_tags(&without_blocks);
    let with_newlines = convert_newline_escapes(&without_tags);
    finish_text(&with_newlines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_timing_variants() {
        assert!(is_index_line("12"));
        assert!(!is_index_line(""));
        assert!(!is_index_line("1a"));
        assert_eq!(
            parse_timing_line("00:00:01,000 --> 00:00:02,000"),
            Some((1.0, 2.0))
        );
        assert_eq!(
            parse_timing_line("00:00:01,000 --> 00:00:02,000 X1:1 X2:2"),
            Some((1.0, 2.0))
        );
        assert!(parse_timing_line("00:00:01,000 --> ").is_none());
        assert!(parse_timing_line("nonsense").is_none());
    }

    #[test]
    fn clean_text_strips_markup() {
        assert_eq!(clean_text("<i>a</i>{\\an8}b\\Nc"), "ab\nc");
        assert_eq!(clean_text("{\\pos(320,240)}still here"), "still here");
    }

    #[test]
    fn blank_lines_inside_a_cue_are_kept() {
        let srt = "1\n00:00:01,000 --> 00:00:05,000\nfirst\n\nsecond\n\n2\n00:00:06,000 --> 00:00:07,000\nnext\n";
        let parsed = parse(srt);
        assert_eq!(parsed.cues.len(), 2);
        assert_eq!(parsed.cues[0].text, "first\n\nsecond");
        assert_eq!(parsed.cues[1].text, "next");
    }

    #[test]
    fn missing_index_and_crlf_are_tolerated() {
        let srt = "00:00:01,000 --> 00:00:02,000\r\nno index\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nsecond";
        let parsed = parse(srt);
        assert_eq!(parsed.cues.len(), 2);
        assert_eq!(parsed.cues[0].text, "no index");
        assert_eq!(parsed.cues[1].start, 3.0);
    }

    #[test]
    fn malformed_block_is_skipped_but_the_rest_survives() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\ngood\n\n2\nnot a timing line\n\n3\n00:00:03,000 --> 00:00:04,000\nalso good\n";
        let parsed = parse(srt);
        assert_eq!(parsed.cues.len(), 2);
        assert_eq!(parsed.cues[0].text, "good");
        assert_eq!(parsed.cues[1].text, "also good");
    }

    #[test]
    fn cues_without_text_are_dropped() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\n\n\n2\n00:00:03,000 --> 00:00:04,000\n\n";
        let parsed = parse(srt);
        assert!(parsed.cues.is_empty());
    }
}
