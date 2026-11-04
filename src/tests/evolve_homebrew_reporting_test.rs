//! What the Homebrew evolve branch tells the user, pinned without spawning
//! anything: the version claim, the timeout message, and the source pins that
//! stop the #1779 defects from creeping back.
//!
//! The behavioural half (a child that really hangs, a stub that really plays
//! brew) is in `evolve_homebrew_supervision_test.rs`.

use crate::brain::tools::evolve::bounded_child::first_version_field;
use crate::brain::tools::evolve::homebrew;
use crate::brain::tools::evolve::restart_status::RestartStatus;
use std::time::Duration;

/// The evolve source, whitespace-collapsed so a pin survives re-indentation.
fn homebrew_flat() -> String {
    include_str!("../brain/tools/evolve/homebrew.rs")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn a_version_is_only_taken_from_a_real_two_field_line() {
    assert_eq!(
        first_version_field("opencrabs 0.5.4\n").as_deref(),
        Some("0.5.4")
    );
    assert_eq!(
        first_version_field("\n  opencrabs 1.2.3  \n").as_deref(),
        Some("1.2.3"),
        "leading blanks and padding are brew's output formatting, not missing data"
    );
    assert_eq!(first_version_field("opencrabs\n").as_deref(), None);
    assert_eq!(first_version_field("").as_deref(), None);
}

#[test]
fn the_timeout_message_names_the_kill_and_the_way_out() {
    let msg = homebrew::timeout_message("upgrade", Duration::from_secs(600));
    assert!(msg.contains("600s"), "the budget must be stated: {msg}");
    assert!(msg.contains("killed"), "the kill must be stated: {msg}");
    assert!(
        msg.contains("brew list --versions opencrabs"),
        "the read-back check must be offered: {msg}"
    );
    assert!(
        msg.contains("brew upgrade opencrabs"),
        "the retry command must be offered: {msg}"
    );
}

#[test]
fn the_success_line_renders_the_read_back_version_and_nothing_else() {
    let confirmed = homebrew::success_line("0.5.3", Some("9.9.9"), &RestartStatus::Scheduled);
    assert!(
        confirmed.contains("v0.5.3 -> v9.9.9"),
        "the read-back version must be the one shown: {confirmed}"
    );
    let unconfirmed = homebrew::success_line("0.5.3", None, &RestartStatus::Scheduled);
    assert!(
        !unconfirmed.contains("-> v"),
        "an unconfirmed read-back must never be rendered as a version: {unconfirmed}"
    );
    assert!(
        unconfirmed.contains("brew reported no installed version"),
        "the user must be told why no version is claimed: {unconfirmed}"
    );
}

#[test]
fn the_restart_ready_status_never_renders_an_unconfirmed_version() {
    assert_eq!(
        homebrew::restart_ready_status("0.5.3", Some("9.9.9")),
        "Evolved via Homebrew: v0.5.3 -> v9.9.9. Restarting now."
    );
    let unconfirmed = homebrew::restart_ready_status("0.5.3", None);
    assert!(unconfirmed.contains("unconfirmed"), "{unconfirmed}");
    assert!(
        !unconfirmed.contains("v0.5.4"),
        "the pre-run release name leaked into the status: {unconfirmed}"
    );
}

#[test]
fn the_pre_run_release_name_never_reaches_the_user_facing_tail() {
    // Everything after the read-back is what the user ends up reading, so
    // `latest_version` (the GitHub release name fetched before brew ran) must not
    // appear in it at all. This is the pin that keeps the defect from coming back
    // one "tidy" refactor at a time.
    let flat = homebrew_flat();
    let after = flat
        .split("let read_back = read_installed_version")
        .nth(1)
        .expect("the evolve still reads the installed version back from brew");
    // Stop before the production wrapper: it takes `latest_version` as a
    // parameter by design, and logging the hope is not the same as reporting it.
    let tail = after
        .split("pub(crate) async fn evolve(")
        .next()
        .unwrap_or(after);
    assert!(
        !tail.contains("latest_version"),
        "the pre-run release name is used again after the read-back: {tail}"
    );
    assert!(
        flat.contains("success_line( current_version, read_back.as_deref()"),
        "the success line must be fed the read-back"
    );
}

#[test]
fn no_brew_child_is_awaited_without_a_budget() {
    let src = include_str!("../brain/tools/evolve/homebrew.rs");
    let flat = homebrew_flat();
    assert!(
        !src.contains("output().await"),
        "a raw `.output().await` is an unbounded brew child; every one must go \
         through run_bounded"
    );
    assert!(
        flat.contains("run_bounded(program, &[\"update\"], budget)"),
        "brew update must obey the injected budget"
    );
    assert!(
        flat.contains("run_bounded(program, &[\"upgrade\", FORMULA], budget)"),
        "brew upgrade must obey the injected budget"
    );
    assert!(
        flat.contains("\"brew\", BREW_TIMEOUT,"),
        "the production entry point must supply the real brew and the real budget"
    );
}

#[test]
fn both_evolve_branches_arm_the_systemd_restart() {
    // The criterion is `rg -n build_systemd_restart_command
    // src/brain/tools/evolve` finding the call in the download branch AND the
    // Homebrew branch. include_str! is the same check with compile-time paths.
    let brew = include_str!("../brain/tools/evolve/homebrew.rs");
    let download = include_str!("../brain/tools/evolve/via_binary_download.rs");
    assert!(
        download.contains("build_systemd_restart_command"),
        "the download branch is the reference implementation for this pin"
    );
    assert!(
        brew.contains("build_systemd_restart_command"),
        "the Homebrew branch must arm the same systemd restart the download branch arms"
    );
    let flat = homebrew_flat();
    assert!(
        flat.contains("build_systemd_restart_command(pid, targets.user, &targets.units).spawn()"),
        "the brew branch must spawn the delayed restart, not merely build it, and it must \
         pass the resolved unit list rather than the discovery glob"
    );
    assert!(
        flat.contains("select_restart_targets(sid)"),
        "the branch must resolve the bus and the unit list instead of scheduling blind"
    );
    assert!(
        flat.contains("sweep_stale_evolve_units(targets.user, sid)"),
        "stale transient evolve units must be reset-failed before a fresh one is armed"
    );
    assert!(
        flat.contains("RestartStatus::NoUnitsMatched"),
        "zero matching units must be reported rather than silently scheduled (#136)"
    );
    assert!(
        flat.contains("RestartStatus::Unresolved"),
        "an unresolvable unit list must be reported rather than scheduled blind (#2081)"
    );
}

#[test]
fn the_user_bus_fallback_lives_in_the_shared_preflight() {
    // #162: an OpenCrabs daemon installed as a user service has zero system-level
    // units, and checking only the system bus is what produced the "Evolved! but
    // the daemon never restarted" reports. The rule now lives in one helper both
    // branches reach, so it cannot drift apart between them.
    let sys = include_str!("../brain/tools/evolve/systemd.rs");
    assert!(
        sys.contains("lookup_bus(SYSTEMD_UNIT_PATTERN, true)"),
        "the user bus must be checked before giving up"
    );
    assert!(
        sys.contains("using user-level units"),
        "the fallback must be logged so an operator can diagnose a silent restart"
    );
    // This pin used to require the words "scheduling restart anyway", i.e. an
    // unreadable unit list fell through to scheduling the glob. That fallback was
    // the same defect by another route: a pattern reaching `systemctl restart`
    // expands to the transient evolve unit once systemd-run has created it, so the
    // restart would stop its own executor (#2081). An unreadable list is now
    // reported as Unresolved instead.
    assert!(
        sys.contains("RestartPlan::Unresolved"),
        "an unreadable unit list must be reported, never satisfied with the discovery glob"
    );
}
