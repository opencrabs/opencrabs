//! The prompt bridge: one `session/prompt` request drives one full tool-loop
//! turn, translating in both directions —
//!
//! - loop `ProgressEvent`s -> outbound `session/update` notifications
//!   (message chunks, thought chunks, tool-call lifecycle, usage);
//! - approval-gated tools -> outbound `session/request_permission` calls,
//!   whose client answer becomes the loop's `(approved, always)` pair;
//! - `session/cancel` -> the turn's `CancellationToken`, surfacing as
//!   `stopReason: "cancelled"`.
//!
//! Constraint that shapes the code: `ProgressCallback` is a synchronous
//! `Fn`, so the tool-call-id pairing map is a plain `std::sync::Mutex` —
//! held for microseconds, never across an await. `ApprovalCallback` returns
//! a future, so the permission round-trip is free to await the client.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::brain::agent::{ApprovalCallback, ProgressCallback, ProgressEvent, ToolApprovalInfo};
use crate::brain::provider::StopReason;

use super::protocol::{self, permission_options, permission_outcome, session_update, text_chunk};
use super::server::{ServerState, SessionState};
use super::transport::TransportHandle;

/// Run one turn to completion and answer the `session/prompt` request.
/// The cancel token is pre-claimed by `session_prompt` under the session's
/// active_cancel lock; run_turn only consumes and releases it.
pub async fn run_turn(
    state: Arc<ServerState>,
    session: Arc<SessionState>,
    request_id: Value,
    text: String,
    cancel: CancellationToken,
) {
    let acp_session_id = session.id.to_string();

    let progress = progress_callback(
        state.handle.clone(),
        acp_session_id.clone(),
        state.agent.clone(),
    );
    let mode = *session.mode.lock().await;
    let approval = approval_callback(state.handle.clone(), acp_session_id, mode, cancel.clone());

    let model = session.model.lock().await.clone();
    let result = state
        .agent
        .send_message_with_tools_and_callback(
            session.id,
            text,
            model,
            Some(cancel.clone()),
            Some(approval),
            Some(progress),
            "acp",
            None,
            None,
        )
        .await;

    *session.active_cancel.lock().await = None;

    match result {
        Ok(response) => {
            let stop = if cancel.is_cancelled() {
                "cancelled"
            } else {
                stop_reason(response.stop_reason)
            };
            state
                .handle
                .respond(request_id, json!({ "stopReason": stop }));
        }
        Err(_) if cancel.is_cancelled() => {
            // A cancelled turn may surface as an error from the loop; the
            // client only cares that the turn ended by cancel.
            state
                .handle
                .respond(request_id, json!({ "stopReason": "cancelled" }));
        }
        Err(e) => {
            tracing::warn!("acp turn failed: {e:#}");
            state.handle.respond_error(
                request_id,
                protocol::INTERNAL_ERROR,
                crate::brain::agent::format_user_error(&e),
            );
        }
    }
}

/// ACP stop reasons from the provider's.
///
/// The output is constrained to the v1 `StopReason` enum: `end_turn`,
/// `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`. `stopReason` is
/// a required field on `PromptResponse`, so a value outside that list does not
/// degrade one frame, it hands a strict client grounds to reject the whole
/// prompt response (#1815 F2).
pub(crate) fn stop_reason(reason: Option<StopReason>) -> &'static str {
    match reason {
        Some(StopReason::MaxTokens) => "max_tokens",
        // A provider stop sequence is a normal completion of the turn: the
        // model produced what we asked and stopped where we told it to. v1
        // has no `stop_sequence` variant, and inventing one put an
        // out-of-enum value on a required field for every client.
        Some(StopReason::StopSequence) => "end_turn",
        // EndTurn/ToolUse/None: the turn completed — ToolUse means the loop
        // ended after tool execution, which from the client's seat is a
        // finished turn.
        _ => "end_turn",
    }
}

/// Whether an `IntermediateText` round aggregate repeats the text live
/// streaming already delivered this round. Whitespace is ignored on both
/// sides: providers disagree about trailing newlines between the delta
/// stream and the round summary. An empty aggregate is never a duplicate —
/// emitting it would only add noise.
pub(crate) fn round_text_is_duplicate(streamed: &str, aggregate: &str) -> bool {
    !aggregate.trim().is_empty() && streamed.trim() == aggregate.trim()
}

/// Map loop progress to `session/update` notifications.
///
/// Tool-call ids are invented here: `ProgressEvent` carries tool names but
/// no call ids, so each `ToolStarted` mints a UUID and the matching
/// `ToolCompleted` pops it FIFO per tool name. Parallel same-name tools pair
/// in start order, which is the only honest pairing the events support.
fn progress_callback(
    handle: TransportHandle,
    acp_session_id: String,
    agent: Arc<crate::brain::agent::AgentService>,
) -> ProgressCallback {
    let open_calls: Arc<StdMutex<HashMap<String, VecDeque<String>>>> =
        Arc::new(StdMutex::new(HashMap::new()));
    // Text streamed since the last round boundary. The loop emits live
    // deltas via `StreamingChunk` and then the round's full text again via
    // `IntermediateText` — both map to `agent_message_chunk`, so without a
    // guard every streamed answer renders twice on the client. Same rule as
    // the WhatsApp handler's pre-send dedup: skip the aggregate when it
    // repeats what streaming already delivered; emit it when it carries
    // text the stream did not (CLI providers stream nothing).
    let streamed: Arc<StdMutex<String>> = Arc::new(StdMutex::new(String::new()));
    // Sequence number for scheduled plan emissions. The plan document is
    // written by the `plan` tool and read back asynchronously, so two plan
    // operations in one turn could otherwise land out of order and repaint an
    // older checklist over a newer one (#1815 F6).
    let plan_seq = Arc::new(std::sync::atomic::AtomicU64::new(0));

    Arc::new(move |session_id, event| {
        let update = match event {
            ProgressEvent::StreamingChunk { text } => {
                if let Ok(mut buf) = streamed.lock() {
                    buf.push_str(&text);
                }
                Some(text_chunk("agent_message_chunk", &text))
            }
            ProgressEvent::IntermediateText { text, .. } => {
                let duplicate = streamed
                    .lock()
                    .map(|mut buf| {
                        let dup = round_text_is_duplicate(&buf, &text);
                        buf.clear();
                        dup
                    })
                    .unwrap_or(false);
                if duplicate {
                    None
                } else {
                    Some(text_chunk("agent_message_chunk", &text))
                }
            }
            ProgressEvent::ReasoningChunk { text } => {
                Some(text_chunk("agent_thought_chunk", &text))
            }
            ProgressEvent::ToolStarted {
                tool_name,
                tool_input,
                ..
            } => {
                // A tool call ends the text round; round 2's stream starts
                // fresh so its aggregate dedups against its own chunks.
                if let Ok(mut buf) = streamed.lock() {
                    buf.clear();
                }
                let call_id = Uuid::new_v4().to_string();
                if let Ok(mut calls) = open_calls.lock() {
                    calls
                        .entry(tool_name.clone())
                        .or_default()
                        .push_back(call_id.clone());
                }
                Some(tool_call_frame(&call_id, &tool_name, &tool_input))
            }
            ProgressEvent::ToolCompleted {
                tool_name,
                success,
                summary,
                ..
            } => {
                let call_id = open_calls
                    .lock()
                    .ok()
                    .and_then(|mut calls| calls.get_mut(&tool_name).and_then(VecDeque::pop_front))
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                // The plan checklist is not a `ProgressEvent`: no variant of
                // that enum carries it (verified against
                // `brain::agent::service::types::ProgressEvent`). Its state
                // lives in the session's plan document, which the `plan` tool
                // writes. So the honest source of a `plan` update is the tool
                // that owns the file, read back after a successful write
                // (#1815 F6).
                if tool_name == "plan" && success {
                    let handle = handle.clone();
                    let acp_id = acp_session_id.clone();
                    let seq = plan_seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    let plan_seq = plan_seq.clone();
                    tokio::spawn(async move {
                        // None means no live document (discarded, archived, or
                        // never created at this path): an empty `entries` is a
                        // legal full replacement and clears the client's card
                        // instead of leaving stale tasks on screen.
                        let doc = crate::utils::plan_files::load_plan(session_id).await;
                        // Only the newest scheduled read may send; a
                        // superseded snapshot is dropped, never queued behind.
                        if plan_seq.load(std::sync::atomic::Ordering::SeqCst) != seq {
                            return;
                        }
                        let entries = doc.as_ref().map(plan_entries).unwrap_or_default();
                        handle.send(session_update(&acp_id, protocol::plan_update(entries)));
                    });
                }
                Some(tool_call_update_frame(
                    &call_id, &tool_name, success, &summary,
                ))
            }
            ProgressEvent::TokenCount(used) => Some(protocol::usage_update(
                used as u64,
                agent.context_limit_for_session(session_id) as u64,
            )),
            // Everything else (compaction notices, retry ticker, provider
            // switches, suggestions, stream strips) has no ACP vocabulary —
            // logged by the loop already, not re-broadcast here.
            _ => None,
        };

        if let Some(update) = update {
            handle.send(session_update(&acp_session_id, update));
        }
    })
}

/// The `tool_call` frame announcing a tool that just started.
///
/// Both identity fields are sent on purpose (#1815 F6). v1 `ToolCall` requires
/// `toolCallId` and `title` and carries an optional stable `name`: `title` is
/// the label a client displays, `name` is what a client keys on to recognize
/// the tool. Sending only the title left clients matching a card to a tool
/// guessing from display text.
pub(crate) fn tool_call_frame(call_id: &str, tool_name: &str, tool_input: &Value) -> Value {
    json!({
        "sessionUpdate": "tool_call",
        "toolCallId": call_id,
        "title": tool_name,
        "name": tool_name,
        "kind": protocol::tool_kind(tool_name),
        "status": "in_progress",
        "rawInput": tool_input,
    })
}

/// The `tool_call_update` frame closing a tool call, same identity fields as
/// the start frame so a client can pair them without re-deriving the tool
/// (#1815 F6). `status` is a v1 `ToolCallStatus`: `completed` or `failed`.
pub(crate) fn tool_call_update_frame(
    call_id: &str,
    tool_name: &str,
    success: bool,
    summary: &str,
) -> Value {
    json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": call_id,
        "name": tool_name,
        "status": if success { "completed" } else { "failed" },
        "rawOutput": summary,
    })
}

/// ACP `PlanEntry` objects for a stored plan document (#1815 F6).
///
/// The plan vocabulary and ours do not line up exactly, and the gaps are
/// stated instead of hidden:
///
/// - v1 `PlanEntry.status` allows only `pending`, `in_progress` and
///   `completed`. Our `TaskStatus` also has `Skipped`, `Failed` and
///   `Blocked(reason)`, which collapse to `pending` in the required field and
///   keep their true label in `_meta.status`, the one place the spec lets an
///   implementation add information without inventing reserved vocabulary. A
///   client that reads only `status` sees "not done"; one that reads `_meta`
///   sees why.
/// - v1 `priority` means importance. The plan tool carries no importance
///   field, only `complexity` (1-5, effort). It is mapped as the closest
///   signal and the raw number is kept in `_meta.complexity`, so the
///   approximation is visible rather than presented as a fact.
/// - `content` asks for "what this task aims to accomplish": the title is that
///   sentence, so the title wins and the description is the fallback.
pub(crate) fn plan_entries(doc: &crate::tui::plan::PlanDocument) -> Vec<Value> {
    doc.tasks
        .iter()
        .map(|task| {
            let status = match task.status {
                crate::tui::plan::TaskStatus::Completed => "completed",
                crate::tui::plan::TaskStatus::InProgress => "in_progress",
                // Pending, Skipped, Failed, Blocked: not done, in v1's
                // three-value vocabulary.
                _ => "pending",
            };
            let priority = match task.complexity {
                4..=u8::MAX => "high",
                3 => "medium",
                _ => "low",
            };
            json!({
                "content": if task.title.is_empty() {
                    &task.description
                } else {
                    &task.title
                },
                "priority": priority,
                "status": status,
                "_meta": {
                    "order": task.order,
                    "taskType": task.task_type.to_string(),
                    "status": task.status.to_string(),
                    "complexity": task.complexity,
                },
            })
        })
        .collect()
}

/// Backstop for a client that drops `session/request_permission` without
/// answering: generous enough for a human reading the prompt, finite enough
/// that a wedged client cannot stall the agent forever.
const PERMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Route approval-gated tools according to the session's mode. Only
/// `supervised` (and non-edit kinds under `auto-accept-edits`) reach the
/// client as `session/request_permission`; `plan` denies mutations outright
/// so the agent learns the boundary from the denial instead of a silent
/// client-side veto. Any transport failure denies — a client that cannot
/// answer must never become an approval. The round-trip also races
/// `session/cancel` and the permission timeout, both resolving to a deny.
fn approval_callback(
    handle: TransportHandle,
    acp_session_id: String,
    mode: protocol::AcpMode,
    cancel: CancellationToken,
) -> ApprovalCallback {
    Arc::new(move |info: ToolApprovalInfo| {
        let handle = handle.clone();
        let acp_session_id = acp_session_id.clone();
        let cancel = cancel.clone();
        Box::pin(async move {
            let kind = protocol::tool_kind(&info.tool_name);
            match mode {
                protocol::AcpMode::Plan => {
                    tracing::info!("acp plan mode: denied {} without asking", info.tool_name);
                    return Ok((false, false));
                }
                protocol::AcpMode::Auto | protocol::AcpMode::FullAccess => {
                    return Ok((true, false));
                }
                protocol::AcpMode::AutoAcceptEdits if kind == "edit" => {
                    return Ok((true, false));
                }
                _ => {}
            }
            let params = json!({
                "sessionId": acp_session_id,
                "toolCall": {
                    "toolCallId": Uuid::new_v4().to_string(),
                    "title": info.tool_name.clone(),
                    // Same `name` field as the streamed tool_call frames: the
                    // permission request carries a `ToolCall` too, and a client
                    // matching an approval back to a tool needs the
                    // machine-readable identity, not just the label (#1815 F6).
                    "name": info.tool_name,
                    "kind": kind,
                    "rawInput": info.tool_input,
                },
                "options": permission_options(),
            });
            let call = handle.call(protocol::SESSION_REQUEST_PERMISSION, params);
            let outcome = tokio::select! {
                res = call => res,
                _ = cancel.cancelled() => {
                    Err(anyhow::anyhow!("turn cancelled before the client answered"))
                }
                _ = tokio::time::sleep(PERMISSION_TIMEOUT) => {
                    Err(anyhow::anyhow!(
                        "no permission response within {PERMISSION_TIMEOUT:?}, denying"
                    ))
                }
            };
            match outcome {
                Ok(result) => Ok(permission_outcome(&result)),
                Err(e) => {
                    tracing::warn!("acp permission request failed, denying: {e:#}");
                    Ok((false, false))
                }
            }
        })
    })
}
