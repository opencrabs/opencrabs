//! #2297: two agents starting at once over one database.
//!
//! The GUI reported `agent offline: db migrations: Failed to run database
//! migrations` while the TUI was open and healthy, with the cause already
//! discarded by `{e}` before it reached the screen (#2299 fixes that trail).
//! These tests reproduce the SHAPE: two independent agents, each on its own
//! thread with its own current-thread runtime and its own pool, exactly the
//! way `src/gui/agent_bridge.rs::spawn_bootstrap` and the TUI start, opening
//! and migrating the same file.
//!
//! What each test pins:
//! - both sides report success, never `Failed to run database migrations`
//! - the file ends at the current `user_version`, so a lost race cannot leave
//!   a half-migrated database behind
//!
//! The home override matters: `run_migrations` resolves the snapshot directory
//! for a pending migration, and without the override these tests would write
//! snapshots into the live profile home.

use crate::config::profile::with_home_override_async;
use crate::db::Database;
use crate::db::database::MIGRATION_SQL;
use std::path::{Path, PathBuf};

/// One agent's whole database start: connect, then migrate. Same two calls the
/// GUI bridge and the daemon make, nothing else, so a failure here is the
/// failure the user saw.
async fn start_agent(db_path: PathBuf, home: PathBuf) -> Result<(), String> {
    with_home_override_async(home, async move {
        let db = Database::connect(&db_path)
            .await
            .map_err(|e| format!("db connect: {e:#}"))?;
        db.run_migrations()
            .await
            .map_err(|e| format!("db migrations: {e:#}"))?;
        Ok(())
    })
    .await
}

/// The agents run on their own threads with their own runtimes on purpose: a
/// shared runtime would serialise the blocking `interact` work and hide the
/// contention this test exists to find.
fn start_agents_at_once(db_path: &Path, home: &Path, agents: usize) -> Vec<Result<(), String>> {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(agents));
    let mut handles = Vec::with_capacity(agents);
    for i in 0..agents {
        let db_path = db_path.to_path_buf();
        let home = home.join(format!("agent-{i}"));
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(
            std::thread::Builder::new()
                .stack_size(4 * 1024 * 1024)
                .name(format!("agent-{i}"))
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("current-thread runtime");
                    // Every agent reaches the database at the same moment.
                    barrier.wait();
                    runtime.block_on(start_agent(db_path, home))
                })
                .expect("spawn agent thread"),
        );
    }
    handles
        .into_iter()
        .map(|h| h.join().expect("agent thread must not panic"))
        .collect()
}

fn user_version(path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(path).expect("open migrated database");
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("read user_version")
}

/// A path whose database does not exist yet, so the race happens while all the
/// migrations still have to run.
fn fresh_db(dir: &Path) -> PathBuf {
    dir.join("opencrabs.db")
}

#[test]
fn migrations_concurrent_two_agents_both_succeed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fresh_db(dir.path());
    let home = dir.path().join("home");

    let results = start_agents_at_once(&db_path, &home, 2);
    for (i, result) in results.iter().enumerate() {
        assert!(
            result.is_ok(),
            "agent {i} failed to start against the same database: {result:?}"
        );
    }
    assert_eq!(
        user_version(&db_path),
        MIGRATION_SQL.len() as i64,
        "the database must end at the current migration count"
    );
}

/// The incident shape: the database is already current, and a second agent
/// joins it. No migration has to run, so nothing may fail either.
#[test]
fn migrations_concurrent_second_agent_on_current_db_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fresh_db(dir.path());
    let home = dir.path().join("home");

    let first = start_agents_at_once(&db_path, &home, 1);
    assert!(first[0].is_ok(), "first agent: {:?}", first[0]);

    let others = start_agents_at_once(&db_path, &home, 3);
    for (i, result) in others.iter().enumerate() {
        assert!(
            result.is_ok(),
            "agent {i} joining a current database failed: {result:?}"
        );
    }
    assert_eq!(user_version(&db_path), MIGRATION_SQL.len() as i64);
}

/// Four agents, not two: contention shows up with a wider pool window, and a
/// green receipt here is what lets #2297 be closed as not-reproducible.
#[test]
fn migrations_concurrent_four_agents_all_succeed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = fresh_db(dir.path());
    let home = dir.path().join("home");

    let results = start_agents_at_once(&db_path, &home, 4);
    for (i, result) in results.iter().enumerate() {
        assert!(
            result.is_ok(),
            "agent {i} failed to start against the same database: {result:?}"
        );
    }
    assert_eq!(user_version(&db_path), MIGRATION_SQL.len() as i64);
}
