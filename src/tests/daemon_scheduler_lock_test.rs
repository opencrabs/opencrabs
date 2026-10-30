//! A failed A2A bind must not leave the daemon holding the cron lock, and must
//! not borrow the plain success line (#2096).
//!
//! Lock expectations run against a `TempDir` through
//! `acquire_scheduler_lock_in`, the dir-injectable core, so nothing here touches
//! the live `~/.opencrabs/locks/`.

use crate::cli::ui::{cron_blocked_by_a2a, daemon_start_line, scheduler_lock_for_start};
use crate::config::profile::acquire_scheduler_lock_in;
use std::cell::Cell;

fn lock_dir() -> tempfile::TempDir {
    tempfile::TempDir::new().expect("temp dir")
}

/// A blocked start takes no lock at all, and releases the one it was handed.
///
/// This is the #2096 receipt: the failed daemon held
/// `locks/scheduler/<profile>.lock` for its whole life while polling no job.
#[test]
fn a_blocked_start_holds_no_scheduler_lock() {
    let dir = lock_dir();
    let inherited = acquire_scheduler_lock_in(dir.path(), "default")
        .expect("the test needs a free lock dir to hand over");
    let acquire_calls = Cell::new(0);

    let taken = scheduler_lock_for_start(true, Some(inherited), || {
        acquire_calls.set(acquire_calls.get() + 1);
        None
    });

    assert!(taken.is_none(), "a start with no scheduler takes no lock");
    assert_eq!(
        acquire_calls.get(),
        0,
        "a blocked start must not even try to acquire the lock"
    );
    assert!(
        acquire_scheduler_lock_in(dir.path(), "default").is_some(),
        "the blocked start must release the lock it was handed, so another process can schedule"
    );
}

/// The ordinary start keeps the lock a multi-profile daemon already holds for
/// this profile, and does not acquire a second one.
#[test]
fn an_unblocked_start_keeps_the_inherited_lock() {
    let dir = lock_dir();
    let inherited = acquire_scheduler_lock_in(dir.path(), "default").expect("free lock dir");

    let taken = scheduler_lock_for_start(false, Some(inherited), || {
        panic!("an inherited lock must not trigger a second acquisition")
    });

    assert!(
        taken.is_some(),
        "the inherited lock is the one this start holds"
    );
    assert!(
        acquire_scheduler_lock_in(dir.path(), "default").is_none(),
        "the lock is still held by this start"
    );
}

/// With nothing inherited, the ordinary start acquires the lock as before.
#[test]
fn an_unblocked_start_acquires_when_nothing_was_inherited() {
    let dir = lock_dir();
    let path = dir.path().to_path_buf();

    let taken =
        scheduler_lock_for_start(false, None, || acquire_scheduler_lock_in(&path, "default"));

    assert!(taken.is_some(), "the start owns scheduling for the profile");
    assert!(
        acquire_scheduler_lock_in(dir.path(), "default").is_none(),
        "and nothing else can take it while this start lives"
    );
}

/// The coupling itself: only a configured gateway that never became ready
/// blocks the scheduler.
#[test]
fn only_an_enabled_unready_gateway_blocks_the_scheduler() {
    assert!(
        !cron_blocked_by_a2a(false, false),
        "A2A off is never a blocker"
    );
    assert!(
        !cron_blocked_by_a2a(false, true),
        "A2A off, readiness irrelevant"
    );
    assert!(!cron_blocked_by_a2a(true, true), "A2A up is not a blocker");
    assert!(
        cron_blocked_by_a2a(true, false),
        "enabled but not ready blocks"
    );
}

/// A degraded start never prints the plain success line.
#[test]
fn a_degraded_start_names_the_failed_bind_and_the_lost_cron() {
    let plain = daemon_start_line(false, "127.0.0.1", 18790);
    assert_eq!(plain, "🦀 OpenCrabs daemon running. Press Ctrl+C to stop.");

    let degraded = daemon_start_line(true, "127.0.0.1", 18790);
    assert_ne!(
        degraded, plain,
        "a failed bind must not claim a normal start"
    );
    assert!(
        !degraded.contains("daemon running."),
        "not the success line"
    );
    assert!(
        degraded.contains("degraded"),
        "the state is named: {degraded}"
    );
    assert!(
        degraded.contains("127.0.0.1:18790"),
        "the operator needs the address that failed: {degraded}"
    );
    assert!(
        degraded.contains("cron scheduler is off"),
        "and what it costs: {degraded}"
    );
}
