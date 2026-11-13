//! The window keeps up with its own conversation (#2685, #2686, #2688).
//!
//! The window has no headless harness, so these checks read source. Each rule
//! below was broken once: a transcript that stopped following its bottom, a
//! topic strip that named the chat leaf over every pane, and a markdown render
//! that re-parsed the whole conversation on every frame.

use crate::tests::gui_dashboard_source::DASHBOARD;

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");

/// The pane's own render (#2805): where a leaf asks the window for its own
/// topic row instead of reading one the walk handed it.
const PANE_VIEW: &str = include_str!("../gui/pane_view.rs");

/// The render pass, up to the rail: the only place the window holds a real
/// viewport to measure a scroll offset from.
fn render_head() -> &'static str {
    GUI_MAIN
        .split("    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {")
        .nth(1)
        .expect("the window renders through one render pass")
        .split("let rail = ")
        .next()
        .expect("the rail is built later in the pass")
}

#[test]
fn every_frame_follows_the_chat_transcript() {
    // A one shot snap is consumed by the next layout, so anything that grows
    // afterwards is out of view. The follow is re-judged every frame instead.
    let head = render_head();
    assert!(
        head.contains("self.follow_chat()"),
        "render asks the chat transcript to follow its bottom"
    );
    assert!(
        head.contains("self.follow_pane(session, cx)"),
        "render re-judges every pane's follow too"
    );
}

#[test]
fn the_follow_is_judged_from_the_readers_own_place() {
    // A reader parked in history must not be yanked down by the next event
    // batch. The decision lives in ui_core so it is testable without a window.
    let src = include_str!("../ui_core/scroll.rs");
    assert!(
        src.contains("pub fn at_bottom("),
        "the bottom test is pure arithmetic, not a window call"
    );
    assert!(
        src.contains("pub struct Follow"),
        "the sticky state has a name of its own"
    );
    // Growth moves the max offset and leaves the offset alone, so only a
    // reader who moved up may release the follow.
    assert!(
        src.contains("let moved_up ="),
        "a release is a move up, never a landed row"
    );
}

#[test]
fn a_pane_draws_its_own_topic_strip() {
    // The strip used to be one window row built from the chat leaf (#2686,
    // #2687), so it named the wrong conversation over every other pane.
    assert!(
        PANE_VIEW.contains("win.pane_topics(session, cx)"),
        "each pane asks for its own topic row, keyed by its own session"
    );
    assert!(
        PANE_VIEW.contains("let topics = window.update(cx,"),
        "the row is rebuilt in the pane's own render, so a pane that only redraws itself keeps it"
    );
    assert!(
        DASHBOARD.contains("fn topic_tabs(&self, pane: Option<Uuid>"),
        "the strip resolves a session instead of reading the chat leaf"
    );
    // The window row stands down only for the unsplit window.
    assert!(
        GUI_MAIN.contains("&& !split_draws_its_own"),
        "the window row is drawn only when the chat leaf is the only surface"
    );
}

#[test]
fn a_topic_click_moves_the_pane_that_drew_it() {
    // Opening a chip through the window door moved the chat leaf, which is the
    // conversation the user was not reading.
    assert!(
        DASHBOARD.contains("fn open_topic_in_pane(&mut self, pane: Uuid, id: Uuid"),
        "a pane-local door exists"
    );
    assert!(
        DASHBOARD.contains("this.open_topic_in_pane(leaf, id, cx)"),
        "a pane's chip goes through it"
    );
    // A new topic hangs under the focused leaf, not the chat leaf.
    assert!(
        DASHBOARD.contains("fn topic_parent(&self) -> Option<Uuid>"),
        "the parent of a new topic is resolved from the focus"
    );
}

#[test]
fn markdown_is_parsed_once_per_source() {
    // `md_block` ran the parser over every visible answer on every frame,
    // which is 4 MB of markdown twenty times a second on a long session.
    assert!(
        TRANSCRIPT.contains("fn md_blocks(src: &str) -> Rc<Vec<markdown::MdBlock>>"),
        "the parse is behind a cache"
    );
    assert!(
        TRANSCRIPT.contains("MD_CACHE.with(|c| c.borrow_mut().get(key))"),
        "a repeated source is a cache hit"
    );
    assert!(
        TRANSCRIPT.contains("let parsed = md_blocks(src);"),
        "the renderer asks the cache, never the parser"
    );
    assert!(
        !TRANSCRIPT.contains("let blocks = markdown::blocks(src);"),
        "no render path reaches the parser directly"
    );
}

/// Split panes passed no fold handlers, so their settled and live cards were
/// painted with no click listener. Each leaf must mutate its own transcript,
/// not the main chat's, and a copy-drag release must not toggle the card (#2855).
#[test]
fn a_pane_expands_its_own_settled_and_live_cards() {
    let render = PANE_VIEW
        .split("impl Render for PaneView {")
        .nth(1)
        .expect("the pane render implementation")
        .split("/// Everything one leaf's frame draws")
        .next()
        .expect("the pane render ends before the leaf box");
    for required in [
        "for anchor in self.transcript.anchors()",
        "this.transcript.toggle_turn(&key)",
        "this.transcript.live_expanded = !this.transcript.live_expanded",
        ".magnitude() > 2.0",
    ] {
        assert!(render.contains(required), "missing pane behavior: {required}");
    }

    let view = render
        .split("let body = transcript::view(")
        .nth(1)
        .expect("the pane builds its transcript")
        .split("// The topic strip")
        .next()
        .expect("the transcript call ends before the topic strip");
    assert!(
        view.contains("fold_handlers,"),
        "settled cards receive the pane-local handlers"
    );
    assert!(
        view.contains("live_click: Some(live_click),"),
        "the live card receives its own click handler"
    );
    assert!(
        !view.contains("HashMap::new(),"),
        "an empty fold-handler map leaves every settled card inert"
    );

    let nav = include_str!("../gui/dashboard_nav.rs");
    assert!(
        nav.contains("self.chat_state.transcript.toggle_turn(anchor);"),
        "the main chat and split panes share the same state transition"
    );
}
