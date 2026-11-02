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

/// The split chord for a keystroke with control and shift held, or None.
/// The shifted glyph arrives as `key_char` on macOS (`|` and `_`), and as the
/// base key on other layouts (`\` and `-`), so both are accepted.
pub fn chord_layout(
    key: &str,
    key_char: Option<&str>,
    control: bool,
    shift: bool,
) -> Option<Layout> {
    if !(control && shift) {
        return None;
    }
    match key_char.unwrap_or(key) {
        "|" => Some(Layout::Side),
        "_" => Some(Layout::Stack),
        _ => match key {
            "\\" => Some(Layout::Side),
            "-" => Some(Layout::Stack),
            _ => None,
        },
    }
}
