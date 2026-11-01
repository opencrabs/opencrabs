//! The selected session keeps its highlight under the pointer, and the chat pane
//! uses less top and bottom padding (#2359). The window has no headless
//! harness, so these checks read source.

const SESSIONS: &str = include_str!("../gui/sessions.rs");
const GUI_MAIN: &str = include_str!("../gui/main.rs");

#[test]
fn the_hover_tint_is_not_applied_to_the_selected_row() {
    assert!(
        SESSIONS.contains(".when(!selected, |d| d.hover("),
        "the hover tint applies only to rows that are not selected"
    );
}

#[test]
fn the_chat_pane_has_less_top_and_bottom_padding_than_24px_on_every_side() {
    let start = GUI_MAIN
        .find("let mut right = div()")
        .expect("the chat pane is built in render");
    let pane = GUI_MAIN
        .get(start..start + 900)
        .expect("the chat pane is in range");
    assert!(pane.contains(".pt_3()"));
    assert!(pane.contains(".pb_4()"));
    assert!(!pane.contains(".p_6()"));
}
