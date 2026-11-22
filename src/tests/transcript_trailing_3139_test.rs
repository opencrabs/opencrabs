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
