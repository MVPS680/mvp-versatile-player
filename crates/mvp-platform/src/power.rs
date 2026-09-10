//! Display/system sleep inhibition during playback.
//!
//! Windows ties "keep the screen on" to the *thread* execution state, which is
//! why [`SleepBlocker`] must be driven from the same thread that should be
//! protected — in practice the UI thread, which is the thread that stays alive
//! for the whole session. Calling it from a short-lived worker thread would
//! silently lose the request when that thread exits.

use std::sync::atomic::{AtomicBool, Ordering};

/// Keeps the display (and the system) awake while the player is playing.
///
/// The request is process-wide once set: `ES_CONTINUOUS` tells Windows to keep
/// the state until it is cleared again. Create one per player (or one global)
/// and flip it from the UI thread as playback starts and stops.
///
/// Dropping the value does *not* restore the default state — call
/// [`SleepBlocker::set`] with `false`, or create it with `enable: false`, so
/// that the intent is explicit at the call site.
#[derive(Debug)]
pub struct SleepBlocker {
    enabled: AtomicBool,
}

impl SleepBlocker {
    /// Create a blocker and immediately apply `enable`.
    ///
    /// Pass `true` while media is playing and `false` when the player starts
    /// idle so the machine's normal power policy applies.
    pub fn new(enable: bool) -> Self {
        let blocker = Self {
            enabled: AtomicBool::new(false),
        };
        blocker.set(enable);
        blocker
    }

    /// Request (`true`) or release (`false`) the keep-awake state.
    ///
    /// Safe to call repeatedly and safe to call from any thread; only the
    /// thread that made the last successful call is protected by the Win32
    /// semantics described in the module docs.
    pub fn set(&self, enable: bool) {
        let applied = apply(enable);
        if applied {
            self.enabled.store(enable, Ordering::Relaxed);
        } else {
            log::warn!(
                "SetThreadExecutionState could not {} the display sleep block",
                if enable { "enable" } else { "disable" }
            );
        }
    }

    /// Whether the last successful call left the keep-awake request in place.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

/// Apply the execution-state change, reporting whether the OS accepted it.
#[cfg(windows)]
fn apply(enable: bool) -> bool {
    use windows::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    };

    let flags = if enable {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
    } else {
        // `ES_CONTINUOUS` alone clears the previous request.
        ES_CONTINUOUS
    };
    // SAFETY: `SetThreadExecutionState` has no pointer arguments and no
    // preconditions beyond a valid flag set.
    let previous = unsafe { SetThreadExecutionState(flags) };
    previous.0 != 0
}

/// Non-Windows stub: there is no execution-state API, so the flag is tracked
/// but nothing is sent to the OS.
#[cfg(not(windows))]
fn apply(enable: bool) -> bool {
    let _ = enable;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_the_requested_state() {
        let blocker = SleepBlocker::new(false);
        assert!(!blocker.is_enabled());

        blocker.set(true);
        assert!(blocker.is_enabled());

        // Idempotent: a second identical call must not flip anything.
        blocker.set(true);
        assert!(blocker.is_enabled());

        blocker.set(false);
        assert!(!blocker.is_enabled());
    }

    #[test]
    fn new_with_enable_starts_enabled() {
        let blocker = SleepBlocker::new(true);
        assert!(blocker.is_enabled());
        blocker.set(false);
        assert!(!blocker.is_enabled());
    }

    #[test]
    fn is_shareable_across_threads() {
        let blocker = SleepBlocker::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| blocker.set(true));
        });
        assert!(blocker.is_enabled());
    }
}
