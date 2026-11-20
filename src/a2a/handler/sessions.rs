//! Read-side JSON-RPC methods the instance rail calls on a remote crab:
//! `session/list`, `session/transcript` and `tasks/list`.
//!
//! All three are `a2a:read`. None of them mutates: a paired window may look
//! at another crab's work, it may not change it. Sending stays behind
//! `a2a:send`.
//!
//! The wire shape is deliberately flat and boring. The rail draws a list of
//! sessions and a transcript; anything it does not draw is not sent, because
//! every field here is a field a remote peer gets to read.

use super::TaskStore;
use crate::a2a::types::*;
use crate::db::repository::SessionListOptions;
use crate::services::{MessageService, ServiceContext, SessionService};
use serde::Deserialize;

/// Sessions returned when the caller does not ask for a page size.
const DEFAULT_SESSION_LIMIT: usize = 50;
/// Upper bound on any page. A remote asking for "everything" gets a page,
/// not the database: an unbounded list is a memory spike on the host.
const MAX_LIMIT: usize = 200;
/// Transcript pages are message counts, and a transcript row can be large.
const DEFAULT_TRANSCRIPT_LIMIT: usize = 40;

fn clamp_limit(requested: Option<usize>, default: usize) -> usize {
    requested.unwrap_or(default).clamp(1, MAX_LIMIT)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ListSessionsParams {
    limit: Option<usize>,
    offset: Option<usize>,
    include_archived: bool,
    /// Case-insensitive substring match on the title.
    query: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TranscriptParams {
    session_id: String,
    limit: Option<usize>,
    /// Page backwards: only messages strictly older than this sequence.
    before_sequence: Option<i32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ListTasksParams {
    limit: Option<usize>,
    /// Only tasks created for this A2A context.
    context_id: Option<String>,
}

/// `session/list` — the rail's session column.
///
/// Returns the fields the strip draws plus the ones a human needs to tell two
/// similar titles apart: model, provider and the working directory's last
/// segment. Ordering is the repository's (most recently updated first), which
/// is what "what is this crab working on" means.
pub async fn handle_list_sessions(
    id: serde_json::Value,
    params: serde_json::Value,
    service_context: ServiceContext,
) -> JsonRpcResponse {
    let p: ListSessionsParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse::error(
                id,
                error_codes::INVALID_PARAMS,
                format!("Invalid params: {e}"),
            );
        }
    };

    let options = SessionListOptions {
        include_archived: p.include_archived,
        limit: Some(clamp_limit(p.limit, DEFAULT_SESSION_LIMIT)),
        offset: p.offset.unwrap_or(0),
        query: p.query,
        include_subagents: false,
    };

    let sessions = SessionService::new(service_context)
        .list_sessions(options)
        .await;

    match sessions {
        Ok(rows) => {
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id.to_string(),
                        "title": s.title.clone().unwrap_or_default(),
                        "model": s.model,
                        "provider": s.provider_name,
                        "updated_at": s.updated_at.to_rfc3339(),
                        "created_at": s.created_at.to_rfc3339(),
                        "archived": s.archived_at.is_some(),
                        "working_directory": s.working_directory,
                        "token_count": s.token_count,
                    })
                })
                .collect();
            JsonRpcResponse::success(id, serde_json::json!({ "sessions": items }))
        }
        Err(e) => JsonRpcResponse::error(
            id,
            error_codes::INTERNAL_ERROR,
            format!("Could not list sessions: {e}"),
        ),
    }
}

/// `session/transcript` — the panel's read-only view of one session.
///
/// Oldest-first within the page, like the local transcript, so the rail can
/// append without re-sorting. `before_sequence` pages backwards for a long
/// session; without it the newest page is returned.
pub async fn handle_transcript(
    id: serde_json::Value,
    params: serde_json::Value,
    service_context: ServiceContext,
) -> JsonRpcResponse {
    let p: TranscriptParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse::error(
                id,
                error_codes::INVALID_PARAMS,
                format!("Invalid params: {e}"),
            );
        }
    };

    let session_id = match uuid::Uuid::parse_str(&p.session_id) {
        Ok(u) => u,
        Err(_) => {
            return JsonRpcResponse::error(
                id,
                error_codes::INVALID_PARAMS,
                "session_id must be a UUID".to_string(),
            );
        }
    };

    let messages = MessageService::new(service_context.clone());
    let limit = clamp_limit(p.limit, DEFAULT_TRANSCRIPT_LIMIT);

    // A missing session and an empty one look the same on the wire, so check
    // the session exists first: "no such session" is a different thing to
    // tell the human than "this session has no messages yet".
    let known = SessionService::new(service_context)
        .get_session(session_id)
        .await;
    match known {
        Ok(Some(_)) => {}
        Ok(None) => {
            return JsonRpcResponse::error(
                id,
                error_codes::TASK_NOT_FOUND,
                format!("Session not found: {}", p.session_id),
            );
        }
        Err(e) => {
            return JsonRpcResponse::error(
                id,
                error_codes::INTERNAL_ERROR,
                format!("Could not read session: {e}"),
            );
        }
    }

    let rows = match p.before_sequence {
        Some(before) => {
            messages
                .list_messages_before_sequence(session_id, before, limit)
                .await
        }
        None => messages.list_messages_for_session(session_id).await,
    };

    match rows {
        Ok(mut rows) => {
            if p.before_sequence.is_none() && rows.len() > limit {
                // Keep the newest page, preserving oldest-first order.
                rows.drain(0..rows.len() - limit);
            }
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id.to_string(),
                        "role": m.role,
                        "content": m.content,
                        "sequence": m.sequence,
                        "created_at": m.created_at.to_rfc3339(),
                    })
                })
                .collect();
            JsonRpcResponse::success(
                id,
                serde_json::json!({
                    "session_id": p.session_id,
                    "messages": items,
                }),
            )
        }
        Err(e) => JsonRpcResponse::error(
            id,
            error_codes::INTERNAL_ERROR,
            format!("Could not read transcript: {e}"),
        ),
    }
}

/// `tasks/list` — the fallback when a crab has no session to show.
///
/// Newest first. This is what the rail uses when the host runs in a mode
/// without a session database (a pure A2A peer), so the strip still shows
/// something true instead of an empty column.
pub async fn handle_list_tasks(
    id: serde_json::Value,
    params: serde_json::Value,
    store: TaskStore,
) -> JsonRpcResponse {
    let p: ListTasksParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse::error(
                id,
                error_codes::INVALID_PARAMS,
                format!("Invalid params: {e}"),
            );
        }
    };

    let limit = clamp_limit(p.limit, DEFAULT_SESSION_LIMIT);
    let tasks = store.read().await;

    let mut rows: Vec<&Task> = tasks
        .values()
        .filter(|t| match &p.context_id {
            Some(ctx) => t.context_id.as_deref() == Some(ctx.as_str()),
            None => true,
        })
        .collect();

    rows.sort_by(|a, b| {
        let at = a.status.timestamp.as_deref().unwrap_or("");
        let bt = b.status.timestamp.as_deref().unwrap_or("");
        bt.cmp(at).then_with(|| b.id.cmp(&a.id))
    });
    rows.truncate(limit);

    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "context_id": t.context_id,
                "state": format!("{:?}", t.status.state),
                "timestamp": t.status.timestamp,
            })
        })
        .collect();

    JsonRpcResponse::success(id, serde_json::json!({ "tasks": items }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_size_is_clamped_to_the_ceiling() {
        assert_eq!(clamp_limit(None, 50), 50);
        assert_eq!(clamp_limit(Some(0), 50), 1);
        assert_eq!(clamp_limit(Some(10_000), 50), MAX_LIMIT);
        assert_eq!(clamp_limit(Some(12), 50), 12);
    }

    #[test]
    fn transcript_params_require_a_session_id() {
        let ok: Result<TranscriptParams, _> = serde_json::from_value(serde_json::json!({
            "session_id": "3f1a2b3c-0000-4000-8000-000000000000"
        }));
        assert!(ok.is_ok());
        let missing: Result<TranscriptParams, _> = serde_json::from_value(serde_json::json!({}));
        assert!(missing.is_err());
    }

    #[test]
    fn list_sessions_params_default_to_the_first_page() {
        let p: ListSessionsParams = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(p.limit, None);
        assert_eq!(p.offset, None);
        assert!(!p.include_archived);
        assert!(p.query.is_none());
    }

    #[test]
    fn list_tasks_params_accept_a_context_filter() {
        let p: ListTasksParams =
            serde_json::from_value(serde_json::json!({ "context_id": "abc", "limit": 5 })).unwrap();
        assert_eq!(p.context_id.as_deref(), Some("abc"));
        assert_eq!(p.limit, Some(5));
    }
}
