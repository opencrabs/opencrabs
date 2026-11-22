//! Handler for `message/send` — creates a task and processes it via AgentService.

use super::{CancelStore, TaskStore};
use crate::a2a::{persistence, types::*};
use crate::brain::agent::service::AgentService;
use crate::services::ServiceContext;
use crate::services::SessionService;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Handle `message/send` — create a task and spawn background processing.
pub async fn handle_send_message(
    id: serde_json::Value,
    params: serde_json::Value,
    store: TaskStore,
    cancel_store: CancelStore,
    agent_service: Arc<AgentService>,
    service_context: ServiceContext,
) -> JsonRpcResponse {
    let send_params: SendMessageParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse::error(
                id,
                error_codes::INVALID_PARAMS,
                format!("Invalid params: {}", e),
            );
        }
    };

    let user_text = send_params
        .message
        .parts
        .iter()
        .filter_map(|p| p.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");

    if user_text.trim().is_empty() {
        return JsonRpcResponse::error(
            id,
            error_codes::INVALID_PARAMS,
            "Message must contain at least one text part",
        );
    }

    let task_id = Uuid::new_v4().to_string();

    // Caller-side retry contract (#2874): a resend carrying the same
    // caller-minted `messageId` is the SAME send, so it answers with the task
    // the first attempt created instead of running the agent twice. A send
    // without a messageId cannot be retried safely and behaves as it always
    // has, so older callers keep working.
    let idem_key = send_params
        .message
        .message_id
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty());
    if let Some(key) = idem_key
        && let Some(existing_id) =
            persistence::claim_idempotency_key(&service_context.pool(), key, &task_id).await
    {
        let cached = {
            let tasks = store.read().await;
            tasks.get(&existing_id).cloned()
        };
        let existing = match cached {
            Some(task) => Some(task),
            None => persistence::load_task(&service_context.pool(), &existing_id).await,
        };
        match existing {
            Some(task) => {
                tracing::info!(
                    "A2A: duplicate send (messageId {key}) answered with task {existing_id}"
                );
                let json = serde_json::to_value(&task)
                    .unwrap_or_else(|_| serde_json::json!({"id": existing_id.as_str()}));
                return JsonRpcResponse::success(id, json);
            }
            None => {
                tracing::warn!(
                    "A2A: messageId {key} maps to task {existing_id}, which is gone; \
                     treating this send as new"
                );
                // Re-point the claim, or every later resend of this key would
                // hit the same missing task and never deliver.
                persistence::repoint_idempotency_key(&service_context.pool(), key, &task_id).await;
            }
        }
    }

    let context_id = send_params
        .message
        .context_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let task = Task {
        id: task_id.clone(),
        context_id: Some(context_id.clone()),
        status: TaskStatus {
            state: TaskState::Working,
            message: Some(Message {
                message_id: Some(Uuid::new_v4().to_string()),
                context_id: Some(context_id.clone()),
                task_id: Some(task_id.clone()),
                role: Role::Agent,
                parts: vec![Part::text(format!(
                    "Task created. Processing: {}",
                    if user_text.len() > 100 {
                        format!("{}...", &user_text[..user_text.floor_char_boundary(100)])
                    } else {
                        user_text.clone()
                    }
                ))],
                metadata: None,
            }),
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
        },
        artifacts: vec![],
        history: vec![send_params.message.clone()],
        metadata: None,
    };

    {
        let mut tasks = store.write().await;
        tasks.insert(task_id.clone(), task.clone());
    }
    persistence::upsert_task(&service_context.pool(), &task).await;

    tracing::info!("A2A: Task {} created, spawning agent", task_id);

    let bg_store = store.clone();
    let bg_cancel_store = cancel_store.clone();
    let bg_task_id = task_id.clone();
    let bg_context_id = context_id.clone();
    let bg_pool = service_context.pool();
    tokio::spawn(async move {
        process_task(
            bg_store,
            bg_cancel_store,
            bg_task_id,
            bg_context_id,
            user_text,
            agent_service,
            service_context,
            bg_pool,
        )
        .await;
    });

    let task_json =
        serde_json::to_value(&task).unwrap_or_else(|_| serde_json::json!({"error": "serialize"}));
    JsonRpcResponse::success(id, task_json)
}

/// Background task processor: creates a DB session, invokes the agent, updates the A2A task.
#[allow(clippy::too_many_arguments)]
async fn process_task(
    store: TaskStore,
    cancel_store: CancelStore,
    task_id: String,
    context_id: String,
    user_text: String,
    agent_service: Arc<AgentService>,
    service_context: ServiceContext,
    pool: crate::db::Pool,
) {
    let session_service = SessionService::new(service_context);
    let title = format!(
        "A2A: {}",
        &user_text[..user_text.floor_char_boundary(60.min(user_text.len()))]
    );
    // Conversation continuity (#1159): tasks sharing a context_id continue
    // the same chat session instead of forking a fresh one per task.
    let session_id = match persistence::lookup_session_for_context(&pool, &context_id).await {
        Some(existing) => existing,
        None => match session_service.create_session(Some(title)).await {
            Ok(session) => {
                persistence::save_context_session(&pool, &context_id, session.id).await;
                session.id
            }
            Err(e) => {
                tracing::error!("A2A: Failed to create session for task {}: {}", task_id, e);
                update_task_failed(
                    &store,
                    &task_id,
                    &context_id,
                    &format!("Session creation failed: {}", e),
                    &pool,
                )
                .await;
                return;
            }
        },
    };

    let cancel_token = CancellationToken::new();
    {
        let mut tokens = cancel_store.write().await;
        tokens.insert(task_id.clone(), cancel_token.clone());
    }

    // Build approval callback that respects config-level approval policy.
    // A2A has no interactive UI, so if policy is not auto-always/auto-session,
    // tools are denied (no human to ask).
    let approval_cb: crate::brain::agent::ApprovalCallback = Arc::new(move |_info| {
        Box::pin(async move {
            match crate::utils::check_approval_policy() {
                Some(result) => Ok(result),
                None => {
                    tracing::warn!(
                        "A2A: Tool requires approval but no interactive UI available and policy is not auto-always"
                    );
                    Ok((false, false))
                }
            }
        })
    });

    let result = agent_service
        .send_message_with_tools_and_callback(
            session_id,
            user_text,
            None,
            Some(cancel_token),
            Some(approval_cb),
            None,
            "a2a",
            None,
            None,
        )
        .await;

    // Clean up cancel token
    {
        let mut tokens = cancel_store.write().await;
        tokens.remove(&task_id);
    }

    match result {
        Ok(response) => {
            let mut tasks = store.write().await;
            if let Some(task) = tasks.get_mut(&task_id) {
                task.status = TaskStatus {
                    state: TaskState::Completed,
                    message: Some(Message {
                        message_id: Some(Uuid::new_v4().to_string()),
                        context_id: Some(context_id),
                        task_id: Some(task_id.clone()),
                        role: Role::Agent,
                        parts: vec![Part::text("Task completed.")],
                        metadata: None,
                    }),
                    timestamp: Some(chrono::Utc::now().to_rfc3339()),
                };
                task.artifacts.push(Artifact {
                    artifact_id: Some(Uuid::new_v4().to_string()),
                    name: Some("response".to_string()),
                    description: Some("Agent response".to_string()),
                    parts: vec![Part::text(&response.content)],
                    metadata: None,
                });
            }
            if let Some(task) = tasks.get(&task_id) {
                persistence::upsert_task(&pool, task).await;
            }
            tracing::info!(
                "A2A: Task {} completed ({} tokens used)",
                task_id,
                response.usage.input_tokens + response.usage.output_tokens
            );
        }
        Err(e) => {
            tracing::error!("A2A: Task {} failed: {}", task_id, e);
            update_task_failed(&store, &task_id, &context_id, &e.to_string(), &pool).await;
        }
    }
}

/// Mark a task as failed in the store and persist to DB.
async fn update_task_failed(
    store: &TaskStore,
    task_id: &str,
    context_id: &str,
    error_msg: &str,
    pool: &crate::db::Pool,
) {
    let mut tasks = store.write().await;
    if let Some(task) = tasks.get_mut(task_id) {
        task.status = TaskStatus {
            state: TaskState::Failed,
            message: Some(Message {
                message_id: Some(Uuid::new_v4().to_string()),
                context_id: Some(context_id.to_string()),
                task_id: Some(task_id.to_string()),
                role: Role::Agent,
                parts: vec![Part::text(format!("Task failed: {}", error_msg))],
                metadata: None,
            }),
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
        };
        persistence::upsert_task(pool, task).await;
    }
}
