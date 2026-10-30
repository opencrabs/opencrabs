//! Inbound Discord reactions as agent turns plus react-back (#381).
//! Ports the Telegram/Slack reaction system: a reaction on a bot message
//! becomes a sentiment-framed turn (approval = keep going, stop = pause
//! and ask), react-only is the default response, and history persists a
//! compact "[System: ...]" tag instead of the prompt scaffolding.
//! Discord delivers unicode emoji directly, so no name mapping is needed;
//! custom guild emoji classify as Neutral.

use super::DiscordState;
use crate::brain::agent::AgentService;
use crate::config::Config;
use crate::services::SessionService;
use serenity::model::channel::{Reaction, ReactionType};
use serenity::prelude::Context;
use std::sync::Arc;

/// Glyph for a Discord reaction: unicode passes through; custom emoji
/// render as their `:name:` (Neutral for the classifier).
fn glyph_of(emoji: &ReactionType) -> String {
    match emoji {
        ReactionType::Unicode(u) => u.clone(),
        ReactionType::Custom { name, .. } => name
            .as_ref()
            .map(|n| format!(":{n}:"))
            .unwrap_or_else(|| ":custom:".to_string()),
        _ => ":unknown:".to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_reaction_add(
    ctx: &Context,
    reaction: &Reaction,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    discord_state: Arc<DiscordState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
) {
    let Some(bot_id) = discord_state.bot_user_id().await else {
        return; // not ready yet — cannot attribute
    };
    let Some(reactor) = reaction.user_id else {
        return;
    };
    // Ignore the bot's own react-backs.
    if reactor.get() == bot_id {
        return;
    }
    // Only reactions to the BOT's messages become turns.
    let msg = match reaction.message(&ctx.http).await {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("Discord reaction: could not fetch message: {e}");
            return;
        }
    };
    if msg.author.id.get() != bot_id {
        return;
    }
    // Same identity gate as the message path (#2195). This used to check
    // `allowed_users` alone, so a channel opened with /cowork answered typed
    // messages and silently dropped every reaction, a user admitted through
    // `allowed_roles` could talk but not react, and an owner who was not also
    // listed in `allowed_users` lost their reactions entirely.
    let cfg = config_rx.borrow().clone();
    let dc_cfg = &cfg.channels.discord;
    let is_dm = msg.guild_id.is_none();
    let channel_str = reaction.channel_id.get().to_string();
    // A thread or forum post resolves through its parent, as on the message
    // path: opening a forum opens every post in it.
    let needs_parent = !is_dm && !dc_cfg.channels.is_empty();
    let parent_id: Option<String> = if needs_parent {
        super::commands::parent_channel_id(&ctx.http, reaction.channel_id).await
    } else {
        None
    };
    let open_here = !is_dm && dc_cfg.channel_open(&channel_str, parent_id.as_deref());
    let guild_roles: Vec<u64> = reaction
        .member
        .as_ref()
        .map(|m| m.roles.iter().map(|r| r.get()).collect())
        .unwrap_or_default();
    let admitted = super::access::identity_admitted(&super::access::DiscordIdentity {
        user_id: reactor.get(),
        allowed_users: &dc_cfg.allowed_users,
        allowed_roles: &dc_cfg.allowed_roles,
        bot_owner: &dc_cfg.bot_owner,
        guild_roles: &guild_roles,
        open_here,
    });
    if !admitted {
        tracing::debug!("Discord reaction: user {reactor} not admitted here — ignoring");
        return;
    }

    let glyph = glyph_of(&reaction.emoji);
    let user_name = match reactor.to_user(&ctx.http).await {
        Ok(u) => u.global_name.unwrap_or(u.name),
        Err(_) => reactor.get().to_string(),
    };

    // Same session keying as handle_message: DMs by user, channels by id.
    let session_id = {
        use crate::channels::session_resolve;
        let (id_str, legacy_title) = if is_dm {
            (
                format!("discord-dm-{}", reactor.get()),
                format!("Discord: DM {}", reactor.get()),
            )
        } else {
            (
                format!("discord-{}", reaction.channel_id.get()),
                format!("Discord: #{}", reaction.channel_id.get()),
            )
        };
        let suffix = session_resolve::chat_id_suffix(&id_str);
        let session_title = format!("{legacy_title} {suffix}");
        match session_resolve::resolve_or_create_channel_session(
            &session_svc,
            &suffix,
            &legacy_title,
            &session_title,
            cfg.channels.discord.session_idle_hours,
            "Discord",
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("Discord reaction: failed to resolve session: {e}");
                return;
            }
        }
    };

    let preview: String = msg.content.chars().take(500).collect();
    let prompt = crate::channels::telegram::reaction_prompt::build_reaction_prompt(
        &user_name, &glyph, &preview, !is_dm,
    );
    tracing::info!(
        "Discord reaction: {user_name} reacted {glyph} on bot message {} — session {session_id}",
        msg.id
    );

    // Turn-scoped scaffolding: full prompt for THIS turn only; history gets
    // the compact system tag (same contract as Telegram/Slack).
    let display = format!("[System: {user_name} reacted with {glyph}]");

    // A reaction is the quietest way to start a turn: one click on an existing
    // bot message, no visible "I am asking something" signal. It still obeys
    // the session turn slot (#2191), so a reaction landing on a session that
    // is mid-turn queues for the live turn instead of forking a second loop
    // that reads a transcript the live one is still writing.
    let Some(_turn_guard) =
        discord_state.claim_interaction_turn(session_id, prompt.clone(), display.clone())
    else {
        return;
    };

    let response = match agent
        .send_message_with_display(session_id, prompt, Some(display), None)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Discord reaction: agent error for session {session_id}: {e}");
            return;
        }
    };

    let (text_only, _imgs) = crate::utils::extract_img_markers(&response.content);
    let text_only = crate::utils::sanitize::sanitize_outbound(&text_only);
    let (text_only, react_emoji) = crate::utils::extract_react_marker(&text_only);
    if let Some(em) = react_emoji {
        let em = em.trim().to_string();
        if let Err(e) = msg
            .react(&ctx.http, ReactionType::Unicode(em.clone()))
            .await
        {
            tracing::warn!("Discord reaction: react-back {em} failed: {e}");
        }
    }
    let trimmed = text_only.trim();
    if !trimmed.is_empty() {
        for chunk in super::handler::split_message(trimmed, 2000) {
            if let Err(e) = super::governor::say_recorded(
                &discord_state.governor,
                reaction.channel_id,
                &ctx.http,
                super::governor::Surface::Send,
                chunk,
            )
            .await
            {
                tracing::warn!("Discord reaction: failed to deliver reply: {e}");
            }
        }
    }
}
