//! Cross-process serialization for [`Database::run_migrations`] (#2297).
//!
//! Two OpenCrabs processes (the TUI or daemon and a gui window, or two gui
//! windows) opening the same ledger at the same moment both call
//! `to_latest` on the same file. `rusqlite_migration` reads `user_version`
//! *before* it opens its transaction (`goto`, lib.rs:714) and then opens a
//! DEFERRED one (`goto_up`, lib.rs:629), so both processes read version 0:
//! the winner commits every migration, and the loser replays the same list
//! from its stale version against a schema that is already advanced. The
//! schema-modernisation migration then copies `is_archived` out of a table
//! the winner already replaced, and the second process dies at startup with
//! "db migrations: Failed to run database migrations".
//!
//! A schema check cannot fix this, because the loser's read happens before
//! any lock exists. So the loser has to wait before it reads anything: an
//! exclusive advisory lock on `<db>.migrate.lock`, held for the whole
//! migration block (snapshot decision, preflight, migrations, heals).
//!
//! Advisory on purpose: SQLite's own fcntl locks answer "is this page being
//! written", never "who is allowed to migrate", and a separate lock file
//! keeps that question away from the image itself.
//!
//! [`Database::run_migrations`]: crate::db::Database::run_migrations

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a waiting process tolerates another migrator before it gives up.
///
/// A healthy migration on a large ledger takes seconds (the O(size) integrity
/// scan is gone, see `run_migrations`), so minutes mean the other process is
/// wedged. Failing with the lock path beats hanging forever with no message.
const WAIT_LIMIT: Duration = Duration::from_secs(300);

/// Poll interval while another process holds the lock.
const POLL: Duration = Duration::from_millis(100);

/// The held lock. Dropping it releases the advisory lock and closes the file.
pub struct MigrationLock {
    file: std::fs::File,
}

impl MigrationLock {
    /// Take the exclusive lock for `db_path`, waiting for any other migrator.
    ///
    /// Blocking on purpose: the caller is a startup path whose only correct
    /// alternative to waiting is failing, and waiting is what makes the
    /// second process see the finished schema instead of replaying into it.
    pub fn acquire(db_path: &Path) -> std::io::Result<Self> {
        let path = lock_path(db_path);
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)?;

        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                // std reports contention as its own type since the file-lock
                // API went stable; the OS error is the second arm.
                Err(std::fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= WAIT_LIMIT {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!(
                                "another OpenCrabs process has held the migration lock {} \
                                 for {}s without finishing",
                                path.display(),
                                started.elapsed().as_secs()
                            ),
                        ));
                    }
                    std::thread::sleep(POLL);
                }
                // Any other error (unsupported filesystem, no permission) is
                // a real problem the operator has to see.
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
    }
}

impl Drop for MigrationLock {
    fn drop(&mut self) {
        // Best effort: the OS releases the lock when the handle closes
        // anyway, and a failing unlock must not panic a startup path.
        let _ = self.file.unlock();
    }
}

/// `<db>.migrate.lock`, next to the image so every process that opens the
/// same database computes the same path.
pub fn lock_path(db_path: &Path) -> PathBuf {
    let mut os = db_path.as_os_str().to_os_string();
    os.push(".migrate.lock");
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::{MigrationLock, lock_path};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn lock_path_sits_next_to_the_database() {
        let db = std::env::temp_dir().join("profile").join("opencrabs.db");
        let path = lock_path(&db);
        assert_eq!(path.parent(), db.parent());
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("opencrabs.db.migrate.lock")
        );
    }

    /// The regression seat for #2297: a second migrator must not read
    /// `user_version` while the first is still migrating. This pins the
    /// *ordering* (second acquire happens after the first release), which is
    /// exactly what `run_migrations` now depends on.
    #[test]
    fn second_lock_waits_for_the_first_to_release() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("ledger.db");
        let released = Arc::new(AtomicBool::new(false));
        let flag = released.clone();
        let held_for = Duration::from_millis(250);

        let holder = std::thread::spawn(move || {
            let _lock = MigrationLock::acquire(&db).expect("first acquire");
            std::thread::sleep(held_for);
            flag.store(true, Ordering::SeqCst);
        });

        // Give the holder a moment to take the lock before racing it.
        std::thread::sleep(Duration::from_millis(50));

        let started = Instant::now();
        let _second =
            MigrationLock::acquire(&dir.path().join("ledger.db")).expect("second acquire");
        let waited = started.elapsed();

        assert!(
            released.load(Ordering::SeqCst),
            "the second migrator acquired the lock while the first still held it"
        );
        assert!(
            waited >= Duration::from_millis(100),
            "the second migrator did not wait (waited {waited:?})"
        );

        holder.join().expect("holder thread");
    }

    /// Dropping the guard must release the lock, or every later startup on
    /// the same profile would hit the wait limit.
    #[test]
    fn dropping_the_guard_releases_the_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("ledger.db");
        {
            let _lock = MigrationLock::acquire(&db).expect("first acquire");
        }
        let started = Instant::now();
        let _again = MigrationLock::acquire(&db).expect("re-acquire after drop");
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "the lock was still held after the guard dropped"
        );
    }
}
