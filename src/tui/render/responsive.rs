//! Shared terminal breakpoints.
//!
//! Every screen that reflows has to agree on where the breaks are. If two
//! screens each invent their own numbers, the same terminal width produces
//! a two column grid on one and a three column grid on the other, and a
//! user resizing the window sees the app disagree with itself.
//!
//! So there is one table, here, and every responsive screen reads it.

/// Blank columns kept between content and the pane edge on every full-bleed
/// screen (#2175).
///
/// #2162 removed the borders, and with them the right padding a border used
/// to supply for free. Only the left gutter was re-added, so every full-bleed
/// row ran flush to the last column and the right side had no breathing
/// space. Both sides inset by this many columns now, in the rows and in the
/// footer label alike, so one screen cannot disagree with another.
pub const EDGE_INSET: u16 = 2;

/// Width at which a card grid gets three columns.
pub const GRID_THREE_COL_MIN: u16 = 90;

/// Width at which a card grid gets two columns.
pub const GRID_TWO_COL_MIN: u16 = 56;

/// Columns a card grid should use at this width: three when there is room
/// for them, two when there is some, one when there is not.
pub fn grid_cols(width: u16) -> usize {
    if width >= GRID_THREE_COL_MIN {
        3
    } else if width >= GRID_TWO_COL_MIN {
        2
    } else {
        1
    }
}

/// Width at which Mission Control gets its two column split.
pub const MC_SPLIT_MIN: u16 = 100;

/// Width at which that split widens to the wide 40/60 ratio. Below this and
/// above [`MC_SPLIT_MIN`] the left column gets 45% instead of 40%.
pub const MC_WIDE_MIN: u16 = 120;

/// Does Mission Control have room for two columns at this width? Below this
/// it shows one panel at a time, filling the area.
pub fn mc_two_column(width: u16) -> bool {
    width >= MC_SPLIT_MIN
}

/// How much of a session row's metadata fits at the current width (#2162).
///
/// A session row is always exactly one terminal row. Which badges it can
/// carry is decided here, once, so the row and any future consumer of the
/// same data cannot disagree about what a 110 column terminal shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRowTier {
    /// Under 100 columns: name, clock time, status glyph.
    Narrow,
    /// 100 to 119: full timestamp and the cumulative token count.
    Medium,
    /// 120 to 149: plus context usage and the provider/model pair.
    Wide,
    /// 150 and up: plus the project badge, working directory and branch.
    ExtraWide,
}

/// Width at which a session row grows from clock-only to full timestamp.
pub const SESSION_ROW_MEDIUM_MIN: u16 = 100;

/// Width at which a session row adds context usage and provider/model.
pub const SESSION_ROW_WIDE_MIN: u16 = 120;

/// Width at which a session row adds project, working directory and branch.
pub const SESSION_ROW_EXTRA_MIN: u16 = 150;

/// Which badge tier a session row uses at `width` columns.
pub fn session_row_tier(width: u16) -> SessionRowTier {
    if width >= SESSION_ROW_EXTRA_MIN {
        SessionRowTier::ExtraWide
    } else if width >= SESSION_ROW_WIDE_MIN {
        SessionRowTier::Wide
    } else if width >= SESSION_ROW_MEDIUM_MIN {
        SessionRowTier::Medium
    } else {
        SessionRowTier::Narrow
    }
}
