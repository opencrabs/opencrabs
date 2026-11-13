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

/// The modules come first so a scan that cuts the window's own test module
/// off the end still sees every one of them: the split marker sits in
/// `main.rs`, and it is the last part of this text.
pub const DASHBOARD: &str = concat!(
    include_str!("../gui/dashboard_commands.rs"),
    include_str!("../gui/dashboard_finder.rs"),
    include_str!("../gui/dashboard_keys.rs"),
    include_str!("../gui/dashboard_messages.rs"),
    include_str!("../gui/dashboard_models.rs"),
    include_str!("../gui/dashboard_nav.rs"),
    include_str!("../gui/dashboard_panes.rs"),
    include_str!("../gui/dashboard_projects.rs"),
    include_str!("../gui/projects_screen.rs"),
    include_str!("../gui/dashboard_rail.rs"),
    include_str!("../gui/dashboard_screens.rs"),
    include_str!("../gui/dashboard_sessions.rs"),
    include_str!("../gui/dashboard_topics.rs"),
    include_str!("../gui/dashboard_turns.rs"),
    include_str!("../gui/main.rs"),
);
