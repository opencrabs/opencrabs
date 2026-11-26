//! Release discovery: the GitHub `releases/latest` probe, its status-aware
//! diagnostics, platform-asset presence and semver comparison.

use crate::utils::install::{InstallMethod, platform_suffix};

pub(super) const GITHUB_API: &str =
    "https://api.github.com/repos/opencrabs/opencrabs/releases/latest";

/// Build an honest, status-aware error string for a non-success
/// response from `releases/latest`. Replaces the prior hardcoded
/// "rate limited or unavailable" suffix that lied about every
/// non-2xx — a real 404 (no published release) and a 403 (rate
/// limit) looked identical to the user, sending us down wrong
/// debug paths.
///
/// `body_excerpt` should be the first ~300 chars of the response
/// body so the message can quote the API's own explanation when
/// it returns one (GitHub error envelopes carry a useful `message`
/// field, e.g. "API rate limit exceeded for ...").
pub(crate) fn diagnose_releases_latest_status(
    status: reqwest::StatusCode,
    body_excerpt: &str,
    ratelimit_remaining: Option<&str>,
    ratelimit_reset: Option<&str>,
) -> String {
    let code = status.as_u16();
    let body_tail = if body_excerpt.trim().is_empty() {
        String::new()
    } else {
        format!(" — API said: {}", body_excerpt.trim())
    };
    let ratelimit_tail = match (ratelimit_remaining, ratelimit_reset) {
        (Some(r), Some(reset)) => {
            format!(" [x-ratelimit-remaining={r}, x-ratelimit-reset={reset}]")
        }
        (Some(r), None) => format!(" [x-ratelimit-remaining={r}]"),
        _ => String::new(),
    };
    match code {
        404 => format!(
            "GitHub returned 404 for releases/latest — no published \
             (non-draft, non-prerelease) release exists for this repo \
             at this moment, or there's a brief publish-propagation lag. \
             Try again in a minute.{body_tail}{ratelimit_tail}"
        ),
        403 | 429 => format!(
            "GitHub rate limit hit ({code}) — unauthenticated requests \
             are capped at 60/hr per IP. Wait an hour, or set GITHUB_TOKEN \
             in your env to raise the cap to 5000/hr if you share this \
             IP.{body_tail}{ratelimit_tail}"
        ),
        500..=599 => format!(
            "GitHub API returned {code} — server-side issue, retry in a \
             few minutes.{body_tail}"
        ),
        _ => format!("GitHub API returned {status}.{body_tail}{ratelimit_tail}"),
    }
}

/// Check GitHub for a newer release. Returns `Some(latest_version)` if an
/// update is available **and** a binary asset exists for this platform,
/// `None` if already on latest, no asset ready, or on error.
pub async fn check_for_update() -> Option<String> {
    let current_version = crate::VERSION;
    let client = crate::utils::http::shared();
    let resp = match client
        .get(GITHUB_API)
        .header("User-Agent", format!("opencrabs/{}", current_version))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "evolve",
                url = GITHUB_API,
                error = %e,
                "background update check failed to reach GitHub"
            );
            return None;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body_excerpt: String = body.chars().take(300).collect();
        tracing::warn!(
            target: "evolve",
            url = GITHUB_API,
            %status,
            body_excerpt,
            "background update check: releases/latest returned non-2xx"
        );
        return None;
    }
    let release: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "evolve",
                url = GITHUB_API,
                error = %e,
                "background update check: failed to parse releases/latest JSON"
            );
            return None;
        }
    };

    let latest_tag = match release["tag_name"].as_str() {
        Some(t) => t,
        None => {
            tracing::warn!(
                target: "evolve",
                "background update check: releases/latest payload missing tag_name"
            );
            return None;
        }
    };
    let latest_version = latest_tag.strip_prefix('v').unwrap_or(latest_tag);

    if !is_newer(latest_version, current_version) {
        return None;
    }

    // If running from source, check if Cargo.toml already has the latest version
    if let Some(source_version) = source_cargo_version()
        && source_version == latest_version
    {
        return None;
    }

    // For pre-built binary installs, only report "available" if the platform
    // asset actually exists in the release (release may still be building).
    if matches!(InstallMethod::detect(), InstallMethod::PrebuiltBinary)
        && !has_platform_asset(&release, latest_tag)
    {
        tracing::debug!(
            "Release {} exists but no asset for this platform yet",
            latest_tag
        );
        return None;
    }

    Some(latest_version.to_string())
}

/// Check whether the release JSON contains a downloadable asset for the
/// current platform.
pub(crate) fn has_platform_asset(release: &serde_json::Value, tag: &str) -> bool {
    let suffix = match platform_suffix() {
        Some(s) => s,
        None => return false,
    };
    let ext = if std::env::consts::OS == "windows" {
        "zip"
    } else {
        "tar.gz"
    };
    let expected = format!("opencrabs-{}-{}.{}", tag, suffix, ext);
    let legacy = format!("opencrabs-{}.{}", suffix, ext);

    release["assets"]
        .as_array()
        .map(|arr| {
            arr.iter().any(|a| {
                let name = a["name"].as_str().unwrap_or("");
                name == expected || name == legacy
            })
        })
        .unwrap_or(false)
}

/// Compare semver strings: returns true if `latest` is strictly newer than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> { v.split('.').filter_map(|s| s.parse().ok()).collect() };
    let l = parse(latest);
    let c = parse(current);
    l > c
}

/// Try to read the version from the source Cargo.toml relative to the running
/// binary. Returns `None` if not running from a source build or file not found.
pub(super) fn source_cargo_version() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let target_dir = exe.parent()?;
    let repo_root = target_dir.parent()?.parent()?;
    let cargo_toml = repo_root.join("Cargo.toml");
    let content = std::fs::read_to_string(&cargo_toml).ok()?;
    let table: toml::Table = content.parse().ok()?;
    table
        .get("package")?
        .get("version")?
        .as_str()
        .map(String::from)
}
