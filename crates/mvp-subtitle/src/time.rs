//! Timecode parsing shared by the SubRip, ASS and WebVTT parsers.
//!
//! Every format writes timestamps slightly differently (`00:01:02,500`,
//! `0:01:02.50`, `01:02.500`), and every format is also found in the wild with
//! at least one of those quirks missing: no hours field, three-digit fractions,
//! a comma where a dot belongs. One tolerant parser covers all of them.

/// Parse a subtitle timestamp into seconds.
///
/// Accepts `HH:MM:SS[.,]fff`, `MM:SS[.,]fff` and a bare `SS[.,]fff`, with any
/// number of digits in each field (so both a missing hours field and a
/// four-digit hour work). The final field carries the fraction, so `01.50`
/// means one and a half seconds and `01.050` means one and five hundredths.
///
/// Returns `None` when `raw` is not a timestamp at all, which lets callers skip
/// a malformed cue instead of abandoning the file.
pub(crate) fn parse_timecode(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    // A comma is the SubRip decimal separator; normalising it keeps the rest of
    // the parser format-agnostic.
    let normalized = trimmed.replace(',', ".");
    let parts: Vec<&str> = normalized.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }

    let field_count = parts.len();
    let mut total = 0.0f64;
    for (i, part) in parts.iter().enumerate() {
        let value: f64 = part.trim().parse().ok()?;
        if !value.is_finite() {
            return None;
        }
        let remaining = field_count - 1 - i;
        if remaining == 0 {
            total += value;
        } else {
            total += value * 60f64.powi(remaining as i32);
        }
    }

    if total.is_finite() {
        Some(total)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn parses_every_shape() {
        assert!(approx(parse_timecode("00:00:01,000").unwrap_or(-1.0), 1.0));
        assert!(approx(
            parse_timecode("01:02:03.500").unwrap_or(-1.0),
            3723.5
        ));
        // Hours field missing.
        assert!(approx(parse_timecode("01:02.250").unwrap_or(-1.0), 62.25));
        // Hundredths (ASS centiseconds).
        assert!(approx(parse_timecode("0:00:01.50").unwrap_or(-1.0), 1.5));
        // More than two hour digits.
        assert!(approx(
            parse_timecode("123:00:00.000").unwrap_or(-1.0),
            442800.0
        ));
        // Bare seconds.
        assert!(approx(parse_timecode("12.5").unwrap_or(-1.0), 12.5));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_timecode("").is_none());
        assert!(parse_timecode("abc").is_none());
        assert!(parse_timecode("00:00:0X,000").is_none());
        assert!(parse_timecode("1:2:3:4").is_none());
    }
}
