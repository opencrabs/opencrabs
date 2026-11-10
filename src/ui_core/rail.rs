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

/// The platform words a channel row can lead with.
const PLATFORMS: [&str; 5] = ["Telegram", "Discord", "Slack", "WhatsApp", "Trello"];

/// Whether a title's leading word is a channel platform, which a name column
/// never prints: `Telegram: Build [chat:-100:topic:42]` is the row, not the
/// topic.
fn is_platform(word: &str) -> bool {
    PLATFORMS.iter().any(|p| p.eq_ignore_ascii_case(word))
}

/// The platform a stored channel title leads with (#2766).
///
/// The GUI draws this platform's mark in front of the name instead of the
/// word, so a card says the chat once. A title whose first word is not a known
/// platform is not a channel row and gets no mark.
pub fn platform_of(stored: Option<&str>) -> Option<&'static str> {
    let stored = stored?.trim_start();
    let word = stored.split_once(':')?.0.trim_end();
    PLATFORMS
        .iter()
        .copied()
        .find(|p| p.eq_ignore_ascii_case(word))
}

/// A stored title with the bookkeeping off it: the `[chat:...]` key and a
/// leading platform word. What is left is the name the row is about.
fn body_of(stored: &str) -> &str {
    let trimmed = stored.trim();
    let base = match trimmed.rfind("[chat:") {
        Some(i) if trimmed.ends_with(']') => trimmed.get(..i).unwrap_or(trimmed).trim_end(),
        _ => trimmed,
    };
    match base.split_once(':') {
        Some((prefix, rest)) if is_platform(prefix.trim_end()) => rest.trim_start(),
        _ => base,
    }
}

/// The chat a stored title belongs to (#2766): platform and channel key off,
/// and the topic segment (`<chat> / <topic>`) dropped, so
/// `Telegram: OpenCrabs Dev / Memes [chat:-100:topic:5]` reads
/// `OpenCrabs Dev`.
pub fn chat_name(stored: Option<&str>) -> String {
    let Some(stored) = stored.map(str::trim).filter(|s| !s.is_empty()) else {
        return UNTITLED.to_string();
    };
    let body = body_of(stored);
    // The topic segment is the last one: `telegram_base_title` strips it the
    // same way, so the two parses cannot disagree about which part is the chat.
    let chat = match body.rsplit_once(" / ") {
        Some((chat, _)) => chat.trim_end(),
        None => body,
    };
    if chat.is_empty() {
        UNTITLED.to_string()
    } else {
        chat.to_string()
    }
}

/// The topic a stored title names, when it names one (#2766).
///
/// A base chat row has no topic segment, so it resolves to `None` and the
/// caller falls back to [`chat_name`].
pub fn topic_name(stored: Option<&str>) -> Option<String> {
    let stored = stored.map(str::trim).filter(|s| !s.is_empty())?;
    let body = body_of(stored);
    let (_, topic) = body.rsplit_once(" / ")?;
    let topic = topic.trim();
    (!topic.is_empty()).then(|| topic.to_string())
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
    // A topic row reads as its topic alone (#2766): the chat is on the card
    // above it, so repeating it in every chip would print one name per chip.
    // A base chat row has no topic segment and keeps its chat name.
    topic_name(stored).unwrap_or_else(|| chat_name(stored))
}

#[cfg(test)]
mod tests {
    use super::{UNTITLED, chat_name, chip_name, platform_of, rail_title, topic_name};

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

    /// The card replaces the platform word with a mark (#2766), so the word
    /// has to be identifiable on its own.
    #[test]
    fn a_channel_row_names_its_platform() {
        assert_eq!(
            platform_of(Some("Telegram: OpenCrabs Dev [chat:-100]")),
            Some("Telegram")
        );
        assert_eq!(
            platform_of(Some("Discord: #general [chat:discord-42]")),
            Some("Discord")
        );
        assert_eq!(platform_of(Some("Rust: borrow rules")), None);
        assert_eq!(platform_of(Some("Plan day")), None);
        assert_eq!(platform_of(None), None);
    }

    /// The card says the chat once. The topic belongs to the chip under it, so
    /// the card must not repeat it (#2766).
    #[test]
    fn a_card_names_the_chat_without_the_topic() {
        let topic_row = "Telegram: OpenCrabs Dev / Memes [chat:-1003954413375:topic:42]";
        assert_eq!(chat_name(Some(topic_row)), "OpenCrabs Dev");
        assert_eq!(
            chat_name(Some("Telegram: OpenCrabs Dev [chat:-1003954413375]")),
            "OpenCrabs Dev"
        );
        assert_eq!(chat_name(Some("Plan day")), "Plan day");
        assert_eq!(chat_name(None), UNTITLED);
        assert_eq!(chat_name(Some("   ")), UNTITLED);
    }

    /// A chip under the card names its own topic and nothing else (#2766).
    #[test]
    fn a_topic_chip_reads_its_topic_alone() {
        let topic_row = "Telegram: OpenCrabs Dev / Memes [chat:-1003954413375:topic:42]";
        assert_eq!(topic_name(Some(topic_row)).as_deref(), Some("Memes"));
        assert_eq!(
            topic_name(Some("Telegram: OpenCrabs Dev [chat:-1003954413375]")),
            None
        );
        assert_eq!(topic_name(None), None);
        assert_eq!(chip_name(Some(topic_row), None), "Memes");
        // A base chat row has no topic to name, so it keeps the chat.
        assert_eq!(
            chip_name(Some("Telegram: OpenCrabs Dev [chat:-100]"), None),
            "OpenCrabs Dev"
        );
    }
}
