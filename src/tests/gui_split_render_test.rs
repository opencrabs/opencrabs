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
        chat.contains("let primary_width = match &self.split {"),
        "the chat pane plans its rows against the width it is actually drawn into"
    );
    assert!(
        chat.contains("split_pane_width(pane_width, pane.layout.is_stacked())"),
        "two panes share the content width through split_pane_width, which subtracts the gap and the rule"
    );
}

#[test]
fn the_second_pane_is_composed_in_one_place() {
    let helper = GUI_MAIN
        .split("fn split_body(")
        .nth(1)
        .expect("split_body is defined");
    assert!(
        helper.contains("\"split-close-secondary\""),
        "the second pane carries its session header and close button"
    );
    assert!(
        helper.contains("let stacked = pane.layout.is_stacked();")
            && helper.contains(".when(stacked, |d| d.flex_col())")
            && helper.contains(".when(!stacked, |d| d.flex_row())"),
        "Split _ stacks the panes, Split | sets them side by side"
    );
    assert_eq!(
        GUI_MAIN.matches("\"split-secondary\"").count(),
        1,
        "the second pane is built in split_body only, so both screens draw the same thing"
    );
}

#[test]
fn both_panes_carry_a_header_and_a_close_button() {
    let helper = GUI_MAIN
        .split("fn split_body(")
        .nth(1)
        .expect("split_body is defined");
    // Only the second pane used to have a header, so one session read as a
    // named session and the other as a nameless strip (#2474).
    assert!(
        helper.contains("\"split-close-primary\""),
        "the chat pane carries a header too"
    );
    assert!(
        helper.contains("\"split-close-secondary\""),
        "the picked pane keeps its header"
    );
    assert!(
        GUI_MAIN.contains("fn pane_header("),
        "one helper draws both headers, so the two cannot drift"
    );
    assert!(
        GUI_MAIN.contains("focus == PaneFocus::Primary")
            && GUI_MAIN.contains("focus == PaneFocus::Secondary"),
        "the pane holding the caret wears the accent, so the focus is visible"
    );
}

#[test]
fn ctrl_x_closes_the_focused_pane() {
    let keys = GUI_MAIN
        .split("fn on_key(")
        .nth(1)
        .expect("on_key is defined")
        .split("fn ")
        .next()
        .expect("the handler has a body");
    assert!(
        keys.contains("&& ks.key == \"x\"")
            && keys.contains("&& self.split.is_some()")
            && keys.contains("self.close_focused_pane(cx);"),
        "ctrl+x closes the focused pane, the chord the terminal answers"
    );
    let close = GUI_MAIN
        .split("fn close_focused_pane(")
        .nth(1)
        .expect("close_focused_pane is defined");
    assert!(
        close.contains("(PaneFocus::Primary, Some(pane)) => self.open_session(pane.session, cx)"),
        "closing the chat hands the window to the picked session rather than dropping it"
    );
    assert!(
        close.contains("self.pane_focus = PaneFocus::Primary;"),
        "focus returns to the chat, so the next chord lands somewhere known"
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
    let tail = choose
        .get(stored..)
        .expect("the pane is stored on a char boundary");
    let painted = tail
        .find("cx.notify();")
        .expect("storing the pane repaints the window");
    assert!(
        painted < tail.find("cx.spawn").unwrap_or(painted + 1),
        "the repaint happens before the transcript load is spawned"
    );
}

#[test]
fn both_halves_take_the_same_share_of_the_width() {
    let body = GUI_MAIN
        .split("fn split_body(")
        .nth(1)
        .expect("split_body is defined");
    // The chat used to be handed in bare, so it sized to its content and the
    // split read as a chat with a strip beside it (#2473).
    assert!(
        body.contains("let half = div()"),
        "the primary pane is wrapped so it can take an equal share"
    );
    assert!(
        body.contains(".child(half)") && body.contains(".child(secondary)"),
        "both halves are children of the split row"
    );
    // The rule is what makes the boundary readable on a dark theme.
    assert!(
        body.contains("split_divider(stacked)"),
        "a visible rule sits between the panes"
    );
    assert!(
        GUI_MAIN.contains("const SPLIT_DIVIDER_PX: f32 = 1.0;"),
        "the rule has a named width"
    );
}

#[test]
fn a_stacked_pane_plans_against_the_whole_width() {
    let helper = GUI_MAIN
        .split("fn split_pane_width(")
        .nth(1)
        .expect("split_pane_width is defined")
        .split("\n}\n")
        .next()
        .expect("the helper has a body");
    assert!(
        helper.contains("if stacked {\n        content\n    }"),
        "stacked panes share the height, not the width, so a row gets all of it"
    );
    assert!(
        helper.contains("SPLIT_GAP_PX * 2.0") && helper.contains("SPLIT_DIVIDER_PX"),
        "side by side the halves subtract the gap either side and the rule"
    );
}
