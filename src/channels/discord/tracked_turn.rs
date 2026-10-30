//! The tracked resume-turn runner for Discord (#1990).
//!
//! Modelled on `interactions::route_followup_turn` (itself the tap twin of
//! `handler::handle_message`): the turn-start tool-group shell is born BEFORE
//! dispatch, the flow ticker keeps its clock live, tool events edit the
//! bubble in place, approvals route to the Discord buttons, and the answer
//! settles the group. Boot recovery never had any of this:
//! `resume_delivery_task` ran the revived turn with `progress = None` and
//! posted one plain notice line, so the channel looked dead while the answer
//! cooked, and a follow-up sent meanwhile forked a second concurrent loop
//! (#1990).
//!
//! The runner claims the session slot FIRST, before `store_cancel_token`:
//! a resume turn that lost the claim race would otherwise take over the live
//! turn's cancel slot (and, before the store stopped cancelling, kill the
//! turn it meant to join).
//! The dispatch is injectable because the same visible turn is the right
//! shape for all three producers: the boot-recovery replay, the
//! background-completion turn (#1986), and the follow-up flush.

use std::sync::Arc;

use serenity::http::Http;
use serenity::model::id::{ChannelId, MessageId};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::DiscordState;
use super::tool_card::entry_matches_completion;
use super::tool_group::{GroupEntry, GroupState};
use crate::brain::agent::AgentService;
use crate::channels::background_work::{FlowOutcome, outcome_for_error};

/// The agent call a tracked turn rides (#1990).
pub(crate) enum ResumeDispatch {
    /// Boot-recovery replay: `resume_interrupted_turn`. Untracked on
    /// purpose (#729): a resume that is itself interrupted must not leave a
    /// new pending row behind.
    Recovery { prompt: String },
    /// Background-completion push: `send_push_turn`, tracked with origin
    /// `system` so a completion turn killed mid-tool is visible to the next
    /// boot recovery (#12).
    Push { context_text: String },
    /// A follow-up drained from the session queue after the turn that would
    /// have injected it had ended: a full tool-loop send keeping the
    /// separate display text.
    Display {
        text: String,
        display_tag: Option<String>,
    },
}

/// What a tracked turn became (#1990).
pub(crate) enum ResumeTurnOutcome {
    /// The turn held the slot and settled with its answer posted.
    Delivered,
    /// The slot was held by another turn: the caller must hand its message
    /// to the follow-up queue so the live loop injects it between rounds.
    /// The runner does not enqueue: only the caller still holds the whole
    /// message with origin and receipt payload intact.
    Queued,
    /// The turn held the slot and ended cancelled or errored; the channel
    /// saw the terminal stamp. The boot ledger counts this as failed, the
    /// same rule #1952 set for delivery outcomes.
    Failed,
}

/// Run one visible, slot-holding Discord turn. See the module docs for why
/// the claim precedes every side effect.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_tracked_resume_turn(
    http: Arc<Http>,
    channel: ChannelId,
    session_id: Uuid,
    dstate: Arc<DiscordState>,
    agent: Arc<AgentService>,
    dispatch: ResumeDispatch,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ResumeTurnOutcome> + Send>> {
    // The boxed return is load-bearing, not style: the end-of-turn flush
    // spawns THIS runner from its own body, so an inferred future would be
    // a Sendness cycle rustc refuses to resolve ("future cannot be sent
    // between threads safely"). One declared `+ Send` breaks the cycle.
    Box::pin(async move {
        // Claim before anything can cancel anything (#1990, module docs): losing
        // the claim must never touch the live turn's token.
        let Some(_turn_guard) = dstate.try_begin_turn(session_id) else {
            return ResumeTurnOutcome::Queued;
        };

        dstate
            .register_session_channel(session_id, channel.get())
            .await;

        let cancel_token = tokio_util::sync::CancellationToken::new();
        dstate
            .store_cancel_token(session_id, cancel_token.clone())
            .await;

        let trace_narration = crate::config::Config::current()
            .channels
            .discord
            .trace_narration;

        // Per-turn dedup state, the #456/#459/#943/#951 class, exactly as the
        // tap turn keeps it: the tool loop emits the last iteration's text BOTH
        // as IntermediateText AND as response.content.
        type SentIntermediate = String;
        let sent_intermediates: Arc<Mutex<Vec<SentIntermediate>>> =
            Arc::new(Mutex::new(Vec::new()));
        let intermediate_handles: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        // The bubble slot owns the id and the birth claim (#2192): a birth
        // may sleep in the governor, so it must not hold the id lock that the
        // settle path needs to stamp the finished bubble.
        let bubble = Arc::new(super::group_bubble::BubbleSlot::new());
        let turn_group_mid: Arc<Mutex<Option<MessageId>>> = bubble.mid_handle();
        // The text a suggest_options surface halted on (see options_host.rs).
        let options_host = Arc::new(std::sync::Mutex::new(
            super::options_host::OptionsHost::default(),
        ));
        let options_host_cb = options_host.clone();

        let progress_cb: crate::brain::agent::ProgressCallback = {
            use crate::brain::agent::ProgressEvent;
            use serenity::builder::EditMessage;

            let tools: Arc<Mutex<Vec<GroupEntry>>> = Arc::new(Mutex::new(Vec::new()));
            let group_msg_id = turn_group_mid.clone();
            let bubble_cb = bubble.clone();
            let group_state_cb = dstate.clone();
            let http = http.clone();
            let sent = sent_intermediates.clone();
            let handles_cb = intermediate_handles.clone();

            Arc::new(move |session_id, event| {
                let tools = tools.clone();
                let http = http.clone();

                match event {
                    ProgressEvent::ToolStarted {
                        tool_name,
                        tool_input,
                        tool_call_id,
                    } => {
                        if let Ok(mut host) = options_host_cb.lock() {
                            host.on_tool_started(&tool_name);
                        }
                        let ctx_hint = crate::utils::tool_context_hint(&tool_name, &tool_input);
                        let bubble = bubble_cb.clone();
                        let dstate = group_state_cb.clone();
                        tokio::spawn(async move {
                            let entries = {
                                let mut t = tools.lock().await;
                                t.push(GroupEntry {
                                    name: tool_name,
                                    context: ctx_hint,
                                    call_id: tool_call_id.clone(),
                                    task_id: None,
                                    status: None,
                                });
                                t.clone()
                            };
                            // The bubble exists: append without touching the
                            // birth claim.
                            if let Some(mid) = bubble.read().await {
                                super::group_bubble::append_tool_entry(
                                    &dstate,
                                    channel,
                                    &http,
                                    mid,
                                    entries,
                                    "tracked turn",
                                )
                                .await;
                                return;
                            }
                            // Birth: one bubble per turn, so births serialise.
                            // The id lock stays free while `governor::say`
                            // sleeps, which is what #2192 fixes.
                            let _birth = bubble.birth_claim().await;
                            if let Some(mid) = bubble.read().await {
                                super::group_bubble::append_tool_entry(
                                    &dstate,
                                    channel,
                                    &http,
                                    mid,
                                    entries,
                                    "tracked turn",
                                )
                                .await;
                                return;
                            }
                            let group = GroupState {
                                page: 0,
                                entries,
                                notes: Vec::new(),
                                expanded: false,
                                started_at: std::time::Instant::now(),
                                settled: None,
                            };
                            let content = super::tool_group::render_content(&group);
                            match super::governor::say(
                                &dstate.governor,
                                channel,
                                &http,
                                super::governor::Surface::Send,
                                &content,
                            )
                            .await
                            {
                                Ok(sent_msg) => {
                                    let comps = super::tool_group::render_components(
                                        &group,
                                        sent_msg.id.get(),
                                    );
                                    if !comps.is_empty()
                                        && let Some(Err(e)) = super::governor::edit_chrome(
                                            &dstate.governor,
                                            channel,
                                            &http,
                                            sent_msg.id,
                                            EditMessage::new()
                                                .embeds(super::tool_group::render_embeds(&group))
                                                .components(comps),
                                        )
                                        .await
                                    {
                                        tracing::warn!(
                                            "Discord: tracked turn tool group component fixup failed: {e}"
                                        );
                                    }
                                    dstate.upsert_tool_group(sent_msg.id.get(), group).await;
                                    bubble.store(sent_msg.id).await;
                                }
                                Err(e) => tracing::warn!(
                                    "Discord: tracked turn failed to post tool group message: {e}"
                                ),
                            }
                        });
                    }
                    ProgressEvent::ToolCompleted {
                        tool_name,
                        success,
                        tool_call_id,
                        ..
                    } => {
                        let gmid = group_msg_id.clone();
                        let dstate = group_state_cb.clone();
                        tokio::spawn(async move {
                            let entries = {
                                let mut t = tools.lock().await;
                                if let Some(entry) = t.iter_mut().rev().find(|e| {
                                    e.status.is_none()
                                        && entry_matches_completion(e, &tool_name, &tool_call_id)
                                }) {
                                    entry.status = Some(success);
                                }
                                t.clone()
                            };
                            if let Some(mid) = *gmid.lock().await {
                                let group = dstate
                                    .upsert_tool_group(
                                        mid.get(),
                                        GroupState {
                                            page: 0,
                                            entries,
                                            notes: Vec::new(),
                                            expanded: false,
                                            started_at: std::time::Instant::now(),
                                            settled: None,
                                        },
                                    )
                                    .await;
                                let edit = EditMessage::new()
                                    .content(super::tool_group::render_content(&group))
                                    .embeds(super::tool_group::render_embeds(&group))
                                    .components(super::tool_group::render_components(
                                        &group,
                                        mid.get(),
                                    ));
                                if let Some(Err(e)) = super::governor::edit_chrome(
                                    &dstate.governor,
                                    channel,
                                    &http,
                                    mid,
                                    edit,
                                )
                                .await
                                {
                                    tracing::warn!(
                                        "Discord: tracked turn tool group edit failed (status): {e}"
                                    );
                                }
                            }
                        });
                    }
                    ProgressEvent::SelfHealingAlert { message } => {
                        let dstate = group_state_cb.clone();
                        tokio::spawn(async move {
                            let text = format!(
                                "🔧 {}",
                                crate::utils::sanitize::normalize_dashes(&message)
                            );
                            if let Err(e) = super::governor::say(
                                &dstate.governor,
                                channel,
                                &http,
                                super::governor::Surface::Send,
                                &text,
                            )
                            .await
                            {
                                tracing::warn!(error = %e, "Discord: tracked turn self-heal post failed");
                            }
                        });
                    }
                    ProgressEvent::IntermediateText { text, .. } => {
                        // Same sanitation the live paths run, so the dedup keys
                        // below normalize identically for intermediate and final
                        // copies.
                        let clean = crate::utils::sanitize::strip_llm_artifacts(&text);
                        let clean = crate::utils::sanitize::redact_secrets(&clean);
                        let (clean, _) = crate::utils::extract_img_markers(&clean);
                        let (clean, _) = crate::utils::extract_vid_markers(&clean);
                        let clean = super::table_convert::tables_to_discord(&clean);
                        if clean.trim().is_empty() {
                            return;
                        }
                        if let Ok(mut host) = options_host_cb.lock() {
                            host.on_text(&clean);
                        }
                        if trace_narration {
                            let gmid = group_msg_id.clone();
                            let dstate = group_state_cb.clone();
                            let http = http.clone();
                            let handles = handles_cb.clone();
                            let note = super::tool_group::clip_note(&clean);
                            let handle = tokio::spawn(async move {
                                let Some(mid) = *gmid.lock().await else {
                                    return;
                                };
                                let Some(group) = dstate.append_note(mid.get(), note).await else {
                                    return;
                                };
                                let edit = EditMessage::new()
                                    .content(super::tool_group::render_content(&group))
                                    .embeds(super::tool_group::render_embeds(&group))
                                    .components(super::tool_group::render_components(
                                        &group,
                                        mid.get(),
                                    ));
                                if let Some(Err(e)) = super::governor::edit_chrome(
                                    &dstate.governor,
                                    channel,
                                    &http,
                                    mid,
                                    edit,
                                )
                                .await
                                {
                                    tracing::debug!(
                                        "Discord: tracked turn trace note edit failed: {e}"
                                    );
                                }
                            });
                            if let Ok(mut g) = handles.lock() {
                                g.push(handle);
                            }
                            return;
                        }
                        let sent = sent.clone();
                        let handles = handles_cb.clone();
                        let http = http.clone();
                        let dstate = group_state_cb.clone();
                        let handle = tokio::spawn(async move {
                            {
                                let mut prev = sent.lock().await;
                                if prev.iter().any(|b| b == &clean) {
                                    return;
                                }
                                prev.push(clean.clone());
                            }
                            for chunk in super::handler::split_message(&clean, 2000) {
                                if let Err(e) = super::governor::say_recorded(
                                    &dstate.governor,
                                    channel,
                                    &http,
                                    super::governor::Surface::Send,
                                    &chunk,
                                )
                                .await
                                {
                                    tracing::debug!(
                                        "Discord: tracked turn intermediate send failed: {e}"
                                    )
                                }
                            }
                        });
                        if let Ok(mut g) = handles.lock() {
                            g.push(handle);
                        }
                    }
                    ProgressEvent::RetryAttempt {
                        attempt,
                        max,
                        reason,
                    } => {
                        let http = http.clone();
                        let dstate = group_state_cb.clone();
                        tokio::spawn(async move {
                            let text = format!("⏳ Retry {}/{} - {}", attempt, max, reason);
                            if let Err(e) = super::governor::say(
                                &dstate.governor,
                                channel,
                                &http,
                                super::governor::Surface::Send,
                                &text,
                            )
                            .await
                            {
                                tracing::warn!(error = %e, "Discord: tracked turn retry post failed");
                            }
                        });
                    }
                    ProgressEvent::ProviderSwitched {
                        to_name, to_model, ..
                    } => {
                        let http = http.clone();
                        let dstate = group_state_cb.clone();
                        tokio::spawn(async move {
                            let text = format!("🔄 Now using {}/{}", to_name, to_model);
                            if let Err(e) = super::governor::say(
                                &dstate.governor,
                                channel,
                                &http,
                                super::governor::Surface::Send,
                                &text,
                            )
                            .await
                            {
                                tracing::warn!(error = %e, "Discord: tracked turn provider switch post failed");
                            }
                        });
                    }
                    ProgressEvent::SuggestedOptions(options) => {
                        // Trace mode folded the answer into the bubble: post it
                        // before the buttons. Trace off already posted it.
                        let host = if trace_narration {
                            options_host_cb.lock().ok().and_then(|mut h| h.take())
                        } else {
                            None
                        };
                        let http = http.clone();
                        let state = group_state_cb.clone();
                        let sent = sent.clone();
                        let gmid = group_msg_id.clone();
                        let raw_options: Vec<String> =
                            options.into_iter().map(|item| item.label).collect();
                        tokio::spawn(async move {
                            if let Some(host) = host {
                                let mid = *gmid.lock().await;
                                super::options_host::post_options_host(
                                    &state, channel, &http, &sent, mid, &host,
                                )
                                .await;
                            }
                            super::suggest_options::render_suggestions(
                                &http,
                                &state,
                                session_id,
                                raw_options,
                            )
                            .await;
                        });
                    }
                    _ => {}
                }
            })
        };
        // Turn-start group shell, born BEFORE dispatch (the #1845 pattern the tap
        // turn proved): the recovered work is visible with a live clock from
        // second zero. On a post failure the mid stays None and bubble creation
        // falls back to the first tool event.
        let turn_shell = GroupState {
            page: 0,
            entries: Vec::new(),
            notes: Vec::new(),
            expanded: false,
            started_at: std::time::Instant::now(),
            settled: None,
        };
        match super::governor::say(
            &dstate.governor,
            channel,
            &http,
            super::governor::Surface::Send,
            &super::tool_group::render_content(&turn_shell),
        )
        .await
        {
            Ok(sent_msg) => {
                dstate
                    .upsert_tool_group(sent_msg.id.get(), turn_shell)
                    .await;
                *turn_group_mid.lock().await = Some(sent_msg.id);
            }
            Err(e) => tracing::warn!("Discord: tracked turn-start shell post failed: {e}"),
        }

        // Flow ticker: the bubble's clock never freezes between tool events.
        super::handler::spawn_flow_ticker(
            http.clone(),
            channel,
            turn_group_mid.clone(),
            dstate.clone(),
        );

        let approval_cb = super::handler::make_approval_callback(dstate.clone());
        let chat_id_str = channel.get().to_string();
        let result = match dispatch {
            ResumeDispatch::Recovery { prompt } => {
                agent
                    .resume_interrupted_turn(
                        session_id,
                        prompt,
                        None,
                        Some(cancel_token),
                        Some(approval_cb),
                        Some(progress_cb),
                        "discord",
                        Some(&chat_id_str),
                    )
                    .await
            }
            ResumeDispatch::Push { context_text } => {
                agent
                    .send_push_turn(
                        session_id,
                        context_text,
                        None,
                        Some(cancel_token),
                        Some(approval_cb),
                        Some(progress_cb),
                        "discord",
                        Some(&chat_id_str),
                        None,
                    )
                    .await
            }
            ResumeDispatch::Display { text, display_tag } => {
                agent
                    .send_message_with_tools_and_display(
                        session_id,
                        text,
                        display_tag,
                        None,
                        Some(cancel_token),
                        Some(approval_cb),
                        Some(progress_cb),
                        "discord",
                        Some(&chat_id_str),
                        None,
                    )
                    .await
            }
        };

        dstate.remove_cancel_token(session_id).await;

        let outcome = match result {
            Ok(response) => {
                // Await in-flight intermediate posts before the dedup read
                // (spawn-then-push race, the #459/#951 class).
                let pending = {
                    let mut g = intermediate_handles.lock().expect("poisoned");
                    std::mem::take(&mut *g)
                };
                for h in pending {
                    if let Err(e) = h.await {
                        tracing::warn!("Discord: tracked turn intermediate task panicked: {e}");
                    }
                }
                let (response_content, _react) =
                    crate::utils::extract_react_marker(&response.content);
                let (text_only, _imgs) = crate::utils::extract_img_markers(&response_content);
                let text_only = crate::utils::sanitize::strip_llm_artifacts(&text_only);
                let text_only = crate::utils::sanitize::redact_secrets(&text_only);
                let text_only = super::table_convert::tables_to_discord(&text_only);

                let ctx_max = agent.context_limit_for_session(session_id);
                let ctx_line = crate::utils::format_ctx_footer(
                    response.context_tokens,
                    ctx_max,
                    response.tokens_per_second,
                );

                let skip_final_post = {
                    let posted = sent_intermediates.lock().await;
                    if text_only.trim().is_empty() {
                        true
                    } else {
                        let final_key = super::handler::norm_key(&text_only);
                        posted
                            .iter()
                            .any(|b| super::handler::norm_key(b) == final_key)
                    }
                };

                // Trace mode: drop the mirror note the tool loop folded in as
                // the trailing intermediate (the full answer posts below).
                let answer_head = text_only
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
                    .to_lowercase();
                if let Some(mid) = *turn_group_mid.lock().await
                    && let Some(group) = dstate
                        .drop_note_if(mid.get(), |n| answer_head.starts_with(&n.to_lowercase()))
                        .await
                {
                    let edit = serenity::builder::EditMessage::new()
                        .content(super::tool_group::render_content(&group))
                        .embeds(super::tool_group::render_embeds(&group))
                        .components(super::tool_group::render_components(&group, mid.get()));
                    if let Some(Err(e)) =
                        super::governor::edit_chrome(&dstate.governor, channel, &http, mid, edit)
                            .await
                    {
                        tracing::debug!("Discord: tracked turn trace mirror-note drop failed: {e}");
                    }
                }

                // Settled chrome: freeze the clock, stamp the ctx budget, and
                // hold the ⏳ waiting line when detached work outlives the turn
                // (#1987's registration, so resume.rs can flip it later).
                let waiting = {
                    let (_, bg_count) =
                        crate::channels::background_work::bg_indicator_for(&agent, session_id);
                    crate::channels::background_work::waiting_verb(
                        bg_count,
                        crate::channels::background_work::subagent_counts_for(&agent, session_id),
                    )
                };
                if let Some(mid) = *turn_group_mid.lock().await
                    && let Some(group) = dstate
                        .settle_tool_group(
                            mid.get(),
                            if ctx_line.is_empty() {
                                None
                            } else {
                                Some(ctx_line.clone())
                            },
                            waiting.clone(),
                            None,
                        )
                        .await
                {
                    if waiting.is_some() {
                        dstate.register_waiting_group(session_id, mid.get()).await;
                    }
                    let edit = serenity::builder::EditMessage::new()
                        .content(super::tool_group::render_content(&group))
                        .embeds(super::tool_group::render_embeds(&group))
                        .components(super::tool_group::render_components(&group, mid.get()));
                    if let Err(e) = super::governor::edit_content(
                        &dstate.governor,
                        channel,
                        &http,
                        mid,
                        super::governor::Surface::Final,
                        edit,
                    )
                    .await
                    {
                        tracing::debug!("Discord: tracked turn settled stamp failed: {e}");
                    }
                    // #1912: the card's last word, FINAL like this stamp.
                    super::plan_card::refresh_plan_card(&dstate, &http, channel, session_id, true)
                        .await;
                }

                if !skip_final_post {
                    // The #1899 cap disease applies here too: chunk through the
                    // fence-aware splitter, never one plain `say`.
                    for chunk in super::resume::resume_delivery_chunks(&text_only) {
                        if let Err(e) = super::governor::say_recorded(
                            &dstate.governor,
                            channel,
                            &http,
                            super::governor::Surface::Send,
                            &chunk,
                        )
                        .await
                        {
                            tracing::error!("Discord: tracked turn reply delivery failed: {e}");
                        }
                    }
                }
                ResumeTurnOutcome::Delivered
            }
            Err(ref e) if matches!(e, crate::brain::agent::AgentError::Cancelled) => {
                tracing::info!("Discord: tracked turn cancelled for session {session_id}");
                if let Some(mid) = *turn_group_mid.lock().await
                    && let Some(group) = dstate
                        .settle_tool_group(mid.get(), None, None, Some(FlowOutcome::Cancelled))
                        .await
                {
                    let edit = serenity::builder::EditMessage::new()
                        .content(super::tool_group::render_content(&group))
                        .embeds(super::tool_group::render_embeds(&group))
                        .components(super::tool_group::render_components(&group, mid.get()));
                    if let Err(e) = super::governor::edit_content(
                        &dstate.governor,
                        channel,
                        &http,
                        mid,
                        super::governor::Surface::Final,
                        edit,
                    )
                    .await
                    {
                        tracing::debug!("Discord: tracked turn cancelled settle stamp failed: {e}");
                    }
                }
                ResumeTurnOutcome::Failed
            }
            Err(e) => {
                tracing::error!("Discord: tracked turn agent error: {e}");
                let error_msg =
                    format!("❌ Error\n\n{}", crate::brain::agent::format_user_error(&e));
                if let Err(e) = super::governor::say_recorded(
                    &dstate.governor,
                    channel,
                    &http,
                    super::governor::Surface::Send,
                    error_msg,
                )
                .await
                {
                    tracing::warn!("Discord: tracked turn error post failed: {e}");
                }
                if let Some(mid) = *turn_group_mid.lock().await
                    && let Some(group) = dstate
                        .settle_tool_group(mid.get(), None, None, Some(outcome_for_error(&e)))
                        .await
                {
                    let edit = serenity::builder::EditMessage::new()
                        .content(super::tool_group::render_content(&group))
                        .embeds(super::tool_group::render_embeds(&group))
                        .components(super::tool_group::render_components(&group, mid.get()));
                    if let Err(e) = super::governor::edit_content(
                        &dstate.governor,
                        channel,
                        &http,
                        mid,
                        super::governor::Surface::Final,
                        edit,
                    )
                    .await
                    {
                        tracing::debug!("Discord: tracked turn error settle stamp failed: {e}");
                    }
                }
                ResumeTurnOutcome::Failed
            }
        };

        // End-of-turn flush (#201's Telegram rule, #1990): items enqueued AFTER
        // the loop's last between-rounds drain have no consumer left on this
        // path. Release the slot first so the flushed follow-up turn can claim
        // it; if it loses that brand-new race, the winner's queue takes the
        // message back, so nothing is ever dropped.
        drop(_turn_guard);
        if let Some(joined) = dstate.drain_followups(session_id) {
            let http = http.clone();
            let dstate = dstate.clone();
            let agent = agent.clone();
            let text = joined.context_text.clone();
            let display = joined.display_text.clone();
            tokio::spawn(async move {
                if matches!(
                    run_tracked_resume_turn(
                        http,
                        channel,
                        session_id,
                        dstate.clone(),
                        agent,
                        ResumeDispatch::Display {
                            text,
                            display_tag: Some(display),
                        },
                    )
                    .await,
                    ResumeTurnOutcome::Queued
                ) {
                    dstate.enqueue_followup(session_id, joined);
                }
            });
        }
        outcome
    })
}
