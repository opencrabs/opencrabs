//! Clean starts and closing every pane (#2361). A layout that won't clear is
//! the reported bug: `/panes close` empties it in one command, and `chat --new`
//! starts with one pane and a new session.

use crate::tui::pane::{PaneManager, SplitDirection};

const ARGS: &str = include_str!("../cli/args.rs");
const UI: &str = include_str!("../cli/ui.rs");
const MESSAGING: &str = include_str!("../tui/app/messaging.rs");
const APP_STATE: &str = include_str!("../tui/app/state.rs");

#[test]
fn close_all_but_focused_leaves_one_pane_and_counts_the_closed_ones() {
    let mut panes = PaneManager::new();
    panes.split(SplitDirection::Horizontal);
    panes.split(SplitDirection::Vertical);
    panes.split(SplitDirection::Horizontal);
    assert_eq!(panes.pane_count(), 4);

    let closed = panes.close_all_but_focused();

    assert_eq!(closed, 3);
    assert_eq!(panes.pane_count(), 1);
    assert!(!panes.is_split());
}

#[test]
fn close_all_on_a_single_pane_closes_nothing() {
    let mut panes = PaneManager::new();
    assert_eq!(panes.close_all_but_focused(), 0);
    assert_eq!(panes.pane_count(), 1);
}

#[test]
fn chat_new_is_a_flag_on_the_chat_command() {
    assert!(
        ARGS.contains("        /// Start clean: drop the saved pane layout and open a new session")
    );
    assert!(ARGS.contains("        new: bool,"));
    assert!(ARGS.contains("ui::cmd_chat(&config, session, force_onboard, start_fresh)"));
}

#[test]
fn chat_new_resets_the_layout_and_starts_a_new_session() {
    assert!(UI.contains("app.pane_manager = crate::tui::pane::PaneManager::new();"));
    assert!(APP_STATE.contains("} else if self.start_fresh {"));
    assert!(APP_STATE.contains("self.create_new_session().await?;"));
}

#[test]
fn panes_close_is_a_slash_command() {
    assert!(MESSAGING.contains("\"/panes\" => {"));
    assert!(MESSAGING.contains("self.pane_manager.close_all_but_focused()"));
}

#[test]
fn ctrl_x_logs_a_failed_pane_load_instead_of_stopping_the_handler() {
    let start = APP_STATE
        .find("if keys::is_close_pane(&event) && self.pane_manager.is_split() {")
        .expect("the Ctrl+X handler exists");
    let handler = &APP_STATE[start..start + 900];
    assert!(handler.contains("if let Err(e) = self.load_session(session_id).await {"));
    assert!(!handler.contains("self.load_session(session_id).await?;"));
}
