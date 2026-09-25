//! HTTP transport for the update service, built directly on **WinHTTP**.
//!
//! WinHTTP rather than a Rust HTTP client on purpose: it is part of Windows, so
//! it adds nothing to the binary, it uses the system certificate store and
//! Schannel (so there is no bundled TLS stack to keep patched) and it honours
//! the machine's proxy configuration automatically. The two requests the update
//! flow ever makes — "what is the latest release?" and "give me the package
//! bytes" — are both plain `GET`s, which is exactly the shape WinHTTP makes
//! easy.
//!
//! The package download is *streaming*: bytes go straight to disk while a
//! SHA-256 is folded over them, so a 120 MB package never has to be held in
//! memory to be verified. The digest is what the caller compares against the
//! service's `sha256` before anything is unpacked.

use std::path::Path;

use anyhow::{bail, Context, Result};

#[cfg(windows)]
use sha2::{Digest, Sha256};

/// Callback that receives each downloaded chunk together with the total content
/// length (when the server declared one).
#[cfg(windows)]
type ChunkSink<'a> = dyn FnMut(&[u8], Option<u64>) -> Result<()> + 'a;

/// Status code and body of a completed `GET`.
#[derive(Debug)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Whether the status is in the 2xx range.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The body decoded as UTF-8, with invalid sequences replaced.
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Perform a `GET` and return the whole body.
#[cfg(windows)]
pub fn get(url: &str) -> Result<HttpResponse> {
    let mut body = Vec::new();
    let status = win::stream_get(url, &mut |chunk, _total| {
        body.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(HttpResponse { status, body })
}

/// Non-Windows stub: there is no WinHTTP.
#[cfg(not(windows))]
pub fn get(url: &str) -> Result<HttpResponse> {
    let _ = url;
    bail!("the update transport is only available on Windows")
}

/// Perform a `GET` and parse the body as JSON.
///
/// Returns the status code alongside the value so the caller can distinguish a
/// `404` ("no published version", not an error) from a `200` with an unexpected
/// payload.
pub fn get_json(url: &str) -> Result<(u16, serde_json::Value)> {
    let response = get(url)?;
    let value: serde_json::Value = serde_json::from_slice(&response.body)
        .with_context(|| format!("the response from {url} was not valid JSON"))?;
    Ok((response.status, value))
}

/// Download `url` to `dest`, streaming to disk, and return the **lowercase hex
/// SHA-256** of everything received.
///
/// `progress` is called after every chunk with `(received, total)`; `total` is
/// the server's `Content-Length` when it supplied one, otherwise `None` so the
/// caller can fall back to showing just the byte count.
#[cfg(windows)]
pub fn download_to_file(
    url: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let file = std::fs::File::create(dest)
        .with_context(|| format!("could not create {}", dest.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;

    let status = win::stream_get(url, &mut |chunk, total| {
        use std::io::Write;
        writer
            .write_all(chunk)
            .with_context(|| format!("could not write to {}", dest.display()))?;
        hasher.update(chunk);
        received += chunk.len() as u64;
        progress(received, total);
        Ok(())
    })?;

    if !(200..300).contains(&status) {
        bail!("the download from {url} failed with HTTP {status}");
    }
    use std::io::Write;
    writer
        .flush()
        .with_context(|| format!("could not flush {}", dest.display()))?;

    Ok(to_hex(&hasher.finalize()))
}

/// Non-Windows stub: there is no WinHTTP.
#[cfg(not(windows))]
pub fn download_to_file(
    url: &str,
    dest: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String> {
    let _ = (url, dest, progress);
    bail!("the update transport is only available on Windows")
}

/// Render `bytes` as lowercase hexadecimal.
pub fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// WinHTTP backend
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    use anyhow::{bail, Context, Result};
    use windows::core::{Error as WinError, PCWSTR, PWSTR};
    use windows::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
        WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
        WinHttpSendRequest, WinHttpSetTimeouts, INTERNET_DEFAULT_HTTPS_PORT, URL_COMPONENTS,
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
        WINHTTP_FLAG_SECURE, WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_OPEN_REQUEST_FLAGS,
        WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    };

    use crate::config;

    /// How many bytes are pulled from WinHTTP per read.
    const CHUNK: usize = 64 * 1024;

    /// RAII wrapper for a WinHTTP handle: closed exactly once, including on the
    /// error paths that unwind out of a request.
    struct Handle(*mut c_void);

    impl Handle {
        /// Wrap a freshly returned handle, turning a null pointer (failure) into
        /// an error carrying the last Win32 code.
        fn new(pointer: *mut c_void, what: &str) -> Result<Handle> {
            if pointer.is_null() {
                return Err(anyhow::anyhow!(
                    "WinHTTP {what} failed: {}",
                    WinError::from_thread()
                ));
            }
            Ok(Handle(pointer))
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is a live WinHTTP handle owned by this wrapper and
            // not used again.
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }

    /// Encode `s` as a NUL-terminated UTF-16 buffer for `PCWSTR`.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Read a WinHTTP-returned `PWSTR` of `len` UTF-16 units as an owned String.
    fn pwstr_to_string(pointer: PWSTR, len: u32) -> String {
        if pointer.is_null() || len == 0 {
            return String::new();
        }
        // SAFETY: WinHttpCrackUrl set `pointer` to a run of `len` units inside
        // the URL buffer this module owns for the duration of the call.
        let units = unsafe { std::slice::from_raw_parts(pointer.0 as *const u16, len as usize) };
        String::from_utf16_lossy(units)
    }

    /// The pieces of a URL WinHTTP needs to open a request.
    struct Target {
        host: String,
        path: String,
        port: u16,
        secure: bool,
    }

    /// Split `url` into host / path / port using the documented `WinHttpCrackUrl`
    /// contract (length fields of `-1` return pointers into the original buffer).
    fn crack(url: &str) -> Result<Target> {
        // No NUL: `WinHttpCrackUrl` takes an explicit length and must not count a
        // terminator as part of the URL.
        let units: Vec<u16> = url.encode_utf16().collect();
        let mut components = URL_COMPONENTS {
            dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
            ..Default::default()
        };
        // Asking for a pointer into the buffer rather than a copy.
        components.dwSchemeLength = u32::MAX;
        components.dwHostNameLength = u32::MAX;
        components.dwUserNameLength = u32::MAX;
        components.dwPasswordLength = u32::MAX;
        components.dwUrlPathLength = u32::MAX;
        components.dwExtraInfoLength = u32::MAX;

        // SAFETY: `units` is a live UTF-16 buffer and `components` points at it
        // only for the duration of this call; both survive until the fields are
        // copied out below.
        unsafe { WinHttpCrackUrl(&units, 0, &mut components) }
            .with_context(|| format!("could not parse the update URL {url:?}"))?;

        let secure = components.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS;
        let host = pwstr_to_string(components.lpszHostName, components.dwHostNameLength);
        if host.is_empty() {
            bail!("the update URL {url:?} has no host");
        }
        let path = {
            let path = pwstr_to_string(components.lpszUrlPath, components.dwUrlPathLength);
            let extra = pwstr_to_string(components.lpszExtraInfo, components.dwExtraInfoLength);
            let joined = format!("{path}{extra}");
            if joined.is_empty() {
                "/".to_string()
            } else {
                joined
            }
        };
        let port = if components.nPort != 0 {
            components.nPort
        } else if secure {
            INTERNET_DEFAULT_HTTPS_PORT
        } else {
            80
        };
        Ok(Target {
            host,
            path,
            port,
            secure,
        })
    }

    /// A live `GET`: the request plus the connection and session it hangs off.
    ///
    /// WinHTTP ties a request to the connection (and the connection to the
    /// session) that produced it, and closing either parent cancels every read
    /// still outstanding on the request with `ERROR_WINHTTP_OPERATION_CANCELLED`.
    /// So all three handles must outlive the *body* read, not just the response
    /// headers: the old code kept the session and connection as locals of
    /// `open_get`, closed them on return, and every read afterwards failed.
    /// Fields drop in declaration order, so the request is closed before its
    /// parents.
    struct Get {
        request: Handle,
        _connect: Handle,
        _session: Handle,
        status: u16,
        total: Option<u64>,
    }

    /// Open a session, connect, issue a `GET` and leave the response ready to
    /// read, returning the live handles plus the status and declared length.
    fn open_get(url: &str) -> Result<Get> {
        let target = crack(url)?;

        let agent = wide(&config::user_agent());
        // SAFETY: every buffer is NUL-terminated and outlives the call; the
        // session is closed by the `Handle` that wraps it.
        let mut session = unsafe {
            WinHttpOpen(
                PCWSTR(agent.as_ptr()),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        };
        // `AUTOMATIC_PROXY` needs Windows 8.1; fall back on anything older.
        if session.is_null() {
            session = unsafe {
                WinHttpOpen(
                    PCWSTR(agent.as_ptr()),
                    WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    0,
                )
            };
        }
        let session = Handle::new(session, "WinHttpOpen")?;

        // Bounded waits so a black-holed host cannot hang the updater forever.
        // A failure here is not fatal (defaults still apply), so it is ignored.
        unsafe { WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 30_000) }.ok();

        let host = wide(&target.host);
        // SAFETY: `host` is NUL-terminated and outlives the call.
        let connect = unsafe { WinHttpConnect(session.0, PCWSTR(host.as_ptr()), target.port, 0) };
        let connect = Handle::new(connect, "WinHttpConnect")?;

        let verb = wide("GET");
        let path = wide(&target.path);
        let flags = if target.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        // SAFETY: `verb` and `path` are NUL-terminated; the null accept-types
        // pointer means "accept anything"; WinHTTP follows redirects to the
        // package's CDN for us.
        let request = unsafe {
            WinHttpOpenRequest(
                connect.0,
                PCWSTR(verb.as_ptr()),
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            )
        };
        let request = Handle::new(request, "WinHttpOpenRequest")?;

        // SAFETY: no extra headers or body; a null optional pointer is allowed.
        unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }
            .context("WinHttpSendRequest failed")?;
        // SAFETY: the reserved parameter must be null.
        unsafe { WinHttpReceiveResponse(request.0, std::ptr::null_mut()) }
            .context("WinHttpReceiveResponse failed")?;

        let status = query_u32(request.0, WINHTTP_QUERY_STATUS_CODE)
            .context("could not read the HTTP status code")? as u16;
        let total = query_u32(request.0, WINHTTP_QUERY_CONTENT_LENGTH)
            .ok()
            .map(u64::from);
        Ok(Get {
            request,
            _connect: connect,
            _session: session,
            status,
            total,
        })
    }

    /// Read one numeric header (the `WINHTTP_QUERY_FLAG_NUMBER` form).
    fn query_u32(request: *mut c_void, header: u32) -> Result<u32> {
        let mut value: u32 = 0;
        let mut length = std::mem::size_of::<u32>() as u32;
        // SAFETY: `value` is a live `u32` that `length` describes exactly, which
        // is the contract for the numeric-query form.
        unsafe {
            WinHttpQueryHeaders(
                request,
                header | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut value as *mut u32 as *mut c_void),
                &mut length,
                std::ptr::null_mut(),
            )
        }?;
        Ok(value)
    }

    /// Issue `GET` and feed each received chunk to `sink` as `(bytes, total)`.
    /// Returns the HTTP status.
    pub(super) fn stream_get(url: &str, sink: &mut super::ChunkSink<'_>) -> Result<u16> {
        // `get` owns the request *and* its connection and session: they have to
        // stay alive for the whole loop, or the very first read is cancelled.
        let get = open_get(url)?;
        let mut buffer = vec![0u8; CHUNK];
        loop {
            let mut available: u32 = 0;
            // SAFETY: `available` is a live `u32` out-parameter.
            unsafe { WinHttpQueryDataAvailable(get.request.0, &mut available) }
                .context("WinHttpQueryDataAvailable failed")?;
            if available == 0 {
                break;
            }
            let want = available.min(buffer.len() as u32);
            let mut read: u32 = 0;
            // SAFETY: `buffer` has at least `want` writable bytes and `read`
            // receives the count actually written.
            unsafe {
                WinHttpReadData(
                    get.request.0,
                    buffer.as_mut_ptr() as *mut c_void,
                    want,
                    &mut read,
                )
            }
            .context("WinHttpReadData failed")?;
            if read == 0 {
                break;
            }
            sink(&buffer[..read as usize], get.total)?;
        }
        Ok(get.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_hex_is_lowercase_and_padded() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(to_hex(&[]), "");
    }

    #[test]
    fn response_status_helpers() {
        let ok = HttpResponse {
            status: 200,
            body: b"hi".to_vec(),
        };
        assert!(ok.is_success());
        assert_eq!(ok.body_text(), "hi");
        assert!(!HttpResponse {
            status: 404,
            body: Vec::new(),
        }
        .is_success());
    }

    #[test]
    fn json_parse_reports_non_json_body() {
        // A 404 body that is not JSON must surface as an error rather than a
        // panic; the API layer turns the *status* into "no version".
        let response = HttpResponse {
            status: 404,
            body: b"<html>nope</html>".to_vec(),
        };
        let parsed: Result<serde_json::Value> = serde_json::from_slice(&response.body)
            .with_context(|| "the response was not valid JSON");
        assert!(parsed.is_err());
    }
}
