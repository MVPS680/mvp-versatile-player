//! Acceptance tests for the `mvp-subtitle` public API.
//!
//! These exercise the crate exactly the way the player will: through
//! [`Subtitle`], with raw bytes and no access to internals.

use encoding_rs::{GBK, UTF_16LE};
use mvp_subtitle::{decode_text, detect_encoding, Align, EncodingHint, SubFormat, Subtitle};

/// 1. A well-formed SubRip file parses into three cues with exact times/text.
#[test]
fn well_formed_srt() {
    let srt = "1\n\
00:00:01,000 --> 00:00:02,500\n\
Hello there\n\
\n\
2\n\
00:01:03,250 --> 00:01:05,000\n\
Second cue\n\
\n\
3\n\
01:02:03,000 --> 01:02:04,750\n\
Third cue\n";

    let subtitle = Subtitle::parse(srt.as_bytes());
    assert_eq!(subtitle.format, SubFormat::Srt);
    assert_eq!(subtitle.encoding, "UTF-8");
    assert_eq!(subtitle.len(), 3);
    assert!(!subtitle.is_empty());

    assert_eq!(subtitle.cues[0].start, 1.0);
    assert_eq!(subtitle.cues[0].end, 2.5);
    assert_eq!(subtitle.cues[0].text, "Hello there");
    assert_eq!(subtitle.cues[0].duration(), 1.5);

    assert_eq!(subtitle.cues[1].start, 63.25);
    assert_eq!(subtitle.cues[1].end, 65.0);
    assert_eq!(subtitle.cues[1].text, "Second cue");

    assert_eq!(subtitle.cues[2].start, 3723.0);
    assert_eq!(subtitle.cues[2].end, 3724.75);
    assert_eq!(subtitle.cues[2].text, "Third cue");
}

/// 2. Dot millisecond separators and a missing hours field still parse.
#[test]
fn srt_with_dots_and_no_hours() {
    let srt = "1\n\
00:01.000 --> 00:02.500\n\
Dotted\n\
\n\
2\n\
1:02.250 --> 1:03.000\n\
Minutes too\n";

    let subtitle = Subtitle::parse(srt.as_bytes());
    assert_eq!(subtitle.len(), 2);
    assert_eq!(subtitle.cues[0].start, 1.0);
    assert_eq!(subtitle.cues[0].end, 2.5);
    assert_eq!(subtitle.cues[1].start, 62.25);
    assert_eq!(subtitle.cues[1].end, 63.0);
    assert_eq!(subtitle.cues[1].text, "Minutes too");
}

/// 3. Inline tags, `{\an8}` blocks and `\N` collapse to clean plain text.
#[test]
fn srt_markup_becomes_plain_text() {
    let srt = "1\n\
00:00:01,000 --> 00:00:03,000\n\
<i>Hello</i>{\\an8} world\\Nsecond line\n";

    let subtitle = Subtitle::parse(srt.as_bytes());
    assert_eq!(subtitle.len(), 1);
    assert_eq!(subtitle.cues[0].text, "Hello world\nsecond line");
    assert!(subtitle.cues[0].text.contains('\n'));
    assert!(!subtitle.cues[0].text.contains('<'));
    assert!(!subtitle.cues[0].text.contains('{'));
}

/// 4. A GBK-encoded Chinese SubRip file round-trips (no binary fixtures).
#[test]
fn gbk_chinese_srt_round_trips() {
    let srt = "1\n\
00:00:01,000 --> 00:00:03,000\n\
你好，世界！\n\
\n\
2\n\
00:00:04,000 --> 00:00:06,000\n\
再见，朋友\n";

    let (bytes, _, had_errors) = GBK.encode(srt);
    assert!(!had_errors, "the fixture must be representable in GBK");

    assert_eq!(detect_encoding(&bytes), GBK);

    let (decoded, name) = decode_text(&bytes, EncodingHint::Auto);
    assert_eq!(name, "GBK");
    assert_eq!(decoded, srt);

    let subtitle = Subtitle::parse(&bytes);
    assert_eq!(subtitle.encoding, "GBK");
    assert_eq!(subtitle.format, SubFormat::Srt);
    assert_eq!(subtitle.len(), 2);
    assert_eq!(subtitle.cues[0].text, "你好，世界！");
    assert_eq!(subtitle.cues[1].text, "再见，朋友");
}

/// 5. UTF-8 and UTF-16LE byte-order marks decode and are not left in the text.
#[test]
fn bom_encodings() {
    let srt = "1\n00:00:01,000 --> 00:00:02,000\nHello\n";

    let mut utf8_bom = vec![0xEF, 0xBB, 0xBF];
    utf8_bom.extend_from_slice(srt.as_bytes());
    let (text, name) = decode_text(&utf8_bom, EncodingHint::Auto);
    assert_eq!(name, "UTF-8");
    assert_eq!(text, srt);
    let from_utf8_bom = Subtitle::parse(&utf8_bom);
    assert_eq!(from_utf8_bom.len(), 1);
    assert_eq!(from_utf8_bom.cues[0].text, "Hello");

    // UTF-16LE is built explicitly so the fixture is exactly one BOM plus
    // little-endian code units.
    let mut utf16: Vec<u8> = vec![0xFF, 0xFE];
    for unit in srt.encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    assert_eq!(detect_encoding(&utf16), UTF_16LE);
    let (text, name) = decode_text(&utf16, EncodingHint::Auto);
    assert_eq!(name, "UTF-16LE");
    assert_eq!(text, srt);
    let from_utf16 = Subtitle::parse(&utf16);
    assert_eq!(from_utf16.encoding, "UTF-16LE");
    assert_eq!(from_utf16.len(), 1);
    assert_eq!(from_utf16.cues[0].text, "Hello");
}

/// 6. An ASS file with a reordered `Format:` line parses timings correctly, and
///    `\an8`, `\b1` and `\c&H0000FF&` (pure red) are honoured.
#[test]
fn ass_reordered_format_and_overrides() {
    let ass = "[Script Info]\n\
Title: Demo\n\
ScriptType: v4.00+\n\
\n\
[V4+ Styles]\n\
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n\
\n\
[Events]\n\
Format: Start, End, Style, Text\n\
Dialogue: 0:00:01.00,0:00:02.50,Default,{\\an8\\b1\\c&H0000FF&}Hello, World\\NSecond line\n\
Comment: 0:00:03.00,0:00:04.00,Default,This is ignored\n\
Dialogue: 0:00:05.00,0:00:06.00,Default,Plain\n";

    let subtitle = Subtitle::parse(ass.as_bytes());
    assert_eq!(subtitle.format, SubFormat::Ass);
    assert_eq!(subtitle.title.as_deref(), Some("Demo"));
    assert_eq!(subtitle.len(), 2, "Comment: lines must be ignored");

    let first = &subtitle.cues[0];
    assert_eq!(first.start, 1.0);
    assert_eq!(first.end, 2.5);
    // The comma inside the text field must survive the reordered columns.
    assert_eq!(first.text, "Hello, World\nSecond line");
    assert_eq!(first.style.align, Align::TopCenter);
    assert!(first.style.bold);
    assert!(!first.style.italic);
    assert_eq!(first.style.color, Some([255, 0, 0]));
    assert_eq!(first.style.margin_v, Some(10));

    let second = &subtitle.cues[1];
    assert_eq!(second.start, 5.0);
    assert_eq!(second.text, "Plain");
    assert_eq!(second.style.color, Some([255, 255, 255]));
    assert_eq!(second.style.align, Align::BottomCenter);
}

/// 7. WebVTT with a `NOTE` block, a cue identifier and `align:start`.
#[test]
fn vtt_with_note_identifier_and_settings() {
    let vtt = "WEBVTT\n\
\n\
NOTE This note block\n\
spans several lines\n\
\n\
STYLE\n\
::cue { color: red }\n\
\n\
intro\n\
00:00:01.000 --> 00:00:04.000 align:start position:10%\n\
<i>Hello</i> <b>world</b>\n\
<v Bob>Voice line</v>\n\
\n\
00:00:05.000 --> 00:00:06.500\n\
No settings\n";

    let subtitle = Subtitle::parse(vtt.as_bytes());
    assert_eq!(subtitle.format, SubFormat::WebVtt);
    assert_eq!(subtitle.len(), 2);

    let first = &subtitle.cues[0];
    assert_eq!(first.start, 1.0);
    assert_eq!(first.end, 4.0);
    assert_eq!(first.text, "Hello world\nVoice line");
    assert_eq!(first.style.align, Align::BottomLeft);

    let second = &subtitle.cues[1];
    assert_eq!(second.start, 5.0);
    assert_eq!(second.end, 6.5);
    assert_eq!(second.text, "No settings");
    assert_eq!(second.style.align, Align::BottomCenter);
}

/// 8. `contains`/`active_at` use the half-open range `[start, end)`.
#[test]
fn active_at_is_half_open() {
    let srt = "1\n00:00:01,000 --> 00:00:02,000\nA\n\n2\n00:00:02,000 --> 00:00:03,000\nB\n";
    let subtitle = Subtitle::parse(srt.as_bytes());

    // Exactly at the start of the first cue.
    assert_eq!(
        subtitle.active_at(1.0).map(|cue| cue.text.as_str()),
        Some("A")
    );
    // Exactly at its end: a miss, and the next cue has not started either
    // because it starts at the same instant.
    assert_eq!(
        subtitle.active_at(2.0).map(|cue| cue.text.as_str()),
        Some("B")
    );
    assert!(!subtitle.cues[0].contains(2.0));
    assert!(subtitle.cues[1].contains(2.0));

    // The end of the last cue is a miss.
    assert!(subtitle.active_at(3.0).is_none());
    assert!(!subtitle.cues[1].contains(3.0));
    assert!(subtitle.active_at(1.999).is_some());
    assert!(subtitle.active_at(0.5).is_none());
}

/// 9. `shift(+1.0)` moves cues, keeps them sorted, and `active_at` follows.
#[test]
fn shift_applies_a_delay() {
    let srt = "1\n00:00:01,000 --> 00:00:02,000\nA\n\n2\n00:00:04,000 --> 00:00:05,000\nB\n";
    let mut subtitle = Subtitle::parse(srt.as_bytes());

    subtitle.shift(1.0);
    assert_eq!(subtitle.cues[0].start, 2.0);
    assert_eq!(subtitle.cues[0].end, 3.0);
    assert_eq!(subtitle.cues[1].start, 5.0);
    assert!(subtitle
        .cues
        .windows(2)
        .all(|pair| pair[0].start <= pair[1].start));

    assert!(subtitle.active_at(1.5).is_none());
    assert_eq!(
        subtitle.active_at(2.5).map(|cue| cue.text.as_str()),
        Some("A")
    );
    assert_eq!(
        subtitle.active_at(5.5).map(|cue| cue.text.as_str()),
        Some("B")
    );

    // A large negative shift must still leave the list sorted.
    subtitle.shift(-100.0);
    assert!(
        subtitle
            .cues
            .windows(2)
            .all(|pair| pair[0].start <= pair[1].start),
        "shift must keep the cue list sorted"
    );
    assert_eq!(subtitle.len(), 2);
}

/// 10. Malformed input never panics and always yields a usable `Subtitle`.
#[test]
fn malformed_input_is_survivable() {
    let cases: [&[u8]; 8] = [
        b"",
        b"\x00\x01\x02\xff\xfe\x13\x37",
        b"1\n00:00:01,000 --> \nhello\n",
        b"00:00:0X,000 --> 00:00:02,000\nhello\n",
        b"1\n00:00:01,000 --> 00:00:02,000\n",
        b"-->-->-->",
        b"{\xff\xfe}[Script Info]\nTitle: broken\n",
        b"\xff\xfe\x00\x00garbage\x13",
    ];

    for bytes in cases {
        let subtitle = Subtitle::parse(bytes);
        // The whole public surface must stay usable.
        assert_eq!(subtitle.len(), subtitle.cues.len());
        let _ = subtitle.active_at(0.5);
        let _ = subtitle.active_all(1.0);
        let _ = subtitle.index_at(2.0);
        assert!(subtitle.len() < 64);
    }

    // A truncated timing line costs exactly one cue, not the whole file.
    let truncated = "1\n00:00:01,000 --> \nbroken\n\n2\n00:00:03,000 --> 00:00:04,000\nfine\n";
    let subtitle = Subtitle::parse(truncated.as_bytes());
    assert_eq!(subtitle.len(), 1);
    assert_eq!(subtitle.cues[0].text, "fine");
    assert_eq!(subtitle.cues[0].start, 3.0);

    assert!(Subtitle::parse(b"").is_empty());
    assert!(Subtitle::parse(b"").active_at(0.0).is_none());
}

/// 11. `merge_adjacent` joins identical neighbours across a small gap only.
#[test]
fn merge_adjacent_joins_split_cues() {
    let srt = "1\n\
00:00:01,000 --> 00:00:02,000\n\
Same text\n\
\n\
2\n\
00:00:02,200 --> 00:00:03,000\n\
Same text\n\
\n\
3\n\
00:00:03,200 --> 00:00:04,000\n\
Different text\n";

    let mut subtitle = Subtitle::parse(srt.as_bytes());
    assert_eq!(subtitle.len(), 3);

    let merged = subtitle.merge_adjacent(0.2);
    assert_eq!(merged, 1, "only the identical pair may merge");
    assert_eq!(subtitle.len(), 2);
    assert_eq!(subtitle.cues[0].start, 1.0);
    assert_eq!(subtitle.cues[0].end, 3.0, "the merged cue spans both");
    assert_eq!(subtitle.cues[0].text, "Same text");
    assert_eq!(subtitle.cues[1].text, "Different text");

    // A gap that is too large must not merge.
    let mut far_apart = Subtitle::parse(
        b"1\n00:00:01,000 --> 00:00:02,000\nSame text\n\n2\n00:00:09,000 --> 00:00:10,000\nSame text\n",
    );
    assert_eq!(far_apart.merge_adjacent(0.2), 0);
    assert_eq!(far_apart.len(), 2);
}
