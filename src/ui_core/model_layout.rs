//! Responsive geometry for the shared provider/model picker.
//!
//! GPUI paints in logical pixels while rem-based text and spacing grow with
//! the window zoom. These pure decisions keep the onboarding rail and footer
//! from claiming space the content no longer has.

/// Space reserved for the picker itself before an onboarding rail may appear.
const MIN_CONTENT_REM: f32 = 20.0;
/// The rail's normal width, expressed in rem so its labels scale with zoom.
const ONBOARDING_RAIL_REM: f32 = 13.5;
/// Horizontal padding on the right pane: `px_6` on both sides.
const PANE_PADDING_REM: f32 = 3.0;
/// Below this content width the two footer groups stack instead of colliding.
const FOOTER_ROW_MIN_REM: f32 = 44.0;
/// Below this picker width the provider name moves under the model id.
const MODEL_COLUMNS_MIN_REM: f32 = 36.0;
const PROVIDER_LABEL_REM: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterLayout {
    Row,
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRowLayout {
    Columns,
    Stacked,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelLayout {
    /// Width left for the middle pane after the outer session rail.
    pub pane_width: f32,
    /// Zero hides the onboarding rail before it can paint over the picker.
    pub onboarding_rail_width: f32,
    pub footer: FooterLayout,
    pub model_row: ModelRowLayout,
    /// Zero in stacked mode; bounded to one third of the picker in columns.
    pub provider_label_width: f32,
}

/// Plan one picker frame. `pane_width` is the live middle-pane width, `rem`
/// is the current zoomed rem size, and `base_rem` is the unzoomed rem size.
pub fn model_layout(
    pane_width: f32,
    rem: f32,
    base_rem: f32,
    show_onboarding_rail: bool,
) -> ModelLayout {
    let pane_width = if pane_width.is_finite() {
        pane_width.max(0.0)
    } else {
        0.0
    };
    let scale = if rem.is_finite() && base_rem.is_finite() && base_rem > 0.0 {
        (rem / base_rem).max(0.01)
    } else {
        1.0
    };
    let content_width = (pane_width - PANE_PADDING_REM * rem).max(0.0);
    let rail = ONBOARDING_RAIL_REM * rem;
    let minimum = MIN_CONTENT_REM * rem;
    let onboarding_rail_width = if show_onboarding_rail && content_width >= rail + minimum {
        rail
    } else {
        0.0
    };
    let footer = if content_width >= FOOTER_ROW_MIN_REM * scale * base_rem {
        FooterLayout::Row
    } else {
        FooterLayout::Column
    };
    let picker_width = (content_width - onboarding_rail_width).max(0.0);
    let model_row = if picker_width >= MODEL_COLUMNS_MIN_REM * rem {
        ModelRowLayout::Columns
    } else {
        ModelRowLayout::Stacked
    };
    let provider_label_width = if model_row == ModelRowLayout::Columns {
        (PROVIDER_LABEL_REM * rem).min(picker_width / 3.0)
    } else {
        0.0
    };
    ModelLayout {
        pane_width,
        onboarding_rail_width,
        footer,
        model_row,
        provider_label_width,
    }
}
