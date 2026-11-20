//! Background-task resume producer for WhatsApp (#731).
//!
//! Mirrors Telegram's `build_enqueue_callback`: when a detached long command
//! finishes, resume the originating session and send the result to its chat.
//! The session→JID map is populated per turn in the handler (`register_session_jid`).
//!
//! The resumed turn shows a live tool block (#2023), the same shape a typed
//! message gets on the live path. The block opens before the agent runs so
//! its clock covers the thinking, and the answer lands after it settles.

use super::WhatsAppState;
use super::flow_message::{self, FlowConfig};
use super::flow_render::Phase;
use crate::brain::agent::AgentService;
use crate::brain::agent::service::MessageEnqueueCallback;
use crate::channels::bg_resume::{self, AgentHolder};
use std::sync::Arc;
use uuid::Uuid;

pub(crate) fn build_enqueue_callback(
    state: Arc<WhatsAppState>,
    agent_holder: AgentHolder,
    wa_cfg: crate::config::types::WhatsAppConfig,
) -> MessageEnqueueCallback {
    Arc::new(move |session_id, msg| {
        let state = state.clone();
        let agent_holder = agent_holder.clone();
        let wa_cfg = wa_cfg.clone();
        tokio::spawn(async move {
            let Some(jid_str) = state.session_jid(session_id).await else {
                tracing::warn!(
                    "[bg-resume] whatsapp: no chat jid for session {session_id}; dropping"
                );
                return;
            };
            // #1989: the in-turn tail loop stops when the task is marked
            // finished, which happens BEFORE this resume turn runs, so the
            // chat sat silent through the slowest message of the whole
            // cycle. Composing every 5 s across the turn and the delivery,
            // first ping immediately; ticks without a client are skipped.
            let typing = tokio::spawn({
                let state = state.clone();
                let jid_str = jid_str.clone();
                async move {
                    if let Err(e) = send_composing_now(&state, &jid_str).await {
                        tracing::debug!(error = %e, "whatsapp resume: first composing skipped");
                    }
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        if let Err(e) = send_composing_now(&state, &jid_str).await {
                            tracing::debug!(error = %e, "whatsapp resume: composing skipped");
                        }
                    }
                }
            });
            let resume_flow = async {
                let Some(agent) = bg_resume::upgrade(&agent_holder) else {
                    tracing::warn!("[bg-resume] whatsapp: agent gone; dropping resume");
                    return;
                };
                // Transport before the turn: the live block posts through the
                // client, so the bounded wait (#1242) runs first, as on Slack
                // and Discord.
                let Some(client) = crate::channels::transport_ready::await_transport(
                    "whatsapp",
                    session_id,
                    || state.client(),
                )
                .await
                else {
                    return;
                };
                let Ok(jid) = jid_str.parse::<wacore_binary::jid::Jid>() else {
                    tracing::warn!("[bg-resume] whatsapp: bad jid '{jid_str}'; dropping delivery");
                    return;
                };
                let rl_owner = wa_cfg.is_owner(jid_str.split('@').next().unwrap_or(&jid_str));
                let posted = Arc::new(tokio::sync::Mutex::new(Vec::new()));
                let (flow, flow_task) = flow_message::spawn(FlowConfig {
                    client: client.clone(),
                    jid: jid.clone(),
                    state: state.clone(),
                    rate_limit: wa_cfg.rate_limit.clone(),
                    is_owner: rl_owner,
                    interactive_buttons: wa_cfg.interactive_buttons,
                    // Eager here (#2609): a background completion has no
                    // other sign of life, so the clock covers the thinking.
                    wait_for_first_tool: false,
                    posted: posted.clone(),
                });
                let outcome = run_flow_turn(
                    agent,
                    session_id,
                    msg.context_text,
                    &jid_str,
                    flow.progress_callback(),
                )
                .await;
                flow.settle(match outcome {
                    Ok(_) => Phase::Finished,
                    Err(_) => Phase::Failed,
                });
                if let Err(e) = flow_task.await {
                    tracing::warn!("[bg-resume] whatsapp: live tool block task failed: {e}");
                }
                let Ok(content) = outcome else {
                    return;
                };
                // The answer a halted turn already posted before its
                // suggestions is not repeated from the sign-off (#2030).
                let held = posted.lock().await.clone();
                let content = super::flow_render::drop_held(&content, &held);
                if content.trim().is_empty() {
                    return;
                }
                // #1407: bg-resume results are agent-output sends: gate them
                // through the shared limiter. Over-budget sends park in the
                // FIFO queue (the drainer flushes and persists them as the
                // rolling 24h window slides); owner-bound resumes bypass.
                match state
                    .rate_limiter
                    .gate(&wa_cfg.rate_limit, &jid_str, &content, rl_owner)
                    .await
                {
                    super::rate_limit::GateOutcome::Queued { .. } => {
                        tracing::info!(
                            "[bg-resume] whatsapp: daily cap reached; result queued for drainer flush"
                        );
                        return;
                    }
                    super::rate_limit::GateOutcome::SendNow => {}
                }
                let out = waproto::whatsapp::Message {
                    conversation: Some(content),
                    ..Default::default()
                };
                if let Err(e) = client.send_message(jid, out).await {
                    tracing::warn!("[bg-resume] whatsapp: send_message failed: {e}");
                }
            };
            resume_flow.await;
            // Reached on every exit of the flow, including its early returns:
            // the indicator lives exactly as long as the resume work does.
            typing.abort();
        });
    })
}

/// Run the background-completion turn for `session_id` with the live block's
/// progress callback attached. Returns the answer text, or the error text so
/// the block can settle as failed.
async fn run_flow_turn(
    agent: Arc<AgentService>,
    session_id: Uuid,
    context_text: String,
    target: &str,
    progress: crate::brain::agent::ProgressCallback,
) -> Result<String, String> {
    match agent
        .send_push_turn(
            session_id,
            context_text,
            None,
            None,
            None,
            Some(progress),
            "whatsapp",
            Some(target),
            None,
        )
        .await
    {
        Ok(resp) => Ok(resp.content),
        Err(e) => {
            tracing::warn!(
                "[bg-resume] whatsapp: resume turn failed for session {session_id}: {e}"
            );
            Err(e.to_string())
        }
    }
}

/// One composing ping for the resume ticker: no-ops quietly when the
/// transport is down (`state.client()` is None until it reconnects).
async fn send_composing_now(state: &WhatsAppState, jid_str: &str) -> Result<(), String> {
    let client = state.client().await.ok_or("whatsapp transport not ready")?;
    let jid = jid_str
        .parse::<wacore_binary::jid::Jid>()
        .map_err(|e| e.to_string())?;
    client
        .chatstate()
        .send_composing(&jid)
        .await
        .map_err(|e| e.to_string())
}
