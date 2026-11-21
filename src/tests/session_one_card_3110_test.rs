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

/// The session surface is the shot: edge to edge, square, and clipped.
///
/// It started as a rounded card the width of the conversation's measure
/// (#3110), which is what made the chat read as one object instead of a stack
/// of strips. The owner then took it one step further: the ground is not a card
/// either. It takes the full height and the full width of the pane with no
/// radius, so no band of empty background shows above the first row or below
/// the field. What keeps a measure is the CONTENT: the transcript caps itself,
/// and every strip goes through `session_row`, which caps and centres too.
///
/// The clip stays. It is the only element above the transcript, and the
/// transcript scrolls, so a clip there would replace the scroll.
#[test]
fn the_session_surface_is_the_whole_shot() {
    let block = session_block();
    assert!(
        !block.contains(".rounded("),
        "the ground carries no corner of its own: {block}"
    );
    assert!(
        block.contains(".bg(rgb(transcript::surface_fill(palette::current().bg)))"),
        "the surface takes the transcript's fill, so no seam shows: {block}"
    );
    assert!(
        block.contains(".w_full()"),
        "the ground spans the pane, not the text measure: {block}"
    );
    assert!(
        block.contains(".h_full()"),
        "and it spans the height, or the background stops above the field: {block}"
    );
    assert!(
        block.contains(".overflow_hidden()"),
        "the surface owns the clip that keeps a row off the edge: {block}"
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
    let end = tail
        .find(".bg(rgb(surface_fill(")
        .expect("the history paints");
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
        RENDER.contains("shell.child(dock.child(footer))"),
        "the composer joins the session surface"
    );
    assert!(
        RENDER.contains("                            .child(session),"),
        "the ground fills the row it is handed, so the wrapper only grows"
    );
    assert!(
        !RENDER.contains("col = col.child(session);"),
        "the card is not handed to the column bare, or it would not centre"
    );
}

/// Every card in the session is the size of a message (#3110, #3114).
///
/// The owner's second pass: with the strips inside the session they took its
/// whole width, so their left and right edges were the session's own edges and
/// only the top and bottom of each card was visible. They have to be the size of
/// the user and assistant cards, which sit half a rem inside the content column.
///
/// That half rem is `px_2`, the transcript's own padding step, and it is a rem
/// rather than a pixel count, so the two agree at every zoom level. A literal
/// eight would match at the default zoom and drift as soon as the reader zoomed,
/// which is why the gutter is not a constant here.
///
/// The ground itself went edge to edge afterwards (#3112), so the helper cannot
/// lean on the surface's width any more: it caps the strip at the conversation's
/// measure and centres it, which is the same column the transcript draws in.
///
/// The composer was the one exception for a while, on a pass that took it edge
/// to edge (#3112). The owner read that on a single pane and sent it back: a
/// full-bleed band under measure-wide bubbles is a slab from another window, and
/// the words inside it had to be pushed back onto the column by hand. So the
/// composer is a card again, placed by the same helper, and its rows pad the
/// card's own column instead of the pane's.
#[test]
fn every_card_in_the_session_is_the_size_of_a_message() {
    let row = RENDER
        .find("fn session_row(view: AnyElement, centred: bool, measure: f32) -> AnyElement {")
        .expect("the strips go through one helper");
    let body = &RENDER[row..row + 700];
    assert!(
        body.contains(".px_2()"),
        "the strip keeps the transcript's own padding step: {body}"
    );
    assert!(
        body.contains(".when(centred, |d| d.w(px(measure)))"),
        "and it takes the content column, not the whole ground: {body}"
    );
    assert!(
        !body.contains("SESSION_GUTTER"),
        "the gutter is a rem step, never a pixel literal: {body}"
    );
    // The transcript's rows are padded by that same step, which is what the
    // strips are matching. The element is sliced to its own chain rather than a
    // fixed character window: comments between the scroll and the padding are
    // free to grow, and a window would start reading the next element's
    // paddings once they do.
    let scroll = TRANSCRIPT
        .find(".track_scroll(scroll)")
        .expect("the transcript scrolls");
    let tail = &TRANSCRIPT[scroll..];
    let end = tail
        .find(".bg(rgb(surface_fill(")
        .expect("the history paints its own surface");
    let element = &tail[..end];
    assert!(
        element.contains(".px_2()"),
        "the history pads its rows by the same rem step the strips use: {element}"
    );
    assert!(
        !element.contains(".px(px("),
        "the padding is a rem step, never a pixel literal: {element}"
    );
    // The composer is a card of a message's size again (#3114). #3112 ran it
    // edge to edge and the owner called that out on a single pane: a full-bleed
    // band under measure-wide bubbles reads as a slab from another window. It
    // goes through the same helper as the strips now, so its edge lands where a
    // message's edge lands.
    let shell = RENDER
        .find("let shell = div()")
        .expect("the composer still builds a shell");
    let end = RENDER
        .find("_ => right.child(middle).child(footer)")
        .expect("the other screens follow the chat arm");
    // The dock is built BEFORE the shell that wraps it, so the card's chrome
    // lives between the shell and the end of the chat arm. Slicing the shell to
    // the dock would run backwards and panic.
    let card = &RENDER[shell..end];
    assert!(
        card.contains(".rounded(px(transcript::MESSAGE_RADIUS))"),
        "the card wears the messages' curve: {card}"
    );
    assert!(
        card.contains(".border_1()"),
        "and their edge, because a card that floats has four sides: {card}"
    );
    let dock = RENDER
        .find("let dock = div()")
        .expect("the composer still builds a dock");
    let rows = &RENDER[dock..end];
    assert!(
        rows.contains(".child(session_row("),
        "the card is placed by the helper the strips use: {rows}"
    );
    assert!(
        rows.contains(".pb_2()"),
        "with the same half rem below it, or its bottom corner is cut off: {rows}"
    );
    // The words inside the card pad the card's own column (#3114). The card is
    // already half a rem inside the session, so a row that measured the pane's
    // inset again would sit half a rem right of every bubble above it.
    assert!(
        !rows.contains("central("),
        "a message-sized card's rows pad it, they do not measure the pane: {rows}"
    );
}

/// Every card in the session draws the same hairline (#3110).
///
/// The composer had a border and the plan card did not, so on the session fill
/// one read as a card and the other as background: the owner's words were that
/// the plan card "looks very much mixed with the background". The edge is one
/// function in the shared card module, fed the theme's raised tone, so none of
/// the cards can drift to a literal and none of them can disagree at a seam.
#[test]
fn every_card_in_the_session_draws_one_edge() {
    const CARD: &str = include_str!("../ui_core/card.rs");
    const PLAN: &str = include_str!("../gui/plan_strip.rs");
    const COMPOSER: &str = include_str!("../gui/composer_view.rs");
    assert!(
        CARD.contains("pub fn session_edge(base: u32) -> Hsla {"),
        "the edge is one token in the shared card module"
    );
    // The edge is a token or it follows the palette. What breaks the theme is an
    // OPAQUE literal, so that is what this rejects. The white washes at
    // `rgba(0xffffff..)` are alpha overlays: they tint whatever the theme put
    // under them, so they are theme-independent by construction, and a border
    // that reads `rgb(palette::current()...)` is following the theme too.
    for (name, src) in [("plan_strip.rs", PLAN), ("composer_view.rs", COMPOSER)] {
        let literal = src
            .lines()
            .find(|line| line.contains("border_color(") && line.contains("rgb(0x"));
        assert!(
            literal.is_none(),
            "{name} paints an edge from an opaque literal, which ignores the theme: {literal:?}"
        );
    }
    // Four plan surfaces: the seeded-less notice, the approval card, the live
    // checklist, and the same checklist compressed inside a split pane.
    assert_eq!(
        PLAN.matches(".border_color(session_edge(palette::current().panel_hi))")
            .count(),
        4,
        "the plan card's four shapes all carry the edge"
    );
    assert_eq!(
        COMPOSER
            .matches(".border_color(session_edge(palette::current().panel_hi))")
            .count(),
        1,
        "the queue strip carries the edge"
    );
    assert!(
        COMPOSER.contains(".border_color(session_edge(p.panel_hi))"),
        "the suggested options carry the edge"
    );
}
