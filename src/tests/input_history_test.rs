//! Draft-preserving composer history shared by the TUI and GUI.

use crate::ui_core::input_history::{
    Vertical, VerticalAction, input_history_down, input_history_up, record_input_history,
    vertical_action,
};

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
    // Both surfaces record through the shared list and the one file writer;
    // neither keeps a private Vec any more.
    assert!(gui.contains("record_input_history(&mut entries, entry)"));
    assert!(gui.contains("composer::append_history(&self.history_path, entry)"));
    assert!(!gui.contains("record_input_history(&mut self.input_history"));
}

#[test]
fn input_history_entering_from_an_empty_field_keeps_the_parked_draft() {
    // Esc twice in the terminal clears the field and parks the text in the
    // stash. Up from that empty field must not overwrite the parked draft
    // with an empty string, or the rescue is a no-op (#2494).
    let entries = vec!["newest".to_string()];
    let mut buffer = String::new();
    let mut index = None;
    let mut draft = "what i was writing".to_string();

    assert!(input_history_up(
        &entries,
        &mut buffer,
        &mut index,
        &mut draft
    ));
    assert_eq!(buffer, "newest");
    assert_eq!(draft, "what i was writing", "the parked draft survives");

    assert!(input_history_down(
        &entries,
        &mut buffer,
        &mut index,
        &mut draft
    ));
    assert_eq!(buffer, "what i was writing", "and comes back on Down");
    assert!(draft.is_empty());
}

/// The caret keeps the vertical key until it is out of the text, and only
/// then does the history take it (#3188).
///
/// This is the rule both surfaces ask, so a recalled block of text is walked
/// line by line in the terminal and in the window, and the history moves one
/// entry at a time once the caret is out of the way.
#[test]
fn the_vertical_key_is_the_carets_until_the_caret_is_out_of_the_text() {
    let text = "first line\nsecond line";
    let len = text.len();

    // The caret sits inside the text: it keeps the key, both ways.
    assert_eq!(
        vertical_action(len, len, Vertical::Up),
        VerticalAction::Caret
    );
    assert_eq!(
        vertical_action(0, len, Vertical::Down),
        VerticalAction::Caret
    );
    // The caret is at an end of the text: the history takes it.
    assert_eq!(
        vertical_action(0, len, Vertical::Up),
        VerticalAction::History
    );
    assert_eq!(
        vertical_action(len, len, Vertical::Down),
        VerticalAction::History
    );
    // An empty field has nowhere for the caret to go.
    assert_eq!(vertical_action(0, 0, Vertical::Up), VerticalAction::History);
    assert_eq!(
        vertical_action(0, 0, Vertical::Down),
        VerticalAction::History
    );
}

/// The terminal and the window ask the same rule, so they cannot drift apart
/// (#3188). Each one names the shared answer instead of keeping a private
/// predicate that says when the caret is out of the way.
#[test]
fn both_surfaces_ask_the_same_vertical_rule() {
    let terminal = include_str!("../tui/app/input.rs");
    let gui = include_str!("../gui/editor.rs");

    for surface in [terminal, gui] {
        assert!(
            surface.contains("vertical_action("),
            "the surface asks the shared rule"
        );
        assert!(
            surface.contains("Vertical::Up") && surface.contains("Vertical::Down"),
            "both directions go through it"
        );
    }
    assert!(
        !terminal.contains("fn can_cursor_move_up("),
        "the terminal keeps no private answer of its own"
    );
    assert!(
        !terminal.contains("fn can_cursor_move_down("),
        "the terminal keeps no private answer of its own"
    );
}

/// The window walks the rows of the text it holds (#3188).
///
/// A recall swaps the whole buffer, and the layout the window kept is the
/// shape of the text it painted before, so walking those rows moves the caret
/// to an offset that means nothing. The rows are trusted only while they are
/// the rows of the text in the field.
#[test]
fn the_window_only_walks_rows_that_belong_to_the_text_in_the_field() {
    let gui = include_str!("../gui/editor.rs");
    assert!(
        gui.contains("shaped_matches_text"),
        "the window checks the layout against the text it holds"
    );
    assert!(
        gui.contains("cache.key.0 == self.content"),
        "and the check is the text the layout was shaped from"
    );
}

/// One file, one format, one entry per record (#3187).
///
/// The file used to hold one entry per line, which cannot hold a pasted
/// block, so a log came back torn into fragments. Both surfaces read and
/// write it through the same module, so neither can keep the old shape.
#[test]
fn both_surfaces_read_and_write_the_one_entry_per_record_file() {
    let terminal = include_str!("../tui/app/input.rs");
    let gui_app = include_str!("../gui/app.rs");
    let gui = include_str!("../gui/editor.rs");

    assert!(terminal.contains("composer::load_history("));
    assert!(terminal.contains("composer::append_history("));
    assert!(gui_app.contains("composer::load_history("));
    assert!(gui.contains("composer::append_history("));

    let composer = include_str!("../ui_core/composer.rs");
    assert!(
        composer.contains("pub const HISTORY_FORMAT_MARKER:"),
        "the file says which format it is in"
    );
    assert!(
        composer.contains("fn encode_history_record(")
            && composer.contains("fn decode_history_record("),
        "an entry is a record with its newlines escaped"
    );
}
