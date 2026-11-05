use crate::ui_core::model_layout::{FooterLayout, ModelRowLayout, model_layout};

const BASE_REM: f32 = 14.0;
const MIN_CONTENT_REM: f32 = 20.0;
const PANE_PADDING_REM: f32 = 3.0;

#[test]
fn model_layout_keeps_rail_and_content_separate_at_normal_size() {
    let layout = model_layout(708.0, BASE_REM, BASE_REM, true);
    assert_eq!(layout.onboarding_rail_width, 189.0);
    assert_eq!(layout.footer, FooterLayout::Row);
    assert_eq!(layout.model_row, ModelRowLayout::Stacked);
    assert_eq!(layout.provider_label_width, 0.0);
    let remaining = layout.pane_width - PANE_PADDING_REM * BASE_REM - layout.onboarding_rail_width;
    assert!(remaining >= MIN_CONTENT_REM * BASE_REM);
}

#[test]
fn model_layout_drops_rail_before_content_can_overlap_at_zoom() {
    let normal = model_layout(708.0, BASE_REM, BASE_REM, true);
    let zoomed = model_layout(572.0, BASE_REM * 1.5, BASE_REM, true);
    assert!(normal.onboarding_rail_width > 0.0);
    assert_eq!(zoomed.onboarding_rail_width, 0.0);
    assert_eq!(zoomed.footer, FooterLayout::Column);
    assert_eq!(zoomed.model_row, ModelRowLayout::Stacked);
    assert_eq!(zoomed.provider_label_width, 0.0);
}

#[test]
fn model_layout_stacks_footer_before_hints_can_collide() {
    assert_eq!(
        model_layout(700.0, BASE_REM, BASE_REM, false).footer,
        FooterLayout::Row
    );
    assert_eq!(
        model_layout(520.0, BASE_REM, BASE_REM, false).footer,
        FooterLayout::Column
    );
}

#[test]
fn model_layout_standalone_never_reserves_onboarding_rail() {
    let layout = model_layout(1_400.0, BASE_REM, BASE_REM, false);
    assert_eq!(layout.onboarding_rail_width, 0.0);
    assert_eq!(layout.model_row, ModelRowLayout::Columns);
    assert!(layout.provider_label_width > 0.0);
    assert!(layout.provider_label_width <= 12.0 * BASE_REM);
}

#[test]
fn model_layout_handles_narrow_and_invalid_geometry_without_negative_space() {
    for width in [0.0, 100.0, f32::NAN] {
        let layout = model_layout(width, BASE_REM, BASE_REM, true);
        assert!(layout.pane_width >= 0.0);
        assert_eq!(layout.onboarding_rail_width, 0.0);
        assert_eq!(layout.footer, FooterLayout::Column);
    }
}
