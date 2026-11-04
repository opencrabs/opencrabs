//! Tests for `ui_core::rail` (#2490): topic-key parsing and display-title
//! precedence. Pure functions, no database.

use crate::ui_core::rail::{UNTITLED, rail_title, topic_thread};

#[test]
fn a_topic_scoped_suffix_parses_into_its_chat_and_thread() {
    let (chat, thread) = topic_thread("Devops [chat:-100123:topic:7]").unwrap();
    assert_eq!(chat, "-100123");
    assert_eq!(thread, "7");
}

#[test]
fn a_base_chat_suffix_resolves_to_no_thread() {
    assert!(topic_thread("Ops [chat:-100123]").is_none());
}

#[test]
fn non_numeric_halves_are_rejected_so_no_fake_key_reaches_the_store() {
    assert!(topic_thread("weird [chat:abc:topic:7]").is_none());
    assert!(topic_thread("weird [chat:-100123:topic:x]").is_none());
    assert!(topic_thread("weird [chat::topic:7]").is_none());
}

#[test]
fn a_title_that_merely_contains_the_shape_is_not_a_key() {
    // rfind takes the LAST [chat:...], and a trailing one that does not close
    // or fails to parse must not yield the inner one.
    assert!(topic_thread("note [chat:1:topic:2] gone [chat:nope]").is_none());
}

#[test]
fn the_live_topic_name_wins_over_the_stored_one() {
    assert_eq!(
        rail_title(Some("topic:2"), Some("Devops")),
        "Devops",
        "rename on Telegram must reach the rail"
    );
}

#[test]
fn the_stored_name_keeps_its_place_when_no_topic_name_exists() {
    assert_eq!(rail_title(Some("Devops session"), None), "Devops session");
}

#[test]
fn blank_or_whitespace_topic_names_are_not_names() {
    assert_eq!(
        rail_title(Some("Devops session"), Some("   ")),
        "Devops session"
    );
    assert_eq!(
        rail_title(Some("Devops session"), Some("")),
        "Devops session"
    );
}

#[test]
fn nothing_at_all_leaves_the_untitled_label() {
    assert_eq!(rail_title(None, None), UNTITLED);
    assert_eq!(rail_title(Some("  "), Some("  ")), UNTITLED);
}
