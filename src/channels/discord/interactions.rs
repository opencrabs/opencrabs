//! Agent-driven interactive components: select menus (#382) and modal
//! forms (#383), with lazy TTL expiry (#386).
//!
//! `discord_send` posts a select menu or a form button; the pick or the
//! submitted fields come back here, get routed into the channel's session
//! as an agent turn (with only a compact "[System: ...]" tag persisted),
//! and the reply is delivered to the channel. Pending component state
//! lives in [`super::DiscordState`] with creation timestamps; clicks past
//! the TTL answer "expired" instead of firing stale actions.

use crate::brain::agent::AgentService;
use crate::channels::background_work::{
    FlowOutcome, bg_indicator_for, outcome_for_error, subagent_counts_for, waiting_verb,
};
use crate::services::SessionService;
use serenity::prelude::Context;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

/// One pending modal form: what the modal shows when the button is hit.
#[derive(Debug, Clone)]
pub(crate) struct FormSpec {
    pub title: String,
    /// (label, multiline) per field, max 5 (Discord's modal cap).
    pub fields: Vec<(String, bool)>,
}

/// Resolve (or create) the session for interaction input, mirroring
/// handle_message's keying: DMs by user, channels/threads by channel id.
pub(crate) async fn resolve_interaction_session(
    session_svc: &SessionService,
    is_dm: bool,
    user_id: u64,
    channel_id: u64,
    idle_hours: Option<f64>,
) -> Option<Uuid> {
    use crate::channels::session_resolve;
    let (id_str, legacy_title) = if is_dm {
        (
            format!("discord-dm-{user_id}"),
            format!("Discord: DM {user_id}"),
        )
    } else {
        (
            format!("discord-{channel_id}"),
            format!("Discord: #{channel_id}"),
        )
    };
    let suffix = session_resolve::chat_id_suffix(&id_str);
    let session_title = format!("{legacy_title} {suffix}");
    match session_resolve::resolve_or_create_channel_session(
        session_svc,
        &suffix,
        &legacy_title,
        &session_title,
        idle_hours,
        "Discord",
    )
    .await
    {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::error!("Discord interaction: failed to resolve session: {e}");
            None
        }
    }
}

/// Run an interaction-originated agent turn and deliver the reply to the
/// channel. `context_text` goes to the model for this turn only; history
/// persists `display_tag` (the system-note contract shared with reactions).
///
/// The reply is a content write, so it pays the governor's budget before it
/// reaches Discord (#2011, the last raw send in this module): a `retry_after`
/// learned here parks the channel instead of being logged and ignored.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn route_interaction_turn(
    ctx: &Context,
    governor: &super::governor::Governor,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    is_dm: bool,
    user_id: u64,
    channel_id: u64,
    idle_hours: Option<f64>,
    context_text: String,
    display_tag: String,
    discord_state: Arc<super::DiscordState>,
) {
    let Some(session_id) =
        resolve_interaction_session(&session_svc, is_dm, user_id, channel_id, idle_hours).await
    else {
        return;
    };

    // Claim the session's turn slot before the turn runs (#2191). A select
    // pick or a modal submit is not a user-intent turn, but it still starts a
    // tool loop against the session, so it obeys the same rule the typed path
    // and the tap follow (#1990). Losing the claim queues the interaction for
    // the live turn: without this, a component interaction arriving mid-turn
    // forked a second concurrent loop onto the same session row.
    let Some(_turn_guard) =
        discord_state.claim_interaction_turn(session_id, context_text.clone(), display_tag.clone())
    else {
        return;
    };

    let response = match agent
        .send_message_with_display(session_id, context_text, Some(display_tag), None)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Discord interaction: agent error for session {session_id}: {e}");
            return;
        }
    };
    let (text_only, _imgs) = crate::utils::extract_img_markers(&response.content);
    let text_only = crate::utils::sanitize::sanitize_outbound(&text_only);
    let (text_only, _react) = crate::utils::extract_react_marker(&text_only);
    let trimmed = text_only.trim();
    if trimmed.is_empty() {
        return;
    }
    let channel = serenity::model::id::ChannelId::new(channel_id);
    for chunk in super::handler::split_message(trimmed, 2000) {
        if let Err(e) = super::governor::say(
            governor,
            channel,
            &ctx.http,
            super::governor::Surface::Send,
            chunk,
        )
        .await
        {
            tracing::warn!("Discord interaction: failed to deliver reply: {e}");
        }
    }
}

/// Run a tapped follow-up suggestion as an agent turn through the SAME
/// tool-loop display path typed messages use (#1852, the Discord twin of
/// #1847). The tap used to ride the bare `send_message_with_display`
/// single-completion path: zero tools, zero progress events, zero approvals,
/// so the channel sat silent from the `▶️` echo until a plain reply landed.
///
/// Here the flow-group shell and its 🕒 ticker are born BEFORE dispatch, tool
/// events edit the bubble in place, intermediate text dedups against the
/// final answer, approvals route to the Discord buttons, and chained
/// `SuggestedOptions` render again. History persists `display_tag` (the
/// tapper's name) exactly like the bare path did.
///
/// The other [`route_interaction_turn`] callers (modal form, select menu)
/// deliberately stay on the bare single-call contract: they are synthetic
/// steering prompts, not user-intent turns.
///
/// The turn claims the session slot before it touches the cancel token, the
/// same rule `handle_message` and the tracked runner follow (#1990). This
/// path was the one entry point `3f0086081` missed, so a tap arriving while
/// a turn was live could start a SECOND concurrent tool loop on the same
/// session instead of queueing behind the live one.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn route_followup_turn(
    ctx: &Context,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    discord_state: Arc<super::DiscordState>,
    is_dm: bool,
    user_id: u64,
    channel_id: u64,
    idle_hours: Option<f64>,
    context_text: String,
    display_tag: String,
) {
    let Some(session_id) =
        resolve_interaction_session(&session_svc, is_dm, user_id, channel_id, idle_hours).await
    else {
        return;
    };

    // Claim the session's turn slot before anything else (#1990). A tap is a
    // user-intent turn like a typed message, and it must never fork a second
    // loop onto a session that already has one: losing the claim queues the
    // pick for the live turn, which injects it between tool rounds or flushes
    // it at turn end, exactly like a typed follow-up.
    let Some(_turn_guard) = discord_state.try_begin_turn(session_id) else {
        tracing::info!(
            "Discord: follow-up tap queued for session {session_id}, a turn is in flight (#1990)"
        );
        discord_state.enqueue_followup(
            session_id,
            crate::brain::agent::QueuedUserMessage {
                context_text: context_text.clone(),
                display_text: context_text,
                origin: crate::brain::agent::PushOrigin::Ingress,
                bg_meta: None,
            },
        );
        return;
    };

    let channel = serenity::model::id::ChannelId::new(channel_id);
    let http = ctx.http.clone();

    // Approvals and chained suggestions resolve their delivery target through
    // the session→channel registration; mirror handle_message and register
    // before dispatch.
    discord_state
        .register_session_channel(session_id, channel_id)
        .await;

    let cancel_token = tokio_util::sync::CancellationToken::new();
    discord_state
        .store_cancel_token(session_id, cancel_token.clone())
        .await;

    let trace_narration = crate::config::Config::current()
        .channels
        .discord
        .trace_narration;

    // Per-turn dedup state (the #456/#459/#943/#951 class, ported from
    // handle_message): tool_loop emits the last iteration's text BOTH as
    // IntermediateText AND as response.content — posting both duplicates
    // the answer.
    type SentIntermediate = String;
    let sent_intermediates: Arc<Mutex<Vec<SentIntermediate>>> = Arc::new(Mutex::new(Vec::new()));
    let intermediate_handles: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    // The bubble slot owns the id and the birth claim (#2192): a birth may
    // sleep in the governor, so it must not hold the id lock that the settle
    // path needs to stamp the finished bubble.
    let bubble = Arc::new(super::group_bubble::BubbleSlot::new());
    let turn_group_mid: Arc<Mutex<Option<serenity::model::id::MessageId>>> = bubble.mid_handle();
    // The text a suggest_options surface halted on (see options_host.rs).
    let options_host = Arc::new(std::sync::Mutex::new(
        super::options_host::OptionsHost::default(),
    ));
    let options_host_cb = options_host.clone();

    // Progress callback: handle_message's tool-loop display arms, scoped to
    // the tap turn.
    let progress_cb: crate::brain::agent::ProgressCallback = {
        use crate::brain::agent::ProgressEvent;
        use serenity::builder::EditMessage;

        use super::tool_group::{GroupEntry, GroupState};

        let tools: Arc<Mutex<Vec<GroupEntry>>> = Arc::new(Mutex::new(Vec::new()));
        let group_msg_id = turn_group_mid.clone();
        let bubble_cb = bubble.clone();
        let group_state_cb = discord_state.clone();
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
                        // The bubble exists: append without touching the birth
                        // claim.
                        if let Some(mid) = bubble.read().await {
                            super::group_bubble::append_tool_entry(
                                &dstate,
                                channel,
                                &http,
                                mid,
                                entries,
                                "follow-up tap",
                            )
                            .await;
                            return;
                        }
                        // Birth: one bubble per turn, so births serialise. The
                        // id lock stays free while `governor::say` sleeps,
                        // which is what #2192 fixes.
                        let _birth = bubble.birth_claim().await;
                        if let Some(mid) = bubble.read().await {
                            super::group_bubble::append_tool_entry(
                                &dstate,
                                channel,
                                &http,
                                mid,
                                entries,
                                "follow-up tap",
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
                                let comps =
                                    super::tool_group::render_components(&group, sent_msg.id.get());
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
                                        "Discord: follow-up tap tool group component fixup failed: {e}"
                                    );
                                }
                                dstate.upsert_tool_group(sent_msg.id.get(), group).await;
                                bubble.store(sent_msg.id).await;
                            }
                            Err(e) => tracing::warn!(
                                "Discord: follow-up tap failed to post tool group message: {e}"
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
                                    && super::tool_card::entry_matches_completion(
                                        e,
                                        &tool_name,
                                        &tool_call_id,
                                    )
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
                                    "Discord: follow-up tap tool group edit failed (status): {e}"
                                );
                            }
                        }
                    });
                }
                ProgressEvent::SelfHealingAlert { message } => {
                    let dstate = group_state_cb.clone();
                    tokio::spawn(async move {
                        let text =
                            format!("🔧 {}", crate::utils::sanitize::normalize_dashes(&message));
                        if let Err(e) = super::governor::say(
                            &dstate.governor,
                            channel,
                            &http,
                            super::governor::Surface::Send,
                            &text,
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "Discord: follow-up tap self-heal post failed");
                        }
                    });
                }
                ProgressEvent::IntermediateText { text, .. } => {
                    // Same sanitation the typed path applies, so the dedup
                    // keys below normalize identically for intermediate and
                    // final copies.
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
                                    "Discord: follow-up tap trace note edit failed: {e}"
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
                                    "Discord: follow-up tap intermediate send failed: {e}"
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
                        let text = format!("⏳ Retry {}/{} — {}", attempt, max, reason);
                        if let Err(e) = super::governor::say(
                            &dstate.governor,
                            channel,
                            &http,
                            super::governor::Surface::Send,
                            &text,
                        )
                        .await
                        {
                            tracing::warn!(error = %e, "Discord: follow-up tap retry post failed");
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
                            tracing::warn!(error = %e, "Discord: follow-up tap provider switch post failed");
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

    // Turn-start group shell (#1845's pattern): post the bubble NOW, before
    // the turn dispatches, so a tapped suggestion has a live 🕒 clock from
    // second zero — the whole of #1852. On a post failure the mid stays
    // None and bubble creation falls back to the first tool event.
    let turn_shell = super::tool_group::GroupState {
        page: 0,
        entries: Vec::new(),
        notes: Vec::new(),
        expanded: false,
        started_at: std::time::Instant::now(),
        settled: None,
    };
    match super::governor::say(
        &discord_state.governor,
        channel,
        &http,
        super::governor::Surface::Send,
        &super::tool_group::render_content(&turn_shell),
    )
    .await
    {
        Ok(sent_msg) => {
            discord_state
                .upsert_tool_group(sent_msg.id.get(), turn_shell)
                .await;
            *turn_group_mid.lock().await = Some(sent_msg.id);
        }
        Err(e) => {
            tracing::warn!("Discord: follow-up tap turn-start shell post failed: {e}")
        }
    }

    // Flow ticker (#1843's twin): re-render the clock every 4 s so the tap
    // turn's bubble never freezes between tool events.
    super::handler::spawn_flow_ticker(
        http.clone(),
        channel,
        turn_group_mid.clone(),
        discord_state.clone(),
    );

    let approval_cb = super::handler::make_approval_callback(discord_state.clone());
    let chat_id_str = channel_id.to_string();
    let result = agent
        .send_message_with_tools_and_display(
            session_id,
            context_text,
            Some(display_tag),
            None,
            Some(cancel_token),
            Some(approval_cb),
            Some(progress_cb),
            "discord",
            Some(&chat_id_str),
            None,
        )
        .await;

    discord_state.remove_cancel_token(session_id).await;

    match result {
        Ok(response) => {
            // Await in-flight intermediate posts before the dedup read
            // (spawn-then-push race, the #459/#951 class).
            let pending = {
                let mut g = intermediate_handles.lock().expect("poisoned");
                std::mem::take(&mut *g)
            };
            for h in pending {
                if let Err(e) = h.await {
                    tracing::warn!("Discord: follow-up tap intermediate task panicked: {e}");
                }
            }
            // Same sanitation tail the typed path runs before delivery. The
            // bare tap path dropped react and media markers; dropping them
            // here too keeps the tap contract unchanged (#1852 is about the
            // live status, not attachments).
            let (response_content, _react) = crate::utils::extract_react_marker(&response.content);
            let (text_only, _imgs) = crate::utils::extract_img_markers(&response_content);
            let text_only = crate::utils::sanitize::strip_llm_artifacts(&text_only);
            let text_only = crate::utils::sanitize::redact_secrets(&text_only);
            let text_only = super::table_convert::tables_to_discord(&text_only);

            // Settled status chrome (#1841's twin): freeze the clock and
            // stamp the ctx budget into the bubble's settled line.
            let ctx_max = agent.context_limit_for_session(session_id);
            let ctx_line = crate::utils::format_ctx_footer(
                response.context_tokens,
                ctx_max,
                response.tokens_per_second,
            );

            let skip_final_post = {
                let posted = sent_intermediates.lock().await;
                if text_only.trim().is_empty() {
                    // Empty-final guard: the real answer already went out
                    // as intermediates; never post a bare shell.
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
                && let Some(group) = discord_state
                    .drop_note_if(mid.get(), |n| answer_head.starts_with(&n.to_lowercase()))
                    .await
            {
                let edit = serenity::builder::EditMessage::new()
                    .content(super::tool_group::render_content(&group))
                    .embeds(super::tool_group::render_embeds(&group))
                    .components(super::tool_group::render_components(&group, mid.get()));
                if let Some(Err(e)) =
                    super::governor::edit_chrome(&discord_state.governor, channel, &http, mid, edit)
                        .await
                {
                    tracing::debug!("Discord: follow-up tap trace mirror-note drop failed: {e}");
                }
            }

            let waiting = {
                let (_, bg_count) = bg_indicator_for(&agent, session_id);
                waiting_verb(bg_count, subagent_counts_for(&agent, session_id))
            };
            if let Some(mid) = *turn_group_mid.lock().await
                && let Some(group) = discord_state
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
                    // #1987: the tap turn shares the session's waiting-group
                    // slot so resume.rs can flip this line when work drains.
                    discord_state
                        .register_waiting_group(session_id, mid.get())
                        .await;
                }
                let edit = serenity::builder::EditMessage::new()
                    .content(super::tool_group::render_content(&group))
                    .embeds(super::tool_group::render_embeds(&group))
                    .components(super::tool_group::render_components(&group, mid.get()));
                if let Err(e) = super::governor::edit_content(
                    &discord_state.governor,
                    channel,
                    &http,
                    mid,
                    super::governor::Surface::Final,
                    edit,
                )
                .await
                {
                    tracing::debug!("Discord: follow-up tap settled stamp failed: {e}");
                }
                // #1912: the card gets the same FINAL treatment as this stamp.
                super::plan_card::refresh_plan_card(
                    &discord_state,
                    &http,
                    channel,
                    session_id,
                    true,
                )
                .await;
            }

            if !skip_final_post {
                for chunk in super::handler::split_message(&text_only, 2000) {
                    if let Err(e) = super::governor::say_recorded(
                        &discord_state.governor,
                        channel,
                        &http,
                        super::governor::Surface::Send,
                        &chunk,
                    )
                    .await
                    {
                        tracing::error!("Discord: follow-up tap reply delivery failed: {e}");
                    }
                }
            }
        }
        Err(ref e) if matches!(e, crate::brain::agent::AgentError::Cancelled) => {
            tracing::info!("Discord: follow-up tap turn cancelled for session {session_id}");
            // #1987: terminal settle so the tap group's 🕒 clock stops with
            // the cancelled turn instead of running to the orphan cap.
            if let Some(mid) = *turn_group_mid.lock().await
                && let Some(group) = discord_state
                    .settle_tool_group(mid.get(), None, None, Some(FlowOutcome::Cancelled))
                    .await
            {
                let edit = serenity::builder::EditMessage::new()
                    .content(super::tool_group::render_content(&group))
                    .embeds(super::tool_group::render_embeds(&group))
                    .components(super::tool_group::render_components(&group, mid.get()));
                if let Err(e) = super::governor::edit_content(
                    &discord_state.governor,
                    channel,
                    &http,
                    mid,
                    super::governor::Surface::Final,
                    edit,
                )
                .await
                {
                    tracing::debug!("Discord: follow-up cancelled settle stamp failed: {e}");
                }
            }
        }
        Err(e) => {
            tracing::error!("Discord: follow-up tap agent error: {e}");
            let error_msg = format!("❌ Error\n\n{}", crate::brain::agent::format_user_error(&e));
            if let Err(e) = super::governor::say_recorded(
                &discord_state.governor,
                channel,
                &http,
                super::governor::Surface::Send,
                error_msg,
            )
            .await
            {
                tracing::warn!("Discord: follow-up tap error post failed: {e}");
            }
            // #1987: the tap turn settles on failure too, and #1911 gives
            // that settle the real state: `⏱ Timed out` for a timeout,
            // `❌ Failed` for anything else.
            if let Some(mid) = *turn_group_mid.lock().await
                && let Some(group) = discord_state
                    .settle_tool_group(mid.get(), None, None, Some(outcome_for_error(&e)))
                    .await
            {
                let edit = serenity::builder::EditMessage::new()
                    .content(super::tool_group::render_content(&group))
                    .embeds(super::tool_group::render_embeds(&group))
                    .components(super::tool_group::render_components(&group, mid.get()));
                if let Err(e) = super::governor::edit_content(
                    &discord_state.governor,
                    channel,
                    &http,
                    mid,
                    super::governor::Surface::Final,
                    edit,
                )
                .await
                {
                    tracing::debug!("Discord: follow-up error settle stamp failed: {e}");
                }
            }
        }
    }
}
