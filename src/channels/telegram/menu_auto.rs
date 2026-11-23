//! Solo-owner group auto-registration (#1155).
//!
//! Groups absent from `[channels.telegram.groups]` never receive a scoped
//! command menu (Gap 1), and startup-only registration goes stale on
//! membership changes (Gap 2). This module closes both for the zero-leak
//! case: a group whose only human member is the bot owner gets the full
//! catalog automatically, with no config entry required.
//!
//! Trigger rule (per #1155): *no humans other than the bot owner*. Extra
//! **bots** are ignored entirely — they neither block registration nor
//! receive menus. Zero-leak holds because the catalog is published under a
//! `ChatMember` scope keyed to the owner's `user_id`, invisible to every
//! other account, and eligibility already guarantees no other human is
//! present.
//!
//! Detection needs no subscription change: any first update from an
//! unconfigured group fires evaluation, including the `new_chat_members`
//! service message delivered when the bot itself is added (the join notice
//! in `member_events`). The Bot API has no "list chats the bot is in", so
//! results are cached in [`TelegramState`] until a membership event clears
//! them for re-evaluation.

use crate::channels::telegram::state::TelegramState;
use crate::config::Config;
use teloxide::Bot;
use teloxide::prelude::Requester;
use teloxide::types::{BotCommandScope, ChatId, Recipient, UserId};

/// Minimal membership view fed to the pure decision function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MemberView {
    pub user_id: i64,
    pub is_bot: bool,
}

/// Outcome of the solo-group eligibility decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SoloEval {
    /// The only human present is the bot owner: publish the catalog.
    Eligible,
    /// Other humans are present; their ids are listed (owner and all bots
    /// excluded). Nothing is registered — strangers keep the default floor.
    OtherHumans(Vec<i64>),
    /// The owner is not among the members at all. Treated like ineligible:
    /// registering an owner scope for an absent owner would fail anyway.
    OwnerAbsent,
}

/// Pure decision core: given every member we can observe plus the owner id,
/// decide whether this group qualifies for automatic owner-catalog
/// registration. Bots are ignored entirely (#1155 trigger rule).
pub(crate) fn evaluate_solo_group(members: &[MemberView], owner_id: i64) -> SoloEval {
    let mut other_humans = Vec::new();
    let mut owner_present = false;
    for m in members {
        if m.is_bot {
            continue;
        }
        if m.user_id == owner_id {
            owner_present = true;
            continue;
        }
        other_humans.push(m.user_id);
    }
    if !owner_present {
        SoloEval::OwnerAbsent
    } else if other_humans.is_empty() {
        SoloEval::Eligible
    } else {
        SoloEval::OtherHumans(other_humans)
    }
}

/// Evaluate (and if eligible, register) the command menu for an unconfigured
/// group chat. Cheap no-op when the chat is configured (startup registration
/// owns those) or was evaluated already and membership hasn't changed.
pub(crate) async fn maybe_auto_register(
    bot: &Bot,
    chat_id: i64,
    cfg: &Config,
    telegram_state: &TelegramState,
) {
    let tg = &cfg.channels.telegram;

    // Configured groups belong to register_scoped_menus' audience table.
    if tg.groups.contains_key(&chat_id.to_string()) {
        return;
    }
    // Already decided and membership unchanged: skip the API round-trips.
    if telegram_state.solo_evaluated(chat_id).await.is_some() {
        return;
    }
    let Some(owner_id) =
        super::menu_scope::owner_user_id(&tg.allowed_users, &tg.bot_owner).map(|id| id as i64)
    else {
        return;
    };

    let members = collect_members(bot, chat_id, tg.allowed_users.as_slice()).await;
    let eval = evaluate_solo_group(&members, owner_id);
    let commands = super::agent::collect_command_catalog();

    match eval {
        SoloEval::Eligible => {
            telegram_state.set_solo_evaluated(chat_id, true).await;
            publish_owner_menu(bot, chat_id, owner_id, &commands).await;
        }
        // Other humans present. The catalog is published under a `ChatMember`
        // scope keyed to the owner's user_id, so it is invisible to every other
        // account in the chat: gating on "solo" was stricter than the security
        // model needs, and it left the owner staring at the bare `/start` floor
        // in any group they happen to share with a colleague. Cached as
        // registered so `menu_refresh` re-publishes it on skills changes.
        SoloEval::OtherHumans(humans) => {
            tracing::debug!(
                "Telegram: group {chat_id} has other humans ({humans:?}) — publishing the \
                 owner catalog under a per-user scope they cannot see"
            );
            telegram_state.set_solo_evaluated(chat_id, true).await;
            publish_owner_menu(bot, chat_id, owner_id, &commands).await;
        }
        SoloEval::OwnerAbsent => {
            tracing::debug!(
                "Telegram: group {chat_id} has no owner among members — nothing to register"
            );
            telegram_state.set_solo_evaluated(chat_id, false).await;
        }
    }
}

/// Gather every member observable via documented API methods: administrators
/// of the chat plus the configured allow-list probed one by one (a failed
/// probe means not-a-member, the #839 trick). The Bot API exposes no full
/// member listing, so silent non-allow-listed lurkers are invisible to us —
/// acceptable because they only ever see the default `/start` floor either way.
async fn collect_members(bot: &Bot, chat_id: i64, allowed_users: &[String]) -> Vec<MemberView> {
    let mut members = Vec::new();
    match bot.get_chat_administrators(ChatId(chat_id)).await {
        Ok(admins) => {
            for cm in admins {
                members.push(MemberView {
                    user_id: cm.user.id.0 as i64,
                    is_bot: cm.user.is_bot,
                });
            }
        }
        Err(e) => {
            tracing::debug!("Telegram: get_chat_administrators({chat_id}) failed: {e}");
            return members;
        }
    }
    for uid_s in allowed_users {
        let Some(uid) = uid_s.parse::<u64>().ok() else {
            continue;
        };
        if let Ok(cm) = bot.get_chat_member(ChatId(chat_id), UserId(uid)).await {
            members.push(MemberView {
                user_id: cm.user.id.0 as i64,
                is_bot: cm.user.is_bot,
            });
        }
    }
    members
}

/// Publish the full owner catalog into the group under a `ChatMember` scope
/// keyed to the owner — the same shape `register_scoped_menus` uses for
/// configured groups, so the two paths stay visually identical. Re-published
/// on skills changes by `menu_refresh` (#1317).
pub(crate) async fn publish_owner_menu(
    bot: &Bot,
    chat_id: i64,
    owner_id: i64,
    commands: &[teloxide::types::BotCommand],
) {
    use teloxide::payloads::SetMyCommandsSetters;
    match bot
        .set_my_commands(commands.to_vec())
        .scope(BotCommandScope::ChatMember {
            chat_id: Recipient::Id(ChatId(chat_id)),
            user_id: UserId(owner_id as u64),
        })
        .await
    {
        Ok(_) => tracing::info!(
            "Telegram: solo-owner group {chat_id} auto-registered — full menu published \
             for the owner ({commands_len} commands)",
            commands_len = commands.len()
        ),
        Err(e) => {
            tracing::warn!("Telegram: solo-owner menu registration for {chat_id} failed: {e}")
        }
    }
}

/// Membership event arrived for a previously-evaluated chat: forget the cached
/// decision so the next message re-evaluates. When the departing member is the
/// OWNER of a solo-registered group, also clear the scoped menu (an empty
/// push — the codebase never calls deleteMyCommands, which does not exist as
/// an API binding here, and Telegram stores scopes server-side).
pub(crate) async fn handle_membership_change(
    bot: &Bot,
    chat_id: i64,
    departed_user_id: i64,
    cfg: &Config,
    telegram_state: &TelegramState,
) {
    let tg = &cfg.channels.telegram;

    // Configured groups: startup/refresh path owns their menus entirely.
    if tg.groups.contains_key(&chat_id.to_string()) {
        return;
    }

    let Some(owner_id) =
        super::menu_scope::owner_user_id(&tg.allowed_users, &tg.bot_owner).map(|id| id as i64)
    else {
        return;
    };

    // Cache hygiene regardless of who left: re-evaluate on next activity.
    telegram_state.clear_solo_evaluated(chat_id).await;

    if departed_user_id != owner_id {
        return;
    }

    // Owner departed: drop their scoped menu back to the floor.
    use teloxide::payloads::SetMyCommandsSetters;
    match bot
        .set_my_commands(Vec::<teloxide::types::BotCommand>::new())
        .scope(BotCommandScope::ChatMember {
            chat_id: Recipient::Id(ChatId(chat_id)),
            user_id: UserId(owner_id as u64),
        })
        .await
    {
        Ok(_) => tracing::info!(
            "Telegram: owner left group {chat_id} — scoped menu cleared (#1155 lifecycle)"
        ),
        Err(e) => tracing::debug!(
            "Telegram: clearing owner menu after departure from {chat_id} failed (harmless if \
             the bot itself is gone): {e}"
        ),
    }
}
