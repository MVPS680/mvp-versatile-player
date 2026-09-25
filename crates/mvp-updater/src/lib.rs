//! External updater for **MVP-Versatile-Player**.
//!
//! The player ships with two halves of one update mechanism:
//!
//! * the **player** links this crate with the default `check` feature: it asks
//!   [`api::fetch_latest`], shows a dialog, downloads and verifies the package
//!   ([`net::download_to_file`]) and hands off to the external binary;
//! * **`mvp-updater.exe`** — this crate's binary, built with the `apply` feature
//!   — waits for the player to exit, unpacks the package, replaces only the
//!   files that actually changed and restarts the player.
//!
//! ## Why an external updater at all
//!
//! A process cannot replace the executable it is running. Splitting the *check*
//! (which needs the UI) from the *apply* (which needs the game files to be idle)
//! lets the update run to completion while the player is fully shut down, and
//! lets the swap be a sequence of atomic file renames that either all land or
//! are rolled back — never a half-updated install.
//!
//! ## Decisions worth remembering
//!
//! * **No `manifest.json`.** The release package is a flat zip whose central
//!   directory already lists every file, its size and its CRC32. Re-deriving a
//!   manifest would be a second source of truth that could disagree with the
//!   bytes; the zip *is* the manifest.
//! * **Content comparison, not whole-package replacement.** Day to day only the
//!   player executable differs between releases; the ~120 MB of FFmpeg DLLs do
//!   not. The updater therefore unpacks, compares each entry against the
//!   installed file and writes back only what changed.
//! * **Two levels of integrity.** The whole package is checked against the
//!   service's SHA-256 before anything is unpacked; the zip's own CRC32 per
//!   entry is checked during unpacking. Neither alone is enough: the digest
//!   guards the download, the CRC guards the decompression.
//! * **Atomic replace with rollback.** Each file is renamed to `*.old` and the
//!   staged copy renamed into place. Any failure walks the journal backwards.
//! * **`404` is silence, not an error.** Before the first release the service
//!   answers `404`; the client treats that as "already up to date".
//!
//! The crate is Windows-only in effect (the transport is WinHTTP); on other
//! hosts the transport functions return an error so `cargo check` still works.

pub mod config;

#[cfg(feature = "check")]
pub mod api;
#[cfg(feature = "check")]
pub mod net;
#[cfg(feature = "check")]
pub mod version;

#[cfg(feature = "apply")]
pub mod apply;
#[cfg(feature = "apply")]
pub mod manifest;

#[cfg(feature = "check")]
pub use api::UpdateInfo;
