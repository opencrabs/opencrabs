//! The chat header and the message input read as surfaces of their own (#2357).
//! Both sit one step above the chat in either theme: the header has a panel
//! background and a divider, and the input has a panel background and a
//! shadow. The window has no headless harness, so these checks read source.

const GUI_MAIN: &str = include_str!("../gui/main.rs");
const EDITOR: &str = include_str!("../gui/editor.rs");

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

/// The leaf arm of the pane walk, from its panel down to the split arm
/// (#2472, #2581).
fn leaf_arm() -> &'static str {
    GUI_MAIN
        .split("let mut node = div()")
        .nth(1)
        .expect("a leaf is built in pane_element")
        .split("PaneTree::Split {")
        .next()
        .expect("the leaf arm comes before the split arm")
}

#[test]
fn a_split_panel_carries_its_own_composer_and_footer() {
    let leaf = leaf_arm();
    assert!(
        leaf.contains(".child(body)"),
        "the transcript still opens the panel"
    );
    assert!(
        leaf.contains("if let Some(chrome) = ctx.chrome.remove(session)"),
        "the panel draws its own composer and plan, not the window's"
    );
    let chrome_at = leaf.find("node.child(chrome)").expect("the panel takes it");
    let footer_at = leaf
        .find("node.child(pane_footer(")
        .expect("the panel draws its own footer row");
    assert!(
        chrome_at < footer_at,
        "the composer sits above the footer, as it does unsplit"
    );
}
