//! Inbound Slack reactions as agent turns, plus outbound emoji mapping
//! (#372). Ports the Telegram reaction system (#302/#353): a reaction on a
//! bot message becomes a sentiment-framed turn (approval = keep going,
//! stop-sign = pause and ask), the default response is a silent react-back,
//! and history persists only a compact "[System: ...]" tag.
//!
//! Slack reactions travel by NAME ("fire"), not by glyph, so sentiment
//! classification and the react-back both go through name/glyph mappings.
//! Sentiment and prompt framing reuse the channel-agnostic helpers in
//! `crate::channels::telegram::reaction_prompt`.

use super::governor::GatedWrites;
use slack_morphism::prelude::*;
use std::sync::Arc;

/// Representative glyph for a Slack reaction name, feeding the shared
/// sentiment classifier. Unknown names classify as Neutral downstream.
pub(crate) fn glyph_for_slack_name(name: &str) -> &'static str {
    match name {
        "+1" | "thumbsup" => "👍",
        "-1" | "thumbsdown" => "👎",
        "ok_hand" => "👌",
        "muscle" => "💪",
        "saluting_face" => "🫡",
        "fire" => "🔥",
        "100" => "💯",
        "no_entry" => "⛔",
        "no_entry_sign" => "🚫",
        "octagonal_sign" => "🛑",
        "x" => "❌",
        _ => "❔",
    }
}

/// Slack reaction name for an agent-chosen emoji glyph (`<<react:EMOJI>>`).
/// Unknown glyphs fall back to thumbsup rather than failing the reaction.
pub(crate) fn slack_name_for_glyph(emoji: &str) -> &'static str {
    match emoji.trim().trim_end_matches('\u{fe0f}') {
        "👍" => "thumbsup",
        "👎" => "thumbsdown",
        "👌" => "ok_hand",
        "👀" => "eyes",
        "🔥" => "fire",
        "🎉" => "tada",
        "✅" => "white_check_mark",
        "❌" => "x",
        "❤" | "❤️" => "heart",
        "😂" | "🤣" => "joy",
        "💯" => "100",
        "🙏" => "pray",
        "👏" => "clap",
        "🚀" => "rocket",
        "🤔" => "thinking_face",
        "🏆" => "trophy",
        "⚡" => "zap",
        "🤝" => "handshake",
        "💪" => "muscle",
        _ => "thumbsup",
    }
}

/// Fire a reaction on a Slack message; failures log, never silently eaten.
pub(crate) async fn add_reaction(
    client: &Arc<SlackHyperClient>,
    token: &SlackApiToken,
    channel: SlackChannelId,
    ts: SlackTs,
    name: &str,
) {
    let session = client.open_session(token);
    let req = SlackApiReactionsAddRequest {
        channel,
        name: SlackReactionName::new(name.to_string()),
        timestamp: ts.clone(),
    };
    if let Err(e) = session.reactions_add(&req).await {
        tracing::warn!("Slack: reactions_add failed (name={name}, ts={ts}): {e}");
    }
}

/// Handle a `reaction_added` push event: a user reacting to one of the
/// bot's messages becomes an agent turn, mirroring Telegram's behavior.
pub(crate) async fn handle_reaction_added(
    ev: SlackReactionAddedEvent,
    client: Arc<SlackHyperClient>,
) {
    let state = match super::handler::handler_state() {
        Some(s) => s,
        None => {
            tracing::error!("Slack: handler state not initialized for reaction event");
            return;
        }
    };
    let bot_user = match state.bot_user_id.as_deref() {
        Some(b) => b,
        None => return, // cannot attribute; skip rather than misfire
    };
    // Ignore the bot's own react-backs, and reactions to non-bot messages.
    if ev.user.0 == bot_user {
        return;
    }
    if ev.item_user.as_ref().map(|u| u.0.as_str()) != Some(bot_user) {
        tracing::debug!("Slack reaction: not on a bot message — ignoring");
        return;
    }
    let SlackReactionsItem::Message(item) = &ev.item else {
        return;
    };
    let Some(channel) = item.origin.channel.clone() else {
        return;
    };
    let reacted_ts = item.origin.ts.clone();
    let name = ev.reaction.to_string();
    let user_id = ev.user.0.to_string();
    let is_dm = channel.0.starts_with('D');

    let token = SlackApiToken::new(SlackApiTokenValue::from(state.current_bot_token()));
    // Display name for the prompt; the id is an acceptable fallback.
    let user_name = {
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
                .and_then(|p| p.display_name.clone().or_else(|| p.real_name.clone()))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| user_id.clone()),
            Err(_) => user_id.clone(),
        }
    };

    // Same session keying as handle_message: DMs by user, channels by id.
    let session_id = {
        use crate::channels::session_resolve;
        let (id_str, legacy_title) = if is_dm {
            (
                format!("slack-dm-{user_id}"),
                format!("Slack: DM {user_id}"),
            )
        } else {
            (
                format!("slack-{}", channel.0),
                format!("Slack: #{}", channel.0),
            )
        };
        let suffix = session_resolve::chat_id_suffix(&id_str);
        let session_title = format!("{legacy_title} {suffix}");
        let idle_hours = state.config_rx.borrow().channels.slack.session_idle_hours;
        match session_resolve::resolve_or_create_channel_session(
            &state.session_svc,
            &suffix,
            &legacy_title,
            &session_title,
            idle_hours,
            "Slack",
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Slack reaction: failed to resolve session: {e}");
                return;
            }
        }
    };

    let glyph = glyph_for_slack_name(&name);
    let prompt = crate::channels::telegram::reaction_prompt::build_reaction_prompt(
        &user_name,
        glyph,
        &format!(":{name}: (Slack reaction)"),
        !is_dm,
    );
    tracing::info!(
        "Slack reaction: {user_name} reacted :{name}: on bot message {reacted_ts} — session {session_id}"
    );

    // Turn-scoped scaffolding: full prompt to the model for THIS turn only,
    // compact system tag in history (same contract as Telegram).
    let display = format!("[System: {user_name} reacted with :{name}:]");
    let response = match state
        .agent
        .send_message_with_display(session_id, prompt, Some(display), None)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Slack reaction: agent error for session {session_id}: {e}");
            return;
        }
    };

    let (text_only, _imgs) = crate::utils::extract_img_markers(&response.content);
    let text_only = crate::utils::sanitize::sanitize_outbound(&text_only);
    let (text_only, react_emoji) = crate::utils::extract_react_marker(&text_only);
    if let Some(em) = react_emoji {
        add_reaction(
            &client,
            &token,
            channel.clone(),
            reacted_ts,
            slack_name_for_glyph(&em),
        )
        .await;
    }
    let trimmed = text_only.trim();
    if !trimmed.is_empty() {
        let body = crate::utils::slack_fmt::markdown_to_mrkdwn(trimmed);
        let session = client.open_session(&token);
        let req = SlackApiChatPostMessageRequest::new(
            channel,
            SlackMessageContent::new().with_text(body),
        );
        if let Err(e) = session.post(&req).await {
            tracing::warn!("Slack reaction: failed to deliver reply: {e}");
        }
    }
}
