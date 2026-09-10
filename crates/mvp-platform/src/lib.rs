//! Windows shell / OS integration for **MVP-Versatile-Player**.
//!
//! This crate isolates every piece of platform glue the player needs:
//!
//! * [`assoc`] — per-user (`HKEY_CURRENT_USER`) file associations, registry
//!   capabilities registration and the *Default apps* deep link. Registering
//!   never requires elevation.
//! * [`single_instance`] — a per-user single-instance guard backed by a named
//!   mutex plus a hidden top-level window used as a `WM_COPYDATA` IPC endpoint.
//! * [`shell`] — small Win32 niceties: AppUserModelID, dark title bar, caption
//!   tinting, "reveal in Explorer", `ShellExecuteW` helpers, taskbar flashing.
//! * [`power`] — [`power::SleepBlocker`] keeps the display awake during
//!   playback.
//!
//! The crate is deliberately portable: every Windows-specific item has a
//! `#[cfg(not(windows))]` counterpart that compiles and behaves as a no-op or
//! returns [`anyhow::Error`] `"windows only"`. That keeps `cargo check` usable
//! on other hosts without pulling the `windows` crate in.

pub mod assoc;
pub mod power;
pub mod shell;
pub mod single_instance;

#[cfg(windows)]
mod wide;

pub use assoc::{AssocReport, FileKinds};
pub use single_instance::{AppInstance, IpcMessage};
