//! Pin the `daily_backup` same-day rotation contract.
//!
//! Regression: 2026-09-07 (#1459). `daily_backup` skipped whenever today's
//! dated backup existed, so every write after the day's first snapshot had NO
//! pre-write copy. When the keys-wipe bug (#1458) fired at 23:52, the safety
//! net was a single morning-aged snapshot — and on any day where an earlier
//! benign write had consumed nothing, a destructive write could still leave
//! every post-snapshot state unrecoverable. (In the actual incident the skip
//! ironically SAVED the full keys: the day's only snapshot predated the wipe.)
//!
//! New contract: today's backup exists + identical content → skip (no spam).
//! Today's backup exists + content differs → rotate the current file to
//! `stem.YYYY-MM-DDTHH-MM-SS.bak` (the day's first snapshot stays intact AND
//! every later pre-write state gets its own copy), then prune under the same
//! `max_days` policy. On read error, do nothing — never destroy on
//! uncertainty. These tests run in a `tempfile` tempdir; `daily_backup` is
//! pure path-in (`path.parent()`), so the live home is never touched.

use crate::config::types::io::daily_backup;
use std::fs;

fn bak_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".bak"))
        .collect();
    names.sort();
    names
}

fn read(dir: &std::path::Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).expect("read backup")
}

/// The save-burst window (#2265) counts a snapshot younger than five seconds
/// as already covering the current burst, so a rotation test has to age the
/// snapshot it wants rotated past first.
fn age(path: &std::path::Path, secs: u64) {
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open backup")
        .set_modified(when)
        .expect("set mtime");
}

#[test]
fn same_day_changed_content_rotates_to_timestamped_sibling() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("f.toml");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today_bak = format!("f.toml.{today}.bak");

    // First snapshot of the day.
    fs::write(&file, "v1").expect("write v1");
    daily_backup(&file, 7);
    assert!(
        tmp.path().join(&today_bak).exists(),
        "today's snapshot missing"
    );
    assert_eq!(read(tmp.path(), &today_bak), "v1", "first snapshot wrong");

    // Caller writes v2, then calls daily_backup again pre-next-write:
    // content differs from today's snapshot → must rotate, not skip. The
    // snapshot is aged past the burst window so this is a second save, not a
    // key later in the same one.
    age(&tmp.path().join(&today_bak), 60);
    fs::write(&file, "v2").expect("write v2");
    daily_backup(&file, 7);

    let names = bak_names(tmp.path());
    assert_eq!(names.len(), 2, "expected exactly 2 backups, got {names:?}");
    assert!(
        names.contains(&today_bak),
        "day's first snapshot must survive"
    );
    let rotated = names
        .iter()
        .find(|n| **n != today_bak)
        .expect("rotated sibling");
    assert!(
        rotated.starts_with(&format!("f.toml.{today}T")),
        "rotated name must be timestamped, got {rotated}"
    );
    assert_eq!(
        read(tmp.path(), rotated),
        "v2",
        "rotated copy must hold the newer pre-write state"
    );

    // Both same-day states are recoverable — the #1459 gap is closed.
    assert_eq!(
        read(tmp.path(), &today_bak),
        "v1",
        "first snapshot was clobbered"
    );
}

#[test]
fn same_day_identical_content_is_no_op() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("f.toml");

    fs::write(&file, "v1").expect("write");
    daily_backup(&file, 7);
    daily_backup(&file, 7);
    daily_backup(&file, 7);

    let names = bak_names(tmp.path());
    assert_eq!(
        names.len(),
        1,
        "identical re-runs must not spawn backups: {names:?}"
    );
    assert_eq!(fs::read_to_string(&file).expect("read file"), "v1");
}

#[test]
fn prune_counts_rotated_siblings_under_max_days() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("f.toml");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today_bak = format!("f.toml.{today}.bak");

    // Seed an older history and a stale first snapshot (all with the real
    // stem: prune's prefix filter must see them as the same file's history).
    for (day, age_days) in [("2026-09-04", 9u64), ("2026-09-03", 10u64)] {
        let path = tmp.path().join(format!("f.toml.{day}.bak"));
        fs::write(&path, "old").expect("seed");
        age(&path, age_days * 86_400);
    }
    let today_path = tmp.path().join(&today_bak);
    fs::write(&today_path, "v0").expect("seed today");
    // Older than the save-burst window, so this write reaches the rotation.
    age(&today_path, 60);
    fs::write(&file, "v1").expect("write v1");

    // Differs from today's snapshot → rotation path, then shared prune.
    daily_backup(&file, 3);

    let names = bak_names(tmp.path());
    assert_eq!(
        names.len(),
        3,
        "max_days=3 must keep exactly 3 files after rotation, got {names:?}"
    );
    // The rotated sibling must rank NEWER than its day's plain snapshot
    // ('T' > '.' lexicographically) and the oldest file must be pruned.
    assert!(
        names.contains(&today_bak)
            && names
                .iter()
                .any(|n| n.starts_with(&format!("f.toml.{today}T"))),
        "today's plain + rotated snapshots must both survive: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("2026-09-03")),
        "oldest backup must be pruned, got {names:?}"
    );
}
