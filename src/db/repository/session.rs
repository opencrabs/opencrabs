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

/// Title marker every cron job session ends with: `Cron: <job name>
/// [cron-job:<uuid>]`. The readable part can be renamed; the marker is the
/// stable lookup key, so classification keys on it too (#2714).
pub const CRON_TITLE_MARKER: &str = "[cron-job:";

/// The exact title of the autonomous RSI cycle session (#2714).
pub const RSI_SESSION_TITLE: &str = "RSI autonomous cycle";

/// What kind of session a title belongs to. Title-based only: cron and RSI
/// encode their identity in the title, and there is no kind column (#2714).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Human,
    Cron,
    Rsi,
    Subagent,
}

/// Classify a session by its title. Pub in the library so the GUI binary and
/// the TUI both call the same decision and one session reads as the same kind
/// on every surface (#2714). The cron check mirrors the scheduler's suffix
/// lookup: the marker must sit in a title that ends with the closing bracket,
/// so a rename of the readable part never changes the kind.
pub fn classify_session_title(title: &str) -> SessionKind {
    if title.starts_with(SUBAGENT_TITLE_PREFIX) {
        SessionKind::Subagent
    } else if title.ends_with(']') && title.contains(CRON_TITLE_MARKER) {
        SessionKind::Cron
    } else if title == RSI_SESSION_TITLE {
        SessionKind::Rsi
    } else {
        SessionKind::Human
    }
}

/// Why an existing session could not be attached as a topic. Typed so the
/// picker can show a plain sentence instead of a raw constraint error; the
/// migration triggers remain the last line of defence behind these guards.
#[derive(Debug, thiserror::Error)]
pub enum AttachTopicError {
    #[error("a session cannot be a topic of itself")]
    SelfParent,
    #[error("the target is itself a topic; topics nest only one level")]
    ParentIsTopic,
    #[error("that session already has topics of its own and cannot become one")]
    ChildIsParent,
    #[error("that session is already a topic of another group")]
    AlreadyAttached,
    #[error("session not found")]
    NotFound,
    #[error(transparent)]
    Db(#[from] anyhow::Error),
}

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

    /// Turn topics on or off for one session. `updated_at` is left alone, so
    /// turning topics on never moves the session in the recency order.
    pub async fn set_topics_enabled(&self, id: Uuid, enabled: bool) -> Result<()> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET topics_enabled = ?2 WHERE id = ?1",
                    params![id_str, enabled as i64],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to set topics_enabled")?;
        Ok(())
    }

    /// Create a topic under a session: a new session row with `parent_id` set,
    /// and topics switched on for the parent.
    ///
    /// The flag matters cross-surface: the chat topic bar and the rail menu read
    /// `topics_enabled` rather than counting children, so a parent left at 0
    /// hides its own topics everywhere except the tree that reads `parent_id`.
    /// `attach_topic` has always done both; this path now matches it.
    ///
    /// The depth trigger (migration 20261101000001) refuses a parent that is
    /// itself a topic, so the refusal comes back as an error here.
    pub async fn create_topic(&self, parent: Uuid, title: &str) -> Result<Uuid> {
        let topic = Session::new(Some(title.to_string()), None, None);
        self.create(&topic).await?;
        let (topic_id, parent_str) = (topic.id.to_string(), parent.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET parent_id = ?2 WHERE id = ?1",
                    params![topic_id, parent_str],
                )?;
                conn.execute(
                    "UPDATE sessions SET topics_enabled = 1 WHERE id = ?1",
                    params![parent_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to attach topic to its session")?;
        Ok(topic.id)
    }

    /// Attach an existing session as a topic of `parent`, transcript and
    /// settings untouched. The guards mirror the depth-1 contract so the UI
    /// gets a readable reason; the migration triggers still backstop them.
    /// Idempotent when the child already belongs to this same parent.
    pub async fn attach_topic(&self, child: Uuid, parent: Uuid) -> Result<(), AttachTopicError> {
        if child == parent {
            return Err(AttachTopicError::SelfParent);
        }
        let (child_str, parent_str) = (child.to_string(), parent.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")
            .map_err(AttachTopicError::Db)?
            .interact(move |conn| -> Result<(), AttachTopicError> {
                let tx = conn
                    .transaction()
                    .map_err(|e| AttachTopicError::Db(e.into()))?;
                let child_parent: Option<String> = tx
                    .query_row(
                        "SELECT parent_id FROM sessions WHERE id = ?1",
                        params![child_str],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| AttachTopicError::Db(e.into()))?
                    .ok_or(AttachTopicError::NotFound)?;
                let parent_parent: Option<String> = tx
                    .query_row(
                        "SELECT parent_id FROM sessions WHERE id = ?1",
                        params![parent_str],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|e| AttachTopicError::Db(e.into()))?
                    .ok_or(AttachTopicError::NotFound)?;
                if parent_parent.is_some() {
                    return Err(AttachTopicError::ParentIsTopic);
                }
                if let Some(existing) = child_parent
                    && existing != parent_str
                {
                    return Err(AttachTopicError::AlreadyAttached);
                }
                let child_kids: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM sessions WHERE parent_id = ?1",
                        params![child_str],
                        |r| r.get(0),
                    )
                    .map_err(|e| AttachTopicError::Db(e.into()))?;
                if child_kids > 0 {
                    return Err(AttachTopicError::ChildIsParent);
                }
                tx.execute(
                    "UPDATE sessions SET parent_id = ?2 WHERE id = ?1",
                    params![child_str, parent_str],
                )
                .map_err(|e| AttachTopicError::Db(e.into()))?;
                tx.execute(
                    "UPDATE sessions SET topics_enabled = 1 WHERE id = ?1",
                    params![parent_str],
                )
                .map_err(|e| AttachTopicError::Db(e.into()))?;
                tx.commit().map_err(|e| AttachTopicError::Db(e.into()))?;
                Ok(())
            })
            .await
            .map_err(|e| AttachTopicError::Db(interact_err(e)))?
    }

    /// The session's parent when it is a topic, `None` for a top-level row.
    ///
    /// `parent_id` is deliberately absent from [`Session`] (the struct predates
    /// topics and every channel path would have to carry it), so the rails and
    /// the Telegram resolver read it through here instead of hand-rolling SQL.
    pub async fn find_parent_id(&self, child: Uuid) -> Result<Option<Uuid>> {
        let child_str = child.to_string();
        let found: Option<Option<String>> = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT parent_id FROM sessions WHERE id = ?1",
                    params![child_str],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read a session's parent")?;
        found
            .flatten()
            .map(|s| Uuid::parse_str(&s).context("parent_id is not a uuid"))
            .transpose()
    }

    /// Live sessions whose title still carries a forum-topic key but that hang
    /// under nobody. One-shot repair input for the Telegram backfill (#2603);
    /// a no-op on an install that already groups its topics.
    pub async fn list_unlinked_topic_sessions(&self) -> Result<Vec<(Uuid, String)>> {
        let rows: Vec<(String, String)> = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, title FROM sessions
                     WHERE parent_id IS NULL
                       AND archived_at IS NULL
                       AND title LIKE '%:topic:%]'
                     ORDER BY created_at ASC",
                )?;
                let rows =
                    stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to list unlinked topic sessions")?;

        Ok(rows
            .into_iter()
            .filter_map(|(id, title)| Uuid::parse_str(&id).ok().map(|uuid| (uuid, title)))
            .collect())
    }

    /// How many live sessions this chat already keeps one-per-topic.
    ///
    /// The count answers one question for the desktop client: is this chat a
    /// forum? A forum is the only channel chat that ever produces a
    /// `[chat:<id>:topic:<tid>]` row, and the Bot API refuses a topic on
    /// anything else. Reading it before the API call keeps a plain group or a
    /// DM from hitting `Bad Request: the chat is not a forum` when the window
    /// offers a topic action on it.
    ///
    /// Counted from titles, not `channel_chat_key`, so it also sees the rows
    /// created before #1721 stamped keys.
    pub async fn count_topics_for_chat(&self, chat_id: i64) -> Result<i64> {
        let pattern = format!("%[chat:{chat_id}:topic:%]");
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM sessions
                     WHERE archived_at IS NULL AND title LIKE ?1",
                    params![pattern],
                    |r| r.get(0),
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to count a chat's topic sessions")
    }

    /// One session plus every topic hanging under it, as `(id, title)` in
    /// creation order, root first when the caller asked for the parent.
    ///
    /// The desktop client reads a chat's whole family through this: a parent
    /// and its topics are separate session rows, so the `All` view's union is
    /// a query, not a merge of structs it already loaded (#2762). A topic asked
    /// for directly has no children and comes back alone.
    ///
    /// Archived rows stay out of the union: a closed topic is not part of what
    /// `All` shows. The title is returned as stored, channel key included; the
    /// caller strips it for display.
    pub async fn family_of(&self, root: Uuid) -> Result<Vec<(Uuid, String)>> {
        let root_str = root.to_string();
        let rows: Vec<(String, String)> = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, COALESCE(title, '') FROM sessions
                     WHERE (id = ?1 OR parent_id = ?1)
                       AND archived_at IS NULL
                     ORDER BY created_at ASC, id ASC",
                )?;
                let rows = stmt.query_map(params![root_str], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read a session family")?;

        Ok(rows
            .into_iter()
            .filter_map(|(id, title)| Uuid::parse_str(&id).ok().map(|uuid| (uuid, title)))
            .collect())
    }

    /// Release a topic back to a top-level session (`parent_id = NULL`). The
    /// depth trigger only fires on a non-null parent, so this always passes.
    pub async fn detach_topic(&self, child: Uuid) -> Result<()> {
        let child_str = child.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET parent_id = NULL WHERE id = ?1",
                    params![child_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to detach topic")?;
        Ok(())
    }

    /// Every (topic, parent) pair, as ids. A topic is a session with a parent.
    pub async fn topic_parents(&self) -> Result<Vec<(Uuid, Uuid)>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, parent_id FROM sessions WHERE parent_id IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
                Ok::<_, rusqlite::Error>(rows)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read topic parents")?
            .into_iter()
            .map(|(child, parent)| {
                Ok((
                    Uuid::parse_str(&child).context("malformed topic id")?,
                    Uuid::parse_str(&parent).context("malformed parent id")?,
                ))
            })
            .collect()
    }

    /// Rename a session or topic in place. `updated_at` is left alone, so a
    /// rename does not move the row in the recency order.
    pub async fn rename(&self, id: Uuid, title: &str) -> Result<()> {
        let (id_str, title) = (id.to_string(), title.to_string());
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET title = ?2 WHERE id = ?1",
                    params![id_str, title],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to rename session")?;
        Ok(())
    }

    /// What the topic info panel shows for one session: its parent (if it is
    /// a topic), when it was created, and how many messages it holds.
    pub async fn topic_info(&self, id: Uuid) -> Result<Option<(Option<Uuid>, i64, i64)>> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT parent_id, created_at,
                        (SELECT COUNT(*) FROM messages WHERE session_id = sessions.id)
                     FROM sessions WHERE id = ?1",
                    params![id_str],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read topic info")?
            .map(|(parent, created, count)| {
                let parent = match parent {
                    Some(p) => Some(Uuid::parse_str(&p).context("malformed parent id")?),
                    None => None,
                };
                Ok((parent, created, count))
            })
            .transpose()
    }

    /// Close a topic (`closed_at` set) or reopen it (`closed_at` cleared). A
    /// closed topic stays readable; it refuses new turns until reopened.
    pub async fn set_closed(&self, id: Uuid, closed: bool) -> Result<()> {
        let id_str = id.to_string();
        let now = Utc::now().timestamp();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET closed_at = ?2 WHERE id = ?1",
                    params![id_str, if closed { Some(now) } else { None }],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to set closed_at")?;
        Ok(())
    }

    /// Every session that is closed, as ids.
    pub async fn closed_ids(&self) -> Result<Vec<Uuid>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt =
                    conn.prepare_cached("SELECT id FROM sessions WHERE closed_at IS NOT NULL")?;
                let ids = stmt
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok::<_, rusqlite::Error>(ids)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read closed topics")?
            .into_iter()
            .map(|s| Uuid::parse_str(&s).context("malformed session id"))
            .collect()
    }

    /// Every session with topics turned on, as ids.
    /// Fold or unfold one session's topic tree. The choice is kept per session (#2427).
    pub async fn set_topics_collapsed(&self, id: Uuid, collapsed: bool) -> Result<()> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET topics_collapsed = ?2 WHERE id = ?1",
                    rusqlite::params![id_str, collapsed as i64],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to write topics_collapsed")?;
        Ok(())
    }

    /// Every session whose topic tree is folded.
    pub async fn topics_collapsed_ids(&self) -> Result<Vec<Uuid>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt =
                    conn.prepare_cached("SELECT id FROM sessions WHERE topics_collapsed = 1")?;
                let ids = stmt
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok::<_, rusqlite::Error>(ids)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read topics_collapsed")?
            .into_iter()
            .map(|s| Uuid::parse_str(&s).context("malformed session id"))
            .collect()
    }

    /// Set or clear one session's icon, root or topic. None clears it back
    /// to the default.
    pub async fn set_topic_icon(&self, id: Uuid, icon: Option<&str>) -> Result<()> {
        let (id_str, icon) = (id.to_string(), icon.map(str::to_string));
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET topic_icon = ?2 WHERE id = ?1",
                    rusqlite::params![id_str, icon],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to write topic_icon")?;
        Ok(())
    }

    /// Every session that has an icon of its own, root or topic, with the
    /// icon. The rename dialog sets one on any conversation, so the read
    /// cannot filter to children: a root card paints its pick too (#2790).
    pub async fn topic_icons(&self) -> Result<Vec<(Uuid, String)>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT id, topic_icon FROM sessions
                     WHERE topic_icon IS NOT NULL",
                )?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
                Ok::<_, rusqlite::Error>(rows)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read topic_icon")?
            .into_iter()
            .map(|(id, icon)| {
                Uuid::parse_str(&id)
                    .context("malformed session id")
                    .map(|id| (id, icon))
            })
            .collect()
    }

    pub async fn topic_enabled_ids(&self) -> Result<Vec<Uuid>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt =
                    conn.prepare_cached("SELECT id FROM sessions WHERE topics_enabled = 1")?;
                let ids = stmt
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok::<_, rusqlite::Error>(ids)
            })
            .await
            .map_err(interact_err)?
            .context("Failed to read topics_enabled")?
            .into_iter()
            .map(|s| Uuid::parse_str(&s).context("malformed session id"))
            .collect()
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
    /// usage joins, this is a real delete, used only by the sub-agent sweep,
    /// where the whole point is to stop the data accumulating (#931). Runs in
    /// one transaction so a failure part-way cannot leave a session row whose
    /// children are gone.
    ///
    /// Attached topics are released, not deleted: `parent_id` cascades on
    /// delete, so a real conversation grouped under this session would be
    /// destroyed with it. Detaching first turns a purge into "the children
    /// become top-level sessions", never "the group wipes its conversations".
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
                // Release attached topics before the row goes: the parent_id
                // foreign key cascades on delete, so without this a grouped
                // conversation dies with its parent. The depth trigger only
                // fires on a non-null parent, so setting NULL passes it.
                tx.execute(
                    "UPDATE sessions SET parent_id = NULL WHERE parent_id = ?1",
                    params![id_str],
                )?;
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

#[cfg(test)]
mod tests {
    use super::{
        AttachTopicError, CRON_TITLE_MARKER, OptionalExt, RSI_SESSION_TITLE, SUBAGENT_TITLE_PREFIX,
        Session, SessionKind, SessionRepository, classify_session_title, params,
    };
    use crate::db::Database;
    use uuid::Uuid;

    /// #2714: one title, one kind, on every surface. The four identities the
    /// rail and session-list filters need to tell apart.
    #[test]
    fn session_kind_classifies_the_four_identities() {
        assert_eq!(
            classify_session_title("Cron: morning recap [cron-job:9b2f]"),
            SessionKind::Cron
        );
        assert_eq!(classify_session_title(RSI_SESSION_TITLE), SessionKind::Rsi);
        assert_eq!(
            classify_session_title("subagent: scout"),
            SessionKind::Subagent
        );
        assert_eq!(classify_session_title("CrabsDev"), SessionKind::Human);
        // Pin the remaining constants to the producer literals too.
        assert_eq!(RSI_SESSION_TITLE, "RSI autonomous cycle");
        assert_eq!(SUBAGENT_TITLE_PREFIX, "subagent:");
    }

    /// The cron marker is the stable key: renaming the readable part of the
    /// title never changes the kind, same contract as the scheduler lookup.
    #[test]
    fn session_kind_cron_marker_survives_a_rename() {
        assert_eq!(
            classify_session_title("Renamed by the user [cron-job:9b2f]"),
            SessionKind::Cron
        );
        assert_eq!(classify_session_title("[cron-job:9b2f]"), SessionKind::Cron);
        // Pin the constants to the literals the producers write (cron
        // scheduler, rsi.rs, spawn_agent) so a drift fails here first.
        assert_eq!(CRON_TITLE_MARKER, "[cron-job:");
        // A mention of the marker mid-title without the closing suffix is
        // not a cron session.
        assert_eq!(
            classify_session_title("notes about [cron-job: syntax"),
            SessionKind::Human
        );
    }

    /// The RSI identity is the exact title, not a prefix or a substring.
    #[test]
    fn session_kind_rsi_is_the_exact_title() {
        assert_eq!(
            classify_session_title("RSI autonomous cycle follow-up"),
            SessionKind::Human
        );
        assert_eq!(
            classify_session_title("my RSI autonomous cycle"),
            SessionKind::Human
        );
    }

    /// The subagent rule stays the existing prefix contract: a title that
    /// merely mentions the prefix mid-string is human.
    #[test]
    fn session_kind_subagent_is_a_prefix_not_a_mention() {
        assert_eq!(
            classify_session_title(SUBAGENT_TITLE_PREFIX),
            SessionKind::Subagent
        );
        assert_eq!(
            classify_session_title("talk about subagent: builds"),
            SessionKind::Human
        );
    }

    async fn repo() -> (Database, SessionRepository) {
        let db = Database::connect_in_memory().await.expect("in-memory db");
        db.run_migrations().await.expect("migrations");
        let repo = SessionRepository::new(db.pool().clone());
        (db, repo)
    }

    async fn session(repo: &SessionRepository, title: &str) -> Uuid {
        let s = Session::new(Some(title.to_string()), None, None);
        let id = s.id;
        repo.create(&s).await.expect("create session");
        id
    }

    async fn parent_of(db: &Database, id: Uuid) -> Option<String> {
        let id_str = id.to_string();
        db.pool()
            .get()
            .await
            .unwrap()
            .interact(move |conn| {
                conn.query_row(
                    "SELECT parent_id FROM sessions WHERE id = ?1",
                    params![id_str],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
            })
            .await
            .unwrap()
            .unwrap()
            .flatten()
    }

    async fn topics_enabled(db: &Database, id: Uuid) -> bool {
        let id_str = id.to_string();
        db.pool()
            .get()
            .await
            .unwrap()
            .interact(move |conn| {
                conn.query_row(
                    "SELECT topics_enabled FROM sessions WHERE id = ?1",
                    params![id_str],
                    |r| r.get::<_, i64>(0),
                )
            })
            .await
            .unwrap()
            .unwrap()
            != 0
    }

    /// A session can never be its own topic.
    #[tokio::test]
    async fn attach_topic_rejects_self() {
        let (_db, repo) = repo().await;
        let a = session(&repo, "A").await;
        let err = repo.attach_topic(a, a).await.unwrap_err();
        assert!(matches!(err, AttachTopicError::SelfParent));
    }

    /// Depth-1 contract: a target that is itself a topic refuses a child.
    #[tokio::test]
    async fn attach_topic_rejects_depth_two() {
        let (_db, repo) = repo().await;
        let a = session(&repo, "A").await;
        let b = session(&repo, "B").await;
        let c = session(&repo, "C").await;
        repo.attach_topic(b, a).await.expect("B under A");
        let err = repo.attach_topic(c, b).await.unwrap_err();
        assert!(matches!(err, AttachTopicError::ParentIsTopic));
    }

    /// A session that already has topics of its own cannot become one.
    #[tokio::test]
    async fn attach_topic_rejects_child_that_is_a_parent() {
        let (_db, repo) = repo().await;
        let a = session(&repo, "A").await;
        let b = session(&repo, "B").await;
        let c = session(&repo, "C").await;
        repo.attach_topic(c, a).await.expect("C under A");
        let err = repo.attach_topic(a, b).await.unwrap_err();
        assert!(matches!(err, AttachTopicError::ChildIsParent));
    }

    /// A child already attached to a different group is refused.
    #[tokio::test]
    async fn attach_topic_rejects_already_attached() {
        let (_db, repo) = repo().await;
        let a = session(&repo, "A").await;
        let b = session(&repo, "B").await;
        let c = session(&repo, "C").await;
        repo.attach_topic(c, a).await.expect("C under A");
        let err = repo.attach_topic(c, b).await.unwrap_err();
        assert!(matches!(err, AttachTopicError::AlreadyAttached));
    }

    /// A successful attach sets the child's parent and turns topics on for
    /// the parent; re-attaching to the same parent is idempotent.
    #[tokio::test]
    async fn attach_topic_sets_parent_and_enables_topics_idempotently() {
        let (db, repo) = repo().await;
        let a = session(&repo, "A").await;
        let c = session(&repo, "C").await;
        repo.attach_topic(c, a).await.expect("C under A");
        assert_eq!(parent_of(&db, c).await, Some(a.to_string()));
        assert!(topics_enabled(&db, a).await);
        repo.attach_topic(c, a).await.expect("same parent again");
        assert_eq!(parent_of(&db, c).await, Some(a.to_string()));
    }

    /// Detach releases a topic back to a top-level session.
    #[tokio::test]
    async fn detach_topic_clears_parent() {
        let (db, repo) = repo().await;
        let a = session(&repo, "A").await;
        let c = session(&repo, "C").await;
        repo.attach_topic(c, a).await.expect("C under A");
        repo.detach_topic(c).await.expect("detach");
        assert_eq!(parent_of(&db, c).await, None);
    }

    /// Purging a group releases its children as top-level sessions instead of
    /// cascading them away: the attached conversation and its messages live.
    #[tokio::test]
    async fn purge_releases_children_instead_of_deleting_them() {
        let (db, repo) = repo().await;
        let a = session(&repo, "A").await;
        let c = session(&repo, "C").await;
        repo.attach_topic(c, a).await.expect("C under A");
        repo.purge(a).await.expect("purge group");
        assert!(repo.find_by_id(a).await.unwrap().is_none(), "group is gone");
        assert!(
            repo.find_by_id(c).await.unwrap().is_some(),
            "the attached conversation survives the purge"
        );
        assert_eq!(
            parent_of(&db, c).await,
            None,
            "the survivor is released to a top-level session"
        );
    }
}
