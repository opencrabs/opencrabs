//! Message Reaction Repository
//!
//! Durable backing store for the reactions a stored transcript row carries
//! (#2825). A reaction is a property of the conversation the user is reading,
//! not of the message, so it lives in a sidecar table (migration
//! `20261106000001_add_message_reactions.sql`) and the write path never touches
//! the `messages` row.
//!
//! The key is `(message_id, emoji)`, so the same picture on the same row is one
//! reaction: [`toggle`](MessageReactionRepository::toggle) adds it when it is
//! absent and deletes it when it is there. That is the whole gesture a chip
//! offers, and it is why the table needs no actor column: there is one local
//! reader, and a second click is that reader taking it back.
//!
//! Foreign keys are on (`PRAGMA foreign_keys = ON` in `src/db/database.rs`) and
//! the table carries `ON DELETE CASCADE`, so pruning a message takes its
//! reactions with it. No orphan sweep exists and none is needed.

use crate::db::Pool;
use crate::db::database::interact_err;
use anyhow::{Context, Result};
use rusqlite::params_from_iter;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Clone)]
pub struct MessageReactionRepository {
    pool: Pool,
}

impl MessageReactionRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Toggle one picture on one row. Returns whether the reaction is there
    /// afterwards, so the caller can say which way it went without a second
    /// read.
    ///
    /// The delete is attempted first: it is the case that needs no new row, and
    /// a zero-row delete is exactly the "not reacted yet" answer the insert
    /// then acts on.
    pub async fn toggle(&self, message_id: Uuid, emoji: &str) -> Result<bool> {
        let mid = message_id.to_string();
        let name = emoji.to_string();
        let reacted = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let removed = conn.execute(
                    "DELETE FROM message_reactions WHERE message_id = ?1 AND emoji = ?2",
                    rusqlite::params![mid, name],
                )?;
                if removed > 0 {
                    return Ok::<_, rusqlite::Error>(false);
                }
                conn.execute(
                    "INSERT INTO message_reactions (message_id, emoji) VALUES (?1, ?2) \
                     ON CONFLICT(message_id, emoji) DO NOTHING",
                    rusqlite::params![mid, name],
                )?;
                Ok(true)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to toggle a message reaction")?;
        Ok(reacted)
    }

    /// Every reaction on the given rows, keyed by message id, in the order the
    /// pictures were first used. Rows nobody reacted to are absent rather than
    /// present and empty, so a renderer can test one map lookup per row.
    ///
    /// One query for the whole window: the transcript asks about the rows it
    /// just loaded, and a read per row would be a query per visible message on
    /// every reload.
    pub async fn for_messages(
        &self,
        message_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<(String, usize)>>> {
        if message_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<String> = message_ids.iter().map(|id| id.to_string()).collect();
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT message_id, emoji, COUNT(*) FROM message_reactions \
             WHERE message_id IN ({placeholders}) \
             GROUP BY message_id, emoji ORDER BY MIN(created_at), emoji"
        );
        let rows = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare(&sql)?;
                let mapped = stmt.query_map(params_from_iter(ids.iter()), |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?;
                mapped.collect::<rusqlite::Result<Vec<_>>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read message reactions")?;
        let mut out: HashMap<Uuid, Vec<(String, usize)>> = HashMap::new();
        for (message_id, emoji, count) in rows {
            let Ok(id) = Uuid::parse_str(&message_id) else {
                continue;
            };
            out.entry(id)
                .or_default()
                .push((emoji, usize::try_from(count).unwrap_or(1)));
        }
        Ok(out)
    }

    /// The reactions on one row, for the read-back after a toggle. Narrower
    /// than [`for_messages`](Self::for_messages) so a click does not re-read
    /// the whole window.
    pub async fn for_message(&self, message_id: Uuid) -> Result<Vec<(String, usize)>> {
        let mut map = self.for_messages(&[message_id]).await?;
        Ok(map.remove(&message_id).unwrap_or_default())
    }
}
