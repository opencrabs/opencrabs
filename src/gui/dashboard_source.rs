//! The dashboard's source, in one string, for the tests that scan it (#2822).
//!
//! The window is its own crate root (`[[bin]] path = "src/gui/main.rs"`), so
//! the rules that read the dashboard as text cannot borrow the library's copy
//! of this constant: `crate::tests` is the library's, and a `#[cfg(test)]`
//! module in the library is not built when the window's test target links it.
//! The concat is therefore kept here, once, and `tests/gui_dashboard_source.rs`
//! holds the library's identical copy for the tests that live over there.
//!
//! A module that keeps code of its own beside this file has to be listed here,
//! or a rule pinning its window behaviour reads a text that does not contain
//! it. The window's boot, input and launch helpers moved out of `main.rs`
//! (#3060) and are listed with the rest.
pub const DASHBOARD: &str = concat!(
    include_str!("app.rs"),
    include_str!("boot.rs"),
    include_str!("channel_chip.rs"),
    include_str!("launch.rs"),
    include_str!("live_fold.rs"),
    include_str!("pane_focus.rs"),
    include_str!("task_strip.rs"),
    include_str!("turn_swap.rs"),
    include_str!("usage_load.rs"),
    include_str!("window_input.rs"),
    include_str!("dashboard_commands.rs"),
    include_str!("dashboard_config.rs"),
    include_str!("dashboard_finder.rs"),
    include_str!("dashboard_keys.rs"),
    include_str!("dashboard_messages.rs"),
    include_str!("dashboard_models.rs"),
    include_str!("dashboard_nav.rs"),
    include_str!("dashboard_panes.rs"),
    include_str!("dashboard_projects.rs"),
    include_str!("projects_screen.rs"),
    include_str!("dashboard_rail.rs"),
    include_str!("dashboard_render.rs"),
    include_str!("rail_view.rs"),
    include_str!("dashboard_screens.rs"),
    include_str!("dashboard_sessions.rs"),
    include_str!("dashboard_state.rs"),
    include_str!("dashboard_topics.rs"),
    include_str!("dashboard_turns.rs"),
    // The bang operator's built-in editor (#3163): the rules that pin the
    // window's modal and key behaviour read this text, so its code is in.
    include_str!("editor_dialog.rs"),
    include_str!("main.rs"),
);

/// Everything in [`DASHBOARD`] that the dashboard runs.
///
/// This used to cut at the window's inline `mod tests` and return what came
/// before it. That cut is gone, and the window file is why (#3060): the
/// declaration `mod tests;` matches the same marker as the inline block it
/// replaced, so the cut landed 108 lines into `main.rs` and every rule pinning
/// the render or the launch read a text without either in it.
///
/// Nothing is lost by not cutting. The window's tests live in `gui/tests/`,
/// beside this file and outside this concat, so no assertion's own needle can
/// be mistaken for the code it forbids (#2999). `dashboard_rail.rs` and
/// `rail_view.rs` still carry inline test modules and their text stays in,
/// which is the state every rule here already passed on; a marker cut could
/// only reach the last module in the concat and would drop the five after it.
pub fn production_half() -> &'static str {
    DASHBOARD
}
