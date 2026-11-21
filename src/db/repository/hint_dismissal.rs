//! Hint Dismissal Repository
//!
//! Durable backing store for "hide this hint for a while". A dismissal is a
//! property of the reader, not of the session, so it lives in a global sidecar
//! table (migration `20261121000001_add_hint_dismissals.sql`) and never touches
//! a `sessions` row. The same table answers both frontends, which is why the
//! dismissal a reader makes in the window is already in effect in the terminal.
//!
//! The row stores an absolute `until_at`, not the moment of dismissal, so the
//! read is a single comparison: an expired row is simply not returned, and the
//! write path sweeps what has expired so the table stays the size of the tip
//! list rather than the size of the history.
//!
//! Nothing here knows what a hint says. The ids come from
//! [`crate::ui_core::hints`], and the text is free to change without moving a
//! single row.

use crate::db::Pool;
use crate::db::database::interact_err;
use anyhow::{Context, Result};

/// How long a dismissal lasts, in seconds, for a window measured in days.
fn days_to_seconds(days: i64) -> i64 {
    // Saturating on purpose: a caller passing a nonsense number gets the
    // longest representable window instead of a wrapped negative that would
    // expire the dismissal on the next frame.
    days.saturating_mul(86_400)
}

#[derive(Clone)]
pub struct HintDismissalRepository {
    pool: Pool,
}

impl HintDismissalRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Hide `hint_id` for `days` days from now.
    ///
    /// Idempotent: a repeat dismissal of the same id restarts its window, which
    /// is what the reader pressing the control twice means.
    pub async fn dismiss(&self, hint_id: &str, days: i64) -> Result<()> {
        let id = hint_id.to_string();
        let seconds = days_to_seconds(days);
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO hint_dismissals (hint_id, until_at) \
                     VALUES (?1, strftime('%s', 'now') + ?2) \
                     ON CONFLICT(hint_id) DO UPDATE \
                     SET until_at = strftime('%s', 'now') + ?2",
                    rusqlite::params![id, seconds],
                )?;
                // The sweep rides the write, in the same transaction-sized
                // round trip: a read must never take a write lock just to find
                // out what is hidden, and the table then stays the size of the
                // tip list instead of the size of the history.
                //
                // The sweep is the closure's tail expression, so its
                // `rusqlite::Result` is what pins the closure's error type.
                // A trailing `Ok(())` after two `?` sites leaves that type
                // uninferable, because rusqlite::Error has more than one
                // `From` impl in scope (E0282/E0283), which is the same
                // receipt as session_favorites.rs.
                conn.execute(
                    "DELETE FROM hint_dismissals WHERE until_at <= strftime('%s', 'now')",
                    [],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to dismiss hint")?;
        Ok(())
    }

    /// The ids still inside their window.
    ///
    /// Expired rows are filtered here rather than deleted, so a read never
    /// writes: the sweep belongs to the write path, and a frame that only wants
    /// to know what to hide must not take a write lock to find out.
    pub async fn active(&self) -> Result<Vec<String>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT hint_id FROM hint_dismissals \
                     WHERE until_at > strftime('%s', 'now')",
                )?;
                let mapped = stmt.query_map([], |r| r.get::<_, String>(0))?;
                // The error parameter is pinned explicitly: `anyhow::Result` is
                // in scope and rusqlite::Error has several `From` impls, so a
                // bare `Ok(vec)` leaves E uninferable (E0282/E0283). Same
                // receipt as session_favorites.rs.
                mapped.collect::<rusqlite::Result<Vec<String>>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read hint dismissals")
    }
}

#[cfg(test)]
mod tests {
    use super::days_to_seconds;

    #[test]
    fn a_week_is_seven_days_of_seconds() {
        assert_eq!(days_to_seconds(7), 604_800);
    }

    #[test]
    fn a_nonsense_window_saturates_instead_of_wrapping_negative() {
        // A wrapped value would expire the dismissal immediately, which looks
        // exactly like the feature being broken.
        assert!(days_to_seconds(i64::MAX) > 0);
        assert_eq!(days_to_seconds(0), 0);
    }
}
