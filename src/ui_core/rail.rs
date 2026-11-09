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

/// Whether a title's leading word is a channel platform, which a name column
/// never prints: `Telegram: Build [chat:-100:topic:42]` is the row, not the
/// topic.
fn is_platform(word: &str) -> bool {
    matches!(
        word,
        "Telegram" | "Discord" | "Slack" | "WhatsApp" | "Trello"
    )
}

/// What a topic chip prints (#2762).
///
/// Same precedence as [`rail_title`], then the bookkeeping comes off: the
/// channel key (`[chat:-100:topic:42]`) and a platform prefix (`Telegram: `)
/// are how the row is scoped, never what the topic is called. Before this the
/// strip fell back to the raw title, so every topic whose live name had not
/// loaded yet was drawn as `Telegram: Open…`, and the chip stopped naming the
/// conversation it opens.
///
/// A title that carries its own colon (`Rust: borrow rules`) keeps every word:
/// only a known platform word is a prefix.
pub fn chip_name(stored: Option<&str>, topic: Option<&str>) -> String {
    let topic = topic.map(str::trim).filter(|t| !t.is_empty());
    if let Some(topic) = topic {
        return topic.to_string();
    }
    let Some(stored) = stored.map(str::trim).filter(|s| !s.is_empty()) else {
        return UNTITLED.to_string();
    };
    let base = match stored.rfind("[chat:") {
        Some(i) if stored.ends_with(']') => stored[..i].trim_end(),
        _ => stored,
    };
    let base = match base.split_once(':') {
        Some((prefix, rest)) if is_platform(prefix) => rest.trim_start(),
        _ => base,
    };
    if base.is_empty() {
        UNTITLED.to_string()
    } else {
        base.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{UNTITLED, chip_name, rail_title};

    /// The live topic name still wins, so a rename in Telegram reaches the
    /// chip on the next refresh pass.
    #[test]
    fn a_chip_prefers_the_live_topic_name() {
        let stored = Some("Telegram: Build [chat:-100:topic:42]");
        assert_eq!(chip_name(stored, Some("Memes")), "Memes");
        assert_eq!(rail_title(stored, Some("Memes")), "Memes");
    }

    /// The defect the strip shipped with: no live name yet, so the raw row
    /// title came through whole.
    #[test]
    fn a_chip_strips_the_channel_key_and_platform_prefix() {
        let stored = Some("Telegram: Build [chat:-100:topic:42]");
        assert_eq!(chip_name(stored, None), "Build");
        assert_eq!(chip_name(stored, Some("   ")), "Build");
        assert_eq!(
            chip_name(Some("Discord: #general [chat:discord-42]"), None),
            "#general"
        );
    }

    /// A name is a name. A colon in the middle of one is not a platform.
    #[test]
    fn a_chip_keeps_a_title_that_is_not_a_platform_row() {
        assert_eq!(
            chip_name(Some("Rust: borrow rules [chat:-100:topic:7]"), None),
            "Rust: borrow rules"
        );
        assert_eq!(chip_name(Some("Plan day"), None), "Plan day");
        assert_eq!(chip_name(None, None), UNTITLED);
        assert_eq!(chip_name(Some("  "), None), UNTITLED);
    }
}
