//! Text-level cleanup shared by the SubRip and WebVTT parsers.
//!
//! All of it is intentionally forgiving: a stray `<` or an unterminated `{` in a
//! user's file must survive as literal text rather than swallow the cue that
//! follows it.

/// Returns `true` when the inside of a `<...>` pair looks like a real markup tag
/// rather than a literal comparison such as `a < b > c`.
///
/// Accepted: an ASCII letter (`<i>`, `<font color=#fff>`), a slash followed by a
/// letter (`</i>`) or a WebVTT timestamp (`<00:00:01.000>`). A tag-like string
/// is never trimmed first, so `< b >` stays literal text.
fn is_tag_like(inner: &str) -> bool {
    let mut chars = inner.chars();
    match chars.next() {
        Some('/') => chars.next().is_some_and(|c| c.is_ascii_alphabetic()),
        Some(c) if c.is_ascii_alphabetic() => true,
        Some(c) if c.is_ascii_digit() => {
            // WebVTT timestamp tag: digits, ':' and a '.'/',' separated fraction.
            let mut digits = 0usize;
            for c in inner.chars() {
                if c.is_ascii_digit() {
                    digits += 1;
                } else if c != ':' && c != '.' && c != ',' {
                    return false;
                }
            }
            digits >= 6
        }
        _ => false,
    }
}

/// Remove `<i>`, `</b>`, `<font ...>`, `<v Name>`, `<00:00:01.000>` and friends.
///
/// Unpaired or non-tag-like angle brackets are kept verbatim.
pub(crate) fn strip_angle_tags(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '<' {
            // Walk to the closing '>' but give up on a nested '<' or a newline,
            // so an unterminated tag cannot eat the rest of the cue.
            let mut j = i + 1;
            let mut closed = false;
            while j < chars.len() && j - i <= 128 {
                if chars[j] == '>' {
                    closed = true;
                    break;
                }
                if chars[j] == '<' || chars[j] == '\n' {
                    break;
                }
                j += 1;
            }
            if closed {
                let inner: String = chars[i + 1..j].iter().collect();
                if is_tag_like(&inner) {
                    i = j + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Remove `{...}` override blocks (`{\an8}`, `{\pos(320,240)}`).
///
/// Blocks are dropped whole; an unterminated `{` or a nested `{` is left alone.
pub(crate) fn strip_brace_blocks(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '{' {
            let mut j = i + 1;
            let mut closed = false;
            while j < chars.len() && j - i <= 256 {
                if chars[j] == '}' {
                    closed = true;
                    break;
                }
                if chars[j] == '{' || chars[j] == '\n' {
                    break;
                }
                j += 1;
            }
            if closed {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Turn SubRip backslash escapes (`\N` and `\n`) into real newlines.
///
/// Any other backslash is preserved, so a path such as `C:\temp` survives intact
/// — but note that a literal `\n` inside a path is, by the format's own rules,
/// indistinguishable from a line break.
pub(crate) fn convert_newline_escapes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('N') | Some('n') => {
                    chars.next();
                    out.push('\n');
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    out
}

/// Resolve the handful of HTML entities WebVTT payloads actually use.
///
/// `&amp;` is expanded last so that `&amp;lt;` decodes to the literal `&lt;`
/// rather than to `<`.
pub(crate) fn decode_entities(input: &str) -> String {
    if !input.contains('&') {
        return input.to_string();
    }
    input
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", "\u{a0}")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Normalise a cue's raw text: trim every line, then drop leading and trailing
/// blank lines while keeping intentional blank lines in the middle.
pub(crate) fn finish_text(raw: &str) -> String {
    let mut lines: Vec<&str> = raw.split('\n').map(str::trim).collect();
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_real_tags_but_keeps_comparisons() {
        assert_eq!(strip_angle_tags("<i>hi</i>"), "hi");
        assert_eq!(
            strip_angle_tags("<font color=\"#ff0000\">red</font>"),
            "red"
        );
        assert_eq!(strip_angle_tags("<00:00:01.000>now"), "now");
        assert_eq!(strip_angle_tags("a < b > c"), "a < b > c");
        assert_eq!(strip_angle_tags("3 < 4"), "3 < 4");
        assert_eq!(strip_angle_tags("unterminated <i"), "unterminated <i");
    }

    #[test]
    fn strips_override_blocks() {
        assert_eq!(strip_brace_blocks("{\\an8}top"), "top");
        assert_eq!(strip_brace_blocks("a { b } c"), "a  c");
        assert_eq!(strip_brace_blocks("open { forever"), "open { forever");
    }

    #[test]
    fn converts_only_newline_escapes() {
        assert_eq!(
            convert_newline_escapes(r"one\Ntwo\nthree"),
            "one\ntwo\nthree"
        );
        // A backslash that is not `\N`/`\n` stays literal.
        assert_eq!(convert_newline_escapes(r"C:\temp\file"), r"C:\temp\file");
        assert_eq!(convert_newline_escapes(r"trailing\"), r"trailing\");
    }

    #[test]
    fn entities_are_decoded_once() {
        assert_eq!(decode_entities("a &amp; b &lt;c&gt;"), "a & b <c>");
        assert_eq!(decode_entities("&amp;lt;"), "&lt;");
    }

    #[test]
    fn finish_text_trims_edges_only() {
        assert_eq!(finish_text("  a  \n\n  b \n\n"), "a\n\nb");
    }
}
