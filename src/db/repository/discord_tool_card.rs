//! Durable Discord tool cards (#1880).
//!
//! A card lived only in an in-memory map, so a restart left its message with
//! nothing to page, expand or log. One row per card message holds the card's
//! state as JSON; the channel layer owns the shape of that JSON.

use anyhow::{Context, Result};
use deadpool_sqlite::Pool;
use rusqlite::OptionalExtension;
use rusqlite::params;

use crate::db::database::interact_err;

pub struct DiscordToolCardRepository {
    pool: Pool,
}

impl DiscordToolCardRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// The stored state for a card message, if any.
    pub async fn get(&self, message_id: u64) -> Result<Option<String>> {
        let id = message_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT state FROM discord_tool_cards WHERE message_id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read tool card")
    }

    /// Store (or replace) a card's state.
    pub async fn set(&self, message_id: u64, state: &str) -> Result<()> {
        let id = message_id.to_string();
        let state = state.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO discord_tool_cards (message_id, state, updated_at) \
                     VALUES (?1, ?2, strftime('%s','now')) \
                     ON CONFLICT(message_id) DO UPDATE SET \
                       state = excluded.state, \
                       updated_at = excluded.updated_at",
                    params![id, state],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to write tool card")?;
        Ok(())
    }
}
