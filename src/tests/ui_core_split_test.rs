//! The split-pane chord and layout rule (#2438).

use crate::ui_core::split::{Layout, chord_layout};

#[test]
fn ctrl_shift_pipe_splits_side_by_side() {
    assert_eq!(
        chord_layout("\\", Some("|"), true, true),
        Some(Layout::Side)
    );
    assert_eq!(chord_layout("|", None, true, true), Some(Layout::Side));
}

#[test]
fn ctrl_shift_underscore_stacks() {
    assert_eq!(
        chord_layout("-", Some("_"), true, true),
        Some(Layout::Stack)
    );
    assert_eq!(chord_layout("_", None, true, true), Some(Layout::Stack));
}

#[test]
fn the_chord_needs_both_control_and_shift() {
    assert_eq!(chord_layout("\\", Some("|"), false, true), None);
    assert_eq!(chord_layout("\\", Some("|"), true, false), None);
    assert_eq!(chord_layout("a", Some("a"), true, true), None);
}

#[test]
fn layouts_name_their_glyph_and_axis() {
    assert_eq!(Layout::Side.label(), "|");
    assert_eq!(Layout::Stack.label(), "_");
    assert!(!Layout::Side.is_stacked());
    assert!(Layout::Stack.is_stacked());
}
