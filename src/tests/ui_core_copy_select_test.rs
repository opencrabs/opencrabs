//! The drag-to-copy rule for transcript rows (#2383).

use crate::ui_core::copy_select::{copied_text, covered_rows};

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
