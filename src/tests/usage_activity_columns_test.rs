//! Tests for `usage::cards::activity_column_widths`.
//!
//! Pins the 2026-04-25 fix where the "By Activity" usage card clipped
//! the "1-shot" header to "1-sho" because the column was sized only
//! for the data values ("60%" → 4 chars) and the longer header text
//! (6 chars) overflowed `inner.width`. Same near-miss for "Turns" (5)
//! vs "4162" (4).
//!
//! The fix: each column width is `max(header_label, widest_data)` so
//! the line fits regardless of which side is wider.

use crate::ui_core::usage::view_model::{ActivityRow, UsageViewModel};
use crate::usage::cards::{ActivityColumnWidths, activity_column_widths};
use crate::usage::data::{ActivityStats, DashboardData, Period};

/// One fixture row, built through the real view-model pipeline so the
/// width math is exercised on exactly what the renderer will draw.
fn act(category: &str, cost: f64, turns: i64, one_shot_pct: f64) -> ActivityRow {
    UsageViewModel::build(
        &DashboardData {
            activities: vec![ActivityStats {
                category: category.to_string(),
                cost,
                turns,
                one_shot_pct,
            }],
            ..Default::default()
        },
        Period::AllTime,
    )
    .activities
    .remove(0)
}

#[test]
fn empty_input_yields_safe_defaults_no_panic() {
    let w = activity_column_widths(&[]);
    // Defaults must still be at least as wide as the headers so an
    // empty card doesn't render with "1-sho" either.
    assert!(w.cost >= "Cost".len());
    assert!(w.turns >= "Turns".len());
    assert!(w.pct >= "1-shot".len());
}

#[test]
fn pct_column_fits_one_shot_header_when_values_are_short() {
    // The exact 2026-04-25 screenshot case: 60% values, "1-shot"
    // header. Old code would size pct=4 (2 digits + "%" + 1 padding)
    // and clip the 6-char header to "1-sho".
    let w = activity_column_widths(&[
        act("dev", 818.49, 4162, 60.0),
        act("ops", 66.08, 394, 46.0),
        act("misc", 1.60, 21, 60.0),
    ]);
    assert_eq!(
        w.pct,
        "1-shot".len(),
        "pct column must accommodate the 6-char '1-shot' header even when values are 2-3 chars"
    );
}

#[test]
fn pct_column_grows_with_three_digit_values() {
    // 100% one-shot rate is plausible for tools that always succeed.
    // "100%" = 4 chars; still narrower than "1-shot" (6) so the pct
    // column should sit at the header floor.
    let w = activity_column_widths(&[act("dev", 1.0, 1, 100.0)]);
    assert_eq!(w.pct, "1-shot".len());
}

#[test]
fn turns_column_fits_turns_header_when_values_are_short() {
    // Single-digit turns: "Turns" header (5) > value width (1).
    let w = activity_column_widths(&[act("dev", 1.0, 7, 0.0)]);
    assert_eq!(w.turns, "Turns".len());
}

#[test]
fn turns_column_grows_with_value_width() {
    // 7-digit turns count exceeds the header width — column tracks
    // the value, not the header.
    let w = activity_column_widths(&[act("dev", 1.0, 1234567, 0.0)]);
    assert_eq!(w.turns, "1234567".len());
    assert!(w.turns > "Turns".len());
}

#[test]
fn cost_column_grows_with_value_width() {
    // "$818.49" (7) > "Cost" (4) — column tracks the formatted value.
    let w = activity_column_widths(&[act("dev", 818.49, 1, 0.0)]);
    assert!(w.cost >= "$818.49".len());
}

#[test]
fn cost_column_holds_floor_when_values_are_smaller_than_header() {
    // "$0.10" (5) > "Cost" (4); cost still fine. But test the floor
    // explicitly with a tiny value just under the header width.
    // Note: fmt_cost may pad differently for sub-dollar amounts;
    // we assert only the floor invariant here.
    let w = activity_column_widths(&[act("x", 0.001, 1, 0.0)]);
    assert!(
        w.cost >= "Cost".len(),
        "cost column must always fit the 'Cost' header"
    );
}

#[test]
fn cat_column_tracks_widest_category() {
    let w = activity_column_widths(&[
        act("rsi", 1.0, 1, 0.0),
        act("self-improvement", 1.0, 1, 0.0),
        act("dev", 1.0, 1, 0.0),
    ]);
    assert_eq!(w.cat, "self-improvement".len());
}

#[test]
fn full_screenshot_repro_yields_widths_that_fit_in_terminal() {
    // Reproduce the exact data from the 2026-04-25 screenshot and
    // assert that the resulting widths ALL fit their header labels
    // in the same order they're rendered. This is the regression
    // scenario in code form.
    let stats = [
        act("dev", 818.49, 4162, 60.0),
        act("ops", 66.08, 394, 46.0),
        act("misc", 1.60, 21, 60.0),
    ];
    let ActivityColumnWidths {
        cat: _,
        cost,
        turns,
        pct,
    } = activity_column_widths(&stats);
    // Header strings used by render_activities for this card.
    assert!(
        cost >= "Cost".len(),
        "Cost column too narrow: got {} for header len {}",
        cost,
        "Cost".len()
    );
    assert!(
        turns >= "Turns".len(),
        "Turns column too narrow: got {} for header len {}",
        turns,
        "Turns".len()
    );
    assert!(
        pct >= "1-shot".len(),
        "1-shot column too narrow: got {} for header len {}",
        pct,
        "1-shot".len()
    );
}
