//! Rail row titles for channel topics (#2490).
//!
//! A Telegram forum topic binds its own session, but Telegram stays the source
//! of truth for what that topic is called. Creation names and renames land in
//! `channel_messages.topic_name`, with renames ranked above creation names
//! (#143). The session row keeps whatever name it was created with, so a rail
//! that prints the stored session name drifts the moment a topic is renamed.
//!
//! This module owns the precedence and nothing else: no database, no GPUI, no
//! ratatui. Both rails call it, so the two surfaces cannot disagree.

/// What a rail row prints when it has no name at all.
pub const UNTITLED: &str = "Untitled";

/// The `(chat_id, thread_id)` a session title is scoped to.
///
/// A topic-bound title ends with `[chat:<id>:topic:<tid>]`
/// (`session_resolve::chat_id_suffix`). A base chat ends with `[chat:<id>]` and
/// resolves to `None`: it has no forum topic to look up. Both halves must parse
/// as numbers, so a title that merely contains the shape cannot send a made-up
/// key to the store.
pub fn topic_thread(title: &str) -> Option<(&str, &str)> {
    let start = title.rfind("[chat:")?;
    let body = title.get(start + "[chat:".len()..)?.strip_suffix(']')?;
    let (chat, thread) = body.split_once(":topic:")?;
    let (chat, thread) = (chat.trim(), thread.trim());
    let numeric = chat.parse::<i64>().is_ok() && thread.parse::<i64>().is_ok();
    (numeric && !chat.is_empty() && !thread.is_empty()).then_some((chat, thread))
}

/// The display title for a rail row: the live topic name when there is one,
/// otherwise the session's own name.
///
/// A blank or whitespace-only topic name is not a name, so the stored name
/// keeps its place instead of the row going empty.
pub fn rail_title(stored: Option<&str>, topic: Option<&str>) -> String {
    let topic = topic.map(str::trim).filter(|t| !t.is_empty());
    if let Some(topic) = topic {
        return topic.to_string();
    }
    match stored.map(str::trim).filter(|s| !s.is_empty()) {
        Some(stored) => stored.to_string(),
        None => UNTITLED.to_string(),
    }
}
