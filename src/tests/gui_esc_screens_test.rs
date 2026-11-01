//! Esc leaves the usage ledger and the files screen (#2330).
//!
//! The window answers Esc in one chain. Screens sit in that chain with the
//! overlays, so a regression would show up as a missing branch rather than a
//! visible failure in a headless test. These checks read the handler source
//! and assert that both screens are answered, and that they are answered
//! before the running-turn gate, so a busy turn cannot hold a screen open.

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

#[test]
fn esc_leaves_the_usage_screen() {
    let branch = escape_branch();
    assert!(
        branch.contains("Screen::Usage => {") && branch.contains("self.toggle_usage(cx)"),
        "Esc returns from the usage ledger"
    );
}

#[test]
fn esc_leaves_the_files_screen() {
    let branch = escape_branch();
    assert!(
        branch.contains("Screen::Files => {") && branch.contains("self.toggle_files(cx)"),
        "Esc returns from the files screen"
    );
}
