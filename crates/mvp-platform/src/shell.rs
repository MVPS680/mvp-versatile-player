//! Small Win32 shell niceties used by the player UI and the start-up path.
//!
//! Every function here is deliberately lossy about failures: they are cosmetic
//! (title-bar tinting, taskbar flashing) or already surface their own error
//! (opening a document). The one rule they all follow is *never panic* — a
//! missing DWM, a disabled Explorer or an invalid `HWND` must degrade to a log
//! line, not a crash.

use std::path::Path;

/// Tell the shell which application this process is, so that taskbar grouping,
/// jump lists and notifications use the right identity.
///
/// Call once, early, before any window is created. `id` should match the
/// AppUserModelID used by the installer's shortcut, e.g.
/// `"MVP-Versatile-Player"` or a stable reverse-DNS-ish id.
#[cfg(windows)]
pub fn set_app_user_model_id(id: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    let id_w = crate::wide::wide(id);
    // SAFETY: `id_w` is a NUL-terminated UTF-16 buffer that outlives the call.
    let result = unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(id_w.as_ptr())) };
    if let Err(error) = result {
        log::warn!("SetCurrentProcessExplicitAppUserModelID({id:?}) failed: {error}");
    }
}

/// Non-Windows stub: no AppUserModelID concept.
#[cfg(not(windows))]
pub fn set_app_user_model_id(id: &str) {
    let _ = id;
}

/// Switch the native title bar between the light and dark system themes.
///
/// Uses `DWMWA_USE_IMMERSIVE_DARK_MODE`, falling back to the pre-Windows-11
/// attribute number 19. Silently does nothing when DWM is unavailable (a
/// compositing-disabled session, or a platform without DWM at all).
#[cfg(windows)]
pub fn set_dark_titlebar(hwnd: isize, dark: bool) {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{FALSE, HWND, TRUE};
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};

    if hwnd == 0 {
        return;
    }
    let window = HWND(hwnd as *mut core::ffi::c_void);
    let value: BOOL = if dark { TRUE } else { FALSE };
    let size = std::mem::size_of::<BOOL>() as u32;

    // Attribute 20 is the documented value; 19 is what builds before Windows 11
    // 21H2 understood.
    for attribute in [
        windows::Win32::Graphics::Dwm::DWMWA_USE_IMMERSIVE_DARK_MODE,
        DWMWINDOWATTRIBUTE(19),
    ] {
        // SAFETY: `value` is a live `BOOL` for the duration of the call and
        // `size` matches its size exactly, which is the contract of
        // `DwmSetWindowAttribute`.
        let result = unsafe {
            DwmSetWindowAttribute(
                window,
                attribute,
                &value as *const BOOL as *const core::ffi::c_void,
                size,
            )
        };
        if result.is_ok() {
            return;
        }
    }
    log::debug!("DWMWA_USE_IMMERSIVE_DARK_MODE is unavailable; keeping the system title bar");
}

/// Non-Windows stub: no title-bar theming.
#[cfg(not(windows))]
pub fn set_dark_titlebar(hwnd: isize, dark: bool) {
    let _ = (hwnd, dark);
}

/// Tint the native caption, caption text and border to the app's own colours.
///
/// Requires Windows 11 build 22000 or newer; on older builds (or when DWM is
/// off) every call fails and is ignored, which is the intended behaviour since
/// this only ever makes the frame match the player's background.
#[cfg(windows)]
pub fn set_caption_color(hwnd: isize, bg: [u8; 3], text: [u8; 3], border: [u8; 3]) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
    };

    if hwnd == 0 {
        return;
    }
    let window = HWND(hwnd as *mut core::ffi::c_void);
    for (attribute, colour) in [
        (DWMWA_CAPTION_COLOR, bg),
        (DWMWA_TEXT_COLOR, text),
        (DWMWA_BORDER_COLOR, border),
    ] {
        let value = colorref(colour);
        // SAFETY: `value` is a live `u32` (a `COLORREF`) and `size` matches its
        // size, which is the contract of `DwmSetWindowAttribute`.
        let result = unsafe {
            DwmSetWindowAttribute(
                window,
                attribute,
                &value as *const u32 as *const core::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
            )
        };
        if let Err(error) = result {
            log::debug!("DwmSetWindowAttribute({attribute:?}) failed: {error}");
        }
    }
}

/// Non-Windows stub: no caption tinting.
#[cfg(not(windows))]
pub fn set_caption_color(hwnd: isize, bg: [u8; 3], text: [u8; 3], border: [u8; 3]) {
    let _ = (hwnd, bg, text, border);
}

/// Hand the caption colours back to the system theme.
///
/// [`set_caption_color`] is a one-way door — once a colour is set, DWM keeps it
/// until the window is recreated. Turning the player's "dark title bar" switch
/// off therefore has to write `DWMWA_COLOR_DEFAULT` explicitly, or the title bar
/// would stay tinted for the rest of the session.
#[cfg(windows)]
pub fn reset_caption_color(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_COLOR_DEFAULT,
        DWMWA_TEXT_COLOR,
    };

    if hwnd == 0 {
        return;
    }
    let window = HWND(hwnd as *mut core::ffi::c_void);
    for attribute in [DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_BORDER_COLOR] {
        let value: u32 = DWMWA_COLOR_DEFAULT;
        // SAFETY: `value` is a live `u32` (a `COLORREF`) and `size` matches its
        // size, which is the contract of `DwmSetWindowAttribute`.
        let result = unsafe {
            DwmSetWindowAttribute(
                window,
                attribute,
                &value as *const u32 as *const core::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
            )
        };
        if let Err(error) = result {
            log::debug!("DwmSetWindowAttribute({attribute:?}) failed: {error}");
        }
    }
}

/// Non-Windows stub: nothing was tinted in the first place.
#[cfg(not(windows))]
pub fn reset_caption_color(hwnd: isize) {
    let _ = hwnd;
}

/// Pack an RGB triple into the `COLORREF` (`0x00BBGGRR`) DWM expects.
#[cfg(windows)]
fn colorref([r, g, b]: [u8; 3]) -> u32 {
    u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
}

/// Show `path` in a File Explorer window with the item pre-selected.
///
/// Uses `SHOpenFolderAndSelectItems`, which is the only reliable way to select
/// an item (the legacy `/select,` switch races with the shell). For a path that
/// does not exist any more — a deleted file, a removed drive — it falls back to
/// `explorer.exe /select,"<path>"`, which at least opens the containing folder.
/// Never fails; problems are logged.
#[cfg(windows)]
pub fn reveal_in_explorer(path: &Path) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};

    let path_w = crate::wide::wide(&path.to_string_lossy());
    // SAFETY: `path_w` is a live NUL-terminated UTF-16 buffer.
    let item = unsafe { ILCreateFromPathW(PCWSTR(path_w.as_ptr())) };
    if !item.is_null() {
        // SAFETY: `item` was just allocated by `ILCreateFromPathW` and is
        // released with `ILFree` immediately after; `None` means "select only
        // the single item".
        let result = unsafe { SHOpenFolderAndSelectItems(item, None, 0) };
        // SAFETY: `item` is owned by us and not used afterwards.
        unsafe { ILFree(Some(item)) };
        match result {
            Ok(()) => return,
            Err(error) => log::debug!(
                "SHOpenFolderAndSelectItems({}) failed: {error}; falling back to explorer /select",
                path.display()
            ),
        }
    }

    explorer_select_fallback(path);
}

/// Non-Windows stub: no Explorer.
#[cfg(not(windows))]
pub fn reveal_in_explorer(path: &Path) {
    let _ = path;
}

/// `explorer.exe /select,"<path>"` fallback used by [`reveal_in_explorer`].
#[cfg(windows)]
fn explorer_select_fallback(path: &Path) {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // Explorer needs a backslash path; `/select,"C:\dir\file"` is required to
    // be quoted *inside* a single argument, so `raw_arg` is used rather than
    // letting Rust quote two separate arguments.
    let target = path.to_string_lossy().replace('/', r"\");
    let argument = format!(r#"/select,"{target}""#);
    if let Err(error) = Command::new("explorer.exe").raw_arg(argument).spawn() {
        log::warn!("could not launch explorer.exe: {error}");
    }
}

/// Open `path` with the shell's default handler for its type.
///
/// Returns an error when `ShellExecuteW` reports a failure (its return value is
/// an `HINSTANCE` that is only a success code when greater than 32).
#[cfg(windows)]
pub fn open_in_default_app(path: &Path) -> anyhow::Result<()> {
    shell_execute("open", &path.to_string_lossy())
}

/// Non-Windows stub: fall back to the caller's own launcher.
#[cfg(not(windows))]
pub fn open_in_default_app(path: &Path) -> anyhow::Result<()> {
    let _ = path;
    Err(anyhow::anyhow!("windows only"))
}

/// Open `url` in the user's default browser / handler.
#[cfg(windows)]
pub fn open_url(url: &str) -> anyhow::Result<()> {
    shell_execute("open", url)
}

/// Non-Windows stub: fall back to the caller's own launcher.
#[cfg(not(windows))]
pub fn open_url(url: &str) -> anyhow::Result<()> {
    let _ = url;
    Err(anyhow::anyhow!("windows only"))
}

/// Shared `ShellExecuteW` wrapper with the documented `> 32` success check.
#[cfg(windows)]
fn shell_execute(verb: &str, target: &str) -> anyhow::Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb_w = crate::wide::wide(verb);
    let target_w = crate::wide::wide(target);
    // SAFETY: both buffers are NUL-terminated and live across the call; there
    // is no parent window, no extra parameters and no working directory.
    let instance = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb_w.as_ptr()),
            PCWSTR(target_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = instance.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "ShellExecuteW({verb:?}, {target:?}) failed with code {code}"
        ))
    }
}

/// Flash the window's taskbar button to draw the user's attention (bounded:
/// three flashes, then it stops when the window becomes active).
#[cfg(windows)]
pub fn flash_window(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        FlashWindowEx, FLASHWINFO, FLASHW_ALL, FLASHW_TIMERNOFG,
    };

    if hwnd == 0 {
        return;
    }
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd: HWND(hwnd as *mut core::ffi::c_void),
        dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
        uCount: 3,
        dwTimeout: 0,
    };
    // SAFETY: `info` is fully initialised, `cbSize` is correct, and the struct
    // outlives the call.
    let _ = unsafe { FlashWindowEx(&info) };
}

/// Non-Windows stub: no taskbar.
#[cfg(not(windows))]
pub fn flash_window(hwnd: isize) {
    let _ = hwnd;
}

/// Restore (if minimised) and bring the window to the foreground.
///
/// Windows only honours this for the process that currently owns the
/// foreground window; a secondary instance should call
/// `AllowSetForegroundWindow` first (see
/// [`crate::single_instance::send_to_primary`]).
#[cfg(windows)]
pub fn foreground_window(hwnd: isize) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    if hwnd == 0 {
        return;
    }
    let window = HWND(hwnd as *mut core::ffi::c_void);
    // SAFETY: `window` is only used as an opaque handle for the duration of
    // these calls; a stale handle simply makes them fail.
    unsafe {
        if IsIconic(window).0 != 0 {
            let _ = ShowWindow(window, SW_RESTORE);
        }
        let _ = SetForegroundWindow(window);
    }
}

/// Non-Windows stub: no window manager integration.
#[cfg(not(windows))]
pub fn foreground_window(hwnd: isize) {
    let _ = hwnd;
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn colorref_packs_bgr() {
        assert_eq!(colorref([0x00, 0x00, 0x00]), 0x0000_0000);
        assert_eq!(colorref([0xff, 0x00, 0x00]), 0x0000_00ff);
        assert_eq!(colorref([0x00, 0xff, 0x00]), 0x0000_ff00);
        assert_eq!(colorref([0x00, 0x00, 0xff]), 0x00ff_0000);
        assert_eq!(colorref([0x12, 0x34, 0x56]), 0x0056_3412);
    }

    /// Smoke tests: a zero `HWND` must be ignored rather than crash the tests.
    #[test]
    fn null_hwnd_calls_are_no_ops() {
        set_dark_titlebar(0, true);
        set_caption_color(0, [1, 2, 3], [4, 5, 6], [7, 8, 9]);
        flash_window(0);
        foreground_window(0);
        set_app_user_model_id("MVP-Versatile-Player.test");
    }
}
