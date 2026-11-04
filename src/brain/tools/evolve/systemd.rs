//! systemd units for the evolve restart path.
//!
//! Split out of `evolve.rs` (#963 review): these are self-contained command
//! builders with no dependency on the tool or its strategies, and pinning
//! their arg lists in tests is the whole point: silent drift in any flag
//! re-introduces the "Evolved! but the daemon never restarted" symptom.
//!
//! Also holds [`select_restart_targets`], the pre-flight-and-arm sequence,
//! since that is the only consumer of the builders and it belongs with them
//! rather than inside one strategy file. It was added for the Homebrew branch
//! (#1779), which previously armed no backstop at all and relied on the
//! in-process `exec()` alone.

/// Service-unit glob used to *discover* candidate units for the systemd
/// restart path. Matches every profile (default, ops, staging, ...) sharing
/// the same binary.
///
/// This is a listing pattern only. It is never handed to `systemctl restart`:
/// the scheduled restart runs inside a transient unit of our own, and that
/// unit matches this glob, so a glob used as the restart target contains its
/// own executor (see [`EVOLVE_UNIT_PREFIX`]).
pub(crate) const SYSTEMD_UNIT_PATTERN: &str = "opencrabs*.service";

/// Prefix of the transient unit that performs a scheduled restart. Its unit
/// name is `opencrabs-evolve-<pid>` (systemd appends `.service`), which
/// [`SYSTEMD_UNIT_PATTERN`] also matches.
///
/// That overlap is why the restart target is a resolved unit list and not the
/// glob. Passing the glob made the transient unit stop itself 16 ms in,
/// systemd retried until `start-limit-hit`, and every retry re-issued the
/// restart job, so the daemon was churned five times inside one second and a
/// failed transient unit was left behind until reboot (#2081).
pub(crate) const EVOLVE_UNIT_PREFIX: &str = "opencrabs-evolve-";

/// A resolved restart target: which bus, and the exact units to restart.
pub(crate) struct RestartTargets {
    /// `true` to target user-level units (`systemctl --user`), e.g. when
    /// OpenCrabs was installed via `install_systemd_service()` which writes
    /// to `~/.config/systemd/user/`.
    pub(crate) user: bool,
    /// Explicit unit names. Never a pattern.
    pub(crate) units: Vec<String>,
}

/// Outcome of the pre-flight that decides what an evolve restart should target.
pub(crate) enum RestartPlan {
    /// Units were resolved and the restart should be armed.
    Targets(RestartTargets),
    /// systemctl answered and no opencrabs unit is registered on either bus.
    /// Scheduling would be a no-op, so the caller reports it instead (#136).
    NoUnitsMatched,
    /// systemctl could not be queried, so no unit name could be resolved.
    /// Distinct from [`Self::NoUnitsMatched`]: there we know there is nothing
    /// to restart, here we could not ask. A pattern is not a safe stand-in
    /// (#2081), so the caller reports this rather than scheduling blind.
    Unresolved,
}

/// Build the `systemd-run` command that schedules a delayed restart of the
/// given units. Extracted so the arg list can be pinned by tests, since silent
/// drift in any of these flags would re-introduce the "Evolved! but daemon
/// didn't restart" symptom that issue #136 reported.
///
/// Set `user` to `true` to target user-level units (`systemctl --user`),
/// e.g. when OpenCrabs was installed via `install_systemd_service()` which
/// writes to `~/.config/systemd/user/`.
///
/// The `pid` argument is used to derive a unique transient unit
/// name (`opencrabs-evolve-<pid>`) so concurrent evolve calls don't
/// collide on the transient unit registry.
///
/// `units` is the already-resolved target list. Entries carrying
/// [`EVOLVE_UNIT_PREFIX`] are dropped here as well: the caller filters them
/// while resolving, and repeating the filter at the boundary means a future
/// caller cannot arm a restart that includes the unit running it (#2081).
pub(crate) fn build_systemd_restart_command(
    pid: u32,
    user: bool,
    units: &[String],
) -> std::process::Command {
    let unit_name = format!("{EVOLVE_UNIT_PREFIX}{pid}");
    let mut cmd = std::process::Command::new("systemd-run");
    let mut args = vec![];
    // --user on systemd-run itself is required when the daemon runs as a
    // user service: without it, systemd-run tries to talk to the system
    // bus and either fails (no permission from within a --user service)
    // or creates the transient timer in the system instance, where the
    // spawned systemctl won't have DBUS_SESSION_BUS_ADDRESS available.
    if user {
        args.push("--user".to_string());
    }
    args.push("--on-active=3".to_string());
    args.push(format!("--unit={unit_name}"));
    args.push("systemctl".to_string());
    // --user on systemctl is needed to target the user service manager.
    if user {
        args.push("--user".to_string());
    }
    args.push("restart".to_string());
    let targets = restart_targets_from(units);
    if targets.len() != units.len() {
        tracing::warn!(
            target: "evolve",
            dropped = units.len() - targets.len(),
            "evolve: dropped a transient evolve unit from the restart target list, \
             it is the unit this restart runs inside"
        );
    }
    args.extend(targets);
    cmd.args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// Drop every candidate that is itself a transient evolve restart unit.
///
/// Pure, so the filter is testable without systemd. [`SYSTEMD_UNIT_PATTERN`]
/// matches `opencrabs-evolve-<pid>.service`, and a restart set that contains
/// the unit running the restart makes systemd SIGTERM the executor mid-job
/// (#2081).
pub(crate) fn restart_targets_from(candidates: &[String]) -> Vec<String> {
    candidates
        .iter()
        .filter(|unit| !unit.trim_start().starts_with(EVOLVE_UNIT_PREFIX))
        .cloned()
        .collect()
}

/// Glob matching every transient evolve restart unit we have ever
/// scheduled. `build_systemd_restart_command` embeds the scheduling
/// process PID in each unit name (`opencrabs-evolve-<pid>`), so over a
/// host's lifetime many such units are created, one per evolve / auto
/// update attempt.
pub(crate) const EVOLVE_UNIT_GLOB: &str = "opencrabs-evolve-*.service";

/// Build the command that garbage-collects spent evolve restart units.
///
/// We cannot pass `--collect` to `systemd-run` (it's unsupported on
/// systemd < v240, RHEL 7 / CentOS 7), so a finished transient
/// `opencrabs-evolve-<pid>` unit lingers in systemd's registry, and a
/// restart that *fails* (e.g. it lost the channel-token race against a
/// running TUI) lingers in the **failed** state, accumulating forever.
/// `systemctl reset-failed <glob>` clears those spent units and is
/// available on every systemd version we target, so we call it right
/// before scheduling a fresh restart. Best-effort: its failure must
/// never block the evolve.
pub(crate) fn build_systemd_cleanup_command(user: bool) -> std::process::Command {
    let mut cmd = std::process::Command::new("systemctl");
    if user {
        cmd.arg("--user");
    }
    cmd.arg("reset-failed")
        .arg(EVOLVE_UNIT_GLOB)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// List the systemd service units matching the given glob pattern, at either
/// system or user level.
///
/// Set `user` to `true` to query user-level units (`systemctl --user`).
///
/// Returns `Some(names)` on a successful query, which may be empty, or `None`
/// if `systemctl` failed to spawn / returned a non-zero exit status (a
/// permissions issue or non-systemd host). `None` is a "don't know" signal and
/// is deliberately distinct from `Some(vec![])`: a confirmed empty list means
/// no daemon is registered, while an unknown list means we could not ask.
///
/// Uses `--no-legend --no-pager` to keep stdout machine-parseable.
pub(crate) fn list_matching_systemd_units(pattern: &str, user: bool) -> Option<Vec<String>> {
    let mut cmd = std::process::Command::new("systemctl");
    cmd.args(["list-units", "--no-legend", "--no-pager"]);
    if user {
        cmd.arg("--user");
    }
    cmd.arg(pattern);
    cmd.stderr(std::process::Stdio::null());
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_systemd_unit_names(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Parse `systemctl list-units --no-legend --no-pager` output into unit names.
///
/// One unit per line: the name, then LOAD / ACTIVE / SUB / DESCRIPTION. The
/// name is the first token that looks like a service unit, not simply the
/// first token, because systemd prints a status glyph (`●` for a failed unit)
/// in its own column ahead of the name. Reading the first token literally
/// would silently drop every failed unit, which is exactly the population this
/// listing exists to find. The description is free text and may contain
/// anything, but it comes after the name, so the first `.service` token is the
/// name. Blank lines yield nothing.
pub(crate) fn parse_systemd_unit_names(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            line.split_whitespace()
                .find(|token| token.ends_with(".service"))
        })
        .map(|name| name.to_string())
        .collect()
}

/// The result of looking for restart candidates on one bus.
enum BusLookup {
    /// Real units to restart, with transient evolve units already filtered out.
    Units(Vec<String>),
    /// systemctl could not answer. Must not be read as "no units here".
    Unknown,
    /// systemctl answered and there is nothing on this bus to restart.
    Empty,
}

fn lookup_bus(pattern: &str, user: bool) -> BusLookup {
    match list_matching_systemd_units(pattern, user) {
        None => BusLookup::Unknown,
        Some(candidates) => {
            let units = restart_targets_from(&candidates);
            if units.is_empty() {
                BusLookup::Empty
            } else {
                BusLookup::Units(units)
            }
        }
    }
}

/// Resolve which bus an evolve restart should target and the exact units to
/// restart on it.
///
/// Both evolve branches (binary download, Homebrew) pre-flight through here so
/// the bus rules cannot drift apart.
///
/// The user-bus fallback is the substance of #162: OpenCrabs installed as a
/// user service (`install_systemd_service()` writes to
/// `~/.config/systemd/user/`) has zero system-level units, and checking only
/// the system bus is what produced the "Evolved! but the daemon never
/// restarted" line of reports (#136).
///
/// Transient evolve units are filtered out of the result by
/// [`restart_targets_from`]: they match the discovery glob, and one of them is
/// the unit this very restart will run inside (#2081).
///
/// An unreadable unit list returns [`RestartPlan::Unresolved`] rather than
/// scheduling the glob as a fallback. The old code scheduled blind in that
/// case, which is the same defect by another route: a pattern reaching
/// `systemctl restart` can expand to the transient unit once systemd-run has
/// created it three seconds later.
pub(crate) fn select_restart_targets(sid: uuid::Uuid) -> RestartPlan {
    match lookup_bus(SYSTEMD_UNIT_PATTERN, false) {
        BusLookup::Units(units) => {
            tracing::info!(
                target: "evolve",
                pattern = SYSTEMD_UNIT_PATTERN,
                matched_units = units.len(),
                use_user_units = false,
                session_id = %sid,
                "evolve: pre-flight found system-level units, scheduling restart (+3s)"
            );
            return RestartPlan::Targets(RestartTargets { user: false, units });
        }
        BusLookup::Unknown => {
            tracing::warn!(
                target: "evolve",
                pattern = SYSTEMD_UNIT_PATTERN,
                session_id = %sid,
                "evolve: could not list system-level units (systemctl query failed), \
                 no restart target could be resolved"
            );
            return RestartPlan::Unresolved;
        }
        BusLookup::Empty => {}
    }

    match lookup_bus(SYSTEMD_UNIT_PATTERN, true) {
        BusLookup::Units(units) => {
            tracing::info!(
                target: "evolve",
                pattern = SYSTEMD_UNIT_PATTERN,
                user_units = units.len(),
                session_id = %sid,
                "evolve: no system-level units found, using user-level units, \
                 scheduling restart with --user"
            );
            return RestartPlan::Targets(RestartTargets { user: true, units });
        }
        BusLookup::Unknown => {
            tracing::warn!(
                target: "evolve",
                pattern = SYSTEMD_UNIT_PATTERN,
                session_id = %sid,
                "evolve: could not list user-level units (systemctl query failed), \
                 no restart target could be resolved"
            );
            return RestartPlan::Unresolved;
        }
        BusLookup::Empty => {}
    }

    tracing::warn!(
        target: "evolve",
        pattern = SYSTEMD_UNIT_PATTERN,
        session_id = %sid,
        "evolve: no systemd units matched the pattern (checked system and user level), \
         skipping scheduled restart"
    );
    RestartPlan::NoUnitsMatched
}

/// Sweep spent transient evolve units before scheduling a fresh one.
///
/// Best-effort and deliberately silent about failure: a `reset-failed` that
/// cannot run must never block the restart that matters. Without it, failed
/// transient units accumulate without bound across evolves.
pub(crate) fn sweep_stale_evolve_units(use_user_units: bool, sid: uuid::Uuid) {
    match build_systemd_cleanup_command(use_user_units).status() {
        Ok(status) => tracing::debug!(
            target: "evolve",
            glob = EVOLVE_UNIT_GLOB,
            success = status.success(),
            session_id = %sid,
            "evolve: reset-failed swept stale evolve units"
        ),
        Err(e) => tracing::warn!(
            target: "evolve",
            glob = EVOLVE_UNIT_GLOB,
            error = %e,
            session_id = %sid,
            "evolve: could not sweep stale evolve units, the restart still proceeds"
        ),
    }
}
