//! Where the topic tab row sits in the chat (#2402). The choice is saved as
//! `[gui] topic_tabs`; the leading button of the row cycles it, as Telegram's
//! does.

/// One side of the chat the topic row can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Left,
    Bottom,
    Right,
}

impl Edge {
    /// The saved word for this side, the value written to `[gui] topic_tabs`.
    pub fn as_str(self) -> &'static str {
        match self {
            Edge::Top => "top",
            Edge::Left => "left",
            Edge::Bottom => "bottom",
            Edge::Right => "right",
        }
    }

    /// The saved word read back. Unset or unknown is the top row.
    pub fn from_config(word: Option<&str>) -> Edge {
        match word {
            Some("left") => Edge::Left,
            Some("bottom") => Edge::Bottom,
            Some("right") => Edge::Right,
            _ => Edge::Top,
        }
    }

    /// The next side when the leading button is pressed: top, left, bottom,
    /// right, then top again.
    pub fn next(self) -> Edge {
        match self {
            Edge::Top => Edge::Left,
            Edge::Left => Edge::Bottom,
            Edge::Bottom => Edge::Right,
            Edge::Right => Edge::Top,
        }
    }

    /// Whether the row runs down the side of the chat rather than across it.
    pub fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}
