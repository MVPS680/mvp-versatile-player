//! The release package's file list, the comparison against what is installed,
//! and the record of the last successful install.
//!
//! ## The zip *is* the manifest
//!
//! A zip's central directory already names every member and carries its
//! uncompressed size and CRC32. Rather than ship a second `manifest.json` that
//! could drift out of step with the bytes it describes, the package's own
//! directory is treated as the source of truth: if a file is in the archive it
//! is part of this release, and the archive already knows enough about it to
//! decide whether the installed copy needs replacing.
//!
//! ## How a file is classified
//!
//! For each entry, against `target-dir\<name>`:
//!
//! * missing locally → **install** (a new file);
//! * different size → **install** (cheap, decisive);
//! * same size → compare CRC32. The installed copy's CRC is taken from the
//!   [`InstallManifest`] cache when the cached `(size, mtime)` still matches,
//!   otherwise the file is read and hashed. Equal → **skip**, else **install**.
//!
//! Files that exist locally but are not in the package are left alone: a user
//! who dropped a `portable.txt` next to the executable keeps it.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

/// One file the package carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    /// File name; the package is flat, so this is a bare name.
    pub name: String,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// CRC32 of the uncompressed bytes.
    pub crc32: u32,
}

/// What is remembered about one installed file, so the next update can decide
/// whether it changed without re-reading it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// Size in bytes.
    pub size: u64,
    /// Last-write time, in seconds since the Unix epoch.
    pub mtime: u64,
    /// CRC32 of the contents.
    pub crc32: u32,
}

/// The record of the last successful install.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallManifest {
    /// Version that was installed.
    #[serde(default)]
    pub version: String,
    /// One entry per packaged file, keyed by name.
    #[serde(default)]
    pub files: BTreeMap<String, FileRecord>,
}

impl InstallManifest {
    /// Read the recorded state for `name`, if any.
    pub fn record(&self, name: &str) -> Option<&FileRecord> {
        self.files.get(name)
    }
}

/// A file that must be written, with the reason kept for logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The file to write.
    pub entry: PackageEntry,
    /// What triggered the write.
    pub kind: ChangeKind,
}

/// Why a file is in the change set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Not present in the install.
    New,
    /// Present but different.
    Changed,
}

/// The outcome of comparing a package against an install.
#[derive(Debug, Clone, Default)]
pub struct ChangeSet {
    /// Files to write, in package order.
    pub changes: Vec<Change>,
    /// Files already up to date.
    pub unchanged: usize,
}

impl ChangeSet {
    /// Whether nothing needs to be written.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// Reject any entry name that could escape the target directory.
///
/// The release package is flat (bare file names, all ASCII), so anything with a
/// directory separator, a `..`, or an absolute path is either a mistake or a
/// malicious archive. Refusing them outright is cheaper to reason about than
/// sanitising them.
pub fn is_safe_entry_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains(':')
        && !Path::new(name).is_absolute()
}

/// Read the file list from an open archive's central directory.
pub fn entries_from_archive<R: Read + Seek>(archive: &mut ZipArchive<R>) -> Result<Vec<PackageEntry>> {
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .with_context(|| format!("could not read package entry #{index}"))?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_string();
        if !is_safe_entry_name(&name) {
            bail!("the package contains an unsafe entry name: {name:?}");
        }
        entries.push(PackageEntry {
            name,
            size: file.size(),
            crc32: file.crc32(),
        });
    }
    Ok(entries)
}

/// Read the file list of a package on disk.
pub fn package_entries(package: &Path) -> Result<Vec<PackageEntry>> {
    let file = File::open(package)
        .with_context(|| format!("could not open the package {}", package.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("{} is not a readable zip", package.display()))?;
    entries_from_archive(&mut archive)
}

/// CRC32 of a whole file, read in chunks so a large DLL never lands in memory
/// in one piece.
pub fn crc32_of_file(path: &Path) -> Result<u32> {
    let mut file =
        File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let mut hasher = crc32fast::Hasher::new();
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
    Ok(hasher.finalize())
}

/// Last-write time of `metadata` in seconds since the Unix epoch (`0` when the
/// platform cannot say).
pub fn mtime_secs(metadata: &std::fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Compare a package's entries against the installed files.
///
/// `cache` is the previous install record; it is only ever used to skip reading
/// a file whose size and modification time are unchanged since we last hashed
/// it.
pub fn plan_changes(
    entries: &[PackageEntry],
    target_dir: &Path,
    cache: Option<&InstallManifest>,
) -> Result<ChangeSet> {
    let mut set = ChangeSet::default();
    for entry in entries {
        let local = target_dir.join(&entry.name);
        let metadata = match std::fs::metadata(&local) {
            Ok(metadata) => metadata,
            Err(_) => {
                set.changes.push(Change {
                    entry: entry.clone(),
                    kind: ChangeKind::New,
                });
                continue;
            }
        };

        if metadata.len() != entry.size {
            set.changes.push(Change {
                entry: entry.clone(),
                kind: ChangeKind::Changed,
            });
            continue;
        }

        // Same size: the contents decide. Prefer the cached digest when the file
        // has not been touched since it was recorded.
        let local_crc = match cache.and_then(|manifest| manifest.record(&entry.name)) {
            Some(record) if record.size == entry.size && record.mtime == mtime_secs(&metadata) => {
                record.crc32
            }
            _ => crc32_of_file(&local)?,
        };

        if local_crc == entry.crc32 {
            set.unchanged += 1;
        } else {
            set.changes.push(Change {
                entry: entry.clone(),
                kind: ChangeKind::Changed,
            });
        }
    }
    Ok(set)
}

/// Build the install record to write after a successful update.
///
/// Every packaged file is recorded — not just the ones that changed — so the
/// next run can take the fast path for the whole install.
pub fn build_install_manifest(
    entries: &[PackageEntry],
    target_dir: &Path,
    version: &str,
) -> InstallManifest {
    let mut manifest = InstallManifest {
        version: version.to_string(),
        files: BTreeMap::new(),
    };
    for entry in entries {
        let local = target_dir.join(&entry.name);
        let mtime = std::fs::metadata(&local)
            .map(|metadata| mtime_secs(&metadata))
            .unwrap_or(0);
        manifest.files.insert(
            entry.name.clone(),
            FileRecord {
                size: entry.size,
                mtime,
                crc32: entry.crc32,
            },
        );
    }
    manifest
}

/// Read the previous install record; a missing or unreadable file is simply "no
/// cache" rather than an error.
pub fn read_install_manifest(path: &Path) -> Option<InstallManifest> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write the install record, creating the directory if needed.
pub fn write_install_manifest(path: &Path, manifest: &InstallManifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(manifest).context("could not encode install.json")?;
    std::fs::write(path, text).with_context(|| format!("could not write {}", path.display()))
}

/// A per-run unique directory under the system temp folder.
#[cfg(test)]
pub(crate) fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "mvp-updater-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = File::create(path).unwrap();
        file.write_all(bytes).unwrap();
    }

    #[test]
    fn rejects_unsafe_entry_names() {
        for name in ["", "a/b", "a\\b", "..", "../evil", "..\\evil", "C:\\x", "/etc/passwd"] {
            assert!(!is_safe_entry_name(name), "{name:?} should be rejected");
        }
    }

    #[test]
    fn accepts_flat_ascii_names() {
        for name in [
            "mvp-versatile-player.exe",
            "avcodec-63.dll",
            "mvp-updater.exe",
            "README.md",
            "LICENSE-FFmpeg.txt",
        ] {
            assert!(is_safe_entry_name(name), "{name:?} should be accepted");
        }
    }

    #[test]
    fn plan_classifies_new_changed_and_unchanged() {
        let dir = unique_temp_dir("plan");
        let same = b"identical bytes";
        write_file(&dir.join("same.txt"), same);
        write_file(&dir.join("changed.txt"), b"old contents here");

        let crc = |bytes: &[u8]| crc32fast::hash(bytes);
        let entry = |name: &str, bytes: &[u8]| PackageEntry {
            name: name.to_string(),
            size: bytes.len() as u64,
            crc32: crc(bytes),
        };

        let entries = vec![
            entry("same.txt", same),
            entry("changed.txt", b"new contents!"),
            entry("new.txt", b"brand new"),
        ];
        let set = plan_changes(&entries, &dir, None).unwrap();

        let names: Vec<_> = set.changes.iter().map(|c| c.entry.name.clone()).collect();
        assert_eq!(names, vec!["changed.txt", "new.txt"]);
        assert_eq!(set.changes[0].kind, ChangeKind::Changed);
        assert_eq!(set.changes[1].kind, ChangeKind::New);
        assert_eq!(set.unchanged, 1);
    }

    #[test]
    fn cache_hit_skips_hashing() {
        let dir = unique_temp_dir("cache");
        // Local contents deliberately do NOT match the entry, so only trusting
        // the cache can make the planner call this file unchanged.
        write_file(&dir.join("lib.dll"), b"local on disk");
        let entry = PackageEntry {
            name: "lib.dll".to_string(),
            size: b"local on disk".len() as u64,
            crc32: 0xdead_beef,
        };
        let metadata = std::fs::metadata(dir.join("lib.dll")).unwrap();
        let mut manifest = InstallManifest::default();
        manifest.files.insert(
            "lib.dll".to_string(),
            FileRecord {
                size: entry.size,
                mtime: mtime_secs(&metadata),
                crc32: entry.crc32,
            },
        );
        let set = plan_changes(&[entry], &dir, Some(&manifest)).unwrap();
        assert!(set.is_empty());
        assert_eq!(set.unchanged, 1);
    }

    #[test]
    fn cache_miss_falls_back_to_hashing() {
        let dir = unique_temp_dir("cache-miss");
        let bytes = b"the real contents";
        write_file(&dir.join("lib.dll"), bytes);
        let entry = PackageEntry {
            name: "lib.dll".to_string(),
            size: bytes.len() as u64,
            crc32: crc32fast::hash(bytes),
        };
        // A stale record (different mtime) must not be trusted; the actual hash
        // still matches, so the file is unchanged.
        let mut manifest = InstallManifest::default();
        manifest.files.insert(
            "lib.dll".to_string(),
            FileRecord {
                size: entry.size,
                mtime: 1,
                crc32: 0,
            },
        );
        let set = plan_changes(&[entry], &dir, Some(&manifest)).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn install_manifest_round_trips() {
        let dir = unique_temp_dir("manifest");
        let path = dir.join("install.json");
        let mut manifest = InstallManifest {
            version: "1.0.3".to_string(),
            files: BTreeMap::new(),
        };
        manifest.files.insert(
            "a.exe".to_string(),
            FileRecord {
                size: 10,
                mtime: 1234,
                crc32: 0x1234_5678,
            },
        );
        write_install_manifest(&path, &manifest).unwrap();
        assert_eq!(read_install_manifest(&path), Some(manifest));
        // A corrupt file is "no cache", not an error.
        std::fs::write(&path, b"{ not json ").unwrap();
        assert_eq!(read_install_manifest(&path), None);
    }

    #[test]
    fn package_entries_reads_the_central_directory() {
        let dir = unique_temp_dir("zip");
        let path = dir.join("pkg.zip");
        {
            let file = File::create(&path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("mvp-versatile-player.exe", options).unwrap();
            writer.write_all(b"the executable").unwrap();
            writer.start_file("README.md", options).unwrap();
            writer.write_all(b"docs").unwrap();
            writer.finish().unwrap();
        }
        let mut entries = package_entries(&path).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "README.md");
        assert_eq!(entries[0].size, 4);
        assert_eq!(entries[0].crc32, crc32fast::hash(b"docs"));
        assert_eq!(entries[1].name, "mvp-versatile-player.exe");
        assert_eq!(entries[1].size, 14);
    }
}
