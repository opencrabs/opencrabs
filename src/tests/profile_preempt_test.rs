//! TUI-priority preemption: when the interactive TUI starts while a
//! background instance of the same profile already holds the channel token
//! locks (e.g. an `opencrabs daemon` auto-started by systemd on boot), the
//! TUI shuts that instance down so it can own the channels itself.
//!
//! SAFETY: the preemption sends real SIGTERM/SIGKILL to every live PID it
//! finds in the lock directory. A test must therefore NEVER run it against
//! the real `~/.opencrabs/locks/` — doing so kills the user's own running
//! instances (regression: a `cargo test` run killed the live TUI). These
//! tests exercise the internal `preempt_instances_in`, which takes an explicit
//! lock dir, against a TempDir only, with system-service stopping disabled.

#![cfg(unix)]

use crate::config::profile::{
    active_profile, is_pid_alive, parse_launchctl_labels, preempt_instances_in,
};
use std::fs;
use tempfile::TempDir;

#[test]
fn empty_lock_dir_preempts_nothing() {
    // No lock files → nothing to preempt, and crucially no signals are sent.
    let tmp = TempDir::new().unwrap();
    let preempted = preempt_instances_in(tmp.path(), false);
    assert!(
        preempted.is_empty(),
        "an empty lock dir must preempt nothing, got: {preempted:?}"
    );
}

#[test]
fn dead_pid_lock_is_skipped() {
    // A stale lock owned by a dead PID must not be reported or signalled.
    let tmp = TempDir::new().unwrap();
    let profile = active_profile().unwrap_or("default");
    // PID 999999 is exceedingly unlikely to be alive.
    fs::write(
        tmp.path().join("telegram_deadbeef.lock"),
        format!("{profile}:999999"),
    )
    .unwrap();
    let preempted = preempt_instances_in(tmp.path(), false);
    assert!(
        preempted.is_empty(),
        "a dead-PID lock must be skipped, got: {preempted:?}"
    );
}

#[test]
fn other_profile_lock_is_not_touched() {
    // A live PID under a DIFFERENT profile must never be preempted — even our
    // own (alive) PID, as long as the profile name does not match.
    let tmp = TempDir::new().unwrap();
    let other = format!("not-{}", active_profile().unwrap_or("default"));
    fs::write(
        tmp.path().join("telegram_cafe.lock"),
        format!("{other}:{}", std::process::id()),
    )
    .unwrap();
    let preempted = preempt_instances_in(tmp.path(), false);
    assert!(
        preempted.is_empty(),
        "a different profile's lock must not be preempted, got: {preempted:?}"
    );
}

#[test]
#[ignore = "spawns and kills a child process; run with `cargo test -- --ignored`"]
fn preempts_and_kills_a_live_instance() {
    // Self-contained: write a same-profile lock in a TempDir pointing at a
    // throwaway child we spawned, then assert preemption stops exactly that
    // child. Touches no real lock files and signals no real instances.
    let tmp = TempDir::new().unwrap();
    let profile = active_profile().unwrap_or("default");

    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn sleep child");
    let child_pid = child.id();

    fs::write(
        tmp.path().join("_test_iso_telegram_abc123.lock"),
        format!("{profile}:{child_pid}"),
    )
    .unwrap();

    let preempted = preempt_instances_in(tmp.path(), false);

    let ours = preempted
        .iter()
        .find(|p| p.pid == child_pid)
        .expect("the live instance must be detected and preempted");
    assert!(
        ours.channels.iter().any(|c| c == "_test_iso_telegram"),
        "preempted instance must report the channel it held: {:?}",
        ours.channels
    );
    assert!(
        ours.stopped,
        "the instance must be stopped (SIGTERM/SIGKILL)"
    );

    let killed = !is_pid_alive(child_pid);
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        killed,
        "the background process must be gone after preemption"
    );
}

#[test]
fn parses_opencrabs_labels_from_launchctl_list() {
    // Realistic `launchctl list` shape: header line, foreign services,
    // our daemon + agent labels, `-` PID for loaded-but-not-running.
    let out = "PID\tStatus\tLabel\n\
               812\t0\tcom.apple.Finder\n\
               4043b\t0\tcom.opencrabs.daemon\n\
               -\t1\tcom.opencrabs.daemon.hermes\n\
               221\t0\tcom.opencrabs.agent\n\
               9\t0\torg.example.thing\n";
    let labels = parse_launchctl_labels(out);
    assert_eq!(
        labels,
        vec![
            "com.opencrabs.daemon".to_string(),
            "com.opencrabs.daemon.hermes".to_string(),
            "com.opencrabs.agent".to_string(),
        ],
        "exactly our labels, in output order, foreign services excluded"
    );
}

#[test]
fn launchctl_parse_dedupes_and_survives_empty_input() {
    let dupes = "PID\tStatus\tLabel\n\
                 1\t0\tcom.opencrabs.daemon\n\
                 2\t0\tcom.opencrabs.daemon\n";
    assert_eq!(
        parse_launchctl_labels(dupes),
        vec!["com.opencrabs.daemon".to_string()],
        "duplicate rows (two profiles' list snapshots) collapse to one bootout"
    );
    assert!(
        parse_launchctl_labels("").is_empty(),
        "empty launchctl output parses to nothing"
    );
    assert!(
        parse_launchctl_labels("PID\tStatus\tLabel\n").is_empty(),
        "bare header line parses to nothing"
    );
}

#[test]
fn launchctl_parse_rejects_lookalike_prefixes() {
    let sneaky = "PID\tStatus\tLabel\n\
                  1\t0\txcom.opencrabs.daemon\n\
                  2\t0\tcom.opencrabsx.evil\n\
                  3\t0\tcom.opencrabs.daemon\n";
    assert_eq!(
        parse_launchctl_labels(sneaky),
        vec!["com.opencrabs.daemon".to_string()],
        "prefix must anchor at label start; near-misses are not ours to bootout"
    );
}

/// A daemon with no enabled channels holds neither a token lock nor a service
/// unit: the instance stamp under `locks/instance/` is its only footprint, so
/// the token scan above sees nobody and the handover never completes (#2835).
#[test]
fn instance_stamp_is_a_preemption_target_on_its_own() {
    let tmp = TempDir::new().unwrap();
    let profile = active_profile().unwrap_or("default");
    // Spawned from a shell that exits at once, so the stand-in is reparented to
    // init instead of staying this test's child. A direct child would sit as a
    // ZOMBIE after SIGTERM until this test waited on it, and `kill(pid, 0)`
    // reports a zombie as alive, so the handover would look like it failed to
    // stop a process that was already dead. A real daemon is reaped by launchd
    // or systemd immediately; this gives the stand-in the same parent.
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 60 >/dev/null 2>&1 & echo $!")
        .output()
        .expect("spawn a stand-in background instance");
    let pid: u32 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("the shell prints the stand-in's pid");

    let instance_dir = tmp.path().join("instance");
    fs::create_dir_all(&instance_dir).unwrap();
    fs::write(
        instance_dir.join(format!("{profile}.lock")),
        format!("{pid}:0"),
    )
    .unwrap();

    let preempted = preempt_instances_in(tmp.path(), false);
    assert_eq!(
        preempted.len(),
        1,
        "the instance stamp must be found on its own, got {preempted:?}"
    );
    assert_eq!(preempted[0].pid, pid);
    assert_eq!(
        preempted[0].channels,
        vec!["instance".to_string()],
        "the reported channel names the lock it actually held"
    );
    assert!(
        preempted[0].stopped,
        "the stamped instance must be gone by the time preemption returns"
    );
}

/// Fail-closed: a stamp whose PID is already gone is refused. Nothing is
/// signalled and the caller is told nobody was preempted, so a recycled PID
/// can never be hit on the strength of a leftover file.
#[test]
fn stale_instance_stamp_is_refused() {
    let tmp = TempDir::new().unwrap();
    let profile = active_profile().unwrap_or("default");
    let dead = {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn a short-lived process");
        let pid = child.id();
        child.wait().expect("reap the short-lived process");
        pid
    };
    assert!(
        !is_pid_alive(dead),
        "the stand-in pid must be gone, otherwise this test proves nothing"
    );

    let instance_dir = tmp.path().join("instance");
    fs::create_dir_all(&instance_dir).unwrap();
    fs::write(
        instance_dir.join(format!("{profile}.lock")),
        format!("{dead}:0"),
    )
    .unwrap();

    let preempted = preempt_instances_in(tmp.path(), false);
    assert!(
        preempted.is_empty(),
        "a dead-PID instance stamp must be refused, got {preempted:?}"
    );
}
