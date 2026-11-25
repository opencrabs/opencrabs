//! JSON-RPC method routing: one arm per A2A method, unknown methods answer
//! `METHOD_NOT_FOUND`.

use super::stores::{CancelStore, TaskStore};
use super::{notify, send, sessions, tasks};
use crate::a2a::types::*;
use crate::brain::agent::service::AgentService;
use crate::services::ServiceContext;
use std::sync::Arc;

/// Dispatch a JSON-RPC request to the appropriate handler.
pub async fn dispatch(
    req: JsonRpcRequest,
    store: TaskStore,
    cancel_store: CancelStore,
    agent_service: Arc<AgentService>,
    service_context: ServiceContext,
) -> JsonRpcResponse {
    match req.method.as_str() {
        "message/send" => {
            send::handle_send_message(
                req.id,
                req.params,
                store,
                cancel_store,
                agent_service,
                service_context,
            )
            .await
        }
        "session/notify" => {
            // #1802: the kill switch rides the config mirror, not the
            // request, so a caller cannot talk a disabled gateway into it.
            let enabled = crate::config::Config::current()
                .agent
                .session_notify_enabled;
            notify::handle_session_notify(req.id, req.params, service_context, enabled).await
        }
        "session/notify-status" => notify::handle_notify_status(req.id, req.params),
        "session/list" => sessions::handle_list_sessions(req.id, req.params, service_context).await,
        "session/transcript" => {
            sessions::handle_transcript(req.id, req.params, service_context).await
        }
        "tasks/list" => sessions::handle_list_tasks(req.id, req.params, store).await,
        "tasks/get" => tasks::handle_get_task(req.id, req.params, store).await,
        "tasks/cancel" => {
            tasks::handle_cancel_task(
                req.id,
                req.params,
                store,
                cancel_store,
                &service_context.pool(),
            )
            .await
        }
        _ => JsonRpcResponse::error(
            req.id,
            error_codes::METHOD_NOT_FOUND,
            format!("Method not found: {}", req.method),
        ),
    }
}
