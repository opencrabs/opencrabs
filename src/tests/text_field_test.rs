//! The one text field behind the composer, the Seeker and the name prompts
//! (#2892).
//!
//! Renaming a conversation or a topic used to append characters to a `String`:
//! the caret was welded to the end, `cmd-a` selected nothing, `alt-left` moved
//! nothing and `cmd+x` cut nothing. The engine now lives in
//! `ui_core::text_field` and three frontends hold it. These tests pin the
//! engine itself and then read the three call sites, because the window has no
//! headless harness and a prompt that quietly went back to a `String` would
//! still compile.

use crate::ui_core::text_field::{self, ClipboardRequest, TextField, TextFieldEdit};

#[test]
fn typing_lands_at_the_caret_not_at_the_end() {
    let mut field = TextField::from_text("alpha beta");
    // from_text opens with the caret at the end, which is what a rename
    // prompt wants: the old name is there to edit, not to select.
    assert_eq!(field.caret(), 10);
    field.move_caret(-4);
    assert!(field.type_char('X'));
    assert_eq!(field.text(), "alpha Xbeta");
    assert_eq!(field.caret(), 7);
    assert!(field.backspace());
    assert_eq!(field.text(), "alpha beta");
    assert!(field.delete_forward());
    assert_eq!(field.text(), "alpha eta");
}

#[test]
fn a_word_walk_moves_and_selects_by_whole_words() {
    let mut field = TextField::from_text("one two three");
    field.move_caret_word(-1);
    assert_eq!(field.caret(), 8, "alt-left lands on the start of `three`");
    field.move_caret_word(-1);
    assert_eq!(field.caret(), 4, "and again on the start of `two`");
    field.move_caret_word(1);
    assert_eq!(field.caret(), 8, "alt-right walks past `two` and its gap");
    field.select_word_to(-1);
    assert_eq!(field.selected_text(), Some("two "));
}

#[test]
fn the_composer_chords_all_reach_the_field() {
    // Clipboard, the line kill and the word kill, the three keys the prompts
    // never had.
    assert_eq!(
        text_field::editing_chord("a", false, true, false, false),
        Some(TextFieldEdit::SelectAll)
    );
    assert_eq!(
        text_field::editing_chord("u", true, false, false, false),
        Some(TextFieldEdit::DeleteToLineStart)
    );
    assert_eq!(
        text_field::editing_chord("w", true, false, false, false),
        Some(TextFieldEdit::Backspace { word: true })
    );
    assert_eq!(
        text_field::editing_chord("backspace", false, false, true, false),
        Some(TextFieldEdit::Backspace { word: true }),
        "alt-backspace deletes the word before the caret, not one character"
    );
    assert_eq!(
        text_field::editing_chord("left", false, true, false, false),
        Some(TextFieldEdit::Home { select: false }),
        "cmd-left reaches the line start, alt-left walks a word"
    );
    assert_eq!(text_field::editing_chord("z", true, false, false, false), None);
}

#[test]
fn copy_and_cut_ask_the_frontend_for_the_clipboard() {
    let mut field = TextField::from_text("rename me");
    field.select_all();
    assert_eq!(
        text_field::apply(&mut field, TextFieldEdit::Copy),
        Some(ClipboardRequest::Write("rename me".to_string()))
    );
    assert_eq!(field.text(), "rename me", "copy leaves the name alone");
    assert_eq!(
        text_field::apply(&mut field, TextFieldEdit::Cut),
        Some(ClipboardRequest::Write("rename me".to_string()))
    );
    assert_eq!(field.text(), "");
    assert_eq!(
        text_field::apply(&mut field, TextFieldEdit::Paste),
        Some(ClipboardRequest::Read),
        "paste is the frontend's half of the job"
    );
    assert_eq!(
        text_field::apply(&mut field, TextFieldEdit::End { select: false }),
        None,
        "movement needs no clipboard"
    );
}

#[test]
fn a_selection_splits_into_three_slices_on_char_boundaries() {
    // Multibyte on both sides of the selection: a byte slice computed in a
    // frontend would panic here, and that is why the split lives in the core.
    let mut field = TextField::from_text("héllo wörld");
    field.move_caret_to(2);
    field.select_to(7);
    assert_eq!(field.render_spans(), ("hé", "llo w", "örld"));
    assert_eq!(field.selection_bytes(), Some((3, 8)));
    assert_eq!(field.caret_byte(), 8);
    assert_eq!(field.selected_text(), Some("llo w"));
}

#[test]
fn the_three_prompts_hold_the_shared_field() {
    // The window's name card: the prompt arm reads the same chord table as
    // the Seeker, and the buffer is seeded as a field rather than a String.
    let dashboard = crate::tests::gui_dashboard_source::DASHBOARD;
    assert!(dashboard.contains("text_field::editing_chord("));
    assert!(dashboard.contains("self.apply_topic_edit(edit, cx)"));
    assert!(dashboard.contains("pub(crate) fn apply_topic_edit("));
    assert!(dashboard.contains("TextField::from_text(&current)"));
    assert!(dashboard.contains("Some(TextField::new())"));
    // The card paints a caret and a selection instead of `{draft}|`.
    let overlays = include_str!("../gui/overlays.rs");
    assert!(overlays.contains("draft: &TextField,"));
    assert!(overlays.contains("draft.render_spans()"));
    assert!(overlays.contains("palette::current().selection"));
    assert!(!overlays.contains("{draft}|"));

    // The terminal's two prompts walk the same buffer.
    let input = include_str!("../tui/app/input.rs");
    assert!(input.contains("fn prompt_edit(event: &crossterm::event::KeyEvent)"));
    assert!(input.contains("fn apply_prompt_edit(field: &mut TextField"));
    assert!(input.contains("apply_prompt_edit(&mut self.session_rename_buffer, edit)"));
    assert!(input.contains("apply_prompt_edit(&mut self.topic_name_buffer, edit)"));
    // `prompt_edit` takes the event by reference, so both prompt arms have to
    // borrow it. Handing it over by value does not compile (#2892).
    assert!(input.contains("prompt_edit(&event)"));
    assert!(!input.contains("prompt_edit(event)"));
    assert!(input.contains("TextField::from_text(&session.title"));
    let state = include_str!("../tui/app/state.rs");
    assert!(state.contains("pub session_rename_buffer: TextField,"));
    assert!(state.contains("pub(crate) topic_name_buffer: TextField,"));
    let render = include_str!("../tui/render/sessions.rs");
    assert!(render.contains("field.render_spans()"));
    assert!(render.contains("Modifier::REVERSED"));
}
