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
    let header = &GUI_MAIN[start..start + 400];
    assert!(header.contains(".bg(rgb(palette::current().panel))"));
    assert!(header.contains(".border_b_1()"));
}

#[test]
fn the_input_has_a_panel_background_and_a_shadow() {
    let start = EDITOR
        .find(".on_mouse_move(cx.listener(Self::on_mouse_move))")
        .expect("the editor root is built in render");
    let root = &EDITOR[start..start + 400];
    assert!(root.contains(".bg(rgb(palette::current().panel))"));
    assert!(root.contains(".shadow_sm()"));
}
