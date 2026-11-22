//! The rows under the last turn ride the conversation's own scroll (#3139).
//!
//! The hint, the follow-up offers and the live plan card used to be children of
//! the `above` box, which is a sibling of the scroll container. Every strip in
//! it took its height from the conversation, so a live plan plus a waiting offer
//! plus the hint was most of the lower half of the window and none of it could
//! be scrolled away.
//!
//! There is no headless window here, so the seam is pinned at the source: the
//! hook exists, every exit from `view` hands its rows over, and the fold keeps
//! the caller's order. That is the same shape the rest of the desktop layout
//! tests use, and it is honest about what it proves.

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");

/// The body of the fold, from its signature to its closing brace.
fn helper_body() -> &'static str {
    let start = TRANSCRIPT
        .find("fn with_trailing(col: Stateful<Div>, trailing: Vec<AnyElement>) -> Stateful<Div> {")
        .expect("the seam is a function of its own");
    TRANSCRIPT[start..]
        .split("\n}\n")
        .next()
        .expect("the helper ends at its closing brace")
}

/// The body of the view, so a `with_trailing` call elsewhere cannot answer for
/// it.
fn view_body() -> &'static str {
    let start = TRANSCRIPT
        .find("pub fn view(")
        .expect("the view is drawn once");
    TRANSCRIPT[start..]
        .split("\n}\n")
        .next()
        .expect("the view ends at its closing brace")
}

#[test]
fn the_hooks_carry_the_rows() {
    assert!(
        TRANSCRIPT.contains("pub trailing: Vec<AnyElement>,"),
        "the caller has a place to hand the rows over"
    );
    assert!(
        TRANSCRIPT.contains("        trailing,\n    } = hooks;"),
        "and the view takes them out of the hooks it was given"
    );
}

#[test]
fn every_exit_from_the_view_hands_the_rows_over() {
    let view = view_body();
    // Three empty states return early: no session, a failed read with nothing
    // to show, and a session with no messages. A live plan card has to draw in
    // all three, so all three go through the fold.
    assert_eq!(
        view.matches("return with_trailing(col, trailing);").count(),
        3,
        "each early exit appends the rows before it returns"
    );
    assert!(
        view.trim_end().ends_with("with_trailing(col, trailing)"),
        "and the normal end of the conversation appends them last"
    );
}

#[test]
fn the_fold_keeps_the_callers_order() {
    let body = helper_body();
    assert!(
        body.contains("trailing.into_iter().fold(col, |col, row| {"),
        "the rows are appended in the order they were handed over: {body}"
    );
    assert!(
        body.contains("col.child(div()"),
        "each one becomes a child of the scroll container: {body}"
    );
    assert!(
        body.contains(".flex_none()"),
        "a tall card scrolls instead of being squeezed into what is left: {body}"
    );
    assert!(
        body.contains(".w_full()") && body.contains(".min_w_0()"),
        "and takes the transcript's own measure, with no second gutter: {body}"
    );
}

const DASHBOARD_RENDER: &str = include_str!("../gui/dashboard_render.rs");

/// The `above` box: from the line that opens it to the frame that decides
/// whether it is drawn at all.
fn above_box() -> &'static str {
    let start = DASHBOARD_RENDER
        .find("let mut above = div()")
        .expect("the chat keeps a box between the history and the field");
    let end = DASHBOARD_RENDER[start..]
        .find("if above_used {")
        .expect("and it is appended only when something went in it");
    &DASHBOARD_RENDER[start..start + end]
}

/// The body of the row builder.
fn trailing_body() -> &'static str {
    let start = DASHBOARD_RENDER
        .find("fn chat_trailing(")
        .expect("the chat builds its rows in one place");
    DASHBOARD_RENDER[start..]
        .split("\n    }\n")
        .next()
        .expect("the method ends at its closing brace")
}

#[test]
fn the_chat_hands_its_rows_to_the_transcript() {
    assert!(
        DASHBOARD_RENDER.contains("let trailing = self.chat_trailing(window, cx);"),
        "the rows are built before the view that draws them"
    );
    assert!(
        DASHBOARD_RENDER.contains("\n                        trailing,\n                    },\n"),
        "and handed over in the hooks"
    );
}

#[test]
fn the_rows_are_no_longer_in_the_above_box() {
    let above = above_box();
    for gone in ["hint_line(", "options_strip(", "editing_view("] {
        assert!(
            !above.contains(gone),
            "{gone} still draws outside the scroll: {above}"
        );
    }
    // What names something the window is doing right now stays where it was.
    assert!(above.contains("queue_strip("), "the queue keeps its row");
    assert!(
        above.contains("running_tasks_label()"),
        "so does the detached command"
    );
}

#[test]
fn the_rows_are_built_in_the_order_that_was_asked_for() {
    let body = trailing_body();
    let hint = body.find("hint_line(").expect("the hint is a row");
    let offers = body.find("options_strip(").expect("the offers are a row");
    let plan = body.find("editing_view(").expect("the plan card is a row");
    assert!(
        hint < offers && offers < plan,
        "the hint comments on the turn above it, then what is waiting, then the plan"
    );
}

const PANE_VIEW: &str = include_str!("../gui/pane_view.rs");

/// The pane's own box between its history and its input.
fn pane_above_box() -> &'static str {
    let start = PANE_VIEW
        .find("let mut above = div()")
        .expect("a pane keeps a box under its history too");
    let end = PANE_VIEW[start..]
        .find("if above_used {")
        .expect("and appends it only when something went in it");
    &PANE_VIEW[start..start + end]
}

#[test]
fn a_split_pane_hands_its_rows_over_too() {
    assert!(
        PANE_VIEW.contains("trailing: pane_rows,"),
        "the pane passes its rows to its own transcript"
    );
    let handed: Vec<&str> = PANE_VIEW.split("pane_rows.push(").skip(1).collect();
    assert_eq!(handed.len(), 2, "the pane hands over exactly two rows");
    assert!(
        handed[0].starts_with("options_strip("),
        "the offers waiting on a click come first"
    );
    assert!(
        handed[1].contains("plan-strip-"),
        "then the plan card the work belongs to"
    );
    assert!(
        !PANE_VIEW.contains("above = above.child(options_strip"),
        "the offers are no longer appended above the input"
    );
    assert!(
        pane_above_box().contains("queue_strip("),
        "the queue keeps its row above the input"
    );
    assert!(
        PANE_VIEW.contains("toggle_pane_plan(session, cx)"),
        "and the plan card keeps the tap that folds it"
    );
}
