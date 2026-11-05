//! The split-pane chord and layout rule (#2438).

use crate::ui_core::split::{
    EVEN_SHARE, Layout, MIN_PANE_PX, PANE_BODY_FLOOR, PANE_CHROME_ROWS, PANE_FOOTER_ROWS,
    PANE_INPUT_ROWS, PANE_STRIP_MAX, PANE_STRIP_MIN, chord_layout, pane_rows, pane_rows_with,
    pane_strip_height, share_after_drag,
};

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

#[test]
fn a_short_pane_gives_the_strip_up_first() {
    // Eight rows is a stacked pane on a 30-row terminal: after the border
    // there is room for the input (its real box is three rows) and the
    // transcript floor and nothing else, so the strip drops to zero rather
    // than crushing the answer (#2494).
    let rows = pane_rows(8, 3);
    assert_eq!(rows.strip, 0);
    assert_eq!(rows.input, PANE_INPUT_ROWS);
    assert_eq!(rows.body, PANE_BODY_FLOOR);
    assert_eq!(rows.inner(), 8 - PANE_CHROME_ROWS);
}

#[test]
fn a_normal_pane_gives_its_strip_three_rows() {
    // Twenty-four rows is a stacked pane on a 40-row terminal, the case that
    // used to hand the strip a twelve-row card and leave five rows to read.
    assert_eq!(pane_strip_height(24, 3), PANE_STRIP_MIN);
    let rows = pane_rows(24, 3);
    assert_eq!(rows.strip, 3);
    assert_eq!(rows.footer, PANE_FOOTER_ROWS);
    assert_eq!(
        rows.body,
        24 - PANE_CHROME_ROWS - PANE_INPUT_ROWS - 3 - PANE_FOOTER_ROWS
    );
}

#[test]
fn a_strip_never_grows_past_four_rows() {
    // A ten-task plan asks for twelve rows; the pane caps it at four so the
    // pane stays a pane. The full checklist lives in the overlay.
    assert_eq!(pane_strip_height(60, 12), PANE_STRIP_MAX);
    assert_eq!(pane_strip_height(60, 4), PANE_STRIP_MAX);
}

#[test]
fn every_pane_keeps_its_own_budget() {
    // The reported bug (#2494): one card, drawn from one session, sized
    // against the window, so panes with no plan lost the same rows and the
    // focused pane's card crossed the divider. Two panes of the same height
    // must budget independently: one keeps its strip, the other keeps its
    // full height, and neither call sees the other.
    let with_plan = pane_rows(24, 3);
    let without_plan = pane_rows(24, 0);
    assert_eq!(with_plan.strip, PANE_STRIP_MIN);
    assert_eq!(without_plan.strip, 0);
    assert_eq!(without_plan.body, with_plan.body + PANE_STRIP_MIN);
    // Recomputed in the other order the numbers do not move, because focus and
    // the neighbour are not inputs to the math.
    assert_eq!(pane_rows(24, 3), with_plan);
    assert_eq!(pane_rows(24, 0), without_plan);
}

#[test]
fn the_rows_add_up_to_the_inner_height() {
    // Every height from 1 to 80: the three slots always account for exactly
    // the rows inside the border, and the transcript floor is honoured
    // whenever a strip was handed out at all.
    for height in 1u16..=80 {
        let rows = pane_rows(height, 4);
        assert_eq!(rows.inner(), height.saturating_sub(PANE_CHROME_ROWS));
        if rows.strip > 0 {
            assert!(rows.body >= PANE_BODY_FLOOR);
            assert!((PANE_STRIP_MIN..=PANE_STRIP_MAX).contains(&rows.strip));
        }
    }
}

#[test]
fn pane_footer_costs_one_row_in_a_normal_pane() {
    // A pane with room to spare spends its last row on the footer, and that
    // row comes out of the transcript rather than the composer, which stays at
    // its full PANE_INPUT_ROWS (#2575).
    let rows = pane_rows_with(24, 3, 0);
    assert_eq!(rows.footer, PANE_FOOTER_ROWS);
    assert_eq!(rows.input, PANE_INPUT_ROWS);
    assert_eq!(
        rows.body,
        24 - PANE_CHROME_ROWS - PANE_INPUT_ROWS - rows.strip - PANE_FOOTER_ROWS
    );
}

#[test]
fn pane_footer_yields_before_the_transcript_floor() {
    // Too short to carry a footer: the label gives way, because a pane too
    // short to read is worse than a pane without one.
    let too_short = PANE_CHROME_ROWS + PANE_INPUT_ROWS + PANE_BODY_FLOOR;
    let rows = pane_rows_with(too_short, 3, 0);
    assert_eq!(rows.footer, 0);
    assert_eq!(rows.body, PANE_BODY_FLOOR);

    // One row more and the footer appears again.
    let fits = rows;
    let taller = pane_rows_with(too_short + PANE_FOOTER_ROWS, 0, 0);
    assert_eq!(taller.footer, PANE_FOOTER_ROWS);
    assert_eq!(fits.inner(), too_short - PANE_CHROME_ROWS);
}
