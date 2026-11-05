//! The drag-to-copy rule for transcript rows (#2383).

use crate::ui_core::copy_select::{TextPoint, copied_range, copied_text, covered_rows};

fn rows() -> Vec<String> {
    ["first", "second", "third"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn a_drag_covers_the_rows_between_its_ends_in_either_direction() {
    assert_eq!(covered_rows(0, 2), Some((0, 2)));
    assert_eq!(covered_rows(2, 0), Some((0, 2)));
}

#[test]
fn a_click_that_never_moved_covers_nothing() {
    assert_eq!(covered_rows(1, 1), None);
}

#[test]
fn copy_joins_the_covered_rows_with_blank_lines() {
    assert_eq!(
        copied_text(&rows(), 0, 1).as_deref(),
        Some("first\n\nsecond")
    );
}

#[test]
fn copy_outside_the_rows_takes_nothing() {
    assert_eq!(copied_text(&rows(), 5, 7), None);
    assert_eq!(copied_text(&rows(), 2, 1), None);
}

#[test]
fn drag_copy_within_one_row_takes_only_the_selected_bytes() {
    let rows = vec!["copy only this range".to_string()];
    assert_eq!(
        copied_range(&rows, TextPoint::new(0, 5), TextPoint::new(0, 13)).as_deref(),
        Some("only thi")
    );
    assert_eq!(
        copied_range(&rows, TextPoint::new(0, 13), TextPoint::new(0, 5)).as_deref(),
        Some("only thi"),
        "a backwards drag selects the same text"
    );
}

#[test]
fn drag_copy_across_rows_keeps_blank_lines_and_partial_endpoints() {
    assert_eq!(
        copied_range(&rows(), TextPoint::new(0, 2), TextPoint::new(2, 4)).as_deref(),
        Some("rst\n\nsecond\n\nthir")
    );
}

#[test]
fn drag_copy_clamps_utf8_offsets_and_ignores_a_click() {
    let rows = vec!["a🦀b".to_string()];
    assert_eq!(
        copied_range(&rows, TextPoint::new(0, 1), TextPoint::new(0, 99)).as_deref(),
        Some("🦀b")
    );
    assert_eq!(
        copied_range(&rows, TextPoint::new(0, 2), TextPoint::new(0, 2)),
        None
    );
}
