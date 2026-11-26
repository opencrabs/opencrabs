//! The window's dashboard source, wherever the split left it (#2822).
//!
//! The dashboard's inherent impl was one block inside `gui/main.rs`. It is now
//! split into modules by concern, so a source-reading rule that used to name
//! `main.rs` and mean "the dashboard" has to read all of them. A rule can move
//! between modules without warning: a guard that reads only one would pass
//! while the code it forbids lives next door, and a positive check would fail
//! on code that is present and correct.
//!
//! The window has no headless harness, so these rules are read as text. A
//! region that has to keep its own order still reads the one module it lives
//! in; this constant is for "the dashboard contains this", which is what the
//! single-file scan meant before the split.

/// The window's boot, input and launch helpers are listed first because they
/// moved out of the window file (#3060): a rule pinning what the window does
/// at startup or on a keystroke reads them here, where they now live. The
/// window file itself is last and carries no inline tests: its tests live in
/// `gui/tests/` now, so the whole of this text is production.
pub const DASHBOARD: &str = concat!(
    include_str!("../gui/app.rs"),
    include_str!("../gui/boot.rs"),
    include_str!("../gui/channel_chip.rs"),
    include_str!("../gui/launch.rs"),
    include_str!("../gui/live_fold.rs"),
    include_str!("../gui/pane_focus.rs"),
    include_str!("../gui/task_strip.rs"),
    include_str!("../gui/turn_swap.rs"),
    include_str!("../gui/usage_load.rs"),
    include_str!("../gui/window_input.rs"),
    include_str!("../gui/dashboard_commands.rs"),
    include_str!("../gui/dashboard_config.rs"),
    include_str!("../gui/dashboard_finder.rs"),
    include_str!("../gui/dashboard_keys.rs"),
    include_str!("../gui/dashboard_messages.rs"),
    include_str!("../gui/dashboard_models.rs"),
    include_str!("../gui/dashboard_nav.rs"),
    include_str!("../gui/dashboard_panes.rs"),
    include_str!("../gui/dashboard_projects.rs"),
    include_str!("../gui/projects_screen.rs"),
    include_str!("../gui/dashboard_rail.rs"),
    include_str!("../gui/dashboard_render.rs"),
    // The rail itself (#2933): rows, chips and the filter menu are built in
    // their own module now, so a "the dashboard contains this" scan has to read
    // it too.
    include_str!("../gui/rail_view.rs"),
    include_str!("../gui/dashboard_screens.rs"),
    include_str!("../gui/dashboard_sessions.rs"),
    include_str!("../gui/dashboard_state.rs"),
    include_str!("../gui/dashboard_topics.rs"),
    include_str!("../gui/dashboard_turns.rs"),
    // The bang operator's built-in editor (#3163), mirroring the window's
    // own concat.
    include_str!("../gui/editor_dialog.rs"),
    include_str!("../gui/main.rs"),
);
