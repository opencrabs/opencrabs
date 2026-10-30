//! Usage dashboard geometry (#2073).
//!
//! The dashboard is a full screen now, so it owns the whole content area: a
//! pinned summary bar, the card grid, and a pinned key hint row. The grid
//! reflows with the terminal instead of being pinned to a 2x2.
//!
//! Everything here is pure. Geometry lives in functions that take `u16` and
//! return rects, so the breakpoints and the floor are testable with no
//! terminal, the same way `tui::onboarding_layout` does it.

use crate::tui::render::responsive;
use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Cards in the dashboard, in focus order: 0 daily, 1 project, 2 model,
/// 3 tools, 4 activity, 5 cache.
///
/// The count is a fact of the view model's data, so it is defined once in
/// `ui_core::usage::view_model` and re-exported here: the grid shape follows
/// the model, never the other way round.
pub use crate::ui_core::usage::view_model::USAGE_CARD_COUNT as CARD_COUNT;

/// Narrowest content area the cards still read at. Below this the text in
/// the cards is wider than the cards.
pub const MIN_WIDTH: u16 = 40;

/// Rows the summary bar takes.
const SUMMARY_ROWS: u16 = 2;

/// Rows the key hint footer takes.
const FOOTER_ROWS: u16 = 1;

/// Fewest rows a single row of cards is allowed. A card is a border plus at
/// least one line of content, so two rows is a card with nothing in it.
const MIN_CARD_ROWS: u16 = 3;

/// Card rectangles for one draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageLayout {
    /// Summary bar, full width, pinned above the grid.
    pub summary: Rect,
    /// One rect per card, indexed by focus order.
    pub cards: [Rect; CARD_COUNT],
    /// Key hint row, pinned below the grid.
    pub footer: Rect,
    /// True when the area is too small for the cards. Every rect is then
    /// empty and the caller draws the message instead of the grid.
    pub too_small: bool,
}

/// Rows the grid needs to show `cols` columns of cards.
pub fn required_height(cols: usize) -> u16 {
    let rows = CARD_COUNT.div_ceil(cols.max(1)) as u16;
    SUMMARY_ROWS + rows * MIN_CARD_ROWS + FOOTER_ROWS
}

/// Shortest area that still shows the cards at this width. Narrow terminals
/// need more rows, because the cards stack instead of sitting side by side.
pub fn min_height(width: u16) -> u16 {
    required_height(responsive::grid_cols(width))
}

/// Does this area have room for the cards?
pub fn fits(width: u16, height: u16) -> bool {
    width >= MIN_WIDTH && height >= min_height(width)
}

/// Compute the dashboard geometry for `area`.
///
/// Below the floor this returns empty rects and `too_small: true`. The
/// alternative, and what the centered dialog used to do, is draw the cards
/// into widths narrower than their own text and let the clipping look like
/// a bug.
pub fn compute(area: Rect) -> UsageLayout {
    let empty = Rect::new(area.x, area.y, 0, 0);
    if !fits(area.width, area.height) {
        return UsageLayout {
            summary: empty,
            cards: [empty; CARD_COUNT],
            footer: empty,
            too_small: true,
        };
    }

    let bands = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(SUMMARY_ROWS),
            Constraint::Min(MIN_CARD_ROWS),
            Constraint::Length(FOOTER_ROWS),
        ])
        .split(area);
    let grid = bands[1];

    let cols = responsive::grid_cols(grid.width);
    let rows = CARD_COUNT.div_ceil(cols);
    let row_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Ratio(1, rows as u32); rows])
        .split(grid);

    let mut cards = [empty; CARD_COUNT];
    for (i, slot) in cards.iter_mut().enumerate() {
        let col_areas = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Ratio(1, cols as u32); cols])
            .split(row_areas[i / cols]);
        *slot = col_areas[i % cols];
    }

    UsageLayout {
        summary: bands[0],
        cards,
        footer: bands[2],
        too_small: false,
    }
}
