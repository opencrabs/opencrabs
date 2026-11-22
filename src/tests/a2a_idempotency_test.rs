//! Caller-side retry contract for `message/send` (#2874).
//!
//! A relay-free gateway keeps no spool for a caller it cannot reach (#2274),
//! so the retry state lives on the caller. A resend carrying the same
//! `messageId` must answer with the task the first attempt created instead of
//! running the agent twice, and a send that carries no `messageId` must keep
//! behaving exactly as it did before.

use crate::a2a::handler::{handle_send_message, new_cancel_store, new_task_store};
use crate::a2a::test_helpers::helpers;
use crate::brain::agent::service::AgentService;
use crate::brain::provider::PlaceholderProvider;
use crate::services::ServiceContext;
use std::sync::Arc;

async fn agent_service(ctx: &ServiceContext) -> Arc<AgentService> {
    let provider = Arc::new(PlaceholderProvider);
    let config = crate::config::Config::default();
    Arc::new(AgentService::new(provider, ctx.clone(), &config).await)
}

fn send_params(message_id: Option<&str>) -> serde_json::Value {
    let mut message = serde_json::json!({
        "role": "user",
        "parts": [{"text": "hello there"}],
        "contextId": "ctx-idempotency"
    });
    if let Some(id) = message_id {
        message["messageId"] = serde_json::json!(id);
    }
    serde_json::json!({ "message": message })
}

fn task_id_of(resp: &crate::a2a::types::JsonRpcResponse) -> String {
    resp.result
        .as_ref()
        .and_then(|r| r["id"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no task id in response: {:?}", resp.error))
}

#[tokio::test]
async fn a_duplicate_message_id_answers_with_the_first_task() {
    let ctx = helpers::placeholder_service_context().await;
    let agent = agent_service(&ctx).await;
    let store = new_task_store();
    let cancel = new_cancel_store();

    let first = handle_send_message(
        serde_json::json!(1),
        send_params(Some("dup-key")),
        store.clone(),
        cancel.clone(),
        agent.clone(),
        ctx.clone(),
    )
    .await;
    let first_id = task_id_of(&first);

    let second = handle_send_message(
        serde_json::json!(2),
        send_params(Some("dup-key")),
        store.clone(),
        cancel.clone(),
        agent.clone(),
        ctx.clone(),
    )
    .await;

    assert!(
        second.error.is_none(),
        "a resend must not fail: {:?}",
        second.error
    );
    assert_eq!(
        task_id_of(&second),
        first_id,
        "a resend must answer with the task the first attempt created"
    );
    assert_eq!(
        store.read().await.len(),
        1,
        "a resend must not create a second task"
    );
}

#[tokio::test]
async fn a_send_without_a_message_id_creates_a_task_each_time() {
    // Callers that never send a messageId keep today's behaviour: no key, no
    // dedupe, one task per send.
    let ctx = helpers::placeholder_service_context().await;
    let agent = agent_service(&ctx).await;
    let store = new_task_store();
    let cancel = new_cancel_store();

    let first = handle_send_message(
        serde_json::json!(1),
        send_params(None),
        store.clone(),
        cancel.clone(),
        agent.clone(),
        ctx.clone(),
    )
    .await;
    let second = handle_send_message(
        serde_json::json!(2),
        send_params(None),
        store.clone(),
        cancel.clone(),
        agent.clone(),
        ctx.clone(),
    )
    .await;

    assert_ne!(task_id_of(&first), task_id_of(&second));
    assert_eq!(store.read().await.len(), 2);
}

#[tokio::test]
async fn distinct_message_ids_are_distinct_sends() {
    let ctx = helpers::placeholder_service_context().await;
    let agent = agent_service(&ctx).await;
    let store = new_task_store();
    let cancel = new_cancel_store();

    let first = handle_send_message(
        serde_json::json!(1),
        send_params(Some("key-a")),
        store.clone(),
        cancel.clone(),
        agent.clone(),
        ctx.clone(),
    )
    .await;
    let second = handle_send_message(
        serde_json::json!(2),
        send_params(Some("key-b")),
        store.clone(),
        cancel.clone(),
        agent.clone(),
        ctx.clone(),
    )
    .await;

    assert_ne!(task_id_of(&first), task_id_of(&second));
    assert_eq!(store.read().await.len(), 2);
}
