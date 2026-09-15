//! How much room the window has, and what that buys.
//!
//! Every panel in the player is built from fixed-size parts — a 28 pt icon
//! button, a 90 pt volume slider, an 84 pt transport bar — which is exactly what
//! keeps the interface aligned at the size it was designed for, and exactly what
//! makes it fall apart in a window the user has dragged narrow. Rather than let
//! `egui` squeeze those parts into each other, the layout *asks* how much width
//! is available and drops the optional pieces, in a fixed order, until what is
//! left fits.
//!
//! All thresholds live here so a narrow window degrades the same way in every
//! panel, and so the arithmetic can be tested: [`transport_budget`] is a pure
//! function, and the tests walk every window width the player allows, asserting
//! that the controls it selects never need more room than there is.

use egui::{Context, Rect};

use crate::theme::{font, space};

/// Gap between two controls (`style.spacing.item_spacing.x`).
const GAP: f32 = 8.0;
/// A tool button. `icons::icon_button` grows its hit area to 28 pt square.
const TOOL: f32 = 28.0;
/// The play/pause button, deliberately larger than the rest.
const PLAY: f32 = 40.0;
/// Width of the volume slider.
const VOLUME_SLIDER: f32 = 90.0;
/// Width reserved for the "100%" readout.
const VOLUME_PERCENT: f32 = 38.0;
/// The speed icon shown before the speed menu.
const SPEED_ICON: f32 = 18.0;
/// Horizontal padding `egui` adds to a button, both sides together.
const BUTTON_PADDING: f32 = 24.0;
/// Space between the transport group and the volume group.
const GROUP_GAP: f32 = 12.0;
/// Room kept in hand for the small errors that accumulate when a layout is
/// measured rather than computed: the spacing egui adds around the two
/// `add_space` gaps in the bar, a font metric, and a rounding or two.
const SLACK: f32 = 24.0;

/// Width and height of the area being laid out, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Width available to a full-width panel.
    pub width: f32,
    /// Height available to a full-height panel.
    pub height: f32,
}

impl Metrics {
    /// Measure the window.
    pub fn of(ctx: &Context) -> Self {
        let screen: Rect = ctx.screen_rect();
        Self::new(screen.width(), screen.height())
    }

    /// Measure an explicit size.
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width: width.max(1.0),
            height: height.max(1.0),
        }
    }

    /// The sidebar's `(minimum, maximum)` width.
    ///
    /// The sidebar is a third of the interface on a wide screen and a nuisance
    /// on a narrow one, so its ceiling follows the window instead of being a
    /// fixed 460 pt that would leave a 720 pt window with no picture at all.
    pub fn sidebar_width_range(&self) -> (f32, f32) {
        let max = (self.width * 0.42).clamp(200.0, 460.0);
        let min = (max * 0.6).clamp(160.0, 240.0);
        (min, max)
    }

    /// The width the sidebar opens at.
    pub fn sidebar_default_width(&self) -> f32 {
        let (min, max) = self.sidebar_width_range();
        (self.width * 0.24).clamp(min, max)
    }

    /// The width of the floating transport bar in fullscreen.
    ///
    /// Wide enough to hold the whole control row, but never wider than the
    /// screen it floats over — on a narrow display the bar has to shrink rather
    /// than hang off both edges.
    pub fn transport_overlay_width(&self) -> f32 {
        (self.width - 48.0).clamp(320.0, 720.0)
    }

    /// A dialog's size: `preferred`, shrunk to fit this window and never below
    /// `minimum` — unless the window itself is smaller than that, in which case
    /// the window wins, because a dialog larger than its owner is unusable.
    pub fn dialog_size(&self, preferred: [f32; 2], minimum: [f32; 2]) -> [f32; 2] {
        let room_w = (self.width - 48.0).max(200.0);
        let room_h = (self.height - 72.0).max(180.0);
        [
            preferred[0].min(room_w).max(minimum[0].min(room_w)),
            preferred[1].min(room_h).max(minimum[1].min(room_h)),
        ]
    }

    /// The width of a centred content column inside `available` points.
    pub fn content_width(&self, available: f32, preferred: f32, minimum: f32) -> f32 {
        let room = available.max(1.0);
        preferred.min(room).max(minimum.min(room))
    }
}

/// What the record takes of the audio canvas, in each direction.
const AUDIO_ARTWORK_FRACTION: f32 = 0.30;
/// Smallest and largest the record is ever drawn, so it neither vanishes in a
/// small window nor turns into a cartoon in a huge one.
const AUDIO_ARTWORK_RANGE: (f32, f32) = (56.0, 200.0);
/// Shortest audio canvas that still has room for the technical line.
const AUDIO_FACTS_HEIGHT: f32 = 420.0;
/// Narrowest one.
const AUDIO_FACTS_WIDTH: f32 = 480.0;
/// Font size multiplier that turns a line of type into the height it takes.
/// `egui` sets a line at roughly 1.2x its point size; rounding up keeps the
/// column's own arithmetic an over-estimate, which is the safe direction.
const LINE_HEIGHT: f32 = 1.5;

/// How the audio screen divides its canvas.
///
/// A file with no picture has no letterbox to fit anything into, so its screen
/// is a poster instead: a record, the title, who made it, how it is encoded and
/// where the playhead is, stacked in the middle. Every size is derived from the
/// canvas here — not at the call site — for the same reason the transport bar
/// has a budget: a poster laid out with fixed sizes loses its bottom half in a
/// small window, and one laid out ad hoc loses something different in each
/// place it is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioScreen {
    /// Diameter of the record at the centre.
    pub artwork: f32,
    /// Gap between two blocks of the column.
    pub gap: f32,
    /// Font size of the title.
    pub title: f32,
    /// Font size of the secondary lines (artist, encoding).
    pub caption: f32,
    /// Font size of the clock.
    pub clock: f32,
    /// Whether the technical line (codec, sample rate, bit rate) is shown.
    pub facts: bool,
}

impl AudioScreen {
    /// Size the poster for a canvas `width` x `height` points.
    ///
    /// The canvas of the smallest window the player opens at is about 560x304
    /// points with the sidebar out, which is where the compact type sizes come
    /// from; the technical line is the first thing to go, exactly as the
    /// transport bar gives up its readouts before its controls.
    pub fn fit(width: f32, height: f32) -> Self {
        let width = width.max(1.0);
        let height = height.max(1.0);
        let compact = height < AUDIO_FACTS_HEIGHT || width < AUDIO_FACTS_WIDTH;
        let artwork = (width * AUDIO_ARTWORK_FRACTION)
            .min(height * AUDIO_ARTWORK_FRACTION)
            .clamp(AUDIO_ARTWORK_RANGE.0, AUDIO_ARTWORK_RANGE.1);
        Self {
            artwork,
            gap: if compact { space::SM } else { space::LG },
            title: if compact { font::H2 } else { font::H1 },
            caption: if compact { font::SMALL } else { font::BODY },
            clock: if compact { font::H3 } else { font::H2 },
            facts: !compact,
        }
    }

    /// The height of the whole column, record included.
    pub fn height(&self) -> f32 {
        let title = self.title * LINE_HEIGHT;
        let caption = self.caption * LINE_HEIGHT;
        let facts = if self.facts { caption } else { 0.0 };
        let clock = self.clock * LINE_HEIGHT;
        let gaps = self.gap * if self.facts { 3.0 } else { 2.0 };
        self.artwork + gaps + title + caption + facts + clock
    }
}

/// Which optional controls the transport bar has room for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportBudget {
    /// The volume slider.
    pub volume_slider: bool,
    /// The "100%" readout next to it.
    pub volume_percent: bool,
    /// The speed menu.
    pub speed: bool,
    /// Take a snapshot.
    pub snapshot: bool,
    /// Repeat mode.
    pub repeat: bool,
    /// Shuffle.
    pub shuffle: bool,
    /// Subtitle visibility.
    pub subtitles: bool,
}

impl TransportBudget {
    /// Everything the bar can show.
    const FULL: Self = Self {
        volume_slider: true,
        volume_percent: true,
        speed: true,
        snapshot: true,
        repeat: true,
        shuffle: true,
        subtitles: true,
    };

    /// What is left when not even the optional controls fit: the bar always
    /// shows previous, rewind, play, forward, next, mute and the three
    /// right-hand buttons, whatever size the window is.
    const MINIMAL: Self = Self {
        volume_slider: false,
        volume_percent: false,
        speed: false,
        snapshot: false,
        repeat: false,
        shuffle: false,
        subtitles: false,
    };

    /// Drop one control, in the order the bar gives things up. `step` counts
    /// from the least useful: the readout first, then the controls that have a
    /// menu entry and a keyboard shortcut to fall back on.
    fn without(step: usize) -> Self {
        let mut budget = Self::FULL;
        for index in 0..step {
            match index {
                0 => budget.volume_percent = false,
                1 => budget.speed = false,
                2 => budget.volume_slider = false,
                3 => budget.snapshot = false,
                4 => budget.subtitles = false,
                5 => budget.shuffle = false,
                _ => budget.repeat = false,
            }
        }
        budget
    }

    /// The width this selection needs, in points.
    fn demand(&self, speed_label_width: f32) -> f32 {
        // Left group: previous, rewind, play/pause, forward, next, mute.
        let mut left = 4.0 * (TOOL + GAP) + (PLAY + GAP) + GROUP_GAP + (TOOL + GAP);
        if self.volume_slider {
            left += VOLUME_SLIDER + GAP;
        }
        if self.volume_percent {
            left += VOLUME_PERCENT + GAP;
        }
        if self.speed {
            left += SPEED_ICON + GAP + speed_label_width + BUTTON_PADDING + GAP;
        }

        // Right group: fullscreen, settings and the sidebar toggle never leave.
        let mut right = 3.0 * (TOOL + GAP);
        if self.subtitles {
            right += TOOL + GAP;
        }
        if self.shuffle {
            right += TOOL + GAP;
        }
        if self.repeat {
            right += TOOL + GAP;
        }
        if self.snapshot {
            right += TOOL + GAP;
        }

        // The `GAP` between the two groups, plus the slack that absorbs the
        // difference between this arithmetic and what `egui` actually measures.
        left + right + GAP + SLACK
    }
}

/// How many controls have to be given up, out of `TransportBudget::FULL`.
const DROP_STEPS: usize = 7;

/// Choose the transport controls that fit in `width` points.
///
/// `speed_label_width` is the measured width of the text inside the speed menu
/// ("1.00x"), which depends on the font and so cannot be a constant here.
pub fn transport_budget(width: f32, speed_label_width: f32) -> TransportBudget {
    for step in 0..=DROP_STEPS {
        let budget = TransportBudget::without(step);
        if budget.demand(speed_label_width) <= width {
            return budget;
        }
    }
    TransportBudget::MINIMAL
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plausible widths for the "1.00x" label, from an empty font to a very
    /// wide one, so the tests do not depend on a font metric.
    const SPEED_LABELS: [f32; 4] = [0.0, 26.0, 34.0, 60.0];

    #[test]
    fn a_default_window_shows_every_control() {
        for label in SPEED_LABELS {
            assert_eq!(
                transport_budget(1280.0, label),
                TransportBudget::FULL,
                "the designed-for window size must not drop anything (label {label})"
            );
        }
    }

    #[test]
    fn the_selected_controls_always_fit() {
        // The whole point of the budget: whatever width the window is, the
        // controls chosen for it must not need more room than it has. This is
        // the arithmetic that keeps the two halves of the bar from overlapping.
        //
        // Below the width where even the essential controls fit there is
        // nothing left to give up, so the walk starts there.
        let floor = TransportBudget::MINIMAL.demand(0.0).ceil() as u32;
        for width in floor..=2560 {
            for label in SPEED_LABELS {
                let budget = transport_budget(width as f32, label);
                assert!(
                    budget.demand(label) <= width as f32,
                    "width {width} chose {budget:?}, needing {}",
                    budget.demand(label)
                );
            }
        }
    }

    #[test]
    fn the_essential_controls_fit_the_narrowest_window() {
        // The narrowest window the player opens at, even on a small screen, is
        // 560 pt across; the bar has to hold its essential controls there, and
        // the widest the speed label can plausibly be must not change that.
        assert!(TransportBudget::MINIMAL.demand(64.0) <= 560.0);
    }

    #[test]
    fn the_most_important_controls_are_the_last_to_go() {
        // The bar may look thin on a tiny window, but it must never lose the
        // controls that have no replacement.
        let tiny = transport_budget(200.0, 34.0);
        assert_eq!(tiny, TransportBudget::MINIMAL);
        assert!(!tiny.volume_slider && !tiny.speed && !tiny.snapshot);
    }

    #[test]
    fn controls_are_given_up_one_at_a_time() {
        // As the window narrows the bar must shed things in a fixed order,
        // never keeping a less important control while dropping a more
        // important one.
        let mut previous = transport_budget(4000.0, 34.0);
        let mut drops = 0;
        for width in (400..=4000).rev() {
            let budget = transport_budget(width as f32, 34.0);
            let count = [
                budget.volume_slider,
                budget.volume_percent,
                budget.speed,
                budget.snapshot,
                budget.repeat,
                budget.shuffle,
                budget.subtitles,
            ]
            .iter()
            .filter(|shown| **shown)
            .count();
            let before = [
                previous.volume_slider,
                previous.volume_percent,
                previous.speed,
                previous.snapshot,
                previous.repeat,
                previous.shuffle,
                previous.subtitles,
            ]
            .iter()
            .filter(|shown| **shown)
            .count();
            if count != before {
                drops += 1;
                assert_eq!(count + 1, before, "controls must be dropped one at a time");
            }
            previous = budget;
        }
        assert!(drops > 0, "a 400 pt window cannot show the full bar");
    }

    #[test]
    fn the_sidebar_never_takes_the_whole_window() {
        for width in [320.0, 720.0, 1280.0, 2560.0, 3840.0] {
            let metrics = Metrics::new(width, 800.0);
            let (min, max) = metrics.sidebar_width_range();
            assert!(min <= max, "width {width}");
            assert!(max < width, "the sidebar must leave room for the picture");
            let default = metrics.sidebar_default_width();
            assert!((min..=max).contains(&default), "width {width}");
        }
    }

    #[test]
    fn a_dialog_fits_the_window_it_opens_in() {
        let preferred = [860.0, 620.0];
        let minimum = [620.0, 440.0];

        let roomy = Metrics::new(2560.0, 1440.0).dialog_size(preferred, minimum);
        assert_eq!(roomy, preferred);

        // 1366×768 at 150 % scaling: 910×512 points of screen.
        let tight = Metrics::new(910.0, 512.0).dialog_size(preferred, minimum);
        assert!(tight[0] <= 910.0 - 48.0, "{tight:?}");
        assert!(tight[1] <= 512.0 - 72.0, "{tight:?}");

        // A window smaller than the minimum still gets a dialog it can hold.
        let tiny = Metrics::new(500.0, 360.0).dialog_size(preferred, minimum);
        assert!(tiny[0] <= 500.0, "{tiny:?}");
        assert!(tiny[1] <= 360.0, "{tiny:?}");
    }

    #[test]
    fn a_content_column_respects_its_room() {
        let metrics = Metrics::new(1280.0, 800.0);
        assert_eq!(metrics.content_width(1200.0, 460.0, 180.0), 460.0);
        assert_eq!(metrics.content_width(300.0, 460.0, 180.0), 300.0);
        assert_eq!(metrics.content_width(100.0, 460.0, 180.0), 100.0);
    }

    #[test]
    fn a_narrow_window_gets_an_overlay_that_fits() {
        assert_eq!(Metrics::new(1280.0, 800.0).transport_overlay_width(), 720.0);
        assert_eq!(Metrics::new(560.0, 400.0).transport_overlay_width(), 512.0);
        assert_eq!(Metrics::new(200.0, 200.0).transport_overlay_width(), 320.0);
    }

    #[test]
    fn the_audio_poster_always_fits_its_canvas() {
        // The audio screen is one column, and a column that does not fit is a
        // column with its bottom half off the screen — the clock, of all
        // things, is what would go. The sweep starts at the canvas of the
        // smallest window the player opens at (720x420 points, less the menu
        // bar and the transport bar) and runs to a 4K screen.
        for width in (520..=3840).step_by(40) {
            for height in (240..=2160).step_by(40) {
                let screen = AudioScreen::fit(width as f32, height as f32);
                assert!(
                    screen.height() <= height as f32,
                    "{width}x{height} needs {} but has {height}",
                    screen.height()
                );
                assert!(
                    screen.artwork <= width as f32 && screen.artwork <= height as f32,
                    "{width}x{height} asked for a {} pt record",
                    screen.artwork
                );
            }
        }
    }

    #[test]
    fn the_audio_poster_grows_with_the_canvas_and_then_stops() {
        let small = AudioScreen::fit(560.0, 304.0);
        let medium = AudioScreen::fit(1280.0, 720.0);
        let huge = AudioScreen::fit(3840.0, 2160.0);

        assert!(small.artwork < medium.artwork, "{small:?} / {medium:?}");
        assert_eq!(medium.artwork, AUDIO_ARTWORK_RANGE.1, "{medium:?}");
        assert_eq!(huge.artwork, AUDIO_ARTWORK_RANGE.1, "{huge:?}");

        // The column is the same shape at both ends of the range: the record
        // stays the dominant element instead of the type taking over.
        for screen in [small, medium, huge] {
            assert!(screen.artwork > screen.title * 2.0, "{screen:?}");
        }
    }

    #[test]
    fn a_short_audio_canvas_gives_up_its_technical_line() {
        // The same order as everywhere else in the player: the optional detail
        // goes first, and what is left is the record, the title and the clock.
        let compact = AudioScreen::fit(560.0, 304.0);
        assert!(!compact.facts, "{compact:?}");
        assert!(compact.caption < compact.title && compact.clock < compact.title);

        let roomy = AudioScreen::fit(1280.0, 720.0);
        assert!(roomy.facts, "{roomy:?}");
        assert!(roomy.artwork > compact.artwork, "{roomy:?} / {compact:?}");
    }
}
