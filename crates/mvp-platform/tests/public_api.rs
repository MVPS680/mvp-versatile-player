//! Compile-time contract for the public API of `mvp-platform`.
//!
//! This file deliberately does **not** execute anything that could touch the
//! registry, open a window or change power state: every item is only *named*
//! with its exact signature, so any accidental change to a public signature
//! fails the build instead of failing a downstream crate.

use std::path::{Path, PathBuf};

use mvp_platform::assoc::{self, AssocReport, FileKinds, APP_NAME, EXE_NAME, PROG_ID_PREFIX};
use mvp_platform::power::SleepBlocker;
use mvp_platform::shell;
use mvp_platform::single_instance::{self, AppInstance, IpcMessage};
// The crate-root re-exports, which is what most callers use.
use mvp_platform::{AssocReport as ReexportedReport, FileKinds as ReexportedKinds};

#[test]
fn constants_and_re_exports_are_stable() {
    let _: &str = APP_NAME;
    let _: &str = PROG_ID_PREFIX;
    let _: &str = EXE_NAME;
    assert_eq!(APP_NAME, "MVP-Versatile-Player");
    assert_eq!(PROG_ID_PREFIX, "MVPVersatilePlayer");
    assert_eq!(EXE_NAME, "mvp-versatile-player.exe");

    // Only compiles while the re-exports name the very same types.
    let _: Option<ReexportedReport> = None::<AssocReport>;
    let _: Option<ReexportedKinds> = None::<FileKinds>;
}

#[test]
fn assoc_signatures_are_stable() {
    let _: fn() -> PathBuf = assoc::exe_path;
    let _: fn() -> &'static [&'static str] = assoc::video_extensions;
    let _: fn() -> &'static [&'static str] = assoc::audio_extensions;
    let _: fn() -> &'static [&'static str] = assoc::image_extensions;
    let _: fn() -> &'static [&'static str] = assoc::playlist_extensions;
    let _: fn(&str) -> String = assoc::prog_id_for;
    let _: fn(&Path) -> String = assoc::open_command_line;
    let _: fn(&str) -> String = assoc::describe_extension;
    let _: fn(&FileKinds, bool, bool) -> anyhow::Result<AssocReport> = assoc::register;
    let _: fn() -> anyhow::Result<()> = assoc::unregister;
    let _: fn() -> bool = assoc::is_registered;
    let _: fn(&str) -> Option<String> = assoc::current_handler_for;
    let _: fn() -> anyhow::Result<()> = assoc::open_default_apps_settings;
    let _: fn() = assoc::refresh_shell;
}

#[test]
fn assoc_types_are_stable() {
    let kinds = FileKinds::default();
    let _: bool = kinds.video;
    let _: bool = kinds.audio;
    let _: bool = kinds.image;
    let _: bool = kinds.playlist;
    assert!(kinds.video && kinds.audio && kinds.image && kinds.playlist);

    // `FileKinds` must stay `Copy` + `Eq`.
    let copied: FileKinds = kinds;
    assert_eq!(copied, kinds);

    let report = AssocReport {
        extensions: vec![".mp4".to_string()],
        capabilities: true,
        direct_default_attempted: Vec::new(),
        requires_user_confirmation: true,
    };
    // `AssocReport` is `Clone`, not `Copy`.
    let cloned = report.clone();
    let _: Vec<String> = cloned.extensions;
    let _: bool = cloned.capabilities;
    let _: Vec<String> = cloned.direct_default_attempted;
    let _: bool = cloned.requires_user_confirmation;
}

#[test]
fn single_instance_signatures_are_stable() {
    let _: fn(&str) -> anyhow::Result<Option<AppInstance>> = AppInstance::acquire;
    let _: fn(&AppInstance) = AppInstance::shutdown;
    let _: fn(&AppInstance, fn(IpcMessage)) = AppInstance::set_handler::<fn(IpcMessage)>;
    let _: fn(&str, &IpcMessage) -> anyhow::Result<bool> = single_instance::send_to_primary;

    let messages = [
        IpcMessage::Activate,
        IpcMessage::OpenPaths(vec![PathBuf::from("a.mp4")]),
        IpcMessage::Quit,
    ];
    for message in &messages {
        assert_eq!(message.clone(), *message);
    }
    assert_ne!(messages[0], messages[2]);
}

#[test]
fn shell_signatures_are_stable() {
    let _: fn(&str) = shell::set_app_user_model_id;
    let _: fn(isize, bool) = shell::set_dark_titlebar;
    let _: fn(isize, [u8; 3], [u8; 3], [u8; 3]) = shell::set_caption_color;
    let _: fn(&Path) = shell::reveal_in_explorer;
    let _: fn(&Path) -> anyhow::Result<()> = shell::open_in_default_app;
    let _: fn(&str) -> anyhow::Result<()> = shell::open_url;
    let _: fn(isize) = shell::flash_window;
    let _: fn(isize) = shell::foreground_window;
}

#[test]
fn power_signatures_are_stable() {
    let _: fn(bool) -> SleepBlocker = SleepBlocker::new;
    let _: fn(&SleepBlocker, bool) = SleepBlocker::set;
    let _: fn(&SleepBlocker) -> bool = SleepBlocker::is_enabled;
}
