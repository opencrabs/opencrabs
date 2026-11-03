//! Split panes, stage one (#2438): the chat shows two sessions side by side
//! (`ctrl+shift+|`) or stacked (`ctrl+shift+_`). This module holds the chord
//! and the layout rule; the window owns the panes and their transcripts.

/// How the two panes sit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Side by side, a vertical divider: `ctrl+shift+|`.
    Side,
    /// One above the other, a horizontal divider: `ctrl+shift+_`.
    Stack,
}

impl Layout {
    /// The label the menu and the dialog print for this layout.
    pub fn label(self) -> &'static str {
        match self {
            Layout::Side => "|",
            Layout::Stack => "_",
        }
    }

    /// Whether the second pane is laid out as a column rather than a row.
    pub fn is_stacked(self) -> bool {
        matches!(self, Layout::Stack)
    }
}

/// The narrowest a pane may be dragged to, in px (#2476). Below this a pane
/// stops being a transcript and becomes a strip, so the drag stops here.
pub const MIN_PANE_PX: f32 = 240.0;

/// The share the panes start from: an even split (#2473, #2476).
pub const EVEN_SHARE: f32 = 0.5;

/// The first pane's share of the pane area after a drag step (#2476).
///
/// `travelled` is how far the pointer moved along the drag axis in px and
/// `span` is the pane area along that axis. The result is clamped so neither
/// pane can be dragged under [`MIN_PANE_PX`], and an area too small to hold
/// two panes at that minimum keeps the split where it was: collapsing a side
/// would be the one outcome a resize must never produce.
pub fn share_after_drag(from_share: f32, travelled: f32, span: f32) -> f32 {
    if !span.is_finite() || span <= MIN_PANE_PX * 2.0 {
        return from_share;
    }
    let floor = MIN_PANE_PX / span;
    (from_share + travelled / span).clamp(floor, 1.0 - floor)
}

/// The split chord for a keystroke holding control, or None (#2438, #2471).
///
/// The shifted glyph arrives as `key_char` on most layouts (`|` and `_`), and
/// as the base key with shift held on others (`\` and `-`), so both are
/// accepted. The glyph alone is enough: macOS folds shift into it and reports
/// `shift = false` for a keystroke that plainly typed `|` (`gpui`
/// `platform/mac/events.rs`), so demanding `control && shift` here meant the
/// chord could never fire on the window it was written for (#2471).
pub fn chord_layout(
    key: &str,
    key_char: Option<&str>,
    control: bool,
    shift: bool,
) -> Option<Layout> {
    if !control {
        return None;
    }
    match key_char.unwrap_or(key) {
        "|" => return Some(Layout::Side),
        "_" => return Some(Layout::Stack),
        _ => {}
    }
    // The base key still needs shift: ctrl+- is zoom out and a bare ctrl+\ is
    // nothing, so neither may open a pane.
    if !shift {
        return None;
    }
    match key {
        "\\" => Some(Layout::Side),
        "-" => Some(Layout::Stack),
        _ => None,
    }
}
