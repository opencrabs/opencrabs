//! Tests for `tui::render::plan_window` — the visible-window math
//! that backs the plan checklist widget.
//!
//! Two surfaces under test:
//!   * `current_task_index` — pick the right anchor task.
//!   * `pick_visible_window` — slice the task list given the row
//!     budget and the anchor, reserving one row each for above /
//!     below "more" indicators only when they're actually needed.
//!
//! Regression context: the panel previously hard-capped at 8 lines
//! total (6 visible task rows). A 7-task plan rendered as 5 visible
//! + `... (2 more)`, which the user noticed on 2026-05-30 02:21.

use crate::tui::plan::{PlanTask, TaskStatus, TaskType};
use crate::tui::render::plan_window::{VisibleWindow, current_task_index, pick_visible_window};
use uuid::Uuid;

fn task(order: usize, status: TaskStatus) -> PlanTask {
    PlanTask {
        id: Uuid::new_v4(),
        order,
        title: format!("Task #{order}"),
        description: String::new(),
        task_type: TaskType::Edit,
        dependencies: Vec::new(),
        complexity: 1,
        acceptance_criteria: Vec::new(),
        status,
        notes: None,
        retry_count: 0,
        verification: None,
        spec_check: None,
        scope: None,
    }
}

// ── current_task_index ──────────────────────────────────────────

#[test]
fn anchor_is_first_in_progress_task() {
    let tasks = vec![
        task(1, TaskStatus::Completed),
        task(2, TaskStatus::Completed),
        task(3, TaskStatus::InProgress),
        task(4, TaskStatus::Pending),
    ];
    assert_eq!(current_task_index(&tasks), 2);
}

#[test]
fn anchor_falls_back_to_first_pending() {
    let tasks = vec![
        task(1, TaskStatus::Completed),
        task(2, TaskStatus::Completed),
        task(3, TaskStatus::Pending),
        task(4, TaskStatus::Pending),
    ];
    assert_eq!(current_task_index(&tasks), 2);
}

#[test]
fn anchor_falls_back_to_blocked_when_no_pending() {
    let tasks = vec![
        task(1, TaskStatus::Completed),
        task(2, TaskStatus::Blocked("waiting on review".to_string())),
        task(3, TaskStatus::Skipped),
    ];
    assert_eq!(current_task_index(&tasks), 1);
}

#[test]
fn anchor_falls_back_to_failed_when_no_pending_or_blocked() {
    let tasks = vec![
        task(1, TaskStatus::Completed),
        task(2, TaskStatus::Failed),
        task(3, TaskStatus::Skipped),
    ];
    assert_eq!(current_task_index(&tasks), 1);
}

#[test]
fn anchor_falls_back_to_last_when_everything_done() {
    let tasks = vec![
        task(1, TaskStatus::Completed),
        task(2, TaskStatus::Skipped),
        task(3, TaskStatus::Completed),
    ];
    // All terminal — user should see the tail of the run, not the head.
    assert_eq!(current_task_index(&tasks), 2);
}

#[test]
fn anchor_index_on_empty_list_is_zero() {
    let tasks: Vec<PlanTask> = Vec::new();
    assert_eq!(current_task_index(&tasks), 0);
}

#[test]
fn in_progress_wins_even_when_a_later_task_is_pending() {
    let tasks = vec![
        task(1, TaskStatus::InProgress),
        task(2, TaskStatus::Pending),
    ];
    assert_eq!(current_task_index(&tasks), 0);
}

// ── pick_visible_window ─────────────────────────────────────────

#[test]
fn empty_list_returns_zero_window() {
    let w = pick_visible_window(0, 10, 0);
    assert_eq!(w, VisibleWindow { start: 0, len: 0 });
}

#[test]
fn zero_available_rows_returns_zero_window() {
    let w = pick_visible_window(5, 0, 2);
    assert_eq!(w, VisibleWindow { start: 0, len: 0 });
}

#[test]
fn list_fitting_in_budget_renders_in_full() {
    // 7 tasks, 10 rows available — everything fits, no indicators.
    let w = pick_visible_window(7, 10, 3);
    assert_eq!(w, VisibleWindow { start: 0, len: 7 });
}

#[test]
fn list_exactly_filling_budget_renders_in_full() {
    let w = pick_visible_window(10, 10, 5);
    assert_eq!(w, VisibleWindow { start: 0, len: 10 });
}

#[test]
fn overflowing_anchor_at_head_skips_above_indicator() {
    // 15 tasks, 10 row budget, anchor at index 1 (head). No row needed
    // for "above" indicator since start == 0; we get 9 visible
    // (10 budget − 1 reserved for "below").
    let w = pick_visible_window(15, 10, 1);
    assert_eq!(w.start, 0);
    assert_eq!(w.len, 9);
}

#[test]
fn overflowing_anchor_at_tail_skips_below_indicator() {
    // 15 tasks, 10 row budget, anchor at the very last task. No row
    // needed for "below" since start + len == total; we get 9 visible
    // (10 budget − 1 reserved for "above").
    let w = pick_visible_window(15, 10, 14);
    assert_eq!(w.start + w.len, 15);
    assert_eq!(w.len, 9);
}

#[test]
fn overflowing_anchor_in_middle_reserves_both_indicators() {
    // 15 tasks, 10 row budget, anchor at index 7. Both indicators
    // needed → 8 task rows visible (10 − 2), centered on anchor.
    let w = pick_visible_window(15, 10, 7);
    assert_eq!(w.len, 8);
    // Anchor must be inside the window.
    assert!(w.start <= 7);
    assert!(w.start + w.len > 7);
    // Centered: 4 above + anchor + 3 below (or 3+4 — either is fine
    // as long as anchor is centered-ish).
    assert!(w.start >= 3 && w.start <= 4);
}

#[test]
fn overflowing_anchor_keeps_visible_when_near_head() {
    // Anchor at index 2, 15 tasks, 10 budget. Window centers on
    // anchor but must clamp to start >= 0. Since start == 0, the
    // "above" indicator row is reclaimed → 9 visible.
    let w = pick_visible_window(15, 10, 2);
    assert_eq!(w.start, 0);
    assert_eq!(w.len, 9);
    assert!(w.start <= 2 && 2 < w.start + w.len);
}

#[test]
fn overflowing_anchor_keeps_visible_when_near_tail() {
    // Anchor at index 13, 15 tasks, 10 budget.
    let w = pick_visible_window(15, 10, 13);
    assert!(w.start <= 13 && 13 < w.start + w.len);
    assert_eq!(w.start + w.len, 15);
}

#[test]
fn tiny_budget_of_three_rows_still_renders_one_task() {
    // Pathological: 10 tasks, 3 row budget. Both indicators worst
    // case → 1 task slot remaining. Must still show the anchor.
    let w = pick_visible_window(10, 3, 5);
    assert!(w.len >= 1);
    assert!(w.start <= 5 && 5 < w.start + w.len);
}

#[test]
fn budget_of_one_row_anchor_in_middle() {
    // 1 row budget, no space for indicators. Must show exactly 1
    // task — the anchor — and skip the indicators.
    let w = pick_visible_window(10, 1, 5);
    assert_eq!(w.len, 1);
    assert_eq!(w.start, 5);
}

#[test]
fn overflowing_at_minimum_overflow_renders_correctly() {
    // 11 tasks, 10 row budget — one task overflows. Anchor at head.
    let w = pick_visible_window(11, 10, 0);
    assert_eq!(w.start, 0);
    // With both indicators it would be 8, but start==0 reclaims the
    // "above" row → 9 visible, plus the "below" indicator covers
    // task 10 (index 10).
    assert_eq!(w.len, 9);
}

#[test]
fn anchor_past_end_does_not_panic() {
    // Defensive: even if current_task_index were to return an
    // out-of-range value, pick_visible_window must still produce
    // a sane window.
    let w = pick_visible_window(5, 10, 99);
    assert!(w.len <= 5);
    assert!(w.start + w.len <= 5);
}

// ── End-to-end: the screenshot's 7-task plan now fits without
// truncation under the new 12-row cap (10 task rows + chrome). ──

#[test]
fn release_planning_screenshot_no_longer_truncates() {
    // The exact case from the user's screenshot: 7 tasks. The
    // parent layout now allocates up to 12 rows (10 + chrome) so
    // the widget gets 10 row budget. All 7 tasks must render.
    let w = pick_visible_window(7, 10, 5);
    assert_eq!(w, VisibleWindow { start: 0, len: 7 });
}
