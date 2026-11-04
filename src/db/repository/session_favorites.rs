//! Session Favorites Repository
//!
//! Durable backing store for the sessions dialog's favorite pins. A favorite
//! is a property of the user's view, not of the session, so it lives in a
//! sidecar table (migration `20261031000001_add_session_favorites.sql`) and
//! the write path never touches the `sessions` row. That is the whole point:
//! pinning must not look like activity, because `updated_at` is the recency
//! signal every other consumer trusts. Unpinning therefore returns the session
//! to its true recency slot instead of leaving it floating at the top.
//!
//! Foreign keys are on (`PRAGMA foreign_keys = ON` in `src/db/database.rs`),
//! and the table carries `ON DELETE CASCADE`, so session pruning drops the
//! pin with the session. No orphan sweep exists and none is needed.

use crate::db::Pool;
use crate::db::database::interact_err;
use anyhow::{Context, Result};

#[derive(Clone)]
pub struct SessionFavoritesRepository {
    pool: Pool,
}

impl SessionFavoritesRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Pin `session_id`. Idempotent: the session id is the primary key, so a
    /// repeat pin refreshes `favorited_at` and changes nothing else.
    pub async fn add(&self, session_id: &str) -> Result<()> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO session_favorites (session_id) VALUES (?1) \
                     ON CONFLICT(session_id) DO UPDATE \
                     SET favorited_at = strftime('%s', 'now')",
                    rusqlite::params![sid],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to favorite session")?;
        Ok(())
    }

    /// Unpin `session_id`. Deleting the row is the unpin; the `sessions` row
    /// it pointed at is untouched.
    pub async fn remove(&self, session_id: &str) -> Result<()> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "DELETE FROM session_favorites WHERE session_id = ?1",
                    rusqlite::params![sid],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to unfavorite session")?;
        Ok(())
    }

    /// The full pin set, as session ids. The sessions dialog hydrates this
    /// into an in-memory set at load time and consults it when rendering and
    /// sorting, so the row count stays small and no per-row query exists.
    pub async fn all(&self) -> Result<Vec<String>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare("SELECT session_id FROM session_favorites")?;
                let mapped = stmt.query_map([], |r| r.get::<_, String>(0))?;
                // Pin the error parameter explicitly: `anyhow::Result` is in
                // scope and rusqlite::Error has several `From` impls, so a
                // bare `Ok(vec)` leaves E uninferable (E0282/E0283). Same
                // receipt as session_skills.rs.
                mapped.collect::<rusqlite::Result<Vec<String>>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read session favorites")
    }
}
