//! Daemon profile scoping and adoption guard ordering (#1723).
//!
//! Two contracts:
//! 1. A `-p <name>` daemon is scoped to THAT profile: it adopts nothing.
//!    Cross-profile adoption is a default-launch behavior. The scoping
//!    decision and the candidate filter are pinned by pure unit tests
//!    against a TempDir instance-lock dir.
//! 2. A losing scheduler adopter (profile owned by another process) must
//!    not write the foreign profile's home. The brain seed happens AFTER
//!    the lock is won, so a denied adopter leaves the foreign home
//!    untouched. Verified against a probe-named profile: on correct code
//!    the test writes NOTHING anywhere; the probe profile dir under the
//!    real base stays absent (or, if a previous mutant created it, stays
//!    template-free is NOT asserted; what is asserted is absence of the seed marker).

use crate::cli::ui::profiles_to_adopt;
use crate::config::profile::ProfileEntry;
// Used by the seeding test, which holds a unix flock.
#[cfg(unix)]
use std::path::PathBuf;
use tempfile::TempDir;

fn entry(name: &str) -> ProfileEntry {
    ProfileEntry {
        name: name.to_string(),
        description: None,
        created_at: String::new(),
        last_used: None,
    }
}

#[test]
fn scoped_launch_adopts_nothing() {
    let dir = TempDir::new().unwrap();
    let entries = vec![entry("default"), entry("ops"), entry("family")];
    let adopted = profiles_to_adopt(entries, "ops", dir.path(), true);
    assert!(
        adopted.is_empty(),
        "a -p scoped daemon must adopt nothing, got {adopted:?}"
    );
}

#[test]
fn unscoped_skips_active_and_adopts_the_rest() {
    let dir = TempDir::new().unwrap();
    let entries = vec![entry("default"), entry("ops"), entry("family")];
    let adopted = profiles_to_adopt(entries, "default", dir.path(), false);
    assert_eq!(adopted.len(), 2, "both non-active profiles adoptable");
    assert!(adopted.contains(&"ops".to_string()));
    assert!(adopted.contains(&"family".to_string()));
    assert!(
        !adopted.contains(&"default".to_string()),
        "the active profile is run by cmd_chat_inner, never adopted"
    );
}

#[test]
fn unscoped_skips_profiles_with_a_live_instance() {
    let dir = TempDir::new().unwrap();
    // ops has a live instance: a lock file holding THIS test process's PID.
    std::fs::write(dir.path().join("ops.lock"), std::process::id().to_string()).unwrap();
    let entries = vec![entry("default"), entry("ops"), entry("family")];
    let adopted = profiles_to_adopt(entries, "default", dir.path(), false);
    assert_eq!(
        adopted,
        vec!["family".to_string()],
        "live ops must be skipped (#194)"
    );
}

#[test]
fn dead_pid_lockfile_is_adoptable() {
    let dir = TempDir::new().unwrap();
    // A stale lock from a crashed instance (pid far beyond pid_max) must
    // not wedge adoption forever.
    std::fs::write(dir.path().join("ops.lock"), "999999999").unwrap();
    let entries = vec![entry("default"), entry("ops")];
    let adopted = profiles_to_adopt(entries, "default", dir.path(), false);
    assert_eq!(
        adopted,
        vec!["ops".to_string()],
        "stale lock must not block adoption"
    );
}

/// A losing adopter (profile's scheduler held elsewhere) must not seed the
/// foreign profile's brain (#1723: seed happens after the lock is won).
///
/// Mechanics: this test holds the scheduler flock for a probe-named profile
/// in the REAL global locks dir (the same dir `acquire_scheduler_lock`
/// uses; it is deliberately not home-override-able), then drives
/// `spawn_cron_scheduler_for_profile` for that profile and asserts the
/// probe profile's home under the real base was never created/seeded.
/// On green code this test performs zero writes anywhere; the flock is
/// released on drop. The probe name is unique so no live daemon ever
/// holds it and no real profile ever uses it.
#[cfg(unix)]
#[tokio::test]
async fn losing_adopter_leaves_foreign_profile_home_untouched() {
    let probe = "1723-seed-order-probe";
    // Setup hygiene: a prior failed/mutant run may have left the probe dir
    // behind. Nuke it BEFORE anything runs so the absence assert below is
    // meaningful (remove_dir_all on a nonexistent dir is a no-op, so green
    // code still performs zero writes).
    let _ = std::fs::remove_dir_all(
        crate::config::profile::base_opencrabs_dir()
            .join("profiles")
            .join(probe),
    );
    // Hold the scheduler lock exactly as another daemon would.
    let guard = crate::config::profile::acquire_scheduler_lock(probe);
    assert!(
        guard.is_some(),
        "probe lock must be acquirable in a clean run"
    );

    let inner = crate::cli::ui::spawn_cron_scheduler_for_profile(probe.to_string());
    // The spawn function re-scopes the profile home itself; nothing it does
    // on the DENY path may touch the real base.
    inner.await;

    let probe_dir = crate::config::profile::base_opencrabs_dir()
        .join("profiles")
        .join(probe);
    let seeded_marker = probe_dir.join("SOUL.md");
    assert!(
        !seeded_marker.exists(),
        "a losing adopter must not seed the foreign profile's brain (found {})",
        seeded_marker.display()
    );
    // The test must be self-contained even if a prior mutant created the dir.
    let _ = std::fs::remove_dir_all(&probe_dir);
    let _: PathBuf = probe_dir;
}
