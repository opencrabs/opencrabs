//! Seeker editing chords and the GUI wiring that applies them (#2534).

use crate::ui_core::palette::{PaletteState, SeekerEdit, byte_to_char, editing_chord};

#[test]
fn seeker_editing_chords_cover_clipboard_words_and_line_edges() {
    assert_eq!(
        editing_chord("a", false, true, false, false),
        Some(SeekerEdit::SelectAll)
    );
    assert_eq!(
        editing_chord("c", true, false, false, false),
        Some(SeekerEdit::Copy)
    );
    assert_eq!(
        editing_chord("x", false, true, false, false),
        Some(SeekerEdit::Cut)
    );
    assert_eq!(
        editing_chord("v", true, false, false, false),
        Some(SeekerEdit::Paste)
    );
    assert_eq!(
        editing_chord("left", false, false, true, true),
        Some(SeekerEdit::Move {
            delta: -1,
            word: true,
            select: true,
        })
    );
    assert_eq!(
        editing_chord("right", true, false, false, false),
        Some(SeekerEdit::Move {
            delta: 1,
            word: true,
            select: false,
        })
    );
    assert_eq!(
        editing_chord("left", false, true, false, true),
        Some(SeekerEdit::Home { select: true })
    );
    assert_eq!(
        editing_chord("end", false, false, false, false),
        Some(SeekerEdit::End { select: false })
    );
    assert_eq!(
        editing_chord("backspace", false, false, true, false),
        Some(SeekerEdit::Backspace { word: true })
    );
    assert_eq!(
        editing_chord("delete", false, false, false, false),
        Some(SeekerEdit::Delete { word: false })
    );
    assert_eq!(editing_chord("c", false, false, false, false), None);
}

#[test]
fn seeker_editing_shift_word_selection_survives_repeated_presses() {
    let mut state = PaletteState::new(Vec::new());
    state.insert_text("alpha beta");
    state.select_word_to(-1);
    assert_eq!(state.selected_text(), Some("beta"));
    state.select_word_to(-1);
    assert_eq!(state.selected_text(), Some("alpha beta"));
    state.move_caret_to(3);
    assert_eq!(state.selection(), None);
    assert_eq!(state.caret(), 3);
    state.select_to(7);
    assert_eq!(state.selected_text(), Some("ha b"));
    state.move_caret_to(usize::MAX);
    assert_eq!(state.caret(), "alpha beta".chars().count());
}

#[test]
fn seeker_editing_mouse_offsets_never_split_multibyte_text() {
    let text = "aé🦀z";
    assert_eq!(byte_to_char(text, 0), 0);
    assert_eq!(byte_to_char(text, 3), 2);
    assert_eq!(byte_to_char(text, 7), 3);
    assert_eq!(byte_to_char(text, text.len()), 4);
    let mut state = PaletteState::new(Vec::new());
    state.insert_text(text);
    state.move_caret_to(2);
    state.type_char('X');
    assert_eq!(state.query(), "aéX🦀z");
}

#[test]
fn seeker_editing_cut_and_paste_replace_the_live_selection() {
    let mut state = PaletteState::new(Vec::new());
    state.insert_text("find the right session");
    state.select_all();
    assert_eq!(state.selected_text(), Some("find the right session"));
    assert_eq!(
        state.take_selected_text().as_deref(),
        Some("find the right session")
    );
    assert_eq!(state.query(), "");
    state.insert_text("mission control");
    assert_eq!(state.query(), "mission control");
    assert_eq!(state.caret(), "mission control".chars().count());
}

#[test]
fn seeker_editing_gui_uses_clipboard_shaped_hits_and_selection_paint() {
    let main = include_str!("../gui/main.rs");
    assert!(main.contains("ClipboardItem::new_string"));
    assert!(main.contains("cx.read_from_clipboard()"));
    assert!(main.contains("fn seeker_index_for_position"));
    assert!(main.contains("closest_index_for_position"));
    assert!(main.contains("find::editing_chord("));
    assert!(main.contains("state.select_word_to(delta)"));
    let palette = include_str!("../gui/command_palette.rs");
    assert!(palette.contains("fn selection_highlight"));
    assert!(palette.contains("position_for_index"));
    assert!(palette.contains("finder_mouse_down"));
    assert!(palette.contains("finder_mouse_move"));
}
