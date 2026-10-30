//! #2084: every lock stamp carries the writer's creation time, and every
//! reader checks it before calling the stamp a live owner.
//!
//! The bug: `pid:ticks` was written on Windows only. On Linux and macOS a lock
//! carried a bare PID, so a stale lock file whose PID had been recycled by an
//! unrelated process (the reported case: a `redis-server` sitting on the PID of
//! a dead instance) read as live forever and every channel refused to start.
//! The fix stamps Linux (`/proc/<pid>/stat` field 22) and macOS
//! (`proc_pidinfo` + `kern.boottime`) too, and `stamp_owner_is_live` now guards
//! all three call sites: token lock, scheduler lock, and the preempt scan.
//!
//! These tests pin the stamp shape, the exact-match rejection, and the
//! boot-epoch fallback that covers stamps written by older builds.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::{Duration, UNIX_EPOCH};

// Each platform-gated import is listed under the same cfg as the tests that
// use it, so a Windows or unsupported-platform build does not warn about an
// import only its own build cannot reach.
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::config::profile::boot_epoch;
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
use crate::config::profile::{active_profile, foreign_lock_owners, process_creation_stamp};
use crate::config::profile::{owner_stamp, split_pid_ticks, stamp_owner_is_live};

/// Serialize against every other test that reads or writes under the
/// $HOME-resolved `~/.opencrabs`, the same lock `profile_test` takes.
fn fs_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::tests::HOME_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write `contents` to a fresh lock file and force its mtime to `written`, so
/// the boot-epoch check is exercised on purpose instead of racing the clock.
fn stamped_lock(
    dir: &tempfile::TempDir,
    name: &str,
    contents: &str,
    written: SystemTime,
) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, contents).expect("write stamp");
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("open stamp")
        .set_modified(written)
        .expect("set mtime");
    path
}

/// The stamp this build writes must carry a real creation time wherever the
/// platform can be asked, which is the whole point of #2084. A bare PID here
/// means the reader is back to trusting a PID alone.
#[test]
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn owner_stamp_carries_a_creation_time_on_this_platform() {
    let (pid, ticks) = split_pid_ticks(&owner_stamp()).expect("own stamp must parse");
    assert_eq!(pid, std::process::id());
    let ticks = ticks.expect("this platform can read a creation time");
    assert_ne!(ticks, 0, "a zero creation time proves nothing");
    assert_eq!(process_creation_stamp(pid), Some(ticks));
}

/// A platform with no reading keeps the old shape rather than inventing one.
#[test]
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn owner_stamp_is_a_bare_pid_where_no_reading_exists() {
    let (pid, ticks) = split_pid_ticks(&owner_stamp()).expect("own stamp must parse");
    assert_eq!(pid, std::process::id());
    assert_eq!(ticks, None);
}

/// The exact-match rule: a stamp whose recorded creation time disagrees with
/// the live process names a process that died holding that PID. Reporting it as
/// live is what hands an unrelated process to the preempt path's SIGTERM.
#[test]
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn stamp_owner_is_live_rejects_a_recycled_creation_time() {
    let _guard = fs_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pid = std::process::id();
    let ticks = process_creation_stamp(pid).expect("this platform reads a creation time");

    let live = stamped_lock(
        &dir,
        "a.lock",
        &format!("p:{pid}:{ticks}"),
        SystemTime::now(),
    );
    assert!(
        stamp_owner_is_live(&format!("{pid}:{ticks}"), &live),
        "the live process's own reading must be accepted"
    );
    let recycled = stamped_lock(
        &dir,
        "b.lock",
        &format!("p:{pid}:{}", ticks.saturating_add(1)),
        SystemTime::now(),
    );
    assert!(
        !stamp_owner_is_live(&format!("{pid}:{}", ticks.saturating_add(1)), &recycled),
        "a recorded creation time that disagrees with the live process is a recycled PID"
    );
}

/// A lock file written before this machine booted cannot name a process that is
/// alive now, whatever its PID resolves to. This is the check that still covers
/// bare-PID stamps written by builds that predate the creation-time field.
#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stamp_owner_is_live_rejects_a_lock_written_before_the_boot() {
    let _guard = fs_lock();
    let boot = boot_epoch().expect("this platform reports a boot time");
    let dir = tempfile::tempdir().expect("tempdir");
    let pid = std::process::id();
    let before = UNIX_EPOCH + Duration::from_secs(boot.saturating_sub(1) as u64);

    for (name, field) in [("a.lock", owner_stamp()), ("b.lock", pid.to_string())] {
        let path = stamped_lock(&dir, name, &format!("p:{field}"), before);
        assert!(
            !stamp_owner_is_live(&field, &path),
            "{field} written before the boot must not read as live"
        );
    }
}
/// The residual, stated rather than hidden: a stamp written by a build that
/// predates the creation-time field, after the current boot, whose PID was
/// recycled within that same boot, still reads as live. Nothing left can
/// disprove it, so the check stays fail-open instead of refusing to start.
#[test]
fn stamp_owner_is_live_accepts_a_legacy_stamp_written_after_the_boot() {
    let _guard = fs_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let pid = std::process::id();
    let path = stamped_lock(&dir, "a.lock", &format!("p:{pid}"), SystemTime::now());
    assert!(stamp_owner_is_live(&pid.to_string(), &path));
}

/// The preempt scan SIGTERMs whatever it returns, so it has to apply the same
/// stamp discipline: a file naming a recycled PID is dropped instead of being
/// handed to `kill`, while the live owner's own stamp still comes through.
#[test]
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
fn foreign_lock_owners_drops_a_recycled_stamp() {
    use std::process::{Command, Stdio};
    let _guard = fs_lock();
    let profile = active_profile().unwrap_or("default");
    let mut cmd = if cfg!(windows) {
        Command::new("ping")
    } else {
        Command::new("/bin/sleep")
    };
    if cfg!(windows) {
        cmd.args(["-n", "60", "127.0.0.1"]);
    } else {
        cmd.args(["60"]);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a helper process to own a live pid");
    let live = child.id();
    let ticks = process_creation_stamp(live).expect("this platform reads a creation time");
    let dir = tempfile::tempdir().expect("tempdir");

    let recycled = stamped_lock(
        &dir,
        "telegram_aaa.lock",
        &format!("{profile}:{live}:{}", ticks.saturating_add(1)),
        SystemTime::now(),
    );
    assert!(
        !foreign_lock_owners(dir.path()).contains_key(&live),
        "a recycled stamp must not be offered to the preempt path"
    );

    fs::write(&recycled, format!("{profile}:{live}:{ticks}")).expect("rewrite stamp");
    let owners = foreign_lock_owners(dir.path());
    let owner = owners
        .get(&live)
        .expect("the live owner must still be reported");
    assert_eq!(owner.creation_ticks, Some(ticks));
    assert_eq!(owner.channels, vec!["telegram".to_string()]);
    let _ = child.kill();
    let _ = child.wait();
}

/// A PID that names no process at all is still the first check: no reading is
/// taken, and a stamp whose PID has been freed is never an owner.
#[test]
fn stamp_owner_is_live_rejects_a_dead_pid() {
    let _guard = fs_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = stamped_lock(&dir, "a.lock", "p:4294967295:9000", SystemTime::now());
    assert!(!stamp_owner_is_live("4294967295", &path));
    assert!(!stamp_owner_is_live("4294967295:9000", &path));
    // PID 0 is never a real owner; on Unix `kill(0, 0)` probes the CALLER's
    // process group, so trusting it would read as alive.
    assert!(!stamp_owner_is_live("0", &path));
}

/// A stamp that does not start with a usable PID names nobody. The token lock
/// reads this as corruption and takes the lock over (issue #192), so the reader
/// must never call it live.
#[test]
fn stamp_owner_is_live_rejects_a_garbage_pid_field() {
    let _guard = fs_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = stamped_lock(&dir, "a.lock", "junk", SystemTime::now());
    for field in ["", "not-a-pid", "101528ops:103104family:101507", "1.5"] {
        assert!(
            !stamp_owner_is_live(field, &path),
            "{field:?} names no owner and must not read as live"
        );
    }
}

/// Fail-open, one leg of it: with the lock file unreadable there is no mtime to
/// compare against the boot epoch, so the boot check cannot be taken and the
/// answer stays `true`. Refusing to start is worse than a duplicate start, and
/// the alternative here is a stamp that can never be read as anything but held.
#[test]
fn stamp_owner_is_live_fails_open_when_the_boot_check_cannot_be_taken() {
    let _guard = fs_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("gone.lock");
    assert!(
        stamp_owner_is_live(&std::process::id().to_string(), &missing),
        "an unreadable lock file must not read as a stale owner"
    );
}
