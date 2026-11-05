//! Draft-preserving composer history shared by the TUI and GUI.

use crate::ui_core::input_history::{input_history_down, input_history_up, record_input_history};

#[test]
fn input_history_round_trip_restores_the_exact_typed_draft() {
    let entries = vec!["older".to_string(), "newest".to_string()];
    let original = "draft 🦀 with \x1b[<35;116;77M literal text\nand two lines";
    let mut buffer = original.to_string();
    let mut index = None;
    let mut draft = String::new();

    assert!(input_history_up(
        &entries,
        &mut buffer,
        &mut index,
        &mut draft
    ));
    assert_eq!(buffer, "newest");
    assert_eq!(index, Some(1));
    assert_eq!(draft, original);

    assert!(input_history_up(
        &entries,
        &mut buffer,
        &mut index,
        &mut draft
    ));
    assert_eq!(buffer, "older");
    assert_eq!(index, Some(0));

    assert!(input_history_down(
        &entries,
        &mut buffer,
        &mut index,
        &mut draft
    ));
    assert_eq!(buffer, "newest");
    assert!(input_history_down(
        &entries,
        &mut buffer,
        &mut index,
        &mut draft
    ));
    assert_eq!(buffer, original);
    assert_eq!(index, None);
    assert!(draft.is_empty());
}

#[test]
fn input_history_without_entries_leaves_the_draft_alone() {
    let mut buffer = "unfinished".to_string();
    let mut index = None;
    let mut draft = String::new();
    assert!(!input_history_up(&[], &mut buffer, &mut index, &mut draft));
    assert!(!input_history_down(
        &[],
        &mut buffer,
        &mut index,
        &mut draft
    ));
    assert_eq!(buffer, "unfinished");
}

#[test]
fn input_history_recording_deduplicates_and_caps_entries() {
    let mut entries = Vec::new();
    record_input_history(&mut entries, "  first  ");
    record_input_history(&mut entries, "first");
    record_input_history(&mut entries, "   ");
    assert_eq!(entries, ["first"]);
    for number in 0..510 {
        record_input_history(&mut entries, &format!("message {number}"));
    }
    assert_eq!(entries.len(), 500);
    assert_eq!(entries.first().map(String::as_str), Some("message 10"));
    assert_eq!(entries.last().map(String::as_str), Some("message 509"));
}

#[test]
fn input_history_both_composers_use_the_shared_recall_machine() {
    let terminal = include_str!("../tui/app/input.rs");
    assert!(terminal.contains("input_history_up("));
    assert!(terminal.contains("input_history_down("));
    assert!(
        !terminal.contains("(self.input_buffer.is_empty() || self.input_history_index.is_some())")
    );
    assert!(terminal.contains("if !restoring_draft"));

    let gui = include_str!("../gui/editor.rs");
    assert!(gui.contains("input_history_up("));
    assert!(gui.contains("input_history_down("));
    assert!(gui.contains("record_input_history(&mut self.input_history, &text)"));
}
