use std::time::{Duration, Instant};

use crate::remote::state_lock::*;

/// flock is per-open-description (and winlock's range locks are
/// per-handle): a second lock attempt from THIS process contends
/// exactly like one from a second process, so the budget paths are
/// testable without spawning anything.
#[test]
fn contended_lock_is_acquired_within_budget_after_holder_releases() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.lock");
    let held = exclusive_wait(&path, WAIT).expect("first acquire");
    let release_at = Instant::now() + Duration::from_millis(60);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        drop(held);
    });
    // Budget comfortably covers the 60ms hold; success, not a bail.
    let got = exclusive_wait(&path, WAIT).expect("must win the wait loop");
    assert!(
        Instant::now() >= release_at,
        "acquiring before the holder released would mean the lock is fake"
    );
    drop(got);
    releaser.join().unwrap();
}

#[test]
fn contended_lock_fails_with_a_clear_error_after_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.lock");
    let _held = exclusive_wait(&path, WAIT).unwrap();
    let err = exclusive_wait(&path, Duration::from_millis(50)).expect_err("must give up cleanly");
    assert!(
        err.to_string().contains("held by another writer"),
        "error should name contention, got: {err}"
    );
}
