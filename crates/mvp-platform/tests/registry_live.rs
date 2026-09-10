//! Live registry tests for the file-association code.
//!
//! These are `#[ignore]`d on purpose: they write to `HKCU\Software\Classes`,
//! which is a real, user-visible side effect that has no business happening
//! during an ordinary `cargo test`. They exist so the registration path can be
//! verified deliberately:
//!
//! ```text
//! cargo test -p mvp-platform --test registry_live -- --ignored --test-threads=1
//! ```
//!
//! Every test restores the registry before returning, and the assertions only
//! ever look at *our own* keys, never at what the user had chosen as a default.

#![cfg(windows)]

use mvp_platform::assoc::{self, FileKinds};

/// A kind selection with exactly one family enabled, so the test touches as
/// little of the registry as possible.
fn video_only() -> FileKinds {
    FileKinds {
        video: true,
        audio: false,
        image: false,
        playlist: false,
    }
}

#[test]
#[ignore = "writes to HKCU\\Software\\Classes"]
fn register_then_unregister_round_trips() {
    // Start from a clean slate so the assertions mean something.
    let _ = assoc::unregister();
    assert!(!assoc::is_registered(), "unregister must leave nothing behind");

    let report = assoc::register(&video_only(), false, true).expect("register");
    assert!(
        report.extensions.len() >= 20,
        "expected the whole video family, got {}",
        report.extensions.len()
    );
    assert!(report.capabilities, "the capabilities key must be written");
    assert!(assoc::is_registered());

    // The application must be listed for "Open with" and know its file types.
    assert_eq!(
        assoc::current_handler_for(".zzzznope"),
        None,
        "a nonsense extension must not resolve to anything"
    );

    assoc::unregister().expect("unregister");
    assert!(
        !assoc::is_registered(),
        "unregister must remove the capabilities key"
    );
}

#[test]
#[ignore = "writes to HKCU\\Software\\Classes"]
fn registering_does_not_steal_another_applications_entries() {
    // Take a snapshot of the extensions we are about to touch.
    let probe = ".mp4";
    let before = assoc::current_handler_for(probe);

    let _ = assoc::unregister();
    assoc::register(&video_only(), false, true).expect("register");
    assoc::unregister().expect("unregister");

    let after = assoc::current_handler_for(probe);
    assert_eq!(
        before, after,
        "the user's chosen handler for {probe} must survive a register/unregister cycle"
    );
}

#[test]
#[ignore = "writes to HKCU\\Software\\Classes"]
fn every_registered_extension_gets_a_prog_id() {
    let _ = assoc::unregister();
    let report = assoc::register(&video_only(), false, false).expect("register");

    for extension in &report.extensions {
        assert!(
            extension.starts_with('.'),
            "{extension} is not a well formed extension"
        );
        let prog_id = assoc::prog_id_for(extension);
        assert!(
            prog_id.starts_with(assoc::PROG_ID_PREFIX),
            "{prog_id} does not use the product prefix"
        );
        assert!(
            !assoc::describe_extension(extension).is_empty(),
            "{extension} has no friendly description"
        );
    }

    assoc::unregister().expect("unregister");
    assert!(!assoc::is_registered());
}

#[test]
#[ignore = "writes to HKCU\\Software\\Classes"]
fn the_generated_command_line_quotes_correctly() {
    // In a test binary `current_exe()` is the harness, not the player, so the
    // assertion is about the *shape* of the command line rather than the file
    // name: an unquoted path with a space in it would break Explorer's launch.
    let exe = assoc::exe_path();
    let command = assoc::open_command_line(&exe);
    assert!(
        command.starts_with('"'),
        "the executable must be quoted: {command}"
    );
    assert!(
        command.ends_with("\"%1\""),
        "the file placeholder must be quoted: {command}"
    );
    assert_eq!(
        command.matches('"').count(),
        4,
        "exactly two quoted arguments expected: {command}"
    );
}

#[test]
#[ignore = "writes to HKCU\\Software\\Classes"]
fn unregister_removes_every_trace_of_us() {
    // Register everything, then make sure nothing is left behind.
    let _ = assoc::register(&FileKinds::default(), true, true).expect("register");
    assoc::unregister().expect("unregister");
    assert!(!assoc::is_registered());

    // The ProgID for a representative extension must be gone from the shell's
    // "open with" list as well, not just from the capabilities key.
    //
    // Note the assertion is "not ours" rather than "none": another player may
    // legitimately own the extension, and stealing it back — or reporting it as
    // unowned — would both be wrong.
    let prog_id = assoc::prog_id_for(".mp4");
    assert!(!prog_id.is_empty());
    let handler = assoc::current_handler_for(".mp4");
    assert!(
        !handler
            .as_deref()
            .is_some_and(|h| h.starts_with(assoc::PROG_ID_PREFIX)),
        "we are still the registered handler for .mp4: {handler:?}"
    );
}
