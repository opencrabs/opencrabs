//! The transcript's row selection (#2613): what a click means in select mode,
//! how a shift-click extends, and the order a forward takes the picks in.
//!
//! The model lives in [`crate::ui_core::message_select`]; these are the cases
//! that decide its shape, kept in the test tree so a plain `cargo test --lib
//! ui_core_message_select` runs all of them.

use crate::ui_core::message_select::{Selection, span};

fn order() -> Vec<&'static str> {
    vec!["m1", "m2", "m3", "m4", "m5"]
}

#[test]
fn select_mode_starts_empty_and_keeps_no_anchor() {
    let mut sel: Selection<&str> = Selection::new();
    assert!(!sel.is_active());
    sel.begin();
    assert!(sel.is_active());
    assert_eq!(sel.count(), 0);
    assert!(sel.is_empty());
}

#[test]
fn toggling_picks_then_unpicks_the_same_row() {
    let mut sel: Selection<&str> = Selection::new();
    sel.begin();
    sel.toggle("m2");
    assert!(sel.contains(&"m2"));
    assert_eq!(sel.count(), 1);
    sel.toggle("m2");
    assert!(!sel.contains(&"m2"));
    assert_eq!(sel.count(), 0);
}

#[test]
fn shift_click_takes_every_row_between_anchor_and_target() {
    let o = order();
    let mut sel: Selection<&str> = Selection::new();
    sel.begin();
    sel.toggle("m2");
    assert!(sel.extend(&o, &"m4"));
    assert_eq!(sel.picked_in_order(&o), vec!["m2", "m3", "m4"]);
}

#[test]
fn extending_backwards_covers_the_same_rows() {
    let o = order();
    let mut a: Selection<&str> = Selection::new();
    a.begin();
    a.toggle("m2");
    a.extend(&o, &"m4");

    let mut b: Selection<&str> = Selection::new();
    b.begin();
    b.toggle("m4");
    b.extend(&o, &"m2");

    assert_eq!(a.picked_in_order(&o), b.picked_in_order(&o));
}

/// A shift-click re-anchors on its target, so a second one grows the same run
/// instead of starting a new one: m1, shift m2, shift m5 is the whole span,
/// the way a dragged range grows.
#[test]
fn a_second_shift_click_extends_the_same_run_from_its_new_anchor() {
    let o = order();
    let mut sel: Selection<&str> = Selection::new();
    sel.begin();
    sel.toggle("m1");
    sel.extend(&o, &"m2");
    sel.extend(&o, &"m5");
    assert_eq!(sel.picked_in_order(&o), vec!["m1", "m2", "m3", "m4", "m5"]);
}

/// Union, not replace: a row picked elsewhere in the transcript survives a
/// range that does not cover it. This is how the non-contiguous selection the
/// copy and forward paths consume gets built.
#[test]
fn extending_a_range_keeps_rows_picked_outside_it() {
    let o = order();
    let mut sel: Selection<&str> = Selection::new();
    sel.begin();
    sel.toggle("m5");
    sel.toggle("m1");
    sel.extend(&o, &"m2");
    assert_eq!(sel.picked_in_order(&o), vec!["m1", "m2", "m5"]);
}

#[test]
fn picked_rows_come_back_in_transcript_order_not_click_order() {
    let o = order();
    let mut sel: Selection<&str> = Selection::new();
    sel.begin();
    sel.toggle("m5");
    sel.toggle("m1");
    sel.toggle("m3");
    assert_eq!(sel.picked_in_order(&o), vec!["m1", "m3", "m5"]);
}

#[test]
fn extend_without_an_anchor_picks_only_the_clicked_row() {
    let o = order();
    let mut sel: Selection<&str> = Selection::new();
    sel.begin();
    assert!(sel.extend(&o, &"m3"));
    assert_eq!(sel.picked_in_order(&o), vec!["m3"]);
}

#[test]
fn an_anchor_that_left_the_transcript_extends_nothing() {
    let o = order();
    let mut sel: Selection<&str> = Selection::new();
    sel.begin();
    sel.toggle("gone");
    assert!(!sel.extend(&o, &"m3"));
    assert_eq!(sel.count(), 1);
}

#[test]
fn cancel_drops_mode_and_every_pick() {
    let mut sel: Selection<&str> = Selection::new();
    sel.begin();
    sel.toggle("m1");
    sel.cancel();
    assert!(!sel.is_active());
    assert!(sel.is_empty());
    assert!(!sel.contains(&"m1"));
}

#[test]
fn span_is_none_when_either_end_is_absent() {
    let o = order();
    assert_eq!(span(&o, &"m1", &"m5"), Some((0, 4)));
    assert_eq!(span(&o, &"m5", &"m1"), Some((0, 4)));
    assert_eq!(span(&o, &"x", &"m1"), None);
}
