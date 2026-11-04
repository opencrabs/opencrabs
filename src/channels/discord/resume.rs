//! Background-task resume producer for Discord (#731).
//!
//! Mirrors Telegram's `build_enqueue_callback`: when a detached long command
//! finishes, resume the originating session and deliver the result to its
//! Discord channel. Since #1990 the delivery rides the tracked runner
//! (`tracked_turn::run_tracked_resume_turn`), the same visible, slot-holding
//! turn shape ingress and boot recovery use; a completion that loses the
//! slot claim queues its text as a follow-up instead of forking a second
//! tool loop on the busy session.

use super::DiscordState;
use crate::brain::agent::service::MessageEnqueueCallback;
use crate::channels::background_work::{bg_indicator_for, subagent_counts_for, waiting_verb};
use crate::channels::bg_resume::{self, AgentHolder};
use std::sync::Arc;

/// Discord rejects any message over 2000 characters with `Message too large`
/// and drops the entire payload (#1899), so a long resumed verdict that fails
/// never reaches the channel at all. Chunk it through the same fence- and
/// markup-aware splitter the live delivery path uses
/// (`handler::split_message`, #876).
///
/// Blank content yields no chunks: an empty `say` is a 400 and the splitter
/// would hand back one empty chunk for it.
pub(crate) fn resume_delivery_chunks(content: &str) -> Vec<String> {
    if content.trim().is_empty() {
        return Vec::new();
    }
    super::handler::split_message(content, 2000)
}

pub(crate) fn build_enqueue_callback(
    state: Arc<DiscordState>,
    agent_holder: AgentHolder,
) -> MessageEnqueueCallback {
    Arc::new(move |session_id, msg| {
        let state = state.clone();
        let agent_holder = agent_holder.clone();
        tokio::spawn(async move {
            let Some(channel_id) = state.session_channel(session_id).await else {
                tracing::warn!(
                    "[bg-resume] discord: no channel for session {session_id}; dropping"
                );
                return;
            };
            // Bounded wait rather than a drop (#1242): at boot the transport
            // is usually seconds away, and returning here lost the wake for
            // good.
            let Some(http) =
                crate::channels::transport_ready::await_transport("discord", session_id, || {
                    state.http()
                })
                .await
            else {
                return;
            };
            let Some(agent) = bg_resume::upgrade(&agent_holder) else {
                tracing::warn!("[bg-resume] discord: agent gone; dropping resume");
                return;
            };
            // #1987: a completion just landed. If this session's turn settled
            // to a ⏳ waiting line, re-fold the verb from both registries and
            // re-render: the line narrows as work drains and flips to the
            // plain finished check when it empties. Sessions with no waiting
            // group are untouched; an aged-out group drops its registration.
            if let Some(gmid) = state.waiting_group_for(session_id).await {
                let (_, bg_count) = bg_indicator_for(&agent, session_id);
                let verb = waiting_verb(bg_count, subagent_counts_for(&agent, session_id));
                if let Some(group) = state.refresh_waiting_line(gmid, verb).await {
                    let edit = serenity::builder::EditMessage::new()
                        .content(super::tool_group::render_content(&group))
                        .embeds(super::tool_group::render_embeds(&group))
                        .components(super::tool_group::render_components(&group, gmid));
                    let flip_mid = serenity::model::id::MessageId::new(gmid);
                    if let Some(Err(e)) = super::governor::edit_chrome(
                        &state.governor,
                        serenity::model::id::ChannelId::new(channel_id),
                        &http,
                        flip_mid,
                        edit,
                    )
                    .await
                    {
                        tracing::debug!("[bg-resume] discord: waiting-line flip edit failed: {e}");
                    }
                } else {
                    state.clear_waiting_group(session_id).await;
                }
            }
            // #1990: run the completion turn through the tracked runner
            // (tool-group shell, live clock, progress events, approvals)
            // instead of the invisible all-None `run_resume_turn` followed
            // by plain chunks. If a turn already owns the session, this
            // text joins its injection queue rather than racing it; only
            // the caller holds the whole message with origin and receipt
            // payload, so the enqueue happens HERE, not in the runner.
            let outcome = super::tracked_turn::run_tracked_resume_turn(
                http,
                serenity::model::id::ChannelId::new(channel_id),
                session_id,
                state.clone(),
                agent,
                super::tracked_turn::ResumeDispatch::Push {
                    context_text: msg.context_text.clone(),
                },
            )
            .await;
            match outcome {
                super::tracked_turn::ResumeTurnOutcome::Queued => {
                    state.enqueue_followup(session_id, msg);
                }
                super::tracked_turn::ResumeTurnOutcome::Failed => {
                    tracing::warn!(
                        "[bg-resume] discord: tracked resume turn failed for session {session_id}"
                    );
                }
                super::tracked_turn::ResumeTurnOutcome::Delivered => {}
            }
        });
    })
}
