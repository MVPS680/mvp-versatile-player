//! Every value that ties this crate to a particular update service, in one
//! place.
//!
//! The player and the standalone updater both drive the same **MVP Update
//! Manager** public API. Nothing else in the crate hard-codes a host name, a
//! product id or a platform string — they all read these constants — so moving
//! to another service (or another product of the same service) is a one-file
//! change.

use std::path::PathBuf;

/// Base address of the public update API. No trailing slash; request paths are
/// appended with a leading `/`.
pub const API_BASE: &str = "https://updateman.mvpclub.cc/api/v1/public";

/// Product identifier as registered in the update manager.
///
/// The numeric id (`7`) identifies the same product and is what the API accepts
/// interchangeably, but the name survives a database reshape and is what the
/// service's own examples use, so it is the one we ship.
pub const PRODUCT: &str = "MVP-Versatile-Player";

/// Platform query value the service expects.
pub const PLATFORM: &str = "windows";

/// Architecture query value the service expects.
pub const ARCHITECTURE: &str = "x86_64";

/// Folder name under the roaming app-data directory that holds settings, the
/// update staging area and the update log. Matches the player's own
/// `settings::APP_NAME` so both halves agree on where things live.
pub const APP_NAME: &str = "MVP-Versatile-Player";

/// `User-Agent` sent with every request. Some CDNs reject a request with no
/// agent, and identifying ourselves makes the service's logs useful.
pub fn user_agent() -> String {
    format!(
        "{APP_NAME}/{} ({PLATFORM}; {ARCHITECTURE})",
        env!("CARGO_PKG_VERSION")
    )
}

/// The user's roaming app-data directory (`%APPDATA%`).
///
/// Read from the environment rather than a location helper so the crate needs
/// no extra dependency for the one directory it uses; on Windows this is
/// exactly `FOLDERID_RoamingAppData`, the same directory `dirs::config_dir`
/// resolves to for the player.
fn roaming_app_data() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `%APPDATA%\MVP-Versatile-Player` — where the player keeps its settings and
/// where the updater keeps its staging area and log.
pub fn app_data_dir() -> PathBuf {
    roaming_app_data().join(APP_NAME)
}

/// `%APPDATA%\MVP-Versatile-Player\update` — the working directory of the
/// external updater.
///
/// The updater runs from a *copy* here rather than from the install directory,
/// so the running `mvp-updater.exe` is never one of the files it is trying to
/// replace.
pub fn update_dir() -> PathBuf {
    app_data_dir().join("update")
}

/// Where a freshly downloaded package is written before it is opened.
pub fn download_dir() -> PathBuf {
    update_dir()
}

/// Where the package is unpacked and verified before anything is replaced.
pub fn staging_dir() -> PathBuf {
    update_dir().join("staging")
}

/// The persistent record of the last successful install, used as the fast path
/// of the content comparison.
pub fn install_manifest_path() -> PathBuf {
    update_dir().join("install.json")
}

/// The append-only updater log.
pub fn log_path() -> PathBuf {
    update_dir().join("updater.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_paths_live_under_app_data() {
        let update = update_dir();
        assert!(update.ends_with("update"));
        assert!(update.starts_with(app_data_dir()));
        assert!(staging_dir().starts_with(&update));
        assert!(install_manifest_path().starts_with(&update));
        assert!(log_path().starts_with(&update));
    }

    #[test]
    fn user_agent_carries_name_and_version() {
        let agent = user_agent();
        assert!(agent.starts_with(APP_NAME));
        assert!(agent.contains(env!("CARGO_PKG_VERSION")));
        assert!(agent.contains(PLATFORM));
    }
}
