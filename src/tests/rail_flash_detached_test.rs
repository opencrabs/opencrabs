//! Rail notification follow-ups: the plan bar flashes when a task settles, and
//! the card and `/sessions` row both name detached commands.
//!
//! The rail has no headless pixel harness here, so the presentation is pinned
//! at two levels: the pure helpers by value, and the paint wiring by reading
//! the exact source blocks that draw it. That split is what keeps a colour
//! decision testable without a GPU and still catches a param that was added to
//! a signature and dropped at the call site.
//!
//! The badge's own wording is pinned in `background_indicator_test.rs`, beside
//! the `format_background_tasks` cases it is the compact sibling of.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};
use crate::utils::completion_sound::{
    FLASH_BLINK_MS, FLASH_WINDOW, flash_blink_on, flash_lit, newly_resolved_in,
};
use uuid::Uuid;

const SESSIONS_RS: &str = include_str!("../tui/render/sessions.rs");
const GUI_SESSIONS_RS: &str = include_str!("../gui/sessions.rs");
const GUI_MAIN_RS: &str = include_str!("../gui/main.rs");

fn block<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = source.split_once(start).expect("source block starts");
    let (body, _) = tail.split_once(end).expect("source block ends");
    body
}

fn task(order: usize, status: TaskStatus) -> PlanTask {
    let mut t = PlanTask::new(
        order,
        format!("task {order}"),
        String::new(),
        TaskType::Other("test".to_string()),
    );
    t.status = status;
    t
}

fn plan_with(statuses: &[TaskStatus]) -> PlanDocument {
    let mut plan = PlanDocument::new(Uuid::new_v4(), "rail flash".to_string());
    plan.status = PlanStatus::Active;
    for (i, status) in statuses.iter().enumerate() {
        plan.add_task(task(i + 1, status.clone()));
    }
    plan
}

#[test]
fn flash_window_closes_on_its_own() {
    let now = Instant::now();
    let end = now + FLASH_WINDOW;
    assert!(
        flash_lit(Some(end), now),
        "the frame that just settled a task must light the bar"
    );
    assert!(
        flash_lit(Some(end), end - Duration::from_millis(1)),
        "the last millisecond of the window is still lit"
    );
    assert!(
        !flash_lit(Some(end), end),
        "the deadline itself ends the window"
    );
    assert!(
        !flash_lit(Some(end), end + Duration::from_secs(1)),
        "a row that finished a while ago keeps its resting colour"
    );
    assert!(
        !flash_lit(None, now),
        "a session that never flashed has no deadline to be inside"
    );
}

#[test]
fn flash_blinks_in_the_accent_half_of_the_window() {
    let start = Instant::now();
    let end = start + FLASH_WINDOW;
    let half = Duration::from_millis(FLASH_BLINK_MS);

    assert!(
        flash_blink_on(Some(end), start),
        "the bar starts lit, so the settle is visible on the very next frame"
    );
    assert!(
        !flash_blink_on(Some(end), start + half),
        "one half later it is dark, which is what makes it a blink"
    );
    assert!(
        flash_blink_on(Some(end), start + half * 2),
        "and lit again on the following half"
    );
    assert!(
        !flash_blink_on(Some(end), end),
        "past the window the paint collapses to the resting teal"
    );
    assert!(
        !flash_blink_on(None, start),
        "no deadline means nothing to blink"
    );
}

#[test]
fn flash_survives_a_clock_that_moved_backwards() {
    // `until` in the past relative to `now` is the ordinary case (the window
    // closed), but a monotonic clock that jumped must dim, never panic.
    let now = Instant::now();
    let backwards = now - FLASH_WINDOW - Duration::from_secs(5);
    assert!(
        !flash_lit(Some(backwards), now),
        "a deadline already behind us is closed"
    );
    assert!(
        !flash_blink_on(Some(backwards), now),
        "saturating phase math keeps the paint path panic-free"
    );
}

#[test]
fn settling_one_task_flavours_that_plan_only() {
    let settled = plan_with(&[TaskStatus::Completed, TaskStatus::Pending]);
    let quiet = plan_with(&[TaskStatus::Pending, TaskStatus::Pending]);
    let mut seen: HashSet<Uuid> = HashSet::new();

    assert_eq!(
        newly_resolved_in(&mut seen, Some(&settled)),
        1,
        "the plan that just settled a task reports it"
    );
    assert_eq!(
        newly_resolved_in(&mut seen, Some(&quiet)),
        0,
        "a plan where nothing moved reports nothing, so its row stays dark"
    );
    assert_eq!(
        newly_resolved_in(&mut seen, Some(&settled)),
        0,
        "a reload of the same document must not re-flash the same task"
    );
    let new_one = plan_with(&[TaskStatus::Skipped]);
    assert_eq!(
        newly_resolved_in(&mut seen, Some(&new_one)),
        1,
        "a skipped task counts as settled, same as completed"
    );
}

#[test]
fn sessions_rows_read_the_detached_badge_from_the_frame_plan() {
    let row_plan = block(SESSIONS_RS, "fn row_plan(", "fn fit_cell(");
    assert!(
        row_plan.contains("background_manager()"),
        "the task roster is read once per frame, not once per row"
    );
    assert!(
        row_plan.contains("row_background_badge("),
        "the status badge comes from the shared formatter"
    );
    assert!(
        row_plan.contains("background,"),
        "the map is carried on the RowPlan so the row painter can use it"
    );

    let status_arm = block(SESSIONS_RS, "Column::Status => {", "spans\n            }");
    assert!(
        status_arm.contains("if let Some(badge) = background"),
        "the status cell renders the badge it was handed"
    );
    assert!(
        !status_arm.contains("background_manager()"),
        "and never reaches into the manager itself while painting"
    );
}

#[test]
fn rail_card_stacks_the_running_line_under_the_plan_bar() {
    let row = block(GUI_SESSIONS_RS, "pub fn row(", "pub fn on_click(");
    let bar = row.find("plan_progress_cell(done, total, flash)");
    let running = row.find(".when_some(running, |d, line| {");
    assert!(
        bar.is_some() && running.is_some(),
        "the card keeps both the plan bar and the detached line"
    );
    assert!(
        bar.expect("plan bar") < running.expect("running line"),
        "detached work is drawn BELOW the bar, which is what the owner asked for"
    );

    let bar_cell = block(
        GUI_SESSIONS_RS,
        "fn plan_progress_cell(",
        "pub fn on_click(",
    );
    assert!(
        bar_cell.contains("if flash {") && bar_cell.contains("palette::current().accent"),
        "a settling task lifts the bar to the accent orange"
    );
    assert!(
        bar_cell.contains("palette::current().teal"),
        "and it falls back to the resting teal when the window closes"
    );
}

#[test]
fn the_rail_supplies_both_facts_per_frame() {
    assert!(
        GUI_MAIN_RS.contains("plan_flash_until"),
        "each session's flash deadline is stored, so the blink survives a repaint"
    );
    let map = block(GUI_MAIN_RS, "fn rail_running_map(", "\n    fn ");
    assert!(
        map.contains("format_background_tasks("),
        "the card's detached line reuses the composer strip's formatter"
    );
    assert!(
        !map.contains("plan_flash_active"),
        "the running roster is a separate read from the flash deadlines"
    );
}
