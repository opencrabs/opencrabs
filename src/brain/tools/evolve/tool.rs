//! [`EvolveTool`]: the `evolve` tool surface, the single-flight guard and
//! the dispatch across install methods (pre-built binary, cargo install,
//! source build). The strategies themselves live in `via_binary_download`
//! and `via_cargo_install`.

use super::super::error::Result;
use super::super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use super::release_check::{GITHUB_API, diagnose_releases_latest_status, has_platform_asset};
use crate::brain::agent::{ProgressCallback, ProgressEvent};
use crate::utils::install::InstallMethod;
use async_trait::async_trait;
use serde_json::Value;

pub struct EvolveTool {
    pub(super) progress: Option<ProgressCallback>,
}

impl EvolveTool {
    pub fn new(progress: Option<ProgressCallback>) -> Self {
        Self { progress }
    }
}

/// Set while a real (non-check-only) evolve is downloading/swapping the binary.
/// Prevents two evolves — e.g. a manual `evolve` and the background
/// auto-updater — from running at once and clobbering each other's temp binary
/// (observed on a VPS: concurrent runs collided on the shared temp path, one
/// failing with ETXTBSY mid-write, the other with ENOENT after the sibling
/// deleted it).
static EVOLVE_IN_PROGRESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// RAII guard that clears [`EVOLVE_IN_PROGRESS`] when an evolve finishes,
/// however it returns.
struct EvolveInProgressGuard;

impl Drop for EvolveInProgressGuard {
    fn drop(&mut self) {
        EVOLVE_IN_PROGRESS.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[async_trait]
impl Tool for EvolveTool {
    fn name(&self) -> &str {
        "evolve"
    }

    fn description(&self) -> &str {
        "Check for and install the latest OpenCrabs release. This is the \
         UPGRADE path for users: it fetches what was RELEASED. \
         Automatically detects the install method (pre-built binary, \
         cargo install, or source) and uses the right update strategy. \
         Hot-restarts into the new version after installation. It does not \
         apply uncommitted local changes — build from the source tree with \
         `cargo build --release` first if you carry local edits."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "check_only": {
                    "type": "boolean",
                    "description": "If true, only check for updates without installing. Default: false."
                }
            },
            "required": []
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::SystemModification]
    }

    fn requires_approval(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let check_only = input
            .get("check_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let current_user_version: Option<i64> =
            input.get("current_user_version").and_then(|v| v.as_i64());

        let current_version = crate::VERSION;
        let sid = context.session_id;
        let install_method = InstallMethod::detect();

        // Single-flight: a real evolve downloads and swaps the binary. If one is
        // already running, don't start a second — concurrent runs race on the
        // temp binary and both fail. `check_only` runs are read-only, so they're
        // exempt. The guard clears the flag on every return path.
        let _evolve_guard = if check_only {
            None
        } else if EVOLVE_IN_PROGRESS
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            tracing::warn!(
                target: "evolve",
                session_id = %sid,
                "evolve: another evolve is already in progress — skipping this concurrent run"
            );
            return Ok(ToolResult::error(
                "An update is already in progress; skipping this concurrent evolve run."
                    .to_string(),
            ));
        } else {
            Some(EvolveInProgressGuard)
        };

        // Emit progress
        if let Some(ref cb) = self.progress {
            cb(
                sid,
                ProgressEvent::IntermediateText {
                    text: format!(
                        "Checking for updates (install: {})...",
                        install_method.description()
                    ),
                    reasoning: None,
                },
            );
        }

        // Fetch latest release info from GitHub
        let client = crate::utils::http::shared();
        tracing::info!(
            target: "evolve",
            url = GITHUB_API,
            current_version,
            install_method = install_method.description(),
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
            session_id = %sid,
            check_only,
            "evolve: fetching releases/latest"
        );
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
                    session_id = %sid,
                    "evolve: network error reaching GitHub"
                );
                return Ok(ToolResult::error(format!(
                    "Failed to reach GitHub ({GITHUB_API}): {e}"
                )));
            }
        };
        let status = resp.status();
        let ratelimit_remaining = resp
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let ratelimit_reset = resp
            .headers()
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let release: Value = if status.is_success() {
            match resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        target: "evolve",
                        url = GITHUB_API,
                        error = %e,
                        session_id = %sid,
                        "evolve: 200 response but JSON parse failed"
                    );
                    return Ok(ToolResult::error(format!(
                        "Failed to parse release info from {GITHUB_API}: {e}"
                    )));
                }
            }
        } else {
            let body = resp.text().await.unwrap_or_default();
            let body_excerpt: String = body.chars().take(300).collect();
            tracing::warn!(
                target: "evolve",
                url = GITHUB_API,
                %status,
                ratelimit_remaining = ratelimit_remaining.as_deref().unwrap_or("-"),
                ratelimit_reset = ratelimit_reset.as_deref().unwrap_or("-"),
                body_excerpt = %body_excerpt,
                session_id = %sid,
                "evolve: releases/latest returned non-2xx"
            );
            return Ok(ToolResult::error(diagnose_releases_latest_status(
                status,
                &body_excerpt,
                ratelimit_remaining.as_deref(),
                ratelimit_reset.as_deref(),
            )));
        };

        let latest_tag = release["tag_name"].as_str().unwrap_or("unknown");
        let latest_version = latest_tag.strip_prefix('v').unwrap_or(latest_tag);

        // Compare versions
        if latest_version == current_version {
            return Ok(ToolResult::success(format!(
                "Already on the latest version (v{}).",
                current_version
            )));
        }

        // For pre-built binary installs, verify the platform asset exists
        // before reporting the update as available (release may still be building).
        if matches!(install_method, InstallMethod::PrebuiltBinary)
            && !has_platform_asset(&release, latest_tag)
        {
            let asset_count = release["assets"].as_array().map(|a| a.len()).unwrap_or(0);
            return Ok(ToolResult::error(format!(
                "v{} release exists but the binary for {}/{} is not available yet \
                 ({} assets uploaded so far). The release may still be building — \
                 try again in a few minutes.",
                latest_version,
                std::env::consts::OS,
                std::env::consts::ARCH,
                asset_count
            )));
        }

        if check_only {
            return Ok(ToolResult::success(format!(
                "Update available: v{} -> v{} (install method: {}). Run /evolve to install.",
                current_version,
                latest_version,
                install_method.description()
            )));
        }

        // Dispatch based on install method
        match install_method {
            InstallMethod::Source(_) => {
                return Ok(ToolResult::success(format!(
                    "Update available: v{} -> v{}. You're running from source — rebuild from \
                     the source tree with `cargo build --release`, or `git checkout v{}` to switch.",
                    current_version, latest_version, latest_version
                )));
            }
            InstallMethod::CargoInstall => {
                return self
                    .evolve_via_cargo_install(sid, current_version, latest_version)
                    .await;
            }
            InstallMethod::Homebrew => {
                return super::homebrew::evolve(
                    sid,
                    current_version,
                    latest_version,
                    self.progress.as_ref(),
                )
                .await;
            }
            InstallMethod::PrebuiltBinary => {
                return self
                    .evolve_via_binary_download(
                        sid,
                        client,
                        &release,
                        current_version,
                        latest_tag,
                        latest_version,
                        current_user_version,
                    )
                    .await;
            }
        }
    }
}
