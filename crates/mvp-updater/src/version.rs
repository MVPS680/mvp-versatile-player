//! Semantic-version handling for the update comparison.
//!
//! The update service publishes an `update_available` flag, but the client must
//! be able to reach the same conclusion on its own: a missing flag, an older
//! service build or a cache in the way should never make the player advertise an
//! update that is not there — or worse, offer to *downgrade*. So the version
//! strings are compared numerically here as a fallback.
//!
//! Only the three leading dotted numbers matter (`major.minor.patch`). The known
//! scheme is plain semantic versions such as `1.0.2` with no `v` prefix, which
//! matches `CARGO_PKG_VERSION`; a leading `v` is tolerated anyway. Any
//! pre-release or build-metadata suffix (`-beta`, `+build`) is ignored for the
//! comparison, which is the safe direction: it can only ever make the client
//! *less* eager to update.

use std::cmp::Ordering;

/// A parsed `major.minor.patch` triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    /// Leading number.
    pub major: u64,
    /// Second number.
    pub minor: u64,
    /// Third number.
    pub patch: u64,
}

impl Version {
    /// Parse `1.0.10`, `v1.0.2`, `1.2` (missing parts default to `0`).
    ///
    /// Returns `None` only when the leading component is not a number, which is
    /// the "this is not a version I understand" case the caller treats as
    /// "cannot compare".
    pub fn parse(text: &str) -> Option<Version> {
        let trimmed = text.trim();
        let trimmed = trimmed.strip_prefix('v').unwrap_or(trimmed);
        // Cut off any pre-release (`-`) or build-metadata (`+`) suffix.
        let core = trimmed.split(['-', '+']).next().unwrap_or("");
        let mut parts = core.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let patch = parts.next().unwrap_or("0").parse().unwrap_or(0);
        Some(Version {
            major,
            minor,
            patch,
        })
    }

    /// Whether this version is strictly newer than `other`, comparing each
    /// numeric segment in turn so that `1.0.10 > 1.0.9` (a lexicographic string
    /// compare would get that backwards).
    pub fn is_newer_than(&self, other: &Version) -> bool {
        self.cmp_numeric(other) == Ordering::Greater
    }

    /// Numeric ordering of the three segments.
    pub fn cmp_numeric(&self, other: &Version) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Decide whether `remote` should be offered over `local`.
///
/// `None` means "the local string is not something we can compare against", in
/// which case the caller falls back to whatever the service said (or to "no
/// update") rather than guessing.
pub fn remote_is_newer(remote: &str, local: &str) -> Option<bool> {
    let remote = Version::parse(remote)?;
    let local = Version::parse(local)?;
    Some(remote.is_newer_than(&local))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_prefixed_versions() {
        assert_eq!(
            Version::parse("1.0.2"),
            Some(Version {
                major: 1,
                minor: 0,
                patch: 2
            })
        );
        assert_eq!(Version::parse("v2.3.4"), Version::parse("2.3.4"));
        assert_eq!(
            Version::parse("1.2"),
            Some(Version {
                major: 1,
                minor: 2,
                patch: 0
            })
        );
        // Suffixes are ignored rather than rejected.
        assert_eq!(Version::parse("1.2.3-beta.1"), Version::parse("1.2.3"));
        assert_eq!(Version::parse("1.2.3+build7"), Version::parse("1.2.3"));
    }

    #[test]
    fn rejects_non_versions() {
        assert_eq!(Version::parse(""), None);
        assert_eq!(Version::parse("latest"), None);
        assert_eq!(Version::parse("x.y.z"), None);
    }

    #[test]
    fn numeric_segments_beat_lexicographic_order() {
        // The whole reason for parsing instead of comparing strings.
        assert_eq!(remote_is_newer("1.0.10", "1.0.9"), Some(true));
        assert_eq!(remote_is_newer("1.0.9", "1.0.10"), Some(false));
        assert_eq!(remote_is_newer("0.9.9", "1.0.0"), Some(false));
        assert_eq!(remote_is_newer("2.0.0", "1.99.99"), Some(true));
    }

    #[test]
    fn equal_versions_are_not_newer() {
        assert_eq!(remote_is_newer("1.0.2", "1.0.2"), Some(false));
        assert_eq!(remote_is_newer("v1.0.2", "1.0.2"), Some(false));
    }

    #[test]
    fn unparsable_side_yields_none() {
        assert_eq!(remote_is_newer("nope", "1.0.0"), None);
        assert_eq!(remote_is_newer("1.0.0", "nope"), None);
    }
}
