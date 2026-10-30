//! Sentinel tests for the `evolve` tool's systemd restart helpers.
//!
//! Context. PR #137 (#136) added a post-swap step that schedules a
//! delayed `systemctl restart` via `systemd-run`. Two gaps from the PR
//! review remained open after merge:
//!
//!   #3  Silent failure when no units match the glob: the agent
//!       would say "Evolved!" and the daemon would never restart
//!       (zero matching units), the exact symptom #136 was filed for.
//!   #6  No test coverage on the restart path, where flag drift in
//!       systemd-run args could silently break the restart again.
//!
//! These tests pin both: the systemd-run command construction stays
//! stable across refactors, AND the user-facing message for every
//! `RestartStatus` outcome clearly tells the user what actually
//! happened (or didn't).
//!
//! #2081 added a third invariant. The restart target is a resolved unit
//! list, never the discovery glob, because the transient unit the restart
//! runs inside (`opencrabs-evolve-<pid>`) also matches that glob. Passing
//! the glob made systemd SIGTERM the transient unit mid-job, retry until
//! `start-limit-hit`, and re-issue the restart once per retry.
//!
//! What we DON'T test: actual systemd interaction. Spawning real
//! `systemd-run` requires a Linux host with systemd running, which
//! isn't portable across CI (macOS + Windows have no systemd).
//! Construction + message shape is what we can pin portably; the
//! end-to-end behaviour was validated empirically by the PR author.

use crate::brain::tools::evolve::systemd::{
    EVOLVE_UNIT_GLOB, EVOLVE_UNIT_PREFIX, SYSTEMD_UNIT_PATTERN, build_systemd_cleanup_command,
    build_systemd_restart_command, parse_systemd_unit_names, restart_targets_from,
};

/// The resolved restart target list the tests hand the builder. Explicit
/// names, never the discovery glob (#2081).
fn resolved_targets() -> Vec<String> {
    vec![
        "opencrabs.service".to_string(),
        "opencrabs-staging.service".to_string(),
    ]
}

/// The arg vector of a built command, as owned strings.
fn args_of(cmd: &std::process::Command) -> Vec<String> {
    cmd.get_args()
        .map(|a| a.to_string_lossy().to_string())
        .collect()
}

#[test]
fn unit_pattern_is_glob_so_multiple_profiles_match() {
    // Adding a new profile (opencrabs-staging.service) must not
    // require a code change. The pattern is shipped as a public
    // const so refactors that hardcode "opencrabs.service" would
    // diverge from the tested invariant. It is the *discovery*
    // pattern: the resolver lists with it, and the restart receives
    // the names it resolved.
    assert_eq!(
        SYSTEMD_UNIT_PATTERN, "opencrabs*.service",
        "the glob must match every opencrabs-*.service variant; a non-glob value \
         would silently break multi-profile discovery"
    );
    assert_eq!(
        EVOLVE_UNIT_PREFIX, "opencrabs-evolve-",
        "the transient-unit prefix is the filter's contract; if it drifts, the \
         transient unit stops being recognised and re-enters the restart set"
    );
    assert!(
        "opencrabs-evolve-12345.service".starts_with(EVOLVE_UNIT_PREFIX),
        "the discovery glob matches the transient evolve unit, which is why the \
         glob can never be the restart target"
    );
}

#[test]
fn restart_command_uses_systemd_run_binary() {
    let cmd = build_systemd_restart_command(12345, false, &resolved_targets());
    assert_eq!(
        cmd.get_program(),
        "systemd-run",
        "command must invoke systemd-run, not systemctl directly, because only the \
         transient unit escapes the daemon cgroup"
    );
}

#[test]
fn restart_command_system_level_args_are_pinned() {
    let cmd = build_systemd_restart_command(12345, false, &resolved_targets());
    assert_eq!(
        args_of(&cmd),
        vec![
            "--on-active=3",
            "--unit=opencrabs-evolve-12345",
            "systemctl",
            "restart",
            "opencrabs.service",
            "opencrabs-staging.service",
        ],
        "system-level (user=false) arg list must not drift, since each flag's removal or rename re-introduces \
         a known regression mode: --on-active=3 = the 3s delivery window, \
         --unit=... = the PID-derived name that avoids concurrent-evolve collisions, \
         and the trailing names are the RESOLVED units, one per profile. A glob \
         here is the #2081 defect: it also matches the transient unit running the \
         restart. \
         NOTE: --collect and --quiet are intentionally absent (incompatible with \
         systemd < v240 on RHEL 7 / CentOS 7); do NOT re-add them without \
         confirming the minimum systemd version policy."
    );
}

#[test]
fn restart_command_user_level_includes_user_flag() {
    let cmd = build_systemd_restart_command(12345, true, &resolved_targets());
    assert_eq!(
        args_of(&cmd),
        vec![
            "--user",
            "--on-active=3",
            "--unit=opencrabs-evolve-12345",
            "systemctl",
            "--user",
            "restart",
            "opencrabs.service",
            "opencrabs-staging.service",
        ],
        "user-level (user=true) command must include --user on both systemd-run \
         (to connect to the user bus and create the timer in the user instance) \
         and systemctl (to target the user service manager)"
    );
}

#[test]
fn restart_command_unit_name_includes_pid() {
    // Concurrent evolve calls would collide on a fixed transient
    // unit name. The PID embedding makes the name unique per
    // process. Verify both that the PID appears verbatim AND that
    // different PIDs produce different names.
    let cmd_a = build_systemd_restart_command(12345, false, &resolved_targets());
    let cmd_b = build_systemd_restart_command(67890, false, &resolved_targets());
    let unit_a = args_of(&cmd_a)
        .into_iter()
        .find(|a| a.starts_with("--unit="))
        .expect("unit arg must exist");
    let unit_b = args_of(&cmd_b)
        .into_iter()
        .find(|a| a.starts_with("--unit="))
        .expect("unit arg must exist");
    assert_eq!(unit_a, "--unit=opencrabs-evolve-12345");
    assert_eq!(unit_b, "--unit=opencrabs-evolve-67890");
    assert_ne!(
        unit_a, unit_b,
        "two concurrent evolves on different PIDs must produce different unit names \
         or systemd-run will fail on the second one"
    );
}

// ── #2081: the transient unit must never be a restart target ────

#[test]
fn the_target_filter_removes_only_transient_evolve_units() {
    // Pure, so the filter is pinned without a systemd host.
    let candidates = vec![
        "opencrabs.service".to_string(),
        "opencrabs-evolve-1003.service".to_string(),
        "opencrabs-ops.service".to_string(),
        "opencrabs-evolve-9.service".to_string(),
    ];
    assert_eq!(
        restart_targets_from(&candidates),
        vec![
            "opencrabs.service".to_string(),
            "opencrabs-ops.service".to_string(),
        ],
        "the filter must drop every transient evolve unit and keep every real profile"
    );
    assert!(
        restart_targets_from(&[]).is_empty(),
        "no candidates must filter to no targets, not to a pattern"
    );
    assert_eq!(
        restart_targets_from(&["opencrabs-evolve-1.service".to_string()]),
        Vec::<String>::new(),
        "a candidate list of nothing but transient units must filter down to empty, \
         which the resolver reports as Unresolved rather than scheduling blind"
    );
}

#[test]
fn restart_command_drops_the_transient_unit_from_its_own_targets() {
    // The regression itself. The transient unit systemd-run creates is named
    // opencrabs-evolve-<pid>, which the discovery glob also matches. If it
    // reaches the restart target list, systemd SIGTERMs the unit that is
    // running the restart, retries until start-limit-hit, and re-issues the
    // restart once per retry, churning the daemon five times in a second.
    let mut candidates = resolved_targets();
    candidates.push("opencrabs-evolve-12345.service".to_string());
    let cmd = build_systemd_restart_command(12345, false, &candidates);
    let args = args_of(&cmd);
    assert!(
        !args.iter().any(|a| a.starts_with(EVOLVE_UNIT_PREFIX)),
        "the restart target list must not contain a transient evolve unit: {args:?}"
    );
    assert_eq!(
        args,
        vec![
            "--on-active=3",
            "--unit=opencrabs-evolve-12345",
            "systemctl",
            "restart",
            "opencrabs.service",
            "opencrabs-staging.service",
        ],
        "dropping the transient unit must not disturb the real targets"
    );
}

#[test]
fn the_unit_list_parser_reads_names_and_ignores_the_description() {
    // Real `systemctl list-units --no-legend --no-pager 'opencrabs*.service'`
    // shape: name, LOAD, ACTIVE, SUB, then a free-text description that may
    // contain spaces, dots and anything else. A failed unit carries a status
    // glyph in its own column ahead of the name.
    let stdout = "\
  opencrabs.service      loaded active running OpenCrabs daemon (profile default) - headless channel bot
  opencrabs-tui.service  loaded active running OpenCrabs TUI
● opencrabs-bad.service  loaded failed failed  OpenCrabs failed to start

";
    assert_eq!(
        parse_systemd_unit_names(stdout),
        vec![
            "opencrabs.service".to_string(),
            "opencrabs-tui.service".to_string(),
            "opencrabs-bad.service".to_string(),
        ],
        "the first column is the unit name; the description must never leak in, and \
         a leading status glyph must not shift a FAILED unit out of the listing"
    );
    assert!(
        parse_systemd_unit_names("").is_empty(),
        "an empty listing is an empty list, not a unit named nothing"
    );
    assert!(
        parse_systemd_unit_names("\n   \n").is_empty(),
        "blank lines carry no unit"
    );
}

// ── stale-unit cleanup (the accumulation fix) ───────────────────

#[test]
fn cleanup_uses_reset_failed_not_collect() {
    // We can't pass --collect to systemd-run (unsupported on systemd
    // < v240, RHEL 7 / CentOS 7), so spent transient units are swept
    // with `systemctl reset-failed <glob>` instead. reset-failed has
    // existed far longer than --collect, so it stays portable.
    let cmd = build_systemd_cleanup_command(false);
    assert_eq!(
        cmd.get_program(),
        "systemctl",
        "cleanup must use systemctl reset-failed, not systemd-run --collect"
    );
    assert_eq!(
        args_of(&cmd),
        vec!["reset-failed", "opencrabs-evolve-*.service"],
        "system-level cleanup must reset-failed exactly the evolve-unit glob. A glob \
         is safe and intended HERE: reset-failed acts on unit registrations, it never \
         starts or stops anything, so matching the transient unit is the point. A \
         wider glob could clear unrelated units, a narrower one would miss \
         accumulated failed restarts"
    );
}

#[test]
fn cleanup_glob_matches_restart_unit_names() {
    // The cleanup glob must match the per-PID unit names that
    // build_systemd_restart_command creates, or nothing gets swept.
    let restart = build_systemd_restart_command(12345, false, &resolved_targets());
    let unit = args_of(&restart)
        .into_iter()
        .find(|a| a.starts_with("--unit="))
        .expect("unit arg must exist");
    assert_eq!(unit, "--unit=opencrabs-evolve-12345");
    // EVOLVE_UNIT_GLOB is "opencrabs-evolve-*.service"; the unit name is
    // "opencrabs-evolve-12345" (systemd appends .service). Assert the
    // glob's fixed prefix matches so the two can never drift apart.
    let prefix = EVOLVE_UNIT_GLOB.trim_end_matches("*.service");
    assert_eq!(
        prefix, EVOLVE_UNIT_PREFIX,
        "the cleanup glob prefix and the transient-unit prefix are the same fact \
         written twice, and they must agree"
    );
    assert!(
        "opencrabs-evolve-12345".starts_with(prefix),
        "cleanup glob prefix must match the restart unit-name prefix"
    );
}

#[test]
fn cleanup_user_level_includes_user_flag() {
    let cmd = build_systemd_cleanup_command(true);
    assert_eq!(
        args_of(&cmd),
        vec!["--user", "reset-failed", "opencrabs-evolve-*.service"],
        "user-level cleanup must target the user manager with --user, matching \
         the user-level restart path"
    );
}

#[test]
fn evolve_sweeps_stale_units_before_scheduling_restart() {
    // Sentinel: the schedule path must call the cleanup before spawning
    // the restart. Removing it re-introduces the accumulation bug where
    // failed opencrabs-evolve-* units pile up forever.
    let src = include_str!("../brain/tools/evolve/via_binary_download.rs");
    assert!(
        src.contains("sweep_stale_evolve_units(targets.user, sid)"),
        "evolve must sweep stale evolve units (reset-failed) before scheduling a \
         new restart, or failed transient units accumulate without bound"
    );
}

// ── #2081: no branch may name the discovery pattern ────────────

#[test]
fn no_evolve_branch_hands_the_builder_the_discovery_glob() {
    // The #2081 defect in one assertion. The builder must receive resolved
    // names, so the discovery pattern must not appear in either branch at all:
    // the resolver in systemd.rs is its only owner. If a refactor reintroduces
    // it here, this fails before a glob can reach `systemctl restart`.
    for (name, src) in [
        (
            "via_binary_download.rs",
            include_str!("../brain/tools/evolve/via_binary_download.rs"),
        ),
        (
            "homebrew.rs",
            include_str!("../brain/tools/evolve/homebrew.rs"),
        ),
    ] {
        assert!(
            src.contains("build_systemd_restart_command(pid, targets.user, &targets.units)"),
            "{name} must pass the resolved unit list to the builder"
        );
        assert!(
            !src.contains("SYSTEMD_UNIT_PATTERN"),
            "{name} must not reference the discovery pattern; resolving it is \
             select_restart_targets's job (#2081)"
        );
    }
}

// ── User-message coverage for every RestartStatus branch ────────

#[test]
fn restart_status_messages_are_distinct_per_outcome() {
    let src = include_str!("../brain/tools/evolve/restart_status.rs");
    assert!(
        src.contains("Evolved from v{current} to v{latest}."),
        "the Scheduled branch must confirm the evolve completed"
    );
    assert!(
        src.contains("Binary updated on disk; restart"),
        "the NotSystemd branch must tell the user the binary is updated but they \
         need to restart"
    );
    assert!(
        src.contains("no \\\n                 systemd units matched")
            || src.contains("no systemd units matched"),
        "the NoUnitsMatched branch must explicitly call out the zero-units case, \
         because silently saying 'Restarting' here is the #136 regression"
    );
    assert!(
        src.contains("scheduling the systemd restart failed"),
        "the SpawnFailed branch must quote the actual error so the user knows \
         systemd-run couldn't fire"
    );
    assert!(
        src.contains("the systemd unit list could not be read"),
        "the Unresolved branch must say the list was unreadable, which is a \
         different fact from 'nothing matched' (#2081)"
    );
}

#[test]
fn the_manual_restart_advice_never_hands_the_user_the_glob() {
    // The message used to tell the user to run
    // `systemctl --user restart opencrabs*.service`, which is the #2081 defect
    // handed to a human: that glob also matches the transient evolve unit. The
    // glob may appear only as a list-units argument, where it is a safe filter.
    let src = include_str!("../brain/tools/evolve/restart_status.rs");
    assert!(
        !src.contains("restart {SYSTEMD_UNIT_PATTERN}"),
        "the manual-restart advice must name the unit, not the discovery glob"
    );
    assert!(
        src.contains("systemctl --user restart <your-unit>"),
        "the advice must still give a runnable --user form"
    );
    assert!(
        src.contains("list-units '{SYSTEMD_UNIT_PATTERN}'"),
        "the glob must still be offered where it is safe, as a listing filter"
    );
}

#[test]
fn no_units_matched_message_mentions_user_flag() {
    // The user-facing message should guide the user toward
    // `systemctl --user restart` as well as the system variant.
    let src = include_str!("../brain/tools/evolve/restart_status.rs");
    assert!(
        src.contains("systemctl --user restart"),
        "NoUnitsMatched user message must mention --user restart as an option"
    );
}

#[test]
fn spawn_failed_message_mentions_user_flag() {
    let src = include_str!("../brain/tools/evolve/via_binary_download.rs");
    assert!(
        src.contains("systemctl --user restart"),
        "SpawnFailed user message must mention --user restart as an option"
    );
}

#[test]
fn evolve_falls_back_to_user_level_when_system_level_empty() {
    // The core fix in PR #162: when system-level units return zero,
    // evolve must check user-level units before giving up.
    // The fallback now lives in the shared resolver, so the sentinel
    // checks that both branches reach it and that it checks the user bus.
    let download = include_str!("../brain/tools/evolve/via_binary_download.rs");
    assert!(
        download.contains("select_restart_targets(sid)"),
        "evolve must go through the shared resolver, which owns the user-bus \
         fallback; bypassing it re-introduces the 'evolve said success but the \
         daemon didn't restart' bug (#136)"
    );
    let sys = include_str!("../brain/tools/evolve/systemd.rs");
    assert!(
        sys.contains("lookup_bus(SYSTEMD_UNIT_PATTERN, true)"),
        "the resolver must check the user bus before giving up"
    );
}

#[test]
fn evolve_logs_user_level_unit_count_on_fallback() {
    // When the fallback triggers, evolve must log the user-level unit
    // names so operators can debug "why didn't my daemon restart".
    let sys = include_str!("../brain/tools/evolve/systemd.rs");
    assert!(
        sys.contains("using user-level units"),
        "the resolver must log the user-level fallback for debugging; silent \
         fallbacks make #136-style issues impossible to diagnose"
    );
    let download = include_str!("../brain/tools/evolve/via_binary_download.rs");
    assert!(
        download.contains("restart_units"),
        "the restart log must name the units actually targeted, so a wrong unit \
         list is visible in the journal"
    );
}
