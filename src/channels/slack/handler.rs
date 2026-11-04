//! Slack Message Handler
//!
//! Processes incoming Slack messages: text, allowlist enforcement,
//! session routing (owner shares TUI session, others get per-user sessions).
//!
//! Uses a module-level static for handler state because slack-morphism's
//! Socket Mode callbacks require plain function pointers (not closures).

use super::SlackState;
use super::flow_progress::{FlowProgress, build_flow_progress};
use super::governor::GatedWrites;
use crate::brain::agent::AgentService;
use crate::channels::background_work::{bg_indicator_for, subagent_counts_for, waiting_verb};
use crate::channels::group_history;
use crate::config::{Config, RespondTo};
use crate::db::ChannelMessageRepository;
use crate::db::models::ChannelMessage as DbChannelMessage;
use crate::services::SessionService;
use crate::utils::sanitize::redact_secrets;
use crate::utils::truncate_str;
use slack_morphism::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use uuid::Uuid;

/// Socket Mode interaction callback — handles button clicks for tool approvals.
pub async fn on_interaction(
    event: SlackInteractionEvent,
    client: Arc<SlackHyperClient>,
    _states: SlackClientEventsUserState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if let SlackInteractionEvent::BlockActions(block_actions) = event {
        let state = match HANDLER_STATE.get() {
            Some(s) => s.clone(),
            None => {
                tracing::warn!("Slack: interaction received but HANDLER_STATE not initialized");
                return Ok(());
            }
        };

        if let Some(actions) = block_actions.actions {
            for action in actions {
                let action_id = action.action_id.0.as_str();
                tracing::info!("Slack callback received: action_id={}", action_id);

                // Tool-group Expand/Collapse toggle (#373): flip stored
                // state, re-render the same message in place.
                if let Some(ts) = action_id.strip_prefix("toolgroup:") {
                    if let Some(group) = state.slack_state.toggle_tool_group(ts).await {
                        let token =
                            SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                        let session = client.open_session(&token);
                        let slack_ts = SlackTs::new(ts.to_string());
                        let content = super::tool_group::render(&group, &slack_ts);
                        let upd = SlackApiChatUpdateRequest::new(
                            group.channel.clone(),
                            content,
                            slack_ts,
                        );
                        if let Err(e) = session.update(&upd).await {
                            tracing::warn!("Slack: tool group toggle update failed: {e}");
                        }
                    } else {
                        tracing::debug!("Slack: tool group {ts} aged out — toggle ignored");
                    }
                    continue;
                }

                // Optional follow-up suggestion tapped (#599): inject the chosen
                // suggestion as the user's next message (a fresh turn).
                if let Some(rest) = action_id.strip_prefix(super::suggest_options::FOLLOWUP_PREFIX)
                {
                    if let Some((sid_str, idx_str)) = rest.rsplit_once(':')
                        && let Ok(sid) = uuid::Uuid::parse_str(sid_str)
                        && let Ok(idx) = idx_str.parse::<usize>()
                        && let Some(text) = state.slack_state.take_pending_followup(sid, idx).await
                        && let Some(ref channel) = block_actions.channel
                    {
                        // #1838: route the tapped suggestion through the same
                        // tool-loop display path `handle_message` runs, so the
                        // turn is visible from the second the tap works.
                        let handler_clone = state.clone();
                        let channel_id_clone = channel.id.clone();
                        let client_clone = client.clone();
                        tokio::spawn(async move {
                            let token = SlackApiToken::new(SlackApiTokenValue::from(
                                handler_clone.current_bot_token(),
                            ));
                            let session = client_clone.open_session(&token);
                            // Echo the pick.
                            let echo = SlackApiChatPostMessageRequest::new(
                                channel_id_clone.clone(),
                                SlackMessageContent::new()
                                    .with_text(format!("\u{25b6}\u{fe0f} {text}")),
                            );
                            if let Err(e) = session.post(&echo).await {
                                tracing::warn!(error = %e, "failed to post Slack message");
                            }
                            run_followup_turn(
                                handler_clone,
                                client_clone,
                                sid,
                                channel_id_clone.to_string(),
                                text,
                            )
                            .await;
                        });
                    }
                    continue;
                }

                // Provider picker callback → show models for that provider
                if let Some(provider_name) = action_id.strip_prefix("provider:") {
                    let resp = crate::channels::commands::models_for_provider(provider_name).await;
                    tracing::info!("Slack: showing models for provider {}", provider_name);
                    if let Some(ref channel) = block_actions.channel {
                        let token =
                            SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                        let session = client.open_session(&token);

                        // Agent-handled providers (OpenRouter 300+ models, custom)
                        if resp.agent_handled {
                            let session_id = *state.shared_session.lock().await;
                            let display =
                                crate::channels::commands::provider_display_name(provider_name);
                            let config = crate::config::Config::current();
                            if let Ok(new_provider) =
                                crate::brain::provider::factory::create_provider_by_name(
                                    &config,
                                    provider_name,
                                )
                                .await
                            {
                                match session_id {
                                    Some(sid) => state.agent.swap_provider_for_session(
                                        sid,
                                        new_provider.clone(),
                                        new_provider.default_model().to_string(),
                                    ),
                                    None => state.agent.swap_provider(new_provider),
                                }
                            }
                            if !resp.current_model.is_empty()
                                && let Err(e) = crate::channels::commands::switch_model(
                                    &state.agent,
                                    &resp.current_model,
                                    session_id,
                                    Some(provider_name),
                                )
                                .await
                            {
                                tracing::warn!(error = %e, "failed to switch model in Slack handler");
                            }
                            if let Some(sid) = session_id {
                                let prompt = if resp.current_model.is_empty() {
                                    format!(
                                        "[System: User selected {} provider but no default model is set. \
                                         Ask them which model they want. Use config_manager tool to read \
                                         providers section, then set the default_model. Keep current provider \
                                         until a model is chosen.]",
                                        display
                                    )
                                } else {
                                    format!(
                                        "[System: User switched to {} provider with model {}. \
                                         Confirm the switch. Ask if they want a different model — \
                                         if so, use config_manager to update providers.{}.default_model \
                                         and confirm.]",
                                        display,
                                        resp.current_model,
                                        if provider_name == "openrouter" {
                                            "openrouter"
                                        } else {
                                            provider_name
                                        }
                                    )
                                };
                                let agent_clone = state.agent.clone();
                                let bot_token = state.current_bot_token();
                                let channel_id_clone = channel.id.clone();
                                let client_clone = client.clone();
                                tokio::spawn(async move {
                                    match agent_clone.send_message(sid, prompt, None).await {
                                        Ok(r) => {
                                            let token = SlackApiToken::new(
                                                SlackApiTokenValue::from(bot_token),
                                            );
                                            let session = client_clone.open_session(&token);
                                            let request = SlackApiChatPostMessageRequest::new(
                                                channel_id_clone,
                                                SlackMessageContent::new().with_text(r.content),
                                            );
                                            if let Err(e) = session.post(&request).await {
                                                tracing::warn!(error = %e, "failed to post Slack message");
                                            }
                                        }
                                        Err(e) => tracing::error!("Agent follow-up failed: {}", e),
                                    }
                                });
                            }
                            continue;
                        }

                        let header = SlackBlock::Section(SlackSectionBlock::new().with_text(
                            SlackBlockText::MarkDown(SlackBlockMarkDownText::new(
                                resp.text.clone(),
                            )),
                        ));
                        let buttons: Vec<SlackActionBlockElement> = resp
                            .models
                            .iter()
                            .take(25)
                            .map(|m| {
                                let label = if *m == resp.current_model {
                                    format!("✓ {}", m)
                                } else {
                                    m.clone()
                                };
                                SlackActionBlockElement::Button(SlackBlockButtonElement::new(
                                    SlackActionId::new(format!(
                                        "model:{}:{}",
                                        resp.provider_name, m
                                    )),
                                    SlackBlockPlainTextOnly::from(SlackBlockPlainText::new(label)),
                                ))
                            })
                            .collect();
                        let mut blocks = vec![header];
                        for chunk in buttons.chunks(5) {
                            blocks
                                .push(SlackBlock::Actions(SlackActionsBlock::new(chunk.to_vec())));
                        }
                        let request = SlackApiChatPostMessageRequest::new(
                            channel.id.clone(),
                            SlackMessageContent::new().with_blocks(blocks),
                        );
                        if let Err(e) = session.post(&request).await {
                            tracing::warn!(error = %e, "failed to post Slack message");
                        }
                    }
                    continue;
                }

                // Model switch callback (format: model:<provider>:<model>)
                if let Some(rest) = action_id.strip_prefix("model:") {
                    let (provider_name, model_name) = if let Some((p, m)) = rest.split_once(':') {
                        (Some(p), m)
                    } else {
                        (None, rest)
                    };
                    // Resolve the session first so the provider swap lands on
                    // the right per-session slot.
                    let session_id = *state.shared_session.lock().await;
                    let mut provider_err: Option<String> = None;
                    if let Some(pname) = provider_name {
                        match crate::config::Config::load() {
                            Ok(config) => {
                                match crate::brain::provider::factory::create_provider_by_name(
                                    &config, pname,
                                )
                                .await
                                {
                                    Ok(new_provider) => match session_id {
                                        Some(sid) => state.agent.swap_provider_for_session(
                                            sid,
                                            new_provider.clone(),
                                            new_provider.default_model().to_string(),
                                        ),
                                        None => state.agent.swap_provider(new_provider),
                                    },
                                    Err(e) => {
                                        provider_err = Some(format!(
                                            "Failed to create provider '{}': {}",
                                            pname, e
                                        ))
                                    }
                                }
                            }
                            Err(e) => provider_err = Some(format!("Failed to load config: {}", e)),
                        }
                    }
                    let reply = if let Some(err) = provider_err {
                        tracing::warn!("Slack: provider switch failed: {}", err);
                        format!("⚠️ {}", err)
                    } else {
                        match crate::channels::commands::switch_model(
                            &state.agent,
                            model_name,
                            session_id,
                            provider_name,
                        )
                        .await
                        {
                            Ok(_) => {
                                tracing::info!("Slack: model switched to {}", model_name);
                                format!("✅ Model switched to `{}`", model_name)
                            }
                            Err(e) => {
                                tracing::warn!("Slack: model switch failed: {}", e);
                                format!("⚠️ {}", e)
                            }
                        }
                    };
                    if let Some(ref channel) = block_actions.channel {
                        let token =
                            SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                        let session = client.open_session(&token);
                        let request = SlackApiChatPostMessageRequest::new(
                            channel.id.clone(),
                            SlackMessageContent::new().with_text(reply),
                        );
                        if let Err(e) = session.post(&request).await {
                            tracing::warn!(error = %e, "failed to post Slack message");
                        }
                    }
                    continue;
                }

                // Session switch callback
                if let Some(session_id_str) = action_id.strip_prefix("session:") {
                    if let Ok(new_id) = session_id_str.parse::<Uuid>() {
                        let cfg = state.config_rx.borrow().clone();
                        let caller_id = block_actions
                            .user
                            .as_ref()
                            .map(|u| u.id.0.as_str())
                            .unwrap_or("");
                        let is_owner = cfg.channels.slack.is_owner(caller_id);

                        if is_owner {
                            *state.shared_session.lock().await = Some(new_id);
                        } else {
                            state
                                .extra_sessions
                                .lock()
                                .await
                                .insert(caller_id.to_string(), (new_id, std::time::Instant::now()));
                        }
                        if let Some(ref channel) = block_actions.channel {
                            state
                                .slack_state
                                .register_session_channel(new_id, channel.id.0.to_string(), None)
                                .await;
                            let token = SlackApiToken::new(SlackApiTokenValue::from(
                                state.current_bot_token(),
                            ));
                            let session = client.open_session(&token);
                            let display = match state.session_svc.get_session(new_id).await {
                                Ok(Some(s)) => s.title.unwrap_or_else(|| {
                                    session_id_str[..8.min(session_id_str.len())].to_string()
                                }),
                                _ => session_id_str[..8.min(session_id_str.len())].to_string(),
                            };
                            let request = SlackApiChatPostMessageRequest::new(
                                channel.id.clone(),
                                SlackMessageContent::new()
                                    .with_text(format!("✅ Switched to session `{}`", display)),
                            );
                            if let Err(e) = session.post(&request).await {
                                tracing::warn!(error = %e, "failed to post Slack message");
                            }
                        }
                    }
                    continue;
                }

                let (approved, always, yolo, id) =
                    if let Some(id) = action_id.strip_prefix("approve:") {
                        (true, false, false, id.to_string())
                    } else if let Some(id) = action_id.strip_prefix("always:") {
                        (true, true, false, id.to_string())
                    } else if let Some(id) = action_id.strip_prefix("yolo:") {
                        (true, true, true, id.to_string())
                    } else if let Some(id) = action_id.strip_prefix("deny:") {
                        (false, false, false, id.to_string())
                    } else {
                        tracing::warn!("Slack: unknown action_id: {}", action_id);
                        continue;
                    };
                // OC-01: the approval buttons sit in a channel where any member
                // can click them, so re-check the clicker is the owner before
                // acting, same as the session branch above. A non-owner click
                // otherwise runs the pending tool, and a YOLO click persists
                // auto-always instance-wide.
                {
                    let cfg = state.config_rx.borrow().clone();
                    let caller_id = block_actions
                        .user
                        .as_ref()
                        .map(|u| u.id.0.as_str())
                        .unwrap_or("");
                    if !cfg.channels.slack.is_owner(caller_id) {
                        tracing::warn!(
                            "Slack: non-owner {} clicked '{}' — refused (OC-01)",
                            caller_id,
                            action_id
                        );
                        continue;
                    }
                }
                if yolo {
                    crate::utils::persist_auto_always_policy();
                }
                let resolved = state
                    .slack_state
                    .resolve_pending_approval(&id, approved, always)
                    .await;
                tracing::info!(
                    "Slack approval resolved: id={}, approved={}, always={}, found_pending={}",
                    id,
                    approved,
                    always,
                    resolved
                );
                if !resolved {
                    tracing::warn!(
                        "Slack: no pending approval for id={} — may have timed out or already resolved",
                        id
                    );
                }
            }
        }
    }
    Ok(())
}

/// Global handler state — set once by the agent before starting the listener.
pub static HANDLER_STATE: OnceLock<Arc<HandlerState>> = OnceLock::new();

/// Shared state for the Slack message handler callbacks.
pub struct HandlerState {
    pub agent: Arc<AgentService>,
    pub session_svc: SessionService,
    pub extra_sessions: Arc<Mutex<HashMap<String, (Uuid, std::time::Instant)>>>,
    pub shared_session: Arc<Mutex<Option<Uuid>>>,
    pub slack_state: Arc<SlackState>,
    pub bot_token: String,
    pub bot_user_id: Option<String>,
    pub config_rx: tokio::sync::watch::Receiver<Config>,
    pub channel_msg_repo: ChannelMessageRepository,
    /// Dedup: recently seen message timestamps (Slack retries if ack is slow).
    /// Uses VecDeque for FIFO eviction — oldest entries are dropped when limit
    /// is reached, preserving the rest so retries never slip through a full clear.
    pub seen_ts: Mutex<VecDeque<String>>,
}

impl HandlerState {
    /// Get the current bot token — prefers hot-reloaded config, falls back to startup token.
    pub fn current_bot_token(&self) -> String {
        self.config_rx
            .borrow()
            .channels
            .slack
            .token
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.bot_token.clone())
    }
}

/// Split a message into chunks that fit Slack's limit (conservative 3000 chars).
pub fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max_len).min(text.len());
        // Ensure end falls on a char boundary (back up if inside a multi-byte char)
        while end < text.len() && !text.is_char_boundary(end) {
            end -= 1;
        }
        let break_at = if end < text.len() {
            text[start..end]
                .rfind('\n')
                .filter(|&pos| pos > end - start - 200)
                .map(|pos| start + pos + 1)
                .unwrap_or(end)
        } else {
            end
        };
        chunks.push(&text[start..break_at]);
        start = break_at;
    }
    chunks
}

/// Socket Mode push event callback (function pointer — required by slack-morphism).
///
/// Returns immediately so Slack gets the ack within 3 s (prevents retries).
/// Actual processing is spawned as a background task.
/// Deduplicates by message timestamp to drop Slack retries.
pub async fn on_push_event(
    event: SlackPushEventCallback,
    client: Arc<SlackHyperClient>,
    _states: SlackClientEventsUserState,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::debug!("Slack: received push event");
    match event.event {
        SlackEventCallbackBody::Message(msg) => {
            let ts = msg.origin.ts.to_string();
            let channel = msg
                .origin
                .channel
                .as_ref()
                .map(|c| c.0.as_str())
                .unwrap_or("");
            let user = msg.sender.user.as_ref().map(|u| u.0.as_str()).unwrap_or("");
            if !dedup_ts(channel, user, &ts).await {
                return Ok(());
            }
            tracing::debug!(
                "Slack: message event from user={:?}, channel={:?}, bot_id={:?}",
                msg.sender.user,
                msg.origin.channel,
                msg.sender.bot_id
            );
            tokio::spawn(async move {
                handle_message(&msg, client, false).await;
            });
        }
        SlackEventCallbackBody::ReactionAdded(ev) => {
            tokio::spawn(async move {
                super::reactions::handle_reaction_added(ev, client).await;
            });
        }
        SlackEventCallbackBody::AppMention(mention) => {
            let ts = mention.origin.ts.to_string();
            let channel = mention.channel.0.as_str();
            let user = mention.user.0.as_str();
            if !dedup_ts(channel, user, &ts).await {
                return Ok(());
            }
            tracing::info!(
                "Slack: app_mention from user={:?}, channel={:?}, text={:?}",
                mention.user,
                mention.channel,
                mention
                    .content
                    .text
                    .as_ref()
                    .map(|t| crate::utils::truncate_str(t, 80)),
            );
            // Convert app_mention into a SlackMessageEvent so handle_message can process it
            let msg = SlackMessageEvent {
                origin: SlackMessageOrigin {
                    ts: mention.origin.ts,
                    channel: Some(mention.channel),
                    channel_type: None,
                    thread_ts: mention.origin.thread_ts,
                    client_msg_id: None,
                },
                content: Some(mention.content),
                sender: SlackMessageSender {
                    user: Some(mention.user),
                    bot_id: None,
                    username: None,
                    display_as_bot: None,
                    user_profile: None,
                    bot_profile: None,
                },
                subtype: None,
                hidden: None,
                message: None,
                previous_message: None,
                deleted_ts: None,
            };
            tokio::spawn(async move {
                handle_message(&msg, client, true).await;
            });
        }
        other => {
            tracing::debug!(
                "Slack: unhandled event type: {:?}",
                std::any::type_name_of_val(&other)
            );
        }
    }
    Ok(())
}

/// Returns `true` if this is the first time we see this message (proceed).
/// Returns `false` if it's a duplicate (skip).
/// Uses composite key (channel + user + ts) to catch Slack retries that
/// arrive through different event types or with slightly different metadata.
/// FIFO eviction (VecDeque) so the dedup window is never fully wiped.
async fn dedup_ts(channel: &str, user: &str, ts: &str) -> bool {
    let state = match HANDLER_STATE.get() {
        Some(s) => s,
        None => return true,
    };
    // Composite key catches retries across event types
    let key = format!("{}:{}:{}", channel, user, ts);
    let mut seen = state.seen_ts.lock().await;
    if seen.contains(&key) {
        tracing::debug!("Slack: dropping duplicate event key={}", key);
        return false;
    }
    // FIFO eviction: drop oldest when limit reached, never clear the whole window
    if seen.len() >= 500 {
        seen.pop_front();
    }
    seen.push_back(key);
    true
}

/// Socket Mode error handler.
pub fn on_error(
    err: Box<dyn std::error::Error + Send + Sync>,
    _client: Arc<SlackHyperClient>,
    _states: SlackClientEventsUserState,
) -> HttpStatusCode {
    tracing::error!("Slack: socket mode error: {}", err);
    HttpStatusCode::OK
}

/// Handle an incoming Slack message event.
pub(crate) fn handler_state() -> Option<Arc<HandlerState>> {
    HANDLER_STATE.get().cloned()
}

/// Render the turn's step group into Slack, creating the message on the first
/// step and updating it on every one after.
///
/// Shared by tool steps and narration so a note folds into the same collapsible
/// block as the tools it sits between (#943). Narration used to take a separate
/// `chat_post_message` path, which put the agent's thinking in the channel as
/// an ordinary message.
/// Re-render throttle for the live flow group (#1807): the `🕒` clock must
/// roll between tool events like Telegram's 1500 ms edit loop, but Slack
/// `chat.update` is rate-limited, so the tick is 4 s. One ticker per turn,
/// spawned where the turn's group-ts slot is created: it idles until the
/// first step posts the group, re-renders each tick, and stops when the
/// group settles TERMINALLY (settle keeps the last word via the race fixup
/// below; a waiting settle is not terminal, the clock keeps rolling over it
/// until the flip, #1988), when retention prunes it, or at the 30 min cap.
const FLOW_TICKER_INTERVAL: std::time::Duration = std::time::Duration::from_secs(4);
const FLOW_TICKER_CAP: std::time::Duration = std::time::Duration::from_secs(30 * 60);

pub(crate) fn spawn_flow_ticker(
    client: Arc<SlackHyperClient>,
    slack_state: Arc<SlackState>,
    group_ts: Arc<Mutex<Option<SlackTs>>>,
) {
    tokio::spawn(async move {
        let born = std::time::Instant::now();
        loop {
            tokio::time::sleep(FLOW_TICKER_INTERVAL).await;
            if born.elapsed() > FLOW_TICKER_CAP {
                break;
            }
            let Some(ts) = group_ts.lock().await.clone() else {
                // Group not born yet: the turn has not reached its first
                // step. Keep waiting.
                continue;
            };
            let Some(group) = slack_state.tool_group_snapshot(ts.as_ref()).await else {
                break; // pruned by retention mid-turn
            };
            if group.settled_terminal() {
                break; // terminal settle already posted the final line
            }
            let content = super::tool_group::render(&group, &ts);
            let token = SlackApiToken::new(SlackApiTokenValue::from(
                slack_state
                    .bot_token
                    .lock()
                    .await
                    .clone()
                    .unwrap_or_default(),
            ));
            let session = client.open_session(&token);
            let upd = SlackApiChatUpdateRequest::new(group.channel.clone(), content, ts.clone());
            if let Some(Err(e)) = session.update_chrome(&upd).await {
                tracing::warn!("Slack: flow ticker chat_update failed (ts={ts}): {e}");
            }
            // Race guard: settle may have stamped and posted while this
            // tick's update was in flight. The settled line must be last,
            // so if the group settled behind us, re-render its content once.
            match slack_state.tool_group_snapshot(ts.as_ref()).await {
                Some(re) if re.settled_terminal() => {
                    let content = super::tool_group::render(&re, &ts);
                    let upd = SlackApiChatUpdateRequest::new(re.channel, content, ts);
                    if let Err(e) = session.update(&upd).await {
                        tracing::warn!("Slack: flow ticker settle fixup failed: {e}");
                    }
                    break;
                }
                Some(_) => {}  // still live: keep ticking
                None => break, // pruned mid-tick
            }
        }
    });
}

pub(crate) async fn sync_step_group<'a>(
    session: &SlackClientSession<'a, slack_morphism::hyper_tokio::SlackClientHyperHttpsConnector>,
    slack_state: &Arc<SlackState>,
    channel: SlackChannelId,
    thread_ts: Option<SlackTs>,
    group_ts: &Arc<Mutex<Option<SlackTs>>>,
    entries: Vec<super::tool_group::GroupEntry>,
) {
    use super::tool_group::GroupState;
    let mut gts = group_ts.lock().await;
    match gts.as_ref() {
        Some(ts) => {
            let group = slack_state
                .upsert_tool_group(ts.to_string(), GroupState::new(channel.clone(), entries))
                .await;
            let content = super::tool_group::render(&group, ts);
            let upd = SlackApiChatUpdateRequest::new(channel, content, ts.clone());
            if let Some(Err(e)) = session.update_chrome(&upd).await {
                tracing::warn!("Slack: chat_update failed (step group append): {e}");
            }
        }
        None => {
            // First step: post with a placeholder ts in the button id, then
            // re-render with the real ts.
            let group = GroupState::new(channel.clone(), entries);
            let content = super::tool_group::render(&group, &SlackTs::new("0".into()));
            let mut req = SlackApiChatPostMessageRequest::new(channel.clone(), content);
            if let Some(ref ts) = thread_ts {
                req = req.with_thread_ts(ts.clone());
            }
            match session.post(&req).await {
                Ok(resp) => {
                    let fixed = super::tool_group::render(&group, &resp.ts);
                    let upd = SlackApiChatUpdateRequest::new(channel, fixed, resp.ts.clone());
                    if let Err(e) = session.update(&upd).await {
                        tracing::warn!("Slack: chat_update failed (step group ts fixup): {e}");
                    }
                    slack_state
                        .upsert_tool_group(resp.ts.to_string(), group)
                        .await;
                    *gts = Some(resp.ts);
                }
                Err(e) => tracing::warn!("Slack: failed to post step group message: {e}"),
            }
        }
    }
}

/// Post the salvaged answer as its own message.
///
/// The salvage path for a turn whose final response is empty: the folded
/// narration is the only answer there is, promoted out of the step group
/// (#1805). Clean prose: the ctx budget lives on the settled line only
/// (#1806).
pub(crate) async fn post_final_text<'a>(
    session: &SlackClientSession<'a, slack_morphism::hyper_tokio::SlackClientHyperHttpsConnector>,
    channel_id: &str,
    thread_ts: Option<&SlackTs>,
    text: &str,
) {
    let mrkdwn = crate::utils::slack_fmt::markdown_to_mrkdwn(text);
    let blocks = super::blocks::blocks_from_mrkdwn(&mrkdwn);
    let mut req = SlackApiChatPostMessageRequest::new(
        SlackChannelId::new(channel_id.to_string()),
        SlackMessageContent::new()
            .with_text(mrkdwn)
            .with_blocks(blocks),
    );
    if let Some(ts) = thread_ts {
        req = req.with_thread_ts(ts.clone());
    }
    if let Err(e) = session.post(&req).await {
        // The answer is lost if this fails, so it is an error, not a debug.
        tracing::error!("Slack: failed to post salvaged final response: {e}");
    }
}

/// Settle the turn's step group (#1797): stamp the delivery outcome (or the
/// background-wait state) onto the group and re-render its message once the
/// answer is out. The Slack mirror of Telegram's settled flow header, and
/// since #1806 the turn's ONLY ctx-budget carrier: answer messages are clean
/// prose. A no-op when the turn never opened a group (plain
/// tool-less replies). A Finished turn that ends with detached background
/// tasks or working sub-agents settles into the waiting state instead (both
/// registries folded by the shared helpers, #1988); the flip back to
/// Finished runs from `resume.rs` when a completion is delivered, and at
/// the channel's next inbound event as a backstop (see `flip_waiting_group`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn settle_step_group<'a>(
    session: &SlackClientSession<'a, slack_morphism::hyper_tokio::SlackClientHyperHttpsConnector>,
    slack_state: &Arc<SlackState>,
    agent: &crate::brain::agent::AgentService,
    session_id: Uuid,
    channel: SlackChannelId,
    channel_id: &str,
    group_ts: &Arc<Mutex<Option<SlackTs>>>,
    outcome: super::tool_group::TurnOutcome,
    ctx_footer: &str,
) {
    let Some(ts) = group_ts.lock().await.clone() else {
        return;
    };
    let (_, task_count) = bg_indicator_for(agent, session_id);
    let waiting = waiting_verb(task_count, subagent_counts_for(agent, session_id));
    let ctx = (!ctx_footer.is_empty()).then(|| ctx_footer.to_string());
    let Some(group) = slack_state
        .settle_tool_group(ts.as_ref(), outcome, waiting, ctx)
        .await
    else {
        return;
    };
    let content = super::tool_group::render(&group, &ts);
    let upd = SlackApiChatUpdateRequest::new(channel, content, ts.clone());
    if let Err(e) = session.update(&upd).await {
        tracing::warn!("Slack: chat_update failed (step group settle, ts={ts}): {e}");
    }
    if group.settled.as_ref().is_some_and(|s| s.waiting) {
        slack_state
            .note_waiting_group(channel_id.to_string(), ts.to_string(), session_id)
            .await;
    }
}

/// Flip a background-waiting step group to its terminal line (#1797).
///
/// A group that settled while background work was still running reads
/// "⏳ Waiting for ..." with a rolling clock. #1988 made the flip event
/// driven: `resume.rs` calls this right after a completion is delivered,
/// and handle_message still calls it at its top as a backstop. Recount
/// BOTH registries via the shared helpers: drained work flips the line to
/// `✅ Finished · N tool calls · ctx · ⏱️`, anything still alive refreshes
/// the count and stays waiting. The ctx string is the one captured at
/// first settle, so the budget never changes under the user. Cheap no-op
/// when nothing on this channel is waiting.
pub(crate) async fn flip_waiting_group(
    client: &Arc<SlackHyperClient>,
    slack_state: &Arc<SlackState>,
    agent: &AgentService,
    bot_token: String,
    channel_id: &str,
) {
    let Some((ts, session_id)) = slack_state.take_waiting_group_for(channel_id).await else {
        return;
    };
    let (_, task_count) = bg_indicator_for(agent, session_id);
    let waiting = waiting_verb(task_count, subagent_counts_for(agent, session_id));
    let token = SlackApiToken::new(SlackApiTokenValue::from(bot_token));
    let session = client.open_session(&token);
    let Some(group) = slack_state
        .settle_tool_group(
            &ts,
            super::tool_group::TurnOutcome::Finished,
            waiting,
            None, // keep the ctx stamped at first settle
        )
        .await
    else {
        // Aged out of retention: nothing left to flip.
        return;
    };
    let content = super::tool_group::render(&group, &SlackTs::new(ts.clone()));
    let upd = SlackApiChatUpdateRequest::new(
        SlackChannelId::new(channel_id.to_string()),
        content,
        SlackTs::new(ts.clone()),
    );
    if let Err(e) = session.update(&upd).await {
        tracing::warn!("Slack: chat_update failed (waiting-group flip, ts={ts}): {e}");
    }
    if group.settled.as_ref().is_some_and(|s| s.waiting) {
        slack_state
            .note_waiting_group(channel_id.to_string(), ts, session_id)
            .await;
    }
}

async fn handle_message(
    msg: &SlackMessageEvent,
    client: Arc<SlackHyperClient>,
    is_app_mention: bool,
) {
    let state = match HANDLER_STATE.get() {
        Some(s) => s.clone(),
        None => {
            tracing::error!("Slack: handler state not initialized");
            return;
        }
    };

    // Skip bot messages
    if msg.sender.bot_id.is_some() {
        tracing::debug!(
            "Slack: skipping bot message (bot_id={:?})",
            msg.sender.bot_id
        );
        return;
    }

    // Extract user ID
    let user_id = match &msg.sender.user {
        Some(uid) => uid.to_string(),
        None => {
            tracing::debug!("Slack: message has no sender user ID, ignoring");
            return;
        }
    };

    // Extract channel ID
    let channel_id = match &msg.origin.channel {
        Some(ch) => ch.to_string(),
        None => {
            tracing::debug!("Slack: message has no channel ID, ignoring");
            return;
        }
    };

    // A previous turn on this channel may have settled with background work
    // still running; this inbound event is the backstop flip for its step
    // group (#1797, #1988: the primary flip is `resume.rs` on completion).
    // No-op when nothing is waiting.
    flip_waiting_group(
        &client,
        &state.slack_state,
        &state.agent,
        state.current_bot_token(),
        &channel_id,
    )
    .await;

    // Resolve user display name via Slack API (cached per conversation turn)
    let user_name = {
        let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
        let session = client.open_session(&token);
        match session
            .users_info(&SlackApiUsersInfoRequest::new(SlackUserId::new(
                user_id.clone(),
            )))
            .await
        {
            Ok(resp) => resp
                .user
                .profile
                .as_ref()
                .and_then(|p| {
                    p.display_name
                        .clone()
                        .filter(|n| !n.is_empty())
                        .or_else(|| p.real_name.clone())
                })
                .unwrap_or_else(|| user_id.clone()),
            Err(e) => {
                tracing::debug!("Slack: failed to resolve user name for {}: {}", user_id, e);
                user_id.clone()
            }
        }
    };

    // Resolve channel name via Slack API
    let channel_name = {
        let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
        let session = client.open_session(&token);
        match session
            .conversations_info(&SlackApiConversationsInfoRequest::new(SlackChannelId::new(
                channel_id.clone(),
            )))
            .await
        {
            Ok(resp) => resp
                .channel
                .name
                .map(|n| format!("#{n}"))
                .unwrap_or_else(|| channel_id.clone()),
            Err(e) => {
                tracing::debug!(
                    "Slack: failed to resolve channel name for {}: {}",
                    channel_id,
                    e
                );
                channel_id.clone()
            }
        }
    };

    // Extract text (may be empty if user sent files only)
    let text = msg
        .content
        .as_ref()
        .and_then(|c| c.text.clone())
        .unwrap_or_default();

    // Check for files
    let files: Vec<_> = msg
        .content
        .as_ref()
        .and_then(|c| c.files.as_ref())
        .map(|v| v.as_slice())
        .unwrap_or(&[])
        .to_vec();

    // Require at least text or files
    if text.is_empty() && files.is_empty() {
        tracing::debug!("Slack: message has no text and no files, ignoring");
        return;
    }

    // Helper: passively capture a channel message for history
    let store_channel_msg = |text: String| {
        let repo = state.channel_msg_repo.clone();
        let ch_id = channel_id.clone();
        let uid = user_id.clone();
        let uname = user_name.clone();
        let ch_name = channel_name.clone();
        // Persist which thread the message belongs to. Without this every Slack
        // row lands with thread_id NULL, and a thread-scoped recent() lookup
        // matches nothing (SQL NULL never equals a value) (#1620).
        let thread_id = msg.origin.thread_ts.as_ref().map(|ts| ts.to_string());
        async move {
            if text.is_empty() {
                return;
            }
            let cm = DbChannelMessage::new(
                "slack".into(),
                ch_id,
                Some(ch_name),
                uid,
                uname,
                text,
                "text".into(),
                None,
            )
            .with_thread(thread_id, None);
            if let Err(e) = repo.insert(&cm).await {
                tracing::warn!("Failed to store Slack channel message: {e}");
            }
        }
    };

    // Read latest config from watch channel — single source of truth
    let cfg = state.config_rx.borrow().clone();
    let sl_cfg = &cfg.channels.slack;
    let allowed: HashSet<String> = sl_cfg.allowed_users.iter().cloned().collect();
    let respond_to = &sl_cfg.respond_to;
    let allowed_channels: HashSet<String> = sl_cfg.allowed_channels.iter().cloned().collect();
    let idle_timeout_hours = sl_cfg.session_idle_hours;
    let voice_config = cfg.voice_config();

    // Deny-by-default allowlist (OC-02). An empty allowlist used to accept
    // everyone, unlike Telegram. Now an unconfigured workspace (no allowed_users
    // and no bot_owner) denies, and a configured one admits only allowlisted
    // users or the owner.
    let is_owner =
        crate::config::owner::is_owner(&sl_cfg.allowed_users, &sl_cfg.bot_owner, &user_id);
    let unconfigured = allowed.is_empty() && sl_cfg.bot_owner.is_empty();
    if unconfigured || !(is_owner || allowed.contains(&user_id)) {
        tracing::debug!(
            "Slack: ignoring message from non-allowed user {} (deny-by-default, OC-02)",
            user_id
        );
        return;
    }

    // respond_to / allowed_channels filtering — DMs (channel starts with 'D') always pass
    let is_dm = channel_id.starts_with('D');
    if !is_dm {
        // Check allowed_channels (empty = all channels allowed)
        if !allowed_channels.is_empty() && !allowed_channels.contains(&channel_id) {
            tracing::debug!(
                "Slack: ignoring message in non-allowed channel {}",
                channel_id
            );
            store_channel_msg(text.clone()).await;
            return;
        }

        match respond_to {
            RespondTo::DmOnly => {
                tracing::debug!("Slack: respond_to=dm_only, ignoring channel message");
                store_channel_msg(text.clone()).await;
                return;
            }
            RespondTo::Mention => {
                // app_mention events are already verified by Slack — trust them
                let mentioned = is_app_mention
                    || if let Some(ref bid) = state.bot_user_id {
                        text.contains(&format!("<@{}>", bid))
                    } else {
                        text.contains("<@U")
                    };
                if !mentioned {
                    tracing::debug!(
                        "Slack: respond_to=mention, bot not mentioned — ignoring (bot_user_id={:?}, text={:?})",
                        state.bot_user_id,
                        crate::utils::truncate_str(&text, 120),
                    );
                    store_channel_msg(text.clone()).await;
                    return;
                }
            }
            RespondTo::All => {} // pass through
            RespondTo::Auto => {
                // Active sender tracking not implemented for Slack yet;
                // fall back to mention-only behaviour (#244).
                let mentioned = is_app_mention
                    || if let Some(ref bid) = state.bot_user_id {
                        text.contains(&format!("<@{}>", bid))
                    } else {
                        text.contains("<@U")
                    };
                if !mentioned {
                    tracing::debug!("Slack: respond_to=auto, bot not mentioned — ignoring");
                    store_channel_msg(text.clone()).await;
                    return;
                }
            }
        }
    }

    // Store the user's message for complete history, in channels and DMs alike,
    // so channel_search can see both sides of a DM (#2034).
    store_channel_msg(text.clone()).await;

    // Strip <@BOT_ID> from text when responding to a mention
    let text = if !is_dm && *respond_to == RespondTo::Mention {
        if let Some(ref bid) = state.bot_user_id {
            text.replace(&format!("<@{}>", bid), "").trim().to_string()
        } else {
            // bot_user_id unknown — strip any <@U...> mention tag
            let re = regex::Regex::new(r"<@U[A-Z0-9]+>").unwrap();
            re.replace_all(&text, "").trim().to_string()
        }
    } else {
        text
    };

    let text_preview = truncate_str(&text, 50);
    tracing::info!("Slack: message from {}: {}", user_id, text_preview);

    // Track owner's channel for proactive messaging
    let is_owner = sl_cfg.is_owner(&user_id);

    if is_owner {
        state
            .slack_state
            .set_owner_channel(channel_id.clone())
            .await;
    }

    // Sessions are ALWAYS isolated per chat — owner DMs no longer share the
    // TUI session. DMs keyed by user_id; channels keyed by channel_id. Title
    // carries a stable `[chat:slack-…]` suffix so auto-rename rewrites the
    // visible label without orphaning the row (issue #121 port of PR #123).
    let session_id = {
        use crate::channels::session_resolve;
        let (id_str, legacy_title) = if is_dm {
            (
                format!("slack-dm-{}", user_id),
                format!("Slack: DM {}", user_id),
            )
        } else {
            (
                format!("slack-{}", channel_id),
                format!("Slack: #{}", channel_id),
            )
        };
        let suffix = session_resolve::chat_id_suffix(&id_str);
        let session_title = format!("{legacy_title} {suffix}");

        match session_resolve::resolve_or_create_channel_session(
            &state.session_svc,
            &suffix,
            &legacy_title,
            &session_title,
            idle_timeout_hours,
            "Slack",
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Slack: failed to resolve session: {e:#} (#442)");
                let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                let session = client.open_session(&token);
                let request = SlackApiChatPostMessageRequest::new(
                    SlackChannelId::new(channel_id.clone()),
                    SlackMessageContent::new().with_text(format!(
                        "⚠️ Could not load this chat's session ({e}). Your history is \
                         intact and this message was NOT processed. Try again, or send \
                         /new if you deliberately want a fresh session."
                    )),
                );
                if let Err(e) = session.post(&request).await {
                    tracing::warn!(error = %e, "failed to post Slack message");
                }
                return;
            }
        }
    };

    // Session gate (#1051, ADR-003): mark group sessions so memory_search
    // keeps external index content out of them by default.
    if !is_dm {
        crate::memory::mark_session_shared(session_id);
    }

    // The user sent their own message — any follow-up suggestion buttons from
    // the previous turn are stale now (#599).
    state.slack_state.clear_pending_followups(session_id).await;

    // Process attached files — images as <<IMG:tmp_path>>, text files extracted inline
    let mut content = text.clone();
    // Set to true if an incoming audio attachment is successfully transcribed.
    // Used to decide whether to mirror the text response as a TTS voice note.
    let mut is_voice = false;
    if !files.is_empty() {
        use crate::utils::{inject_file_content, process_file_with_vision};
        let cfg = match crate::config::Config::load() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Slack: failed to load config: {}", e);
                return;
            }
        };
        let http = reqwest::Client::new();
        for file in &files {
            let mime = file.mimetype.as_ref().map(|m| m.0.as_str()).unwrap_or("");
            let fname = file.name.as_deref().unwrap_or("file");

            // Download file using bot token (Slack private URLs require auth).
            // Try url_private_download first, fall back to url_private if it
            // returns HTML instead of raw file bytes.
            let download_urls: Vec<&str> = file
                .url_private_download
                .as_ref()
                .map(|u| u.as_str())
                .into_iter()
                .chain(file.url_private.as_ref().map(|u| u.as_str()))
                .collect();

            let mut dl_bytes: Option<Vec<u8>> = None;
            for url in &download_urls {
                match http
                    .get(*url)
                    .header(
                        "Authorization",
                        format!("Bearer {}", state.current_bot_token()),
                    )
                    .send()
                    .await
                {
                    Ok(resp) => {
                        let content_type = resp
                            .headers()
                            .get(reqwest::header::CONTENT_TYPE)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();

                        if content_type.starts_with("text/html") {
                            tracing::warn!(
                                "Slack: download returned HTML (Content-Type: {content_type}) for {fname}, trying fallback URL"
                            );
                            continue;
                        }

                        match resp.bytes().await {
                            Ok(b) => {
                                dl_bytes = Some(b.to_vec());
                                break;
                            }
                            Err(e) => {
                                tracing::error!("Slack: failed to read file bytes: {e}");
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Slack: failed to download file {fname} from {url}: {e}");
                        continue;
                    }
                }
            }

            let dl_bytes = match dl_bytes {
                Some(b) => {
                    tracing::info!(
                        "Slack: downloaded file {fname} ({} bytes, mime={mime})",
                        b.len()
                    );
                    b
                }
                None => {
                    tracing::warn!("Slack: all download URLs failed for {fname}");
                    continue;
                }
            };

            // Durable copy in the channel-attachment store, as Telegram and WhatsApp keep (#1729).
            if let Some(path) =
                crate::channels::persist_channel_attachment("slack", fname, &dl_bytes)
            {
                let line = format!("[file: {}]", path.display());
                if content.is_empty() {
                    content = line;
                } else {
                    content.push_str(&format!("\n\n{line}"));
                }
            }

            // Audio → STT
            if mime.starts_with("audio/") {
                if voice_config.stt_enabled {
                    match crate::channels::voice::transcribe(dl_bytes, &voice_config).await {
                        Ok(transcript) => {
                            tracing::info!(
                                "Slack: transcribed audio: {}",
                                truncate_str(&transcript, 80)
                            );
                            if content.is_empty() {
                                content = transcript;
                            } else {
                                content.push_str(&format!("\n\n[Transcription]: {transcript}"));
                            }
                            is_voice = true;
                        }
                        Err(e) => tracing::error!("Slack: STT error: {e}"),
                    }
                }
                continue;
            }

            let fc = process_file_with_vision(&dl_bytes, mime, fname, &cfg);
            let (injected, needs_vision) = inject_file_content(&fc);
            if !injected.is_empty() {
                tracing::info!(
                    "Slack: injected file {fname} (needs_vision={needs_vision}, len={})",
                    injected.len()
                );
                if content.is_empty() {
                    content = injected;
                } else {
                    content.push_str(&format!("\n\n{injected}"));
                }
            } else {
                tracing::warn!("Slack: file {fname} produced empty injection");
            }
        }
    }

    if content.is_empty() {
        tracing::debug!("Slack: no processable content after file handling, ignoring");
        return;
    }

    // Restore session's own provider (each session keeps its provider independently)
    let session_meta = state
        .session_svc
        .get_session(session_id)
        .await
        .ok()
        .flatten();
    crate::channels::commands::sync_provider_for_session(
        &state.agent,
        session_id,
        session_meta
            .as_ref()
            .and_then(|s| s.provider_name.as_deref()),
        session_meta.as_ref().and_then(|s| s.model.as_deref()),
    )
    .await;

    // `/respond_to` writes Telegram's section when it reaches the shared parser
    // with no chat id, so Slack answers it from its own channel setting (#2013).
    if let Some(reply) = crate::channels::respond_to_scope::respond_to_outside_telegram(
        &content,
        is_owner,
        "Slack",
        Some(&sl_cfg.respond_to),
    ) {
        let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
        let session = client.open_session(&token);
        let request = SlackApiChatPostMessageRequest::new(
            SlackChannelId::new(channel_id),
            SlackMessageContent::new().with_text(reply),
        );
        if let Err(e) = session.post(&request).await {
            tracing::warn!(error = %e, "failed to post Slack message");
        }
        return;
    }

    // ── Channel commands (/help, /usage, /models) ──────────────────────────
    {
        use crate::channels::commands::{self, ChannelCommand};
        let cmd = commands::handle_command(
            &content,
            session_id,
            &state.agent,
            &state.session_svc,
            is_owner,
            None,
        )
        .await;

        // Handle simple text-response commands (Help, Usage, Evolve, Doctor, etc.)
        if let Some(reply) = commands::try_execute_text_command(&cmd).await {
            let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
            let session = client.open_session(&token);
            let request = SlackApiChatPostMessageRequest::new(
                SlackChannelId::new(channel_id),
                SlackMessageContent::new().with_text(reply),
            );
            if let Err(e) = session.post(&request).await {
                tracing::warn!(error = %e, "failed to post Slack message");
            }
            return;
        }

        match cmd {
            ChannelCommand::Models(resp) => {
                let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                let session = client.open_session(&token);
                let header = SlackBlock::Section(SlackSectionBlock::new().with_text(
                    SlackBlockText::MarkDown(SlackBlockMarkDownText::new(resp.text.clone())),
                ));
                let buttons: Vec<SlackActionBlockElement> = resp
                    .providers
                    .iter()
                    .take(25)
                    .map(|(name, label, configured)| {
                        let marker = crate::channels::commands::provider_marker(
                            name,
                            &resp.current_provider,
                            *configured,
                        );
                        let display = match marker {
                            "🔒" => format!("🔒 {} (setup)", label),
                            "✓" => format!("✓ {}", label),
                            _ => label.clone(),
                        };
                        let cb = if *configured {
                            format!("provider:{}", name)
                        } else {
                            format!("setup:{}", name)
                        };
                        SlackActionBlockElement::Button(SlackBlockButtonElement::new(
                            SlackActionId::new(cb),
                            SlackBlockPlainTextOnly::from(SlackBlockPlainText::new(display)),
                        ))
                    })
                    .collect();
                let mut blocks = vec![header];
                for chunk in buttons.chunks(5) {
                    blocks.push(SlackBlock::Actions(SlackActionsBlock::new(chunk.to_vec())));
                }
                let request = SlackApiChatPostMessageRequest::new(
                    SlackChannelId::new(channel_id),
                    SlackMessageContent::new().with_blocks(blocks),
                );
                if let Err(e) = session.post(&request).await {
                    tracing::warn!(error = %e, "failed to post Slack message");
                }
                return;
            }
            ChannelCommand::NewSession => {
                // MUST match the per-message resolver format above —
                // DM titles include the "DM" prefix so /new and the
                // next typed message land on the same row (issue #89). Both
                // the suffix and legacy lookups run so auto-titled rows still
                // get archived (issue #121).
                use crate::channels::session_resolve;
                let (id_str, legacy_title) = if is_dm {
                    (
                        format!("slack-dm-{}", user_id),
                        format!("Slack: DM {}", user_id),
                    )
                } else {
                    (
                        format!("slack-{}", channel_id),
                        format!("Slack: #{}", channel_id),
                    )
                };
                let suffix = session_resolve::chat_id_suffix(&id_str);
                let session_title = format!("{legacy_title} {suffix}");
                // The new session inherits its working directory from the
                // session that received this /new (same chat), not the global
                // most-recent session (#263).
                let prior_session = match state
                    .session_svc
                    .find_session_by_title_suffix(&suffix)
                    .await
                {
                    Ok(Some(s)) => Some(s),
                    Ok(None) => state
                        .session_svc
                        .find_session_by_title(&legacy_title)
                        .await
                        .unwrap_or_else(|e| {
                            tracing::error!(
                                "Slack: /new legacy prior-session lookup failed: {e:#}"
                            );
                            None
                        }),
                    Err(e) => {
                        // /new means a fresh session IS the intent; the
                        // failure is logged, never silent (#442).
                        tracing::error!("Slack: /new prior-session lookup failed: {e:#}");
                        None
                    }
                };
                if !is_owner
                    && let Some(old) = prior_session.as_ref()
                    && let Err(e) = state.session_svc.archive_session(old.id).await
                {
                    tracing::error!("Slack: failed to archive old session {}: {}", old.id, e);
                }
                match crate::channels::session_init::create_channel_session(
                    &state.session_svc,
                    Some(session_title),
                    prior_session.as_ref(),
                )
                .await
                {
                    Ok(new_session) => {
                        if is_owner && is_dm {
                            *state.shared_session.lock().await = Some(new_session.id);
                        }
                        state
                            .slack_state
                            .register_session_channel(
                                new_session.id,
                                channel_id.clone(),
                                msg.origin.thread_ts.as_ref().map(|ts| ts.to_string()),
                            )
                            .await;
                        // Sync provider for the new session so baseline is accurate
                        let new_meta = state
                            .session_svc
                            .get_session(new_session.id)
                            .await
                            .ok()
                            .flatten();
                        crate::channels::commands::sync_provider_for_session(
                            &state.agent,
                            new_session.id,
                            new_meta.as_ref().and_then(|s| s.provider_name.as_deref()),
                            new_meta.as_ref().and_then(|s| s.model.as_deref()),
                        )
                        .await;
                        let baseline = state.agent.base_context_tokens();
                        let ctx_max = state.agent.context_limit_for_session(new_session.id);
                        let footer = crate::utils::format_ctx_footer(baseline, ctx_max, None);
                        let msg_text = format!("✅ New session started.\n\n{footer}");
                        let token =
                            SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                        let session = client.open_session(&token);
                        let request = SlackApiChatPostMessageRequest::new(
                            SlackChannelId::new(channel_id),
                            SlackMessageContent::new().with_text(msg_text),
                        );
                        if let Err(e) = session.post(&request).await {
                            tracing::warn!(error = %e, "failed to post Slack message");
                        }
                        tracing::info!(
                            "Slack /new: sent ctx footer='{}' (baseline={}, ctx_max={})",
                            footer,
                            baseline,
                            ctx_max,
                        );
                    }
                    Err(e) => {
                        tracing::error!("Slack: failed to create session: {}", e);
                        let token =
                            SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                        let session = client.open_session(&token);
                        let request = SlackApiChatPostMessageRequest::new(
                            SlackChannelId::new(channel_id),
                            SlackMessageContent::new()
                                .with_text("Failed to create session.".to_string()),
                        );
                        if let Err(e) = session.post(&request).await {
                            tracing::warn!(error = %e, "failed to post Slack message");
                        }
                    }
                }
                return;
            }
            ChannelCommand::Sessions(resp) => {
                let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                let session = client.open_session(&token);
                let header = SlackBlock::Section(SlackSectionBlock::new().with_text(
                    SlackBlockText::MarkDown(SlackBlockMarkDownText::new(resp.text.clone())),
                ));
                let buttons: Vec<SlackActionBlockElement> = resp
                    .sessions
                    .iter()
                    .take(25)
                    .map(|(id, label)| {
                        let display = if *id == resp.current_session_id {
                            format!("▸ {} ← current", label)
                        } else {
                            label.clone()
                        };
                        SlackActionBlockElement::Button(SlackBlockButtonElement::new(
                            SlackActionId::new(format!("session:{}", id)),
                            SlackBlockPlainTextOnly::from(SlackBlockPlainText::new(display)),
                        ))
                    })
                    .collect();
                let mut blocks = vec![header];
                for chunk in buttons.chunks(5) {
                    blocks.push(SlackBlock::Actions(SlackActionsBlock::new(chunk.to_vec())));
                }
                let request = SlackApiChatPostMessageRequest::new(
                    SlackChannelId::new(channel_id),
                    SlackMessageContent::new().with_blocks(blocks),
                );
                if let Err(e) = session.post(&request).await {
                    tracing::warn!(error = %e, "failed to post Slack message");
                }
                return;
            }
            ChannelCommand::Stop => {
                let cancelled = state.slack_state.cancel_session(session_id).await;
                let reply = if cancelled {
                    "Operation cancelled."
                } else {
                    "No operation in progress."
                };
                let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                let session = client.open_session(&token);
                let request = SlackApiChatPostMessageRequest::new(
                    SlackChannelId::new(channel_id),
                    SlackMessageContent::new().with_text(reply.to_string()),
                );
                if let Err(e) = session.post(&request).await {
                    tracing::warn!(error = %e, "failed to post Slack message");
                }
                return;
            }
            ChannelCommand::StopTarget(_) => {
                // #2108: targeting another session is Telegram-only for now.
                // Answer explicitly instead of falling through to the agent,
                // which would look like the command ran. Reuses the gated
                // final-text helper, so this arm adds no new write site of
                // its own and the governor's coverage count stays honest.
                let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                let session = client.open_session(&token);
                post_final_text(
                    &session,
                    &channel_id,
                    msg.origin.thread_ts.as_ref(),
                    "Targeted stop is Telegram-only. Use /stop here.",
                )
                .await;
                return;
            }
            ChannelCommand::Compact => {
                let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                let session = client.open_session(&token);
                let request = SlackApiChatPostMessageRequest::new(
                    SlackChannelId::new(channel_id.clone()),
                    SlackMessageContent::new().with_text("⏳ Compacting context...".to_string()),
                );
                if let Err(e) = session.post(&request).await {
                    tracing::warn!(error = %e, "failed to post Slack message");
                }
                content =
                    "[SYSTEM: Compact context now. Summarize this conversation for continuity.]"
                        .to_string();
            }
            ChannelCommand::ClearContext => {
                // No agent turn: the marker row is the whole operation (#1585).
                let reply = match state.agent.clear_context(session_id).await {
                    Ok(receipt) => receipt.user_line(),
                    Err(e) => {
                        tracing::error!("/clear failed: {e}");
                        format!("/clear did nothing, the context is unchanged: {e}")
                    }
                };
                let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                let session = client.open_session(&token);
                let request = SlackApiChatPostMessageRequest::new(
                    SlackChannelId::new(channel_id.clone()),
                    SlackMessageContent::new().with_text(reply),
                );
                if let Err(e) = session.post(&request).await {
                    tracing::warn!(error = %e, "failed to post Slack clear receipt");
                }
                return;
            }
            ChannelCommand::UserPrompt(prompt) => {
                content = prompt;
                // fall through to agent with the prompt as the message
            }
            ChannelCommand::NotACommand => {}
            // Help, Usage, Evolve, Doctor, UserSystem handled by try_execute_text_command above
            ChannelCommand::Profiles(resp) => {
                let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                let session = client.open_session(&token);
                let request = SlackApiChatPostMessageRequest::new(
                    SlackChannelId::new(channel_id),
                    SlackMessageContent::new().with_text(resp.text.clone()),
                );
                if let Err(e) = session.post(&request).await {
                    tracing::warn!(error = %e, "failed to post Slack message");
                }
                return;
            }
            _ => {}
        }
    }

    // Detect thread replies so the agent knows the message is in a thread context.
    // Also store the thread_ts so we reply in the same thread.
    let thread_ts = msg.origin.thread_ts.clone();
    let reply_context = thread_ts
        .as_ref()
        .map(|ts| format!("[Replying in thread (thread_ts: {ts})]"));

    // Tell the LLM its text response is automatically delivered to the chat,
    // so it should NOT use slack_send for simple text replies.
    // If images are attached, instruct the agent to analyze ALL of them.
    // Check before `content` is moved into agent_input below.
    let has_images = content.contains("<<IMG:");
    let image_hint = if has_images {
        " IMPORTANT: Multiple images may be attached. Call analyze_image for EACH <<IMG:path>> marker separately. Do not skip any image."
    } else {
        ""
    };

    // Build the human-readable display text (used for DB persistence + TUI).
    // Owner DMs show bare text; multi-user/group conversations get a
    // `Sender: text` prefix so OpenCrabs sessions stay readable.
    let display_text = if is_owner && is_dm {
        content.clone()
    } else {
        format!("{user_name}: {content}")
    };

    // Fast-cancel: any recognised stop intent, in any supported language (#965).
    // MUST run before content is moved into agent_input below.
    if crate::utils::stop_intent::is_stop_command_or_intent(&content) {
        state.slack_state.cancel_session(session_id).await;
        let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
        let session = client.open_session(&token);
        let request = SlackApiChatPostMessageRequest::new(
            SlackChannelId::new(channel_id),
            SlackMessageContent::new().with_text("Operation cancelled.".to_string()),
        );
        if let Err(e) = session.post(&request).await {
            tracing::warn!(error = %e, "failed to post Slack message");
        }
        return;
    }

    // Name the current sender. In a channel this always runs — even for the
    // owner — because the history block below carries other members' names, and
    // without the label the model addresses the sender by one of those (#682).
    // DMs keep the old shape: nobody else's name is in play there.
    let agent_input = if !is_dm {
        let role = if is_owner { "owner" } else { "user" };
        format!(
            "{}\n{content}",
            group_history::current_sender_label(
                "Slack channel",
                &channel_name,
                &user_name,
                &format!(" ({user_id})"),
                role,
            )
        )
    } else if !is_owner {
        format!("[Slack DM from {user_name} ({user_id})]\n{content}")
    } else {
        content
    };

    // Prepend reply/thread context if the message is in a thread.
    let agent_input = if let Some(ref ctx) = reply_context {
        format!("{ctx}\n{agent_input}")
    } else {
        agent_input
    };

    // Inject recent channel history so the agent has full conversation context.
    // Deduped against the live session window: after a compaction the model
    // still holds those turns, so re-sending all 30 every turn was pure waste
    // (#1620, the Slack half of #133).
    //
    // Scoped to THIS thread, mirroring Telegram's forum topics (#226): a
    // channel-wide fetch pulled every parallel thread's messages into context,
    // so each thread saw all the others. A top-level message carries no
    // thread_ts and keeps the channel-wide view.
    let agent_input = if !is_dm {
        let thread_id_str = thread_ts.as_ref().map(|ts| ts.to_string());
        let fetched = state
            .channel_msg_repo
            .recent(
                Some("slack"),
                &channel_id,
                30,
                thread_id_str.as_deref(),
                None,
            )
            .await
            .unwrap_or_default();
        match group_history::build_preamble(
            state.session_svc.pool(),
            session_id,
            fetched,
            "channel",
            "Slack",
        )
        .await
        {
            Some(preamble) => format!("{preamble}\n{agent_input}"),
            None => agent_input,
        }
    } else {
        agent_input
    };

    // Tell the LLM its text response is automatically delivered to the chat,
    // so it should NOT use slack_send for simple text replies. Surface the
    // channel id so the agent can target THIS channel for cron reports /
    // cross-surface sends without guessing (#533, mirror of upstream #510).
    let agent_input = format!(
        "{}{image_hint}{agent_input}",
        super::formatting_prompt::slack_preamble(&channel_id)
    );

    // Register channel (and originating thread, #1988) for approval
    // routing and resume replies, then send with approval callback
    state
        .slack_state
        .register_session_channel(
            session_id,
            channel_id.clone(),
            thread_ts.as_ref().map(|ts| ts.to_string()),
        )
        .await;

    // Claim this session's background-task completions for Slack: a completion
    // must be delivered by the surface that OWNS the session, not by whichever
    // service happened to run the command (#940).
    crate::brain::agent::service::session_routes::claim_for_channel(
        session_id,
        state.agent.message_enqueue_callback(),
    );
    let approval_cb = make_approval_callback(state.slack_state.clone());

    // Follow-up interrupt: cancel any running agent for this session before starting new work
    state.slack_state.cancel_session(session_id).await;

    let cancel_token = tokio_util::sync::CancellationToken::new();
    state
        .slack_state
        .store_cancel_token(session_id, cancel_token.clone())
        .await;

    let turn_group_ts: Arc<Mutex<Option<SlackTs>>> = Arc::new(Mutex::new(None));
    let turn_group_ts_final = turn_group_ts.clone();
    // Open the flow group at turn start (#1808): the counter and the 🕒
    // clock must cover the thinking latency too, not start at the first
    // tool call. The old static placeholder is gone, the
    // live group with its rolling clock IS the processing feedback.
    {
        let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
        let session = client.open_session(&token);
        sync_step_group(
            &session,
            &state.slack_state,
            SlackChannelId::new(channel_id.clone()),
            thread_ts.clone(),
            &turn_group_ts,
            Vec::new(),
        )
        .await;
    }
    // Roll the flow clock between tool events (#1807): one ticker per turn,
    // self-stopping on settle so it never outlives the group.
    spawn_flow_ticker(
        client.clone(),
        state.slack_state.clone(),
        turn_group_ts.clone(),
    );

    // Track every IntermediateText `tokio::spawn` handle so the
    // final-response branch can await ALL of them before settling, so the
    // step group is fully updated before the answer lands. Narration folds
    // into the group (#943), nothing posts standalone. Sync
    // `std::sync::Mutex` because the progress callback closure
    // is synchronous and we only ever drain (no contention across
    // .await).
    let intermediate_handles: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let intermediate_handles_cb = intermediate_handles.clone();
    let intermediate_handles_final = intermediate_handles.clone();

    // The turn's folded steps. Declared out here, not inside the callback, so
    // the delivery path can reach the narration when the final response comes
    // back empty — otherwise the answer is sealed inside a collapsed group and
    // nothing is posted at all (#951).
    let steps: Arc<Mutex<Vec<super::tool_group::GroupEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let steps_final = steps.clone();
    // Answers posted before the buttons when a turn halts on suggest_options
    // (#2029). The final body strips them so a repeated sign-off stays single.
    let posted_answers: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let posted_final = posted_answers.clone();

    // ts of the turn's step-group message, shared by the progress callbacks
    // and the delivery tail: the settle pass (#1797) stamps the group only
    // after the answer is out, and it reaches the same ts the callbacks
    // posted.

    // Build progress callback — sends tool call status as Slack messages
    #[allow(clippy::type_complexity)]
    let progress_cb: crate::brain::agent::ProgressCallback = {
        use crate::brain::agent::ProgressEvent;

        use super::tool_group::{GroupEntry, GroupState};

        let tools = steps;
        // ts of the single grouped tool message for this turn, once posted.
        let tool_group_ts = turn_group_ts.clone();
        let bot_token_cb = state.current_bot_token();
        let channel_cb = SlackChannelId::new(channel_id.clone());
        let client_cb = client.clone();
        let thread_ts_cb = thread_ts.clone();
        let tool_group_ts_outer = tool_group_ts.clone();
        let posted_cb = posted_answers.clone();

        let slack_state_outer = state.slack_state.clone();

        Arc::new(move |session_id, event| {
            let tools = tools.clone();
            let tool_group_ts_cb = tool_group_ts_outer.clone();
            let slack_state_grp = slack_state_outer.clone();
            let token = SlackApiToken::new(SlackApiTokenValue::from(bot_token_cb.clone()));
            let channel = channel_cb.clone();
            let client = client_cb.clone();
            let thread_ts_inner = thread_ts_cb.clone();

            match event {
                ProgressEvent::ToolStarted {
                    tool_name,
                    tool_input,
                    ..
                } => {
                    let group_ts = tool_group_ts_cb.clone();
                    let ctx = crate::utils::tool_context_hint(&tool_name, &tool_input);
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        // Append to the turn's grouped tool message (#371),
                        // collapsed by default with an Expand toggle (#373).
                        let entries = {
                            let mut t = tools.lock().await;
                            t.push(GroupEntry::Tool {
                                name: tool_name,
                                context: ctx,
                                status: None,
                                task_id: None,
                            });
                            t.clone()
                        };
                        sync_step_group(
                            &session,
                            &slack_state_grp,
                            channel,
                            thread_ts_inner,
                            &group_ts,
                            entries,
                        )
                        .await;
                    });
                }
                ProgressEvent::ToolCompleted {
                    tool_name, success, ..
                } => {
                    let group_ts = tool_group_ts_cb.clone();
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        let entries = {
                            let mut t = tools.lock().await;
                            if let Some(GroupEntry::Tool { status, .. }) =
                                t.iter_mut().rev().find(|e| {
                                    matches!(
                                        e,
                                        GroupEntry::Tool { name, status: None, .. }
                                            if *name == tool_name
                                    )
                                })
                            {
                                *status = Some(success);
                            }
                            t.clone()
                        };
                        if let Some(ts) = group_ts.lock().await.as_ref() {
                            let group = slack_state_grp
                                .upsert_tool_group(
                                    ts.to_string(),
                                    GroupState::new(channel.clone(), entries),
                                )
                                .await;
                            let content = super::tool_group::render(&group, ts);
                            let upd = SlackApiChatUpdateRequest::new(channel, content, ts.clone());
                            if let Some(Err(e)) = session.update_chrome(&upd).await {
                                tracing::warn!(
                                    "Slack: chat_update failed (tool group status, ts={}): {}",
                                    ts,
                                    e
                                );
                            }
                        }
                    });
                }
                ProgressEvent::SelfHealingAlert { message } => {
                    let thread_ts_heal = thread_ts_inner.clone();
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        let text =
                            format!("🔧 {}", crate::utils::sanitize::normalize_dashes(&message));
                        let mut req = SlackApiChatPostMessageRequest::new(
                            channel,
                            SlackMessageContent::new().with_text(text),
                        );
                        if let Some(ref ts) = thread_ts_heal {
                            req = req.with_thread_ts(ts.clone());
                        }
                        if let Err(e) = session.post(&req).await {
                            tracing::warn!(error = %e, "failed to post Slack message");
                        }
                    });
                }
                ProgressEvent::IntermediateText { text, .. } => {
                    let thread_ts_resp = thread_ts_inner.clone();
                    // Strip LLM-hallucinated artifacts (<!-- tools-v2: ... -->,
                    // <tool_call> XML blocks, etc.) BEFORE posting. The
                    // final-response handler does this on text_only; this is
                    // the intermediate-path mirror so the same content
                    // doesn't leak the raw `<!-- tools-v2: [...] -->`
                    // comment into the channel as visible text. Observed
                    // in a turn with multiple bash steps where one tool's
                    // tools-v2 wrapper rendered as raw HTML in Slack
                    // because the streaming chunk never hit the final
                    // response cleanup path.
                    let text = crate::utils::sanitize::sanitize_outbound(&text);
                    // Strip <<IMG:path>> markers — the final-response handler
                    // extracts these and uploads via files_upload, but if the
                    // LLM emits the marker mid-stream the intermediate path
                    // would post the raw token verbatim AND the prefixed body
                    // would no longer hash-match a prior clean intermediate,
                    // breaking dedup against the final post (same root cause
                    // as the Telegram fix at 37d9f69a).
                    let (text_clean, _img_paths) = crate::utils::extract_img_markers(&text);
                    // Same reasoning for <<VID:>> markers — strip so a
                    // mid-stream emit doesn't leak the raw token AND
                    // doesn't break hash-match against the final.
                    let (text_clean, _vid_paths) = crate::utils::extract_vid_markers(&text_clean);
                    let text_clone = text_clean;
                    let group_ts = tool_group_ts_cb.clone();
                    let handle = tokio::spawn(async move {
                        if text_clone.trim().is_empty() {
                            return;
                        }
                        let session = client.open_session(&token);
                        // Fold the narration into the turn's step group instead
                        // of posting it as its own message (#943). Standalone it
                        // read as an answer, and on a turn that ended with an
                        // empty final the empty-final guard promoted it to one.
                        let text_fmt = crate::utils::slack_fmt::markdown_to_mrkdwn(&text_clone);
                        let entries = {
                            let mut t = tools.lock().await;
                            t.push(GroupEntry::Note(text_fmt));
                            t.clone()
                        };
                        sync_step_group(
                            &session,
                            &slack_state_grp,
                            channel,
                            thread_ts_resp,
                            &group_ts,
                            entries,
                        )
                        .await;
                    });
                    if let Ok(mut g) = intermediate_handles_cb.lock() {
                        g.push(handle);
                    }
                }
                ProgressEvent::RetryAttempt {
                    attempt,
                    max,
                    reason,
                } => {
                    let thread_ts_retry = thread_ts_inner.clone();
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        let text = format!("⏳ Retry {}/{} — {}", attempt, max, reason);
                        let mut req = SlackApiChatPostMessageRequest::new(
                            channel,
                            SlackMessageContent::new().with_text(text),
                        );
                        if let Some(ref ts) = thread_ts_retry {
                            req = req.with_thread_ts(ts.clone());
                        }
                        if let Err(e) = session.post(&req).await {
                            tracing::warn!(error = %e, "failed to post Slack message");
                        }
                    });
                }
                ProgressEvent::ProviderSwitched {
                    to_name, to_model, ..
                } => {
                    let thread_ts_switch = thread_ts_inner.clone();
                    tokio::spawn(async move {
                        let session = client.open_session(&token);
                        let text = format!("🔄 Now using {}/{}", to_name, to_model);
                        let mut req = SlackApiChatPostMessageRequest::new(
                            channel,
                            SlackMessageContent::new().with_text(text),
                        );
                        if let Some(ref ts) = thread_ts_switch {
                            req = req.with_thread_ts(ts.clone());
                        }
                        if let Err(e) = session.post(&req).await {
                            tracing::warn!(error = %e, "failed to post Slack message");
                        }
                    });
                }
                // Optional follow-up suggestions (#599): post tap-to-send
                // buttons under the response; a tap injects a new turn.
                ProgressEvent::SuggestedOptions(options) => {
                    let raw_options: Vec<String> =
                        options.into_iter().map(|item| item.label).collect();
                    let handles = intermediate_handles_cb.clone();
                    let posted = posted_cb.clone();
                    let group_ts = tool_group_ts_cb.clone();
                    tokio::spawn(async move {
                        // The answer the surface halted on posts before the
                        // buttons, out of the group (#2029).
                        super::options_host::drain_note_tasks(&handles).await;
                        let session = client.open_session(&token);
                        super::options_host::post_halted_answer(
                            &session,
                            &slack_state_grp,
                            channel,
                            thread_ts_inner,
                            &group_ts,
                            &tools,
                            &posted,
                        )
                        .await;
                        super::suggest_options::render_suggestions(
                            &slack_state_grp,
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

    let result = state
        .agent
        .send_message_with_tools_and_display(
            session_id,
            agent_input,
            Some(display_text),
            None,
            Some(cancel_token),
            Some(approval_cb),
            Some(progress_cb),
            "slack",
            Some(&channel_id),
            None,
        )
        .await;

    state.slack_state.remove_cancel_token(session_id).await;

    match result {
        Ok(response) => {
            // Extract <<IMG:path>> and <<VID:path>> markers. IMG paths are
            // uploaded via files_upload below; VID paths are only stripped
            // (the agent already analyzed them via analyze_video — we don't
            // re-attach the source video to Slack). Stripping VID here so
            // the final hash matches the intermediate hash (which also
            // strips VID), preserving dedup.
            let (text_only, img_paths) = crate::utils::extract_img_markers(&response.content);
            let (text_only, _vid_paths) = crate::utils::extract_vid_markers(&text_only);
            let text_only = crate::utils::sanitize::strip_llm_artifacts(&text_only);
            // React-back (#372): the marker used to leak into Slack text.
            let (text_only, react_emoji) = crate::utils::extract_react_marker(&text_only);
            if let Some(ref em) = react_emoji {
                let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
                super::reactions::add_reaction(
                    &client,
                    &token,
                    SlackChannelId::new(channel_id.clone()),
                    msg.origin.ts.clone(),
                    super::reactions::slack_name_for_glyph(em),
                )
                .await;
            }
            let text_only = redact_secrets(&text_only);

            // Drop narration the step group already shows (#1010). The final
            // body carries every folded note as well, and the completion path
            // below only reconciles standalone intermediate POSTS, so without
            // this the whole turn's commentary ships ahead of the answer.
            // Structural: only strings the group actually folded are removed.
            let text_only = {
                let mut folded = super::final_body::folded_paragraphs(
                    super::tool_group::notes_text(&steps_final.lock().await),
                );
                folded.extend(super::options_host::held_paragraphs(
                    &posted_final.lock().await,
                ));
                super::final_body::strip_folded_notes(&text_only, &folded)
            };

            // Slack renders neither tables nor headings, so rewrite them
            // into its own shape before mrkdwn conversion (#1016). Raw pipes
            // in a proportional font align with nothing, and a bare `##`
            // renders as two literal hashes.
            let text_only = super::table_convert::structure_to_slack(&text_only);

            let text_only = crate::utils::slack_fmt::markdown_to_mrkdwn(&text_only);

            let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
            let session = client.open_session(&token);

            // Await every IntermediateText spawn before touching the answer
            // path, so the step group carries every folded note before the
            // final response branch reads it (spawn-then-read race).
            let pending = {
                let mut g = intermediate_handles_final.lock().expect("poisoned");
                std::mem::take(&mut *g)
            };
            if !pending.is_empty() {
                tracing::debug!(
                    "Slack: awaiting {} in-flight intermediate post(s) before dedup",
                    pending.len()
                );
                for h in pending {
                    if let Err(e) = h.await {
                        tracing::warn!(error = %e, "Slack intermediate post task panicked");
                    }
                }
            }

            // Context budget footer (display-only: never stored in the DB,
            // never fed to TTS). Sole carrier since #1806: the settled
            // step-group line. Answer messages are clean prose; intermediates
            // never post standalone since #943, so there is nothing to
            // dedup the final against.
            let ctx_max = state.agent.context_limit_for_session(session_id);
            let footer = crate::utils::format_ctx_footer(
                response.context_tokens,
                ctx_max,
                response.tokens_per_second,
            );

            // Empty-final guard: when `text_only` is empty/whitespace (model
            // emitted its real answer mid-stream as IntermediateText and the
            // final response.content was just a wrap-up with no content), the
            // intermediates ARE the answer. Don't delete them, don't post
            // anything else — leave them as the visible reply. Without this
            // guard, hash("") never matches any real intermediate's hash, so
            // every intermediate gets classified "non-matching" and deleted,
            // and the empty post loop emits nothing → user sees zero messages.
            // Observed at 01:53 today: bot's only response was a streaming
            // intermediate, the final was empty, my dedup deleted the
            // intermediate and posted nothing.
            // Post-#943 the intermediates list below is always empty — narration
            // folds into the step group and nothing posts standalone — so the
            // empty-final answer is the salvage path: it promotes the folded
            // notes to the answer message and consumes them out of the group
            // (#1805), mirroring Telegram's take_folded_final reclaim.
            if text_only.trim().is_empty() {
                // Nothing was posted standalone, because narration now folds
                // into the step group (#943). When the final is empty that
                // folded text is the only answer there is, and leaving it
                // sealed in a collapsed group posts nothing at all (#951).
                let salvaged = super::tool_group::notes_text(&steps_final.lock().await);
                if let Some(answer) = salvaged {
                    tracing::info!(
                        "Slack: final response is empty — posting the folded narration as the answer ({} chars)",
                        answer.len()
                    );
                    post_final_text(&session, &channel_id, thread_ts.as_ref(), &answer).await;
                    // Consume the notes out of the group (#1805): the answer
                    // message now carries them, and a group still displaying
                    // the same text turns every expanded group into a visible
                    // duplicate — the exact shape reported on #1805.
                    let remaining = super::tool_group::consume_notes(&steps_final.lock().await);
                    let group_ts_snapshot = turn_group_ts_final.lock().await.clone();
                    if let Some(ts) = group_ts_snapshot {
                        if remaining.is_empty() {
                            // The group was narration only: its message is an
                            // empty shell now that the answer stands alone.
                            let del = SlackApiChatDeleteRequest::new(
                                SlackChannelId::new(channel_id.clone()),
                                ts.clone(),
                            );
                            if let Err(e) = session.chat_delete(&del).await {
                                tracing::warn!(
                                    "Slack: chat_delete failed (consumed note-only group, ts={ts}): {e}"
                                );
                            }
                            *turn_group_ts_final.lock().await = None;
                        } else {
                            sync_step_group(
                                &session,
                                &state.slack_state,
                                SlackChannelId::new(channel_id.clone()),
                                thread_ts.clone(),
                                &turn_group_ts_final,
                                remaining,
                            )
                            .await;
                        }
                    }
                    settle_step_group(
                        &session,
                        &state.slack_state,
                        &state.agent,
                        session_id,
                        SlackChannelId::new(channel_id.clone()),
                        &channel_id,
                        &turn_group_ts_final,
                        super::tool_group::TurnOutcome::Finished,
                        &footer,
                    )
                    .await;
                } else {
                    // Neither a final nor any narration. Say so: a turn that
                    // deliberately posts nothing is indistinguishable from one
                    // that lost its answer, which is how #951 went unnoticed.
                    tracing::warn!(
                        "Slack: turn produced neither a final response nor narration — nothing to post"
                    );
                    settle_step_group(
                        &session,
                        &state.slack_state,
                        &state.agent,
                        session_id,
                        SlackChannelId::new(channel_id.clone()),
                        &channel_id,
                        &turn_group_ts_final,
                        super::tool_group::TurnOutcome::Finished,
                        &footer,
                    )
                    .await;
                }
                return;
            }

            for img_path in img_paths {
                match tokio::fs::read(&img_path).await {
                    Ok(bytes) => {
                        let fname = std::path::Path::new(&img_path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("image.png")
                            .to_string();
                        let content_type =
                            crate::brain::tools::slack_send::content_type_for(&fname);
                        if let Err(e) = super::upload::upload_external(
                            &session,
                            SlackChannelId::new(channel_id.clone()),
                            bytes,
                            &fname,
                            content_type,
                            None,
                        )
                        .await
                        {
                            tracing::error!("Slack: failed to upload generated image: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Slack: failed to read image {}: {}", img_path, e);
                    }
                }
            }

            let chunks: Vec<String> = split_message(&text_only, 3000)
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            for chunk in chunks.iter() {
                if chunk.is_empty() {
                    continue;
                }
                // Rich delivery (#455): the chunk goes out as Block Kit
                // sections/dividers, with the plain text kept as the
                // notification fallback. A rejected blocks post retries
                // text-only so delivery never regresses on a Block Kit
                // error (invalid block, limit change, ...).
                let blocks = super::blocks::blocks_from_mrkdwn(chunk);
                let fallback_text = chunk.clone();
                let content = if blocks.is_empty() {
                    SlackMessageContent::new().with_text(fallback_text.clone())
                } else {
                    SlackMessageContent::new()
                        .with_text(fallback_text.clone())
                        .with_blocks(blocks)
                };
                let mut request = SlackApiChatPostMessageRequest::new(
                    SlackChannelId::new(channel_id.clone()),
                    content,
                );
                if let Some(ref ts) = thread_ts {
                    request = request.with_thread_ts(ts.clone());
                }
                if let Err(e) = session.post(&request).await {
                    tracing::warn!("Slack: blocks post failed ({e}) — retrying as plain text");
                    let mut plain = SlackApiChatPostMessageRequest::new(
                        SlackChannelId::new(channel_id.clone()),
                        SlackMessageContent::new().with_text(fallback_text.clone()),
                    );
                    if let Some(ref ts) = thread_ts {
                        plain = plain.with_thread_ts(ts.clone());
                    }
                    if let Err(e) = session.post(&plain).await {
                        tracing::error!("Slack: failed to send reply: {}", e);
                    }
                }
            }

            // Post-completion sweep: await any IntermediateText spawn that
            // fired after the pre-answer drain (e.g. a stream chunk delivered
            // post-stream-end) so the step group is fully updated before the
            // reply is recorded and settled. Narration folds into the group
            // (#943): nothing posts standalone, so there is nothing to
            // delete here.
            let late_pending = {
                let mut g = intermediate_handles_final.lock().expect("poisoned");
                std::mem::take(&mut *g)
            };
            for h in late_pending {
                if let Err(e) = h.await {
                    tracing::warn!(error = %e, "Slack late intermediate post task panicked");
                }
            }

            // Record the bot's reply in channel_messages so recent() context
            // queries on the next turn see both sides of the conversation,
            // not only user messages. Matches the Telegram/Discord/WhatsApp
            // pattern. Applies to all Slack chats (channels + DMs) since
            // store_channel_msg above also stores for both.
            if !text_only.trim().is_empty() {
                let cm = DbChannelMessage::new(
                    "slack".into(),
                    channel_id.clone(),
                    Some(channel_name.clone()),
                    "bot:opencrabs".to_string(),
                    "OpenCrabs".to_string(),
                    text_only.clone(),
                    "text".into(),
                    None,
                )
                .with_thread(thread_ts.as_ref().map(|ts| ts.to_string()), None);
                if let Err(e) = state.channel_msg_repo.insert(&cm).await {
                    tracing::warn!(
                        "Slack: failed to record bot reply in channel_messages: {}",
                        e
                    );
                }
            }

            // The answer is out: settle the step group into its terminal
            // line (#1797), before the voice-note detour so the status never
            // lags the delivery.
            settle_step_group(
                &session,
                &state.slack_state,
                &state.agent,
                session_id,
                SlackChannelId::new(channel_id.clone()),
                &channel_id,
                &turn_group_ts_final,
                super::tool_group::TurnOutcome::Finished,
                &footer,
            )
            .await;

            // If input was audio AND TTS is enabled, also upload a voice note
            // (OGG/Opus) alongside the text reply. Slack doesn't have a
            // dedicated "voice note" primitive like Telegram's send_voice —
            // audio files uploaded via files.upload play inline with a
            // waveform UI, which is the closest analogue.
            if is_voice && voice_config.tts_enabled {
                tracing::info!(
                    "Slack: TTS requested — synthesizing response text (len={})",
                    response.content.len()
                );
                match crate::channels::voice::synthesize(&response.content, &voice_config).await {
                    Ok(audio_bytes) => {
                        tracing::info!(
                            "Slack: TTS succeeded — {} bytes of audio, uploading to channel {}",
                            audio_bytes.len(),
                            channel_id
                        );
                        if let Err(e) = super::upload::upload_external(
                            &session,
                            SlackChannelId::new(channel_id.clone()),
                            audio_bytes,
                            "response.ogg",
                            "audio/ogg",
                            thread_ts.clone(),
                        )
                        .await
                        {
                            tracing::error!("Slack: failed to upload TTS voice note: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Slack: TTS synthesis failed: {:#}", e);
                    }
                }
            }

            // ctx footer already appended inline above
        }
        Err(ref e) if matches!(e, crate::brain::agent::AgentError::Cancelled) => {
            tracing::info!("Slack: agent call cancelled for session {}", session_id);
            let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
            let session = client.open_session(&token);
            settle_step_group(
                &session,
                &state.slack_state,
                &state.agent,
                session_id,
                SlackChannelId::new(channel_id.clone()),
                &channel_id,
                &turn_group_ts_final,
                super::tool_group::TurnOutcome::Cancelled,
                "",
            )
            .await;
        }
        Err(e) => {
            tracing::error!("Slack: agent error: {}", e);
            let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
            let session = client.open_session(&token);
            // Shared helper — same wording as TUI / Telegram / Discord /
            // WhatsApp so a user moving between channels sees consistent
            // failure messages.
            let error_msg = format!("❌ Error\n\n{}", crate::brain::agent::format_user_error(&e));
            let mut request = SlackApiChatPostMessageRequest::new(
                SlackChannelId::new(channel_id.clone()),
                SlackMessageContent::new().with_text(error_msg),
            );
            if let Some(ref ts) = thread_ts {
                request = request.with_thread_ts(ts.clone());
            }
            if let Err(e) = session.post(&request).await {
                tracing::warn!(error = %e, "failed to post Slack message");
            }
            settle_step_group(
                &session,
                &state.slack_state,
                &state.agent,
                session_id,
                SlackChannelId::new(channel_id.clone()),
                &channel_id,
                &turn_group_ts_final,
                {
                    // Classify like Telegram's settle: a timeout is its own
                    // outcome (#1797), everything else is a plain failure.
                    let es = e.to_string().to_lowercase();
                    if es.contains("timed out") || es.contains("timeout") || es.contains("deadline")
                    {
                        super::tool_group::TurnOutcome::TimedOut
                    } else {
                        super::tool_group::TurnOutcome::Failed
                    }
                },
                "",
            )
            .await;
        }
    }
}

/// Run the turn a follow-up suggestion tap started (#1838).
///
/// The tap used to ride the bare single-completion send: zero tools, zero
/// progress events, and a channel that went silent from the echo until the
/// final answer landed minutes later — which read as a dead button. This is
/// the `handle_message` display path reduced to what a tap needs: the flow
/// group is born before the agent starts thinking (live status from second
/// zero), tool steps and narration fold into it, chained suggestions still
/// render, and the final answer settles the group the same way.
async fn run_followup_turn(
    state: Arc<HandlerState>,
    client: Arc<SlackHyperClient>,
    session_id: Uuid,
    channel_id: String,
    text: String,
) {
    use super::tool_group::TurnOutcome;

    let channel = SlackChannelId::new(channel_id.clone());

    // Follow-up interrupt (handle_message parity): cancel any turn still
    // running for this session before starting new work.
    state.slack_state.cancel_session(session_id).await;
    let cancel_token = tokio_util::sync::CancellationToken::new();
    state
        .slack_state
        .store_cancel_token(session_id, cancel_token.clone())
        .await;

    // Open the flow group immediately (#1838): the 🕒 clock must cover the
    // thinking latency too, so the live status posts before the turn starts.
    let turn_group_ts: Arc<Mutex<Option<SlackTs>>> = Arc::new(Mutex::new(None));
    {
        let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
        let session = client.open_session(&token);
        sync_step_group(
            &session,
            &state.slack_state,
            channel.clone(),
            None,
            &turn_group_ts,
            Vec::new(),
        )
        .await;
    }
    spawn_flow_ticker(
        client.clone(),
        state.slack_state.clone(),
        turn_group_ts.clone(),
    );

    let FlowProgress {
        cb: progress_cb,
        steps,
        intermediate_handles,
        posted,
    } = build_flow_progress(
        client.clone(),
        state.slack_state.clone(),
        channel.clone(),
        None,
        turn_group_ts.clone(),
        state.current_bot_token(),
    );

    let approval_cb = make_approval_callback(state.slack_state.clone());

    let result = state
        .agent
        .send_message_with_tools_and_display(
            session_id,
            text.clone(),
            Some(text),
            None,
            Some(cancel_token),
            Some(approval_cb),
            Some(progress_cb),
            "slack",
            Some(&channel_id),
            None,
        )
        .await;

    state.slack_state.remove_cancel_token(session_id).await;

    // Drain narration-spawn tasks before the final posts so the group is
    // settled from its complete contents.
    let pending = {
        let mut g = intermediate_handles.lock().expect("poisoned");
        std::mem::take(&mut *g)
    };
    for h in pending {
        if let Err(e) = h.await {
            tracing::warn!(error = %e, "Slack tap: intermediate fold task panicked");
        }
    }

    let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
    let session = client.open_session(&token);

    match result {
        Ok(response) => {
            let (text_only, img_paths) = crate::utils::extract_img_markers(&response.content);
            let (text_only, _vid_paths) = crate::utils::extract_vid_markers(&text_only);
            let text_only = crate::utils::sanitize::strip_llm_artifacts(&text_only);
            let text_only = redact_secrets(&text_only);
            // The answer a halted turn posted before its suggestions is not
            // repeated from the sign-off (#2029).
            let text_only = super::final_body::strip_folded_notes(
                &text_only,
                &super::options_host::held_paragraphs(&posted.lock().await),
            );

            let ctx_max = state.agent.context_limit_for_session(session_id);
            let footer = crate::utils::format_ctx_footer(
                response.context_tokens,
                ctx_max,
                response.tokens_per_second,
            );

            if text_only.trim().is_empty() {
                // Empty-final salvage (#951/#1805): the folded notes are the
                // only answer there is — promote them before settling.
                if let Some(answer) = super::tool_group::notes_text(&steps.lock().await) {
                    post_final_text(&session, &channel_id, None, &answer).await;
                    let remaining = super::tool_group::consume_notes(&steps.lock().await);
                    if let Some(ts) = turn_group_ts.lock().await.clone() {
                        if remaining.is_empty() {
                            let del = SlackApiChatDeleteRequest::new(channel.clone(), ts.clone());
                            if let Err(e) = session.chat_delete(&del).await {
                                tracing::warn!(
                                    "Slack tap: chat_delete failed (consumed note-only group, ts={ts}): {e}"
                                );
                            }
                            *turn_group_ts.lock().await = None;
                        } else {
                            sync_step_group(
                                &session,
                                &state.slack_state,
                                channel.clone(),
                                None,
                                &turn_group_ts,
                                remaining,
                            )
                            .await;
                        }
                    }
                }
                settle_step_group(
                    &session,
                    &state.slack_state,
                    &state.agent,
                    session_id,
                    channel.clone(),
                    &channel_id,
                    &turn_group_ts,
                    TurnOutcome::Finished,
                    &footer,
                )
                .await;
                return;
            }

            for img_path in img_paths {
                match tokio::fs::read(&img_path).await {
                    Ok(bytes) => {
                        let fname = std::path::Path::new(&img_path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("image.png")
                            .to_string();
                        let content_type =
                            crate::brain::tools::slack_send::content_type_for(&fname);
                        if let Err(e) = super::upload::upload_external(
                            &session,
                            channel.clone(),
                            bytes,
                            &fname,
                            content_type,
                            None,
                        )
                        .await
                        {
                            tracing::error!("Slack tap: failed to upload generated image: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::error!("Slack tap: failed to read image {}: {}", img_path, e);
                    }
                }
            }

            let chunks: Vec<String> = split_message(&text_only, 3000)
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            for chunk in chunks.iter() {
                if chunk.is_empty() {
                    continue;
                }
                let blocks = super::blocks::blocks_from_mrkdwn(chunk);
                let fallback_text = chunk.clone();
                let content = if blocks.is_empty() {
                    SlackMessageContent::new().with_text(fallback_text.clone())
                } else {
                    SlackMessageContent::new()
                        .with_text(fallback_text.clone())
                        .with_blocks(blocks)
                };
                let request = SlackApiChatPostMessageRequest::new(channel.clone(), content);
                if let Err(e) = session.post(&request).await {
                    tracing::warn!("Slack tap: blocks post failed ({e}) — retrying as plain text");
                    let plain = SlackApiChatPostMessageRequest::new(
                        channel.clone(),
                        SlackMessageContent::new().with_text(fallback_text),
                    );
                    if let Err(e) = session.post(&plain).await {
                        tracing::error!("Slack tap: failed to send reply: {e}");
                    }
                }
            }

            settle_step_group(
                &session,
                &state.slack_state,
                &state.agent,
                session_id,
                channel.clone(),
                &channel_id,
                &turn_group_ts,
                TurnOutcome::Finished,
                &footer,
            )
            .await;
        }
        Err(ref e) if matches!(e, crate::brain::agent::AgentError::Cancelled) => {
            tracing::info!("Slack tap: agent call cancelled for session {session_id}");
            settle_step_group(
                &session,
                &state.slack_state,
                &state.agent,
                session_id,
                channel.clone(),
                &channel_id,
                &turn_group_ts,
                TurnOutcome::Cancelled,
                "",
            )
            .await;
        }
        Err(e) => {
            tracing::error!("Slack follow-up tap turn failed: {e}");
            let error_msg = format!("❌ Error\n\n{}", crate::brain::agent::format_user_error(&e));
            let request = SlackApiChatPostMessageRequest::new(
                channel.clone(),
                SlackMessageContent::new().with_text(error_msg),
            );
            if let Err(pe) = session.post(&request).await {
                tracing::warn!(error = %pe, "Slack tap: failed to post error");
            }
            let es = e.to_string().to_lowercase();
            let outcome =
                if es.contains("timed out") || es.contains("timeout") || es.contains("deadline") {
                    TurnOutcome::TimedOut
                } else {
                    TurnOutcome::Failed
                };
            settle_step_group(
                &session,
                &state.slack_state,
                &state.agent,
                session_id,
                channel.clone(),
                &channel_id,
                &turn_group_ts,
                outcome,
                "",
            )
            .await;
        }
    }
}

/// Build an `ApprovalCallback` that sends a Slack Block Kit message with 3 buttons
/// (Yes / Always / No) and waits up to 5 min for a click.
pub(crate) fn make_approval_callback(
    state: Arc<super::SlackState>,
) -> crate::brain::agent::ApprovalCallback {
    use crate::brain::agent::ToolApprovalInfo;
    use crate::utils::{check_approval_policy, persist_auto_session_policy};
    use tokio::sync::oneshot;

    Arc::new(move |info: ToolApprovalInfo| {
        let state = state.clone();
        Box::pin(async move {
            if let Some(result) = check_approval_policy() {
                return Ok(result);
            }

            let client = match state.client().await {
                Some(c) => c,
                None => {
                    tracing::warn!("Slack approval: bot not connected");
                    return Ok((false, false));
                }
            };

            let bot_token = match state.bot_token().await {
                Some(t) => t,
                None => {
                    tracing::warn!("Slack approval: no bot token");
                    return Ok((false, false));
                }
            };

            let channel_id = match state.session_channel(info.session_id).await {
                Some(id) => id,
                None => match state.owner_channel_id().await {
                    Some(id) => id,
                    None => {
                        tracing::warn!(
                            "Slack approval: no channel_id for session {}",
                            info.session_id
                        );
                        return Ok((false, false));
                    }
                },
            };

            let approval_id = uuid::Uuid::new_v4().to_string();
            let safe_input = crate::utils::redact_tool_input(&info.tool_input);
            let input_pretty = serde_json::to_string_pretty(&safe_input)
                .unwrap_or_else(|_| safe_input.to_string());
            let text = format!(
                "🔐 *Tool Approval Required*\n\nTool: `{}`\nInput:\n```\n{}\n```",
                info.tool_name,
                truncate_str(&input_pretty, 1800),
            );

            let section = SlackBlock::Section(SlackSectionBlock::new().with_text(
                SlackBlockText::MarkDown(SlackBlockMarkDownText::new(text.clone())),
            ));
            let approve_btn = SlackBlockButtonElement::new(
                SlackActionId::new(format!("approve:{}", approval_id)),
                SlackBlockPlainTextOnly::from(SlackBlockPlainText::new("✅ Yes".to_string())),
            )
            .with_style(SlackBlockButtonStyle::Primary);
            let always_btn = SlackBlockButtonElement::new(
                SlackActionId::new(format!("always:{}", approval_id)),
                SlackBlockPlainTextOnly::from(SlackBlockPlainText::new(
                    "🔁 Always (session)".to_string(),
                )),
            );
            let yolo_btn = SlackBlockButtonElement::new(
                SlackActionId::new(format!("yolo:{}", approval_id)),
                SlackBlockPlainTextOnly::from(SlackBlockPlainText::new("🔥 YOLO".to_string())),
            );
            let deny_btn = SlackBlockButtonElement::new(
                SlackActionId::new(format!("deny:{}", approval_id)),
                SlackBlockPlainTextOnly::from(SlackBlockPlainText::new("❌ No".to_string())),
            )
            .with_style(SlackBlockButtonStyle::Danger);
            let actions = SlackBlock::Actions(SlackActionsBlock::new(vec![
                SlackActionBlockElement::Button(approve_btn),
                SlackActionBlockElement::Button(always_btn),
                SlackActionBlockElement::Button(yolo_btn),
                SlackActionBlockElement::Button(deny_btn),
            ]));

            let content = SlackMessageContent::new()
                .with_text(text)
                .with_blocks(vec![section, actions]);
            let request = SlackApiChatPostMessageRequest::new(
                SlackChannelId::new(channel_id.clone()),
                content,
            );
            let token = SlackApiToken::new(SlackApiTokenValue::from(bot_token.clone()));
            let session = client.open_session(&token);

            // Register BEFORE sending to prevent race condition
            let (tx, rx) = oneshot::channel();
            state
                .register_pending_approval(approval_id.clone(), tx)
                .await;
            tracing::info!(
                "Slack approval: registered pending id={}, sending to channel={}",
                approval_id,
                channel_id
            );

            let sent = match session.post(&request).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Slack approval: failed to send message: {}", e);
                    return Ok((false, false));
                }
            };

            let msg_ts = sent.ts.clone();
            tracing::info!(
                "Slack approval: message sent, waiting for response (id={})",
                approval_id
            );

            match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
                Ok(Ok((approved, always))) => {
                    tracing::info!(
                        "Slack approval: user responded id={}, approved={}, always={}",
                        approval_id,
                        approved,
                        always
                    );
                    if always {
                        persist_auto_session_policy();
                    }
                    let label = if always {
                        "🔁 Always approved (session)"
                    } else if approved {
                        "✅ Approved"
                    } else {
                        "❌ Denied"
                    };
                    let update = SlackApiChatUpdateRequest::new(
                        SlackChannelId::new(channel_id),
                        SlackMessageContent::new().with_text(label.to_string()),
                        msg_ts.clone(),
                    );
                    if let Err(e) = session.update(&update).await {
                        tracing::warn!(
                            "Slack: chat_update failed (approval result, ts={}): {}",
                            msg_ts,
                            e
                        );
                    }
                    Ok((approved, always))
                }
                Ok(Err(_)) => {
                    tracing::warn!(
                        "Slack approval: oneshot channel closed (id={})",
                        approval_id
                    );
                    Ok((false, false))
                }
                Err(_) => {
                    tracing::warn!(
                        "Slack approval: 5-minute timeout — auto-denying (id={})",
                        approval_id
                    );
                    let update = SlackApiChatUpdateRequest::new(
                        SlackChannelId::new(channel_id),
                        SlackMessageContent::new()
                            .with_text("⏱️ Approval timed out — denied".to_string()),
                        msg_ts.clone(),
                    );
                    if let Err(e) = session.update(&update).await {
                        tracing::warn!(
                            "Slack: chat_update failed (approval timeout, ts={}): {}",
                            msg_ts,
                            e
                        );
                    }
                    Ok((false, false))
                }
            }
        })
    })
}
