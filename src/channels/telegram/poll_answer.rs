//! Telegram poll votes (#2288).
//!
//! `poll_answer` is a separate update type: it carries the poll id, the voter,
//! and option INDICES only. It has no chat and no message, so a vote can only
//! be routed if the bot remembered where the poll was sent. `TelegramState`
//! holds that map (`register_poll` / `lookup_poll`); this module turns a vote
//! into one synthetic user message for the session that sent the poll.
//!
//! Before this, `poll_answer` was not even in `allowed_updates`, so Telegram
//! never delivered the update: polls were one-way for the agent that made
//! them.

use std::sync::Arc;

use teloxide::prelude::*;
use teloxide::types::PollAnswer;

use crate::brain::agent::AgentService;

use super::state::TelegramState;

/// One vote, described for the session that asked the question.
///
/// Empty `option_ids` means the voter retracted, which is still information
/// the agent needs: a retracted vote must not be read as one that never came.
pub(crate) fn vote_summary(answer: &PollAnswer, options: &[String], question: &str) -> String {
    // A non-anonymous poll gives a user; an anonymous one gives the chat that
    // voted (a channel or group acting on its own behalf).
    let voter = match answer.voter.user() {
        Some(user) => user.full_name(),
        None => match answer.voter.chat() {
            Some(chat) => chat.title().unwrap_or("unknown chat").to_string(),
            None => "unknown voter".to_string(),
        },
    };
    let labels: Vec<&str> = answer
        .option_ids
        .iter()
        .filter_map(|idx| options.get(*idx as usize).map(<String>::as_str))
        .collect();
    // Empty ids means a retraction. Non-empty ids that resolve to nothing mean
    // the option list we remember is out of step with the poll, which must not
    // be reported as a retraction: the vote did happen.
    let choice = if answer.option_ids.is_empty() {
        "retracted their vote".to_string()
    } else if labels.is_empty() {
        let raw: Vec<String> = answer.option_ids.iter().map(|i| i.to_string()).collect();
        format!("voted for option(s) {} (labels unknown)", raw.join(", "))
    } else {
        format!("voted: {}", labels.join(", "))
    };
    format!(
        "[poll:{}] {voter} {choice} (poll: \"{question}\")",
        answer.poll_id
    )
}

/// Route a vote to the session that sent the poll.
pub(crate) async fn handle_poll_answer(
    bot: Bot,
    answer: PollAnswer,
    agent: Arc<AgentService>,
    state: Arc<TelegramState>,
    config_rx: tokio::sync::watch::Receiver<crate::config::Config>,
) {
    let Some(origin) = state.lookup_poll(&answer.poll_id.0) else {
        tracing::info!(
            "Telegram: poll_answer for unknown poll {} (not sent by this bot, or evicted), dropped",
            answer.poll_id
        );
        return;
    };

    // A vote arrives from whoever tapped the option, and it drives an agent
    // turn. The chat is the one the poll was sent to, so the same per-chat ACL
    // that guards messages guards a vote (#2288).
    let voter_id = answer.voter.user().map(|u| u.id.0);
    if let Some(voter_id) = voter_id {
        let chat_key = origin.chat_id.to_string();
        let is_dm = origin.chat_id > 0;
        let allowed = {
            let cfg = config_rx.borrow();
            cfg.channels
                .telegram
                .user_allowed(&voter_id.to_string(), &chat_key, is_dm)
        };
        if !allowed {
            tracing::info!(
                "Telegram: poll vote from unauthorized user {voter_id} in chat {}, dropped",
                origin.chat_id
            );
            return;
        }
    }

    let text = vote_summary(&answer, &origin.options, &origin.question);
    tracing::info!(
        "Telegram: poll vote for session {} in chat {}: {}",
        origin.session_id,
        origin.chat_id,
        text
    );

    // General's `Some(1)` is a scoping key, never an address (#1319), so the
    // shared helper decides whether a thread is really addressed.
    let thread = super::session_resolve::delivery_thread_id(origin.thread_id);

    // Same rule as a callback tap: a vote that lands mid-turn is queued, never
    // dropped and never run concurrently with the turn in flight (#2287).
    let _guard = match state.try_begin_turn(origin.session_id) {
        Some(guard) => guard,
        None => {
            let queued = super::handler::build_midturn_queued_message(None, &text, &text);
            state.enqueue_reaction(origin.session_id, queued);
            return;
        }
    };

    if let Err(e) = super::resume::resume_session_inner(
        bot,
        ChatId(origin.chat_id),
        thread,
        origin.session_id,
        text,
        agent,
        state.clone(),
        false, // user-initiated vote, not a push
    )
    .await
    {
        tracing::warn!(
            "Telegram: poll vote delivery failed for session {}: {e}",
            origin.session_id
        );
    }
}
