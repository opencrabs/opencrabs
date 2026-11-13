//! Backup retention regressions (#2265).
//!
//! Two defects lived in `daily_backup`:
//! 1. it only compared against the day's FIRST snapshot, so a burst of saves
//!    wrote one rotation per key even when nothing had changed since the last
//!    one;
//! 2. it pruned by file NAME, and `config.toml.bak` sorts after every
//!    `config.toml.YYYY-MM-DD.bak`. The reverse walk treated the undated file
//!    as the newest, kept it forever, and deleted the current day's snapshot
//!    first. That is how a 7-slot budget dropped today's backup while keeping
//!    34 KB of unrelated state.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::config::types::daily_backup;

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write fixture");
}

fn set_age_days(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_secs(days * 24 * 60 * 60);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open fixture");
    file.set_modified(when).expect("set mtime");
}

#[test]
fn undated_backup_is_pruned_before_dated_snapshots() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    write(&config, "current = true\n");

    // The undated file is the OLDEST by mtime but the NEWEST by name.
    let undated = dir.path().join("config.toml.bak");
    write(&undated, "stale = true\n");
    set_age_days(&undated, 10);

    let older = dir.path().join("config.toml.2026-11-01.bak");
    write(&older, "older = true\n");
    set_age_days(&older, 5);

    let newest = dir.path().join("config.toml.2026-11-12.bak");
    write(&newest, "newest = true\n");
    set_age_days(&newest, 1);

    daily_backup(&config, 2);

    assert!(
        !undated.exists(),
        "the undated .bak is the oldest file and must be pruned first, not kept forever"
    );
    assert!(
        !older.exists(),
        "only the two newest snapshots survive a 2-slot budget"
    );
    assert!(newest.exists(), "yesterday's snapshot must survive");

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert!(
        dir.path().join(format!("config.toml.{today}.bak")).exists(),
        "the pre-write snapshot of the current file is the newest and must survive"
    );
}

#[test]
fn identical_content_skips_a_backup_even_when_todays_snapshot_is_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    write(&config, "same = true\n");

    let old = dir.path().join("config.toml.2000-01-01.bak");
    write(&old, "same = true\n");
    set_age_days(&old, 3);

    daily_backup(&config, 7);

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert!(
        !dir.path().join(format!("config.toml.{today}.bak")).exists(),
        "nothing changed since the newest backup, so there is nothing to protect"
    );
}

#[test]
fn changed_content_still_takes_a_snapshot() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    write(&config, "now = 2\n");

    let old = dir.path().join("config.toml.2000-01-01.bak");
    write(&old, "now = 1\n");
    set_age_days(&old, 1);

    daily_backup(&config, 7);

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let snapshot = dir.path().join(format!("config.toml.{today}.bak"));
    assert!(snapshot.exists(), "a real change must be snapshotted");
    assert_eq!(
        std::fs::read_to_string(&snapshot).unwrap(),
        "now = 2\n",
        "the snapshot is the pre-write state of the file"
    );
}
#[test]
fn one_fresh_backup_covers_the_whole_save_burst() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");

    // A save's first write already took the pre-save snapshot a moment ago.
    let burst = dir.path().join("config.toml.2026-11-13T20-08-31.bak");
    write(&burst, "key_one = 1\n");
    write(&config, "key_one = 1\nkey_two = 2\n");

    daily_backup(&config, 7);

    let count = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".bak"))
        .count();
    assert_eq!(
        count, 1,
        "the whole burst shares one pre-save snapshot; per-key copies starve \
         the retention budget"
    );
}
