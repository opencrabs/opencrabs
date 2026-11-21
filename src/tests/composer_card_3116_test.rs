//! The composer is a card the size of a message (#3116).
//!
//! This file pins the reversal of #3112. That pass took the composer edge to
//! edge, made it flat, and left the surface's own clip to round its bottom
//! corners, on the owner's words "it looks great if it's completely flat end to
//! end". Seen on a single pane it did not hold: a full-bleed band under
//! measure-wide bubbles reads as a slab from another window, and the words in it
//! had to be pushed back onto the conversation's column by hand. The owner sent
//! it back to a card of the messages' own size, matching the plan, user and
//! assistant cards in width AND radius.
//!
//! So the composer is a card again, and the surface behind it stays what #3112
//! made it: square, full height, full width, with no gutter of its own. A card
//! floats ON that ground; the ground is not a card.

/// The desktop window's chat screen.
const RENDER: &str = include_str!("../gui/dashboard_render.rs");
/// The composer itself, which a focused split pane renders bare.
const EDITOR: &str = include_str!("../gui/editor.rs");
/// The dimmed composer a keyboard-less pane draws.
const PANE_CHROME: &str = include_str!("../gui/pane_chrome.rs");

/// The composer's shell, from the element that builds it to the line that puts
/// it in the session.
fn shell() -> &'static str {
    let start = RENDER
        .find("let mut shell = div()")
        .expect("the composer still builds a shell");
    RENDER[start..]
        .split("col = col.child(")
        .next()
        .expect("the shell ends where it joins the column")
}

/// The editor's own chrome, from the dock gate to the field's padding.
fn undocked_chrome() -> &'static str {
    let start = EDITOR
        .find(".when(!self.docked, |d| {")
        .expect("a pane's composer still paints its own surface");
    let tail = &EDITOR[start..];
    tail.get(..tail.find(".py_3()").expect("the field's vertical padding"))
        .expect("the chrome ends on a char boundary")
}

/// The docked composer wears the bubbles' curve and the bubbles' edge.
///
/// The width comes from `session_row`, which the plan card, the queue and this
/// card all go through, so the card's edge lands where a message's edge lands.
/// The curve is `MESSAGE_RADIUS`, the one number the plan, user and assistant
/// cards share: the owner has asked for the composer to match them by name.
#[test]
fn the_docked_composer_is_a_message_sized_card() {
    let body = shell();
    assert!(
        body.contains(".rounded(px(transcript::MESSAGE_RADIUS))"),
        "the composer wears the messages' curve: {body}"
    );
    assert!(
        body.contains(".border_1()"),
        "a card that floats has four sides, not one hairline: {body}"
    );
    assert!(
        body.contains("session_row("),
        "and it is placed by the helper the other cards use: {body}"
    );
    assert!(
        body.contains(".pb_2()"),
        "with the transcript's own step below it, or the bottom corner is cut: {body}"
    );
    assert!(
        !body.contains("central("),
        "a message-sized card's rows pad it, they do not measure the pane: {body}"
    );
}

/// A bare pane's composer keeps the chrome it always had.
///
/// A focused split pane renders the editor with no card around it, so that copy
/// is its own surface: the fill and the hairline are what make it a footer at
/// all. It stays square because the pane around it is a square box, and a curve
/// there would round one corner of a shape that has none.
#[test]
fn a_bare_pane_composer_keeps_its_own_chrome() {
    let body = undocked_chrome();
    assert!(
        body.contains(".border_t(px(1.))"),
        "the pane's footer draws its seam: {body}"
    );
    assert!(
        body.contains(".bg(glass_fill(palette::current().panel))"),
        "and keeps the fill, or the pane loses its footer entirely: {body}"
    );
    assert!(
        !body.contains(".rounded("),
        "with no curve, because the pane it fills is square: {body}"
    );
    assert!(
        !PANE_CHROME.contains(".rounded("),
        "the dim pane's composer matches it, or the pane reshapes on focus loss"
    );
}

/// The ground the card floats on is square and full-bleed.
///
/// #3112 made the session surface the shot: edge to edge, top to bottom, no
/// corner of its own. #3116 kept that and only brought the composer back in
/// from the edges, so the surface still carries no radius for the card to
/// inherit, and the clip stays because one scrolled row painted past the edge
/// before it moved up here.
#[test]
fn the_ground_is_square_and_full_bleed() {
    let start = RENDER
        .find("let mut session = div()")
        .expect("the chat builds a session surface");
    let tail = &RENDER[start..];
    let clip = ".overflow_hidden();";
    let end = tail.find(clip).expect("the surface clips") + clip.len();
    let surface = tail.get(..end).expect("the surface ends on a char boundary");
    assert!(
        !surface.contains(".rounded("),
        "the ground carries no corner, so nothing inherits one: {surface}"
    );
    assert!(
        surface.contains(".w_full()") && surface.contains(".h_full()"),
        "and it fills the pane, so the field ends on the screen's own edge: {surface}"
    );
    assert!(
        surface.contains(".overflow_hidden()"),
        "the clip stays: it is the only element above the transcript"
    );
}

/// The recording takeover is the composer, so it takes the composer's shape.
///
/// A rounded slab inside a square footer was the same mistake one level down:
/// the block is the field's own box while the microphone is open, so it is flat
/// and the pane supplies its edges.
#[test]
fn the_voice_block_takes_the_composer_shape() {
    let start = EDITOR
        .find("fn voice_block(")
        .expect("the recording takeover still exists");
    let tail = &EDITOR[start..];
    let body = tail
        .get(
            ..tail
                .find("fn voice_strip(")
                .expect("the strip follows the block"),
        )
        .expect("the block ends on a char boundary");
    assert!(
        !body.contains(".rounded("),
        "the block is the composer while the microphone is open: {body}"
    );
    assert!(
        body.contains("BLOCK_HEIGHT"),
        "and it is still the composer's own height"
    );
}
