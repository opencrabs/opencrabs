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

#[test]
fn the_right_pane_uses_the_full_window_height() {
    let start = GUI_MAIN
        .find("let mut right = div()")
        .expect("the right pane is built in render");
    let pane = GUI_MAIN
        .get(start..start + 900)
        .expect("the right pane is in range");
    assert!(
        pane.contains(".h_full()"),
        "a tall window must stretch the pane so the footer stays below the body"
    );
}

#[test]
fn onboarding_footer_hints_keep_their_horizontal_width() {
    let start = GUI_MAIN
        .find("let horizontal_hints =")
        .expect("onboarding footer has a responsive layout");
    let footer = GUI_MAIN
        .get(start..start + 2200)
        .expect("onboarding footer is in range");
    assert!(footer.contains(".when(horizontal_hints, |d| d.flex_1())"));
    assert!(footer.contains(".when(horizontal_hints, |d| d.flex_none())"));
    assert_eq!(
        footer
            .matches(".when(!horizontal_hints, |d| d.w_full())")
            .count(),
        2,
        "stacked hints use full rows instead of shrinking to one character"
    );
}
