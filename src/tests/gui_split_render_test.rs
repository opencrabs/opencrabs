//! The window draws the split's second pane on the chat screen (#2438). The
//! window has no headless harness, so these checks read source. Stage one
//! stored the picked pane and drew it only inside the themes preview, so
//! choosing a session from the menu changed state and nothing appeared.

const GUI_MAIN: &str = include_str!("../gui/main.rs");

/// The chat arm of the screen body, from its opening to the next screen.
fn chat_arm() -> &'static str {
    GUI_MAIN
        .split("let (middle, footer): (AnyElement, AnyElement) = match self.screen {")
        .nth(1)
        .expect("the screen body match is built in render")
        .split("Screen::Usage => {")
        .next()
        .expect("the chat arm comes before the usage arm")
}

#[test]
fn the_chat_screen_draws_the_second_pane() {
    assert!(
        chat_arm().contains("split_body("),
        "the chat arm must draw the split through split_body, not only the themes preview"
    );
}

#[test]
fn a_split_widens_nothing_when_it_is_absent() {
    let chat = chat_arm();
    assert!(
        chat.contains("let primary_width = if self.split.is_some() {"),
        "the chat pane plans its rows against the width it is actually drawn into"
    );
    assert!(
        chat.contains("(pane_width - 8.0) / 2.0"),
        "two panes share the content width, less the gap between them"
    );
}

#[test]
fn the_second_pane_is_composed_in_one_place() {
    let helper = GUI_MAIN
        .split("fn split_body(")
        .nth(1)
        .expect("split_body is defined");
    assert!(
        helper.contains("split_header(pane, close)"),
        "the second pane carries its session header and close button"
    );
    assert!(
        helper.contains(".when(pane.layout.is_stacked(), |d| d.flex_col())"),
        "Split _ stacks the panes, Split | sets them side by side"
    );
    assert_eq!(
        GUI_MAIN.matches("\"split-secondary\"").count(),
        1,
        "the second pane is built in split_body only, so both screens draw the same thing"
    );
}

#[test]
fn choosing_a_session_repaints_before_the_transcript_loads() {
    let choose = GUI_MAIN
        .split("fn choose_split(")
        .nth(1)
        .expect("choose_split is defined");
    let stored = choose
        .find("self.split = Some(SplitPane {")
        .expect("the picked pane is stored");
    let painted = choose[stored..]
        .find("cx.notify();")
        .expect("storing the pane repaints the window");
    assert!(
        painted < choose[stored..].find("cx.spawn").unwrap_or(painted + 1),
        "the repaint happens before the transcript load is spawned"
    );
}
