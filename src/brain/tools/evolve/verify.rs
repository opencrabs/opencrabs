//! Integrity verification for downloaded release assets (OC-03).
//!
//! Every release publishes a `SHA256SUMS` asset (`.github/workflows/release.yml`
//! runs `sha256sum *.tar.gz *.zip > SHA256SUMS`). The updater must verify the
//! downloaded archive against it before swapping the running binary, or a
//! compromised release, a stolen release token, or a hijacked
//! `browser_download_url` becomes code execution on the next auto-update. The
//! only prior check was a `--version` health probe, which a hostile binary
//! passes trivially.
//!
//! This fails closed: a missing SHA256SUMS asset, a missing entry for the
//! asset, a download served from an unexpected host, or a digest mismatch all
//! refuse the swap.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

/// Hosts a release asset may legitimately be served from. GitHub browser
/// download URLs live on github.com and redirect to
/// objects.githubusercontent.com; pinning to these stops a release JSON whose
/// `browser_download_url` points off-GitHub from being fetched at all.
const ALLOWED_HOSTS: &[&str] = &["github.com", "objects.githubusercontent.com"];

/// Attempts for one release-asset GET before giving up (#2229).
///
/// `/evolve` fetches the platform archive first and the 514 byte `SHA256SUMS`
/// second. One dropped connection on that second request used to abort an
/// update whose 46 MB archive had already come down over the same route.
pub(crate) const FETCH_ATTEMPTS: u32 = 3;

/// Wait between those attempts: long enough for a dropped packet to clear,
/// short enough that a real retry stays invisible to the user.
pub(crate) const FETCH_BACKOFF: Duration = Duration::from_millis(400);

/// Why verification refused, split by what the user should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VerifyFailure {
    /// The fetch never completed: a transport error, or a 5xx/429 that survived
    /// the retry budget. The release may be fine, so the advice is to retry.
    Transient(String),
    /// The bytes are wrong or the release cannot be verified. Retrying cannot
    /// help and the swap must never happen.
    Integrity(String),
}

impl VerifyFailure {
    /// The underlying reason, without the advice wrapper.
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::Transient(m) | Self::Integrity(m) => m,
        }
    }

    /// True when the same request could succeed later.
    pub(crate) fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

/// The user-facing text for a failed verification.
///
/// Only a retry-exhausted transport failure points at `/evolve`; an integrity
/// failure keeps the hard refusal, because re-running cannot fix a bad digest
/// and a release with no `SHA256SUMS` entry stays unverifiable forever.
pub(crate) fn failure_message(failure: &VerifyFailure) -> String {
    match failure {
        VerifyFailure::Transient(msg) => format!(
            "Could not verify the download: {msg}. Nothing was changed, run /evolve again to retry."
        ),
        VerifyFailure::Integrity(msg) => {
            format!("Refusing to update: {msg}. Build from source instead.")
        }
    }
}

/// GET a release asset with a bounded retry (#2229).
///
/// Retries transport failures and 5xx/429 responses. Any other status comes
/// back untouched so a 404 fails fast instead of burning the budget on an asset
/// that is simply not published.
pub(crate) async fn get_with_retry(
    client: &reqwest::Client,
    url: &str,
) -> Result<reqwest::Response, String> {
    let mut last = String::new();
    for attempt in 1..=FETCH_ATTEMPTS {
        match client.get(url).send().await {
            Ok(resp)
                if resp.status().is_server_error()
                    || resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS =>
            {
                last = format!("HTTP {}", resp.status());
            }
            Ok(resp) => return Ok(resp),
            Err(e) => last = e.to_string(),
        }
        if attempt < FETCH_ATTEMPTS {
            tokio::time::sleep(FETCH_BACKOFF).await;
        }
    }
    Err(format!(
        "request failed after {FETCH_ATTEMPTS} attempts (last error: {last})"
    ))
}

/// True when `url` is an https URL whose host we trust to serve a release asset.
pub(crate) fn is_allowed_download_host(url: &str) -> bool {
    is_allowed_download_host_with(url, ALLOWED_HOSTS)
}

/// [`is_allowed_download_host`] with an injectable allowlist, so tests can
/// point verification at a loopback stub. Production only ever passes
/// `ALLOWED_HOSTS`, which holds no loopback entry, so the plain-http exemption
/// below is unreachable outside tests.
pub(crate) fn is_allowed_download_host_with(url: &str, allowed: &[&str]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host_ok = allowed
        .iter()
        .any(|a| host == *a || host.ends_with(&format!(".{a}")));
    if !host_ok {
        return false;
    }
    parsed.scheme() == "https" || is_loopback_host(host)
}

/// Loopback hosts, reachable only through an injected allowlist (tests).
fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]"
}

/// The `SHA256SUMS` asset's download URL, if the release publishes one.
fn sha256sums_url(release: &Value) -> Option<String> {
    release["assets"].as_array()?.iter().find_map(|a| {
        if a["name"].as_str()? == "SHA256SUMS" {
            a["browser_download_url"].as_str().map(String::from)
        } else {
            None
        }
    })
}

/// Lowercase hex SHA-256 of `bytes`.
fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The expected digest for `asset_name` from a `sha256sum`-format document, or
/// None when the file has no entry for it. Lines are `<64 hex>  <filename>`;
/// the filename may carry a leading `*` in binary mode.
pub(crate) fn expected_digest(sums_text: &str, asset_name: &str) -> Option<String> {
    for line in sums_text.lines() {
        let mut parts = line.split_whitespace();
        let hex = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) && name == asset_name {
            return Some(hex.to_ascii_lowercase());
        }
    }
    None
}

/// Verify `archive_bytes` against the release's SHA256SUMS entry for
/// `asset_name`. Fails closed on every uncertainty.
pub(super) async fn verify_asset_checksum(
    client: &reqwest::Client,
    release: &Value,
    asset_name: &str,
    archive_bytes: &[u8],
) -> Result<(), VerifyFailure> {
    verify_asset_checksum_with_hosts(client, release, asset_name, archive_bytes, ALLOWED_HOSTS)
        .await
}

/// [`verify_asset_checksum`] with an injectable host allowlist, so tests can
/// drive the real fetch and retry path against a loopback stub.
pub(crate) async fn verify_asset_checksum_with_hosts(
    client: &reqwest::Client,
    release: &Value,
    asset_name: &str,
    archive_bytes: &[u8],
    allowed_hosts: &[&str],
) -> Result<(), VerifyFailure> {
    let sums_url = sha256sums_url(release).ok_or_else(|| {
        VerifyFailure::Integrity(
            "release publishes no SHA256SUMS asset; refusing to swap an unverified binary (OC-03)"
                .to_string(),
        )
    })?;
    if !is_allowed_download_host_with(&sums_url, allowed_hosts) {
        return Err(VerifyFailure::Integrity(format!(
            "SHA256SUMS is served from an unexpected host, refusing: {sums_url}"
        )));
    }

    let response = get_with_retry(client, &sums_url)
        .await
        .map_err(VerifyFailure::Transient)?;
    let sums_text = response
        .error_for_status()
        .map_err(|e| {
            VerifyFailure::Integrity(format!("SHA256SUMS fetch returned an error status: {e}"))
        })?
        .text()
        .await
        .map_err(|e| VerifyFailure::Transient(format!("failed to read SHA256SUMS body: {e}")))?;

    let expected = expected_digest(&sums_text, asset_name).ok_or_else(|| {
        VerifyFailure::Integrity(format!(
            "SHA256SUMS has no entry for {asset_name}; refusing to swap (OC-03)"
        ))
    })?;

    let actual = hex_sha256(archive_bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(VerifyFailure::Integrity(format!(
            "checksum mismatch for {asset_name}: SHA256SUMS says {expected}, downloaded asset is \
             {actual}; refusing to swap (OC-03)"
        )))
    }
}

/// The filename portion of a release download URL, used to match its
/// SHA256SUMS line.
pub(crate) fn asset_basename(url: &str) -> Option<String> {
    url.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// True when the current process runs as root (euid 0). Unix only; false
/// elsewhere, where this specific root-swap concern does not arise.
pub(super) fn running_as_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid() always succeeds and has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}
