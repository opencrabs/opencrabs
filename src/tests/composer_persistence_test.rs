//! The composer keeps what the user typed: across history recall, across a
//! pane focus change, and across a session switch (#2494).
//!
//! The recall invariant itself is covered in `input_history_test`. Here the
//! state around it is pinned: the pane's stash, the escape rule, and the
//! session-keyed draft store the terminal files text in.

use crate::tui::pane::{PaneId, PaneManager, SplitDirection};
use crate::ui_core::composer::{ComposerDraft, DraftStore, escape_clears_input};

const MESSAGING_SRC: &str = include_str!("../tui/app/messaging.rs");
const INPUT_SRC: &str = include_str!("../tui/app/input.rs");

#[test]
fn composer_pane_stash_carries_the_draft_the_recall_was_covering() {
    let mut panes = PaneManager::new();
    assert_eq!(panes.focused, PaneId::ROOT);

    let mut buffer = "half a sentence".to_string();
    let mut cursor = buffer.len();
    let mut index = Some(0);
    let mut stash = "what i typed before browsing".to_string();

    panes.stash_input(&mut buffer, &mut cursor, &mut index, &mut stash);
    assert!(buffer.is_empty(), "the pane took the text");
    assert!(stash.is_empty(), "the pane took the recall's draft too");
    assert_eq!(index, None);

    let new_pane = panes.split(SplitDirection::Horizontal);
    assert_eq!(panes.focused, new_pane);

    // The new pane has nothing of its own.
    panes.adopt_input(&mut buffer, &mut cursor, &mut index, &mut stash);
    assert!(buffer.is_empty());
    assert!(stash.is_empty());
    assert_eq!(index, None);

    // Focusing the original pane brings everything back, draft included.
    panes.focused = PaneId::ROOT;
    panes.adopt_input(&mut buffer, &mut cursor, &mut index, &mut stash);
    assert_eq!(buffer, "half a sentence");
    assert_eq!(index, Some(0));
    assert_eq!(
        stash, "what i typed before browsing",
        "a focus change mid-recall must not orphan the draft"
    );
}

#[test]
fn composer_pane_clear_drops_the_stash_with_the_buffer() {
    let mut panes = PaneManager::new();
    if let Some(pane) = panes.focused_pane_mut() {
        pane.input_buffer = "text".to_string();
        pane.input_history_index = Some(3);
        pane.input_history_stash = "draft".to_string();
        pane.clear_input();
    }
    let pane = panes.focused_pane().expect("a focused pane");
    assert!(pane.input_buffer.is_empty());
    assert!(pane.input_history_stash.is_empty());
    assert_eq!(pane.input_history_index, None);
}

#[test]
fn composer_escape_rule_survives_a_recalled_line() {
    // The bug: Esc twice while browsing wrote the recalled entry into the
    // stash and left the walk open, so the typed draft was gone for good.
    let mut buffer = "recalled".to_string();
    let mut cursor = buffer.len();
    let mut index = Some(0);
    let mut stash = "typed by hand".to_string();
    escape_clears_input(&mut buffer, &mut cursor, &mut index, &mut stash);
    assert!(buffer.is_empty());
    assert_eq!(index, None, "the walk ends, so Up can restore the draft");
    assert_eq!(stash, "typed by hand");

    // Up then recovers it, exactly as Arrow Up does on an empty field.
    buffer = std::mem::take(&mut stash);
    assert_eq!(buffer, "typed by hand");
}

#[test]
fn composer_switch_back_returns_the_same_draft() {
    let mut drafts = DraftStore::new();
    let session = uuid::Uuid::new_v4();
    drafts.store(
        session,
        ComposerDraft {
            text: "unsent".to_string(),
            cursor: 6,
            history_index: None,
            history_stash: String::new(),
        },
    );
    let back = drafts.take(session).expect("the draft came back");
    assert_eq!(back.text, "unsent");
    assert_eq!(back.cursor, 6);
}

/// The session rebind is the one place that decides a draft's fate, so both
/// doors must walk it: `load_session` (rail, Tab, pane rotation) and
/// `create_new_session` (Ctrl+N).
#[test]
fn composer_session_switch_files_and_restores_the_draft() {
    let load = ["self.bind_focused_pane_", "session(session_id);"].concat();
    assert!(
        MESSAGING_SRC.contains(&load),
        "load_session rebinds through the draft-carrying helper"
    );
    let create = ["self.bind_focused_pane_", "session(session.id);"].concat();
    assert!(
        MESSAGING_SRC.contains(&create),
        "a new session must not inherit the previous draft"
    );

    let (_, helper) = MESSAGING_SRC
        .split_once(&["fn bind_focused_pane_", "session(&mut self"].concat())
        .expect("the helper exists");
    let filed = ["self.drafts.", "store(outgoing, draft)"].concat();
    assert!(helper.contains(&filed), "leaving files the draft");
    let restored = ["self.drafts.", "take(session_id)"].concat();
    assert!(helper.contains(&restored), "arriving restores it");
}

/// Esc twice clears the field through the shared rule, not an inline copy of
/// it: two copies drift, and the drifted one lost the draft.
#[test]
fn composer_escape_is_shared_not_reimplemented() {
    let call = ["composer::escape_", "clears_input("].concat();
    assert!(
        INPUT_SRC.contains(&call),
        "the Esc handler clears through ui_core"
    );
    let inline = ["self.input_history_stash = self.", "input_buffer.clone();"].concat();
    assert!(
        !INPUT_SRC.contains(&inline),
        "no inline stash copy left behind"
    );
}
