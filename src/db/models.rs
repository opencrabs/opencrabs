//! Database Models
//!
//! Data structures representing database entities.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ─── Row helpers ─────────────────────────────────────────────────────────────

/// Read a column declared `TEXT` but tolerate the row's storage class being
/// `BLOB`. SQLite's flexible typing accepts byte-buffer inserts into TEXT
/// columns, and an earlier sqlx-era binding for `cron_jobs.prompt` produced
/// exactly this glitch: 4 of 7 rows lived as BLOB and broke the strict
/// `String::column_result` decode used by `row.get::<String, _>` —
/// `CronJobRepository::list_all` failed, Mission Control's schedule panel
/// silently empties out (2026-05-17). Migration `20260517...` heals the
/// stored rows; this helper covers any future row that slips through with
/// the wrong storage class without bringing down whole list queries.
pub fn text_or_blob_col(row: &rusqlite::Row, col: &str) -> rusqlite::Result<String> {
    use rusqlite::types::ValueRef;
    match row.get_ref(col)? {
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
            String::from_utf8(bytes.to_vec()).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        }
        ValueRef::Null => Err(rusqlite::Error::InvalidColumnType(
            0,
            col.to_string(),
            rusqlite::types::Type::Null,
        )),
        _ => Err(rusqlite::Error::InvalidColumnType(
            0,
            col.to_string(),
            rusqlite::types::Type::Text,
        )),
    }
}

/// Parse a UUID string column from a rusqlite row.
pub fn uuid_col(row: &rusqlite::Row, col: &str) -> rusqlite::Result<Uuid> {
    let s: String = row.get(col)?;
    Uuid::parse_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

/// Parse a Unix-timestamp column into `DateTime<Utc>`.
pub fn timestamp_col(row: &rusqlite::Row, col: &str) -> rusqlite::Result<DateTime<Utc>> {
    let ts: i64 = row.get(col)?;
    DateTime::from_timestamp(ts, 0).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            format!("Invalid timestamp for {col}").into(),
        )
    })
}

/// Parse an optional Unix-timestamp column.
pub fn opt_timestamp_col(
    row: &rusqlite::Row,
    col: &str,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let ts: Option<i64> = row.get(col)?;
    Ok(ts.and_then(|t| DateTime::from_timestamp(t, 0)))
}

/// Parse an RFC-3339 string column into `DateTime<Utc>`.
pub fn rfc3339_col(row: &rusqlite::Row, col: &str) -> rusqlite::Result<DateTime<Utc>> {
    let s: String = row.get(col)?;
    DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

/// Parse an optional RFC-3339 string column.
pub fn opt_rfc3339_col(row: &rusqlite::Row, col: &str) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let s: Option<String> = row.get(col)?;
    Ok(s.and_then(|v| {
        DateTime::parse_from_rfc3339(&v)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }))
}

// ─── Project ─────────────────────────────────────────────────────────────────

/// Project model — groups related sessions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    /// Normalized origin remote (`host/owner/repo`), adopted from a session
    /// whose checkout proved it (#1510). The identity stronger than the name:
    /// basename matching conflates unrelated checkouts that share a name.
    pub repo_remote: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Project {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Project {
            id: uuid_col(row, "id")?,
            name: row.get("name")?,
            description: row.get("description")?,
            // `unwrap_or` rather than `?`: heal tests build partial schemas
            // via `to_version(N)` and a project row read from a pre-#1510
            // schema has no remote for it to be, exactly like a NULL.
            repo_remote: row.get("repo_remote").unwrap_or(None),
            created_at: timestamp_col(row, "created_at")?,
            updated_at: timestamp_col(row, "updated_at")?,
        })
    }

    /// Create a new project
    pub fn new(name: String, description: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            name,
            description,
            repo_remote: None,
            created_at: now,
            updated_at: now,
        }
    }
}

// ─── Session ─────────────────────────────────────────────────────────────────

/// Session model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub title: Option<String>,
    pub model: Option<String>,
    pub provider_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub token_count: i64,
    pub total_cost: f64,
    pub working_directory: Option<String>,
    pub auto_title_attempted: bool,
    pub project_id: Option<Uuid>,
    /// Stable channel identity `[chat:<id>]` extracted from the title
    /// (#1721). NULL for TUI/local sessions; unique across live rows.
    pub channel_chat_key: Option<String>,
}

impl Session {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Session {
            id: uuid_col(row, "id")?,
            title: row.get("title")?,
            model: row.get("model")?,
            provider_name: row.get("provider_name")?,
            created_at: timestamp_col(row, "created_at")?,
            updated_at: timestamp_col(row, "updated_at")?,
            archived_at: opt_timestamp_col(row, "archived_at")?,
            token_count: row.get("token_count")?,
            total_cost: row.get("total_cost")?,
            working_directory: row.get("working_directory")?,
            auto_title_attempted: row.get::<_, i32>("auto_title_attempted").unwrap_or(0) != 0,
            project_id: row
                .get::<_, Option<String>>("project_id")
                .ok()
                .flatten()
                .and_then(|s| Uuid::parse_str(&s).ok()),
            channel_chat_key: row.get("channel_chat_key")?,
        })
    }

    /// Create a new session
    pub fn new(
        title: Option<String>,
        model: Option<String>,
        provider_name: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            title,
            model,
            provider_name,
            created_at: now,
            updated_at: now,
            archived_at: None,
            token_count: 0,
            total_cost: 0.0,
            working_directory: None,
            auto_title_attempted: false,
            project_id: None,
            channel_chat_key: None,
        }
    }

    /// Check if the session is archived
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }
}

// ─── Message ─────────────────────────────────────────────────────────────────

/// Message model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub session_id: Uuid,
    pub role: String,
    pub content: String,
    pub sequence: i32,
    pub created_at: DateTime<Utc>,
    /// Output tokens (completion) reported by the provider. For assistant
    /// messages this is `usage.completion_tokens` / `output_tokens`.
    pub token_count: Option<i64>,
    pub cost: Option<f64>,
    /// Server-reported prompt token count for the request that produced
    /// this assistant message (`usage.prompt_tokens` / `input_tokens`).
    /// Populated only on assistant rows; always `None` for user rows.
    /// Used as the authoritative "last known context size" on session load
    /// — no more tokenizing raw message content to estimate.
    pub input_tokens: Option<i64>,
    /// Tokens written to the provider's prompt cache (cache creation).
    /// Populated only on assistant rows; always `None` for user rows.
    pub cache_creation_tokens: Option<i64>,
    /// Tokens served from the provider's prompt cache (cache hits).
    /// Populated only on assistant rows; always `None` for user rows.
    pub cache_read_tokens: Option<i64>,
    /// Reasoning/thinking content for non-CLI providers (dialagram, custom
    /// OpenAI-compatible). Persisted separately from `content` so it
    /// survives restart and can be reconstructed as Ctrl+O expandable
    /// thinking blocks in the TUI. CLI providers store reasoning inline
    /// inside `content` as `<!-- reasoning -->` markers instead.
    pub thinking: Option<String>,
    /// Wall-clock seconds the turn took, measured from a monotonic clock and
    /// stamped at turn end (#964). Populated only on assistant rows.
    ///
    /// Stored rather than re-derived: the assistant row is created at turn
    /// START and updated in place, so its `created_at` matches the triggering
    /// user message and any timestamp subtraction collapses to zero.
    pub duration_secs: Option<i64>,
    /// Where a forwarded copy came from (#2608). `None` for every row the
    /// user typed or the agent wrote; `Some` only on rows that were copied
    /// out of another session.
    ///
    /// Boxed on purpose: provenance is rare, and paying its 72 bytes on every
    /// message grew `AgentResponse`, which tripped `clippy::large_enum_variant`
    /// on `TuiEvent::ResponseComplete` (#2608).
    pub forward: Option<Box<ForwardProvenance>>,
}

/// The provenance of one forwarded row (#2608).
///
/// A forward is a copy, so the destination keeps its own row and carries a
/// frozen description of where it came from. The label is stored, not joined:
/// renaming the source session later must not rewrite what the reader saw
/// when the message arrived.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardProvenance {
    /// The session the message was copied out of.
    pub from_session: Uuid,
    /// The row it was copied from.
    pub from_message: Uuid,
    /// The source session's name, frozen at forward time.
    pub label: String,
    /// Shared by every row of one multi-select forward, so the renderer
    /// draws one header and one accent bar over N rows.
    pub batch: Uuid,
}

/// Read the forward provenance off a row, if any. Each column is optional on
/// its own so a partially-written row loads as no provenance instead of
/// failing the whole transcript (#2608).
fn forward_from_columns(row: &rusqlite::Row) -> Option<Box<ForwardProvenance>> {
    let from_session = uuid_col(row, "forwarded_from_session").ok()?;
    let from_message = uuid_col(row, "forwarded_from_message").ok()?;
    let batch = uuid_col(row, "forward_batch_id").ok()?;
    let label: Option<String> = row.get("forwarded_from_label").ok().flatten();
    Some(Box::new(ForwardProvenance {
        from_session,
        from_message,
        label: label.unwrap_or_default(),
        batch,
    }))
}

/// The rows one forward writes into the destination session (#2608).
///
/// Pure, so the two rules that decide whether a forward is safe to read are
/// pinned by test rather than by the SQL around them:
///
/// 1. Every copied row lands as `role = "user"`. Keeping the source role would
///    make the destination's next agent turn read another conversation's
///    answer as its own prior output, which silently corrupts that session.
/// 2. Nothing about the source turn rides along: no cost, tokens, cache
///    counters or duration, because those belong to the turn that produced
///    them, not to the session that received the copy.
pub fn forwarded_rows(
    source: &[Message],
    target: Uuid,
    first_sequence: i32,
    label: &str,
    batch: Uuid,
) -> Vec<Message> {
    source
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut row = Message::new(
                target,
                "user".to_string(),
                m.content.clone(),
                first_sequence + i as i32,
            );
            row.forward = Some(Box::new(ForwardProvenance {
                from_session: m.session_id,
                from_message: m.id,
                label: label.to_string(),
                batch,
            }));
            row
        })
        .collect()
}

impl Message {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Message {
            id: uuid_col(row, "id")?,
            session_id: uuid_col(row, "session_id")?,
            role: row.get("role")?,
            content: row.get("content")?,
            sequence: row.get("sequence")?,
            created_at: timestamp_col(row, "created_at")?,
            token_count: row.get("token_count")?,
            cost: row.get("cost")?,
            input_tokens: row.get("input_tokens").ok(),
            cache_creation_tokens: row.get("cache_creation_tokens").ok(),
            cache_read_tokens: row.get("cache_read_tokens").ok(),
            thinking: row.get("thinking").ok().flatten(),
            // `.ok()` so rows from before the column existed still load.
            duration_secs: row.get("duration_secs").ok().flatten(),
            forward: forward_from_columns(row),
        })
    }

    /// Create a new message
    pub fn new(session_id: Uuid, role: String, content: String, sequence: i32) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            role,
            content,
            sequence,
            created_at: Utc::now(),
            token_count: None,
            cost: None,
            input_tokens: None,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            thinking: None,
            duration_secs: None,
            forward: None,
        }
    }
}

// ─── File ────────────────────────────────────────────────────────────────────

/// File model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct File {
    pub id: Uuid,
    pub session_id: Uuid,
    pub path: std::path::PathBuf,
    pub content: Option<String>,
    pub size: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl File {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(File {
            id: uuid_col(row, "id")?,
            session_id: uuid_col(row, "session_id")?,
            path: std::path::PathBuf::from(row.get::<_, String>("path")?),
            content: row.get("content")?,
            size: row.get("size").ok(),
            created_at: timestamp_col(row, "created_at")?,
            updated_at: timestamp_col(row, "updated_at")?,
        })
    }

    /// Create a new file record
    pub fn new(session_id: Uuid, path: std::path::PathBuf, content: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            session_id,
            path,
            content,
            size: None,
            created_at: now,
            updated_at: now,
        }
    }
}

// ─── Attachment ──────────────────────────────────────────────────────────────

/// Attachment model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub id: Uuid,
    pub message_id: Uuid,
    #[serde(rename = "type")]
    pub attachment_type: String,
    pub mime_type: Option<String>,
    pub path: Option<std::path::PathBuf>,
    pub size_bytes: Option<i64>,
    pub created_at: DateTime<Utc>,
}

impl Attachment {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Attachment {
            id: uuid_col(row, "id")?,
            message_id: uuid_col(row, "message_id")?,
            attachment_type: row.get("attachment_type")?,
            mime_type: row.get("mime_type")?,
            path: row
                .get::<_, Option<String>>("path")?
                .map(std::path::PathBuf::from),
            size_bytes: row.get("size_bytes")?,
            created_at: timestamp_col(row, "created_at")?,
        })
    }
}

// ─── ToolExecution ───────────────────────────────────────────────────────────

/// Tool execution model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecution {
    pub id: Uuid,
    pub message_id: Uuid,
    pub tool_name: String,
    /// JSON
    pub arguments: String,
    /// JSON
    pub result: Option<String>,
    pub status: String,
    pub approved_at: Option<DateTime<Utc>>,
    pub executed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl ToolExecution {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(ToolExecution {
            id: uuid_col(row, "id")?,
            message_id: uuid_col(row, "message_id")?,
            tool_name: row.get("tool_name")?,
            arguments: row.get("arguments")?,
            result: row.get("result")?,
            status: row.get("status")?,
            approved_at: opt_timestamp_col(row, "approved_at")?,
            executed_at: opt_timestamp_col(row, "executed_at")?,
            created_at: timestamp_col(row, "created_at")?,
        })
    }
}

// ─── ChannelMessage ──────────────────────────────────────────────────────────

/// Channel message model — passive capture of messages from channel platforms
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub id: Uuid,
    pub channel: String,
    pub channel_chat_id: String,
    pub channel_chat_name: Option<String>,
    pub sender_id: String,
    pub sender_name: String,
    pub content: String,
    pub message_type: String,
    pub platform_message_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub thread_id: Option<String>,
    pub topic_name: Option<String>,
}

impl ChannelMessage {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(ChannelMessage {
            id: uuid_col(row, "id")?,
            channel: row.get("channel")?,
            channel_chat_id: row.get("channel_chat_id")?,
            channel_chat_name: row.get("channel_chat_name")?,
            sender_id: row.get("sender_id")?,
            sender_name: row.get("sender_name")?,
            content: row.get("content")?,
            message_type: row.get("message_type")?,
            platform_message_id: row.get("platform_message_id")?,
            created_at: timestamp_col(row, "created_at")?,
            thread_id: row.get("thread_id").unwrap_or(None),
            topic_name: row.get("topic_name").unwrap_or(None),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel: String,
        channel_chat_id: String,
        channel_chat_name: Option<String>,
        sender_id: String,
        sender_name: String,
        content: String,
        message_type: String,
        platform_message_id: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            channel,
            channel_chat_id,
            channel_chat_name,
            sender_id,
            sender_name,
            content,
            message_type,
            platform_message_id,
            created_at: Utc::now(),
            thread_id: None,
            topic_name: None,
        }
    }

    /// Set thread/topic context for forum-aware messages (e.g. Telegram topics)
    pub fn with_thread(mut self, thread_id: Option<String>, topic_name: Option<String>) -> Self {
        self.thread_id = thread_id;
        self.topic_name = topic_name;
        self
    }
}

// ─── CronJob ─────────────────────────────────────────────────────────────────

/// Cron job model — a scheduled isolated session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: Uuid,
    pub name: String,
    pub cron_expr: String,
    pub timezone: String,
    pub prompt: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: String,
    pub auto_approve: bool,
    pub deliver_to: Option<String>,
    pub deliver_api_key: Option<String>,
    pub enabled: bool,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Profile this job was created in. `None` = legacy job created before
    /// profile stamping (scheduler runs it anywhere). Newly created jobs store
    /// the active profile name, or the literal `"default"` for the base
    /// profile, so the scheduler refuses to run them under another profile's
    /// brain/config/tools (#182).
    pub profile_name: Option<String>,
    /// Optional shell command to run as a pre-flight probe.
    pub trigger_cmd: Option<String>,
    /// Condition for the trigger to fire: "non_empty" (default), "exit_non_zero", "always".
    pub trigger_on: Option<String>,
    /// If true and deliver_to resolves to a session, sets the active goal in that session.
    pub set_goal: bool,
    /// Template for formatting goal/notification from trigger output.
    pub goal_template: Option<String>,
    /// One-shot retirement flag (#544): when true, the dispatch-time schedule
    /// advance (the same UPDATE that moves `next_run_at`) also sets
    /// `enabled = 0`, so a spent one-shot reads disabled instead of staying
    /// armed until the same date next year.
    pub run_once: bool,
}

impl CronJob {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(CronJob {
            id: uuid_col(row, "id")?,
            name: text_or_blob_col(row, "name")?,
            cron_expr: text_or_blob_col(row, "cron_expr")?,
            timezone: text_or_blob_col(row, "timezone")?,
            // `prompt` was the column that surfaced the bug — earlier
            // sqlx-era inserts stored it as BLOB on 4 of 7 rows.
            // `text_or_blob_col` handles either storage class; covers
            // the other free-form TEXT columns defensively.
            prompt: text_or_blob_col(row, "prompt")?,
            provider: row.get("provider")?,
            model: row.get("model")?,
            thinking: text_or_blob_col(row, "thinking")?,
            auto_approve: row.get::<_, i32>("auto_approve")? != 0,
            deliver_to: row.get("deliver_to")?,
            deliver_api_key: row.get("deliver_api_key")?,
            enabled: row.get::<_, i32>("enabled")? != 0,
            last_run_at: opt_rfc3339_col(row, "last_run_at")?,
            next_run_at: opt_rfc3339_col(row, "next_run_at")?,
            created_at: rfc3339_col(row, "created_at")?,
            updated_at: rfc3339_col(row, "updated_at")?,
            profile_name: row.get("profile_name")?,
            trigger_cmd: row.get("trigger_cmd").unwrap_or(None),
            trigger_on: row.get("trigger_on").unwrap_or(None),
            set_goal: row
                .get::<_, Option<i32>>("set_goal")
                .unwrap_or(None)
                .unwrap_or(0)
                != 0,
            goal_template: row.get("goal_template").unwrap_or(None),
            run_once: row
                .get::<_, Option<i32>>("run_once")
                .unwrap_or(None)
                .unwrap_or(0)
                != 0,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: String,
        cron_expr: String,
        timezone: String,
        prompt: String,
        provider: Option<String>,
        model: Option<String>,
        thinking: String,
        auto_approve: bool,
        deliver_to: Option<String>,
        deliver_api_key: Option<String>,
    ) -> Self {
        Self::new_with_trigger(
            name,
            cron_expr,
            timezone,
            prompt,
            provider,
            model,
            thinking,
            auto_approve,
            deliver_to,
            deliver_api_key,
            None,
            None,
            false,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_trigger(
        name: String,
        cron_expr: String,
        timezone: String,
        prompt: String,
        provider: Option<String>,
        model: Option<String>,
        thinking: String,
        auto_approve: bool,
        deliver_to: Option<String>,
        deliver_api_key: Option<String>,
        trigger_cmd: Option<String>,
        trigger_on: Option<String>,
        set_goal: bool,
        goal_template: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            name,
            cron_expr,
            timezone,
            prompt,
            provider,
            model,
            thinking,
            auto_approve,
            deliver_to,
            deliver_api_key,
            enabled: true,
            last_run_at: None,
            next_run_at: None,
            created_at: now,
            updated_at: now,
            // Stamp the profile this job is born into. The base profile is
            // stored as the literal "default" (not None) so the scheduler can
            // enforce the match. Only legacy pre-stamping rows stay NULL.
            // current_profile_name() reads the task-local profile, so a job
            // created while running inside a foreign profile's scope is
            // attributed to that profile, not the process global.
            profile_name: Some(crate::config::profile::current_profile_name()),
            trigger_cmd,
            trigger_on: trigger_on.or_else(|| Some("non_empty".to_string())),
            set_goal,
            goal_template,
            // run_once is set by the caller after construction, the same way
            // cron_manage sets next_run_at (#544), so this ctor keeps its arity.
            run_once: false,
        }
    }
}

// ─── CronJobRun ──────────────────────────────────────────────────────────────

/// A single execution record for a cron job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJobRun {
    pub id: Uuid,
    pub job_id: Uuid,
    pub job_name: String,
    pub status: String, // "running", "success", "error", "interrupted"
    pub content: Option<String>,
    pub error: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost: f64,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl CronJobRun {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(CronJobRun {
            id: uuid_col(row, "id")?,
            job_id: uuid_col(row, "job_id")?,
            job_name: row.get("job_name")?,
            status: row.get("status")?,
            content: row.get("content")?,
            error: row.get("error")?,
            input_tokens: row.get("input_tokens")?,
            output_tokens: row.get("output_tokens")?,
            cost: row.get("cost")?,
            provider: row.get("provider")?,
            model: row.get("model")?,
            started_at: rfc3339_col(row, "started_at")?,
            completed_at: opt_rfc3339_col(row, "completed_at")?,
            created_at: rfc3339_col(row, "created_at")?,
        })
    }

    pub fn new_running(
        job_id: Uuid,
        job_name: String,
        provider: Option<String>,
        model: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            job_id,
            job_name,
            status: "running".to_string(),
            content: None,
            error: None,
            input_tokens: 0,
            output_tokens: 0,
            cost: 0.0,
            provider,
            model,
            started_at: now,
            completed_at: None,
            created_at: now,
        }
    }
}

// ─── FeedbackEntry ──────────────────────────────────────────────────────────

/// Feedback ledger entry — append-only observations for recursive self-improvement.
///
/// Records tool outcomes, user corrections, provider errors, and performance
/// signals. Consumed by the feedback_analyze and self_improve tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub id: i64,
    pub session_id: String,
    pub event_type: String,
    pub dimension: String,
    pub value: f64,
    pub metadata: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl FeedbackEntry {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(FeedbackEntry {
            id: row.get("id")?,
            session_id: row.get("session_id")?,
            event_type: row.get("event_type")?,
            dimension: row.get("dimension")?,
            value: row.get("value")?,
            metadata: row.get("metadata")?,
            created_at: rfc3339_col(row, "created_at")?,
        })
    }
}

// ─── GoalState ─────────────────────────────────────────────────────────────

/// Goal state model — tracks an autonomous goal per session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalState {
    pub id: Uuid,
    pub session_id: Uuid,
    pub goal_text: String,
    /// "active", "paused", "completed", "failed"
    pub state: String,
    pub turns_used: i32,
    pub max_turns: i32,
    pub consecutive_parse_failures: i32,
    pub judge_verdict: Option<String>,
    pub judge_reason: Option<String>,
    pub channel: Option<String>,
    pub channel_chat_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl GoalState {
    pub fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(GoalState {
            id: uuid_col(row, "id")?,
            session_id: uuid_col(row, "session_id")?,
            goal_text: row.get("goal_text")?,
            state: row.get("state")?,
            turns_used: row.get("turns_used")?,
            max_turns: row.get("max_turns")?,
            consecutive_parse_failures: row.get("consecutive_parse_failures")?,
            judge_verdict: row.get("judge_verdict")?,
            judge_reason: row.get("judge_reason")?,
            channel: row.get("channel")?,
            channel_chat_id: row.get("channel_chat_id")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }

    pub fn new(
        session_id: Uuid,
        goal_text: String,
        channel: Option<String>,
        channel_chat_id: Option<String>,
        max_turns: Option<u32>,
    ) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4(),
            session_id,
            goal_text,
            state: "active".to_string(),
            turns_used: 0,
            max_turns: max_turns.unwrap_or(20) as i32,
            consecutive_parse_failures: 0,
            judge_verdict: None,
            judge_reason: None,
            channel,
            channel_chat_id,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}
