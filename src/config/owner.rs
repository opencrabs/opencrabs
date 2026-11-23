//! Shared bot-owner identity resolution.
//!
//! Every channel config (Telegram, Discord, Slack, WhatsApp, Trello) exposes a
//! `bot_owner` allow list and an `is_owner()` method. To keep the semantics
//! identical across channels — and to make the rules unit-testable in one
//! place — the actual resolution lives here.
//!
//! Two modes govern ownership:
//!
//! - **Open mode**: when the channel's allow list is empty, the bot responds to
//!   everyone, so everyone is treated as an owner. This mirrors the allowlist
//!   gate ("empty = accept all") and avoids locking owners out of an
//!   unconfigured channel.
//! - **Positional fallback**: when `bot_owner` is left unset, the owner defaults
//!   to the *first* entry in the allow list. This is the user who set the
//!   channel up. Setting `bot_owner` explicitly overrides this and is the
//!   recommended, unambiguous configuration.

/// Decide whether `user_id` is a bot owner for a channel.
///
/// * If both `allowed` and `bot_owner` are empty, the channel is unconfigured
///   and denies access (returns `false`). Users must configure `allowed` or
///   `bot_owner` to enable the bot.
/// * Otherwise, if `bot_owner` is non-empty, ownership is exactly membership in
///   `bot_owner`.
/// * Otherwise (no explicit owners configured), the owner is the **first** entry
///   in `allowed` — the positional fallback to the setup user.
pub fn is_owner(allowed: &[String], bot_owner: &[String], user_id: &str) -> bool {
    if allowed.is_empty() && bot_owner.is_empty() {
        return false;
    }
    // Normalize the leading plus on BOTH sides: users naturally paste
    // international numbers as "+15551234567" into allowed_phones/bot_owner,
    // while the digits extracted from a WhatsApp JID never carry one. A raw
    // comparison silently broke owner gating for those configs (#277). The
    // other channels' ids (numeric strings, handles) never start with '+',
    // so the trim is a no-op there.
    let user_id = user_id.trim_start_matches('+');
    if !bot_owner.is_empty() {
        bot_owner
            .iter()
            .any(|o| o.trim_start_matches('+') == user_id)
    } else {
        allowed
            .first()
            .is_some_and(|o| o.trim_start_matches('+') == user_id)
    }
}

/// Compute an explicit `bot_owner` list seeded from the allow list, or `None`
/// when no seeding is needed/possible.
///
/// * If `bot_owner` is already non-empty, ownership is explicit; returns `None`
///   (nothing to seed).
/// * Otherwise the owner is seeded from the **first** allow-list entry (the
///   setup user), returning `Some(vec![first])`.
/// * If `allowed` is also empty there is nobody to seed from; returns `None`.
pub fn seed_bot_owner(allowed: &[String], bot_owner: &[String]) -> Option<Vec<String>> {
    if !bot_owner.is_empty() {
        return None;
    }
    allowed.first().map(|o| vec![o.clone()])
}

/// Resolve the numeric owner id used for per-user bot state (command menus,
/// member-event notices).
///
/// Same precedence as [`is_owner`]: an explicit `bot_owner` list decides
/// ownership, and the allow list is only consulted when `bot_owner` is empty.
/// Within a list the first entry that parses as a numeric id wins.
///
/// Non-numeric entries are skipped rather than aborting the lookup, because
/// the shipped convention allows the wildcard `"*"` anywhere in the allow
/// list and real configs do put it first:
///
/// ```toml
/// allowed_users = ["*", "1608111860"]
/// ```
///
/// Callers used to take `.first()` and parse it, which yielded `None` on every
/// such config. Every owner-menu path treats `None` as "no owner configured",
/// so the Telegram command menu was never published (only `/start` appeared in
/// the `/` picker), and member-event notices to the owner were dropped.
pub fn resolve_owner_id(allowed: &[String], bot_owner: &[String]) -> Option<u64> {
    fn first_numeric(list: &[String]) -> Option<u64> {
        list.iter()
            .map(|entry| entry.trim().trim_start_matches('+'))
            .find_map(|entry| entry.parse::<u64>().ok())
    }

    if !bot_owner.is_empty() {
        first_numeric(bot_owner)
    } else {
        first_numeric(allowed)
    }
}
