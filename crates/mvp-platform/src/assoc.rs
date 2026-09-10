//! Per-user file associations, registry capabilities and the *Default apps*
//! deep link.
//!
//! Everything in this module writes **only** under `HKEY_CURRENT_USER`, so
//! registering the player as a media handler never triggers a UAC prompt and
//! is always undoable by [`unregister`].
//!
//! Since Windows 8 the shell protects `Software\Microsoft\Windows\CurrentVersion\
//! Explorer\FileExts\<ext>\UserChoice`; a program may no longer silently take
//! over an existing association. We therefore do the two things that *are*
//! allowed and let the user finish the job if Windows insists:
//!
//! 1. advertise every extension through the `RegisteredApplications` /
//!    `Capabilities` mechanism (this is what makes the player appear in
//!    *Settings ▸ Apps ▸ Default apps*), and
//! 2. best-effort write the classic `Software\Classes\.<ext>` default value.
//!
//! [`AssocReport::requires_user_confirmation`] reports whether step 2 can be
//! trusted — it cannot on Windows 8 or newer, so the UI should offer
//! [`open_default_apps_settings`] as a follow-up.

use std::path::{Path, PathBuf};

/// Display name used for the application, the capabilities key and the
/// `RegisteredApplications` value name.
pub const APP_NAME: &str = "MVP-Versatile-Player";

/// Prefix for every ProgID we own: `MVPVersatilePlayer.<ext-without-dot>`.
pub const PROG_ID_PREFIX: &str = "MVPVersatilePlayer";

/// File name of the player executable (also the sub-key under
/// `Software\Classes\Applications`).
pub const EXE_NAME: &str = "mvp-versatile-player.exe";

/// Description advertised in the Windows capabilities store.
#[cfg(windows)]
const APPLICATION_DESCRIPTION: &str = "多功能音视频与图片播放器 (FFmpeg)";

/// Registry path of our capabilities key; also the value written to
/// `Software\RegisteredApplications`.
#[cfg(windows)]
const CAPABILITIES_KEY: &str = r"Software\MVPVersatilePlayer\Capabilities";

/// Context-menu key for *files*.
#[cfg(windows)]
const CONTEXT_MENU_FILE_KEY: &str = r"Software\Classes\*\shell\MVPVersatilePlayer";
/// Context-menu key for *directories*.
#[cfg(windows)]
const CONTEXT_MENU_DIR_KEY: &str = r"Software\Classes\Directory\shell\MVPVersatilePlayer";

/// Container extensions (video) that we register.
const VIDEO: &[&str] = &[
    ".mp4", ".m4v", ".mkv", ".webm", ".avi", ".mov", ".wmv", ".flv", ".f4v", ".ts", ".m2ts",
    ".mts", ".mpg", ".mpeg", ".vob", ".3gp", ".3g2", ".ogv", ".rm", ".rmvb", ".asf", ".divx",
    ".mxf", ".m2v", ".mpv", ".m1v", ".y4m", ".dav", ".dat", ".amv", ".m3u8",
];

/// Container extensions (audio) that we register.
const AUDIO: &[&str] = &[
    ".mp3", ".flac", ".aac", ".m4a", ".ogg", ".oga", ".opus", ".wav", ".wma", ".ac3", ".eac3",
    ".dts", ".ape", ".alac", ".amr", ".mka", ".mp2", ".mpc", ".wv", ".tta", ".aiff", ".aif",
    ".au", ".mid", ".midi", ".m4b", ".spx", ".caf",
];

/// Still-image extensions that we register.
const IMAGE: &[&str] = &[
    ".jpg", ".jpeg", ".jpe", ".jfif", ".png", ".bmp", ".gif", ".webp", ".tif", ".tiff", ".ico",
    ".tga", ".dds", ".ppm", ".pgm", ".pbm", ".pnm", ".qoi", ".avif", ".heic", ".heif", ".jxl",
    ".exr", ".hdr", ".svg", ".psd", ".raw", ".cr2", ".nef", ".arw", ".dng", ".orf", ".rw2",
    ".raf", ".pef", ".sr2",
];

/// Playlist extensions that we register.
const PLAYLIST: &[&str] = &[".m3u", ".m3u8", ".pls", ".xspf", ".wpl"];

/// Which media families to register.
///
/// The fields are independent so the settings UI can offer one switch per
/// family. Serialisation is enabled with the (default-on) `serde` feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct FileKinds {
    /// Register the video container list.
    pub video: bool,
    /// Register the audio container list.
    pub audio: bool,
    /// Register the still-image list.
    pub image: bool,
    /// Register the playlist list.
    pub playlist: bool,
}

impl Default for FileKinds {
    /// All families enabled — what a fresh install should do.
    fn default() -> Self {
        Self {
            video: true,
            audio: true,
            image: true,
            playlist: true,
        }
    }
}

impl FileKinds {
    /// Names of the families that are currently enabled, for logging/UI.
    pub fn enabled_families(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.video {
            out.push("video");
        }
        if self.audio {
            out.push("audio");
        }
        if self.image {
            out.push("image");
        }
        if self.playlist {
            out.push("playlist");
        }
        out
    }
}

/// Outcome of a call to [`register`].
#[derive(Debug, Clone)]
pub struct AssocReport {
    /// Every extension actually registered, e.g. `".mp4"`, deduplicated and in
    /// list order (video, audio, image, playlist).
    pub extensions: Vec<String>,
    /// `true` when the `Capabilities` + `RegisteredApplications` keys were
    /// written.
    pub capabilities: bool,
    /// Extensions where the plain `Software\Classes\.<ext>` default value was
    /// also written (empty unless `set_default` was requested).
    pub direct_default_attempted: Vec<String>,
    /// `true` on Windows 8+, where the shell owns `UserChoice` and may ignore
    /// the plain default we wrote; the user then has to confirm the change in
    /// *Default apps*.
    pub requires_user_confirmation: bool,
}

/// Video container extensions handled by the player.
pub fn video_extensions() -> &'static [&'static str] {
    VIDEO
}

/// Audio container extensions handled by the player.
pub fn audio_extensions() -> &'static [&'static str] {
    AUDIO
}

/// Still-image extensions handled by the player.
pub fn image_extensions() -> &'static [&'static str] {
    IMAGE
}

/// Playlist extensions handled by the player.
pub fn playlist_extensions() -> &'static [&'static str] {
    PLAYLIST
}

/// Path of the running executable, canonicalised when the filesystem agrees.
///
/// `canonicalize` on Windows yields a `\\?\`-prefixed *verbatim* path. Those
/// work for `CreateProcess` but are fragile inside shell command lines and
/// registry values, so the verbatim prefix is stripped again (`\\?\UNC\server\
/// share` becomes `\\server\share`). If `current_exe()` fails entirely we fall
/// back to the bare [`EXE_NAME`], which the shell resolves relative to the
/// current directory.
pub fn exe_path() -> PathBuf {
    match std::env::current_exe() {
        Ok(raw) => match std::fs::canonicalize(&raw) {
            Ok(canonical) => strip_verbatim_prefix(canonical),
            Err(_) => raw,
        },
        Err(_) => PathBuf::from(EXE_NAME),
    }
}

/// Turn a canonical Windows path back into its conventional (non-verbatim)
/// spelling.
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest.to_string())
    } else {
        path
    }
}

/// Normalise an extension to the canonical `".ext"` spelling used everywhere
/// in this module: trimmed, lowercase, exactly one leading dot.
///
/// `"MP4"`, `".MP4"` and `"mp4"` all normalise to `".mp4"`.
pub fn normalize_extension(ext: &str) -> String {
    let trimmed = ext.trim().trim_start_matches('.');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(".{}", trimmed.to_ascii_lowercase())
    }
}

/// ProgID we own for `ext`, e.g. `".mp4"` (or `"mp4"`) → `"MVPVersatilePlayer.mp4"`.
///
/// A dotless input is accepted and handled like the dotted spelling. An empty
/// or all-dot input yields the bare prefix, which callers should treat as
/// "nothing to register".
pub fn prog_id_for(ext: &str) -> String {
    let normalized = normalize_extension(ext);
    format!("{PROG_ID_PREFIX}{normalized}")
}

/// Command line stored under `shell\open\command` for both the `Applications`
/// entry and every ProgID.
///
/// The `%1` placeholder is what Explorer substitutes with the clicked file;
/// the quotes make paths with spaces work.
pub fn open_command_line(exe: &Path) -> String {
    format!("\"{}\" \"%1\"", exe.display())
}

/// Human-readable type label for `ext`, e.g. `".mp4"` → `"MP4 视频文件"`.
///
/// Unknown extensions still get a non-empty label built from their uppercase
/// spelling, so the registry never ends up with a blank `FriendlyTypeName`.
pub fn describe_extension(ext: &str) -> String {
    let normalized = normalize_extension(ext);
    let upper = normalized.trim_start_matches('.').to_ascii_uppercase();
    if upper.is_empty() {
        return "媒体文件".to_string();
    }
    format!("{upper} {}", family_label(&normalized))
}

/// Family suffix used by [`describe_extension`].
fn family_label(normalized_ext: &str) -> &'static str {
    if VIDEO.contains(&normalized_ext) {
        "视频文件"
    } else if AUDIO.contains(&normalized_ext) {
        "音频文件"
    } else if IMAGE.contains(&normalized_ext) {
        "图片文件"
    } else if PLAYLIST.contains(&normalized_ext) {
        "播放列表"
    } else {
        "媒体文件"
    }
}

/// Every extension this crate knows about, deduplicated, in registration order.
///
/// `.m3u8` deliberately appears in both the video and the playlist list; this
/// is where that overlap is collapsed.
pub fn all_extensions() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for list in [VIDEO, AUDIO, IMAGE, PLAYLIST] {
        for ext in list {
            if !out.contains(ext) {
                out.push(ext);
            }
        }
    }
    out
}

/// Extensions selected by `kinds`, deduplicated, in registration order.
#[cfg(windows)]
fn extensions_for(kinds: &FileKinds) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |list: &'static [&'static str]| {
        for ext in list {
            if !out.iter().any(|existing| existing == ext) {
                out.push((*ext).to_string());
            }
        }
    };
    if kinds.video {
        push(VIDEO);
    }
    if kinds.audio {
        push(AUDIO);
    }
    if kinds.image {
        push(IMAGE);
    }
    if kinds.playlist {
        push(PLAYLIST);
    }
    out
}

/// `Software\Classes\Applications\<EXE_NAME>`.
#[cfg(windows)]
fn applications_key() -> String {
    format!(r"Software\Classes\Applications\{EXE_NAME}")
}

/// `Software\Classes\<ProgID>`.
#[cfg(windows)]
fn prog_id_key(prog_id: &str) -> String {
    format!(r"Software\Classes\{prog_id}")
}

/// `Software\Classes\<.ext>`.
#[cfg(windows)]
fn ext_class_key(ext: &str) -> String {
    format!(r"Software\Classes\{ext}")
}

/// `Software\Classes\<.ext>\OpenWithProgids`.
#[cfg(windows)]
fn open_with_progids_key(ext: &str) -> String {
    format!(r"{}\OpenWithProgids", ext_class_key(ext))
}

/// `Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\<.ext>\UserChoice`.
#[cfg(windows)]
fn user_choice_key(ext: &str) -> String {
    format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{ext}\UserChoice")
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

/// Register the player for the requested media families.
///
/// Writes the `Applications` entry, one ProgID per extension,
/// `OpenWithProgids`, the `Capabilities` store and (optionally) the context-menu
/// verbs, then notifies the shell. When `set_default` is true the classic
/// `Software\Classes\.<ext>` default value is written too — best-effort, since
/// Windows 8+ may keep the user's `UserChoice` instead.
///
/// Returns a summary of what was written; the first failing registry write
/// aborts with context, leaving whatever was already written in place (call
/// [`unregister`] to clean up).
#[cfg(windows)]
pub fn register(
    kinds: &FileKinds,
    set_default: bool,
    add_context_menu: bool,
) -> anyhow::Result<AssocReport> {
    win::register(kinds, set_default, add_context_menu)
}

/// Non-Windows stub: file association registration is Windows-only.
#[cfg(not(windows))]
pub fn register(
    kinds: &FileKinds,
    set_default: bool,
    add_context_menu: bool,
) -> anyhow::Result<AssocReport> {
    let _ = (kinds, set_default, add_context_menu);
    Err(anyhow::anyhow!("windows only"))
}

/// Remove every registry entry [`register`] may have created.
///
/// Runs over *all* known extensions rather than only the currently selected
/// families, so switching families off and then uninstalling still leaves no
/// orphans. Shared containers such as `OpenWithProgids` are pruned only when we
/// were their last user, and a `.ext` default value is deleted only when it
/// still points at *our* ProgID.
#[cfg(windows)]
pub fn unregister() -> anyhow::Result<()> {
    win::unregister()
}

/// Non-Windows stub: file association registration is Windows-only.
#[cfg(not(windows))]
pub fn unregister() -> anyhow::Result<()> {
    Err(anyhow::anyhow!("windows only"))
}

/// Best-effort check whether the player is currently advertised as a handler.
///
/// Looks for our `RegisteredApplications` value pointing at our capabilities
/// key. Never fails and never panics — this is a UI hint, not a guarantee.
#[cfg(windows)]
pub fn is_registered() -> bool {
    win::is_registered()
}

/// Non-Windows stub: always `false`.
#[cfg(not(windows))]
pub fn is_registered() -> bool {
    false
}

/// ProgID currently handling `ext`, preferring the user's explicit choice.
///
/// Reads `...\FileExts\<ext>\UserChoice\ProgId` first (present whenever the
/// user picked a default through the shell) and falls back to the classic
/// `Software\Classes\<ext>` default value. Returns `None` when `ext` is empty
/// or nothing is registered for it. Read-only, never panics.
#[cfg(windows)]
pub fn current_handler_for(ext: &str) -> Option<String> {
    win::current_handler_for(ext)
}

/// Non-Windows stub: no handler is ever registered.
#[cfg(not(windows))]
pub fn current_handler_for(ext: &str) -> Option<String> {
    let _ = ext;
    None
}

/// Open *Settings ▸ Apps ▸ Default apps* filtered to this application.
///
/// The `ms-settings:defaultapps?registeredAppUser=` deep link is understood by
/// Windows 11 and recent Windows 10 builds; on older builds it degrades to the
/// generic Default apps page. Use this after [`register`] reported
/// [`AssocReport::requires_user_confirmation`].
#[cfg(windows)]
pub fn open_default_apps_settings() -> anyhow::Result<()> {
    crate::shell::open_url(&format!(
        "ms-settings:defaultapps?registeredAppUser={APP_NAME}"
    ))
}

/// Non-Windows stub: there is no *Default apps* settings page.
#[cfg(not(windows))]
pub fn open_default_apps_settings() -> anyhow::Result<()> {
    Err(anyhow::anyhow!("windows only"))
}

/// Tell the shell that associations changed so icons and *Open with* menus
/// refresh. Fire-and-forget; failures are not reportable.
#[cfg(windows)]
pub fn refresh_shell() {
    win::refresh_shell();
}

/// Non-Windows stub: no shell to notify.
#[cfg(not(windows))]
pub fn refresh_shell() {}

// ---------------------------------------------------------------------------
// Windows registry implementation (private)
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use super::{
        all_extensions, applications_key, describe_extension, exe_path, ext_class_key,
        extensions_for, open_command_line, open_with_progids_key, prog_id_for, prog_id_key,
        user_choice_key, AssocReport, FileKinds, APP_NAME, APPLICATION_DESCRIPTION,
        CAPABILITIES_KEY, CONTEXT_MENU_DIR_KEY, CONTEXT_MENU_FILE_KEY,
    };
    use crate::wide::{from_wide_bytes, wide, wide_bytes};

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR,
    };
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW,
        RegOpenKeyExW, RegQueryInfoKeyW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_READ, KEY_WRITE, REG_NONE, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ,
    };
    // `REG_SAM_FLAGS` is used only in the signature of `open_key`.
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

    /// Owned registry key handle that closes itself on drop.
    ///
    /// Predefined keys such as `HKEY_CURRENT_USER` must never be wrapped in
    /// this type, because closing them is undefined.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from `RegCreateKeyExW`/`RegOpenKeyExW`, so it
            // is a valid owned key handle, and this is the only place it is closed.
            let _ = unsafe { RegCloseKey(self.0) };
        }
    }

    /// Translate a registry status code into an `anyhow` error with context.
    fn reg_error(operation: &str, path: &str, status: WIN32_ERROR) -> anyhow::Error {
        anyhow::anyhow!("{operation} failed for {path}: Win32 error {}", status.0)
    }

    /// `true` when a status code only means "key or value absent".
    fn is_missing(status: WIN32_ERROR) -> bool {
        status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND
    }

    /// Create (or open) `sub` under `root`, requesting read+write access.
    fn create_key(root: HKEY, sub: &str) -> anyhow::Result<Key> {
        let sub_w = wide(sub);
        let mut handle = HKEY(std::ptr::null_mut());
        // SAFETY: `sub_w` is a live NUL-terminated UTF-16 buffer, `handle` is a
        // valid out-pointer, and no security attributes / class are needed.
        let status = unsafe {
            RegCreateKeyExW(
                root,
                PCWSTR(sub_w.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE | KEY_READ,
                None,
                &mut handle,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(reg_error("RegCreateKeyExW", sub, status));
        }
        Ok(Key(handle))
    }

    /// Open `sub` under `root` with `access`; `Ok(None)` when it does not exist.
    fn open_key(root: HKEY, sub: &str, access: REG_SAM_FLAGS) -> anyhow::Result<Option<Key>> {
        let sub_w = wide(sub);
        let mut handle = HKEY(std::ptr::null_mut());
        // SAFETY: `sub_w` is a live NUL-terminated UTF-16 buffer and `handle` is
        // a valid out-pointer.
        let status =
            unsafe { RegOpenKeyExW(root, PCWSTR(sub_w.as_ptr()), None, access, &mut handle) };
        if status == ERROR_SUCCESS {
            Ok(Some(Key(handle)))
        } else if is_missing(status) {
            Ok(None)
        } else {
            Err(reg_error("RegOpenKeyExW", sub, status))
        }
    }

    /// Open `sub` read-only; `Ok(None)` when it does not exist.
    fn open_key_read(root: HKEY, sub: &str) -> anyhow::Result<Option<Key>> {
        open_key(root, sub, KEY_READ)
    }

    /// Write `value` as a `REG_SZ` under `name` (`""` for the default value).
    fn set_string(key: &Key, name: &str, value: &str) -> anyhow::Result<()> {
        let name_w = wide(name);
        // `wide_bytes` already includes the terminating NUL, which is what the
        // registry wants the byte count to cover.
        let payload = wide_bytes(value);
        // SAFETY: `name_w` is NUL-terminated and `payload` outlives the call;
        // `key.0` is an open key handle owned by `key`.
        let status = unsafe {
            RegSetValueExW(
                key.0,
                PCWSTR(name_w.as_ptr()),
                None,
                REG_SZ,
                Some(&payload),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(reg_error("RegSetValueExW(REG_SZ)", name, status));
        }
        Ok(())
    }

    /// Write an empty `REG_NONE` value under `name` (what `OpenWithProgids`
    /// entries and `SupportedTypes` entries conventionally use).
    fn set_empty_none(key: &Key, name: &str) -> anyhow::Result<()> {
        let name_w = wide(name);
        let empty: [u8; 0] = [];
        // SAFETY: `name_w` is NUL-terminated; an empty slice is explicitly
        // allowed (`cbData == 0`), and `key.0` is a valid open handle.
        let status = unsafe {
            RegSetValueExW(key.0, PCWSTR(name_w.as_ptr()), None, REG_NONE, Some(&empty))
        };
        if status != ERROR_SUCCESS {
            return Err(reg_error("RegSetValueExW(REG_NONE)", name, status));
        }
        Ok(())
    }

    /// Write a `REG_SZ` default value (`name == ""`).
    fn set_default_string(key: &Key, value: &str) -> anyhow::Result<()> {
        set_string(key, "", value)
    }

    /// Delete a single value, tolerating a missing key or value.
    ///
    /// `prune` also removes the containing key when it becomes completely
    /// empty (no values, no sub-keys), which is how the shared
    /// `OpenWithProgids` containers are cleaned up without disturbing other
    /// applications that may still use them.
    fn delete_value(root: HKEY, sub: &str, name: &str, prune: bool) -> anyhow::Result<()> {
        // `RegDeleteValueW` needs `KEY_SET_VALUE`, which `KEY_WRITE` implies.
        let Some(key) = open_key(root, sub, KEY_READ | KEY_WRITE)? else {
            return Ok(());
        };
        let name_w = wide(name);
        // SAFETY: `name_w` is NUL-terminated and `key.0` is an open handle with
        // `KEY_WRITE` access.
        let status = unsafe { RegDeleteValueW(key.0, PCWSTR(name_w.as_ptr())) };
        if status != ERROR_SUCCESS && !is_missing(status) {
            return Err(reg_error("RegDeleteValueW", &format!("{sub}\\{name}"), status));
        }
        if !prune {
            return Ok(());
        }

        let mut subkey_count = 0u32;
        let mut value_count = 0u32;
        // SAFETY: both out-pointers are valid and the handle is still open.
        let status = unsafe {
            RegQueryInfoKeyW(
                key.0,
                None,
                None,
                None,
                Some(&mut subkey_count),
                None,
                None,
                Some(&mut value_count),
                None,
                None,
                None,
                None,
            )
        };
        if status != ERROR_SUCCESS || subkey_count != 0 || value_count != 0 {
            return Ok(());
        }
        drop(key);

        let sub_w = wide(sub);
        // SAFETY: the key is empty (verified above), so `RegDeleteKeyW` cannot
        // silently remove somebody else's data.
        let status = unsafe { RegDeleteKeyW(root, PCWSTR(sub_w.as_ptr())) };
        if status != ERROR_SUCCESS && !is_missing(status) {
            return Err(reg_error("RegDeleteKeyW", sub, status));
        }
        Ok(())
    }

    /// Delete a whole sub-tree, tolerating an absent key.
    fn delete_tree(root: HKEY, sub: &str) -> anyhow::Result<()> {
        let sub_w = wide(sub);
        // SAFETY: `sub_w` is NUL-terminated; `RegDeleteTreeW` only removes the
        // named sub-tree below the predefined root.
        let status = unsafe { RegDeleteTreeW(root, PCWSTR(sub_w.as_ptr())) };
        if status != ERROR_SUCCESS && !is_missing(status) {
            return Err(reg_error("RegDeleteTreeW", sub, status));
        }
        Ok(())
    }

    /// Read a `REG_SZ`-ish value, returning `None` when it is absent or empty.
    fn query_string(root: HKEY, sub: &str, name: &str) -> Option<String> {
        let key = open_key_read(root, sub).ok().flatten()?;
        let name_w = wide(name);

        let mut size = 0u32;
        // First call asks only for the size.
        // SAFETY: `name_w` is NUL-terminated, `size` is a valid out-pointer and
        // all data pointers are null.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name_w.as_ptr()),
                None,
                None,
                None,
                Some(&mut size),
            )
        };
        if status != ERROR_SUCCESS || size == 0 {
            return None;
        }

        let mut buffer = vec![0u8; size as usize];
        let mut written = size;
        // SAFETY: `buffer` is at least `written` bytes long and the handle is open.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name_w.as_ptr()),
                None,
                None,
                Some(buffer.as_mut_ptr()),
                Some(&mut written),
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        buffer.truncate(written as usize);

        let text = from_wide_bytes(&buffer);
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// `true` when the shell protects `UserChoice`, i.e. on Windows 8+.
    ///
    /// `GetVersionExW` lies without an app manifest, so the version is read
    /// straight from `ntdll!RtlGetVersion`.
    fn user_choice_is_protected() -> bool {
        #[link(name = "ntdll")]
        extern "system" {
            fn RtlGetVersion(version_information: *mut OSVERSIONINFOW) -> i32;
        }

        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..Default::default()
        };
        // SAFETY: `info` is a correctly sized, properly initialised
        // `OSVERSIONINFOW`, which is exactly what `RtlGetVersion` writes into.
        let status = unsafe { RtlGetVersion(&mut info) };
        if status < 0 {
            // Unknown version: assume the modern, protected behaviour so the UI
            // offers the *Default apps* shortcut.
            return true;
        }
        // NT 6.2 == Windows 8.
        info.dwMajorVersion > 6 || (info.dwMajorVersion == 6 && info.dwMinorVersion >= 2)
    }

    /// See [`super::register`].
    pub(super) fn register(
        kinds: &FileKinds,
        set_default: bool,
        add_context_menu: bool,
    ) -> anyhow::Result<AssocReport> {
        let exe = exe_path();
        let exe_text = exe.display().to_string();
        let icon = format!("\"{exe_text}\",0");
        let command = open_command_line(&exe);
        let extensions = extensions_for(kinds);

        log::info!(
            "registering {} file association(s) for {} (default={set_default}, context_menu={add_context_menu})",
            extensions.len(),
            APP_NAME
        );

        write_application_entry(&icon, &command, &extensions)?;
        let mut direct_default_attempted = Vec::new();
        for ext in &extensions {
            write_prog_id(ext, &icon, &command)?;
            if set_default {
                write_direct_default(ext)?;
                direct_default_attempted.push(ext.clone());
            }
        }
        write_capabilities(&icon, &extensions)?;
        if add_context_menu {
            write_context_menu(&icon, &command)?;
        }

        super::refresh_shell();

        Ok(AssocReport {
            extensions,
            capabilities: true,
            direct_default_attempted,
            requires_user_confirmation: user_choice_is_protected(),
        })
    }

    /// Step 1: the `Applications\<exe>` entry Explorer uses to describe us.
    fn write_application_entry(
        icon: &str,
        command: &str,
        extensions: &[String],
    ) -> anyhow::Result<()> {
        let base = applications_key();
        let app = create_key(HKEY_CURRENT_USER, &base)?;
        set_string(&app, "FriendlyAppName", APP_NAME)?;
        set_string(&app, "ApplicationIcon", icon)?;

        let open_command = create_key(HKEY_CURRENT_USER, &format!(r"{base}\shell\open\command"))?;
        set_default_string(&open_command, command)?;

        let supported = create_key(HKEY_CURRENT_USER, &format!(r"{base}\SupportedTypes"))?;
        for ext in extensions {
            set_string(&supported, ext, "")?;
        }
        Ok(())
    }

    /// Step 2a: the ProgID describing one extension.
    fn write_prog_id(ext: &str, icon: &str, command: &str) -> anyhow::Result<()> {
        let prog_id = prog_id_for(ext);
        let label = describe_extension(ext);
        let base = prog_id_key(&prog_id);

        let key = create_key(HKEY_CURRENT_USER, &base)?;
        set_default_string(&key, &label)?;
        set_string(&key, "FriendlyTypeName", &label)?;

        let icon_key = create_key(HKEY_CURRENT_USER, &format!(r"{base}\DefaultIcon"))?;
        set_default_string(&icon_key, icon)?;

        let open_key = create_key(HKEY_CURRENT_USER, &format!(r"{base}\shell\open"))?;
        set_string(&open_key, "FriendlyAppName", APP_NAME)?;

        let command_key = create_key(HKEY_CURRENT_USER, &format!(r"{base}\shell\open\command"))?;
        set_default_string(&command_key, command)?;

        let open_with = create_key(HKEY_CURRENT_USER, &open_with_progids_key(ext))?;
        set_empty_none(&open_with, &prog_id)?;
        Ok(())
    }

    /// Step 2b: the classic `.ext` default value (best-effort on Win8+).
    fn write_direct_default(ext: &str) -> anyhow::Result<()> {
        let key = create_key(HKEY_CURRENT_USER, &ext_class_key(ext))?;
        set_default_string(&key, &prog_id_for(ext))
    }

    /// Step 3: the capabilities store plus its `RegisteredApplications` entry.
    fn write_capabilities(icon: &str, extensions: &[String]) -> anyhow::Result<()> {
        let caps = create_key(HKEY_CURRENT_USER, CAPABILITIES_KEY)?;
        set_default_string(&caps, APP_NAME)?;
        set_string(&caps, "ApplicationName", APP_NAME)?;
        set_string(&caps, "ApplicationDescription", APPLICATION_DESCRIPTION)?;
        set_string(&caps, "ApplicationIcon", icon)?;

        let associations =
            create_key(HKEY_CURRENT_USER, &format!(r"{CAPABILITIES_KEY}\FileAssociations"))?;
        for ext in extensions {
            set_string(&associations, ext, &prog_id_for(ext))?;
        }

        let registered = create_key(HKEY_CURRENT_USER, r"Software\RegisteredApplications")?;
        set_string(&registered, APP_NAME, CAPABILITIES_KEY)?;
        Ok(())
    }

    /// Step 4: "Open with MVP-Versatile-Player" for files and folders.
    fn write_context_menu(icon: &str, command: &str) -> anyhow::Result<()> {
        let files = create_key(HKEY_CURRENT_USER, CONTEXT_MENU_FILE_KEY)?;
        set_default_string(&files, "用 MVP-Versatile-Player 打开")?;
        set_string(&files, "Icon", icon)?;
        // `Player` tells Explorer the verb accepts multi-selection, which
        // suppresses the old "only the first file will be opened" behaviour.
        set_string(&files, "MultiSelectModel", "Player")?;
        let files_command = create_key(
            HKEY_CURRENT_USER,
            &format!(r"{CONTEXT_MENU_FILE_KEY}\command"),
        )?;
        set_default_string(&files_command, command)?;

        let dirs = create_key(HKEY_CURRENT_USER, CONTEXT_MENU_DIR_KEY)?;
        set_default_string(&dirs, "用 MVP-Versatile-Player 播放文件夹")?;
        set_string(&dirs, "Icon", icon)?;
        let dirs_command =
            create_key(HKEY_CURRENT_USER, &format!(r"{CONTEXT_MENU_DIR_KEY}\command"))?;
        set_default_string(&dirs_command, command)?;
        Ok(())
    }

    /// See [`super::unregister`].
    pub(super) fn unregister() -> anyhow::Result<()> {
        let mut removed = 0usize;

        // 1. Applications entry.
        delete_tree(HKEY_CURRENT_USER, &applications_key())?;

        // 2. Every ProgID we could ever have written, plus the shared
        //    `OpenWithProgids` value and the `.ext` default when it is ours.
        for ext in all_extensions() {
            let prog_id = prog_id_for(ext);
            delete_tree(HKEY_CURRENT_USER, &prog_id_key(&prog_id))?;

            let open_with = open_with_progids_key(ext);
            delete_value(HKEY_CURRENT_USER, &open_with, &prog_id, true)?;

            let ext_key = ext_class_key(ext);
            if query_string(HKEY_CURRENT_USER, &ext_key, "").as_deref() == Some(prog_id.as_str()) {
                delete_value(HKEY_CURRENT_USER, &ext_key, "", true)?;
            }
            removed += 1;
        }

        // 3. Capabilities + the RegisteredApplications pointer.
        delete_tree(HKEY_CURRENT_USER, r"Software\MVPVersatilePlayer")?;
        delete_value(
            HKEY_CURRENT_USER,
            r"Software\RegisteredApplications",
            APP_NAME,
            false,
        )?;

        // 4. Context-menu verbs.
        delete_tree(HKEY_CURRENT_USER, CONTEXT_MENU_FILE_KEY)?;
        delete_tree(HKEY_CURRENT_USER, CONTEXT_MENU_DIR_KEY)?;

        super::refresh_shell();
        log::info!("removed file associations for {removed} extension(s)");
        Ok(())
    }

    /// See [`super::is_registered`].
    pub(super) fn is_registered() -> bool {
        query_string(HKEY_CURRENT_USER, r"Software\RegisteredApplications", APP_NAME)
            .is_some_and(|value| value.eq_ignore_ascii_case(CAPABILITIES_KEY))
    }

    /// See [`super::current_handler_for`].
    pub(super) fn current_handler_for(ext: &str) -> Option<String> {
        let normalized = super::normalize_extension(ext);
        // Reject anything that is not a plain alphanumeric extension: an empty
        // name, embedded NULs or separator characters would otherwise be fed
        // straight into a registry key path.
        if normalized.len() < 2
            || !normalized[1..].chars().all(|c| c.is_ascii_alphanumeric())
        {
            return None;
        }
        if let Some(prog_id) =
            query_string(HKEY_CURRENT_USER, &user_choice_key(&normalized), "ProgId")
        {
            return Some(prog_id);
        }
        query_string(HKEY_CURRENT_USER, &ext_class_key(&normalized), "")
    }

    /// See [`super::refresh_shell`].
    pub(super) fn refresh_shell() {
        use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
        // SAFETY: `SHCNF_IDLIST` explicitly means both item pointers are unused,
        // so passing nulls is correct here.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    /// The four public lists, in registration order.
    fn lists() -> [(&'static str, &'static [&'static str]); 4] {
        [
            ("video", video_extensions()),
            ("audio", audio_extensions()),
            ("image", image_extensions()),
            ("playlist", playlist_extensions()),
        ]
    }

    #[test]
    fn every_extension_is_canonical() {
        for (family, list) in lists() {
            assert!(!list.is_empty(), "{family} list is empty");
            for ext in list {
                assert!(
                    ext.starts_with('.') && ext.len() > 1,
                    "{family}: {ext:?} must start with a dot"
                );
                assert_eq!(
                    *ext,
                    ext.to_ascii_lowercase(),
                    "{family}: {ext:?} must be lowercase"
                );
                assert_eq!(
                    normalize_extension(ext),
                    *ext,
                    "{family}: {ext:?} is not in normalised form"
                );
            }
        }
    }

    #[test]
    fn no_duplicates_within_or_across_families() {
        for (family, list) in lists() {
            let unique: HashSet<&&str> = list.iter().collect();
            assert_eq!(unique.len(), list.len(), "{family} list has duplicates");
        }

        // `.m3u8` is intentionally in both video and playlist; the combined
        // list must collapse it.
        let all = all_extensions();
        let unique: HashSet<&str> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len(), "all_extensions() has duplicates");
        assert!(all.contains(&".m3u8"));
        assert_eq!(
            all.iter().filter(|ext| **ext == ".m3u8").count(),
            1,
            "overlapping extensions must be collapsed"
        );
    }

    #[test]
    fn required_extensions_are_present() {
        for ext in [".mp4", ".mkv", ".webm", ".m3u8", ".dat"] {
            assert!(video_extensions().contains(&ext), "video is missing {ext}");
        }
        for ext in [".mp3", ".flac", ".opus", ".caf"] {
            assert!(audio_extensions().contains(&ext), "audio is missing {ext}");
        }
        for ext in [".jpg", ".avif", ".heic", ".sr2"] {
            assert!(image_extensions().contains(&ext), "image is missing {ext}");
        }
        for ext in [".m3u", ".pls", ".xspf", ".wpl"] {
            assert!(
                playlist_extensions().contains(&ext),
                "playlist is missing {ext}"
            );
        }
    }

    #[test]
    fn prog_id_uses_the_documented_prefix() {
        assert_eq!(prog_id_for(".mp4"), "MVPVersatilePlayer.mp4");
        assert_eq!(prog_id_for("mp4"), "MVPVersatilePlayer.mp4");
        assert_eq!(prog_id_for(".MP4"), "MVPVersatilePlayer.mp4");
        assert_eq!(prog_id_for("  .Mkv "), "MVPVersatilePlayer.mkv");
        for ext in all_extensions() {
            assert_eq!(prog_id_for(ext), format!("{PROG_ID_PREFIX}{ext}"));
        }
    }

    #[test]
    fn open_command_line_quotes_the_exe_and_placeholder() {
        let exe = Path::new(r"C:\Program Files\MVP Player\mvp-versatile-player.exe");
        let line = open_command_line(exe);
        assert_eq!(
            line,
            r#""C:\Program Files\MVP Player\mvp-versatile-player.exe" "%1""#
        );
        assert!(line.starts_with('"'));
        assert!(line.ends_with(r#""%1""#));
    }

    #[test]
    fn describe_extension_is_non_empty_and_distinct() {
        let samples = [".mp4", ".mkv", ".mp3", ".flac", ".jpg", ".png", ".m3u", ".zzz"];
        let mut seen = HashSet::new();
        for ext in samples {
            let label = describe_extension(ext);
            assert!(!label.trim().is_empty(), "{ext} produced an empty label");
            assert!(seen.insert(label.clone()), "{ext} reused the label {label:?}");
        }
        assert_eq!(describe_extension(".mp4"), "MP4 视频文件");
        assert_eq!(describe_extension("MP4"), "MP4 视频文件");
        assert_eq!(describe_extension(".mp3"), "MP3 音频文件");
        assert_eq!(describe_extension(".JPG"), "JPG 图片文件");
        assert_eq!(describe_extension(".m3u"), "M3U 播放列表");
        assert!(!describe_extension("").is_empty());
    }

    #[test]
    fn exe_path_is_absolute_and_not_verbatim() {
        let path = exe_path();
        assert!(!path.as_os_str().is_empty());
        let text = path.to_string_lossy();
        assert!(
            !text.starts_with(r"\\?\"),
            "verbatim prefix should be stripped: {text}"
        );
        if let Ok(current) = std::env::current_exe() {
            assert!(current.exists());
        }
    }

    #[test]
    fn file_kinds_default_enables_everything() {
        let kinds = FileKinds::default();
        assert!(kinds.video && kinds.audio && kinds.image && kinds.playlist);
        assert_eq!(
            kinds.enabled_families(),
            vec!["video", "audio", "image", "playlist"]
        );
        assert!(!FileKinds {
            video: false,
            ..FileKinds::default()
        }
        .enabled_families()
        .contains(&"video"));
    }

    /// Read-only probe: this touches the registry but never writes to it, and
    /// an unknown extension cannot exist in a normal installation.
    #[test]
    fn current_handler_for_unknown_extension_is_none_and_never_panics() {
        assert_eq!(current_handler_for(".zzzznope"), None);
        assert_eq!(current_handler_for("zzzznope"), None);
        assert_eq!(current_handler_for(""), None);
        assert_eq!(current_handler_for("..."), None);
        assert_eq!(current_handler_for("\0"), None);
    }
}
