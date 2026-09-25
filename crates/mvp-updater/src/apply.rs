//! Applying a verified package: unpack, compare, replace atomically, restart.
//!
//! This is the half that only the external `mvp-updater.exe` runs. By the time
//! it is invoked the player has already exited, so nothing holds the install
//! directory open and the executable can be replaced like any other file.
//!
//! ## The swap is a journal, not a loop
//!
//! Each changed file is handled as two moves:
//!
//! ```text
//! target\name            -> target\name.old     (rename: keeps the old bytes)
//! staging\name           -> target\name         (rename: installs the new bytes)
//! ```
//!
//! Renames within a directory are atomic, so a file is never observed half
//! written. If any move fails, the entries already done are walked backwards —
//! the `.old` copies restored, the newly placed files removed — which returns
//! the install to exactly where it started. The `.old` files are only deleted
//! once every move has succeeded.
//!
//! ## Whole-package digest first
//!
//! When the caller supplies the expected SHA-256 (it always does when the
//! player downloaded the package) it is checked before anything is unpacked, so
//! a truncated or tampered download is rejected while the install is still
//! untouched.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use zip::ZipArchive;

use crate::config;
use crate::manifest::{self, Change, PackageEntry};
use crate::net;

/// Everything the apply step needs.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The install directory to update.
    pub target_dir: PathBuf,
    /// The downloaded package.
    pub package: PathBuf,
    /// The version being installed (recorded in `install.json`).
    pub version: String,
    /// Expected whole-package SHA-256, when known.
    pub expected_sha256: Option<String>,
    /// Player executable to relaunch once the swap is done.
    pub restart_exe: Option<PathBuf>,
}

/// What a run did.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// Files the package contains.
    pub entries: usize,
    /// Files actually written.
    pub installed: usize,
    /// Files already up to date.
    pub unchanged: usize,
    /// Executable relaunched, if any.
    pub restarted: Option<PathBuf>,
}

/// One file's place in the rollback journal.
struct Journal {
    name: String,
    /// Whether a `.old` copy exists to restore.
    had_backup: bool,
}

/// Run the whole apply step.
pub fn run(options: &RunOptions) -> Result<RunOutcome> {
    if let Some(expected) = &options.expected_sha256 {
        verify_package_sha256(&options.package, expected)?;
    }

    let staging = config::staging_dir();
    let entries = extract_all(&options.package, &staging)?;

    let cache = manifest::read_install_manifest(&config::install_manifest_path());
    let set = manifest::plan_changes(&entries, &options.target_dir, cache.as_ref())?;

    install_files(&set.changes, &staging, &options.target_dir)?;

    let record =
        manifest::build_install_manifest(&entries, &options.target_dir, &options.version);
    manifest::write_install_manifest(&config::install_manifest_path(), &record)?;

    // Nothing below this point is allowed to fail the update: the install is
    // already correct, so staging and the package are best-effort cleanups.
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_file(&options.package);

    let restarted = match &options.restart_exe {
        Some(exe) => {
            restart_player(exe, &options.target_dir)?;
            Some(exe.clone())
        }
        None => None,
    };

    Ok(RunOutcome {
        entries: entries.len(),
        installed: set.changes.len(),
        unchanged: set.unchanged,
        restarted,
    })
}

/// Hex SHA-256 of a file, streaming.
fn sha256_of_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("could not read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(net::to_hex(&hasher.finalize()))
}

/// Check a package against the digest the service published.
pub fn verify_package_sha256(package: &Path, expected: &str) -> Result<()> {
    let expected = expected.trim();
    if expected.is_empty() {
        return Ok(());
    }
    let actual = sha256_of_file(package)?;
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("the package digest does not match: expected {expected}, got {actual}");
    }
    Ok(())
}

/// Unpack every entry into `staging`, verifying each one's size and CRC32 as it
/// is written, and return the package's file list.
///
/// Everything is unpacked and validated before a single byte is written to the
/// install directory, so a package that fails verification leaves the install
/// untouched.
pub fn extract_all(package: &Path, staging: &Path) -> Result<Vec<PackageEntry>> {
    let file = File::open(package)
        .with_context(|| format!("could not open the package {}", package.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("{} is not a readable zip", package.display()))?;
    let entries = manifest::entries_from_archive(&mut archive)?;

    if staging.exists() {
        fs::remove_dir_all(staging)
            .with_context(|| format!("could not clear {}", staging.display()))?;
    }
    fs::create_dir_all(staging)
        .with_context(|| format!("could not create {}", staging.display()))?;

    for entry in &entries {
        let mut source = archive
            .by_name(&entry.name)
            .with_context(|| format!("could not read {} from the package", entry.name))?;
        let dest = staging.join(&entry.name);
        let mut out = File::create(&dest)
            .with_context(|| format!("could not create {}", dest.display()))?;

        let mut hasher = crc32fast::Hasher::new();
        let mut written: u64 = 0;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = source
                .read(&mut buffer)
                .with_context(|| format!("could not decompress {}", entry.name))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            out.write_all(&buffer[..read])
                .with_context(|| format!("could not write {}", dest.display()))?;
            written += read as u64;
        }
        out.flush()
            .with_context(|| format!("could not flush {}", dest.display()))?;

        let crc = hasher.finalize();
        if written != entry.size || crc != entry.crc32 {
            bail!(
                "the package entry {} failed verification (size {written}/{}, crc {crc:08x}/{:08x})",
                entry.name,
                entry.size,
                entry.crc32
            );
        }
    }
    Ok(entries)
}

/// The `.old` sibling used as the backup of `name` inside `target`.
fn backup_path(target: &Path, name: &str) -> PathBuf {
    target.join(format!("{name}.old"))
}

/// A `.new` sibling used when a cross-volume copy has to be turned into a
/// same-directory rename.
fn sibling_temp(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|part| part.to_string_lossy().into_owned())
        .unwrap_or_default();
    dest.with_file_name(format!("{name}.new"))
}

/// Move the staged file into place, falling back to copy-then-rename when the
/// staging directory and the install directory are on different volumes (a
/// `%APPDATA%` on `C:` updating an install on `D:`).
fn place_staged(staged: &Path, dest: &Path) -> Result<()> {
    if fs::rename(staged, dest).is_ok() {
        return Ok(());
    }
    let temp = sibling_temp(dest);
    fs::copy(staged, &temp)
        .with_context(|| format!("could not copy into {}", dest.display()))?;
    fs::rename(&temp, dest)
        .with_context(|| format!("could not move into place {}", dest.display()))?;
    let _ = fs::remove_file(staged);
    Ok(())
}

/// Walk the finished entries backwards, undoing each swap.
fn rollback(journal: &[Journal], target: &Path) {
    for entry in journal.iter().rev() {
        let dest = target.join(&entry.name);
        let backup = backup_path(target, &entry.name);
        let _ = fs::remove_file(&dest);
        if entry.had_backup {
            let _ = fs::rename(&backup, &dest);
        }
    }
}

/// Replace the changed files, backing each up first and rolling back entirely
/// on the first failure.
pub fn install_files(changes: &[Change], staging: &Path, target: &Path) -> Result<()> {
    let mut journal: Vec<Journal> = Vec::with_capacity(changes.len());
    for change in changes {
        let name = &change.entry.name;
        let staged = staging.join(name);
        let dest = target.join(name);
        let backup = backup_path(target, name);

        let had_backup = dest.exists();
        if had_backup {
            // Clear a stale backup so the rename has somewhere to go.
            let _ = fs::remove_file(&backup);
            fs::rename(&dest, &backup)
                .with_context(|| format!("could not back up {name}"))?;
        }

        match place_staged(&staged, &dest) {
            Ok(()) => journal.push(Journal {
                name: name.clone(),
                had_backup,
            }),
            Err(error) => {
                // Undo this file, then everything already swapped.
                if had_backup {
                    let _ = fs::remove_file(&dest);
                    let _ = fs::rename(&backup, &dest);
                }
                rollback(&journal, target);
                return Err(error).with_context(|| format!("could not install {name}"));
            }
        }
    }

    // Every move succeeded: the backups are no longer needed.
    for entry in &journal {
        if entry.had_backup {
            let _ = fs::remove_file(backup_path(target, &entry.name));
        }
    }
    Ok(())
}

/// Relaunch the player, using the install directory as the working directory.
#[cfg(windows)]
pub fn restart_player(exe: &Path, workdir: &Path) -> Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb = wide("open");
    let file = wide(&exe.to_string_lossy());
    let dir = wide(&workdir.to_string_lossy());
    // SAFETY: every buffer is NUL-terminated and outlives the call; there is no
    // parent window and no extra parameters.
    let instance = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR(dir.as_ptr()),
            SW_SHOWNORMAL,
        )
    };
    let code = instance.0 as isize;
    if code > 32 {
        Ok(())
    } else {
        bail!(
            "ShellExecuteW could not start {} (code {code})",
            exe.display()
        )
    }
}

/// Non-Windows stub: no shell.
#[cfg(not(windows))]
pub fn restart_player(exe: &Path, workdir: &Path) -> Result<()> {
    let _ = (exe, workdir);
    bail!("restarting is only supported on Windows")
}

/// Encode `s` as a NUL-terminated UTF-16 buffer for `PCWSTR`.
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ChangeKind;

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = File::create(path).unwrap();
        file.write_all(bytes).unwrap();
    }

    fn change(name: &str, kind: ChangeKind) -> Change {
        Change {
            entry: PackageEntry {
                name: name.to_string(),
                size: 0,
                crc32: 0,
            },
            kind,
        }
    }

    #[test]
    fn install_replaces_and_leaves_no_backup() {
        let root = manifest::unique_temp_dir("install-ok");
        let staging = root.join("staging");
        let target = root.join("target");
        write_file(&staging.join("a.txt"), b"new");
        write_file(&target.join("a.txt"), b"old");

        install_files(&[change("a.txt", ChangeKind::Changed)], &staging, &target).unwrap();

        assert_eq!(fs::read(target.join("a.txt")).unwrap(), b"new");
        assert!(!target.join("a.txt.old").exists());
    }

    #[test]
    fn install_failure_rolls_everything_back() {
        let root = manifest::unique_temp_dir("install-rollback");
        let staging = root.join("staging");
        let target = root.join("target");
        // First file is staged; the second is deliberately not, so placing it
        // fails after the first has already been swapped.
        write_file(&staging.join("a.txt"), b"new a");
        write_file(&target.join("a.txt"), b"old a");

        let error = install_files(
            &[
                change("a.txt", ChangeKind::Changed),
                change("b.txt", ChangeKind::New),
            ],
            &staging,
            &target,
        )
        .unwrap_err();
        assert!(error.to_string().contains("b.txt"), "unexpected error: {error}");

        // The first file is back to its original contents and its backup is
        // gone (it was consumed by the restore).
        assert_eq!(fs::read(target.join("a.txt")).unwrap(), b"old a");
        assert!(!target.join("a.txt.old").exists());
        assert!(!target.join("b.txt").exists());
    }

    #[test]
    fn install_rolls_back_a_brand_new_file() {
        let root = manifest::unique_temp_dir("install-new-rollback");
        let staging = root.join("staging");
        let target = root.join("target");
        write_file(&staging.join("a.txt"), b"brand new");
        fs::create_dir_all(&target).unwrap();

        install_files(
            &[
                change("a.txt", ChangeKind::New),
                change("missing.txt", ChangeKind::New),
            ],
            &staging,
            &target,
        )
        .unwrap_err();

        assert!(!target.join("a.txt").exists());
    }

    #[test]
    fn verify_package_digest_accepts_and_rejects() {
        let root = manifest::unique_temp_dir("digest");
        let pkg = root.join("pkg.bin");
        write_file(&pkg, b"the package bytes");
        let good = sha256_of_file(&pkg).unwrap();
        verify_package_sha256(&pkg, &good).unwrap();
        verify_package_sha256(&pkg, &good.to_uppercase()).unwrap();
        assert!(verify_package_sha256(&pkg, "deadbeef").is_err());
        // An empty expectation means "do not check".
        verify_package_sha256(&pkg, "  ").unwrap();
    }

    #[test]
    fn extract_then_install_round_trips_a_package() {
        let root = manifest::unique_temp_dir("extract");
        let package = root.join("pkg.zip");
        {
            let file = File::create(&package).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("player.exe", options).unwrap();
            writer.write_all(b"the new player").unwrap();
            writer.start_file("README.md", options).unwrap();
            writer.write_all(b"docs").unwrap();
            writer.finish().unwrap();
        }

        let staging = root.join("staging");
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();

        let entries = extract_all(&package, &staging).unwrap();
        assert_eq!(entries.len(), 2);
        let set = manifest::plan_changes(&entries, &target, None).unwrap();
        assert_eq!(set.changes.len(), 2);

        install_files(&set.changes, &staging, &target).unwrap();
        assert_eq!(fs::read(target.join("player.exe")).unwrap(), b"the new player");
        assert_eq!(fs::read(target.join("README.md")).unwrap(), b"docs");
    }
}
