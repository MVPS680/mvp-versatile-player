//! Screen geometry: how much room the monitors actually give a window.
//!
//! A media player wants "a window as large as the screen is willing to allow",
//! and getting that wrong is what leaves the transport bar hanging off the
//! bottom of a 1366×768 laptop, or a 150 %-scaled display, where the usable
//! area in *logical* points is far smaller than the resolution suggests. The
//! startup path therefore asks the operating system, before any window exists,
//! where the monitors are, how much of each is usable and how the pixels are
//! scaled.
//!
//! Everything here is deliberately best-effort: a measurement that cannot be
//! taken returns `None` and the caller falls back to a fixed, conservative
//! window size. Nothing in this module may panic, and none of it is on the
//! per-frame path — it is used once, while the window is being created.

/// A rectangle in physical pixels, relative to the origin of the virtual
/// desktop.
///
/// The origin is the top-left of the *primary* monitor, so a monitor placed to
/// the left of it, or above it, has negative coordinates. That is normal and
/// must not be treated as "off screen".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PixelRect {
    /// Left edge, in physical pixels.
    pub x: i32,
    /// Top edge, in physical pixels.
    pub y: i32,
    /// Width, in physical pixels.
    pub width: i32,
    /// Height, in physical pixels.
    pub height: i32,
}

impl PixelRect {
    /// Right edge (exclusive).
    pub fn right(&self) -> i32 {
        self.x.saturating_add(self.width)
    }

    /// Bottom edge (exclusive).
    pub fn bottom(&self) -> i32 {
        self.y.saturating_add(self.height)
    }

    /// Whether the rectangle covers no pixels at all.
    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    /// The area shared with `other`, or `None` when they do not touch.
    pub fn intersect(&self, other: &PixelRect) -> Option<PixelRect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= x || bottom <= y {
            return None;
        }
        Some(PixelRect {
            x,
            y,
            width: right - x,
            height: bottom - y,
        })
    }

    /// The same rectangle in logical points at `dpi` (`96` = 100 %).
    ///
    /// `egui` and `winit` speak points, the Win32 API speaks pixels, and this is
    /// the only place the two are converted — so a window is never asked to be
    /// 1280 *pixels* on a screen that only has room for 1280 *points*.
    pub fn to_points(&self, dpi: u32) -> (f32, f32) {
        let scale = dpi_scale(dpi);
        (self.width as f32 / scale, self.height as f32 / scale)
    }
}

/// Ratio between physical pixels and logical points for `dpi`.
///
/// `96` dpi is the 100 % baseline; 120 is 125 %, 144 is 150 % and so on. A
/// nonsense value (a driver reporting zero) falls back to 100 % rather than
/// dividing by zero.
pub fn dpi_scale(dpi: u32) -> f32 {
    if dpi == 0 {
        return 1.0;
    }
    dpi as f32 / 96.0
}

#[cfg(windows)]
mod imp {
    use super::PixelRect;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, MonitorFromWindow, MONITORINFO,
        MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};

    /// Win32 rectangle → our own, so the Windows types stay in this module.
    fn rect(rc: RECT) -> PixelRect {
        PixelRect {
            x: rc.left,
            y: rc.top,
            width: rc.right - rc.left,
            height: rc.bottom - rc.top,
        }
    }

    /// The *usable* part of a monitor — the screen minus the taskbar and any
    /// other appbars — which is the only area a normal window may occupy.
    fn work_area(hmonitor: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<PixelRect> {
        if hmonitor.is_invalid() {
            return None;
        }
        // SAFETY: `MONITORINFO` is a plain-old-data struct of integers, so an
        // all-zero value is a valid one to hand to `GetMonitorInfoW` once
        // `cbSize` has been set; the call then fills in the rectangles.
        let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        // SAFETY: `info` is a correctly sized, initialised `MONITORINFO`, and
        // the handle was produced by one of the monitor functions above.
        if !unsafe { GetMonitorInfoW(hmonitor, &mut info) }.as_bool() {
            return None;
        }
        let area = rect(info.rcWork);
        (!area.is_empty()).then_some(area)
    }

    pub fn primary_work_area() -> Option<PixelRect> {
        let point = POINT { x: 0, y: 0 };
        // SAFETY: no borrowed state; the point is a value on the stack.
        let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY) };
        work_area(monitor)
    }

    pub fn work_area_at_point(x: i32, y: i32) -> Option<PixelRect> {
        let point = POINT { x, y };
        // SAFETY: as above.
        let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
        work_area(monitor)
    }

    pub fn work_area_for_rect(rect: (i32, i32, i32, i32)) -> Option<PixelRect> {
        let (x, y, width, height) = rect;
        let raw = RECT {
            left: x,
            top: y,
            right: x.saturating_add(width),
            bottom: y.saturating_add(height),
        };
        // `MONITOR_DEFAULTTONULL` is what makes this useful for validation: a
        // rectangle that touches no monitor at all answers "no monitor", while
        // one that is merely clipped by a screen edge answers with that screen.
        // SAFETY: `raw` is a value on the stack and outlives the call.
        let monitor = unsafe { MonitorFromRect(&raw, MONITOR_DEFAULTTONULL) };
        work_area(monitor)
    }

    pub fn work_area_of_window(hwnd: isize) -> Option<PixelRect> {
        if hwnd == 0 {
            return primary_work_area();
        }
        let window = HWND(hwnd as *mut core::ffi::c_void);
        // SAFETY: the caller passes the handle of one of our own windows.
        let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTOPRIMARY) };
        work_area(monitor)
    }

    pub fn system_dpi() -> u32 {
        // SAFETY: no arguments, no borrowed state.
        let dpi = unsafe { GetDpiForSystem() };
        if dpi == 0 {
            96
        } else {
            dpi
        }
    }

    pub fn dpi_of_window(hwnd: isize) -> u32 {
        if hwnd == 0 {
            return system_dpi();
        }
        // SAFETY: the caller passes the handle of one of our own windows.
        let dpi = unsafe { GetDpiForWindow(HWND(hwnd as *mut core::ffi::c_void)) };
        if dpi == 0 {
            system_dpi()
        } else {
            dpi
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::PixelRect;

    pub fn primary_work_area() -> Option<PixelRect> {
        None
    }

    pub fn work_area_at_point(_x: i32, _y: i32) -> Option<PixelRect> {
        None
    }

    pub fn work_area_for_rect(_rect: (i32, i32, i32, i32)) -> Option<PixelRect> {
        None
    }

    pub fn work_area_of_window(_hwnd: isize) -> Option<PixelRect> {
        None
    }

    pub fn system_dpi() -> u32 {
        96
    }

    pub fn dpi_of_window(_hwnd: isize) -> u32 {
        96
    }
}

pub use imp::{
    dpi_of_window, primary_work_area, system_dpi, work_area_at_point, work_area_for_rect,
    work_area_of_window,
};

/// The usable area of the primary monitor, in logical points at `dpi`.
///
/// `None` when the screen cannot be measured, which is the only case where the
/// caller has to guess.
pub fn primary_work_area_points() -> Option<(f32, f32)> {
    let dpi = system_dpi();
    primary_work_area().map(|area| area.to_points(dpi))
}

/// Whether a window of `size` points placed at `position` points would still be
/// reachable by the user.
///
/// A saved window position outlives the display it was chosen on: unplug the
/// second monitor, or change the resolution, and the window reopens somewhere
/// the user cannot see or grab it. The rule here is deliberately forgiving — an
/// edge shared with a screen is enough — because a window that is *partly*
/// visible can still be dragged, while one that is entirely off-screen cannot.
pub fn position_is_reachable(position: (f32, f32), size: (f32, f32)) -> bool {
    /// How much of the window must be on a screen to count as reachable.
    const VISIBLE: i32 = 96;
    /// Title bar height, which is what the user actually grabs.
    const GRABBABLE: i32 = 32;

    let dpi = system_dpi();
    let scale = dpi_scale(dpi);
    let to_px = |value: f32| (value * scale).round() as i32;

    let rect = (
        to_px(position.0),
        to_px(position.1),
        to_px(size.0).max(1),
        to_px(size.1).max(1),
    );
    let Some(work) = work_area_for_rect(rect) else {
        return false;
    };
    let window = PixelRect {
        x: rect.0,
        y: rect.1,
        width: rect.2,
        height: rect.3,
    };
    match window.intersect(&work) {
        Some(shared) => shared.width >= VISIBLE && shared.height >= GRABBABLE,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_scale_maps_the_baseline() {
        assert!((dpi_scale(96) - 1.0).abs() < f32::EPSILON);
        assert!((dpi_scale(120) - 1.25).abs() < 1e-6);
        assert!((dpi_scale(144) - 1.5).abs() < 1e-6);
        // A driver that reports nothing must not divide by zero.
        assert!((dpi_scale(0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn physical_pixels_become_fewer_points_when_scaled() {
        let area = PixelRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        };
        assert_eq!(area.to_points(96), (1920.0, 1040.0));
        // 1920×1040 pixels at 150 % is only 1280×693 points of room.
        assert_eq!(area.to_points(144), (1280.0, 693.3333));
    }

    #[test]
    fn negative_coordinates_are_preserved() {
        let left = PixelRect {
            x: -1920,
            y: -200,
            width: 1920,
            height: 1080,
        };
        assert_eq!(left.right(), 0);
        assert_eq!(left.bottom(), 880);
    }

    #[test]
    fn intersect_reports_the_shared_area() {
        let a = PixelRect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let b = PixelRect {
            x: 50,
            y: 60,
            width: 100,
            height: 100,
        };
        let shared = a.intersect(&b).expect("overlapping rectangles");
        assert_eq!(
            (shared.x, shared.y, shared.width, shared.height),
            (50, 60, 50, 40)
        );

        let apart = PixelRect {
            x: 200,
            y: 200,
            width: 10,
            height: 10,
        };
        assert!(a.intersect(&apart).is_none());
    }

    #[test]
    fn primary_work_area_is_usable_when_the_screen_can_be_measured() {
        // Not an assertion about the machine: on a headless host there is no
        // monitor to measure, and that must be a `None`, not a panic.
        if let Some(area) = primary_work_area() {
            assert!(!area.is_empty());
        }
    }

    #[test]
    fn unmeasurable_geometry_never_panics() {
        let _ = primary_work_area_points();
        let _ = work_area_at_point(i32::MIN, i32::MAX);
        let _ = work_area_for_rect((i32::MIN, i32::MIN, i32::MAX, i32::MAX));
        let _ = position_is_reachable((f32::NAN, f32::NAN), (f32::INFINITY, 0.0));
    }
}
