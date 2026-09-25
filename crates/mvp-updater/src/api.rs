//! The update-service API model and the one request that matters.
//!
//! **MVP Update Manager** exposes a small public REST surface; the player only
//! ever needs `latest`. Every response is wrapped in `{ code, message, data }`
//! where `code` mirrors the HTTP status. Two properties of that surface shape
//! this module:
//!
//! * **`404` is not an error.** It means the product exists but has no published
//!   version yet, which is the ordinary state before the first release. It is
//!   mapped to `Ok(None)` — "nothing to offer" — so the caller can stay silent
//!   instead of showing a network error on every launch.
//! * **`update_available` is a convenience, not a contract.** The service
//!   computes it from the `client_version` we send, but a client must still be
//!   able to decide on its own (see [`UpdateInfo::update_is_available`]) so it
//!   is never talked into a downgrade by a stale cache or a missing field.

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::config::{API_BASE, ARCHITECTURE, PLATFORM, PRODUCT};
use crate::{net, version};

/// A published release as described by `GET .../latest`.
///
/// Every field past `version` is optional: the core of the model (which version
/// is on offer and where to get it) must not be defeated by the service growing
/// or dropping a secondary field.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateInfo {
    /// The published version, e.g. `1.0.3` (no `v` prefix), directly comparable
    /// to `CARGO_PKG_VERSION`.
    pub version: String,
    /// Monotonic integer the service also uses to order releases.
    #[serde(default)]
    pub version_code: Option<i64>,
    /// Release notes, Markdown.
    #[serde(default)]
    pub change_log: Option<String>,
    /// Full URL of the package (often a CDN address).
    pub download_url: String,
    /// Package size in bytes.
    #[serde(default)]
    pub file_size: Option<u64>,
    /// SHA-256 (lowercase hex) of the package; verified before unpacking.
    #[serde(default)]
    pub sha256: Option<String>,
    /// When set, the update cannot be declined.
    #[serde(default)]
    pub force_update: Option<bool>,
    /// Platform the release targets.
    #[serde(default)]
    pub platform: Option<String>,
    /// Architecture the release targets.
    #[serde(default)]
    pub architecture: Option<String>,
    /// Publication timestamp, as reported.
    #[serde(default)]
    pub published_at: Option<String>,
    /// Whether this is the newest release.
    #[serde(default)]
    pub is_latest: Option<bool>,
    /// The service's own verdict for the `client_version` we sent.
    #[serde(default)]
    pub update_available: Option<bool>,
}

impl UpdateInfo {
    /// Whether this release must be applied without the option to postpone.
    pub fn is_forced(&self) -> bool {
        self.force_update.unwrap_or(false)
    }

    /// Whether the update should actually be offered, given the running version.
    ///
    /// Priority: the service's `update_available` when present, otherwise a
    /// local three-segment comparison of the version strings. Either way a
    /// final "must be strictly newer" guard applies, so a late or cached
    /// response can never propose a downgrade.
    pub fn update_is_available(&self, local_version: &str) -> bool {
        let offered = match self.update_available {
            Some(flag) => flag,
            None => match version::remote_is_newer(&self.version, local_version) {
                Some(newer) => newer,
                // Nothing to compare against: not an update we can vouch for.
                None => return false,
            },
        };
        if !offered {
            return false;
        }
        // The service said yes and we cannot second-guess it when the versions
        // do not parse.
        version::remote_is_newer(&self.version, local_version).unwrap_or(true)
    }

    /// The expected package digest, if the service supplied one.
    pub fn expected_sha256(&self) -> Option<&str> {
        self.sha256.as_deref().filter(|value| !value.is_empty())
    }
}

/// A page of published versions (`GET .../versions`).
#[derive(Debug, Clone, Deserialize)]
pub struct VersionPage {
    /// The releases themselves.
    #[serde(default)]
    pub items: Vec<serde_json::Value>,
    /// Total number of releases.
    #[serde(default)]
    pub total: i64,
}

/// URL of the `latest` query for a given client version.
pub fn latest_url(client_version: &str) -> String {
    format!(
        "{API_BASE}/product/{PRODUCT}/latest?platform={PLATFORM}&architecture={ARCHITECTURE}&client_version={client_version}"
    )
}

/// URL of the version listing.
pub fn versions_url() -> String {
    format!("{API_BASE}/product/{PRODUCT}/versions")
}

/// Ask the service for the latest release of the running version.
///
/// `Ok(None)` means "there is no update to offer": either no version has been
/// published (`404`) or the response carried no data. `Err` is reserved for
/// transport failures and genuinely malformed responses.
pub fn fetch_latest() -> Result<Option<UpdateInfo>> {
    fetch_latest_for(env!("CARGO_PKG_VERSION"))
}

/// [`fetch_latest`] with an explicit client version (for the standalone updater
/// and for tests).
pub fn fetch_latest_for(client_version: &str) -> Result<Option<UpdateInfo>> {
    let url = latest_url(client_version);
    let response = net::get(&url)?;
    if response.status == 404 {
        // "No published version found for this product" — not an error.
        return Ok(None);
    }
    if !response.is_success() {
        bail!(
            "the update service returned HTTP {} for {url}: {}",
            response.status,
            response.body_text()
        );
    }
    let value: serde_json::Value = serde_json::from_slice(&response.body)
        .with_context(|| format!("the response from {url} was not valid JSON"))?;
    interpret_latest(response.status, &value)
}

/// Fetch every published version (used by the live integration test).
pub fn fetch_versions() -> Result<VersionPage> {
    let url = versions_url();
    let response = net::get(&url)?;
    if !response.is_success() {
        bail!("the update service returned HTTP {} for {url}", response.status);
    }
    let value: serde_json::Value = serde_json::from_slice(&response.body)
        .with_context(|| format!("the response from {url} was not valid JSON"))?;
    let code = value.get("code").and_then(serde_json::Value::as_i64).unwrap_or(0);
    if code != 200 {
        bail!(
            "the update service answered code {code}: {}",
            value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        );
    }
    let data = value
        .get("data")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    serde_json::from_value(data).context("the version listing had an unexpected shape")
}

/// Turn a `latest` response body into an [`UpdateInfo`] decision.
///
/// Split out from the transport so the `404` / missing-`data` / success cases
/// can be exercised without a network.
pub(crate) fn interpret_latest(
    status: u16,
    value: &serde_json::Value,
) -> Result<Option<UpdateInfo>> {
    let code = value.get("code").and_then(serde_json::Value::as_i64).unwrap_or(0);
    // The service may signal "no version" either way; both mean the same thing.
    if code == 404 || status == 404 {
        return Ok(None);
    }
    if code != 0 && code != 200 {
        bail!(
            "the update service answered code {code}: {}",
            value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        );
    }
    match value.get("data") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(data) => {
            let info: UpdateInfo = serde_json::from_value(data.clone())
                .context("the latest-release payload had an unexpected shape")?;
            Ok(Some(info))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample(update_available: Option<bool>, version: &str) -> UpdateInfo {
        serde_json::from_value(json!({
            "version": version,
            "download_url": "https://cdn.example/pkg.zip",
            "update_available": update_available,
        }))
        .expect("sample decodes")
    }

    #[test]
    fn envelope_404_is_no_update() {
        let value = json!({ "code": 404, "message": "No published version found for this product" });
        assert!(interpret_latest(404, &value).unwrap().is_none());
        // A 200 transport with a 404 code in the envelope is treated the same.
        assert!(interpret_latest(200, &value).unwrap().is_none());
    }

    #[test]
    fn envelope_missing_data_is_no_update() {
        let value = json!({ "code": 200, "data": null });
        assert!(interpret_latest(200, &value).unwrap().is_none());
        let value = json!({ "code": 200 });
        assert!(interpret_latest(200, &value).unwrap().is_none());
    }

    #[test]
    fn envelope_success_decodes_full_payload() {
        let value = json!({
            "code": 200,
            "message": "ok",
            "data": {
                "version": "1.0.3",
                "version_code": 10003,
                "change_log": "- fix things",
                "download_url": "https://cdn.example/MVP-1.0.3.zip",
                "file_size": 123456,
                "sha256": "abc123",
                "force_update": false,
                "platform": "windows",
                "architecture": "x86_64",
                "update_available": true,
            }
        });
        let info = interpret_latest(200, &value).unwrap().unwrap();
        assert_eq!(info.version, "1.0.3");
        assert_eq!(info.version_code, Some(10003));
        assert_eq!(info.file_size, Some(123456));
        assert_eq!(info.expected_sha256(), Some("abc123"));
        assert!(info.update_is_available("1.0.2"));
        assert!(!info.is_forced());
    }

    #[test]
    fn envelope_error_code_is_reported() {
        let value = json!({ "code": 500, "message": "boom" });
        assert!(interpret_latest(500, &value).is_err());
    }

    #[test]
    fn availability_prefers_service_flag() {
        // Service says yes even though we cannot compare (would otherwise be
        // suppressed): trust it.
        let info = sample(Some(true), "1.0.3");
        assert!(info.update_is_available("1.0.2"));
        // Service says no: honour it even though the version looks newer.
        let info = sample(Some(false), "9.9.9");
        assert!(!info.update_is_available("1.0.2"));
    }

    #[test]
    fn availability_falls_back_to_local_comparison() {
        // No flag: a strictly newer version is offered…
        assert!(sample(None, "1.0.10").update_is_available("1.0.9"));
        // …an equal one is not…
        assert!(!sample(None, "1.0.2").update_is_available("1.0.2"));
        // …and a phantom downgrade is refused outright.
        assert!(!sample(None, "1.0.1").update_is_available("1.0.2"));
    }

    #[test]
    fn availability_refuses_downgrade_even_if_service_insists() {
        let info = sample(Some(true), "1.0.1");
        assert!(!info.update_is_available("1.0.2"));
    }

    #[test]
    fn latest_url_contains_product_and_client_version() {
        let url = latest_url("1.0.2");
        assert!(url.contains(PRODUCT));
        assert!(url.contains("client_version=1.0.2"));
        assert!(url.contains(PLATFORM));
        assert!(url.contains(ARCHITECTURE));
    }

    /// Live check against the real service. `#[ignore]` so the normal test run
    /// never depends on the network; run with
    /// `cargo test -p mvp-updater --features apply -- --ignored`.
    #[test]
    #[ignore = "hits the live update service"]
    fn live_service_is_reachable() {
        let page = fetch_versions().expect("versions endpoint should answer");
        // `items` is an array whether or not anything is published.
        let _ = page.total;
        // `latest` is either a full record or the documented "no version" case.
        let latest = fetch_latest_for("1.0.0").expect("latest endpoint should answer");
        if let Some(info) = latest {
            assert!(!info.version.is_empty());
            assert!(!info.download_url.is_empty());
        }
    }
}
