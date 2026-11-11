//! The chat header and the message input read as surfaces of their own (#2357).
//! Both sit one step above the chat in either theme: the header has a panel
//! background and a divider, and the input has a panel background and a
//! shadow. The window has no headless harness, so these checks read source.

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const EDITOR: &str = include_str!("../gui/editor.rs");

/// The pane's own render (#2805): the frame a leaf draws for itself.
const PANE_VIEW: &str = include_str!("../gui/pane_view.rs");

#[test]
fn the_header_has_a_panel_background_and_a_divider() {
    let start = GUI_MAIN
        .find("let chrome = div()")
        .expect("the chat header is built in render");
    let header = GUI_MAIN
        .get(start..start + 400)
        .expect("the header block is in range");
    assert!(header.contains(".bg(rgb(palette::current().panel))"));
    assert!(header.contains(".border_b_1()"));
}

#[test]
fn the_input_has_a_panel_background_and_a_shadow() {
    let start = EDITOR
        .find(".on_mouse_move(cx.listener(Self::on_mouse_move))")
        .expect("the editor root is built in render");
    let root = EDITOR
        .get(start..start + 400)
        .expect("the editor root is in range");
    // The panel fill comes from the glass helper since #2497; a plain
    // `bg(rgb(..))` is the same surface before that change.
    assert!(
        root.contains(".bg(glass_fill(palette::current().panel))")
            || root.contains(".bg(rgb(palette::current().panel))"),
        "the input sits on the panel surface, however that fill is mixed"
    );
    assert!(root.contains(".shadow_sm()"));
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
    assert!(
        leaf.contains(".flex_col().flex_none().min_w_0().child(chrome)"),
        "the chrome keeps its own height inside the pane (#2624)"
    );
    let footer_at = leaf
        .find("node.child(pane_footer(")
        .expect("the panel draws its own footer row");
    assert!(
        chrome_at < footer_at,
        "the composer sits above the footer, as it does unsplit"
    );
}
