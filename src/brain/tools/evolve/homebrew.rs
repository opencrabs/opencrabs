//! Upgrading a Homebrew-managed install.
//!
//! Homebrew owns the Cellar binary and records which version it believes is
//! installed. Renaming a downloaded binary over it SUCCEEDS, because the prefix
//! is user-owned on Apple Silicon, and then brew's manifest disagrees with the
//! disk until an unrelated `brew upgrade` silently reverts the user. Letting
//! brew do the upgrade keeps the version it reports true (#963).
//!
//! Supervision (#1779). This was the unsupervised branch: children awaited with
//! no wall-clock budget, a reported version that was the GitHub release name
//! fetched BEFORE brew ran, and no restart backstop, so the process exec'd itself
//! into `current_exe()` while brew might still be mutating the Cellar. Now each
//! `brew` child runs under [`BREW_TIMEOUT`] and is killed when it overruns (see
//! [`super::bounded_child`]); the version comes from brew, read back AFTER the
//! upgrade, or none is claimed; and a systemd host arms the same delayed-restart
//! timer the binary-download branch arms.

use super::bounded_child::{Outcome, first_version_field, run_bounded};
use super::restart_status::RestartStatus;
use super::systemd::{
    EVOLVE_UNIT_PREFIX, RestartPlan, build_systemd_restart_command, select_restart_targets,
    sweep_stale_evolve_units,
};
use crate::brain::agent::{ProgressCallback, ProgressEvent};
use crate::brain::tools::error::{Result, ToolError};
use crate::brain::tools::r#trait::ToolResult;
use std::time::Duration;

/// The formula name, which is also the binary name.
const FORMULA: &str = "opencrabs";

/// Wall-clock budget for a single `brew` child, deliberately generous: `brew
/// update` refreshes the whole homebrew-core index and `brew upgrade` may
/// compile from source when no bottle matches the host. What was not acceptable
/// is the previous shape, no budget at all, where a brew wedged on a locked curl
/// held the evolve open and the operator got neither success nor failure (#1779
/// defect 7).
const BREW_TIMEOUT: Duration = Duration::from_secs(600);

/// Why an upgrade attempt did not happen, phrased for the user.
///
/// Pure so the wording is testable without a Homebrew install: a refusal the
/// user cannot act on is worse than the silent overwrite it replaced.
pub(crate) fn spawn_failure_message(err: &str) -> String {
    format!(
        "This build is managed by Homebrew, but `brew` could not be run: {err}. \
         Upgrade with `brew upgrade {FORMULA}`."
    )
}

/// Message for a `brew upgrade` that ran and failed.
pub(crate) fn upgrade_failure_message(stderr: &str) -> String {
    let excerpt: String = stderr.chars().take(500).collect();
    format!("brew upgrade failed: {excerpt}")
}

/// Message for a `brew` child that outlived its budget and was killed.
///
/// Names the check the operator can run, because a killed upgrade leaves the
/// tree in an unknown state: brew may have written the new keg and died before
/// repointing the symlink.
pub(crate) fn timeout_message(stage: &str, budget: Duration) -> String {
    format!(
        "`brew {stage}` was still running after {}s and has been killed. Whether the \
         upgrade landed is unknown: check with `brew list --versions {FORMULA}`, and if \
         it did not land, retry with `brew upgrade {FORMULA}`.",
        budget.as_secs()
    )
}

/// Ask `program` what version of the formula it believes is installed.
///
/// Best-effort by design: a read-back that fails yields `None`, and the user is
/// told the version is unconfirmed rather than being told a guessed one.
pub(crate) async fn read_installed_version(program: &str, budget: Duration) -> Option<String> {
    match run_bounded(program, &["list", "--versions", FORMULA], budget)
        .await
        .ok()?
    {
        Outcome::Exited(output) if output.status.success() => {
            first_version_field(&String::from_utf8_lossy(&output.stdout))
        }
        _ => None,
    }
}

/// The success line, built from what brew reports AFTER the upgrade.
///
/// The pre-run GitHub release name is deliberately not consulted: brew may have
/// installed something else (a different bottle, a pinned formula, nothing at
/// all), and the old wording reported a version the user never received (#1779
/// defect 7).
pub(crate) fn success_line(
    current: &str,
    read_back: Option<&str>,
    restart: &RestartStatus,
) -> String {
    let versions = match read_back {
        Some(v) => format!("Evolved via Homebrew: v{current} -> v{v}."),
        None => format!(
            "Ran `brew upgrade {FORMULA}` from v{current}. brew reported no installed \
             version afterwards, so none is claimed here; confirm with `brew list \
             --versions {FORMULA}`."
        ),
    };
    let clause = restart.restart_clause();
    if clause.is_empty() {
        versions
    } else {
        format!("{versions} {clause}")
    }
}

/// Arm the systemd delayed restart, the backstop the download branch has always
/// had and this branch never did (#1779 defects 5 and 6).
///
/// Order matters: pre-flight resolves the bus and the exact unit list, sweep
/// spent transient units, then spawn `systemd-run`. The caller still emits
/// `RestartReady` so the in-process exec stays the fast path; the timer is what
/// brings the daemon back when that exec dies, which on a brew host is a real
/// risk because brew mutates the Cellar symlink the process is about to
/// re-enter.
///
/// The restart target is the resolved list, never the discovery glob: the
/// transient unit this very call creates also matches that glob, and a
/// glob-as-target restart set would contain its own executor (#2081).
///
/// Private on purpose: calling this from a test could restart a real daemon.
fn arm_systemd_backstop(sid: uuid::Uuid) -> RestartStatus {
    if !std::path::Path::new("/run/systemd/system").exists() {
        return RestartStatus::NotSystemd;
    }
    let targets = match select_restart_targets(sid) {
        RestartPlan::Targets(targets) => targets,
        RestartPlan::NoUnitsMatched => return RestartStatus::NoUnitsMatched,
        RestartPlan::Unresolved => return RestartStatus::Unresolved,
    };
    sweep_stale_evolve_units(targets.user, sid);
    let pid = std::process::id();
    match build_systemd_restart_command(pid, targets.user, &targets.units).spawn() {
        Ok(child) => {
            tracing::info!(
                target: "evolve",
                unit = %format!("{EVOLVE_UNIT_PREFIX}{pid}"),
                systemd_run_pid = ?child.id(),
                restart_units = ?targets.units,
                session_id = %sid,
                "evolve: systemd-run spawned; daemon will restart in 3s"
            );
            RestartStatus::Scheduled
        }
        Err(e) => {
            tracing::warn!(
                target: "evolve",
                unit = %format!("{EVOLVE_UNIT_PREFIX}{pid}"),
                restart_units = ?targets.units,
                error = %e,
                session_id = %sid,
                "evolve: failed to spawn systemd-run, the daemon will NOT auto-restart \
                 and a manual restart is required to load the new binary"
            );
            RestartStatus::SpawnFailed(e.to_string())
        }
    }
}

/// The `RestartReady` status, phrased so an unconfirmed version is never
/// rendered as one. Named apart from the `restart_status` module it must not
/// shadow.
pub(crate) fn restart_ready_status(current: &str, read_back: Option<&str>) -> String {
    let installed = match read_back {
        Some(v) => format!("v{v}"),
        None => "an unconfirmed version (brew reported nothing)".to_string(),
    };
    format!("Evolved via Homebrew: v{current} -> {installed}. Restarting now.")
}

/// The Homebrew evolve, with the brew executable and its budget injectable.
///
/// Both parameters exist for the same reason: the kill path and the read-back are
/// only provable end-to-end, and a test needs a stub script (no Homebrew on macOS
/// CI runners) driven by a budget short enough to overrun inside a test timeout.
/// Production callers go through [`evolve`], which supplies the real `brew` and
/// [`BREW_TIMEOUT`].
pub(crate) async fn evolve_with(
    sid: uuid::Uuid,
    current_version: &str,
    latest_version: &str,
    progress: Option<&ProgressCallback>,
    program: &str,
    budget: Duration,
) -> Result<ToolResult> {
    // `latest_version` is logged, never reported. It was picked up from the
    // GitHub release name before brew ran, so it is a hope, not a fact (#1779).
    tracing::info!(
        target: "evolve",
        current_version,
        latest_version,
        session_id = %sid,
        "evolve: running `brew upgrade {FORMULA}` (latest_version is the pre-run \
         GitHub release name, not a claim about what will be installed)"
    );

    // `brew update` first: upgrade resolves against the LOCAL formula index, so
    // without it brew can believe the installed version is already latest and
    // exit successfully having done nothing. Its failure is not fatal, and now
    // neither is its hang: both are logged and the upgrade proceeds on whatever
    // index is on disk.
    let update_problem = match run_bounded(program, &["update"], budget).await {
        Err(e) => Some(format!("could not spawn ({e})")),
        Ok(Outcome::TimedOut) => Some(format!(
            "outlived its {}s budget and was killed",
            budget.as_secs()
        )),
        Ok(Outcome::Exited(output)) if !output.status.success() => {
            Some(format!("exited {}", output.status))
        }
        Ok(Outcome::Exited(_)) => None,
    };
    if let Some(problem) = update_problem {
        tracing::warn!(
            target: "evolve",
            problem = %problem,
            session_id = %sid,
            "evolve: `brew update` did not succeed, continuing on the existing formula index"
        );
    }

    let output = match run_bounded(program, &["upgrade", FORMULA], budget).await {
        Err(e) => {
            tracing::warn!(
                target: "evolve",
                error = %e,
                session_id = %sid,
                "evolve: failed to spawn `brew` (is Homebrew on PATH?)"
            );
            return Err(ToolError::Execution(spawn_failure_message(&e.to_string())));
        }
        Ok(Outcome::TimedOut) => {
            tracing::warn!(
                target: "evolve",
                budget_secs = budget.as_secs(),
                session_id = %sid,
                "evolve: `brew upgrade` outlived its budget and was killed"
            );
            return Ok(ToolResult::error(timeout_message("upgrade", budget)));
        }
        Ok(Outcome::Exited(output)) => output,
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            target: "evolve",
            exit_status = %output.status,
            session_id = %sid,
            "evolve: `brew upgrade opencrabs` failed"
        );
        return Ok(ToolResult::error(upgrade_failure_message(&stderr)));
    }

    // Read the installed version back from brew, after the upgrade. This is the
    // only version the user is told about.
    let read_back = read_installed_version(program, budget).await;
    if read_back.is_none() {
        tracing::warn!(
            target: "evolve",
            session_id = %sid,
            "evolve: brew reported no installed version after the upgrade, so the success \
             line claims none instead of borrowing the pre-run release name"
        );
    }

    // Arm the systemd backstop before signalling the in-process restart, so a
    // restart that fails (or a process that dies exec'ing) still comes back.
    let restart = arm_systemd_backstop(sid);

    if let Some(cb) = progress {
        cb(
            sid,
            ProgressEvent::RestartReady {
                status: restart_ready_status(current_version, read_back.as_deref()),
                // brew replaced the Cellar binary and repointed the prefix
                // symlink, so the handler resolves the new one via current_exe().
                binary_path: None,
            },
        );
    }
    Ok(ToolResult::success(success_line(
        current_version,
        read_back.as_deref(),
        &restart,
    )))
}

pub(crate) async fn evolve(
    sid: uuid::Uuid,
    current_version: &str,
    latest_version: &str,
    progress: Option<&ProgressCallback>,
) -> Result<ToolResult> {
    evolve_with(
        sid,
        current_version,
        latest_version,
        progress,
        "brew",
        BREW_TIMEOUT,
    )
    .await
}
