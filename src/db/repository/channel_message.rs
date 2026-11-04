//! Channel Message Repository
//!
//! Database operations for passively captured channel messages.

use crate::db::Pool;
use crate::db::database::interact_err;
use crate::db::models::ChannelMessage;
use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

/// Summary of a known chat
pub struct ChatSummary {
    pub channel: String,
    pub channel_chat_id: String,
    pub channel_chat_name: Option<String>,
    pub message_count: i64,
    pub last_message_at: i64,
}

/// Repository for channel message operations
#[derive(Clone)]
pub struct ChannelMessageRepository {
    pool: Pool,
}

impl ChannelMessageRepository {
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Insert a single channel message
    pub async fn insert(&self, msg: &ChannelMessage) -> Result<()> {
        let m = msg.clone();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO channel_messages
                        (id, channel, channel_chat_id, channel_chat_name,
                         sender_id, sender_name, content, message_type,
                         platform_message_id, created_at, thread_id, topic_name)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        m.id.to_string(),
                        m.channel,
                        m.channel_chat_id,
                        m.channel_chat_name,
                        m.sender_id,
                        m.sender_name,
                        m.content,
                        m.message_type,
                        m.platform_message_id,
                        m.created_at.timestamp(),
                        m.thread_id,
                        m.topic_name,
                    ],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to insert channel message")?;

        Ok(())
    }

    /// Rewrite a stored message's content, matched by platform message id.
    ///
    /// Reconciles streamed/edited messages from peer bots: a bot that
    /// streams its reply via Telegram message edits arrives as one initial
    /// frame plus a run of `edited_message` updates. When an edit lands
    /// after we already captured the message, this rewrites the row so
    /// group history reflects the FINAL text rather than the first frame.
    /// Returns the number of rows updated (0 if the message wasn't stored).
    pub async fn update_content(
        &self,
        channel: &str,
        chat_id: &str,
        platform_message_id: &str,
        content: &str,
    ) -> Result<usize> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        let pmid = platform_message_id.to_string();
        let body = content.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE channel_messages SET content = ?4 \
                     WHERE channel = ?1 AND channel_chat_id = ?2 \
                       AND platform_message_id = ?3",
                    params![ch, cid, pmid, body],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to update channel message content")
    }

    /// Drop the stored row for one platform message id.
    ///
    /// Used when a bubble is deleted on Telegram: the plan card (#1684) and any
    /// other bot message the bot removes. Without it, `channel_messages` keeps
    /// a row for a message that no longer exists, and history or a reply lookup
    /// can quote a dead bubble back to the agent.
    pub async fn delete_by_platform_message_id(
        &self,
        channel: &str,
        chat_id: &str,
        platform_message_id: &str,
    ) -> Result<usize> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        let pmid = platform_message_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "DELETE FROM channel_messages \
                     WHERE channel = ?1 AND channel_chat_id = ?2 \
                       AND platform_message_id = ?3",
                    params![ch, cid, pmid],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to delete channel message by platform id")
    }

    /// Forum-topic dead-key eviction (#116): Telegram answered
    /// `400 Bad Request: message thread not found` for `(channel, chat,
    /// thread)` — the remembered topic no longer exists. Clear the
    /// thread_id on every row of that chat carrying it, so the
    /// auto-lookup (`latest_thread_id_for_chat`) and the topic listing
    /// stop serving the dead address. Chat-scoped by design: a deleted
    /// topic in one chat must never touch another chat's rows.
    pub async fn clear_thread_for_chat(
        &self,
        channel: &str,
        chat_id: &str,
        thread_id: &str,
    ) -> Result<u64> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        let tid = thread_id.to_string();
        let updated = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE channel_messages SET thread_id = NULL \
                     WHERE channel = ?1 AND channel_chat_id = ?2 AND thread_id = ?3",
                    params![ch, cid, tid],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to clear dead forum topic thread_id")?;
        Ok(updated as u64)
    }

    /// #1655: General-topic history fetch. General rows persist with
    /// `thread_id = NULL` (General messages carry no `message_thread_id`),
    /// so "this topic only" for General means `thread_id IS NULL` — a state
    /// `recent()`'s `Option<&str>` cannot express, because `None` there means
    /// "no thread filter" and returns EVERY topic's rows. Without this, a
    /// General prompt in a forum pulled cross-topic history and the agent
    /// continued other topics' work in General.
    pub async fn recent_general(
        &self,
        channel: &str,
        chat_id: &str,
        limit: i64,
        message_type: Option<&str>,
    ) -> Result<Vec<ChannelMessage>> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        let mtype = message_type.map(|s| s.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| match mtype {
                Some(mtype) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel = ?1 AND channel_chat_id = ?2 \
                             AND thread_id IS NULL AND message_type = ?3 \
                             ORDER BY created_at DESC LIMIT ?4",
                    )?;
                    let rows =
                        stmt.query_map(params![ch, cid, mtype, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                None => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel = ?1 AND channel_chat_id = ?2 \
                             AND thread_id IS NULL \
                             ORDER BY created_at DESC LIMIT ?3",
                    )?;
                    let rows = stmt.query_map(params![ch, cid, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
            })
            .await
            .map_err(interact_err)?
            .context("Failed to fetch recent general-topic messages")
    }

    /// #719: General-topic content search — `recent_general()`'s sibling for
    /// the `search` operation. General rows persist with `thread_id IS NULL`
    /// (General messages carry no `message_thread_id`), so "search THIS topic"
    /// for General means `thread_id IS NULL`. `search()`'s `Option<&str>`
    /// cannot express that state: `None` there means "no thread filter" and
    /// matches every topic's rows, so a General-bound session searching its own
    /// history got the whole forum back.
    pub async fn search_general(
        &self,
        channel: &str,
        chat_id: &str,
        query: &str,
        limit: i64,
    ) -> Result<Vec<ChannelMessage>> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        let pattern = format!("%{query}%");
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM channel_messages \
                         WHERE channel = ?1 AND channel_chat_id = ?2 \
                         AND content LIKE ?3 AND thread_id IS NULL \
                         ORDER BY created_at DESC LIMIT ?4",
                )?;
                let rows =
                    stmt.query_map(params![ch, cid, pattern, limit], ChannelMessage::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to search general-topic messages")
    }

    /// Get recent messages for a specific chat, optionally filtered by thread_id.
    /// When `thread_id` is Some, only messages belonging to that forum topic are returned.
    pub async fn recent(
        &self,
        channel: Option<&str>,
        chat_id: &str,
        limit: i64,
        thread_id: Option<&str>,
        message_type: Option<&str>,
    ) -> Result<Vec<ChannelMessage>> {
        let ch = channel.map(|s| s.to_string());
        let cid = chat_id.to_string();
        let tid = thread_id.map(|s| s.to_string());
        let mtype = message_type.map(|s| s.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| match (ch, tid, mtype) {
                (Some(ch), Some(tid), Some(mtype)) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel = ?1 AND channel_chat_id = ?2 AND thread_id = ?3 AND message_type = ?4 \
                             ORDER BY created_at DESC LIMIT ?5",
                    )?;
                    let rows =
                        stmt.query_map(params![ch, cid, tid, mtype, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (Some(ch), Some(tid), None) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel = ?1 AND channel_chat_id = ?2 AND thread_id = ?3 \
                             ORDER BY created_at DESC LIMIT ?4",
                    )?;
                    let rows =
                        stmt.query_map(params![ch, cid, tid, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (Some(ch), None, Some(mtype)) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel = ?1 AND channel_chat_id = ?2 AND message_type = ?3 \
                             ORDER BY created_at DESC LIMIT ?4",
                    )?;
                    let rows = stmt.query_map(params![ch, cid, mtype, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (Some(ch), None, None) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel = ?1 AND channel_chat_id = ?2 \
                             ORDER BY created_at DESC LIMIT ?3",
                    )?;
                    let rows = stmt.query_map(params![ch, cid, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (None, Some(tid), Some(mtype)) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel_chat_id = ?1 AND thread_id = ?2 AND message_type = ?3 \
                             ORDER BY created_at DESC LIMIT ?4",
                    )?;
                    let rows =
                        stmt.query_map(params![cid, tid, mtype, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (None, Some(tid), None) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel_chat_id = ?1 AND thread_id = ?2 \
                             ORDER BY created_at DESC LIMIT ?3",
                    )?;
                    let rows =
                        stmt.query_map(params![cid, tid, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (None, None, Some(mtype)) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel_chat_id = ?1 AND message_type = ?2 \
                             ORDER BY created_at DESC LIMIT ?3",
                    )?;
                    let rows = stmt.query_map(params![cid, mtype, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (None, None, None) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel_chat_id = ?1 \
                             ORDER BY created_at DESC LIMIT ?2",
                    )?;
                    let rows = stmt.query_map(params![cid, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
            })
            .await
            .map_err(interact_err)?
            .context("Failed to fetch recent channel messages")
    }

    /// Look up the stored content of a single message by its platform message id
    /// (the Telegram `message_id`). Used to recover the EXACT message a user
    /// replied to — Telegram delivers rich bot messages with empty text, so the
    /// reply handler can't read the quoted content from the update and must look
    /// it up by id instead of guessing "the most recent bot message". Returns
    /// the most recent match (ids are unique per chat, but a defensive ORDER BY
    /// keeps this deterministic). `None` when nothing was stored under that id.
    /// The chat's earliest stored row that carries a platform message id —
    /// the anchor for a bounded PDO history request (#1525). Returns
    /// (platform_message_id, sender_id, created_at).
    pub async fn oldest_for_chat(
        &self,
        channel: &str,
        chat_id: &str,
    ) -> Result<Option<(String, String, chrono::DateTime<chrono::Utc>)>> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        let found: Option<(String, String, i64)> = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT platform_message_id, sender_id, created_at FROM channel_messages \
                     WHERE channel = ?1 AND channel_chat_id = ?2 \
                       AND platform_message_id IS NOT NULL \
                     ORDER BY created_at ASC LIMIT 1",
                    params![ch, cid],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to look up channel message anchor")?;
        Ok(found.map(|(pid, sid, ts)| {
            (
                pid,
                sid,
                chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default(),
            )
        }))
    }

    /// Read-only bounded history search for one chat (#1525): newest-first
    /// substring rows, within a caller-supplied epoch-seconds window.
    pub async fn search_history(
        &self,
        channel: &str,
        chat_id: &str,
        query: &str,
        since_ts: i64,
        limit: i64,
    ) -> Result<Vec<(String, String, i64)>> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        // Escape LIKE wildcards and the escape character itself: a search
        // for "100%" must match a literal percent, not a pattern.
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT content, sender_name, created_at FROM channel_messages \
                     WHERE channel = ?1 AND channel_chat_id = ?2 AND created_at >= ?3 \
                       AND content LIKE ?4 ESCAPE '\\' \
                     ORDER BY created_at DESC LIMIT ?5",
                )?;
                let rows = stmt
                    .query_map(params![ch, cid, since_ts, pattern, limit], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok::<_, rusqlite::Error>(rows)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to search channel message history")
    }

    /// #1529: per-newsletter poll cursor — (last_server_id, last_ts). Absent
    /// row means "never polled": the poller baselines silently rather than
    /// replaying history at whoever flipped the config on.
    pub async fn newsletter_cursor(&self, wa_jid: &str) -> Result<Option<(i64, i64)>> {
        let j = wa_jid.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT last_server_id, last_ts FROM whatsapp_newsletter_cursors \
                     WHERE wa_jid = ?1",
                    params![j],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read newsletter cursor")
    }

    /// Advance (or baseline) the poll cursor. Upsert: first sight creates the
    /// row, later ticks overwrite it.
    pub async fn set_newsletter_cursor(
        &self,
        wa_jid: &str,
        last_server_id: i64,
        last_ts: i64,
    ) -> Result<()> {
        let j = wa_jid.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO whatsapp_newsletter_cursors \
                     (wa_jid, last_server_id, last_ts, updated_at) \
                     VALUES (?1, ?2, ?3, strftime('%s','now')) \
                     ON CONFLICT(wa_jid) DO UPDATE SET \
                       last_server_id = excluded.last_server_id, \
                       last_ts = excluded.last_ts, \
                       updated_at = excluded.updated_at",
                    params![j, last_server_id, last_ts],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to write newsletter cursor")
            .map(|_| ())
    }

    pub async fn content_by_platform_message_id(
        &self,
        channel: &str,
        chat_id: &str,
        platform_message_id: &str,
    ) -> Result<Option<String>> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        let pmid = platform_message_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get a connection")?
            .interact(move |conn| {
                conn.query_row(
                    // One media message can carry TWO rows sharing this id
                    // (#2121): the main store writes the caption plus
                    // `[file: …]`, and the late "log all" pass used to write a
                    // pathless `[photo] <caption>` twin a second later. A plain
                    // `created_at DESC` returned the twin, so a reply to a photo
                    // resolved to text with no way to reach the image.
                    //
                    // The twins are exactly the five shapes below. They sort
                    // last, so a row carrying an attachment wins; a voice twin
                    // (`[voice] <transcript>`) is NOT one of them, because there
                    // the transcript is the useful half and it is the newer row.
                    //
                    // `created_at` is second-granular, so two inserts can land on
                    // the same value and leave the order to chance. `id DESC`
                    // makes the result deterministic; it never decides between a
                    // twin and a real row, only between two rows of the same kind.
                    "SELECT content FROM channel_messages \
                     WHERE channel = ?1 AND channel_chat_id = ?2 AND platform_message_id = ?3 \
                     ORDER BY (content LIKE '[photo] %' \
                               OR content LIKE '[video] %' \
                               OR content LIKE '[animation] %' \
                               OR content LIKE '[video_note]%' \
                               OR content LIKE '[document] %') ASC, \
                              created_at DESC, id DESC \
                     LIMIT 1",
                    params![ch, cid, pmid],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to look up channel message by platform id")
    }

    /// The bot message's content AND the forum thread it was posted in.
    ///
    /// A reaction update carries no thread id, and sessions are keyed by
    /// `(chat_id, topic_id)`, so a reaction in a forum topic could not be
    /// matched to its session from the update alone. The row we already read
    /// for the content knows the thread, which makes it recoverable rather
    /// than guessed at.
    pub async fn bot_message_with_thread(
        &self,
        channel: &str,
        chat_id: &str,
        platform_message_id: &str,
    ) -> Result<Option<(String, Option<String>)>> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        let pmid = platform_message_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT content, thread_id FROM channel_messages \
                     WHERE channel = ?1 AND channel_chat_id = ?2 \
                     AND platform_message_id = ?3 AND sender_id = 'bot:opencrabs' \
                     ORDER BY created_at DESC LIMIT 1",
                    params![ch, cid, pmid],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to look up bot message by platform id")
    }

    /// Like [`content_by_platform_message_id`] but only returns content when the
    /// message was sent by the bot (sender_id = 'bot:opencrabs'). Used by the
    /// inbound reaction handler to skip reactions on user-to-user messages.
    pub async fn bot_content_by_platform_message_id(
        &self,
        channel: &str,
        chat_id: &str,
        platform_message_id: &str,
    ) -> Result<Option<String>> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        let pmid = platform_message_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT content FROM channel_messages \
                     WHERE channel = ?1 AND channel_chat_id = ?2 \
                     AND platform_message_id = ?3 AND sender_id = 'bot:opencrabs' \
                     ORDER BY created_at DESC LIMIT 1",
                    params![ch, cid, pmid],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to look up bot message by platform id")
    }

    /// Most recent non-null forum topic name for a thread, used to label the
    /// session after the topic ("Devops") instead of its numeric thread id
    /// ("topic:2"). Telegram only carries the name on regular topic messages
    /// (via the topic-creation reply target); a user replying to a specific
    /// message inside the topic omits it, so we read the last one we persisted
    /// to keep the label stable.
    pub async fn latest_topic_name(
        &self,
        channel: &str,
        chat_id: &str,
        thread_id: &str,
    ) -> Result<Option<String>> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        let tid = thread_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                // Rename rows (`topic_edited`, #143) outrank creation-name
                // rows: regular messages re-teach the ORIGINAL creation name
                // via their reply target forever, so "newest non-null name"
                // alone re-stales after every rename. A rename is the
                // freshest user intent — if one exists for the thread, its
                // newest instance wins outright; only nameless-of-edit
                // threads fall back to the newest created/learned name.
                conn.query_row(
                    "SELECT topic_name FROM channel_messages \
                         WHERE channel = ?1 AND channel_chat_id = ?2 AND thread_id = ?3 \
                         AND topic_name IS NOT NULL AND topic_name != '' \
                         ORDER BY (message_type = 'topic_edited') DESC, created_at DESC \
                         LIMIT 1",
                    params![ch, cid, tid],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to fetch latest topic name")
    }

    /// Recent user-sent request texts across ALL chats, newest first (#504).
    /// Excludes the bot's own messages (sender_id = 'bot:opencrabs') and
    /// non-text kinds. Feeds the RSI command-pattern detector, which groups
    /// repeated asks into slash-command promotion candidates.
    pub async fn recent_user_requests(&self, limit: i64) -> Result<Vec<String>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT content FROM channel_messages \
                     WHERE sender_id <> 'bot:opencrabs' AND message_type = 'text' \
                     AND content <> '' \
                     ORDER BY created_at DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![limit], |r| r.get::<_, String>(0))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to fetch recent user requests")
    }

    /// Search messages by content (LIKE-based) with optional thread_id filter
    pub async fn search(
        &self,
        channel: Option<&str>,
        chat_id: Option<&str>,
        query: &str,
        limit: i64,
        thread_id: Option<&str>,
    ) -> Result<Vec<ChannelMessage>> {
        let ch = channel.map(|s| s.to_string());
        let cid = chat_id.map(|s| s.to_string());
        let tid = thread_id.map(|s| s.to_string());
        let pattern = format!("%{query}%");

        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| match (ch, cid, tid) {
                (Some(ch), Some(cid), Some(tid)) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel = ?1 AND channel_chat_id = ?2 AND content LIKE ?3 AND thread_id = ?4 \
                             ORDER BY created_at DESC LIMIT ?5",
                    )?;
                    let rows =
                        stmt.query_map(params![ch, cid, pattern, tid, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (Some(ch), Some(cid), None) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel = ?1 AND channel_chat_id = ?2 AND content LIKE ?3 \
                             ORDER BY created_at DESC LIMIT ?4",
                    )?;
                    let rows =
                        stmt.query_map(params![ch, cid, pattern, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (Some(ch), None, Some(tid)) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel = ?1 AND content LIKE ?2 AND thread_id = ?3 \
                             ORDER BY created_at DESC LIMIT ?4",
                    )?;
                    let rows =
                        stmt.query_map(params![ch, pattern, tid, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (Some(ch), None, None) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel = ?1 AND content LIKE ?2 \
                             ORDER BY created_at DESC LIMIT ?3",
                    )?;
                    let rows =
                        stmt.query_map(params![ch, pattern, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (None, Some(cid), Some(tid)) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel_chat_id = ?1 AND content LIKE ?2 AND thread_id = ?3 \
                             ORDER BY created_at DESC LIMIT ?4",
                    )?;
                    let rows =
                        stmt.query_map(params![cid, pattern, tid, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (None, Some(cid), None) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE channel_chat_id = ?1 AND content LIKE ?2 \
                             ORDER BY created_at DESC LIMIT ?3",
                    )?;
                    let rows =
                        stmt.query_map(params![cid, pattern, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (None, None, Some(tid)) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE content LIKE ?1 AND thread_id = ?2 \
                             ORDER BY created_at DESC LIMIT ?3",
                    )?;
                    let rows =
                        stmt.query_map(params![pattern, tid, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
                (None, None, None) => {
                    let mut stmt = conn.prepare_cached(
                        "SELECT * FROM channel_messages \
                             WHERE content LIKE ?1 \
                             ORDER BY created_at DESC LIMIT ?2",
                    )?;
                    let rows = stmt.query_map(params![pattern, limit], ChannelMessage::from_row)?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
            })
            .await
            .map_err(interact_err)?
            .context("Failed to search channel messages")
    }

    /// List distinct chats with message count and last message time
    pub async fn list_chats(&self, channel: Option<&str>) -> Result<Vec<ChatSummary>> {
        let ch = channel.map(|s| s.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                if let Some(ch) = ch {
                    let mut stmt = conn.prepare_cached(
                        "SELECT channel, channel_chat_id, \
                                MAX(channel_chat_name) as channel_chat_name, \
                                COUNT(*) as message_count, \
                                MAX(created_at) as last_message_at \
                         FROM channel_messages \
                         WHERE channel = ?1 \
                         GROUP BY channel, channel_chat_id \
                         ORDER BY last_message_at DESC",
                    )?;
                    let rows = stmt.query_map(params![ch], |row| {
                        Ok(ChatSummary {
                            channel: row.get(0)?,
                            channel_chat_id: row.get(1)?,
                            channel_chat_name: row.get(2)?,
                            message_count: row.get(3)?,
                            last_message_at: row.get(4)?,
                        })
                    })?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                } else {
                    let mut stmt = conn.prepare_cached(
                        "SELECT channel, channel_chat_id, \
                                MAX(channel_chat_name) as channel_chat_name, \
                                COUNT(*) as message_count, \
                                MAX(created_at) as last_message_at \
                         FROM channel_messages \
                         GROUP BY channel, channel_chat_id \
                         ORDER BY last_message_at DESC",
                    )?;
                    let rows = stmt.query_map([], |row| {
                        Ok(ChatSummary {
                            channel: row.get(0)?,
                            channel_chat_id: row.get(1)?,
                            channel_chat_name: row.get(2)?,
                            message_count: row.get(3)?,
                            last_message_at: row.get(4)?,
                        })
                    })?;
                    rows.collect::<std::result::Result<Vec<_>, _>>()
                }
            })
            .await
            .map_err(interact_err)?
            .context("Failed to list channel chats")
    }

    /// Return all distinct forum topics seen in a chat — the
    /// (thread_id, topic_name) pairs captured from incoming messages.
    ///
    /// The bot learns topic names from three sources:
    ///
    /// - `forum_topic_created` service messages it witnesses
    /// - the `reply_to_message().forum_topic_created()` chain that
    ///   Telegram includes on every regular topic message
    /// - `forum_topic_edited` renames, persisted as `topic_edited`
    ///   rows by the handler (#143)
    ///
    /// The name reported per topic is the one from the NEWEST row that
    /// carries a non-null name — not an alphabetical `MAX()`, which
    /// could serve a stale or arbitrarily-ordered name after a rename.
    /// Telegram's Bot API exposes no `listForumTopics` endpoint
    /// — there is no way to enumerate all topics in a supergroup,
    /// only learn them passively as messages arrive.
    ///
    /// Used by `telegram_send`'s `list_topics` action so the agent
    /// can map user-typed names like "#announcements" back to the
    /// numeric `thread_id` it needs to pass to `message_in_thread`.
    pub async fn topics_for_chat(&self, channel: &str, chat_id: &str) -> Result<Vec<TopicSummary>> {
        let ch = channel.to_string();
        let cid = chat_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    // Every topic row: thread stats from the aggregate,
                    // name from a LEFT JOIN pick so threads whose rows
                    // carry NO name at all still surface (with name NULL)
                    // — a thread_id is sendable even when never named (#143).
                    "SELECT t.thread_id, \
                            np.topic_name, \
                            t.message_count, \
                            t.last_message_at \
                     FROM (\
                         SELECT thread_id, COUNT(*) as message_count, \
                                MAX(created_at) as last_message_at \
                         FROM channel_messages \
                         WHERE channel = ?1 AND channel_chat_id = ?2 AND thread_id IS NOT NULL \
                         GROUP BY thread_id\
                     ) t \
                     LEFT JOIN (\
                         SELECT m.thread_id, m.topic_name \
                         FROM channel_messages m \
                         WHERE m.channel = ?1 AND m.channel_chat_id = ?2 \
                           AND m.topic_name IS NOT NULL AND m.topic_name != '' \
                           AND m.id = (\
                               SELECT m2.id FROM channel_messages m2 \
                               WHERE m2.channel = ?1 AND m2.channel_chat_id = ?2 \
                                 AND m2.thread_id = m.thread_id \
                                 AND m2.topic_name IS NOT NULL AND m2.topic_name != ''\
                                 ORDER BY (m2.message_type = 'topic_edited') DESC, \
                                          m2.created_at DESC, m2.id DESC \
                                 LIMIT 1\
                           ) \
                     ) np ON np.thread_id = t.thread_id \
                     ORDER BY t.last_message_at DESC",
                )?;
                let rows = stmt.query_map(params![ch, cid], |row| {
                    Ok(TopicSummary {
                        thread_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        topic_name: row.get(1)?,
                        message_count: row.get(2)?,
                        last_message_at: row.get(3)?,
                    })
                })?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to list topics for chat")
    }
}

/// A forum topic the bot has seen activity in. Surfaced by
/// `ChannelMessageRepository::topics_for_chat` so the agent can
/// translate user-typed topic names to numeric thread_ids.
#[derive(Debug, Clone)]
pub struct TopicSummary {
    pub thread_id: String,
    /// `None` for topics where we only ever saw messages but never
    /// the topic-creation service message and never a reply chain
    /// carrying it. The thread_id is still usable for sending.
    pub topic_name: Option<String>,
    pub message_count: i64,
    /// Unix epoch seconds — matches the column type
    /// `created_at INTEGER` used by every other query in this file.
    pub last_message_at: i64,
}
