//! The subtitle data model shared by every parser in this crate.
//!
//! The model deliberately carries no rendering state: a [`Cue`] is a time range
//! plus plain text and a small set of presentation flags, which is everything a
//! player needs in order to paint a subtitle over a video frame.

use crate::{ass, microdvd, srt, vtt};

/// Subtitle file formats we understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubFormat {
    /// SubRip (`.srt`): an optional index, `HH:MM:SS,mmm --> HH:MM:SS,mmm`, text.
    Srt,
    /// Advanced SubStation Alpha (`.ass`) and its ancestor Sub Station Alpha
    /// (`.ssa`), which share the `[Script Info]` / `[Events]` layout.
    Ass,
    /// WebVTT (`.vtt`), the format used by HTML5 text tracks.
    WebVtt,
    /// MicroDVD (`.sub`): `{startFrame}{endFrame}text`.
    MicroDvd,
    /// The format could not be determined, so no cues were parsed.
    #[default]
    Unknown,
}

/// Where a cue's text block sits inside the video frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    /// Anchored to the bottom edge, left of centre.
    BottomLeft,
    /// Anchored to the bottom edge, horizontally centred.
    #[default]
    BottomCenter,
    /// Anchored to the bottom edge, right of centre.
    BottomRight,
    /// Vertically centred, left of centre.
    MiddleLeft,
    /// Vertically and horizontally centred.
    MiddleCenter,
    /// Vertically centred, right of centre.
    MiddleRight,
    /// Anchored to the top edge, left of centre.
    TopLeft,
    /// Anchored to the top edge, horizontally centred.
    TopCenter,
    /// Anchored to the top edge, right of centre.
    TopRight,
}

/// Presentation attributes attached to a single [`Cue`].
///
/// Parsers fill in whatever the source format provides; everything else keeps
/// the neutral default (upright, unpainted, bottom-centred).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CueStyle {
    /// Text is bold.
    pub bold: bool,
    /// Text is italic.
    pub italic: bool,
    /// Text is underlined.
    pub underline: bool,
    /// Text is struck through.
    pub strikeout: bool,
    /// Text colour as RGB, when the source specifies one.
    pub color: Option<[u8; 3]>,
    /// Text block anchor inside the frame.
    pub align: Align,
    /// Vertical margin override in script pixels (ASS `\pos` y-coordinate or a
    /// non-zero ASS `MarginV`), when the source specifies one.
    pub margin_v: Option<i32>,
}

/// One subtitle cue: a half-open time range `[start, end)` plus its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    /// Start time in seconds from the beginning of the media.
    pub start: f64,
    /// End time in seconds from the beginning of the media.
    pub end: f64,
    /// Plain text with all markup removed; may contain `'\n'` line breaks.
    pub text: String,
    /// Presentation attributes for this cue.
    pub style: CueStyle,
}

impl Cue {
    /// Returns `true` when `t` falls inside the half-open range
    /// `[start, end)`.
    ///
    /// The range is half-open on purpose: back-to-back cues must not both be
    /// active on the exact boundary, and a `NaN` query is never contained.
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }

    /// Length of the cue in seconds, never negative.
    pub fn duration(&self) -> f64 {
        let d = self.end - self.start;
        if d.is_finite() && d > 0.0 {
            d
        } else {
            0.0
        }
    }
}

/// The outcome of parsing one subtitle document.
///
/// Returned by the per-format parsers ([`srt::parse`], [`ass::parse`],
/// [`vtt::parse`], [`microdvd::parse`]) and normally consumed through
/// [`Subtitle`], which adds format/encoding metadata and lookup helpers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Parsed {
    /// Document title, when the format carries one (ASS `Title`, WebVTT header
    /// comment). SubRip and MicroDVD files have no title.
    pub title: Option<String>,
    /// Cues in the order in which they appeared in the file.
    pub cues: Vec<Cue>,
}

/// A fully parsed subtitle track.
///
/// Parsing never fails: malformed input degrades to "fewer cues" rather than an
/// error, because a partially visible subtitle is far better than none.
#[derive(Debug, Clone, PartialEq)]
pub struct Subtitle {
    /// Format the bytes were parsed as.
    pub format: SubFormat,
    /// Document title, when the format carries one.
    pub title: Option<String>,
    /// Cues, always sorted by [`Cue::start`].
    pub cues: Vec<Cue>,
    /// Human-readable encoding name (for example `"UTF-8"`, `"GBK"`), suitable
    /// for the media-info panel.
    pub encoding: &'static str,
}

impl Subtitle {
    /// An empty, formatless track that is safe to hand to the rest of the app.
    pub fn empty() -> Self {
        Self {
            format: SubFormat::Unknown,
            title: None,
            cues: Vec::new(),
            encoding: "UTF-8",
        }
    }

    /// Detect the encoding and the format of `bytes`, then parse them.
    ///
    /// Never fails: undecodable or unrecognised input yields an empty track.
    pub fn parse(bytes: &[u8]) -> Self {
        let (text, encoding) =
            crate::encoding::decode_text(bytes, crate::encoding::EncodingHint::Auto);
        Self::from_text(&text, encoding, None)
    }

    /// Parse `bytes` as `format`, still auto-detecting the encoding.
    ///
    /// Passing [`SubFormat::Unknown`] behaves like [`Subtitle::parse`].
    pub fn parse_with_format(bytes: &[u8], format: SubFormat) -> Self {
        let (text, encoding) =
            crate::encoding::decode_text(bytes, crate::encoding::EncodingHint::Auto);
        Self::from_text(&text, encoding, Some(format))
    }

    /// Guess a format from a file extension, for example `"srt"` (a leading dot
    /// is accepted: `".SRT"` works too).
    pub fn format_from_extension(ext: &str) -> SubFormat {
        let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        match ext.as_str() {
            "srt" => SubFormat::Srt,
            "ass" | "ssa" => SubFormat::Ass,
            "vtt" | "webvtt" => SubFormat::WebVtt,
            "sub" | "microdvd" => SubFormat::MicroDvd,
            _ => SubFormat::Unknown,
        }
    }

    /// The best cue active at `t` (seconds).
    ///
    /// "Best" is the active cue with the greatest `start`, which is the one a
    /// viewer expects to win when cues overlap; ties go to the later cue in file
    /// order. Returns `None` when nothing is active.
    pub fn active_at(&self, t: f64) -> Option<&Cue> {
        let mut best: Option<usize> = None;
        for (i, cue) in self.cues.iter().enumerate() {
            if cue.contains(t) {
                match best {
                    Some(j) if self.cues[j].start > cue.start => {}
                    _ => best = Some(i),
                }
            }
        }
        best.map(|i| &self.cues[i])
    }

    /// Every cue active at `t`, in file order (karaoke and stacked subtitles).
    pub fn active_all(&self, t: f64) -> Vec<&Cue> {
        self.cues.iter().filter(|cue| cue.contains(t)).collect()
    }

    /// Index of the cue covering `t`.
    ///
    /// When no cue covers `t` the index of the next cue starting after `t` is
    /// returned instead, which makes this the anchor for "seek to the next /
    /// previous subtitle". Returns `None` only when `t` is past every cue.
    pub fn index_at(&self, t: f64) -> Option<usize> {
        if let Some(cue) = self.active_at(t) {
            return self.cues.iter().position(|c| std::ptr::eq(c, cue));
        }
        self.cues.iter().position(|c| c.start > t)
    }

    /// Returns `true` when the track holds no cues.
    pub fn is_empty(&self) -> bool {
        self.cues.is_empty()
    }

    /// Number of cues in the track.
    pub fn len(&self) -> usize {
        self.cues.len()
    }

    /// Shift every cue by `delta_secs` (positive delays the subtitles, negative
    /// advances them) and re-sort, since a shift can reorder overlapping cues.
    ///
    /// Times are not clamped at zero: a subtitle deliberately placed before the
    /// first video frame stays negative and simply never becomes active.
    pub fn shift(&mut self, delta_secs: f64) {
        if !delta_secs.is_finite() || delta_secs == 0.0 {
            return;
        }
        for cue in &mut self.cues {
            cue.start += delta_secs;
            cue.end += delta_secs;
        }
        sort_cues(&mut self.cues);
    }

    /// Merge cues with identical [`Cue::text`] whose gap is at most `gap`
    /// seconds, returning how many cues were absorbed.
    ///
    /// This cleans up the extremely common "one sentence split into two SRT
    /// cues" case. Overlapping cues count as a zero (or negative) gap. A
    /// negative or non-finite `gap` is treated as `0.0`.
    pub fn merge_adjacent(&mut self, gap: f64) -> usize {
        let gap = if gap.is_finite() && gap > 0.0 {
            gap
        } else {
            0.0
        };
        let mut merged = 0usize;
        let mut out: Vec<Cue> = Vec::with_capacity(self.cues.len());
        for cue in self.cues.drain(..) {
            if let Some(last) = out.last_mut() {
                let contiguous = cue.start <= last.end + gap;
                if contiguous && last.text == cue.text {
                    if cue.end > last.end {
                        last.end = cue.end;
                    }
                    merged += 1;
                    continue;
                }
            }
            out.push(cue);
        }
        self.cues = out;
        merged
    }

    /// Shared tail of every constructor: pick a format, dispatch to its parser
    /// and normalise the result.
    fn from_text(text: &str, encoding: &'static str, forced: Option<SubFormat>) -> Self {
        let format = match forced {
            Some(f) if f != SubFormat::Unknown => f,
            _ => detect_format(text),
        };
        let parsed = match format {
            SubFormat::Srt => srt::parse(text),
            SubFormat::Ass => ass::parse(text),
            SubFormat::WebVtt => vtt::parse(text),
            SubFormat::MicroDvd => microdvd::parse(text, microdvd::DEFAULT_FPS),
            SubFormat::Unknown => Parsed::default(),
        };
        let Parsed { title, mut cues } = parsed;
        sort_cues(&mut cues);
        Self {
            format,
            title,
            cues,
            encoding,
        }
    }
}

/// Sort cues by start time, treating `NaN` as equal so the order stays total.
pub(crate) fn sort_cues(cues: &mut [Cue]) {
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
}

/// Sniff the format of already-decoded subtitle text.
///
/// The scan is bounded so that a huge, unexpected file cannot make detection
/// expensive; [`SubFormat::Unknown`] means "try nothing, there is nothing here".
pub(crate) fn detect_format(text: &str) -> SubFormat {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let first = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");

    if first
        .get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("WEBVTT"))
    {
        return SubFormat::WebVtt;
    }
    if microdvd::is_microdvd_line(first) {
        return SubFormat::MicroDvd;
    }

    let mut saw_arrow = false;
    for (i, line) in text.lines().enumerate() {
        if i > 2000 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("[script info]")
            || lower.starts_with("[events]")
            || lower.starts_with("[v4+ styles]")
            || lower.starts_with("[v4 styles]")
            || lower.starts_with("dialogue:")
        {
            return SubFormat::Ass;
        }
        if line.contains("-->") {
            saw_arrow = true;
        }
    }

    if saw_arrow {
        SubFormat::Srt
    } else {
        SubFormat::Unknown
    }
}
