//! Member join/left service-message handling, extracted from `handler.rs`
//! (#1086, seam 1 of the handler decomposition).
//!
//! Teloxide 0.17+ delivers service messages as regular `Message` updates, so
//! they flow through `handle_message`; this module owns everything they do.
//! The blocks are captured BEFORE the allowlist check so bot/user IDs are
//! logged and the owner is notified even when the joining user isn't
//! allowlisted yet (the "can't see bot ID" fix).

use std::sync::Arc;

use teloxide::Bot;
use teloxide::prelude::Message;
use teloxide::prelude::Requester;
use teloxide::types::User;

use crate::config::Config;

use super::state::TelegramState;

/// Handle a member join/left service message.
///
/// Returns `true` when `msg` was a service message and is fully processed
/// (service messages carry no further content), `false` to continue normal
/// message handling in the caller.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_member_event(
    bot: &Bot,
    msg: &Message,
    user: &User,
    cfg: &Config,
    telegram_state: &Arc<TelegramState>,
) -> bool {
    let user_id = user.id.0 as i64;
    // ── Service message: member join detection ──────────────────────────
    // Capture new_chat_members BEFORE the allowlist check so bot/user IDs
    // are logged and the owner is notified even when the joining user
    // isn't allowlisted yet. This is the fix for the "can't see bot ID"
    // issue — teloxide 0.17+ delivers service messages as regular Message
    // updates, so they flow through handle_message.
    if let Some(members) = msg.new_chat_members() {
        let chat_title = msg.chat.title().unwrap_or("unknown");
        let chat_id = msg.chat.id.0;
        for member in members {
            let uid = member.id.0;
            let name = member.username.as_deref().unwrap_or(&member.first_name);
            let is_bot = member.is_bot;
            // Who performed the add. Logged on every join so a membership is
            // always attributable afterwards: this was in scope and unused,
            // which is why an unauthorized add could not be traced to anyone
            // (#1042).
            let adder_name_for_guard = user.username.as_deref().unwrap_or(&user.first_name);
            tracing::info!(
                "Telegram: member joined chat \"{}\" (chat_id={}) — user_id={} username={} \
                 is_bot={} added_by={} (user_id={})",
                chat_title,
                chat_id,
                uid,
                name,
                is_bot,
                adder_name_for_guard,
                user_id,
            );

            // Notify the owner when a bot joins so they can grab the ID
            if is_bot {
                let tg_cfg = &cfg.channels.telegram;
                if let Some(owner_id) =
                    super::menu_scope::owner_user_id(&tg_cfg.allowed_users, &tg_cfg.bot_owner)
                        .map(|id| id as i64)
                {
                    // Being added somewhere and watching another bot arrive
                    // are different events needing different advice, so the
                    // notice is chosen here rather than left ambiguous (#1041).
                    let join = if telegram_state.bot_user_id().await == Some(uid as i64) {
                        BotJoin::Ourselves
                    } else {
                        BotJoin::Other {
                            username: name,
                            user_id: uid,
                        }
                    };
                    let notify = format_bot_join_notification(
                        join,
                        chat_title,
                        chat_id,
                        msg.chat.username(),
                        adder_name_for_guard,
                        user_id,
                    );
                    // Send notification to owner's DM. A failure here means
                    // the join goes unreported entirely, so it is logged
                    // rather than discarded.
                    if let Err(e) = crate::channels::telegram::send::message_in_thread(
                        bot,
                        teloxide::types::ChatId(owner_id),
                        None,
                        notify,
                    )
                    .await
                    {
                        tracing::error!(
                            "Telegram: could not tell the owner about a bot joining \"{}\" \
                             (chat_id={}), so the join is unreported: {}",
                            chat_title,
                            chat_id,
                            e
                        );
                    }
                }

                // Being added is a larger grant than any command: it exposes
                // the agent, its tools and its credentials to everyone in the
                // chat. Commands are owner-gated, so this is gated by the very
                // same predicate rather than a second notion of authority
                // (#1042). Telegram does not gate it at all — any member with
                // invite rights can add a public bot.
                if telegram_state.bot_user_id().await == Some(uid as i64)
                    && !cfg.channels.telegram.is_owner(&user_id.to_string())
                {
                    tracing::warn!(
                        "Telegram: {} (user_id={}) is not the owner and added me to \"{}\" \
                         (chat_id={}) — leaving",
                        adder_name_for_guard,
                        user_id,
                        chat_title,
                        chat_id,
                    );
                    // Leave first, notify second. The decision fails closed:
                    // an owner DM that cannot be delivered must not leave the
                    // bot sitting in a chat it was never authorised to join.
                    let left = match bot.leave_chat(msg.chat.id).await {
                        Ok(_) => true,
                        Err(e) => {
                            tracing::error!(
                                "Telegram: could not leave \"{}\" (chat_id={}) after an \
                                 unauthorized add, so I am still in it: {}",
                                chat_title,
                                chat_id,
                                e
                            );
                            false
                        }
                    };
                    let tg_cfg = &cfg.channels.telegram;
                    if let Some(owner_id) =
                        super::menu_scope::owner_user_id(&tg_cfg.allowed_users, &tg_cfg.bot_owner)
                            .map(|id| id as i64)
                    {
                        let notify = format_unauthorized_add_notification(
                            chat_title,
                            chat_id,
                            msg.chat.username(),
                            adder_name_for_guard,
                            user_id,
                            left,
                        );
                        if let Err(e) = crate::channels::telegram::send::message_in_thread(
                            bot,
                            teloxide::types::ChatId(owner_id),
                            None,
                            notify,
                        )
                        .await
                        {
                            tracing::error!(
                                "Telegram: could not warn the owner about an unauthorized add \
                                 to \"{}\" (chat_id={}): {}",
                                chat_title,
                                chat_id,
                                e
                            );
                        }
                    }
                    // Nothing below applies: we are not in this chat.
                    continue;
                }

                // When the joining bot is US, announce ourselves in the group so
                // members know we're here and how to onboard (#707).
                if telegram_state.bot_user_id().await == Some(uid as i64) {
                    // If the user who added us has a pending /cowork session, this
                    // is the owner-initiated cowork open (#718): mark the group
                    // open=true (persisted) so every member is allowed, and clear
                    // the session. `user_id` is the adder (msg.from).
                    let cowork_join = telegram_state.get_cowork_state(user_id).await.is_some();
                    if cowork_join {
                        if let Some(state) = telegram_state.get_cowork_state(user_id).await {
                            let _ = telegram_state
                                .take_cowork_by_session(&state.session_id)
                                .await;
                        }
                        match super::cowork::set_group_open(chat_id) {
                            Ok(()) => tracing::info!(
                                "[cowork] Opened group {} via /cowork (added by {})",
                                chat_id,
                                user_id
                            ),
                            Err(e) => {
                                tracing::warn!("[cowork] Failed to open group {chat_id}: {e}")
                            }
                        }
                    }
                    let opener = if cowork_join {
                        "\n\nThis is a cowork group — everyone here can @mention me and chat."
                    } else {
                        "\n\nNew fellas: smash /start and I'll get you on the crew."
                    };
                    let mut welcome = format!(
                        "🦀 BOOM. Look who just crawled in. OpenCrabs is in the building.{opener} \
                         Then just @mention me and let's cook. 🔥"
                    );
                    // The cowork deep link requests admin, so we usually land
                    // promoted (#709). Only nudge for promotion when we actually
                    // aren't admin (added manually without rights).
                    let is_admin = matches!(
                        bot.get_chat_member(teloxide::types::ChatId(chat_id), member.id)
                            .await
                            .map(|m| m.status()),
                        Ok(teloxide::types::ChatMemberStatus::Administrator)
                            | Ok(teloxide::types::ChatMemberStatus::Owner)
                    );
                    if !is_admin {
                        welcome.push_str(
                            "\n\n(Bump me to admin so I hear the whole room, not just the \
                            shout-outs.)",
                        );
                    }
                    crate::channels::telegram::send::best_effort_note(
                        bot,
                        teloxide::types::ChatId(chat_id),
                        None,
                        &welcome,
                        None,
                        "system",
                        "member_welcome",
                        "new member welcome",
                    )
                    .await;
                }
            }

            // Auto-register a joining member into the group's allowlist ONLY when
            // the owner has opened the group (open=true via /cowork or config,
            // #717). Secure by default: in a non-open group a joiner is not
            // auto-added. open=true already allows every member; the allowlist
            // entry just keeps a visible roster of who's in.
            let group_open = cfg
                .channels
                .telegram
                .groups
                .get(&chat_id.to_string())
                .map(|g| g.open)
                .unwrap_or(false);
            if !is_bot && group_open {
                match super::cowork::auto_register_to_group(uid as i64, chat_id) {
                    Ok(true) => {
                        tracing::info!(
                            "[cowork] Auto-registered user {} ({}) in group {}",
                            uid,
                            name,
                            chat_id
                        );
                        if let Some(owner_id) = super::menu_scope::owner_user_id(
                            &cfg.channels.telegram.allowed_users,
                            &cfg.channels.telegram.bot_owner,
                        )
                        .map(|id| id as i64)
                        {
                            let join_note =
                                format!("✅ New member joined workspace: {} ({})", name, uid);
                            crate::channels::telegram::send::best_effort_note(
                                bot,
                                teloxide::types::ChatId(owner_id),
                                None,
                                &join_note,
                                None,
                                "system",
                                "member_join_owner_notify",
                                "owner join notification",
                            )
                            .await;
                        }
                    }
                    Ok(false) => {
                        tracing::debug!("[cowork] User {} already registered", uid);
                    }
                    Err(e) => {
                        tracing::warn!("[cowork] Failed to auto-register user {}: {}", uid, e);
                    }
                }
            }
        }

        // #1155: any join activity (the bot itself, a human, another bot) in an
        // unconfigured group triggers solo-owner evaluation. Spawned so the
        // service-message path never blocks on API round-trips.
        {
            let bot = bot.clone();
            let cfg = cfg.clone();
            let state = telegram_state.clone();
            let chat_id = msg.chat.id.0;
            tokio::spawn(async move {
                super::menu_auto::maybe_auto_register(&bot, chat_id, &cfg, &state).await;
            });
        }

        // Service messages have no further content to process
        return true;
    }

    // ── Service message: member left ────────────────────────────────────
    if let Some(left) = msg.left_chat_member() {
        let chat_title = msg.chat.title().unwrap_or("unknown");
        let chat_id = msg.chat.id.0;
        let uid = left.id.0;
        let name = left.username.as_deref().unwrap_or(&left.first_name);
        tracing::info!(
            "Telegram: member left chat \"{}\" (chat_id={}) — user_id={} username={} is_bot={}",
            chat_title,
            chat_id,
            uid,
            name,
            left.is_bot,
        );

        // #1155: membership changed in an unconfigured group — forget the
        // cached solo-owner decision, and if the OWNER departed a
        // solo-registered group, clear their scoped menu.
        {
            let bot = bot.clone();
            let cfg = cfg.clone();
            let state = telegram_state.clone();
            tokio::spawn(async move {
                super::menu_auto::handle_membership_change(&bot, chat_id, uid as i64, &cfg, &state)
                    .await;
            });
        }

        return true;
    }
    false
}

/// Who joined, and therefore what the owner should do about it.
///
/// One notice used to serve both, which made them indistinguishable in the
/// owner's DM and gave the wrong advice for half of them: being told to add
/// OpenCrabs' own id to `allowed_users` is not an action anyone should take
/// (#1041).
pub(crate) enum BotJoin<'a> {
    /// OpenCrabs itself was added to a chat.
    Ourselves,
    /// A different bot arrived in a chat OpenCrabs is already in.
    Other { username: &'a str, user_id: u64 },
}

/// How to reach a chat, beyond its numeric id.
///
/// A numeric `chat_id` alone leaves the owner unable to find the group they
/// are being told about. A public chat has a `t.me` handle; a private one does
/// not, and saying so is the answer rather than the absence of one.
fn chat_reference(chat_title: &str, chat_id: i64, chat_username: Option<&str>) -> String {
    match chat_username {
        Some(u) if !u.is_empty() => {
            format!("\"{chat_title}\" (chat_id={chat_id}, https://t.me/{u})")
        }
        _ => format!("\"{chat_title}\" (chat_id={chat_id}, private chat with no public link)"),
    }
}

/// Format the owner's notification for an add by someone who is not the owner.
///
/// Being added to a group grants strictly more than any single command: the
/// whole agent, its tools and its credentials become reachable by everyone in
/// that chat. Commands are already owner-gated, so this is too, and the owner
/// is told who tried with the id needed to act on it (#1042).
pub(crate) fn format_unauthorized_add_notification(
    chat_title: &str,
    chat_id: i64,
    chat_username: Option<&str>,
    adder_name: &str,
    adder_id: i64,
    left: bool,
) -> String {
    let where_ = chat_reference(chat_title, chat_id, chat_username);
    let outcome = if left {
        "I left immediately."
    } else {
        "I tried to leave and could not, so remove me manually or revoke my group access in \
         BotFather."
    };
    format!(
        "🚫 {adder_name} (user_id={adder_id}) added me to {where_} and is not the bot owner. \
         {outcome}"
    )
}

/// Format the owner's notification for a bot join.
///
/// `adder` is who performed the add, taken from the service message's sender.
/// It is the one field that answers "how did this happen", and it was missing
/// entirely before.
pub(crate) fn format_bot_join_notification(
    join: BotJoin<'_>,
    chat_title: &str,
    chat_id: i64,
    chat_username: Option<&str>,
    adder_name: &str,
    adder_id: i64,
) -> String {
    let where_ = chat_reference(chat_title, chat_id, chat_username);
    match join {
        BotJoin::Ourselves => format!(
            "🦀 I was added to {where_} by {adder_name} (user_id={adder_id}). \
             Reply here or check the group's settings if this was not you."
        ),
        BotJoin::Other { username, user_id } => format!(
            "🤖 Another bot joined {where_}, a chat I am already in: @{username} \
             (user_id={user_id}), added by {adder_name} (user_id={adder_id}). \
             Add {user_id} to allowed_users if you want me to respond to it."
        ),
    }
}
