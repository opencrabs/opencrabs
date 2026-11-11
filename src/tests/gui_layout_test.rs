//! The selected session keeps its highlight under the pointer, and the chat pane
//! uses less top and bottom padding (#2359). The window has no headless
//! harness, so these checks read source.

const SESSIONS: &str = include_str!("../gui/sessions.rs");
const GUI_MAIN: &str = include_str!("../gui/main.rs");
const MODEL_PICKER: &str = include_str!("../gui/model_picker.rs");

#[test]
fn the_hover_tint_is_not_applied_to_the_selected_row() {
    // #2792 removed the focus wash: every card paints the same panel, so the
    // gate is selection alone and the pointer never paints over the open row.
    assert!(
        SESSIONS.contains(".when(!selected, |d| d.hover(")
            && SESSIONS.contains("d.hover(|d| d.bg(rgb(palette::current().bg)))"),
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

#[test]
fn stacked_model_rows_use_flexible_text_rows() {
    let start = MODEL_PICKER
        .find("ModelRowLayout::Stacked =>")
        .expect("the shared picker has a stacked row layout");
    let row = MODEL_PICKER
        .get(start..start + 1_800)
        .expect("the stacked row is in range");
    assert_eq!(
        row.matches(".flex_row()").count(),
        2,
        "the model id and provider each get a horizontal text row"
    );
    for label in ["text(row.model.clone()", "text(row.provider_name.clone()"] {
        let start = row.find(label).expect("the stacked label is present");
        let label = row
            .get(start..start + 260)
            .expect("the stacked label is in range");
        assert!(label.contains(".flex_1()"));
        assert!(label.contains(".min_w_0()"));
        assert!(label.contains(".truncate()"));
        assert!(
            !label.contains(".w_full()"),
            "flex rows size the text; percentage-width nested text collapses"
        );
    }
}
