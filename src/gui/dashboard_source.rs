//! The dashboard's source, in one string, for the tests that scan it (#2822).
//!
//! The window is its own crate root (`[[bin]] path = "src/gui/main.rs"`), so
//! the rules that read the dashboard as text cannot borrow the library's copy
//! of this constant: `crate::tests` is the library's, and a `#[cfg(test)]`
//! module in the library is not built when the window's test target links it.
//! The concat is therefore kept here, once, and `tests/gui_dashboard_source.rs`
//! holds the library's identical copy for the tests that live over there.
//!
//! The modules come first so a scan that cuts the window's own test module off
//! the end still sees every one of them: the split marker sits in `main.rs`,
//! and it is the last part of this text.
pub const DASHBOARD: &str = concat!(
    include_str!("dashboard_commands.rs"),
    include_str!("dashboard_finder.rs"),
    include_str!("dashboard_keys.rs"),
    include_str!("dashboard_messages.rs"),
    include_str!("dashboard_models.rs"),
    include_str!("dashboard_nav.rs"),
    include_str!("dashboard_panes.rs"),
    include_str!("dashboard_rail.rs"),
    include_str!("dashboard_screens.rs"),
    include_str!("dashboard_sessions.rs"),
    include_str!("dashboard_topics.rs"),
    include_str!("dashboard_turns.rs"),
    include_str!("main.rs"),
);
