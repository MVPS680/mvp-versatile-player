//! Tiny helpers for building NUL-terminated UTF-16 buffers for Win32 calls.
//!
//! Every `*W` Win32 entry point wants a NUL-terminated UTF-16 string, and the
//! registry additionally wants `REG_SZ` payloads as the *little-endian byte
//! image* of such a buffer with `cbData` covering the terminating NUL. Both
//! shapes are produced here so the call sites stay short and consistent.
//!
//! This module is only compiled on Windows; non-Windows builds never reference
//! it (see `lib.rs`).

use std::iter::once;

/// Encode `s` as a NUL-terminated UTF-16 buffer suitable for `PCWSTR`.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(once(0)).collect()
}

/// Encode `s` as the little-endian byte image of a NUL-terminated UTF-16
/// buffer, i.e. exactly what `RegSetValueExW(.., REG_SZ, ..)` expects.
///
/// The returned length *includes* the terminating NUL, which is what the
/// registry requires for `cbData`.
pub(crate) fn wide_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain(once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Decode a `REG_SZ` payload (little-endian UTF-16 bytes) back into a `String`.
///
/// Any trailing NULs are dropped, an odd trailing byte is ignored, and
/// unpaired surrogates are replaced rather than rejected so that a malformed
/// registry value can never panic the caller.
pub(crate) fn from_wide_bytes(bytes: &[u8]) -> String {
    let usable = bytes.len() - (bytes.len() % 2);
    let mut units: Vec<u16> = bytes[..usable]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    while matches!(units.last(), Some(0)) {
        units.pop();
    }
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_is_nul_terminated() {
        assert_eq!(wide("ab"), vec![0x61, 0x62, 0x00]);
        assert_eq!(wide(""), vec![0x00]);
    }

    #[test]
    fn wide_bytes_covers_terminating_nul() {
        let bytes = wide_bytes("A");
        assert_eq!(bytes, vec![0x41, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn from_wide_bytes_round_trips_and_trims_nuls() {
        for value in ["", "hello", "多功能音视频与图片播放器 (FFmpeg)", "MVPVersatilePlayer.mp4"] {
            let bytes = wide_bytes(value);
            assert_eq!(from_wide_bytes(&bytes), value);
        }
        assert_eq!(from_wide_bytes(&[]), "");
        // An odd trailing byte is ignored instead of panicking.
        assert_eq!(from_wide_bytes(&[0x41]), "");
        assert_eq!(from_wide_bytes(&[0x41, 0x00, 0x00, 0x00]), "A");
        // Unpaired surrogates are replaced, never rejected.
        assert_eq!(from_wide_bytes(&[0x00, 0xd8]), "\u{fffd}");
    }
}
