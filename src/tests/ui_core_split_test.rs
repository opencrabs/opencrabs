//! The split-pane chord and layout rule (#2438).

use crate::ui_core::split::{EVEN_SHARE, Layout, MIN_PANE_PX, chord_layout, share_after_drag};

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
fn macos_folding_shift_into_the_glyph_still_splits() {
    // The reported bug (#2471): a keystroke that plainly typed `|` with
    // control arrives with shift already folded into the glyph and reported as
    // false, so requiring both modifiers made the chord unreachable.
    assert_eq!(chord_layout("|", None, true, false), Some(Layout::Side));
    assert_eq!(chord_layout("_", None, true, false), Some(Layout::Stack));
}

#[test]
fn the_base_key_still_needs_shift() {
    // A layout that sends the unshifted key has to say shift was held: ctrl+-
    // is zoom out and a bare ctrl+backslash is nothing.
    assert_eq!(chord_layout("\\", None, true, false), None);
    assert_eq!(chord_layout("-", None, true, false), None);
}

#[test]
fn the_chord_needs_control() {
    assert_eq!(chord_layout("\\", Some("|"), false, true), None);
    assert_eq!(chord_layout("|", None, false, false), None);
    assert_eq!(chord_layout("a", Some("a"), true, true), None);
}

#[test]
fn layouts_name_their_glyph_and_axis() {
    assert_eq!(Layout::Side.label(), "|");
    assert_eq!(Layout::Stack.label(), "_");
    assert!(!Layout::Side.is_stacked());
    assert!(Layout::Stack.is_stacked());
}

#[test]
fn a_drag_moves_the_share_with_the_pointer() {
    // A 200px drag across a 1000px pane area moves the rule a fifth of the
    // way, in either direction, from an even split (#2476).
    assert!((share_after_drag(EVEN_SHARE, 200.0, 1000.0) - 0.7).abs() < 1e-6);
    assert!((share_after_drag(EVEN_SHARE, -200.0, 1000.0) - 0.3).abs() < 1e-6);
    // A drag that goes nowhere leaves the split exactly where it was.
    assert_eq!(share_after_drag(EVEN_SHARE, 0.0, 1000.0), EVEN_SHARE);
}

#[test]
fn a_drag_stops_at_the_narrowest_usable_pane() {
    // 240px of a 1000px area is the floor: 0.24, mirrored on the other side,
    // so neither pane can be dragged away (#2476).
    let floor = MIN_PANE_PX / 1000.0;
    assert!((share_after_drag(EVEN_SHARE, -900.0, 1000.0) - floor).abs() < 1e-6);
    assert!((share_after_drag(EVEN_SHARE, 900.0, 1000.0) - (1.0 - floor)).abs() < 1e-6);
    // Past the floor the share stops moving, it does not keep sliding.
    assert_eq!(
        share_after_drag(EVEN_SHARE, -5000.0, 1000.0),
        share_after_drag(EVEN_SHARE, -900.0, 1000.0)
    );
}

#[test]
fn a_pane_area_too_small_for_two_panes_keeps_the_split() {
    // Two minimum panes do not fit, so there is no share to clamp into:
    // holding the split is the only outcome that does not collapse a side.
    assert_eq!(share_after_drag(0.6, 300.0, MIN_PANE_PX * 2.0), 0.6);
    assert_eq!(share_after_drag(0.6, 300.0, 0.0), 0.6);
    assert_eq!(share_after_drag(0.6, 300.0, f32::NAN), 0.6);
}

#[test]
fn a_new_split_starts_even() {
    assert_eq!(EVEN_SHARE, 0.5);
}
