//! Tests for `compute_focused`, the pure layout function for Mission
//! Control.
//!
//! Contract: the inbox / activity / schedule / help_bar rects must:
//!   1. Stay strictly inside the outer area.
//!   2. Not overlap each other.
//!   3. Reserve exactly 2 rows for the help bar when the area can spare it.
//!   4. Collapse the help bar to 0-height when the area is too short to
//!      spare those 2 rows.
//!
//! Plus the responsive contract (#2074): wide keeps the 40/60 split, medium
//! shifts to 45/55, narrow shows one panel filling the area, and below the
//! floor the layout says so instead of drawing slivers.

use crate::tui::app::mission_control::McPanel;
use crate::tui::render::mission_control::{McLayout, compute_focused};
use crate::tui::render::responsive::{MC_SPLIT_MIN, MC_WIDE_MIN, mc_two_column};
use ratatui::layout::Rect;

fn rect_inside(child: Rect, parent: Rect) -> bool {
    child.x >= parent.x
        && child.y >= parent.y
        && child.x + child.width <= parent.x + parent.width
        && child.y + child.height <= parent.y + parent.height
}

fn rects_overlap(a: Rect, b: Rect) -> bool {
    let a_right = a.x + a.width;
    let b_right = b.x + b.width;
    let a_bottom = a.y + a.height;
    let b_bottom = b.y + b.height;
    !(a_right <= b.x || b_right <= a.x || a_bottom <= b.y || b_bottom <= a.y)
}

fn rect_empty(r: Rect) -> bool {
    r.width == 0 || r.height == 0
}

/// Every live panel rect is inside the area, no two overlap, and none
/// overlaps the help bar. Zero-sized rects mean "not on screen" and are
/// skipped.
fn assert_well_formed(layout: &McLayout, outer: Rect) {
    assert!(!layout.too_small, "expected panels at {outer:?}");
    let live: Vec<(McPanel, Rect)> = layout
        .panels_in_order()
        .into_iter()
        .filter(|(_, r)| !rect_empty(*r))
        .collect();
    assert!(!live.is_empty(), "no panel is on screen at {outer:?}");
    for (panel, r) in &live {
        assert!(rect_inside(*r, outer), "{panel:?} {r:?} escapes {outer:?}");
        assert!(
            !rects_overlap(*r, layout.help_bar),
            "{panel:?} overlaps the help bar at {outer:?}"
        );
    }
    for i in 0..live.len() {
        for j in (i + 1)..live.len() {
            assert!(
                !rects_overlap(live[i].1, live[j].1),
                "{:?} overlaps {:?} at {outer:?}",
                live[i].0,
                live[j].0
            );
        }
    }
    if layout.help_bar.height > 0 {
        assert!(rect_inside(layout.help_bar, outer));
    }
}

#[test]
fn every_panel_stays_inside_outer_area() {
    let outer = Rect::new(0, 0, 154, 50);
    let layout: McLayout = compute_focused(outer, McPanel::Inbox);
    assert!(rect_inside(layout.inbox, outer), "inbox escaped outer");
    assert!(
        rect_inside(layout.analytics, outer),
        "analytics escaped outer"
    );
    assert!(
        rect_inside(layout.activity, outer),
        "activity escaped outer"
    );
    assert!(
        rect_inside(layout.schedule, outer),
        "schedule escaped outer"
    );
    assert!(
        rect_inside(layout.help_bar, outer),
        "help_bar escaped outer"
    );
}

#[test]
fn panels_do_not_overlap() {
    let outer = Rect::new(0, 0, 154, 50);
    let layout = compute_focused(outer, McPanel::Inbox);
    assert!(
        !rects_overlap(layout.inbox, layout.activity),
        "inbox/activity overlap"
    );
    assert!(
        !rects_overlap(layout.inbox, layout.analytics),
        "inbox/analytics overlap"
    );
    assert!(
        !rects_overlap(layout.analytics, layout.activity),
        "analytics/activity overlap"
    );
    assert!(
        !rects_overlap(layout.analytics, layout.schedule),
        "analytics/schedule overlap"
    );
    assert!(
        !rects_overlap(layout.analytics, layout.help_bar),
        "analytics overlaps help bar"
    );
    assert!(
        !rects_overlap(layout.inbox, layout.schedule),
        "inbox/schedule overlap"
    );
    assert!(
        !rects_overlap(layout.activity, layout.schedule),
        "activity/schedule overlap"
    );
    // Panels must not overlap the help bar either.
    assert!(
        !rects_overlap(layout.inbox, layout.help_bar),
        "inbox overlaps help bar"
    );
    assert!(
        !rects_overlap(layout.activity, layout.help_bar),
        "activity overlaps help bar"
    );
    assert!(
        !rects_overlap(layout.schedule, layout.help_bar),
        "schedule overlaps help bar"
    );
}

#[test]
fn help_bar_takes_exactly_two_rows_when_area_is_tall_enough() {
    let outer = Rect::new(0, 0, 100, 30);
    let layout = compute_focused(outer, McPanel::Inbox);
    assert_eq!(layout.help_bar.height, 2);
    // Help bar sits at the very bottom two rows.
    assert_eq!(layout.help_bar.y, outer.y + outer.height - 2);
    assert_eq!(layout.help_bar.x, outer.x);
    assert_eq!(layout.help_bar.width, outer.width);
}

#[test]
fn help_bar_collapses_to_zero_when_area_is_too_short() {
    // A 2-row area can't spare the 2-row bar — panels take the whole height.
    let outer = Rect::new(0, 0, 100, 2);
    let layout = compute_focused(outer, McPanel::Inbox);
    assert_eq!(layout.help_bar.height, 0);
}

#[test]
fn inbox_takes_left_40_percent_when_wide() {
    let outer = Rect::new(0, 0, 154, 30);
    let layout = compute_focused(outer, McPanel::Inbox);
    assert_eq!(layout.inbox.x, 0);
    // Allow 1-cell rounding tolerance from ratatui's percentage split.
    assert!(
        (layout.inbox.width as i32 - 62).abs() <= 1,
        "inbox width was {}, expected ~62 (40% of 154)",
        layout.inbox.width
    );
}

#[test]
fn analytics_takes_full_right_column_left_stacks_three() {
    let outer = Rect::new(0, 0, 100, 30);
    let layout = compute_focused(outer, McPanel::Inbox);
    let panel_height = outer.height - layout.help_bar.height;
    // Analytics spans the full panel height on the right.
    assert_eq!(
        layout.analytics.height, panel_height,
        "analytics should be full height"
    );
    // Inbox + Activity + Schedule stack to fill the left column.
    let left_height = layout.inbox.height + layout.activity.height + layout.schedule.height;
    assert_eq!(
        left_height, panel_height,
        "left column should fill the panel height"
    );
    // Analytics sits to the right of the left column.
    assert!(
        layout.analytics.x >= layout.inbox.x + layout.inbox.width,
        "analytics should be right of the left column"
    );
}

#[test]
fn handles_zero_area_without_panic() {
    // Pathological case: terminal mid-resize. The layout must not panic.
    let outer = Rect::new(0, 0, 0, 0);
    // The assertion is simply that this call returns without panicking.
    let _ = compute_focused(outer, McPanel::Inbox);
}

// ── Responsive geometry (#2074) ─────────────────────────────────────────────

#[test]
fn wide_terminal_keeps_the_40_60_split() {
    let outer = Rect::new(0, 0, 200, 50);
    let layout = compute_focused(outer, McPanel::Inbox);
    assert_well_formed(&layout, outer);
    assert!(mc_two_column(outer.width));
    assert!(outer.width >= MC_WIDE_MIN);
    // 40% of 200, 1-cell rounding tolerance.
    assert!(
        (layout.inbox.width as i32 - 80).abs() <= 1,
        "inbox width was {}, expected ~80",
        layout.inbox.width
    );
    for (panel, r) in layout.panels_in_order() {
        assert!(!rect_empty(r), "{panel:?} should be visible at 200 wide");
    }
    assert!(layout.analytics.x > layout.inbox.x);
    assert_eq!(layout.inbox.y, outer.y);
}

#[test]
fn medium_terminal_shifts_to_a_45_55_split() {
    let outer = Rect::new(0, 0, 110, 30);
    let layout = compute_focused(outer, McPanel::Inbox);
    assert_well_formed(&layout, outer);
    assert!(mc_two_column(outer.width));
    assert!(outer.width < MC_WIDE_MIN);
    // 45% of 110 is 49.5, so 49 or 50.
    assert!(
        (layout.inbox.width as i32 - 50).abs() <= 1,
        "inbox width was {}, expected ~50 (45% of 110)",
        layout.inbox.width
    );
    // More room than the 40% split would have given the left column.
    assert!(layout.inbox.width > outer.width * 40 / 100);
    for (panel, r) in layout.panels_in_order() {
        assert!(!rect_empty(r), "{panel:?} should be visible at 110 wide");
    }
}

#[test]
fn narrow_terminal_shows_one_panel_filling_the_area() {
    let outer = Rect::new(0, 0, 80, 24);
    let layout = compute_focused(outer, McPanel::Schedule);
    assert_well_formed(&layout, outer);
    assert!(!mc_two_column(outer.width));
    // Exactly one panel is on screen, and it is the focused one.
    let live: Vec<McPanel> = layout
        .panels_in_order()
        .into_iter()
        .filter(|(_, r)| !rect_empty(*r))
        .map(|(p, _)| p)
        .collect();
    assert_eq!(live, vec![McPanel::Schedule]);
    // It fills the panels area, above the help bar.
    assert_eq!(layout.schedule, layout.panels);
    assert_eq!(layout.schedule.x, outer.x);
    assert_eq!(layout.schedule.width, outer.width);
    assert_eq!(
        layout.schedule.height + layout.help_bar.height,
        outer.height
    );
}

#[test]
fn every_panel_can_be_focused_in_narrow_mode() {
    let outer = Rect::new(0, 0, 80, 24);
    for panel in [
        McPanel::Inbox,
        McPanel::Analytics,
        McPanel::Activity,
        McPanel::Schedule,
    ] {
        let layout = compute_focused(outer, panel);
        assert_well_formed(&layout, outer);
        assert_eq!(layout.rect(panel), layout.panels, "{panel:?} should fill");
        for (other, r) in layout.panels_in_order() {
            if other != panel {
                assert!(rect_empty(r), "{other:?} should be hidden");
            }
        }
    }
}

#[test]
fn the_split_starts_exactly_at_the_shared_threshold() {
    let tall = 40u16;
    let below = compute_focused(Rect::new(0, 0, MC_SPLIT_MIN - 1, tall), McPanel::Inbox);
    assert!(!mc_two_column(MC_SPLIT_MIN - 1));
    assert_eq!(below.inbox.width, MC_SPLIT_MIN - 1, "narrow: full width");
    assert!(rect_empty(below.analytics), "narrow: analytics is hidden");

    let at = compute_focused(Rect::new(0, 0, MC_SPLIT_MIN, tall), McPanel::Inbox);
    assert!(mc_two_column(MC_SPLIT_MIN));
    assert!(
        at.inbox.width < MC_SPLIT_MIN - 1,
        "medium: the split takes the left column down from full width"
    );
    assert!(!rect_empty(at.analytics), "medium: analytics is visible");
}

#[test]
fn below_the_floor_the_layout_says_so() {
    let cases = [
        Rect::new(0, 0, 39, 40), // one column short
        Rect::new(0, 0, 80, 6),  // panels area only 4 rows
        Rect::new(0, 0, 0, 0),   // mid-resize
    ];
    for outer in cases {
        let layout = compute_focused(outer, McPanel::Inbox);
        assert!(layout.too_small, "expected too_small at {outer:?}");
        for (panel, r) in layout.panels_in_order() {
            assert!(rect_empty(r), "{panel:?} should be empty at {outer:?}");
        }
    }
}

/// The regression the issue is about: the panels were pinned to a 40/60
/// split at every size, so at 80 columns the left column got 32 and the
/// schedule rows truncated. Every size that renders must now be well formed,
/// and narrow sizes must give the focused panel the full width.
#[test]
fn every_size_renders_a_well_formed_layout_or_says_too_small() {
    for width in 10..=200u16 {
        for height in 3..=50u16 {
            let outer = Rect::new(0, 0, width, height);
            for panel in [McPanel::Inbox, McPanel::Analytics] {
                let layout = compute_focused(outer, panel);
                if layout.too_small {
                    continue;
                }
                assert_well_formed(&layout, outer);
                let focused = layout.rect(panel);
                if mc_two_column(width) {
                    assert!(
                        focused.width < width,
                        "{panel:?} should share the width at {width} columns"
                    );
                } else {
                    assert_eq!(
                        focused.width, width,
                        "{panel:?} should take the full width at {width} columns"
                    );
                }
            }
        }
    }
}
