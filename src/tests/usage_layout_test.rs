//! Geometry tests for the full-screen usage dashboard (#2073).
//!
//! The old screen was a centered dialog at 75% of the terminal, so on a small
//! terminal the cards were drawn narrower than their own text and the clipping
//! read as a bug. These tests pin the replacement contract: the grid reflows
//! with the width, every card stays inside the area, and below the floor the
//! layout says so instead of drawing a squeezed grid.

use crate::tui::render::responsive::{GRID_THREE_COL_MIN, GRID_TWO_COL_MIN, grid_cols};
use crate::usage::layout::{
    CARD_COUNT, MIN_WIDTH, UsageLayout, compute, fits, min_height, required_height,
};
use ratatui::layout::Rect;

/// Two rects overlap when they share at least one cell.
fn overlaps(a: Rect, b: Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

/// Every rect must be non-empty and inside `area`.
fn assert_contained(name: &str, r: Rect, area: Rect) {
    assert!(r.width > 0 && r.height > 0, "{name} is empty: {r:?}");
    assert!(
        r.x >= area.x
            && r.y >= area.y
            && r.x + r.width <= area.x + area.width
            && r.y + r.height <= area.y + area.height,
        "{name} {r:?} escapes the area {area:?}"
    );
}

/// The grid's own invariants, checked at one size.
fn assert_grid_well_formed(layout: &UsageLayout, area: Rect) {
    assert!(!layout.too_small, "expected a grid at {area:?}");
    assert_contained("summary", layout.summary, area);
    assert_contained("footer", layout.footer, area);

    // Summary pinned above the grid, footer pinned below it.
    assert!(layout.summary.y + layout.summary.height <= layout.cards[0].y);
    assert!(
        layout.footer.y >= layout.cards[CARD_COUNT - 1].y + layout.cards[CARD_COUNT - 1].height
    );

    for i in 0..CARD_COUNT {
        assert_contained(&format!("card {i}"), layout.cards[i], area);
    }
    for i in 0..CARD_COUNT {
        for j in (i + 1)..CARD_COUNT {
            assert!(
                !overlaps(layout.cards[i], layout.cards[j]),
                "card {i} {:?} overlaps card {j} {:?} at {area:?}",
                layout.cards[i],
                layout.cards[j]
            );
        }
    }
}

// ── Breakpoints ─────────────────────────────────────────────────────────────

#[test]
fn grid_cols_breaks_at_the_shared_thresholds() {
    assert_eq!(grid_cols(0), 1);
    assert_eq!(grid_cols(GRID_TWO_COL_MIN - 1), 1);
    assert_eq!(grid_cols(GRID_TWO_COL_MIN), 2);
    assert_eq!(grid_cols(GRID_THREE_COL_MIN - 1), 2);
    assert_eq!(grid_cols(GRID_THREE_COL_MIN), 3);
    assert_eq!(grid_cols(u16::MAX), 3);
}

#[test]
fn required_height_counts_the_rows_the_columns_need() {
    // Six cards: 2 rows of 3, 3 rows of 2, 6 rows of 1. Each row is
    // MIN_CARD_ROWS tall, plus the summary bar and the footer.
    assert_eq!(required_height(3), 9);
    assert_eq!(required_height(2), 12);
    assert_eq!(required_height(1), 21);
    // Guard against a divide by zero when a caller passes nonsense.
    assert_eq!(required_height(0), 21);
}

#[test]
fn min_height_grows_as_the_terminal_narrows() {
    assert_eq!(min_height(GRID_THREE_COL_MIN), 9);
    assert_eq!(min_height(GRID_TWO_COL_MIN), 12);
    assert_eq!(min_height(MIN_WIDTH), 21);
    assert!(min_height(MIN_WIDTH) > min_height(GRID_THREE_COL_MIN));
}

#[test]
fn fits_agrees_with_min_height_at_every_width() {
    for width in 0..=200u16 {
        let needed = min_height(width);
        if width < MIN_WIDTH {
            assert!(!fits(width, needed), "width {width} is below the floor");
            continue;
        }
        assert!(fits(width, needed), "width {width} should fit at {needed}");
        if needed > 0 {
            assert!(
                !fits(width, needed - 1),
                "width {width} should not fit one row short"
            );
        }
    }
}

// ── The floor ───────────────────────────────────────────────────────────────

#[test]
fn below_the_floor_every_rect_is_empty_and_flagged() {
    // 39 columns: one column short of the floor.
    // 20 rows at width 40: one row short of the six-row grid.
    let cases = [
        Rect::new(0, 0, MIN_WIDTH - 1, 40),
        Rect::new(0, 0, MIN_WIDTH, min_height(MIN_WIDTH) - 1),
        Rect::new(0, 0, 20, 10),
        Rect::new(0, 0, 0, 0),
    ];
    for area in cases {
        let layout = compute(area);
        assert!(layout.too_small, "expected too_small at {area:?}");
        assert_eq!(layout.summary, Rect::new(area.x, area.y, 0, 0));
        assert_eq!(layout.footer, Rect::new(area.x, area.y, 0, 0));
        for (i, card) in layout.cards.iter().enumerate() {
            assert_eq!(
                *card,
                Rect::new(area.x, area.y, 0, 0),
                "card {i} at {area:?}"
            );
        }
    }
}

#[test]
fn exactly_at_the_floor_draws_the_grid() {
    let area = Rect::new(0, 0, MIN_WIDTH, min_height(MIN_WIDTH));
    let layout = compute(area);
    assert!(!layout.too_small);
    assert_grid_well_formed(&layout, area);
    // One column at this width, so six stacked rows.
    assert_eq!(grid_cols(area.width), 1);
    for i in 1..CARD_COUNT {
        assert_eq!(
            layout.cards[i].x, layout.cards[0].x,
            "card {i} is not stacked"
        );
    }
}

// ── The grid ────────────────────────────────────────────────────────────────

#[test]
fn wide_terminal_lays_the_cards_out_three_across() {
    let area = Rect::new(0, 0, 120, 40);
    let layout = compute(area);
    assert_grid_well_formed(&layout, area);
    assert_eq!(grid_cols(area.width), 3);
    // Two rows of three: cards 0..3 share a row, 3..6 share the next.
    assert_eq!(layout.cards[0].y, layout.cards[1].y);
    assert_eq!(layout.cards[1].y, layout.cards[2].y);
    assert_eq!(layout.cards[3].y, layout.cards[4].y);
    assert!(layout.cards[3].y > layout.cards[0].y);
    assert!(layout.cards[0].x < layout.cards[1].x);
    assert!(layout.cards[1].x < layout.cards[2].x);
}

#[test]
fn medium_terminal_lays_the_cards_out_two_across() {
    let area = Rect::new(0, 0, 70, 40);
    let layout = compute(area);
    assert_grid_well_formed(&layout, area);
    assert_eq!(grid_cols(area.width), 2);
    assert_eq!(layout.cards[0].y, layout.cards[1].y);
    assert_eq!(layout.cards[2].y, layout.cards[3].y);
    assert!(layout.cards[2].y > layout.cards[0].y);
    assert!(layout.cards[0].x < layout.cards[1].x);
}

#[test]
fn narrow_terminal_stacks_the_cards_in_one_column() {
    let area = Rect::new(0, 0, 45, 40);
    let layout = compute(area);
    assert_grid_well_formed(&layout, area);
    assert_eq!(grid_cols(area.width), 1);
    for i in 0..CARD_COUNT {
        assert_eq!(layout.cards[i].x, layout.cards[0].x);
    }
    for i in 1..CARD_COUNT {
        assert!(
            layout.cards[i].y > layout.cards[i - 1].y,
            "card {i} does not follow {i}-1"
        );
    }
}

/// The regression the issue is about: the old centered dialog drew the cards
/// into a fraction of the terminal, so a small terminal produced clipped
/// cards. Every size that fits must now yield a well formed grid, and every
/// size that does not must say so.
#[test]
fn every_size_that_fits_yields_a_well_formed_grid() {
    for width in 20..=160u16 {
        for height in 5..=60u16 {
            let area = Rect::new(0, 0, width, height);
            let layout = compute(area);
            if layout.too_small {
                assert!(!fits(width, height), "{area:?} flagged small but fits");
                continue;
            }
            assert!(fits(width, height), "{area:?} drew a grid but does not fit");
            assert_grid_well_formed(&layout, area);
        }
    }
}

#[test]
fn compute_offsets_the_grid_when_the_area_is_not_at_the_origin() {
    // A full-screen surface is always at the origin, but `compute` takes any
    // rect and the callers trust it to stay inside.
    let area = Rect::new(3, 5, 120, 40);
    let layout = compute(area);
    assert_grid_well_formed(&layout, area);
    assert_eq!(layout.summary.x, area.x);
    assert_eq!(layout.summary.y, area.y);
    assert_eq!(layout.summary.width, area.width);
}

/// The layout reads the shared breakpoints, so a screen and the mission
/// control analytics grid agree on where the columns change.
#[test]
fn the_grid_reflows_where_the_shared_breakpoints_say() {
    for (width, expected) in [
        (MIN_WIDTH, 1),
        (GRID_TWO_COL_MIN - 1, 1),
        (GRID_TWO_COL_MIN, 2),
        (GRID_THREE_COL_MIN - 1, 2),
        (GRID_THREE_COL_MIN, 3),
    ] {
        let area = Rect::new(0, 0, width, min_height(width));
        let layout = compute(area);
        assert!(!layout.too_small, "width {width} should fit");
        assert_eq!(grid_cols(area.width), expected);
        // The first `expected` cards share the top row.
        for i in 1..expected {
            assert_eq!(
                layout.cards[i].y, layout.cards[0].y,
                "width {width}: card {i} should share the top row"
            );
        }
        if expected < CARD_COUNT {
            assert!(
                layout.cards[expected].y > layout.cards[0].y,
                "width {width}: card {expected} should wrap to a second row"
            );
        }
    }
}

// ── The sizes the issue names ───────────────────────────────────────────────

/// The three sizes #2073 names, pinned as their own cases so the contract is
/// legible in one place instead of only inside the width/height sweep above.
///
/// 40x20 is a deviation worth stating: at 40 columns the six cards stack into
/// six rows, which needs 21 rows, so 40x20 sits one row below the floor and
/// the screen draws the "too small" line rather than a squeezed grid. The
/// other two draw a real grid.
#[test]
fn the_sizes_the_issue_names_behave_as_specified() {
    let small = Rect::new(0, 0, 40, 20);
    let layout = compute(small);
    assert!(layout.too_small, "40x20 is one row below the floor");
    assert!(!fits(small.width, small.height));
    assert_eq!(min_height(small.width), 21);
    assert_eq!(layout.summary, Rect::new(0, 0, 0, 0));
    assert_eq!(layout.footer, Rect::new(0, 0, 0, 0));
    for card in layout.cards {
        assert_eq!(card, Rect::new(0, 0, 0, 0));
    }

    for area in [Rect::new(0, 0, 80, 24), Rect::new(0, 0, 160, 50)] {
        let layout = compute(area);
        assert_grid_well_formed(&layout, area);
    }
    assert_eq!(grid_cols(80), 2);
    assert_eq!(grid_cols(160), 3);
}
