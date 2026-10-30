//! Session Repository
//!
//! Database operations for sessions.

use crate::db::Pool;
use crate::db::database::interact_err;
use crate::db::models::Session;
use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use uuid::Uuid;

/// Options for listing sessions
#[derive(Debug, Clone, Default)]
pub struct SessionListOptions {
    /// Include archived sessions
    pub include_archived: bool,
    /// Maximum number of sessions to return
    pub limit: Option<usize>,
    /// Number of sessions to skip
    pub offset: usize,
    /// Filter by title substring (case-insensitive LIKE match)
    pub query: Option<String>,
    /// Include sessions spawned for sub-agents (`subagent: …`).
    ///
    /// Off by default: a sub-agent session is an implementation detail of one
    /// `spawn_agent` call, not somewhere the user ever resumes, and a busy turn
    /// can create several. Listing them buried real sessions (#931). They stay
    /// in the database either way — this only decides whether a session list
    /// shows them.
    pub include_subagents: bool,
}

/// Title prefix every spawned sub-agent session carries, set by `spawn_agent`.
pub const SUBAGENT_TITLE_PREFIX: &str = "subagent:";

/// Repository for session operations
#[derive(Clone)]
pub struct SessionRepository {
    pool: Pool,
}

impl SessionRepository {
    /// Create a new session repository
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Find session by ID
    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<Session>> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare_cached("SELECT * FROM sessions WHERE id = ?1")?
                    .query_row(params![id_str], Session::from_row)
                    .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find session")
    }

    /// Find most recent non-archived session by exact title.
    pub async fn find_by_title(&self, title: &str) -> Result<Option<Session>> {
        let t = title.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare_cached(
                    "SELECT * FROM sessions WHERE title = ?1 AND archived_at IS NULL ORDER BY updated_at DESC LIMIT 1",
                )?
                .query_row(params![t], Session::from_row)
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find session by title")
    }

    /// Find the most recent non-archived session whose title ends with
    /// `suffix`. Used by channel handlers to look up sessions by a stable
    /// platform id embedded in the title (e.g. Telegram `[chat:12345]`)
    /// regardless of any user-driven label rename.
    ///
    /// 2026-04-25: a Telegram group renamed from "🦀 KRAB-INCEPTION 🦀"
    /// to "🦀 HEY IOLO BUILD 🦀" produced two distinct sessions because
    /// `find_by_title` only matched the exact (post-rename) string.
    /// Embedding the stable chat_id as a `[chat:N]` suffix on creation
    /// and looking up by that suffix here keeps a single session per
    /// underlying chat across renames.
    pub async fn find_by_title_suffix(&self, suffix: &str) -> Result<Option<Session>> {
        let pattern = format!("%{}", suffix);
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare_cached(
                    "SELECT * FROM sessions WHERE title LIKE ?1 ESCAPE '\\' AND archived_at IS NULL \
                     ORDER BY updated_at DESC LIMIT 1",
                )?
                .query_row(params![pattern], Session::from_row)
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find session by title suffix")
    }

    /// The live session carrying this stable `[chat:<id>]` key (#2183).
    ///
    /// The key is the unique identity of a channel session, so it is the
    /// lookup that should have been first: matching on a title suffix is a
    /// proxy for it, and the proxy misses whenever a title was auto-renamed.
    pub async fn find_by_channel_chat_key(&self, key: &str) -> Result<Option<Session>> {
        let key = key.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare_cached(
                    "SELECT * FROM sessions WHERE channel_chat_key = ?1 AND archived_at IS NULL \
                     ORDER BY updated_at DESC LIMIT 1",
                )?
                .query_row(params![key], Session::from_row)
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find session by chat key")
    }

    /// Give up this session's claim on its chat key without archiving it
    /// (#2186).
    ///
    /// The unique index is partial on live rows, so "one live session per
    /// chat" is enforced by the key. A `/new` that must leave the previous
    /// session visible (the owner case) therefore cannot just insert a second
    /// keyed row: the insert would resolve the old one and `/new` would be a
    /// no-op. Clearing the key frees the slot while the row stays live, so
    /// `/sessions` keeps showing it.
    ///
    /// `updated_at` is deliberately untouched: the title-suffix fallback in
    /// `resolve_or_create_channel_session` orders by `updated_at DESC`, so
    /// bumping it here would make the released row win that fallback over the
    /// session that just took its key.
    pub async fn release_channel_chat_key(&self, id: Uuid) -> Result<()> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET channel_chat_key = NULL WHERE id = ?1",
                    params![id_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to release session chat key")?;

        tracing::debug!("Released chat key for session: {}", id);
        Ok(())
    }

    /// Create a new session
    pub async fn create(&self, session: &Session) -> Result<()> {
        let s = session.clone();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO sessions (id, title, model, provider_name, created_at, updated_at,
                                          archived_at, token_count, total_cost, working_directory, auto_title_attempted, project_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        s.id.to_string(),
                        s.title,
                        s.model,
                        s.provider_name,
                        s.created_at.timestamp(),
                        s.updated_at.timestamp(),
                        s.archived_at.map(|dt| dt.timestamp()),
                        s.token_count,
                        s.total_cost,
                        s.working_directory,
                        s.auto_title_attempted,
                        s.project_id.map(|id| id.to_string()),
                    ],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to create session")?;

        tracing::debug!("Created session: {}", session.id);
        Ok(())
    }

    /// Atomically insert a channel session carrying its stable chat key
    /// (#1721). On unique-index conflict, another writer already created the
    /// session for this chat (a second process the single-flight gate cannot
    /// span): the existing winner is returned so the caller's message routes
    /// into it instead of forking the chat. Generic `create` never writes
    /// chat keys; this is the only INSERT path that does.
    pub async fn insert_or_resolve_channel(
        &self,
        session: &Session,
        chat_key: &str,
    ) -> Result<Session> {
        self.insert_or_resolve_channel_reported(session, chat_key)
            .await
            .map(|(session, _)| session)
    }

    /// Same as [`Self::insert_or_resolve_channel`], also reporting whether this
    /// call created the row or joined one that already existed (#2183).
    ///
    /// The caller could not tell the two apart, so it logged "created" on
    /// every message of a long-lived chat.
    pub async fn insert_or_resolve_channel_reported(
        &self,
        session: &Session,
        chat_key: &str,
    ) -> Result<(Session, bool)> {
        let mut s = session.clone();
        s.channel_chat_key = Some(chat_key.to_string());
        let cloned = s.clone();
        let inserted = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO sessions (id, title, model, provider_name, created_at, updated_at,
                                           archived_at, token_count, total_cost, working_directory,
                                           auto_title_attempted, project_id, channel_chat_key)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(channel_chat_key) WHERE channel_chat_key IS NOT NULL AND archived_at IS NULL DO NOTHING",
                    params![
                        cloned.id.to_string(),
                        cloned.title,
                        cloned.model,
                        cloned.provider_name,
                        cloned.created_at.timestamp(),
                        cloned.updated_at.timestamp(),
                        cloned.archived_at.map(|dt| dt.timestamp()),
                        cloned.token_count,
                        cloned.total_cost,
                        cloned.working_directory,
                        cloned.auto_title_attempted,
                        cloned.project_id.map(|id| id.to_string()),
                        cloned.channel_chat_key,
                    ],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to create channel session")?;

        if inserted > 0 {
            tracing::debug!("Created session: {} (chat key {})", s.id, chat_key);
            return Ok((s, true));
        }

        // Lost the race: resolve the winner by key.
        let key_row = chat_key.to_string();
        let winner = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare_cached(
                    "SELECT * FROM sessions WHERE channel_chat_key = ?1 AND archived_at IS NULL \
                     ORDER BY updated_at DESC LIMIT 1",
                )?
                .query_row(params![key_row], Session::from_row)
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to resolve winning session by chat key")?
            .with_context(|| {
                format!("Conflict resolved but no live session carries chat key {chat_key}")
            })?;
        tracing::info!(
            "Channel session insert raced a concurrent creator: routed message to winner {} (chat key {})",
            winner.id,
            chat_key
        );
        Ok((winner, false))
    }

    /// Update an existing session
    pub async fn update(&self, session: &Session) -> Result<()> {
        let s = session.clone();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions
                     SET title = ?1, model = ?2, provider_name = ?3, updated_at = ?4,
                         archived_at = ?5, token_count = ?6, total_cost = ?7, working_directory = ?8,
                         auto_title_attempted = ?9, project_id = ?10
                     WHERE id = ?11",
                    params![
                        s.title,
                        s.model,
                        s.provider_name,
                        s.updated_at.timestamp(),
                        s.archived_at.map(|dt| dt.timestamp()),
                        s.token_count,
                        s.total_cost,
                        s.working_directory,
                        s.auto_title_attempted,
                        s.project_id.map(|id| id.to_string()),
                        s.id.to_string(),
                    ],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to update session")?;

        tracing::debug!("Updated session: {}", session.id);
        Ok(())
    }

    /// Bulk-write a `(provider, model)` pair to every non-archived session
    /// in one statement (#1367). Rows already on the pair are excluded by
    /// the NULL-safe `IS NOT` predicate, and `updated_at` is never touched,
    /// so recency ordering in `/sessions` and title-suffix/most-recent
    /// lookups stays intact. Returns the number of rows changed.
    /// How many sessions `set_provider_model_for_all` would change, with the same
    /// predicate, so the confirmation prompt quotes the number the write touches (#2042).
    pub async fn count_provider_model_for_all(
        &self,
        provider: String,
        model: String,
    ) -> Result<usize> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM sessions
                     WHERE archived_at IS NULL
                       AND (provider_name IS NOT ?1 OR model IS NOT ?2)",
                    params![provider, model],
                    |row| row.get::<_, i64>(0),
                )
            })
            .await
            .map_err(interact_err)?
            .map(|n| n.max(0) as usize)
            .context("Failed to count sessions for apply-to-all")
    }

    pub async fn set_provider_model_for_all(
        &self,
        provider: String,
        model: String,
    ) -> Result<usize> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions
                     SET provider_name = ?1, model = ?2
                     WHERE archived_at IS NULL
                       AND (provider_name IS NOT ?1 OR model IS NOT ?2)",
                    params![provider, model],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to bulk-set provider/model")
    }

    /// Delete a session's messages but keep the session row for usage tracking.
    /// The session is archived (soft-deleted) so it no longer appears in the
    /// session list, while usage_ledger joins still resolve its metadata.
    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                // Remove heavy data (messages, files) but preserve the session row
                conn.execute(
                    "DELETE FROM messages WHERE session_id = ?1",
                    params![id_str],
                )?;
                conn.execute("DELETE FROM files WHERE session_id = ?1", params![id_str])?;
                // Mark as archived so it's hidden from the session list
                conn.execute(
                    "UPDATE sessions SET archived_at = strftime('%s', 'now') WHERE id = ?1",
                    params![id_str],
                )?;
                Ok::<_, rusqlite::Error>(())
            })
            .await
            .map_err(interact_err)?
            .context("Failed to delete session")?;

        tracing::debug!("Soft-deleted session (preserved for usage): {}", id);
        Ok(())
    }

    /// Every table that stores rows against a `session_id`.
    ///
    /// Only `messages` and `files` declare `ON DELETE CASCADE`; the other nine
    /// carry a bare `session_id` column with no foreign key, so deleting a
    /// session row leaves their rows behind forever. Listed explicitly so a new
    /// table is a visible omission here rather than a silent leak.
    const SESSION_SCOPED_TABLES: &'static [&'static str] = &[
        "messages",
        "files",
        "usage_ledger",
        "pending_requests",
        "feedback_ledger",
        "tool_executions",
        "goal_state",
        "background_tasks",
        "plan_cards",
        "phantom_events",
        "streaming_recoveries",
    ];

    /// Sub-agent sessions last written before `cutoff_unix`, oldest first.
    ///
    /// Returned rather than deleted here because their on-disk plan files
    /// resolve through the session's project, so the caller has to read that
    /// before the row goes away.
    pub async fn subagent_sessions_older_than(&self, cutoff_unix: i64) -> Result<Vec<Uuid>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT id FROM sessions \
                     WHERE title LIKE ?1 AND updated_at < ?2 \
                     ORDER BY updated_at ASC",
                )?;
                let rows = stmt.query_map(
                    params![format!("{SUBAGENT_TITLE_PREFIX}%"), cutoff_unix],
                    |row| row.get::<_, String>(0),
                )?;
                let mut out = Vec::new();
                for r in rows {
                    match Uuid::parse_str(&r?) {
                        Ok(id) => out.push(id),
                        // A malformed id would otherwise abort the whole sweep.
                        Err(e) => tracing::warn!("Skipping session with unparseable id: {e}"),
                    }
                }
                Ok::<_, rusqlite::Error>(out)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to list expired sub-agent sessions")
    }

    /// Permanently remove a session and every row keyed to it.
    ///
    /// Unlike [`delete`](Self::delete), which archives the row and keeps it for
    /// usage joins, this is a real delete — used only by the sub-agent sweep,
    /// where the whole point is to stop the data accumulating (#931). Runs in
    /// one transaction so a failure part-way cannot leave a session row whose
    /// children are gone.
    pub async fn purge(&self, id: Uuid) -> Result<()> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let tx = conn.transaction()?;
                for table in Self::SESSION_SCOPED_TABLES {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE session_id = ?1"),
                        params![id_str],
                    )?;
                }
                tx.execute("DELETE FROM sessions WHERE id = ?1", params![id_str])?;
                tx.commit()?;
                Ok::<_, rusqlite::Error>(())
            })
            .await
            .map_err(interact_err)?
            .context("Failed to purge session")?;
        Ok(())
    }

    /// List all sessions (most recent first)
    pub async fn list(&self, options: SessionListOptions) -> Result<Vec<Session>> {
        let include_archived = options.include_archived;
        let limit = options.limit;
        let offset = options.offset;
        let query = options.query;
        let include_subagents = options.include_subagents;

        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut conditions = Vec::new();
                let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

                if !include_archived {
                    conditions.push("archived_at IS NULL".to_string());
                }

                if !include_subagents {
                    // `title IS NULL` must pass: an untitled session is a real
                    // one, and `NULL NOT LIKE …` is NULL, which SQLite treats
                    // as false — so without this every untitled session would
                    // silently vanish from the list.
                    conditions.push(format!(
                        "(title IS NULL OR title NOT LIKE '{SUBAGENT_TITLE_PREFIX}%')"
                    ));
                }

                if let Some(ref q) = query {
                    params_vec.push(Box::new(format!("%{}%", q)));
                    conditions.push(format!("title LIKE ?{}", params_vec.len()));
                }

                let where_sql = if conditions.is_empty() {
                    String::new()
                } else {
                    format!(" WHERE {}", conditions.join(" AND "))
                };

                let limit_sql = match limit {
                    Some(lim) => {
                        params_vec.push(Box::new(lim as i64));
                        params_vec.push(Box::new(offset as i64));
                        let lim_idx = params_vec.len() - 1;
                        let off_idx = params_vec.len();
                        format!(" LIMIT ?{} OFFSET ?{}", lim_idx, off_idx)
                    }
                    None => String::new(),
                };

                let sql = format!(
                    "SELECT * FROM sessions{} ORDER BY updated_at DESC{}",
                    where_sql, limit_sql
                );

                let mut stmt = conn.prepare_cached(&sql)?;
                let params_refs: Vec<&dyn rusqlite::types::ToSql> =
                    params_vec.iter().map(|p| p.as_ref()).collect();
                let rows = stmt.query_map(params_refs.as_slice(), Session::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to list sessions")
    }

    /// List non-archived sessions
    pub async fn list_active(&self) -> Result<Vec<Session>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM sessions WHERE archived_at IS NULL ORDER BY updated_at DESC",
                )?;
                let rows = stmt.query_map([], Session::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to list active sessions")
    }

    /// List archived sessions
    pub async fn list_archived(&self) -> Result<Vec<Session>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM sessions WHERE archived_at IS NOT NULL ORDER BY updated_at DESC",
                )?;
                let rows = stmt.query_map([], Session::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to list archived sessions")
    }

    /// Archive a session
    pub async fn archive(&self, id: Uuid) -> Result<()> {
        let now = Utc::now();
        let id_str = id.to_string();

        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET archived_at = ?1, updated_at = ?2 WHERE id = ?3",
                    params![now.timestamp(), now.timestamp(), id_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to archive session")?;

        tracing::debug!("Archived session: {}", id);
        Ok(())
    }

    /// Unarchive a session
    pub async fn unarchive(&self, id: Uuid) -> Result<()> {
        let now = Utc::now();
        let id_str = id.to_string();

        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET archived_at = NULL, updated_at = ?1 WHERE id = ?2",
                    params![now.timestamp(), id_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to unarchive session")?;

        tracing::debug!("Unarchived session: {}", id);
        Ok(())
    }

    /// Update session statistics
    ///
    /// Atomic by construction: the increments happen in one SQL statement, so
    /// two turns finishing on the same session at the same time cannot lose
    /// one of the two (#2180). The old caller read the row, added in Rust and
    /// wrote the whole row back, which silently dropped one increment whenever
    /// a second turn landed inside that window.
    ///
    /// `token_delta` is `i64` because the column is: a session's lifetime
    /// counter passed `i32::MAX` on a long-lived install, and a narrower delta
    /// would truncate or overflow on the way in.
    pub async fn update_stats(&self, id: Uuid, token_delta: i64, cost_delta: f64) -> Result<()> {
        let updated_at = Utc::now();
        let id_str = id.to_string();

        let updated = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions
                     SET token_count = token_count + ?1,
                         total_cost = total_cost + ?2,
                         updated_at = ?3
                     WHERE id = ?4",
                    params![token_delta, cost_delta, updated_at.timestamp(), id_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to update session stats")?;

        // A delta that matched no row means the caller believes a session
        // exists and it does not. The old read-modify-write surfaced that as a
        // "session not found" error; an UPDATE that quietly matches nothing
        // would lose the usage instead (#2180).
        if updated == 0 {
            anyhow::bail!("Session {id} not found while updating its usage");
        }

        Ok(())
    }

    /// Count sessions
    pub async fn count(&self, archived_only: bool) -> Result<i64> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let sql = if archived_only {
                    "SELECT COUNT(*) FROM sessions WHERE archived_at IS NOT NULL"
                } else {
                    "SELECT COUNT(*) FROM sessions WHERE archived_at IS NULL"
                };
                conn.query_row(sql, [], |row| row.get(0))
            })
            .await
            .map_err(interact_err)?
            .context("Failed to count sessions")
    }
}

/// Extension trait for rusqlite to add `.optional()` to query results
trait OptionalExt<T> {
    fn optional(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn optional(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
