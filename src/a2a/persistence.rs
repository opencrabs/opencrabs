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

/// The A2A context a chat session was created under, if it has one.
///
/// The reverse of [`lookup_session_for_context`], and the reason a remote
/// composer can answer the conversation it is reading. `message/send` binds a
/// task to a session through `context_id`; a client that sends without one gets
/// a fresh context, and a fresh context is a fresh session on the far side, so
/// the message never lands in the transcript on screen (#3133). A session this
/// crab started locally has no context and answers `None`, which is correct:
/// the first send to it opens one, and the answer carries it back.
pub async fn lookup_context_for_session(pool: &Pool, session_id: Uuid) -> Option<String> {
    let sid = session_id.to_string();
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
                "SELECT context_id FROM a2a_context_sessions
                 WHERE session_id = ?1
                 ORDER BY updated_at DESC LIMIT 1",
                params![sid],
                |row| row.get::<_, String>(0),
            )
        })
        .await;

    match result {
        Ok(Ok(ctx)) => Some(ctx),
        Ok(Err(rusqlite::Error::QueryReturnedNoRows)) => None,
        Ok(Err(e)) => {
            tracing::warn!("A2A persistence: session context lookup failed: {}", e);
            None
        }
        Err(e) => {
            tracing::warn!("A2A persistence: interact error on session lookup: {}", e);
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
