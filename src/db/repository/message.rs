//! Message Repository
//!
//! Database operations for messages.

use crate::db::Pool;
use crate::db::database::interact_err;
use crate::db::models::Message;
use anyhow::{Context, Result};
use rusqlite::params;
use uuid::Uuid;

/// Repository for message operations
#[derive(Clone)]
pub struct MessageRepository {
    pool: Pool,
}

impl MessageRepository {
    /// Create a new message repository
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Find message by ID
    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<Message>> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare_cached("SELECT * FROM messages WHERE id = ?1")?
                    .query_row(params![id_str], Message::from_row)
                    .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find message")
    }

    /// Find all messages for a session
    pub async fn find_by_session(&self, session_id: Uuid) -> Result<Vec<Message>> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM messages WHERE session_id = ?1 ORDER BY sequence ASC",
                )?;
                let rows = stmt.query_map(params![sid], Message::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find messages by session")
    }

    /// Find the most recent `limit` messages for a session, oldest-first.
    ///
    /// Unlike [`find_by_session`], this pushes the LIMIT into SQL so reading
    /// the tail of a huge session costs `limit` rows instead of the whole
    /// history. The DESC scan is reversed in Rust to restore chronological
    /// order.
    pub async fn find_recent_by_session(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let sid = session_id.to_string();
        let limit = limit as i64;
        let mut messages = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM messages WHERE session_id = ?1 \
                     ORDER BY sequence DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![sid, limit], Message::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find recent messages by session")?;
        messages.reverse();
        Ok(messages)
    }

    /// Find up to `limit` messages strictly older than `before_sequence`,
    /// oldest-first. The PageUp counterpart of [`find_recent_by_session`]:
    /// paging back costs `limit` rows per step, never the whole history.
    pub async fn find_recent_before_sequence(
        &self,
        session_id: Uuid,
        before_sequence: i32,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let sid = session_id.to_string();
        let limit = limit as i64;
        let mut messages = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM messages WHERE session_id = ?1 AND sequence < ?2 \
                     ORDER BY sequence DESC LIMIT ?3",
                )?;
                let rows =
                    stmt.query_map(params![sid, before_sequence, limit], Message::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find messages before sequence")?;
        messages.reverse();
        Ok(messages)
    }

    /// Create a new message
    pub async fn create(&self, message: &Message) -> Result<()> {
        let m = message.clone();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO messages (id, session_id, role, content, sequence,
                                         created_at, token_count, cost, input_tokens,
                                         cache_creation_tokens, cache_read_tokens, thinking,
                                         duration_secs)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        m.id.to_string(),
                        m.session_id.to_string(),
                        m.role,
                        m.content,
                        m.sequence,
                        m.created_at.timestamp(),
                        m.token_count,
                        m.cost,
                        m.input_tokens,
                        m.cache_creation_tokens,
                        m.cache_read_tokens,
                        m.thinking,
                        m.duration_secs,
                    ],
                )?;

                // Touch session's updated_at so /sessions sorts by last interaction
                conn.execute(
                    "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                    params![m.created_at.timestamp(), m.session_id.to_string()],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to create message")?;

        tracing::debug!(
            "Created message: {} in session: {}",
            message.id,
            message.session_id
        );
        Ok(())
    }

    /// Update an existing message
    pub async fn update(&self, message: &Message) -> Result<()> {
        let m = message.clone();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE messages
                     SET content = ?1, token_count = ?2, cost = ?3, input_tokens = ?4,
                         cache_creation_tokens = ?5, cache_read_tokens = ?6, thinking = ?7,
                         duration_secs = ?8
                     WHERE id = ?9",
                    params![
                        m.content,
                        m.token_count,
                        m.cost,
                        m.input_tokens,
                        m.cache_creation_tokens,
                        m.cache_read_tokens,
                        m.thinking,
                        m.duration_secs,
                        m.id.to_string()
                    ],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to update message")?;

        tracing::debug!("Updated message: {}", message.id);
        Ok(())
    }

    /// Fetch the server-reported `input_tokens` from the most recent
    /// assistant message in a session that has one set. Returns `None`
    /// when the session has no assistant responses yet (first turn, or
    /// a fresh session where every assistant row is still streaming).
    /// Used on session load as the authoritative "last known context
    /// size" — no need to tokenize raw content to guess.
    pub async fn last_assistant_input_tokens(&self, session_id: Uuid) -> Result<Option<i32>> {
        let sid = session_id.to_string();
        let tokens = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT input_tokens FROM messages \
                     WHERE session_id = ?1 AND role = 'assistant' \
                     AND input_tokens IS NOT NULL \
                     ORDER BY sequence DESC LIMIT 1",
                    params![sid],
                    |r| r.get::<_, Option<i32>>(0),
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to query last assistant input_tokens")?;
        Ok(tokens.flatten())
    }

    /// Append content to an existing message (for real-time history persistence)
    pub async fn append_content(&self, id: Uuid, content_to_append: &str) -> Result<()> {
        let id_str = id.to_string();
        let content = content_to_append.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE messages SET content = content || ?1 WHERE id = ?2",
                    params![content, id_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to append to message")?;

        tracing::debug!("Appended content to message: {}", id);
        Ok(())
    }

    /// Set the thinking/reasoning content on an existing message.
    /// Called once per iteration for non-CLI providers to persist reasoning
    /// that was collected during streaming into the `thinking` column.
    pub async fn set_thinking(&self, id: Uuid, thinking: &str) -> Result<()> {
        let id_str = id.to_string();
        let thinking_val = thinking.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE messages SET thinking = ?1 WHERE id = ?2",
                    params![thinking_val, id_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to set thinking on message")?;

        tracing::debug!("Set thinking ({} chars) on message: {}", thinking.len(), id);
        Ok(())
    }

    /// Append to existing thinking content (accumulates reasoning across
    /// multiple tool-loop iterations for non-CLI providers).
    pub async fn append_thinking(&self, id: Uuid, thinking_to_append: &str) -> Result<()> {
        let id_str = id.to_string();
        let thinking = thinking_to_append.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                // If thinking is NULL, just set it; otherwise concatenate with separator
                conn.execute(
                    "UPDATE messages SET thinking = CASE \
                     WHEN thinking IS NULL THEN ?1 \
                     ELSE thinking || char(10) || char(10) || ?1 \
                     END \
                     WHERE id = ?2",
                    params![thinking, id_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to append thinking to message")?;

        tracing::debug!(
            "Appended thinking ({} chars) to message: {}",
            thinking_to_append.len(),
            id
        );
        Ok(())
    }

    /// Delete a message
    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute("DELETE FROM messages WHERE id = ?1", params![id_str])
            })
            .await
            .map_err(interact_err)?
            .context("Failed to delete message")?;

        tracing::debug!("Deleted message: {}", id);
        Ok(())
    }

    /// List all messages for a session
    pub async fn list_by_session(&self, session_id: Uuid) -> Result<Vec<Message>> {
        self.find_by_session(session_id).await
    }

    /// Remove a trailing unanswered user query and its empty assistant
    /// placeholder as a pair (#730).
    ///
    /// A turn persists the user row and an empty assistant placeholder up front,
    /// then streams content into the placeholder. If the turn is cancelled
    /// before the agent produces anything, the placeholder stays empty and the
    /// user query is left unanswered — and because the empty placeholder is now
    /// the trailing row, a naive "delete the last user row" cleanup misses it,
    /// so the query survives into the next turn's LLM context and duplicates on
    /// resend. This drops the pair from the tail so nothing lingers.
    ///
    /// Only the freshly-created trailing rows are touched: the last row is
    /// deleted iff it is an empty/whitespace-only assistant placeholder, and the
    /// row before it iff it is a user message. A trailing assistant row that
    /// actually holds a reply is left untouched, so a real answered turn is
    /// never disturbed. Returns the number of rows deleted (0, 1, or 2).
    pub async fn delete_trailing_unanswered_pair(&self, session_id: Uuid) -> Result<usize> {
        let mut deleted = 0;

        // Drop the empty assistant placeholder if it is the trailing row.
        if let Some(last) = self.get_last_message(session_id).await?
            && last.role == "assistant"
            && last.content.trim().is_empty()
        {
            self.delete(last.id).await?;
            deleted += 1;
        }

        // Now drop the unanswered user query if it is the (new) trailing row.
        if let Some(last) = self.get_last_message(session_id).await?
            && last.role == "user"
        {
            self.delete(last.id).await?;
            deleted += 1;
        }

        Ok(deleted)
    }

    /// Search messages by text content using SQL LIKE.
    /// `session_ids = None` searches all sessions; `Some(&[..])` restricts to the listed ids.
    /// Returns up to `limit` matches, most recent first. Case-insensitive.
    pub async fn search_by_content(
        &self,
        session_ids: Option<&[Uuid]>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let limit_i = limit as i64;
        let id_strs: Option<Vec<String>> =
            session_ids.map(|ids| ids.iter().map(|i| i.to_string()).collect());

        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                if let Some(ids) = id_strs {
                    if ids.is_empty() {
                        return Ok::<Vec<Message>, rusqlite::Error>(Vec::new());
                    }
                    let placeholders = vec!["?"; ids.len()].join(",");
                    let sql = format!(
                        "SELECT * FROM messages \
                         WHERE session_id IN ({}) AND content LIKE ? ESCAPE '\\' \
                         COLLATE NOCASE \
                         ORDER BY created_at DESC LIMIT ?",
                        placeholders
                    );
                    let mut stmt = conn.prepare(&sql)?;
                    let mut params_vec: Vec<&dyn rusqlite::ToSql> =
                        Vec::with_capacity(ids.len() + 2);
                    for id in &ids {
                        params_vec.push(id);
                    }
                    params_vec.push(&pattern);
                    params_vec.push(&limit_i);
                    let rows = stmt.query_map(params_vec.as_slice(), Message::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                } else {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM messages \
                         WHERE content LIKE ?1 ESCAPE '\\' COLLATE NOCASE \
                         ORDER BY created_at DESC LIMIT ?2",
                    )?;
                    let rows = stmt.query_map(params![pattern, limit_i], Message::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
            })
            .await
            .map_err(interact_err)?
            .context("Failed to search messages by content")
    }

    /// Tokenized companion to `search_by_content` (#1626): every
    /// whitespace-separated token must appear somewhere in the content (AND
    /// of substring matches) instead of one contiguous substring. Fetches a
    /// recency-ordered pool (5x the limit, capped at 200), then re-ranks in
    /// Rust by total token hit count descending; a stable sort keeps SQL
    /// recency order inside every tie.
    pub async fn search_by_content_tokenized(
        &self,
        session_ids: Option<&[Uuid]>,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Message>> {
        // Dedup exact repeats; cap the arms so a pasted paragraph of a
        // query cannot build a 50-LIKE WHERE clause — 8 tokens already
        // pins hard.
        let mut tokens: Vec<String> = query.split_whitespace().map(str::to_string).collect();
        tokens.sort();
        tokens.dedup();
        tokens.truncate(8);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }

        let fetch_cap = (limit.saturating_mul(5)).clamp(1, 200) as i64;
        let patterns: Vec<String> = tokens
            .iter()
            .map(|t| format!("%{}%", t.replace('%', "\\%").replace('_', "\\_")))
            .collect();
        let like_arms =
            vec!["content LIKE ? ESCAPE '\\' COLLATE NOCASE"; patterns.len()].join(" AND ");
        let id_strs: Option<Vec<String>> =
            session_ids.map(|ids| ids.iter().map(|i| i.to_string()).collect());

        let rows = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::new();
                if let Some(ids) = &id_strs {
                    if ids.is_empty() {
                        return Ok::<Vec<Message>, rusqlite::Error>(Vec::new());
                    }
                    let placeholders = vec!["?"; ids.len()].join(",");
                    let sql = format!(
                        "SELECT * FROM messages \
                         WHERE session_id IN ({}) AND {like_arms} \
                         ORDER BY created_at DESC LIMIT ?",
                        placeholders
                    );
                    let mut stmt = conn.prepare(&sql)?;
                    for id in ids {
                        params_vec.push(id);
                    }
                    for p in &patterns {
                        params_vec.push(p);
                    }
                    params_vec.push(&fetch_cap);
                    let rows = stmt.query_map(params_vec.as_slice(), Message::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                } else {
                    let sql = format!(
                        "SELECT * FROM messages \
                         WHERE {like_arms} \
                         ORDER BY created_at DESC LIMIT ?"
                    );
                    let mut stmt = conn.prepare(&sql)?;
                    for p in &patterns {
                        params_vec.push(p);
                    }
                    params_vec.push(&fetch_cap);
                    let rows = stmt.query_map(params_vec.as_slice(), Message::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
            })
            .await
            .map_err(interact_err)?
            .context("Failed to search messages by content (tokenized)")?;

        // Precompute hit scores once (total occurrences of every token,
        // case-insensitive), then one stable sort by score descending.
        let lower_tokens: Vec<String> = tokens.iter().map(|t| t.to_lowercase()).collect();
        let mut scored: Vec<(usize, Message)> = rows
            .into_iter()
            .map(|m| {
                let lower = m.content.to_lowercase();
                let hits: usize = lower_tokens
                    .iter()
                    .map(|t| lower.matches(t.as_str()).count())
                    .sum();
                (hits, m)
            })
            .collect();
        scored.sort_by_key(|(hits, _)| std::cmp::Reverse(*hits));
        Ok(scored.into_iter().map(|(_, m)| m).take(limit).collect())
    }

    /// Count messages in a session
    pub async fn count_by_session(&self, session_id: Uuid) -> Result<i64> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                    params![sid],
                    |row| row.get(0),
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to count messages")
    }

    /// Count all messages across all sessions
    /// Get the last message in a session
    pub async fn get_last_message(&self, session_id: Uuid) -> Result<Option<Message>> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare_cached(
                    "SELECT * FROM messages WHERE session_id = ?1 ORDER BY sequence DESC LIMIT 1",
                )?
                .query_row(params![sid], Message::from_row)
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to get last message")
    }

    /// Get the most recent assistant message in a session.
    ///
    /// Used to recover reply context in Telegram DMs: bot messages live in
    /// this `messages` table (not `channel_messages`), and rich messages
    /// (Bot API 10.1) arrive with empty `text()`/`caption()`, so the
    /// replied-to text must be recovered from here. The incoming user
    /// message is not yet persisted at recovery time, so the last assistant
    /// row is the message the user is replying to in the common case.
    pub async fn get_last_assistant_message(&self, session_id: Uuid) -> Result<Option<Message>> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare_cached(
                    "SELECT * FROM messages WHERE session_id = ?1 AND role = 'assistant' \
                     ORDER BY sequence DESC LIMIT 1",
                )?
                .query_row(params![sid], Message::from_row)
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to get last assistant message")
    }

    /// Delete all messages in a session
    pub async fn delete_by_session(&self, session_id: Uuid) -> Result<()> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute("DELETE FROM messages WHERE session_id = ?1", params![sid])
            })
            .await
            .map_err(interact_err)?
            .context("Failed to delete session messages")?;

        tracing::debug!("Deleted all messages for session: {}", session_id);
        Ok(())
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
