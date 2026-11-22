//! SQLite persistence for A2A tasks.
//!
//! Tasks are stored as JSON blobs alongside indexed state/timestamps
//! so they survive server restarts.

use super::types::Task;
use crate::db::{Pool, interact_err};
use rusqlite::params;
use uuid::Uuid;

/// Save or update a task in the database.
pub async fn upsert_task(pool: &Pool, task: &Task) {
    let now = chrono::Utc::now().timestamp();
    let state = format!("{:?}", task.status.state).to_lowercase();
    let data = match serde_json::to_string(task) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(
                "A2A persistence: failed to serialize task {}: {}",
                task.id,
                e
            );
            return;
        }
    };

    let task_id = task.id.clone();
    let context_id = task.context_id.clone();

    let result = match pool.get().await {
        Ok(conn) => conn
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO a2a_tasks (id, context_id, state, data, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                     ON CONFLICT(id) DO UPDATE SET state = ?3, data = ?4, updated_at = ?5",
                    params![task_id, context_id, state, data, now],
                )
            })
            .await
            .map_err(interact_err),
        Err(e) => Err(anyhow::anyhow!("Failed to get connection: {}", e)),
    };

    if let Err(e) = result {
        tracing::error!("A2A persistence: failed to upsert task {}: {}", task.id, e);
    }
}

/// Load all non-terminal tasks from the database (for warm-start after restart).
pub async fn load_active_tasks(pool: &Pool) -> Vec<Task> {
    let rows = match pool.get().await {
        Ok(conn) => match conn
            .interact(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT data FROM a2a_tasks WHERE state NOT IN ('completed', 'failed', 'canceled')",
                )?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::error!("A2A persistence: failed to load active tasks: {}", e);
                return vec![];
            }
            Err(e) => {
                tracing::error!("A2A persistence: interact error: {}", e);
                return vec![];
            }
        },
        Err(e) => {
            tracing::error!("A2A persistence: failed to get connection: {}", e);
            return vec![];
        }
    };

    rows.iter()
        .filter_map(|data| {
            serde_json::from_str::<Task>(data)
                .inspect_err(|e| tracing::warn!("A2A persistence: bad task JSON: {}", e))
                .ok()
        })
        .collect()
}

/// Return the chat session bound to an A2A context, if one still exists.
///
/// The JOIN against `sessions` makes stale mappings self-heal (#1159): if the
/// session was deleted or archived since the mapping was written, no row
/// comes back and the caller creates a fresh session instead.
pub async fn lookup_session_for_context(pool: &Pool, context_id: &str) -> Option<Uuid> {
    let ctx = context_id.to_string();
    let conn = match pool.get().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("A2A persistence: failed to get connection: {}", e);
            return None;
        }
    };

    let result = conn
        .interact(move |conn| {
            conn.query_row(
                "SELECT m.session_id FROM a2a_context_sessions m
                 JOIN sessions s ON s.id = m.session_id
                 WHERE m.context_id = ?1 AND s.archived_at IS NULL",
                params![ctx],
                |row| row.get::<_, String>(0),
            )
        })
        .await;

    match result {
        Ok(Ok(id)) => {
            // corrupt mapping (non-UUID session_id): self-heal via fresh session
            Uuid::parse_str(&id).ok()
        }
        Ok(Err(rusqlite::Error::QueryReturnedNoRows)) => None,
        Ok(Err(e)) => {
            tracing::warn!("A2A persistence: context session lookup failed: {}", e);
            None
        }
        Err(e) => {
            tracing::warn!("A2A persistence: interact error on context lookup: {}", e);
            None
        }
    }
}

/// Bind an A2A context to a chat session (upsert).
pub async fn save_context_session(pool: &Pool, context_id: &str, session_id: Uuid) {
    let now = chrono::Utc::now().timestamp();
    let ctx = context_id.to_string();
    let sid = session_id.to_string();

    let result = match pool.get().await {
        Ok(conn) => conn
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO a2a_context_sessions (context_id, session_id, updated_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(context_id) DO UPDATE SET session_id = ?2, updated_at = ?3",
                    params![ctx, sid, now],
                )
            })
            .await
            .map_err(interact_err),
        Err(e) => Err(anyhow::anyhow!("Failed to get connection: {}", e)),
    };

    if let Err(e) = result {
        tracing::error!(
            "A2A persistence: failed to save context session for {}: {}",
            context_id,
            e
        );
    }
}

/// How long a caller-minted idempotency key is remembered, seconds (7 days).
/// Long enough to cover a retry queue that outlives a daemon restart, short
/// enough that the table cannot grow without bound.
const IDEMPOTENCY_TTL_SECS: i64 = 7 * 86_400;

/// Claim `key` for `task_id` before dispatching a send (#2874).
///
/// `None` means the caller owns the key and must dispatch. `Some(existing)`
/// means an earlier attempt already claimed it and the task named there is
/// the answer to return, so a resend never runs the agent twice.
///
/// A storage failure returns `None` (dispatch anyway). The delivery contract
/// here is at-least-once: a database hiccup must not turn into a silently
/// dropped message.
pub async fn claim_idempotency_key(pool: &Pool, key: &str, task_id: &str) -> Option<String> {
    use rusqlite::OptionalExtension as _;

    let now = chrono::Utc::now().timestamp();
    let key_owned = key.to_string();
    let task_owned = task_id.to_string();

    let result = match pool.get().await {
        Ok(conn) => conn
            .interact(move |conn| -> rusqlite::Result<Option<String>> {
                // Opportunistic sweep: a key only matters while a caller
                // might still retry the send it belongs to.
                conn.execute(
                    "DELETE FROM a2a_idempotency WHERE created_at < ?1",
                    params![now - IDEMPOTENCY_TTL_SECS],
                )?;
                let inserted = conn.execute(
                    "INSERT OR IGNORE INTO a2a_idempotency (key, task_id, created_at)
                     VALUES (?1, ?2, ?3)",
                    params![key_owned, task_owned, now],
                )?;
                if inserted == 1 {
                    return Ok(None);
                }
                conn.query_row(
                    "SELECT task_id FROM a2a_idempotency WHERE key = ?1",
                    params![key_owned],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(interact_err),
        Err(e) => Err(anyhow::anyhow!("Failed to get connection: {}", e)),
    };

    match result {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!("A2A persistence: idempotency claim failed for {key:?}: {e}");
            None
        }
    }
}

/// Point `key` at a new task id (#2874).
///
/// A claim can outlive the task it names: the process can die between the
/// claim and the insert. Without this, that key would answer every later
/// resend with a missing task and the send could never succeed.
pub async fn repoint_idempotency_key(pool: &Pool, key: &str, task_id: &str) {
    let now = chrono::Utc::now().timestamp();
    let key_owned = key.to_string();
    let task_owned = task_id.to_string();

    let result = match pool.get().await {
        Ok(conn) => conn
            .interact(move |conn| {
                conn.execute(
                    "UPDATE a2a_idempotency SET task_id = ?2, created_at = ?3 WHERE key = ?1",
                    params![key_owned, task_owned, now],
                )
            })
            .await
            .map_err(interact_err),
        Err(e) => Err(anyhow::anyhow!("Failed to get connection: {}", e)),
    };

    if let Err(e) = result {
        tracing::warn!("A2A persistence: idempotency repoint failed for {key:?}: {e}");
    }
}

/// Load one task by id. Used when a resend matches a claimed idempotency key
/// and the task it points at is no longer in the in-memory store (#2874).
pub async fn load_task(pool: &Pool, task_id: &str) -> Option<Task> {
    use rusqlite::OptionalExtension as _;

    let id = task_id.to_string();
    let result = match pool.get().await {
        Ok(conn) => conn
            .interact(move |conn| {
                conn.query_row(
                    "SELECT data FROM a2a_tasks WHERE id = ?1",
                    params![id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(interact_err),
        Err(e) => Err(anyhow::anyhow!("Failed to get connection: {}", e)),
    };

    match result {
        Ok(Some(json)) => match serde_json::from_str::<Task>(&json) {
            Ok(task) => Some(task),
            Err(e) => {
                tracing::warn!("A2A persistence: task {task_id} unreadable: {e}");
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            tracing::warn!("A2A persistence: load task {task_id} failed: {e}");
            None
        }
    }
}
