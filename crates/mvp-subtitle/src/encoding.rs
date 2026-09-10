//! Character-encoding detection and lossy decoding for subtitle files.
//!
//! Subtitle files in the wild are frequently *not* UTF-8: Chinese `.srt` files
//! are usually GBK, older European ones are Windows-1252 and some tools still
//! emit UTF-16 with a BOM. Nothing here ever fails — the worst case is a few
//! replacement characters — because a subtitle with two broken glyphs is much
//! better than no subtitle at all.

use encoding_rs::{Encoding, BIG5, GBK, SHIFT_JIS, UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252};

/// A caller-supplied hint about how bytes should be decoded.
///
/// [`EncodingHint::Auto`] runs the full detection chain; every other variant
/// forces a specific decoder. A byte-order mark always wins over the hint,
/// because a BOM is authoritative and the hint is only a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncodingHint {
    /// Detect the encoding from a BOM and content heuristics.
    #[default]
    Auto,
    /// UTF-8 (with or without a BOM).
    Utf8,
    /// UTF-16, little endian.
    Utf16Le,
    /// UTF-16, big endian.
    Utf16Be,
    /// Simplified Chinese GBK / GB18030.
    Gbk,
    /// Traditional Chinese Big5.
    Big5,
    /// Japanese Shift_JIS.
    ShiftJis,
    /// Western European Windows-1252 (a superset of ISO-8859-1).
    Latin1,
}

/// The UTF-8 byte-order mark.
const BOM_UTF8: [u8; 3] = [0xEF, 0xBB, 0xBF];
/// The UTF-16 little-endian byte-order mark.
const BOM_UTF16_LE: [u8; 2] = [0xFF, 0xFE];
/// The UTF-16 big-endian byte-order mark.
const BOM_UTF16_BE: [u8; 2] = [0xFE, 0xFF];

/// Map an explicit hint onto a concrete encoding.
///
/// Returns `None` for [`EncodingHint::Auto`], which has to inspect the bytes.
fn encoding_for(hint: EncodingHint) -> Option<&'static Encoding> {
    match hint {
        EncodingHint::Auto => None,
        EncodingHint::Utf8 => Some(UTF_8),
        EncodingHint::Utf16Le => Some(UTF_16LE),
        EncodingHint::Utf16Be => Some(UTF_16BE),
        EncodingHint::Gbk => Some(GBK),
        EncodingHint::Big5 => Some(BIG5),
        EncodingHint::ShiftJis => Some(SHIFT_JIS),
        EncodingHint::Latin1 => Some(WINDOWS_1252),
    }
}

/// The encoding announced by a byte-order mark at the start of `bytes`.
fn bom_encoding(bytes: &[u8]) -> Option<&'static Encoding> {
    if bytes.starts_with(&BOM_UTF8) {
        Some(UTF_8)
    } else if bytes.starts_with(&BOM_UTF16_LE) {
        Some(UTF_16LE)
    } else if bytes.starts_with(&BOM_UTF16_BE) {
        Some(UTF_16BE)
    } else {
        None
    }
}

/// Human-readable name for an encoding, matching the names the media-info panel
/// shows (`encoding_rs` itself returns lower-case names such as
/// `"windows-1252"`).
fn encoding_name(encoding: &'static Encoding) -> &'static str {
    if encoding == UTF_8 {
        "UTF-8"
    } else if encoding == UTF_16LE {
        "UTF-16LE"
    } else if encoding == UTF_16BE {
        "UTF-16BE"
    } else if encoding == GBK {
        "GBK"
    } else if encoding == BIG5 {
        "Big5"
    } else if encoding == SHIFT_JIS {
        "Shift_JIS"
    } else if encoding == WINDOWS_1252 {
        "Windows-1252"
    } else {
        encoding.name()
    }
}

/// Returns `true` when `text` contains a CJK ideograph in the common ranges
/// (CJK Unified Ideographs and Extension A), which is what distinguishes a
/// genuine Chinese/Japanese payload from ASCII or accented Latin text.
fn has_cjk_ideograph(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}'))
}

/// Returns `true` for Japanese kana, used only to recognise Shift_JIS files that
/// happen to contain no ideographs at all.
fn has_kana(text: &str) -> bool {
    text.chars().any(|c| matches!(c, '\u{3040}'..='\u{30FF}'))
}

/// Detect the encoding of `bytes`.
///
/// The rules, in priority order:
///
/// 1. A byte-order mark (UTF-8, UTF-16LE or UTF-16BE).
/// 2. The whole buffer is valid UTF-8.
/// 3. The buffer decodes cleanly as GBK *and* contains CJK ideographs — by far
///    the most common shape of a Chinese `.srt` found in the wild. Big5 and then
///    Shift_JIS are tried only when GBK produced replacement characters and the
///    alternative did not.
/// 4. Otherwise Windows-1252, which maps every remaining byte to something.
///
/// Note that Big5 text is frequently *also* valid GBK, so it is reported as GBK;
/// see the crate documentation for that limitation.
pub fn detect_encoding(bytes: &[u8]) -> &'static Encoding {
    if let Some(encoding) = bom_encoding(bytes) {
        return encoding;
    }
    if std::str::from_utf8(bytes).is_ok() {
        return UTF_8;
    }

    let (gbk_text, gbk_had_errors) = GBK.decode_without_bom_handling(bytes);
    if !gbk_had_errors && has_cjk_ideograph(&gbk_text) {
        return GBK;
    }

    let (big5_text, big5_had_errors) = BIG5.decode_without_bom_handling(bytes);
    if !big5_had_errors && has_cjk_ideograph(&big5_text) {
        return BIG5;
    }

    let (sjis_text, sjis_had_errors) = SHIFT_JIS.decode_without_bom_handling(bytes);
    if !sjis_had_errors && (has_cjk_ideograph(&sjis_text) || has_kana(&sjis_text)) {
        return SHIFT_JIS;
    }

    WINDOWS_1252
}

/// Decode `bytes` to a `String`, honouring a BOM and never failing.
///
/// Returns the decoded text and the human-readable name of the encoding that was
/// actually used (for example `"UTF-8"`, `"GBK"`, `"UTF-16LE"`,
/// `"Windows-1252"`).
///
/// Decoding is deliberately lossy: over-long and invalid sequences become
/// `U+FFFD` instead of returning an error, so a file that is broken in one place
/// still renders everywhere else.
pub fn decode_text(bytes: &[u8], hint: EncodingHint) -> (String, &'static str) {
    let encoding = bom_encoding(bytes)
        .or_else(|| encoding_for(hint))
        .unwrap_or_else(|| detect_encoding(bytes));

    // `decode_without_bom_handling` keeps a BOM as U+FEFF, which would end up in
    // the first cue's text, so strip it here.
    let body = if encoding == UTF_8 {
        bytes.strip_prefix(&BOM_UTF8[..]).unwrap_or(bytes)
    } else if encoding == UTF_16LE {
        bytes.strip_prefix(&BOM_UTF16_LE[..]).unwrap_or(bytes)
    } else if encoding == UTF_16BE {
        bytes.strip_prefix(&BOM_UTF16_BE[..]).unwrap_or(bytes)
    } else {
        bytes
    };

    let (text, _had_errors) = encoding.decode_without_bom_handling(body);
    (text.into_owned(), encoding_name(encoding))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_utf8() {
        assert_eq!(
            detect_encoding(b"1\n00:00:01,000 --> 00:00:02,000\nhi\n"),
            UTF_8
        );
        assert_eq!(decode_text(b"plain", EncodingHint::Auto).1, "UTF-8");
    }

    #[test]
    fn empty_input_is_utf8() {
        let (text, name) = decode_text(b"", EncodingHint::Auto);
        assert!(text.is_empty());
        assert_eq!(name, "UTF-8");
    }

    #[test]
    fn boms_win_over_hint() {
        let mut utf8 = BOM_UTF8.to_vec();
        utf8.extend_from_slice(b"hello");
        let (text, name) = decode_text(&utf8, EncodingHint::Gbk);
        assert_eq!(text, "hello");
        assert_eq!(name, "UTF-8");
    }

    #[test]
    fn latin1_falls_back_to_windows_1252() {
        // 0xE9 alone cannot be GBK, Big5 or Shift_JIS.
        let bytes = b"caf\xE9 au lait";
        assert_eq!(detect_encoding(bytes), WINDOWS_1252);
        let (text, name) = decode_text(bytes, EncodingHint::Auto);
        assert_eq!(text, "café au lait");
        assert_eq!(name, "Windows-1252");
    }

    #[test]
    fn hint_forces_decoding_without_a_bom() {
        let (bytes, _, _) = GBK.encode("你好");
        let (text, name) = decode_text(&bytes, EncodingHint::Gbk);
        assert_eq!(text, "你好");
        assert_eq!(name, "GBK");
    }
}
