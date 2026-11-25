//! Classifying per-member menu scope failures (#839).
//!
//! Global `allowed_users` are admins allowed in every group, so their menus
//! must be cleared wherever they are members. But nothing tells us which
//! groups those are: the config records a global grant, not a roster.
//!
//! Telegram answers a `BotCommandScope::ChatMember` call for a user who is not
//! in that chat with an invalid-id error, and that is the only membership
//! signal available here. `getChatMember` fails the same way for a user the bot
//! has never seen in the chat, so probing first costs a round trip per user per
//! group and still errors. The failed call is the membership test.

/// The bot owner's numeric user id, or `None` when the config carries no
/// resolvable id at all.
///
/// `allowed_users` is a *list*, not an ordered pair: the shipped convention
/// permits the wildcard `"*"` anywhere in it, and plenty of real configs put
/// it first (`allowed_users = ["*", "1608111860"]`). Callers used to take
/// `.first()` and parse it, which returned `None` on exactly those configs, and every owner-menu path treats `None` as "no owner configured", so
/// `register_scoped_menus` returned early and the Telegram `/` picker was
/// left with nothing but `/start`. The first entry that parses wins;
/// non-numeric entries (`"*"`, channel handles, typos) are skipped instead of
/// aborting the lookup.
///
/// Delegates to [`crate::config::owner::resolve_owner_id`] so the per-user
/// menu and the command-level owner gate resolve ownership the same way:
/// explicit `bot_owner` first, positional fallback second.
pub fn owner_user_id(allowed_users: &[String], bot_owner: &[String]) -> Option<u64> {
    crate::config::owner::resolve_owner_id(allowed_users, bot_owner)
}

/// Does this error mean "that user is not in this chat" rather than a fault?
///
/// Matched on the rendered error because teloxide surfaces these as opaque API
/// strings rather than typed variants.
pub fn means_not_a_member(err: &str) -> bool {
    const NOT_A_MEMBER: [&str; 3] = [
        "USER_ID_INVALID",
        "PARTICIPANT_ID_INVALID",
        "USER_NOT_PARTICIPANT",
    ];
    let upper = err.to_ascii_uppercase();
    NOT_A_MEMBER.iter().any(|code| upper.contains(code))
}

/// The supergroup id a migrated group moved to, if this error reports one.
///
/// A basic group that upgrades to a supergroup gets a new chat id, and every
/// later call against the old one fails. Telegram hands the replacement back in
/// the message itself:
///
/// ```text
/// The group has been migrated to a supergroup with ID #-1004441241066
/// ```
///
/// so recovery needs no extra API call — the failure carries its own fix
/// (#946). Parsed from the rendered string for the same reason
/// `means_not_a_member` is: teloxide surfaces these as opaque API text.
pub fn migrated_to(err: &str) -> Option<i64> {
    let tail = err.split("migrated to a supergroup with ID").nth(1)?;
    let digits: String = tail
        .trim()
        .trim_start_matches('#')
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}
