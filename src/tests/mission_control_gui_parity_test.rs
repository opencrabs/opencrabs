//! GUI Mission Control parity sentinels.
//!
//! GPUI rendering has no headless harness here, so the library suite pins the
//! wiring and field coverage while the GUI binary suite exercises navigation.

use crate::brain::mission_control::ACTIVITY_LIMIT;

// The panels are their own module since #2817 follow-up, so the parity scans
// read where the rows are built rather than the crate root.
const GUI_MISSION_VIEW: &str = include_str!("../gui/mission_control_view.rs");
const GUI_MISSION: &str = include_str!("../gui/mission_control.rs");
const TERMINAL_ACTIONS: &str = include_str!("../tui/app/mission_control/actions.rs");

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let tail = source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing {start}"))
        .1;
    tail.split_once(end)
        .unwrap_or_else(|| panic!("missing {end}"))
        .0
}

#[test]
fn mission_control_activity_fetch_limit_is_shared() {
    assert_eq!(ACTIVITY_LIMIT, 100);
    assert!(GUI_MISSION.contains("activity: activity_service::recent(ACTIVITY_LIMIT)"));
    assert!(TERMINAL_ACTIONS.contains("activity_service::recent(ACTIVITY_LIMIT)"));
    assert!(GUI_MISSION_VIEW.contains("mission_control::ACTIVITY_ROWS"));
}

#[test]
fn mission_control_gui_rows_carry_terminal_metadata() {
    let inbox = between(GUI_MISSION_VIEW, "fn mc_inbox_panel", "fn mc_window_tabs");
    assert!(inbox.contains("item.source"));
    assert!(inbox.contains("mc_relative_time(Some(item.created_at))"));
    assert!(inbox.contains("(selected - start) * 3"));

    let schedule = between(
        GUI_MISSION_VIEW,
        "fn mc_schedule_panel",
        "fn mission_control_detail",
    );
    assert!(schedule.contains("job.kind.label()"));

    let top_tools = between(
        GUI_MISSION_VIEW,
        "fn mc_top_tools_panel",
        "fn mc_activity_panel",
    );
    assert!(top_tools.contains("t.fail_rate"));
    assert!(top_tools.contains("mc_tool_bar"));
    assert!(top_tools.contains("let max ="));

    let totals = between(GUI_MISSION_VIEW, "fn mc_totals_panel", "fn mc_models_panel");
    assert!(totals.contains("\"RSI live\""));

    let rate = between(GUI_MISSION_VIEW, "fn mc_rate_color", "fn mc_totals_panel");
    assert!(rate.contains("rate >= 10.0"));
    assert!(!rate.contains("rate >= 5.0"));
}

#[test]
fn mission_control_gui_detail_dispatch_covers_every_panel() {
    let dispatch = between(
        GUI_MISSION_VIEW,
        "fn mission_control_detail",
        "fn mc_detail_body",
    );
    for panel in [
        "mission_control::Panel::Inbox => mc_inbox_detail",
        "mission_control::Panel::Activity => mc_activity_detail",
        "mission_control::Panel::Schedule => mc_schedule_detail",
        "mission_control::Panel::Analytics => mc_analytics_detail",
    ] {
        assert!(dispatch.contains(panel), "missing detail dispatch: {panel}");
    }

    let activity = between(
        GUI_MISSION_VIEW,
        "fn mc_activity_detail",
        "fn mc_schedule_duration",
    );
    for field in ["\"When\"", "\"Source\"", "\"Level\"", "\"Detail\""] {
        assert!(activity.contains(field), "activity detail lost {field}");
    }
    assert!(activity.contains("\"undated\""));

    let schedule = between(
        GUI_MISSION_VIEW,
        "fn mc_schedule_detail",
        "fn mc_analytics_detail",
    );
    for field in [
        "\"Kind\"",
        "\"Label\"",
        "\"State\"",
        "\"Cron\"",
        "\"Next\"",
        "\"At\"",
        "\"Status\"",
        "\"Cost\"",
        "\"Duration\"",
        "\"Target\"",
        "\"Prompt\"",
        "\"Created\"",
        "\"Profile\"",
        "\"ID\"",
    ] {
        assert!(schedule.contains(field), "schedule detail lost {field}");
    }
    for label in [
        "\"cron job\"",
        "\"pending approval\"",
        "\"awaiting user\"",
        "\"active\"",
    ] {
        assert!(schedule.contains(label), "schedule detail changed {label}");
    }
    for conditional in [
        "if let Some(status) = job.last_run_status.as_deref()",
        "if let Some(cost) = job.last_run_cost",
        "if let Some(secs) = job.last_run_duration_secs",
        "if let Some(target) = job.deliver_to.as_deref()",
        "if !job.prompt.is_empty()",
        "if let Some(profile) = job.profile_name.as_deref()",
    ] {
        assert!(
            schedule.contains(conditional),
            "missing conditional: {conditional}"
        );
    }

    let analytics = between(
        GUI_MISSION_VIEW,
        "fn mc_analytics_detail",
        "const MC_LOG_ROWS",
    );
    assert!(analytics.contains("mc_analytics_cards(snapshot, true, shown)"));
    assert!(analytics.contains("mission_control::grid_rows(count, 2)"));
}

#[test]
fn mc_phantoms_empty_breakdown_uses_the_panel_empty_state() {
    let rows = between(
        GUI_MISSION_VIEW,
        "fn mc_phantoms_rows",
        "fn mc_phantoms_panel",
    );
    assert!(rows.contains("if a.phantom.by_model.is_empty()"));
    assert!(rows.contains("return Vec::new()"));

    let panel = between(GUI_MISSION_VIEW, "fn mc_phantoms_panel", "fn mc_rsi_panel");
    assert!(panel.contains("mc_phantoms_rows(a, shown)"));
    assert!(panel.contains("\"No phantoms detected.\""));
}

#[test]
fn mc_line_rows_render_through_the_flex_row_truncate_shape() {
    // A truncated text element inside a plain width-full block div is measured
    // once at the grid's flex-basis zero and stays an ellipsis forever
    // (#2642, gpui caches nowrap layout on wrap_width). The working shape is
    // the one the label/value arm already used: flex row, then a growing,
    // shrinkable text child. Pin both halves so neither can regress.
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let panel = flat(between(
        GUI_MISSION_VIEW,
        "fn mc_panel(",
        "fn mc_inbox_panel",
    ));
    assert!(
        panel.contains("McRow::Line(line, _) => {"),
        "the line arm must exist in the panel row builder"
    );
    assert!(
        panel.contains(".child(text(line, Size::Xs, color).flex_1().min_w_0().truncate())"),
        "the line renders as a growing, shrinkable, truncated text child (#2642)"
    );
    assert!(
        !GUI_MISSION_VIEW.contains(".text_xs().truncate().child("),
        "the block-div truncate shape is back in the panel rows"
    );
}
