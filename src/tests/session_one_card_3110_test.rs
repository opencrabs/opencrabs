//! The session is one card, so nothing paints past the transcript corners
//! (#3110).
//!
//! The owner read the chat screen as a stack of objects that had nothing to do
//! with each other: the plan card and the suggested options sat beside the
//! history instead of on it, a scrolled row painted square over the history's
//! rounded corner, and the composer floated under the whole thing with a
//! surface of its own. With transparency on, every one of those seams was
//! visible.
//!
//! The fix is structural. The history, the strips, the topic rows and the
//! composer are rows of a single rounded, clipped surface, and the radius is
//! the one the message bubbles already use, because that surface is the box
//! they live in. A split pane paints the same surface itself, so the two
//! arrangements agree.
//!
//! These tests read the source: what has to hold is where an element sits in
//! the tree and what owns the clip, and neither is observable from a value.

/// The desktop window's chat screen.
const RENDER: &str = include_str!("../gui/dashboard_render.rs");
/// The transcript, which owns the history's own scroll.
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
/// One pane of a split, which is the session surface in that arrangement.
const PANE: &str = include_str!("../gui/pane_view.rs");

/// The block that builds the session surface, from its first line to the end of
/// the chain that paints it.
fn session_block() -> &'static str {
    let start = RENDER
        .find("let mut session = div()")
        .expect("the chat screen builds a session surface");
    let rest = &RENDER[start..];
    let end = rest
        .find(".overflow_hidden();")
        .expect("the session surface ends its chain by clipping");
    &rest[..end + ".overflow_hidden();".len()]
}

/// The session surface is rounded with the bubbles' own radius, filled with the
/// transcript's own fill, and clipped.
///
/// All three have to be true for the corner to disappear. A different radius
/// leaves the two surfaces reading as separate objects at the edge, a different
/// fill leaves a seam where the inner surface ends, and no clip lets a scrolled
/// row paint over the arc.
#[test]
fn the_session_surface_matches_the_bubbles_it_holds() {
    let block = session_block();
    assert!(
        block.contains(".rounded(px(transcript::MESSAGE_RADIUS))"),
        "the surface takes the message bubbles' curve, not the card's: {block}"
    );
    assert!(
        block.contains(".bg(rgb(transcript::surface_fill(palette::current().bg)))"),
        "the surface takes the transcript's fill, so no seam shows: {block}"
    );
    assert!(
        block.contains(".overflow_hidden()"),
        "the surface owns the clip that keeps a row off the corner: {block}"
    );
    assert!(
        !block.contains("CARD_RADIUS"),
        "the card's radius is the wrong token here: {block}"
    );
}

/// The clip cannot live on the transcript: it scrolls.
///
/// `overflow_hidden` after `overflow_y_scroll` on the same element replaces it,
/// so the history would stop scrolling. The surface one level up is the only
/// place the corner can be cut, which is why the transcript's own element has no
/// clip and keeps the scroll.
#[test]
fn the_transcript_keeps_its_scroll_and_gives_up_the_clip() {
    let scroll = TRANSCRIPT
        .find(".track_scroll(scroll)")
        .expect("the transcript scrolls");
    let tail = &TRANSCRIPT[scroll..];
    let end = tail.find(".bg(rgb(surface_fill(").expect("the history paints");
    let element = &tail[..end];
    assert!(
        element.contains(".rounded(px(MESSAGE_RADIUS))"),
        "the history's own surface takes the bubbles' curve: {element}"
    );
    assert!(
        !element.contains(".overflow_hidden()"),
        "a clip here would replace the scroll: {element}"
    );
}

/// The composer is a row of the card, not a surface under it.
///
/// This is the half the owner called "flying in the UX": the field had its own
/// border and radius on the window background, a gap away from the history, so
/// the two never touched. It is appended to the session now, and the wrapper
/// that centres the card is the only thing the column gets.
#[test]
fn the_composer_is_a_row_of_the_session() {
    assert!(
        RENDER.contains("session = session.child(shell.child(dock.child(footer.into_any_element())));"),
        "the composer joins the session surface"
    );
    assert!(
        RENDER.contains(".child(session.when(!centred_chat, |d| d.w_full()))"),
        "the card is centred as a whole, at the conversation's measure"
    );
    assert!(
        !RENDER.contains("col = col.child(session);"),
        "the card is not handed to the column bare, or it would not centre"
    );
}

/// A split pane paints the same surface, with the same curve.
///
/// Each pane owns its own fill and its own clip in that arrangement, so the
/// window-level surface stands down and the panes carry it. They have to agree
/// with the unsplit card about the radius or the two layouts look like two
/// different products.
#[test]
fn a_split_pane_carries_the_same_surface() {
    assert!(
        PANE.contains(".rounded(px(transcript::MESSAGE_RADIUS))"),
        "the pane's box takes the session's curve"
    );
    assert!(
        !PANE.contains("CARD_RADIUS"),
        "the card's radius is the wrong token for a pane"
    );
    assert!(
        RENDER.contains("if self.window_is_split() {\n                    col = col.child(middle);"),
        "a split keeps the panes bare: they paint their own surface"
    );
}
