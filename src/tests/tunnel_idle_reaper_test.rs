//! The idle tunnel reaper (#2537).
//!
//! The name trap these tests guard against: `tunnel::reap` is a zombie
//! `waitpid` for a child we killed. It has no clock and no traffic notion, so
//! it cannot stop a forgotten `connect`. The idle reaper is a different thing
//! and its decision is what is pinned here.

use crate::remote::tunnel::{
    IDLE_TIMEOUT_DEFAULT, TunnelState, idle_expired, last_activity_secs, reap_idle, stamp_due,
    traffic_stamp_path, write_traffic_stamp,
};
use std::time::Duration;

fn state(started_at: u64) -> TunnelState {
    TunnelState {
        pid: 4242,
        url: "https://example.trycloudflare.com".to_string(),
        port: 8787,
        binary: "/tmp/cloudflared".to_string(),
        started_at,
    }
}

#[test]
fn idle_window_boundary_is_inclusive_and_one_second_short_is_not_idle() {
    let now = 1_000_000u64;
    assert!(idle_expired(now - 43_200, now, IDLE_TIMEOUT_DEFAULT));
    assert!(!idle_expired(now - 43_199, now, IDLE_TIMEOUT_DEFAULT));
    // A clock that moved backwards must read as idle, never as a panic.
    assert!(idle_expired(now, now - 5, Duration::from_secs(1)));
}

#[test]
fn a_tunnel_never_used_is_measured_from_its_own_start() {
    let dir = tempfile::tempdir().unwrap();
    let started = 1_700_000_000u64;
    let s = state(started);
    assert_eq!(
        last_activity_secs(dir.path(), &s),
        started,
        "no stamp means no traffic yet, not idle since the epoch"
    );
}

#[test]
fn a_stamp_older_than_the_tunnel_still_measures_from_the_tunnel() {
    // The leftover-stamp case: a previous tunnel's stamp must not date this
    // one to last week and reap it on the first tick.
    let dir = tempfile::tempdir().unwrap();
    write_traffic_stamp(dir.path());
    assert!(traffic_stamp_path(dir.path()).is_file());
    let started = crate::remote::now_secs();
    assert_eq!(last_activity_secs(dir.path(), &state(started)), started);
}

#[test]
fn a_stamp_newer_than_the_tunnel_wins() {
    let dir = tempfile::tempdir().unwrap();
    write_traffic_stamp(dir.path());
    // A tunnel record that predates the stamp (the daemon wrote the stamp, the
    // record survived a restart) must read as active, not idle.
    let s = state(1);
    assert!(last_activity_secs(dir.path(), &s) > 1);
}

#[test]
fn no_tunnel_means_nothing_to_reap() {
    let dir = tempfile::tempdir().unwrap();
    let reaped = reap_idle(dir.path(), Duration::from_secs(1)).unwrap();
    assert!(reaped.is_none());
}

#[test]
fn stamp_throttle_allows_one_write_a_minute() {
    assert!(stamp_due(1_000, 0));
    assert!(!stamp_due(1_059, 1_000));
    assert!(stamp_due(1_060, 1_000));
}