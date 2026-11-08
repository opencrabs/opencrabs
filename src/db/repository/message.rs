//! Message Repository
//!
//! Database operations for messages.

use crate::db::Pool;
use crate::db::database::interact_err;
use crate::db::models::Message;
use anyhow::{Context, Result};
use rusqlite::params;
use rusqlite::params_from_iter;
use std::collections::HashMap;
use uuid::Uuid;

/// How much of a message fits on a rail card's preview line.
///
/// A character budget, never a byte one: truncating by bytes panics on
/// multibyte content (#2271).
pub const PREVIEW_CHARS: usize = 60;

/// One session's latest message, flattened and cut for the rail card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePreview {
    /// The row's role: `user`, `assistant`, or whatever a tool turn wrote.
    pub role: String,
    /// The content with every run of whitespace collapsed to one space.
    pub text: String,
    /// True when the stored content was longer than [`PREVIEW_CHARS`].
    pub truncated: bool,
}

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

    /// Recent conversation rows for tools that must not expose plan history.
    /// Filtering in SQL preserves the requested tail size.
    pub async fn find_recent_without_completed_plans(
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
                     AND role != 'plan_completed' \
                     ORDER BY sequence DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![sid, limit], Message::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find recent conversation messages")?;
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
                insert_message(conn, &m)?;

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

    /// Copy rows from one session into another as a single forward batch
    /// (#2608).
    ///
    /// The copy is written contiguously at the end of the destination's
    /// sequence so the forwarded rows read as one block, and every row shares
    /// one batch id so the renderer draws one header over them. Nothing here
    /// triggers a turn: a forward delivers, it does not prompt.
    ///
    /// Refused before any write when the selection is empty, when its rows do
    /// not all come from one session, or when the destination is that same
    /// session. A forward into the source would duplicate rows the reader
    /// already has in front of them.
    pub async fn forward(
        &self,
        source: &[Message],
        target: Uuid,
        label: &str,
    ) -> Result<Vec<Uuid>> {
        let Some(first) = source.first() else {
            anyhow::bail!("forward: nothing selected");
        };
        let from = first.session_id;
        if source.iter().any(|m| m.session_id != from) {
            anyhow::bail!("forward: the selection spans more than one session");
        }
        if from == target {
            anyhow::bail!("forward: the destination is the source session");
        }

        let source = source.to_vec();
        let label = label.to_string();
        let batch = Uuid::new_v4();
        let target_key = target.to_string();

        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let tx = conn.transaction()?;
                // The destination's own tail, so the block lands after
                // whatever the reader already had.
                let tail: i32 = tx
                    .prepare_cached(
                        "SELECT COALESCE(MAX(sequence), -1) FROM messages WHERE session_id = ?1",
                    )?
                    .query_row(params![target_key], |r| r.get(0))?;
                let rows =
                    crate::db::models::forwarded_rows(&source, target, tail + 1, &label, batch);
                let mut ids = Vec::with_capacity(rows.len());
                for row in &rows {
                    insert_message(&tx, row)?;
                    ids.push(row.id);
                }
                // One touch, not one per row: the session sorts by recency and
                // a batch arrives at a single moment.
                tx.execute(
                    "UPDATE sessions SET updated_at = strftime('%s', 'now') WHERE id = ?1",
                    params![target_key],
                )?;
                tx.commit()?;
                Ok::<Vec<Uuid>, rusqlite::Error>(ids)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to forward messages")
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
                         WHERE session_id IN ({}) AND role != 'plan_completed' \
                         AND content LIKE ? ESCAPE '\\' COLLATE NOCASE \
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
                         WHERE role != 'plan_completed' \
                         AND content LIKE ?1 ESCAPE '\\' COLLATE NOCASE \
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
                         WHERE session_id IN ({}) AND role != 'plan_completed' \
                         AND {like_arms} \
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
                         WHERE role != 'plan_completed' AND {like_arms} \
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

    /// The latest message of every session in `ids`, in one round trip.
    ///
    /// The rail paints a preview on every card and repaints on a shared clock,
    /// so asking [`Self::get_last_message`] once per card is a query per card
    /// per frame. This asks for the whole visible set at once and is meant to
    /// be called on a rail reload, never from a paint.
    ///
    /// Ordering is by `sequence`, not `created_at`: an assistant row is
    /// created at turn start and updated in place, so its timestamp matches
    /// the triggering user row and cannot order the transcript. `sequence` is
    /// also what `get_last_message` uses, so the preview and the transcript
    /// agree on which row is last.
    ///
    /// Only `user` and `assistant` rows are conversation context. Plan
    /// events (`plan_completed`), tool results and system rows are ledger
    /// noise: ranking them would dump raw JSON like
    /// `plan_completed {"version":1,...}` onto the card, so the window
    /// filters them out and the preview is the newest thing the user or the
    /// crab actually said.
    ///
    /// Ids are chunked well under SQLite's bound-parameter limit, so a long
    /// rail still resolves in a bounded number of statements.
    pub async fn last_messages_for(&self, ids: &[Uuid]) -> Result<HashMap<Uuid, MessagePreview>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        // 999 is the oldest SQLITE_MAX_VARIABLE_NUMBER; staying under it means
        // the chunk size is valid on every SQLite we might be linked against.
        const CHUNK: usize = 500;
        let mut previews = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(CHUNK) {
            let keys: Vec<String> = chunk.iter().map(Uuid::to_string).collect();
            let placeholders = vec!["?"; keys.len()].join(",");
            let sql = format!(
                "SELECT session_id, role, content FROM ( \
                    SELECT session_id, role, content, \
                           ROW_NUMBER() OVER ( \
                               PARTITION BY session_id ORDER BY sequence DESC \
                           ) AS rn \
                    FROM messages WHERE session_id IN ({placeholders}) \
                        AND role IN ('user', 'assistant') \
                ) WHERE rn = 1"
            );
            let rows: Vec<(String, String, String)> = self
                .pool
                .get()
                .await
                .context("Failed to get connection")?
                .interact(move |conn| {
                    let mut stmt = conn.prepare_cached(&sql)?;
                    let mapped = stmt.query_map(params_from_iter(keys.iter()), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })?;
                    let mut collected = Vec::new();
                    for row in mapped {
                        collected.push(row?);
                    }
                    // The error type is named, not inferred: several crates
                    // implement From<rusqlite::Error>, so a bare `Ok` here
                    // leaves `?` ambiguous (E0283).
                    Ok::<_, anyhow::Error>(collected)
                })
                .await
                .map_err(interact_err)?
                .context("Failed to read last messages")?;

            for (session_id, role, content) in rows {
                // A row whose id will not parse is not addressable by the
                // rail, so skipping it loses nothing a card could show.
                let Ok(id) = Uuid::parse_str(&session_id) else {
                    continue;
                };
                let (text, truncated) = preview_text(&content, PREVIEW_CHARS);
                // A last message that is only reasoning (mid-turn) cleans to
                // empty; "Crabs: " with nothing after it is noise, so the
                // card simply gets no message line. The timestamp and cost
                // ride the title line either way.
                if text.is_empty() {
                    continue;
                }
                previews.insert(
                    id,
                    MessagePreview {
                        role,
                        text,
                        truncated,
                    },
                );
            }
        }
        Ok(previews)
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

/// Flatten a message to one preview line and cut it to `max` characters.
///
/// Newlines become single spaces so a multi-line answer cannot open a hole in
/// the rail, and the cut walks characters rather than bytes: a byte slice on
/// multibyte content panics (#2271).
fn preview_text(content: &str, max: usize) -> (String, bool) {
    // The rail shows the answer, not the model's deliberation. CLI providers
    // persist reasoning inline as `<!-- reasoning -->` blocks (and some leak
    // think/tool tags), so run the same delivery-side sanitizer the TUI and
    // channels use before flattening; otherwise the raw markers and the
    // thinking prose land in the one-line card preview.
    let cleaned = crate::utils::sanitize::strip_llm_artifacts(content);
    let flattened = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= max {
        return (flattened, false);
    }
    let mut cut: String = flattened.chars().take(max).collect();
    cut.push('\u{2026}');
    (cut, true)
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

/// Write one row, provenance columns included. Shared by [`MessageRepository::create`]
/// and [`MessageRepository::forward`] so the column list has exactly one
/// definition and a new column cannot land in one path but not the other.
fn insert_message(conn: &rusqlite::Connection, m: &Message) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO messages (id, session_id, role, content, sequence,
                             created_at, token_count, cost, input_tokens,
                             cache_creation_tokens, cache_read_tokens, thinking,
                             duration_secs, forwarded_from_session,
                             forwarded_from_message, forwarded_from_label,
                             forward_batch_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
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
            m.forward.as_ref().map(|f| f.from_session.to_string()),
            m.forward.as_ref().map(|f| f.from_message.to_string()),
            m.forward.as_ref().map(|f| f.label.clone()),
            m.forward.as_ref().map(|f| f.batch.to_string()),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::{Message, MessageRepository, PREVIEW_CHARS, preview_text};
    use crate::db::Database;
    use crate::db::models::Session;
    use crate::db::repository::SessionRepository;
    use uuid::Uuid;

    async fn setup() -> (Database, SessionRepository, MessageRepository) {
        let db = Database::connect_in_memory().await.expect("in-memory db");
        db.run_migrations().await.expect("migrations");
        let sessions = SessionRepository::new(db.pool().clone());
        let messages = MessageRepository::new(db.pool().clone());
        (db, sessions, messages)
    }

    async fn session(sessions: &SessionRepository, title: &str) -> Uuid {
        let s = Session::new(Some(title.to_string()), None, None);
        let id = s.id;
        sessions.create(&s).await.expect("create session");
        id
    }

    async fn say(messages: &MessageRepository, sid: Uuid, role: &str, content: &str, seq: i32) {
        let m = Message::new(sid, role.to_string(), content.to_string(), seq);
        messages.create(&m).await.expect("create message");
    }

    /// One row per session, the newest by sequence, even when the id list
    /// spans the 500-parameter chunk boundary: real sessions sit in both
    /// chunks with filler ids between them.
    #[tokio::test]
    async fn last_messages_for_newest_per_session_across_chunk_boundary() {
        let (_db, sessions, messages) = setup().await;
        let a = session(&sessions, "A").await;
        let b = session(&sessions, "B").await;
        let c = session(&sessions, "C").await;
        say(&messages, a, "user", "old A", 1).await;
        say(&messages, a, "assistant", "new A", 2).await;
        say(&messages, b, "user", "only B", 1).await;
        say(&messages, c, "assistant", "old C", 1).await;
        say(&messages, c, "assistant", "newer C", 2).await;
        say(&messages, c, "user", "newest C", 3).await;

        // A and B land in the first 500-id chunk, C in the second; the rest
        // are ids with no messages, so only the three real sessions resolve.
        let mut ids = vec![a, b];
        ids.extend((0..498).map(|_| Uuid::new_v4()));
        ids.push(c);
        assert_eq!(ids.len(), 501);

        let previews = messages.last_messages_for(&ids).await.expect("batch read");
        assert_eq!(previews.len(), 3, "filler ids resolve to nothing");
        assert_eq!(previews[&a].text, "new A");
        assert_eq!(previews[&a].role, "assistant");
        assert_eq!(previews[&b].text, "only B");
        assert_eq!(previews[&b].role, "user");
        assert_eq!(previews[&c].text, "newest C");
        assert_eq!(previews[&c].role, "user");
    }

    /// Plan events and tool rows are ledger noise, not conversation: the
    /// preview walks back to the newest user/assistant row instead of
    /// dumping `plan_completed {"version":1,...}` JSON onto the card.
    #[tokio::test]
    async fn last_messages_for_skips_non_conversation_roles() {
        let (_db, sessions, messages) = setup().await;
        let d = session(&sessions, "D").await;
        say(&messages, d, "user", "ship it", 1).await;
        say(&messages, d, "assistant", "on it", 2).await;
        say(
            &messages,
            d,
            "plan_completed",
            "{\"version\":1,\"plan\":{\"id\":\"fc3\"}}",
            3,
        )
        .await;
        say(&messages, d, "tool", "{\"ok\":true}", 4).await;
        say(&messages, d, "system", "event noise", 5).await;

        let previews = messages.last_messages_for(&[d]).await.expect("batch read");
        assert_eq!(previews[&d].text, "on it");
        assert_eq!(previews[&d].role, "assistant");

        // A session whose only rows are events gets no preview at all.
        let e = session(&sessions, "E").await;
        say(&messages, e, "plan_completed", "{\"version\":1}", 1).await;
        let previews = messages.last_messages_for(&[e]).await.expect("batch read");
        assert!(!previews.contains_key(&e), "event-only session stays quiet");
    }

    /// Content longer than the budget is cut on a character boundary and
    /// flagged truncated, never sliced through a multibyte byte.
    #[tokio::test]
    async fn last_messages_for_truncates_long_content() {
        let (_db, sessions, messages) = setup().await;
        let d = session(&sessions, "D").await;
        let long = "é".repeat(PREVIEW_CHARS + 20);
        say(&messages, d, "assistant", &long, 1).await;
        let previews = messages.last_messages_for(&[d]).await.expect("batch read");
        let p = &previews[&d];
        assert!(p.truncated);
        assert_eq!(p.text.chars().count(), PREVIEW_CHARS + 1);
        assert!(p.text.ends_with('\u{2026}'));
    }

    /// A last message that is only reasoning cleans to empty, so the card
    /// gets no preview rather than a bare "Crabs: " with nothing after it.
    #[tokio::test]
    async fn last_messages_for_skips_reasoning_only_message() {
        let (_db, sessions, messages) = setup().await;
        let e = session(&sessions, "E").await;
        say(
            &messages,
            e,
            "assistant",
            "<!-- reasoning -->only thinking, no answer yet<!-- /reasoning -->",
            1,
        )
        .await;
        let previews = messages.last_messages_for(&[e]).await.expect("batch read");
        assert!(
            previews.is_empty(),
            "reasoning-only last message stays absent"
        );
    }

    /// The flattener collapses every whitespace run to one space and leaves
    /// short content untruncated.
    #[test]
    fn preview_text_flattens_whitespace() {
        let (text, truncated) = preview_text("a\n  b\tc   d", PREVIEW_CHARS);
        assert_eq!(text, "a b c d");
        assert!(!truncated);
    }
}
