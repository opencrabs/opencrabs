//! Esc leaves the usage ledger and the files screen (#2330).
//!
//! The window has no headless harness, so these checks read the handler
//! source: the escape branch must route through the pure chord table
//! (`screen_chord`, gui/screen_keys.rs) before the running-turn gate, and the
//! arm that applies the action must map it to the screen's own toggle. The
//! table's mapping is pinned windowless by the gui bin suite; the wiring is
//! the contract here, so a busy turn cannot hold a screen open and a refactor
//! cannot silently drop the route (#2356).

const GUI_MAIN: &str = include_str!("../gui/main.rs");

fn escape_branch() -> &'static str {
    let start = GUI_MAIN
        .find("if ev.keystroke.key == \"escape\" {")
        .expect("on_key has an escape branch");
    GUI_MAIN
        .get(start..)
        .expect("the escape branch starts on a char boundary")
        .split("if !self.busy {")
        .next()
        .expect("the escape branch ends at the turn gate")
}

fn screen_action_arm() -> &'static str {
    let start = GUI_MAIN
        .find("fn apply_screen_action")
        .expect("the window applies screen actions through one arm");
    &GUI_MAIN[start..]
}

#[test]
fn esc_leaves_the_usage_screen() {
    let branch = escape_branch();
    assert!(
        branch.contains("screen_chord(\"escape\", mods, self.screen)"),
        "Esc routes through the chord table before the turn gate"
    );
    assert!(
        screen_action_arm().contains(
            "ScreenAction::EnterUsage | ScreenAction::LeaveUsage => self.toggle_usage(cx)"
        ),
        "the table's usage action toggles the ledger"
    );
}

#[test]
fn esc_leaves_the_files_screen() {
    let branch = escape_branch();
    assert!(
        branch.contains("screen_chord(\"escape\", mods, self.screen)"),
        "Esc routes through the chord table before the turn gate"
    );
    assert!(
        screen_action_arm().contains(
            "ScreenAction::EnterFiles | ScreenAction::LeaveFiles => self.toggle_files(cx)"
        ),
        "the table's files action toggles the list"
    );
}
