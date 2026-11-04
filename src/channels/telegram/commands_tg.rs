//! Channel-command ladder for the Telegram handler (#1086 seam 2).
//!
//! Extracted verbatim from `handle_message`; owns /help, /usage, /models and
//! the slash-command resolution that rewrites `text` before the agent turn.

use std::sync::Arc;

use teloxide::Bot;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, Message, ParseMode, ThreadId};
use uuid::Uuid;

use super::intermediates::{send_html_or_plain, send_retrying_rate_limit};
use super::keyboards::{build_cd_keyboard, build_profiles_keyboard};
use super::markdown::{command_md_to_html, split_message};
use super::send::message_in_thread;
use super::session_resolve;
use super::state::TelegramState;
use crate::brain::AgentService;
use crate::services::SessionService;

/// Result of the command ladder: fully handled (caller returns) or fall
/// through to the agent with the (possibly rewritten) bindings carried out.
#[derive(Debug)]
pub(crate) enum CommandOutcome {
    Handled,
    Continue {
        text: String,
        command_invocation: Option<String>,
        pre_rewrite_user_text: String,
    },
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_channel_command(
    bot: &Bot,
    msg: &Message,
    agent: &Arc<AgentService>,
    session_svc: &SessionService,
    telegram_state: &Arc<TelegramState>,
    text: String,
    thread_id: Option<ThreadId>,
    is_dm: bool,
    session_id: Uuid,
    chat_id: i64,
    topic_id: Option<i32>,
    user_id: i64,
    is_owner: bool,
    user: &teloxide::types::User,
    chat_title: &str,
    chat_id_str: &str,
    topic_name: &Option<String>,
    is_voice: bool,
    shared_session: &Arc<tokio::sync::Mutex<Option<Uuid>>>,
) -> Result<CommandOutcome, teloxide::RequestError> {
    // ── Channel commands (/help, /usage, /models) ──────────────────────────
    // Soft-nudge analyzes the user's own words, so capture them before a
    // slash command resolves to a skill/user-command body and rewrites `text`.
    let pre_rewrite_user_text = text.clone();
    let mut text = text;
    // When a slash command resolves to a prompt (a skill or user command),
    // remember the raw invocation (e.g. "/drop_release"). A slash command is a
    // deliberate NEW directive, so if it lands mid-turn it must be injected as
    // its own instruction — not with the "factor into the CURRENT task, do not
    // restart" wrapper a plain follow-up gets, which would neutralize it (#503
    // follow-up: /drop_release arrived mid-turn, got queued, but the wrapper
    // told the model to fold it into unrelated work so the release never ran).
    let mut command_invocation: Option<String> = None;
    if !is_voice {
        use crate::channels::commands::{self, ChannelCommand};
        let cmd = commands::handle_command(
            &text,
            session_id,
            agent,
            session_svc,
            is_owner,
            Some(chat_id_str),
        )
        .await;

        tracing::info!(
            "Telegram: handle_command returned {:?} (chat={}, is_dm={})",
            std::mem::discriminant(&cmd),
            msg.chat.id.0,
            is_dm
        );

        // Handle simple text-response commands (Help, Usage, MissionControl,
        // Evolve, Doctor, etc.). Prefer NATIVE rich rendering — the same
        // `sendRichMessage` path regular messages and cron reports use, which
        // turns markdown tables/headings into real Telegram tables (not `<pre>`
        // ASCII grids). Falls back to chunked HTML-or-plain when rich is
        // disabled, the reply has no rich structure, or the native send fails.
        // (The old single `.parse_mode(Html).await?` had no chunking either, so
        // the >4096-char mission-control report silently failed to send at all.)
        // Direct model switch (#467): on success, Telegram offers an inline
        // "Apply to all sessions" button (#468) unless the user already
        // scoped with the textual `all`. Payload built (and 64-byte-capped)
        // centrally by `apply_all_callback_data` (#1149); skipped entirely
        // when it would not fit.
        if let commands::ChannelCommand::ModelSwitched(reply) = &cmd {
            let mut keyboard: Option<InlineKeyboardMarkup> = None;
            if !reply.starts_with('⚠')
                && !text.trim().ends_with(" all")
                && let Some(arg) = text.trim().split_once(' ').map(|x| x.1.trim())
                && let Ok((prov, model)) = crate::utils::provider_pair::parse_pair(arg)
                && let Some(data) =
                    crate::channels::commands::apply_all_callback_data(&prov, &model)
            {
                keyboard = Some(InlineKeyboardMarkup::new(vec![vec![
                    InlineKeyboardButton::callback("Apply to all sessions", data),
                ]]));
            }
            let mut req = bot.send_message(msg.chat.id, reply.clone());
            if let Some(kb) = keyboard {
                req = req.reply_markup(kb);
            }
            if let Some(t) = thread_id {
                req = req.message_thread_id(t);
            }
            if let Err(e) = req.await {
                tracing::warn!("Telegram: model-switch reply failed: {e}");
                send_html_or_plain(bot, msg.chat.id, thread_id, reply, "turn", None).await?;
            }
            return Ok(CommandOutcome::Handled);
        }
        if let Some(reply) = commands::try_execute_text_command(&cmd).await {
            // Every slash command is owner-gated (built-ins individually,
            // skills and commands.toml entries by the catch-all gate, #975),
            // so its output is addressed to one person: in a group it goes
            // ephemeral (#756). Native rich is tried first for output that
            // benefits from it, so a scoped reply only drops to HTML once the
            // server has refused the rich variant of the parameter.
            if let Some(rx) = super::ephemeral::receiver_for(is_dm, user_id) {
                if super::rich::should_send_native_rich(&reply)
                    && super::ephemeral::try_send_rich(
                        bot.token(),
                        msg.chat.id.0,
                        thread_id,
                        rx,
                        &reply,
                    )
                    .await
                {
                    return Ok(CommandOutcome::Handled);
                }
                let html = command_md_to_html(&reply);
                let chunks = split_message(&html, 4096);
                let delivered = super::ephemeral::send_html_chunks(
                    bot.token(),
                    msg.chat.id.0,
                    thread_id,
                    rx,
                    &chunks,
                )
                .await;
                if delivered > 0 {
                    // A short delivery finishes publicly: dropping the tail
                    // would truncate the reply with nothing to show for it.
                    for chunk in chunks.iter().skip(delivered) {
                        send_html_or_plain(bot, msg.chat.id, thread_id, chunk, "turn", None)
                            .await?;
                    }
                    return Ok(CommandOutcome::Handled);
                }
                // Nothing landed: fall through to the public path unchanged.
            }
            // `.is_ok()` used to discard the error here, so a rich failure left
            // no trace at all and a fallback was indistinguishable from a clean
            // rich send (#927). Both outcomes are logged now.
            let sent_rich = super::rich::should_send_native_rich(&reply) && {
                match super::rich::send_rich_with_mermaid(
                    bot.api_url().as_str(),
                    bot.token(),
                    msg.chat.id.0,
                    thread_id,
                    &reply,
                    "turn",
                    "-",
                )
                .await
                {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!("Telegram: rich command reply failed, using HTML: {e}");
                        false
                    }
                }
            };
            if !sent_rich {
                let html = command_md_to_html(&reply);
                for chunk in split_message(&html, 4096) {
                    send_html_or_plain(bot, msg.chat.id, thread_id, chunk, "turn", None).await?;
                }
            }
            return Ok(CommandOutcome::Handled);
        }

        // Set once for the command acks below. `None` in a DM, where there is
        // nobody to hide the ack from. Commands whose reply carries an inline
        // keyboard (Models, Sessions, ChangeDir, Profiles) stay public: their
        // buttons drive callback edits, which need the ephemeral edit/delete
        // methods 10.2 added and this path does not implement.
        let ephemeral_rx = super::ephemeral::receiver_for(is_dm, user_id);
        match cmd {
            ChannelCommand::Models(resp) => {
                let rows: Vec<Vec<InlineKeyboardButton>> = resp
                    .providers
                    .iter()
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
                        // Unconfigured providers route through `setup:<name>`
                        // so the callback handler can show setup instructions
                        // instead of trying to swap to a provider with no key.
                        let cb = if *configured {
                            format!("provider:{}", name)
                        } else {
                            format!("setup:{}", name)
                        };
                        vec![InlineKeyboardButton::callback(display, cb)]
                    })
                    .collect();
                let keyboard = InlineKeyboardMarkup::new(rows);
                send_retrying_rate_limit("command reply", || {
                    message_in_thread(bot, msg.chat.id, thread_id, command_md_to_html(&resp.text))
                        .parse_mode(ParseMode::Html)
                        .reply_markup(keyboard.clone())
                })
                .await?;
                return Ok(CommandOutcome::Handled);
            }
            ChannelCommand::NewSession => {
                // MUST match the title format used by the per-message
                // session resolver above (see `session_title` at the
                // top of `handle_message`). Without the `[chat:<id>]`
                // suffix, the next typed message won't find this row
                // via `find_session_by_title_suffix` and resolution
                // reverts to the previously-bound session — i.e. /new
                // appears to do nothing (issue #89).
                let session_title = session_resolve::build_session_title(
                    is_dm,
                    &user.first_name,
                    user_id,
                    chat_title,
                    chat_id,
                    topic_id,
                    topic_name.as_deref(),
                );
                // The new session inherits its working directory from the
                // session that received this /new (same chat), not the global
                // most-recent session (#263).
                let prior_session = session_svc
                    .find_session_by_title_suffix(&session_resolve::chat_id_suffix(
                        chat_id, topic_id,
                    ))
                    .await
                    .unwrap_or_else(|e| {
                        // /new means a fresh session IS the intent — creation
                        // proceeds, but the lookup failure is never silent (#442).
                        tracing::error!(
                            "Telegram: /new prior-session lookup failed: {e:#} — \
                             proceeding without wd inheritance"
                        );
                        None
                    });
                // Archive the previous session on /new, except for the owner —
                // owner sessions stay non-archived so they remain visible in
                // /sessions for history review. Guest sessions get archived
                // so the next title lookup resolves cleanly to the new row.
                if !is_owner
                    && let Ok(Some(old)) = session_svc.find_session_by_title(&session_title).await
                    && let Err(e) = session_svc.archive_session(old.id).await
                {
                    tracing::error!("Telegram: failed to archive old session {}: {}", old.id, e);
                }
                match crate::channels::session_init::create_channel_session(
                    session_svc,
                    Some(session_title),
                    prior_session.as_ref(),
                )
                .await
                {
                    Ok(new_session) => {
                        if is_owner {
                            *shared_session.lock().await = Some(new_session.id);
                        }
                        telegram_state
                            .register_session_chat(new_session.id, msg.chat.id.0, topic_id)
                            .await;
                        // Sync provider for the new session so baseline is accurate
                        let new_meta = session_svc.get_session(new_session.id).await.ok().flatten();
                        crate::channels::commands::sync_provider_for_session(
                            agent,
                            new_session.id,
                            new_meta.as_ref().and_then(|s| s.provider_name.as_deref()),
                            new_meta.as_ref().and_then(|s| s.model.as_deref()),
                        )
                        .await;
                        let baseline = agent.base_context_tokens();
                        let ctx_max = agent.context_limit_for_session(new_session.id);
                        let footer = crate::utils::format_ctx_footer(baseline, ctx_max, None);
                        let msg_text = format!("✅ New session started.\n\n{footer}");
                        super::ephemeral::send_ack(
                            bot,
                            msg.chat.id,
                            thread_id,
                            ephemeral_rx,
                            &msg_text,
                        )
                        .await?;
                        tracing::info!(
                            "Telegram /new: sent ctx footer='{}' (baseline={}, ctx_max={})",
                            footer,
                            baseline,
                            ctx_max,
                        );
                    }
                    Err(e) => {
                        tracing::error!("Telegram: failed to create session: {}", e);
                        super::ephemeral::send_ack(
                            bot,
                            msg.chat.id,
                            thread_id,
                            ephemeral_rx,
                            "Failed to create session.",
                        )
                        .await?;
                    }
                }
                return Ok(CommandOutcome::Handled);
            }
            ChannelCommand::Sessions(resp) => {
                let rows: Vec<Vec<InlineKeyboardButton>> = resp
                    .sessions
                    .iter()
                    .map(|(id, label)| {
                        let display = if *id == resp.current_session_id {
                            format!("▸ {} ← current", label)
                        } else {
                            label.clone()
                        };
                        vec![InlineKeyboardButton::callback(
                            display,
                            format!("session:{}", id),
                        )]
                    })
                    .collect();
                let keyboard = InlineKeyboardMarkup::new(rows);
                send_retrying_rate_limit("command reply", || {
                    message_in_thread(bot, msg.chat.id, thread_id, command_md_to_html(&resp.text))
                        .parse_mode(ParseMode::Html)
                        .reply_markup(keyboard.clone())
                })
                .await?;
                return Ok(CommandOutcome::Handled);
            }
            ChannelCommand::Stop => {
                // The message's own session first, then any guest turn the
                // same caller started elsewhere (#2143). A guest session lives
                // in a chat the bot is not in, so this lookup could never find
                // it: the turn kept running with no reachable kill switch.
                let cancelled = telegram_state.cancel_session(session_id).await
                    | (telegram_state.cancel_guest_turns_for(user_id).await > 0);
                let reply = if cancelled {
                    "Operation cancelled."
                } else {
                    "No operation in progress."
                };
                super::ephemeral::send_ack(bot, msg.chat.id, thread_id, ephemeral_rx, reply)
                    .await?;
                return Ok(CommandOutcome::Handled);
            }
            ChannelCommand::StopTarget(target) => {
                // #2108: stop a turn that is running in ANOTHER session. The
                // plain `/stop` above can only reach this message's session, so
                // a wedged turn elsewhere had no kill switch from chat.
                //
                // Resolution order: live sessions first, because that is what
                // the command is for. Only when nothing live matches do we look
                // at recent sessions, so a valid-but-idle id gets an honest
                // "already idle" instead of "no such session".
                let running = telegram_state.running_session_ids();
                let resolved = match crate::channels::commands::resolve_session_prefix(
                    &target,
                    running.iter(),
                ) {
                    crate::channels::commands::TargetMatch::Unique(id) => Some(id),
                    crate::channels::commands::TargetMatch::Ambiguous(ids) => {
                        let list = ids
                            .iter()
                            .map(|id| {
                                format!(
                                    "`{}`",
                                    crate::channels::commands::shortest_unique_prefix(
                                        *id,
                                        running.iter(),
                                    )
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        super::ephemeral::send_ack(
                            bot,
                            msg.chat.id,
                            thread_id,
                            ephemeral_rx,
                            &format!(
                                "`{target}` matches {n} running sessions: {list}\n\
                                 Send a longer prefix.",
                                n = ids.len()
                            ),
                        )
                        .await?;
                        return Ok(CommandOutcome::Handled);
                    }
                    crate::channels::commands::TargetMatch::None => None,
                };
                let resolved = match resolved {
                    Some(id) => Some(id),
                    None => {
                        let recent = session_svc
                            .list_sessions(crate::db::repository::SessionListOptions {
                                include_archived: false,
                                limit: Some(50),
                                offset: 0,
                                query: None,
                                include_subagents: false,
                            })
                            .await
                            .unwrap_or_default();
                        match crate::channels::commands::resolve_session_prefix(
                            &target,
                            recent.iter().map(|s| &s.id),
                        ) {
                            crate::channels::commands::TargetMatch::Unique(id) => Some(id),
                            crate::channels::commands::TargetMatch::Ambiguous(ids) => {
                                super::ephemeral::send_ack(
                                    bot,
                                    msg.chat.id,
                                    thread_id,
                                    ephemeral_rx,
                                    &format!(
                                        "`{target}` matches {} sessions. Send a longer prefix \
                                         (`/sessions` lists them).",
                                        ids.len()
                                    ),
                                )
                                .await?;
                                return Ok(CommandOutcome::Handled);
                            }
                            crate::channels::commands::TargetMatch::None => None,
                        }
                    }
                };
                let Some(target_id) = resolved else {
                    super::ephemeral::send_ack(
                        bot,
                        msg.chat.id,
                        thread_id,
                        ephemeral_rx,
                        &format!("No session matches `{target}`. `/sessions` lists the ids."),
                    )
                    .await?;
                    return Ok(CommandOutcome::Handled);
                };
                let short = crate::channels::commands::shortest_unique_prefix(
                    target_id,
                    running.iter().chain(std::iter::once(&target_id)),
                );
                let outcome = telegram_state.stop_session_target(target_id).await;
                let reply = match outcome {
                    super::state::TargetStop::Delivered { queued_dropped: 0 } => {
                        format!("⏹️ Stopped session `{short}`.")
                    }
                    super::state::TargetStop::Delivered { queued_dropped: n } => {
                        format!("⏹️ Stopped session `{short}` and dropped {n} queued message(s).")
                    }
                    super::state::TargetStop::Queued { dropped } => format!(
                        "Session `{short}` was not running; dropped {dropped} queued message(s)."
                    ),
                    super::state::TargetStop::Idle => {
                        format!("Session `{short}` is already idle.")
                    }
                };
                super::ephemeral::send_ack(bot, msg.chat.id, thread_id, ephemeral_rx, &reply)
                    .await?;
                return Ok(CommandOutcome::Handled);
            }
            ChannelCommand::ChangeDir(resp) => {
                // Store the browsing state for this chat
                telegram_state
                    .set_dir_browser(
                        msg.chat.id.0,
                        thread_id.map(|t| t.0.0),
                        resp.current_path.clone(),
                        resp.filter.clone(),
                    )
                    .await;

                let rows = build_cd_keyboard(&resp);
                let keyboard = InlineKeyboardMarkup::new(rows);
                send_retrying_rate_limit("command reply", || {
                    message_in_thread(bot, msg.chat.id, thread_id, command_md_to_html(&resp.text))
                        .parse_mode(ParseMode::Html)
                        .reply_markup(keyboard.clone())
                })
                .await?;
                return Ok(CommandOutcome::Handled);
            }
            ChannelCommand::Profiles(resp) => {
                let rows = build_profiles_keyboard(&resp);
                let keyboard = InlineKeyboardMarkup::new(rows);
                send_retrying_rate_limit("command reply", || {
                    message_in_thread(bot, msg.chat.id, thread_id, command_md_to_html(&resp.text))
                        .parse_mode(ParseMode::Html)
                        .reply_markup(keyboard.clone())
                })
                .await?;
                return Ok(CommandOutcome::Handled);
            }
            ChannelCommand::Compact => {
                // Only the ack is scoped: the compaction turn that follows is
                // an ordinary agent turn and stays public.
                super::ephemeral::send_ack(
                    bot,
                    msg.chat.id,
                    thread_id,
                    ephemeral_rx,
                    "⏳ Compacting context...",
                )
                .await?;
                text = "[SYSTEM: Compact context now. Summarize this conversation for continuity.]"
                    .to_string();
                // fall through to agent
            }
            ChannelCommand::ClearContext => {
                // No agent turn: the marker row is the whole operation (#1585).
                let reply = match agent.clear_context(session_id).await {
                    Ok(receipt) => receipt.user_line(),
                    Err(e) => {
                        tracing::error!("/clear failed: {e}");
                        format!("/clear did nothing, the context is unchanged: {e}")
                    }
                };
                super::ephemeral::send_ack(bot, msg.chat.id, thread_id, ephemeral_rx, &reply)
                    .await?;
                return Ok(CommandOutcome::Handled);
            }
            ChannelCommand::ExecutePlan => {
                // Approve and /execute are FORBIDDEN while a turn is
                // running: refuse immediately, never queue (locked).
                if telegram_state.is_turn_active(session_id) {
                    super::ephemeral::send_ack(
                        bot,
                        msg.chat.id,
                        thread_id,
                        ephemeral_rx,
                        "⛔ A turn is running. /execute and Approve are refused while \
                         busy; try again when the turn finishes.",
                    )
                    .await?;
                    return Ok(CommandOutcome::Handled);
                }
                match crate::utils::plan_mode::try_approve(
                    session_id,
                    crate::tui::plan::ApprovalSource::User,
                )
                .await
                {
                    crate::utils::plan_mode::ApproveOutcome::Refused(reply) => {
                        super::ephemeral::send_ack(
                            bot,
                            msg.chat.id,
                            thread_id,
                            ephemeral_rx,
                            &reply,
                        )
                        .await?;
                        return Ok(CommandOutcome::Handled);
                    }
                    crate::utils::plan_mode::ApproveOutcome::SeedTurn { prompt } => {
                        // Visible seed turn: fall through to the agent with
                        // the locked implement-turn prompt as the message.
                        text = prompt;
                    }
                }
            }
            ChannelCommand::DiscardPlan => {
                // /discard cancels an in-flight turn first, then cleans up.
                let cancelled = telegram_state.cancel_session(session_id).await;
                let mut reply = crate::utils::plan_mode::discard(session_id, agent.context()).await;
                // Remove the persistent plan card — no turn runs to refresh it
                // away (#580).
                super::plan_card::remove_plan_card(bot, msg.chat.id, telegram_state, session_id)
                    .await;
                if cancelled {
                    reply = format!("⏹️ Cancelled the running turn. {reply}");
                }
                super::ephemeral::send_ack(bot, msg.chat.id, thread_id, ephemeral_rx, &reply)
                    .await?;
                return Ok(CommandOutcome::Handled);
            }
            ChannelCommand::UserPrompt(prompt) => {
                // Capture the raw invocation ("/drop_release") BEFORE `text` is
                // overwritten with the resolved skill/command body, so a
                // mid-turn injection can name the command and frame it as a
                // distinct directive rather than a follow-up to absorb.
                command_invocation = Some(text.clone());
                text = prompt;
                // fall through to agent with the prompt as the message
            }
            ChannelCommand::PlanModeWithQuery(query) => {
                // `/plan <query>`: Plan mode was already armed in handle_command;
                // run `query` as the planning turn so the agent drafts the design
                // from it in one step (#579).
                command_invocation = Some("/plan".to_string());
                text = query;
                // fall through to agent with the query as the message
            }
            ChannelCommand::NotACommand => {
                // #2108: "stop a1b2c3d4" typed as prose is a targeted stop that
                // is not a recognised stop intent, so it would otherwise be
                // dispatched to the model as ordinary input while the sender
                // believes they cancelled something. Answer with the exact
                // command instead. The detector is deliberately narrow: it only
                // fires on an owner message that OPENS with a stop word AND
                // names a session that is running right now, so "stop the
                // docker container" and "wait for the build" are untouched.
                if is_owner
                    && let Some(hint) =
                        commands::targeted_stop_hint(&text, &telegram_state.running_session_ids())
                {
                    super::ephemeral::send_ack(bot, msg.chat.id, thread_id, ephemeral_rx, &hint)
                        .await?;
                    return Ok(CommandOutcome::Handled);
                }
            }
            // Help, Usage, Evolve, Doctor, UserSystem handled by try_execute_text_command above
            _ => {}
        }
    }

    Ok(CommandOutcome::Continue {
        text,
        command_invocation,
        pre_rewrite_user_text,
    })
}
