//! Cross-platform exclusive lock for the remote state files.
//!
//! `pairing.json`, `routes.json` and `tunnel.json` are read-modify-write
//! shared between the daemon and the `opencrabs connect` CLI — two
//! processes, one file each, arbitrated by a `.lock` sibling.
//!
//! Unix holds an `flock(2)` on the sibling via [`crate::config::flock`]
//! (with its EINTR retry): locks die with the process, so a crashed
//! writer never wedges the store. Windows holds the `LockFileEx` range
//! lock that [`crate::config::winlock`] provides — the direct analogue,
//! released when the handle closes. Same contract on both.
//!
//! Contention is retried for a bounded budget, then the caller FAILS.
//! This is deliberately unlike the brain path-lock, which may proceed
//! overlapped and say so: the pairing store's lock guards single-use
//! code consumption and the tunnel's guards check-then-spawn — writing
//! without the lock is not a degradation those callers can accept.

use std::fs::OpenOptions;
use std::path::Path;
use std::time::Duration;
#[cfg(any(unix, windows))]
use std::time::Instant;

/// How long a contended lock waits before failing. Long enough to
/// serialise a daemon write against a CLI write (the hold itself is
/// milliseconds), short enough that a wedged holder surfaces as an error
/// rather than a hang.
pub(crate) const WAIT: Duration = Duration::from_millis(300);

/// Gap between attempts while waiting.
const RETRY_EVERY: Duration = Duration::from_millis(25);

/// Holds the lock; drops release it (fd/handle close does it on both
/// platforms, so a panicking or killed writer cannot leave it taken).
#[derive(Debug)]
pub(crate) struct StateLock {
    /// The lock file. Kept alive because the kernel releases the lock when
    /// its descriptor closes — nothing reads it.
    #[allow(dead_code)]
    file: std::fs::File,
}

/// Take the exclusive lock on `lock_path`, retrying contention for `wait`.
/// Fails on timeout or lock error — callers never proceed unlocked.
pub(crate) fn exclusive_wait(lock_path: &Path, wait: Duration) -> anyhow::Result<StateLock> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .map_err(anyhow::Error::new)
        .map_err(|e| e.context(format!("open {}", lock_path.display())))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();
        let deadline = Instant::now() + wait;
        loop {
            match crate::config::flock::exclusive(fd, true) {
                crate::config::flock::FlockOutcome::Acquired => {
                    return Ok(StateLock { file });
                }
                crate::config::flock::FlockOutcome::Held => {
                    if Instant::now() >= deadline {
                        anyhow::bail!(
                            "lock {} is held by another writer after {wait:?} — \
                             another opencrabs process is mid-write",
                            lock_path.display()
                        );
                    }
                    std::thread::sleep(RETRY_EVERY);
                }
                crate::config::flock::FlockOutcome::Failed(e) => {
                    return Err(
                        anyhow::Error::new(e).context(format!("lock {}", lock_path.display()))
                    );
                }
            }
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let handle = file.as_raw_handle();
        let deadline = Instant::now() + wait;
        loop {
            match crate::config::winlock::exclusive(handle, true) {
                crate::config::winlock::LockOutcome::Acquired => {
                    return Ok(StateLock { file });
                }
                crate::config::winlock::LockOutcome::Held => {
                    if Instant::now() >= deadline {
                        anyhow::bail!(
                            "lock {} is held by another writer after {wait:?} — \
                             another opencrabs process is mid-write",
                            lock_path.display()
                        );
                    }
                    std::thread::sleep(RETRY_EVERY);
                }
                crate::config::winlock::LockOutcome::Failed(e) => {
                    return Err(
                        anyhow::Error::new(e).context(format!("lock {}", lock_path.display()))
                    );
                }
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = &file;
        anyhow::bail!("remote state locking is unsupported on this platform");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let err =
            exclusive_wait(&path, Duration::from_millis(50)).expect_err("must give up cleanly");
        assert!(
            err.to_string().contains("held by another writer"),
            "error should name contention, got: {err}"
        );
    }
}
