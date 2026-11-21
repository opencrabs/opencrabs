//! #3005: the window has to tell the notification transport it holds focus.
//!
//! `should_raise` suppresses a banner only while a surface has REPORTED that
//! it holds focus. An unreported state reads as unknown, and unknown is
//! deliberate: "a surface that never reports is not evidence the user is
//! watching". The terminal honours that by reporting from its focus events,
//! and the window never reported at all, so every event raised a banner even
//! with the window frontmost and the user staring straight at the finished
//! task.
//!
//! gpui gives the dashboard no activation callback, so the window samples the
//! platform's active window on the poll tick it already runs. These checks read
//! source because the window has no headless harness; the behaviour they pin is
//! one call site, and its absence IS the defect.

const DASHBOARD_TURNS: &str = include_str!("../gui/dashboard_turns.rs");
const DESKTOP_NOTIFY: &str = include_str!("../utils/desktop_notify.rs");

#[test]
fn the_window_reports_its_focus_to_the_transport() {
    assert!(
        DASHBOARD_TURNS.contains("desktop_notify::set_surface_focused"),
        "the window never reports focus, so every banner raises"
    );
    assert!(
        DASHBOARD_TURNS.contains("active_window()"),
        "the report must be sampled from the platform's active window"
    );
    // The report comes from the same expression as the state it describes, so
    // the two cannot disagree.
    assert!(
        DASHBOARD_TURNS.contains("set_surface_focused(cx.active_window().is_some())"),
        "focus must be reported from the window that actually holds it"
    );
}

#[test]
fn the_report_lands_above_the_idle_early_return() {
    // The tick used to return early whenever no event had arrived, and an idle
    // window is exactly when a banner matters: reporting from below that return
    // would leave focus unknown for as long as nothing moved.
    let report = DASHBOARD_TURNS
        .find("set_surface_focused(")
        .expect("the tick reports focus");
    let idle = DASHBOARD_TURNS
        .find("if events.is_empty()")
        .expect("the tick still has its idle path");
    assert!(
        report < idle,
        "focus is sampled on every tick, not only on a busy one"
    );
}

#[test]
fn unknown_focus_still_raises() {
    // The fix must not turn "nobody reported" into "nobody is looking". A
    // surface that never reports raises, and the third state is what keeps
    // that true after the window started reporting.
    assert!(
        DESKTOP_NOTIFY.contains("const FOCUS_UNKNOWN: u8 = 0;"),
        "the transport lost its third focus state"
    );
    assert!(
        DESKTOP_NOTIFY.contains("_ => None,"),
        "an unreported surface must read as unknown, not as idle"
    );
    assert!(
        DESKTOP_NOTIFY.contains("if surface_focused() == Some(true) {"),
        "only a positive report may suppress a banner"
    );
}
