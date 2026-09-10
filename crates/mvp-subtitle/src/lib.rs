//! Subtitle parsing and cue lookup for MVP-Versatile-Player.
//!
//! This crate is pure logic: it has no FFmpeg or Windows dependency, so it can
//! be tested on any host and reused by the player's media-info panel.
//!
//! ```
//! use mvp_subtitle::{SubFormat, Subtitle};
//!
//! let srt = "1\n00:00:01,000 --> 00:00:02,500\nHello <i>there</i>\n";
//! let subtitle = Subtitle::parse(srt.as_bytes());
//!
//! assert_eq!(subtitle.format, SubFormat::Srt);
//! assert_eq!(subtitle.len(), 1);
//! assert_eq!(subtitle.active_at(1.5).map(|cue| cue.text.as_str()), Some("Hello there"));
//! assert!(subtitle.active_at(2.5).is_none()); // the range is half-open
//! ```
//!
//! # Design
//!
//! * **Never fail.** [`Subtitle::parse`] takes raw bytes and always returns a
//!   usable [`Subtitle`]; malformed blocks are skipped so that one broken
//!   timing line cannot hide the rest of the file.
//! * **Never panic on user input.** No `unwrap`/`panic!` on the parsing paths;
//!   invalid UTF-8, absurd timings and truncated files all degrade gracefully.
//! * **One model.** Every format is normalised into [`Cue`]s holding plain,
//!   markup-free text plus a small [`CueStyle`].
//!
//! # Supported formats
//!
//! [`SubFormat::Srt`], [`SubFormat::Ass`] (`.ass` and `.ssa`),
//! [`SubFormat::WebVtt`] and [`SubFormat::MicroDvd`] (`.sub`).
//!
//! # Known limitations
//!
//! * Big5 and Shift_JIS files are reported as GBK whenever their bytes also
//!   decode cleanly as GBK, which is common. See [`detect_encoding`].
//! * SubRip and WebVTT inline tags (`<i>`, `<b>`, `<u>`) are stripped but do not
//!   set [`CueStyle`] flags; only ASS/SSA overrides populate them.
//! * An ASS `\r` reset restores the dialogue line's own style; a named reset
//!   target (`\rOtherStyle`) is approximated by that same restore.
//! * The ASS alignment value `9` is read as the numeric-keypad top-right, and
//!   `10`/`11` as the legacy SSA middle-centre/middle-right.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ass;
pub mod encoding;
pub mod microdvd;
pub mod model;
pub mod srt;
pub mod vtt;

mod text;
mod time;

pub use encoding::{decode_text, detect_encoding, EncodingHint};
pub use microdvd::parse_microdvd;
pub use model::{Align, Cue, CueStyle, Parsed, SubFormat, Subtitle};

#[cfg(test)]
mod tests {
    use super::*;

    /// A small but complete SubRip document.
    const SRT: &str = "1\n00:00:01,000 --> 00:00:02,500\nFirst line\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond\n\n3\n00:00:05,000 --> 00:00:06,000\nThird\n";

    #[test]
    fn empty_subtitle_is_usable() {
        let subtitle = Subtitle::empty();
        assert!(subtitle.is_empty());
        assert_eq!(subtitle.len(), 0);
        assert_eq!(subtitle.format, SubFormat::Unknown);
        assert!(subtitle.active_at(0.0).is_none());
        assert_eq!(subtitle.encoding, "UTF-8");
    }

    #[test]
    fn extensions_map_to_formats() {
        assert_eq!(Subtitle::format_from_extension("srt"), SubFormat::Srt);
        assert_eq!(Subtitle::format_from_extension(".SRT"), SubFormat::Srt);
        assert_eq!(Subtitle::format_from_extension("ass"), SubFormat::Ass);
        assert_eq!(Subtitle::format_from_extension("ssa"), SubFormat::Ass);
        assert_eq!(Subtitle::format_from_extension("vtt"), SubFormat::WebVtt);
        assert_eq!(Subtitle::format_from_extension("sub"), SubFormat::MicroDvd);
        assert_eq!(Subtitle::format_from_extension("mp4"), SubFormat::Unknown);
    }

    #[test]
    fn format_is_detected_from_content() {
        assert_eq!(Subtitle::parse(SRT.as_bytes()).format, SubFormat::Srt);
        assert_eq!(
            Subtitle::parse(b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nhi\n").format,
            SubFormat::WebVtt
        );
        assert_eq!(
            Subtitle::parse(b"[Script Info]\nTitle: t\n\n[Events]\nDialogue: 0:00:01.00,0:00:02.00,Default,hi\n").format,
            SubFormat::Ass
        );
        assert_eq!(
            Subtitle::parse(b"{1}{1}{25}{50}hi\n").format,
            SubFormat::MicroDvd
        );
        assert_eq!(
            Subtitle::parse(b"just some text").format,
            SubFormat::Unknown
        );
    }

    #[test]
    fn forced_format_overrides_detection() {
        let ass_like = "[Script Info]\nTitle: t\n\n[Events]\nFormat: Start, End, Style, Text\nDialogue: 0:00:02.00,0:00:03.00,Default,hello\n";
        let subtitle = Subtitle::parse_with_format(ass_like.as_bytes(), SubFormat::Ass);
        assert_eq!(subtitle.format, SubFormat::Ass);
        assert_eq!(subtitle.title.as_deref(), Some("t"));
        assert_eq!(subtitle.len(), 1);
        assert_eq!(subtitle.cues[0].start, 2.0);

        // Forcing SubRip on ASS text yields nothing rather than an error.
        let wrong = Subtitle::parse_with_format(ass_like.as_bytes(), SubFormat::Srt);
        assert_eq!(wrong.format, SubFormat::Srt);
        assert!(wrong.is_empty());
    }

    #[test]
    fn duration_and_contains_are_half_open() {
        let cue = Cue {
            start: 1.0,
            end: 2.5,
            text: "x".to_string(),
            style: CueStyle::default(),
        };
        assert!(cue.contains(1.0));
        assert!(cue.contains(2.499));
        assert!(!cue.contains(2.5));
        assert!(!cue.contains(f64::NAN));
        assert_eq!(cue.duration(), 1.5);

        let backwards = Cue {
            start: 5.0,
            end: 1.0,
            ..cue.clone()
        };
        assert_eq!(backwards.duration(), 0.0);
    }

    #[test]
    fn index_at_finds_current_and_next() {
        let subtitle = Subtitle::parse(SRT.as_bytes());
        assert_eq!(subtitle.index_at(1.5), Some(0));
        assert_eq!(subtitle.index_at(3.5), Some(1));
        // Between cues: the next upcoming one.
        assert_eq!(subtitle.index_at(2.9), Some(1));
        // Past everything.
        assert_eq!(subtitle.index_at(100.0), None);
    }

    #[test]
    fn active_at_prefers_the_latest_start() {
        let subtitle = Subtitle::parse(
            b"1\n00:00:01,000 --> 00:00:05,000\nbackground\n\n2\n00:00:02,000 --> 00:00:03,000\nforeground\n",
        );
        assert_eq!(subtitle.active_all(2.5).len(), 2);
        assert_eq!(
            subtitle.active_at(2.5).map(|cue| cue.text.as_str()),
            Some("foreground")
        );
        assert_eq!(
            subtitle.active_at(4.0).map(|cue| cue.text.as_str()),
            Some("background")
        );
    }

    #[test]
    fn negative_shift_is_kept_but_sorted() {
        let mut subtitle = Subtitle::parse(SRT.as_bytes());
        subtitle.shift(-10.0);
        // Cues deliberately moved before the first frame stay negative and are
        // simply never active until their own range is reached.
        assert_eq!(subtitle.cues[0].start, -9.0);
        assert_eq!(
            subtitle.active_at(-8.0).map(|cue| cue.text.as_str()),
            Some("First line")
        );
        assert!(subtitle.active_at(-5.5).is_none());
        assert!(subtitle
            .cues
            .windows(2)
            .all(|pair| pair[0].start <= pair[1].start));
    }

    #[test]
    fn merge_is_a_no_op_for_different_text_and_far_apart_cues() {
        let mut subtitle = Subtitle::parse(SRT.as_bytes());
        assert_eq!(subtitle.merge_adjacent(0.2), 0);
        assert_eq!(subtitle.len(), 3);
        assert_eq!(subtitle.merge_adjacent(f64::NAN), 0);
        assert_eq!(subtitle.len(), 3);
    }
}
