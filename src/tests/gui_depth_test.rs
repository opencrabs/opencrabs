//! The chat header and the message input read as surfaces of their own (#2357).
//! Both sit one step above the chat in either theme: the header has a panel
//! background and a divider, and the input has a panel background and a
//! shadow. The window has no headless harness, so these checks read source.

const GUI_MAIN: &str = concat!(
    include_str!("../gui/main.rs"),
    include_str!("../gui/app.rs"),
    include_str!("../gui/dashboard_render.rs"),
    include_str!("../gui/dashboard_state.rs"),
);
const EDITOR: &str = include_str!("../gui/editor.rs");

/// The pane's own render (#2805): the frame a leaf draws for itself.
const PANE_VIEW: &str = include_str!("../gui/pane_view.rs");

#[test]
fn the_header_has_a_panel_background_and_a_divider() {
    // The chain, not a byte window: the header's own drag handle (#3014) and
    // its comments pushed the fill past a fixed slice.
    let start = GUI_MAIN
        .find("let chrome = ")
        .expect("the chat header is built in render");
    let header = GUI_MAIN
        .get(start..)
        .expect("the header block is in range")
        .split(".child(icon::banner_logo())")
        .next()
        .expect("the header chain ends at its logo");
    assert!(header.contains(".bg(rgb(palette::current().panel))"));
    assert!(header.contains(".border_b_1()"));
}

#[test]
fn the_input_has_a_panel_background_and_a_hairline() {
    // The chain, not a byte window: the dock comment (#2987) pushed the fill
    // past the 400 bytes a fixed slice used to read.
    let start = EDITOR
        .find("let root = div()")
        .expect("the editor root is built in render");
    let root = EDITOR
        .get(start..)
        .expect("the root block is in range")
        .split(".child(body);")
        .next()
        .expect("the root chain ends at its body");
    // The panel fill comes from the glass helper since #2497; a plain
    // `bg(rgb(..))` is the same surface before that change.
    assert!(
        root.contains(".bg(glass_fill(palette::current().panel))")
            || root.contains(".bg(rgb(palette::current().panel))"),
        "the input sits on the panel surface, however that fill is mixed"
    );
    // Flat since #3112: the shadow was what made the composer read as a card
    // floating inside the pane rather than the pane's own bottom edge. The
    // hairline on top is the only separation it needs.
    assert!(
        !root.contains(".shadow_sm()"),
        "the composer floats again"
    );
    assert!(
        root.contains(".border_t(px(1.))"),
        "the composer lost the hairline that is its only seam"
    );
}

/// One leaf's frame, from its box down to its status row (#2472, #2581,
/// #2805): a pane draws it in its own render, and the chat leaf's is built by
/// the same function.
fn leaf_arm() -> &'static str {
    PANE_VIEW
        .split_once("let mut node = div()")
        .map(|(_, leaf)| leaf)
        .expect("a leaf is built in leaf_box")
}

#[test]
fn a_split_panel_carries_its_own_composer_and_footer() {
    let leaf = leaf_arm();
    assert!(
        leaf.contains(".child(body)"),
        "the transcript still opens the panel"
    );
    assert!(
        leaf.contains("if let Some(chrome) = chrome"),
        "the panel draws its own composer and plan, not the window's"
    );
    let chrome_at = leaf
        .find(".child(chrome)")
        .expect("the panel takes its own chrome");
    // The stack under the transcript may shrink, and it clips: a plan card,
    // a queue and a set of offers that together outgrow a short pane are cut
    // inside the pane instead of being painted over its footer and off the
    // window (#2624, #2980).
    let chrome_box = leaf
        .split("if let Some(chrome) = chrome")
        .nth(1)
        .expect("the panel takes its own chrome");
    assert!(
        chrome_box.contains(".flex_col()")
            && chrome_box.contains(".flex_shrink()")
            && chrome_box.contains(".min_h_0()")
            && chrome_box.contains(".overflow_hidden()"),
        "the chrome stack gives up height and clips what does not fit (#2980)"
    );
    let footer_at = leaf
        .find("node.child(pane_footer(")
        .expect("the panel draws its own footer row");
    assert!(
        chrome_at < footer_at,
        "the composer sits above the footer, as it does unsplit"
    );
}
