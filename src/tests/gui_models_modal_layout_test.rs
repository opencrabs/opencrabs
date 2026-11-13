//! The Models card must contain its own list (#2918).
//!
//! GPUI's default `display` is Block, so a child asking for `flex_1` under a
//! block parent sizes against its content instead of the row it sits in. The
//! body row is a fixed 440, and a 500-model catalogue is far taller than that,
//! so the list painted past the card's bottom edge and across its footer. The
//! window has no headless harness, so containment is checked in the render
//! source, the same way the overlay layer is (#2331).

const MODAL: &str = include_str!("../gui/models_modal.rs");
const PICKER: &str = include_str!("../gui/model_picker.rs");

/// Only the body builder, so a guard cannot be satisfied by a `overflow_hidden`
/// somewhere else in the file.
fn body_row() -> &'static str {
    let (_, from_body) = MODAL
        .split_once("let body = div()")
        .expect("the body row is built in models_modal.rs");
    let (row, _) = from_body
        .split_once("let footer")
        .expect("the footer follows the body row");
    row
}

#[test]
fn the_body_row_clips_what_it_holds() {
    assert!(
        body_row().contains(".overflow_hidden()"),
        "the fixed-height body row must clip, or a long catalogue paints \
         outside the card (#2918)"
    );
}

#[test]
fn the_picker_body_sits_in_a_column_flex_parent() {
    let row = body_row();
    let (before, _) = row
        .split_once(".child(picker.body)")
        .expect("the body row attaches the picker");
    let (_, wrapper) = before
        .rsplit_once(".child(")
        .expect("the picker is a child of some element");
    assert!(
        wrapper.contains(".flex()") && wrapper.contains(".flex_col()"),
        "the picker's parent must be a column flex container, or `flex_1` \
         resolves against the content instead of the row (#2918): {wrapper}"
    );
    assert!(
        wrapper.contains(".min_h_0()"),
        "a flex column that must scroll needs min_h_0 to shrink below its \
         content: {wrapper}"
    );
}

#[test]
fn the_results_list_is_the_scroll_container() {
    assert!(
        PICKER.contains("results = results.flex_1().min_h_0().overflow_y_scroll();"),
        "the model list must own the scroll, not the card"
    );
}

#[test]
fn the_window_is_sized_for_the_fixed_body() {
    // 24 rows is roughly 960px of content inside a 440 body. The list then
    // scrolls, but the selection keeps walking past the visible area because
    // nothing scrolls it back, so the marked row ends up off screen (#2918).
    // The window has to match what the body shows so `visible_window` can
    // slide it instead.
    assert!(
        MODAL.contains("max_models: 8,"),
        "the picker window must fit the padded 440 body, or the selection \
         leaves the visible area (#2918)"
    );
}

#[test]
fn the_two_panels_and_rows_share_the_concentric_radius_family() {
    assert_eq!(
        body_row()
            .matches(".rounded(px(concentric_radius(6.0)))")
            .count(),
        2,
        "the provider and result panels must have the same 18px radius (#2921)"
    );
    assert!(
        PICKER.contains(".rounded(px(concentric_radius(12.0)))"),
        "search and model rows use the nested 12px radius (#2921)"
    );
}
