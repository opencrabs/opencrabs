//! The composer is a flat footer, not a card (#3112).
//!
//! The owner's last pass on the session surface: the messages sit ON the
//! session, half a rem inside it, so their sides are visible against the fill.
//! The composer is not one of them. It takes the surface's full width, ends on
//! its bottom edge, and the surface's own clip rounds the only two corners it
//! has. In his words: "it looks great if it's completely flat end to end".
//!
//! That leaves the composer as the one row in the session with no radius at
//! all, which is why the token is gone from `ui_core::card` instead of being
//! left behind for the next person to reach for. These tests pin the absence:
//! the docked shell, the focused pane's footer, the dim pane's footer and the
//! voice block all carry a top hairline and no curve.

/// The desktop window's chat screen.
const RENDER: &str = include_str!("../gui/dashboard_render.rs");
/// The composer itself, which a focused split pane renders bare.
const EDITOR: &str = include_str!("../gui/editor.rs");
/// The dimmed composer a keyboard-less pane draws.
const PANE_CHROME: &str = include_str!("../gui/pane_chrome.rs");
/// The shared card tokens.
const CARD: &str = include_str!("../ui_core/card.rs");

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

/// Nothing left in the tree can give the composer a radius.
///
/// The token is the dangerous part: a shape that no longer exists but still has
/// a name is one import away from coming back, and the next card that reached
/// for it would round the footer again without anyone noticing why.
#[test]
fn the_composer_has_no_radius_left_to_borrow() {
    assert!(
        !CARD.contains("COMPOSER_RADIUS"),
        "the composer's radius token is gone, so nobody reintroduces it"
    );
    for (name, body) in [("docked", shell()), ("focused pane", undocked_chrome())] {
        assert!(
            !body.contains(".rounded("),
            "the {name} composer is flat end to end: {body}"
        );
    }
    assert!(
        !PANE_CHROME.contains(".rounded("),
        "the dim pane's composer is flat too, or the pane reshapes on focus loss"
    );
}

/// The footer's only seam is the hairline along its top.
///
/// A full border would box a surface that is already an edge, and the shadow
/// that used to ride with it was what made the composer read as a card floating
/// inside the pane. The hairline stays: it is the one line that says the field
/// has ended and the conversation starts.
#[test]
fn the_composer_closes_on_a_hairline() {
    assert!(
        shell().contains(".border_t(px(1.))"),
        "the docked footer lost its top hairline"
    );
    assert!(
        !shell().contains(".border_1()"),
        "a full border boxes a surface that is already an edge"
    );
    assert!(
        undocked_chrome().contains(".border_t(px(1.))"),
        "the pane's footer draws the same seam"
    );
    assert!(
        undocked_chrome().contains(".bg(glass_fill(palette::current().panel))"),
        "and keeps the fill, or the pane loses its footer entirely"
    );
}

/// Nothing rounds the footer, because nothing rounds the ground either.
///
/// The first version of this fix left the curve to the session surface: the
/// field dropped its own radius and the surface's clip supplied the two bottom
/// corners. The owner took that one step further in the same pass. The surface
/// is the whole shot now, edge to edge and top to bottom, so there is no corner
/// to inherit: the field ends on the screen's own bottom edge and no curve
/// appears anywhere along it.
#[test]
fn the_session_surface_is_square_so_nothing_rounds_the_footer() {
    let start = RENDER
        .find("let mut session = div()")
        .expect("the chat builds a session surface");
    let tail = &RENDER[start..];
    let clip = ".overflow_hidden();";
    let end = tail.find(clip).expect("the surface clips") + clip.len();
    let surface = tail.get(..end).expect("the surface ends on a char boundary");
    assert!(
        !surface.contains(".rounded("),
        "the ground carries no corner, so the footer inherits none: {surface}"
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
/// and the surface rounds its bottom for it.
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
