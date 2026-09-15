//! Fitting the window to the screen it opens on.
//!
//! The player remembers the size the user last gave it, and would otherwise
//! hand that size straight to the window manager. On the machine it was saved
//! on that is exactly right; on a 1366×768 laptop, on a 150 %-scaled display,
//! or after a resolution change it is a window taller than the screen, with the
//! transport bar — the only way to control playback — sitting off the bottom
//! edge where it cannot be reached.
//!
//! Two things keep that from happening:
//!
//! * [`initial_size`] fits the size the window is *created* with to the room the
//!   primary monitor actually has, so the player never opens too large;
//! * [`Guard`] re-checks the window while it runs, so a resolution change, a
//!   remote-desktop resize or a move to a smaller monitor is followed instead of
//!   leaving the window hanging off the screen.
//!
//! Both are pure functions over sizes in points, which is what makes them
//! testable without a screen attached.

use mvp_platform::monitor;

/// The size the player opens at when the screen has room for it.
pub const PREFERRED_SIZE: [f32; 2] = [1280.0, 780.0];

/// The smallest window the interface is laid out for, in points.
///
/// Every panel is designed to stay usable down to this size — see
/// [`crate::layout`], which is where the *width* the layout needs is computed.
pub const MIN_SIZE: [f32; 2] = [720.0, 420.0];

/// The most of a screen a start-up window will take.
///
/// Not the whole thing: a window that exactly fills the screen looks like a
/// maximised one and hides the fact that it can be moved, and on a screen with
/// an auto-hiding taskbar it leaves no strip to bring the taskbar back with.
const FILL: f32 = 0.94;

/// Room left for the taskbar and the window's own frame when the guard has to
/// shrink a window that is taller than the screen.
const CHROME: f32 = 56.0;

/// Slack used when comparing sizes, so a sub-pixel rounding difference between
/// the size asked for and the size reported back does not look like a window
/// that does not fit.
const TOLERANCE: f32 = 1.0;

/// The window's starting size and starting minimum size, fitted to a screen.
///
/// `room` is the primary monitor's usable area in points, as measured by
/// [`monitor::primary_work_area_points`]; `None` means the screen could not be
/// measured, and the preferred size is then used unchanged.
pub fn initial_size(requested: [f32; 2], room: Option<(f32, f32)>) -> ([f32; 2], [f32; 2]) {
    let Some((width, height)) = room else {
        return (sanitise(requested), MIN_SIZE);
    };
    let requested = sanitise(requested);
    let available = [(width * FILL).max(1.0), (height * FILL).max(1.0)];
    let size = [
        requested[0].clamp(1.0, available[0]),
        requested[1].clamp(1.0, available[1]),
    ];
    // The minimum has to come down with it: a 720 pt floor on a 640 pt screen
    // would push the window straight back off the edge it was just fitted to.
    let min = [MIN_SIZE[0].min(available[0]), MIN_SIZE[1].min(available[1])];
    (size, min)
}

/// A remembered size, made safe to use.
///
/// `settings.json` outlives the display it was written on and can be edited by
/// hand; a zero, negative, absurd or non-finite size must not reach the window
/// manager, and a size below the minimum would open a window the player cannot
/// lay out.
fn sanitise(size: [f32; 2]) -> [f32; 2] {
    let axis = |value: f32, floor: f32, fallback: f32| {
        if value.is_finite() && value >= floor {
            value
        } else {
            fallback
        }
    };
    [
        axis(size[0], MIN_SIZE[0], PREFERRED_SIZE[0]),
        axis(size[1], MIN_SIZE[1], PREFERRED_SIZE[1]),
    ]
}

/// The size to open at, given the screen the primary monitor offers.
pub fn startup_size(requested: [f32; 2]) -> ([f32; 2], [f32; 2]) {
    initial_size(requested, monitor::primary_work_area_points())
}

/// Whether a remembered window position is still on a screen.
pub fn position_reachable(position: [f32; 2], size: [f32; 2]) -> bool {
    monitor::position_is_reachable((position[0], position[1]), (size[0], size[1]))
}

/// A resize the window should be given.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    /// The window's own size, in points.
    pub size: [f32; 2],
    /// The smallest size to allow, so the window can actually shrink this far.
    pub min: [f32; 2],
}

/// Keeps the window inside the screen it is on.
///
/// The guard acts when the screen *changes* — the first time the window manager
/// tells us about it, and again whenever that report differs. Doing nothing
/// while the report is unchanged is what keeps this from becoming a fight with
/// the user: a window deliberately dragged across two monitors, or resized by
/// hand, is left exactly where it was put.
#[derive(Debug, Default)]
pub struct Guard {
    /// The monitor size as last seen, in points.
    monitor: Option<[f32; 2]>,
}

impl Guard {
    /// Decide what, if anything, to do about a window of `inner` points on a
    /// monitor of `monitor` points.
    ///
    /// Both are what the window manager reports; either may be `None` while the
    /// window is still being created.
    pub fn fit(&mut self, inner: Option<[f32; 2]>, monitor_size: Option<[f32; 2]>) -> Option<Fit> {
        let monitor_size = monitor_size?;
        if !monitor_size[0].is_finite()
            || !monitor_size[1].is_finite()
            || monitor_size[0] < 1.0
            || monitor_size[1] < 1.0
        {
            return None;
        }
        // A window manager that reports the same screen again has nothing new to
        // tell us; only a change re-opens the question.
        if self.monitor.replace(monitor_size) == Some(monitor_size) {
            return None;
        }
        let inner = inner?;
        if !inner[0].is_finite() || !inner[1].is_finite() {
            return None;
        }

        // Too big is measured against the *monitor*, not against the work area:
        // a maximised window legitimately covers the whole work area, and
        // shrinking it would restore it. The size aimed for does leave room for
        // the taskbar, which `monitor_size` still includes.
        let target_height = (monitor_size[1] - CHROME).max(MIN_SIZE[1] * 0.5);
        if inner[0] <= monitor_size[0] + TOLERANCE && inner[1] <= monitor_size[1] + TOLERANCE {
            return None;
        }
        let size = [inner[0].min(monitor_size[0]), inner[1].min(target_height)];
        log::info!(
            "窗口 {:?} 超出屏幕 {:?}，将调整为 {:?}",
            inner,
            monitor_size,
            size
        );
        Some(Fit {
            size,
            min: [MIN_SIZE[0].min(size[0]), MIN_SIZE[1].min(size[1])],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_big_screen_gets_the_preferred_window() {
        // 2560×1440 at 100 %: plenty of room, so nothing changes.
        let (size, min) = initial_size(PREFERRED_SIZE, Some((2560.0, 1400.0)));
        assert_eq!(size, PREFERRED_SIZE);
        assert_eq!(min, MIN_SIZE);
    }

    #[test]
    fn a_small_screen_shrinks_the_window_and_its_minimum() {
        // 1366×768 at 150 % is only about 910×490 points of usable screen.
        let (size, min) = initial_size(PREFERRED_SIZE, Some((910.0, 490.0)));
        assert!(size[0] <= 910.0, "{size:?}");
        assert!(size[1] <= 490.0, "{size:?}");
        assert!(
            min[0] <= size[0] && min[1] <= size[1],
            "{min:?} vs {size:?}"
        );
        assert!(size[0] > 0.0 && size[1] > 0.0);

        // A screen smaller than the player's own minimum: the screen wins, and
        // the minimum has to come down with it or the window cannot fit.
        let (size, min) = initial_size(PREFERRED_SIZE, Some((600.0, 400.0)));
        assert!(min[0] <= 600.0, "{min:?}");
        assert!(min[1] <= 400.0, "{min:?}");
        assert!(size[0] <= 600.0 && size[1] <= 400.0, "{size:?}");
    }

    #[test]
    fn a_remembered_size_that_makes_no_sense_is_replaced() {
        for nonsense in [
            [0.0, 0.0],
            [-100.0, -100.0],
            [f32::NAN, 10.0],
            [10.0, f32::INFINITY],
            [40.0, 30.0],
        ] {
            let (size, _) = initial_size(nonsense, None);
            assert!(size[0] >= MIN_SIZE[0], "{nonsense:?} → {size:?}");
            assert!(size[1] >= MIN_SIZE[1], "{nonsense:?} → {size:?}");
        }
    }

    #[test]
    fn an_unmeasurable_screen_keeps_the_preferred_size() {
        let (size, min) = initial_size(PREFERRED_SIZE, None);
        assert_eq!(size, PREFERRED_SIZE);
        assert_eq!(min, MIN_SIZE);
    }

    #[test]
    fn the_guard_leaves_a_window_that_fits_alone() {
        let mut guard = Guard::default();
        assert!(guard
            .fit(Some([1280.0, 780.0]), Some([1920.0, 1080.0]))
            .is_none());
        // Still nothing once the screen is known and unchanged.
        assert!(guard
            .fit(Some([1280.0, 780.0]), Some([1920.0, 1080.0]))
            .is_none());
    }

    #[test]
    fn the_guard_shrinks_a_window_that_is_bigger_than_the_screen() {
        let mut guard = Guard::default();
        let fit = guard
            .fit(Some([2560.0, 1440.0]), Some([1366.0, 768.0]))
            .expect("a window larger than the screen must be fitted");
        assert!(fit.size[0] <= 1366.0, "{fit:?}");
        assert!(fit.size[1] <= 768.0, "{fit:?}");
        // The minimum must come down too, or the resize would be refused.
        assert!(
            fit.min[0] <= fit.size[0] && fit.min[1] <= fit.size[1],
            "{fit:?}"
        );

        // The request is made once, not on every frame.
        assert!(guard
            .fit(Some([2560.0, 1440.0]), Some([1366.0, 768.0]))
            .is_none());
    }

    #[test]
    fn the_guard_reacts_to_a_resolution_change() {
        let mut guard = Guard::default();
        assert!(guard
            .fit(Some([1280.0, 780.0]), Some([1920.0, 1080.0]))
            .is_none());
        // The screen shrank underneath the window.
        let fit = guard
            .fit(Some([1280.0, 780.0]), Some([1024.0, 768.0]))
            .expect("a screen change must re-open the question");
        assert!(fit.size[0] <= 1024.0, "{fit:?}");
    }

    #[test]
    fn the_guard_ignores_what_it_cannot_measure() {
        let mut guard = Guard::default();
        assert!(guard.fit(None, None).is_none());
        assert!(guard.fit(Some([100.0, 100.0]), None).is_none());
        assert!(guard.fit(Some([100.0, 100.0]), Some([0.0, 0.0])).is_none());
        assert!(guard
            .fit(Some([f32::NAN, 100.0]), Some([1920.0, 1080.0]))
            .is_none());
    }

    #[test]
    fn a_maximised_window_is_not_shrunk() {
        // A maximised window reports the work area, which is smaller than the
        // monitor, so it is never "too big" — the guard must not resize it,
        // because resizing a maximised window restores it.
        let mut guard = Guard::default();
        assert!(guard
            .fit(Some([1920.0, 1040.0]), Some([1920.0, 1080.0]))
            .is_none());
    }

    #[test]
    fn the_size_the_guard_aims_for_leaves_room_for_the_taskbar() {
        let mut guard = Guard::default();
        let fit = guard
            .fit(Some([1920.0, 1200.0]), Some([1920.0, 1080.0]))
            .expect("a window taller than the screen must be fitted");
        assert!(fit.size[0] <= 1920.0, "{fit:?}");
        // Not the full monitor height: the taskbar has to stay visible.
        assert!(fit.size[1] <= 1080.0 - CHROME + f32::EPSILON, "{fit:?}");
    }
}
