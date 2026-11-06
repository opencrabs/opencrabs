//! The Homebrew evolve branch is supervised: brew's children are bounded, killed
//! when they overrun, and the version the user is told is read back from brew
//! after the upgrade (#1779).
//!
//! These are the spawning half of that work: a real child that really hangs, and
//! a stub `/bin/sh` playing brew. The wording and source pins that go with it
//! live in `evolve_homebrew_reporting_test.rs`, and the systemd arg lists in
//! `evolve_systemd_restart_test.rs`.
//!
//! What we DON'T test: real systemd interaction and a real Homebrew install.
//! Neither exists on macOS runners, and `arm_systemd_backstop` is private on
//! purpose so no test can reach for it by accident.
//!
//! Every test here is unix-only: the stubs are `/bin/sh` scripts and
//! `PermissionsExt` does not exist on Windows.

#![cfg(unix)]

use crate::brain::tools::evolve::bounded_child::{Outcome, run_bounded};
use crate::brain::tools::evolve::homebrew;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

/// Write an executable `/bin/sh` stub and return its path.
///
/// The stub is the whole trick: the evolve takes the program to run as a
/// parameter, so a shell script can play brew and decide for itself what
/// "installed" means, on a host that has never heard of Homebrew.
fn stub(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"#!/bin/sh\n").unwrap();
    file.write_all(body.as_bytes()).unwrap();
    file.flush().unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_str().unwrap().to_string()
}

#[tokio::test]
async fn a_child_that_outlives_its_budget_is_killed_not_left_running() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ticks");
    // The budget is deliberately generous: the child cannot prove it ran until
    // the OS has spawned `sh` and executed its first statement, and spawn
    // latency is not bounded on a loaded host. Racing that measured scheduling
    // pressure instead of supervision (#2653).
    const BUDGET_MS: u64 = 1500;
    // 2000 laps at 0.05s is 100s of work, far past any budget.
    let script = format!(
        "i=0; while [ $i -lt 2000 ]; do i=$((i+1)); echo $i >> {}; sleep 0.05; done",
        marker.display()
    );
    let started = std::time::Instant::now();
    let outcome = run_bounded("sh", &["-c", &script], Duration::from_millis(BUDGET_MS))
        .await
        .expect("`sh` spawns on every unix CI host, so a failure here is not a result");
    let elapsed = started.elapsed();

    assert!(
        matches!(outcome, Outcome::TimedOut),
        "the budget expired, so the child must be reported as TimedOut, got {outcome:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the budget ended the wait, not the child's own 100s loop: {elapsed:?}"
    );

    // The kill has to be real, not an abandoned future. Measure GROWTH after the
    // budget rather than an absolute count: a killed child freezes the marker
    // while an orphan keeps appending ~30 lines per 1.5s at this lap time.
    let count = || {
        std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
    };
    let at_budget = count();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let after_window = count();
    assert!(
        at_budget >= 1,
        "the child ran at all before the budget expired (saw {at_budget} ticks)"
    );
    assert!(
        after_window - at_budget <= 5,
        "the child was killed rather than orphaned: the marker grew from {at_budget} to \
         {after_window} ticks after a {BUDGET_MS}ms budget (an orphan adds ~30 per \
         1.5s and reaches 2000)"
    );
}

#[tokio::test]
async fn the_evolve_reports_a_killed_upgrade_instead_of_hanging() {
    let dir = tempfile::tempdir().unwrap();
    let brew = stub(
        dir.path(),
        "brew",
        "case \"$1\" in\n  update) exit 0 ;;\n  *) i=0; while [ $i -lt 2000 ]; do \
         i=$((i+1)); sleep 0.05; done ;;\nesac\n",
    );
    let started = std::time::Instant::now();
    let result = homebrew::evolve_with(
        uuid::Uuid::new_v4(),
        "0.5.3",
        "0.5.4",
        None,
        &brew,
        Duration::from_millis(300),
    )
    .await
    .expect("a killed child is a reported tool error, not a spawn failure");
    let elapsed = started.elapsed();

    assert!(
        !result.success,
        "a killed upgrade must never report success: {}",
        result.output
    );
    let err = result.error.expect("the timeout is reported as an error");
    assert!(
        err.contains("killed"),
        "the message must name the kill: {err}"
    );
    assert!(
        err.contains("brew list --versions opencrabs"),
        "the message must name the check that resolves the unknown state: {err}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the evolve returned on the budget, not on the stub's 100s loop: {elapsed:?}"
    );
}

#[tokio::test]
async fn a_finished_child_hands_its_stdout_and_stderr_to_the_caller() {
    // The regression this exists for: tokio's `Command::spawn` leaves stdout and
    // stderr INHERITED unless they are piped explicitly, so a `wait_with_output`
    // collector sees an empty `Vec` while the child's text goes to the parent's
    // own streams. In that shape every brew message reached the daemon log and the
    // evolve saw nothing at all: an empty failure quote, and a version read-back of
    // `None` forever. Measured 2026-09-28 via the read-back test below.
    let dir = tempfile::tempdir().unwrap();
    let brew = stub(
        dir.path(),
        "brew",
        "echo \"opencrabs 9.9.9\"\necho ohno >&2\nexit 7\n",
    );
    let outcome = run_bounded(
        &brew,
        &["list", "--versions", "opencrabs"],
        Duration::from_secs(20),
    )
    .await
    .expect("a shell script in a tempdir spawns on every unix CI host");
    match outcome {
        Outcome::Exited(out) => {
            assert!(
                !out.status.success(),
                "the stub's exit 7 must reach the caller, not be swallowed"
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "opencrabs 9.9.9\n",
                "stdout must be captured, not inherited"
            );
            assert_eq!(
                String::from_utf8_lossy(&out.stderr),
                "ohno\n",
                "stderr must be captured too, or `upgrade_failure_message` quotes nothing"
            );
        }
        Outcome::TimedOut => panic!("two echoes must not overrun a 20s budget"),
    }
}

#[tokio::test]
async fn the_installed_version_is_read_back_from_brews_own_report() {
    let dir = tempfile::tempdir().unwrap();
    let brew = stub(dir.path(), "brew", "echo \"opencrabs 9.9.9\"\n");
    let read = homebrew::read_installed_version(&brew, Duration::from_secs(20)).await;
    assert_eq!(
        read.as_deref(),
        Some("9.9.9"),
        "the version must be whatever brew reports, not a release name fetched before it ran"
    );
}

#[tokio::test]
async fn a_read_back_that_is_empty_failing_or_hung_yields_no_version() {
    let dir = tempfile::tempdir().unwrap();
    // `brew list --versions` prints the bare formula name when nothing is kegged,
    // and exits non-zero when the formula is unknown to brew entirely.
    let bare = stub(dir.path(), "bare", "echo opencrabs\n");
    let missing = stub(
        dir.path(),
        "missing",
        "echo \"Error: no such formula\" >&2\nexit 1\n",
    );
    let hung = stub(
        dir.path(),
        "hung",
        "i=0; while [ $i -lt 2000 ]; do i=$((i+1)); sleep 0.05; done\n",
    );
    assert_eq!(
        homebrew::read_installed_version(&bare, Duration::from_secs(20)).await,
        None,
        "a bare name is brew saying it has no keg"
    );
    assert_eq!(
        homebrew::read_installed_version(&missing, Duration::from_secs(20)).await,
        None,
        "a failing read-back is no data, not guessed data"
    );
    assert_eq!(
        homebrew::read_installed_version(&hung, Duration::from_millis(300)).await,
        None,
        "a read-back that hangs must not become a hang: the budget kills it"
    );
}
