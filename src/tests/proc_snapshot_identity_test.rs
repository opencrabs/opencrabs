//! A pid is not a process: identity is pid AND start time (#3219).
//!
//! The kernel reuses pids. A build that exited and an unrelated process that
//! inherited its number carry the same pid, and a tracker keyed on the number
//! alone would close the first row on the death of the second, or keep it
//! open forever. [`ProcId`] therefore pairs the pid with `ps`'s start time,
//! and [`same_process`] compares both. This file pins that pairing on
//! constructed rows so it holds on any machine, not only one whose live table
//! happens to contain the shape.

use opencrabs::utils::proc_snapshot::{ProcEntry, ProcId, is_alive, same_process};

#[test]
fn a_reused_pid_is_not_the_tracked_process() {
    // Same pid, different start time: the tracked process exited and the
    // kernel handed the number to something else. The pairing must say so.
    let tracked = ProcId {
        pid: 900,
        start_time: "Fri Nov 27 23:05:39 2026".to_string(),
    };
    let reused = ProcEntry {
        id: ProcId {
            pid: 900,
            start_time: "Sat Nov 28 00:41:02 2026".to_string(),
        },
        pgid: 900,
        command: "unrelated now-owning pid 900".to_string(),
    };
    assert!(
        !same_process(&tracked, &reused),
        "a pid whose start time changed must not be treated as the same process"
    );
    assert!(
        !is_alive(&[reused], &tracked),
        "is_alive must agree with same_process: a reused pid does not keep the row open"
    );
}

#[test]
fn the_same_pid_and_start_time_is_the_same_process() {
    // The positive case, so the rule above cannot be satisfied by rejecting
    // everything: a genuine match must still be recognized.
    let tracked = ProcId {
        pid: 900,
        start_time: "Fri Nov 27 23:05:39 2026".to_string(),
    };
    let seen = ProcEntry {
        id: ProcId {
            pid: 900,
            start_time: "Fri Nov 27 23:05:39 2026".to_string(),
        },
        pgid: 900,
        command: "cargo build --release".to_string(),
    };
    assert!(same_process(&tracked, &seen));
    assert!(is_alive(&[seen], &tracked));
}
