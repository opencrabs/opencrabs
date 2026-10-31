//! Database connection management, pool configuration, and extension traits.

use anyhow::{Context, Result};
use deadpool_sqlite::{Config, Hook, InteractError, Pool as DeadPool, Runtime};
use rusqlite_migration::{M, Migrations};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

/// Flag set when the startup integrity check detects corruption.
static DB_INTEGRITY_FAILED: AtomicBool = AtomicBool::new(false);

/// Global pool handle set once on startup. Used by components that can't
/// otherwise receive a pool via dependency injection (e.g. CLI providers
/// that need to persist streaming progress directly to the DB without
/// waiting for tool_loop to batch-write at end of iteration).
static GLOBAL_POOL: OnceLock<Pool> = OnceLock::new();

/// Get the global DB pool if it has been set. Returns `None` before the
/// first `Database::connect` call (e.g. in unit tests).
pub fn global_pool() -> Option<&'static Pool> {
    GLOBAL_POOL.get()
}

/// Returns true (once) if the last startup integrity check detected corruption.
pub fn db_integrity_failed() -> bool {
    DB_INTEGRITY_FAILED.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// Non-consuming read of the same flag (#1779 defect 4).
///
/// `db_integrity_failed()` SWAPS, so the first reader wins and every later
/// reader sees `false`. The startup path in `cmd_chat_inner` runs before the TUI
/// is built, so a consuming read there would silence the banner for the one user
/// actually looking at a screen. Logging surfaces (daemon startup, `doctor`)
/// peek; the TUI banner keeps the consuming read.
pub fn db_integrity_failed_now() -> bool {
    DB_INTEGRITY_FAILED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Type alias for database pool
pub type Pool = DeadPool;

/// Map deadpool InteractError to anyhow
pub fn interact_err(e: InteractError) -> anyhow::Error {
    anyhow::anyhow!("Database interact error: {}", e)
}

/// Build the full ordered migration list.
///
/// Extracted from [`Database::run_migrations`] so tests can prepare a
/// database at a specific version (e.g. `to_version(32)`) before exercising
/// the migration runner. Order and content are the migration contract:
/// never reorder or edit applied entries, only append new ones.
/// Every migration, oldest first. Adding one is adding its `include_str!`
/// here and nothing else: [`Database::MIGRATION_COUNT`] is this list's
/// length, so the count cannot drift from the list (#1354, after #1292).
pub(crate) const MIGRATION_SQL: &[&str] = &[
    include_str!("../migrations/20251028000001_initial_schema.sql"),
    include_str!("../migrations/20251028000002_modernize_schema.sql"),
    include_str!("../migrations/20251111000001_add_plans.sql"),
    include_str!("../migrations/20251113000001_add_plan_enhancements.sql"),
    include_str!("../migrations/20260224000001_add_a2a_tasks.sql"),
    include_str!("../migrations/20260226000001_add_session_provider.sql"),
    include_str!("../migrations/20260305000001_add_channel_messages.sql"),
    include_str!("../migrations/20260305000002_add_cron_jobs.sql"),
    include_str!("../migrations/20260306000001_add_usage_ledger.sql"),
    include_str!("../migrations/20260307000001_add_session_working_dir.sql"),
    include_str!("../migrations/20260308000001_add_pending_requests.sql"),
    include_str!("../migrations/20260330000001_pending_requests_channel_chat_id.sql"),
    include_str!("../migrations/20260402000001_add_cron_job_runs.sql"),
    include_str!("../migrations/20260412000001_add_feedback_ledger.sql"),
    include_str!("../migrations/20260415000001_add_tool_executions.sql"),
    include_str!("../migrations/20260415000002_add_session_category.sql"),
    include_str!("../migrations/20260415000003_fix_tool_executions_schema.sql"),
    include_str!("../migrations/20260416000001_add_message_input_tokens.sql"),
    include_str!("../migrations/20260421000001_add_message_thinking.sql"),
    include_str!("../migrations/20260426000001_add_recent_paths.sql"),
    include_str!("../migrations/20260507000001_add_cron_deliver_api_key.sql"),
    include_str!("../migrations/20260517000001_cron_jobs_text_recast.sql"),
    include_str!("../migrations/20260522000001_add_auto_title_attempted.sql"),
    include_str!("../migrations/20260529000001_add_channel_thread_id.sql"),
    include_str!("../migrations/20260606000001_add_message_cache_tokens.sql"),
    include_str!("../migrations/20260608000001_add_cron_job_profile.sql"),
    include_str!("../migrations/20260614000001_add_projects_and_file_size.sql"),
    include_str!("../migrations/20260626000001_add_goal_state.sql"),
    include_str!("../migrations/20260706000001_add_usage_ledger_provider.sql"),
    include_str!("../migrations/20260713000001_drop_orphaned_plans_tables.sql"),
    include_str!("../migrations/20260725000001_add_background_tasks.sql"),
    include_str!("../migrations/20260726000001_add_plan_cards.sql"),
    include_str!("../migrations/20260731000001_add_analytics_events.sql"),
    include_str!("../migrations/20260808000001_add_message_duration.sql"),
    include_str!("../migrations/20260822000001_add_a2a_context_sessions.sql"),
    include_str!("../migrations/20260826000001_add_session_bindings.sql"),
    include_str!("../migrations/20260828000001_pending_requests_origin.sql"),
    include_str!("../migrations/20260902000001_add_pending_followups.sql"),
    include_str!("../migrations/20260904000000_add_pending_followups_host_markdown.sql"),
    // #111: durable notify queue — parked session_notify / background-task
    // pushes survive restarts and re-offer at boot. Kept in filename order per
    // the list invariant; databases already stamped past this index are
    // repaired by `heal_notify_queue` after `to_latest` (#1401).
    include_str!("../migrations/20260906000001_add_notify_queue.sql"),
    include_str!("../migrations/20260908000001_pending_requests_thread_id.sql"),
    // #1510: projects.repo_remote, the adoption-only second identity. Appended
    // last per the list invariant; the column is NULL by design on existing
    // rows so no heal pass is needed.
    include_str!("../migrations/20260912000001_add_project_repo_remote.sql"),
    // #138: durable seen-skills store — the post-compaction skill inventory
    // stamp (#125/#131) survives daemon restarts: every mark_seen writes a row,
    // boot hydrates the in-memory registry back. Appended last per the list
    // invariant; the CREATE is idempotent, so no heal pass is needed.
    include_str!("../migrations/20260913000001_add_session_seen_skills.sql"),
    // #1529: per-newsletter poll cursors. Idempotent CREATE, so no heal
    // pass; appended last per the list invariant above.
    include_str!("../migrations/20260914000001_add_whatsapp_newsletter_cursors.sql"),
    // Trigger-gated cron execution: the probe command, its condition and the
    // goal-dispatch flag. ALTER TABLE ADD COLUMN, so appended last per the
    // list invariant above.
    include_str!("../migrations/20260915000001_add_cron_trigger_pipeline.sql"),
    // #1648: L1 exact-decision-reuse ring — versioned decision cache table.
    // Idempotent CREATE + index, so no heal pass needed for stamp drift
    // (same rationale as the #1529 entry above); nothing reads or writes it
    // until the [decisions] tooling lands (PR2). Appended last per the list
    // invariant.
    include_str!("../migrations/20260921000001_add_decision_cache.sql"),
    // #1648 PR2: per-tier counters behind the release-day keep-or-cut call
    // (shadow would-hits, live reuses). Idempotent CREATE, so no heal pass;
    // appended last per the list invariant.
    include_str!("../migrations/20260921000002_add_decision_stats.sql"),
    // #1721: stable chat identity on sessions (column + backfill + dedup +
    // unique index). Column add is idempotent-safe; the dedup must run
    // before the unique index or duplicated keys fail the migration.
    include_str!("../migrations/20260925220000_add_session_channel_chat_key.sql"),
    // #1705: audit-trail READ + OUTCOME columns — turn_retrievals (one row
    // per read-class retrieval) and turn_outcomes (mechanical per-turn
    // verdict). Idempotent CREATE + indexes; both stay empty until
    // [features] audit_recording = true, and nothing reads them except the
    // /audit viewer. Appended last per the list invariant.
    include_str!("../migrations/20260926000001_add_audit_turn_retrievals.sql"),
    // FORK (#544): the one-shot retirement flag. A cron job with
    // `run_once = 1` is disabled by the same dispatch-time schedule advance
    // that moves every job's `next_run_at`, so a spent one-shot reads
    // `enabled = 0` instead of parking armed until the same date next year.
    include_str!("../migrations/20260927000001_add_cron_run_once.sql"),
    // #2010: the Telegram bubble id a turn was streaming into, so a boot
    // resume can edit that bubble instead of opening a second one beside the
    // partial answer the killed process left on screen. ALTER TABLE ADD
    // COLUMN with the column NULL by design on every existing row, so no
    // heal pass is needed (same shape as the project_repo_remote note above).
    // Appended last per the list invariant.
    include_str!("../migrations/20261009000001_pending_requests_channel_message_id.sql"),
    // #2027: the per-row cache-read count behind the By Model cache share.
    // ALTER TABLE ADD COLUMN, NULL by design on every existing row (there is
    // no split to backfill), so no heal pass is needed. Appended last per the
    // list invariant above.
    include_str!("../migrations/20261015000001_add_usage_ledger_cache_read_tokens.sql"),
    include_str!("../migrations/20261016000001_add_discord_tool_cards.sql"),
    include_str!("../migrations/20261019000001_add_usage_ledger_prompt_tokens.sql"),
    // Sessions dialog: the pins behind "favorite a session and keep it at the
    // top". An idempotent CREATE in a sidecar table rather than a `sessions`
    // column, so nothing has to backfill and the pin write never nudges
    // `updated_at` (unpinning returns the session to its real recency slot).
    // Appended last per the list invariant above.
    include_str!("../migrations/20261031000001_add_session_favorites.sql"),
];

pub(crate) fn build_migrations() -> Migrations<'static> {
    Migrations::new(MIGRATION_SQL.iter().copied().map(M::up).collect())
}

/// Heal a database that partially carries migration 33's artifacts (#937).
///
/// If any of the three `tool_executions` columns added by migration 33
/// already exist, add whichever are missing and make sure the analytics
/// tables and indexes exist, then return `true` so the caller stamps
/// `user_version` to 33. When nothing pre-exists returns `false` and the
/// normal migration runs as-is.
///
/// The schema below mirrors `src/migrations/20260731000001_add_analytics_events.sql`,
/// which stays the source of truth — keep both in sync.
pub(crate) fn heal_analytics_migration_33(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    let cols: std::collections::HashSet<String> = conn
        .prepare("PRAGMA table_info(tool_executions)")?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(Result::ok)
        .collect();

    let pre_existing = ["provider", "model", "duration_ms"]
        .iter()
        .any(|c| cols.contains(*c));
    if !pre_existing {
        return Ok(false);
    }

    if !cols.contains("provider") {
        conn.execute_batch("ALTER TABLE tool_executions ADD COLUMN provider TEXT;")?;
    }
    if !cols.contains("model") {
        conn.execute_batch("ALTER TABLE tool_executions ADD COLUMN model TEXT;")?;
    }
    if !cols.contains("duration_ms") {
        conn.execute_batch("ALTER TABLE tool_executions ADD COLUMN duration_ms INTEGER;")?;
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS phantom_events (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL DEFAULT '',
            provider TEXT,
            model TEXT,
            detected_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            resolved INTEGER NOT NULL DEFAULT 0,
            retry_count INTEGER NOT NULL DEFAULT 0,
            tools_after_retry INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_phantom_events_detected_at ON phantom_events(detected_at);
        CREATE INDEX IF NOT EXISTS idx_phantom_events_session ON phantom_events(session_id);
        CREATE TABLE IF NOT EXISTS streaming_recoveries (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL DEFAULT '',
            provider TEXT,
            model TEXT,
            recovered_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            tool_count INTEGER NOT NULL DEFAULT 1
        );
        CREATE INDEX IF NOT EXISTS idx_streaming_recoveries_at ON streaming_recoveries(recovered_at);
        CREATE TABLE IF NOT EXISTS brain_verify_events (
            id TEXT PRIMARY KEY,
            file_name TEXT NOT NULL,
            event_type TEXT NOT NULL DEFAULT 'pass',
            violations TEXT,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
        );
        CREATE INDEX IF NOT EXISTS idx_brain_verify_events_at ON brain_verify_events(created_at);",
    )?;

    tracing::info!(
        "Healed partial migration 33 state in tool_executions — analytics schema completed, advancing to version 33"
    );
    Ok(true)
}

/// Database connection manager
pub struct Database {
    pub(crate) pool: Pool,
    /// The file the pool is backed by, or `None` for an in-memory database.
    ///
    /// Kept next to the pool rather than read off a connection because the
    /// pre-migration integrity check must reach the image *without* borrowing a
    /// pooled connection (#1779 defect 2): on a torn image `post_create` fails
    /// and the pool never yields one.
    pub(crate) db_path: Option<String>,
}

/// Apply PRAGMA settings to a rusqlite connection.
///
/// WAL mode, busy timeout, synchronous NORMAL, 64 MB page cache.
fn apply_pragmas(conn: &rusqlite::Connection) -> std::result::Result<(), rusqlite::Error> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 30000;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -65536;",
    )
}

/// PRAGMA settings for in-memory test databases.
///
/// WAL is a no-op (no file to journal to) but with `cache=shared` it triggers
/// spurious `database table is locked` (SQLITE_LOCKED, code 262) failures
/// under concurrent writers in release mode — the auto_title_e2e_test reliably
/// fell over here. MEMORY journal is the right mode for `:memory:` and shared
/// in-memory URIs; foreign keys stay on so FK violations still surface.
fn apply_pragmas_in_memory(
    conn: &rusqlite::Connection,
) -> std::result::Result<(), rusqlite::Error> {
    conn.execute_batch(
        "PRAGMA journal_mode = MEMORY;
         PRAGMA busy_timeout = 30000;
         PRAGMA synchronous = OFF;
         PRAGMA foreign_keys = ON;",
    )
}

impl Database {
    /// Connect to a SQLite database file.
    ///
    /// Pool is tuned for concurrent access:
    /// - WAL journal mode: readers never block on writers (eliminates the
    ///   "slow statement" timeouts seen under heavy TUI load)
    /// - 16 connections: enough headroom for TUI + all channel handlers
    /// - 30 s busy_timeout: graceful queuing instead of fast-fail on contention
    /// - synchronous = NORMAL: safe with WAL, ~3× faster writes than FULL
    pub async fn connect<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // Create parent directory if it doesn't exist
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            tracing::debug!("Creating database directory: {:?}", parent);
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create database directory: {:?}", parent))?;
        }

        let path_str = path.to_string_lossy().into_owned();

        let pool = Config::new(&path_str)
            .builder(Runtime::Tokio1)
            .context("Failed to build pool config")?
            .max_size(16)
            .post_create(Hook::async_fn(|conn, _| {
                Box::pin(async move {
                    conn.interact(|conn| apply_pragmas(conn))
                        .await
                        .map_err(|e| deadpool_sqlite::HookError::Message(e.to_string().into()))?
                        .map_err(|e| deadpool_sqlite::HookError::Message(e.to_string().into()))?;
                    Ok(())
                })
            }))
            .build()
            .context("Failed to create connection pool")?;

        tracing::info!(
            "Connected to database: {} (WAL, pool=16, busy_timeout=30s)",
            path_str
        );
        // Publish the pool process-wide so components without DI access (e.g.
        // CLI-provider streaming persistence) can still write to the DB. Only
        // the first connect wins; later sets are ignored.
        //
        // Deliberately NOT published in test builds (#2072). `GLOBAL_POOL` is a
        // `OnceLock` and never resets, so one test opening a file DB would hand
        // every later test in the binary a pool pointing at a temp file it then
        // deletes. That made those tests depend on run order; #2071 was the
        // first symptom.
        #[cfg(not(test))]
        let _ = GLOBAL_POOL.set(pool.clone());
        Ok(Self {
            pool,
            db_path: Some(path_str),
        })
    }

    /// Connect to an in-memory database (for testing)
    ///
    /// Pool is capped to `max_size = 1` — a single serialized connection.
    /// This is critical for correctness, not just isolation: SQLite's
    /// in-memory `cache=shared` mode handles cross-connection locking
    /// erratically (release-mode regularly hit `SQLITE_LOCKED` code 262
    /// with WAL pragma, and `MEMORY` journal mode shifted the symptom
    /// without removing it). With one connection there is no
    /// cross-connection lock contention, every interact() runs
    /// sequentially, and tests behave identically in debug and release.
    ///
    /// Each call still uses a unique URI so parallel tests never see
    /// each other's data.
    pub async fn connect_in_memory() -> Result<Self> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let uri = format!("file:mem_{}?mode=memory&cache=shared", id);
        let pool = Config::new(uri)
            .builder(Runtime::Tokio1)
            .context("Failed to build pool config")?
            .max_size(1)
            .post_create(Hook::async_fn(|conn, _| {
                Box::pin(async move {
                    conn.interact(|conn| apply_pragmas_in_memory(conn))
                        .await
                        .map_err(|e| deadpool_sqlite::HookError::Message(e.to_string().into()))?
                        .map_err(|e| deadpool_sqlite::HookError::Message(e.to_string().into()))?;
                    Ok(())
                })
            }))
            .build()
            .context("Failed to create in-memory pool")?;

        tracing::debug!("Connected to in-memory database");
        Ok(Self {
            pool,
            db_path: None,
        })
    }

    /// Get a reference to the connection pool
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Check if the database connection is still valid
    pub fn is_connected(&self) -> bool {
        self.pool.status().size > 0 || self.pool.status().max_size > 0
    }

    /// Total number of migrations, derived from `MIGRATION_SQL` so it can
    /// never lag behind the list.
    pub const MIGRATION_COUNT: usize = MIGRATION_SQL.len();

    /// Run database migrations
    pub async fn run_migrations(&self) -> Result<()> {
        // #2297: one migrator at a time, across processes.
        //
        // Two OpenCrabs processes (the TUI or daemon and a gui window, or two
        // gui windows) opening one ledger at the same moment both call
        // `to_latest`. `rusqlite_migration` reads `user_version` BEFORE it
        // opens its transaction, so the loser keeps a stale version, replays
        // the list against a schema the winner already advanced, and startup
        // dies with "db migrations: Failed to run database migrations". The
        // reproduction lives in
        // `src/tests/gui_two_agents_migrations_2297_test.rs`.
        //
        // The lock is taken before the snapshot probe reads the file, and
        // held to the end of this function, so the loser reads the finished
        // version instead of replaying into it. Called inline rather than on
        // `spawn_blocking` so it works under any executor (several callers
        // and tests poll this without a Tokio reactor installed).
        let _migration_lock = match self.db_path.as_deref() {
            Some(path) => Some(
                crate::db::migration_lock::MigrationLock::acquire(Path::new(path))
                    .context("failed to take the migration lock")?,
            ),
            // In-memory databases have no file to share.
            None => None,
        };

        let migrations = build_migrations();

        // #1779: snapshot the image BEFORE any migration can write to it.
        //
        // Deliberately a separate `interact` with its own anyhow error, not a
        // step inside the migration closure below: that closure's error type is
        // `rusqlite_migration::Error`, so a snapshot refusal returned from there
        // would be wrapped in "Failed to run database migrations", the exact
        // misleading receipt that made the rpi5 corruption unreadable for two
        // days. Here the message the operator reads is the one that explains the
        // restore path.
        //
        // `snapshot_dir()` is resolved on THIS task and moved into the closure:
        // `interact` runs its closure on a `spawn_blocking` thread (deadpool-sync
        // 0.2.0 `Scope::interact`), where the task-local profile-home override is
        // not set, so resolving it inside would silently write snapshots into the
        // default profile's home no matter which profile is starting.
        let snapshot_dir = crate::db::migration_snapshot::snapshot_dir();

        // The snapshot and the preflight exist to protect a write. A database
        // already at the latest migration, with every heal's precondition
        // satisfied, has nothing to protect. `VACUUM INTO` of that image is
        // what fills the disk. The probe opens the file itself: the pool's
        // post_create hook sets WAL, a write, and on a torn image that fails
        // before the preflight can tell the operator how to restore.
        let migration_count = Self::MIGRATION_COUNT as i64;
        let needs_snapshot = match self.db_path.as_deref() {
            Some(path) => crate::db::migration_snapshot::needs_pre_migration_snapshot_at(
                path,
                migration_count,
            )?,
            None => false,
        };

        // #1779 defect 2: this used to be the check *after* the migrations, so
        // on a corrupt image it never ran and the operator died on the
        // migration error with no restore path. It goes first, ahead of the
        // snapshot guard, because on the rpi5 shape `VACUUM INTO` fails too
        // (verified: SQLITE_CORRUPT while stepping), so snapshotting a torn
        // image buys nothing and only spends a write attempt.
        //
        // Skipped when there is no file to check (in-memory database), which is
        // every test that uses `connect_in_memory`. Also skipped when nothing
        // will write: the preflight opens the file read-write and walks every
        // page.
        if needs_snapshot {
            if let Some(path) = self.db_path.as_deref() {
                crate::db::migration_snapshot::integrity_preflight(path, &snapshot_dir)?;
            }

            self.pool
                .get()
                .await
                .context("Failed to get connection for pre-migration snapshot")?
                .interact(move |conn| crate::db::migration_snapshot::guard(conn, &snapshot_dir))
                .await
                .map_err(interact_err)??;
        }

        self.pool
            .get()
            .await
            .context("Failed to get connection for migrations")?
            .interact(
                move |conn| -> std::result::Result<(), rusqlite_migration::Error> {
                    // Detect databases previously managed by sqlx: if the _sqlx_migrations
                    // table exists but rusqlite_migration hasn't run yet (user_version == 0),
                    // stamp the current version so we don't re-run already-applied migrations.
                    let user_version: i64 =
                        conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
                    let has_sqlx: bool = conn
                        .prepare(
                            "SELECT COUNT(*) FROM sqlite_master \
                         WHERE type='table' AND name='_sqlx_migrations'",
                        )?
                        .query_row([], |r| r.get::<_, i64>(0))
                        .map(|c| c > 0)?;

                    if has_sqlx && user_version == 0 {
                        tracing::info!(
                            "Detected sqlx-managed database — stamping migration version to {}",
                            Self::MIGRATION_COUNT
                        );
                        conn.pragma_update(None, "user_version", Self::MIGRATION_COUNT as i64)?;
                    }

                    // Defensive heal for migration 33 (add_analytics_events), #937:
                    // databases that already carry some of migration 33's artifacts
                    // (e.g. a provider column added by an intermediate build) would
                    // crash the migration's ALTER TABLE with "duplicate column name".
                    // Only fires when migration 33 is the NEXT migration to run
                    // (user_version == 32) so it can never skip migrations 1-32.
                    // Heals the schema to the full migration-33 state and stamps
                    // user_version so to_latest skips it; when nothing pre-exists
                    // the normal migration runs untouched.
                    if user_version == 32 && heal_analytics_migration_33(conn)? {
                        conn.pragma_update(None, "user_version", 33i64)?;
                    }

                    // A migration that ALREADY ran under an older list order
                    // must be stamped past BEFORE to_latest, not healed after:
                    // replaying its ALTER fails the whole startup, so there is
                    // no "after" (#1401).
                    crate::db::migration_heal::skip_applied_thread_id_migration(
                        conn,
                        user_version,
                    )?;

                    migrations.to_latest(conn)?;

                    // Schema-checked heals run AFTER the stamp-driven pass: a
                    // migration that two branches appended at the same index can
                    // be skipped with the stamp already past it (#1401), and only
                    // the schema itself can say so.
                    crate::db::migration_heal::heal_pending_requests_origin(conn)?;
                    crate::db::migration_heal::heal_notify_queue(conn)?;
                    Ok(())
                },
            )
            .await
            .map_err(interact_err)?
            .context("Failed to run database migrations")?;

        tracing::info!("Database migrations completed");

        // #459/#677: no `PRAGMA integrity_check` here.
        //
        // The scan reads every page, so its cost is O(database size): measured
        // 93-149 s on the live ops database (2026-09-25) and 189-223 s per
        // daemon boot (2026-09-28). Placed here it is paid by EVERY database
        // open -- every cold CLI verb (`cron list`, `doctor`, `agent`, `run`)
        // and the daemon's own start -- and none of those foreground callers
        // can act on the verdict.
        //
        // The 2026-09-29 sync (d1e4b98ea) re-imported upstream's copy of this
        // block over #459's removal, restoring the O(database size) scan on the
        // open path. It is removed again here, and pinned by
        // `run_migrations_does_not_scan_on_every_open` in
        // `src/tests/db_pre_migration_snapshot_test.rs` so the next sync fails
        // loudly instead of silently re-adding it.
        //
        // The capability is retained, not deleted: `Database::run_integrity_check()`
        // (below) returns the verdict as a value, and the #1779 preflight still
        // runs, gated on a pending write, ahead of the migrations above.

        Ok(())
    }

    /// Close the database connection pool
    pub fn close(&self) {
        self.pool.close();
        tracing::info!("Database connection closed");
    }

    /// Get the current database user_version (migration level).
    ///
    /// This is used by the evolve tool to check if the current binary
    /// can handle the database before swapping.
    pub async fn get_user_version(&self) -> Result<i64> {
        let version = self
            .pool
            .get()
            .await
            .context("Failed to get connection for user_version")?
            .interact(|conn| conn.pragma_query_value(None, "user_version", |r| r.get(0)))
            .await
            .map_err(interact_err)?
            .context("Failed to read user_version")?;
        Ok(version)
    }
}

/// Extension trait for Pool convenience methods
pub trait PoolExt {
    fn is_connected(&self) -> bool;
}

impl PoolExt for Pool {
    fn is_connected(&self) -> bool {
        self.status().size > 0 || self.status().max_size > 0
    }
}
