//! Mission Control panel layout — pure function, no rendering.
//!
//! The MC takes a single outer rect (full content area) and partitions
//! it into four panels. The shape depends on the terminal width, because
//! the panels are dense and a fixed 40/60 split starves the left column
//! on anything narrow (#2074):
//!
//! ```text
//! wide, 120+                medium, 100-119          narrow, <100
//! ┌───────────┬──────────┐  ┌──────────┬────────┐    ┌───────────────┐
//! │ Inbox 40% │Analytics │  │ Inbox 45%│Analytics│   │ the focused   │
//! │ Activity  │  60%     │  │ Activity │  55%   │    │ panel, full   │
//! │ Schedule  │          │  │ Schedule │        │    │ width         │
//! ├───────────┴──────────┤  ├──────────┴────────┤    ├───────────────┤
//! │ help bar (2 rows)    │  │ help bar (2 rows) │    │ help bar      │
//! └──────────────────────┘  └───────────────────┘    └───────────────┘
//! ```
//!
//! Narrow mode needs no new key: `Tab`, `h` and `l` already switch panels
//! and are already advertised in the footer.
//!
//! `compute_focused` is a pure fn so the geometry is unit-testable without a
//! live `Frame` — the contract is "every panel rect stays inside the
//! outer area, panels don't overlap, the help bar takes 2 rows at the
//! bottom whenever the area is tall enough, and below the floor the
//! layout says so instead of drawing slivers".

use crate::tui::app::mission_control::McPanel;
use crate::tui::render::responsive;
use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Narrowest area Mission Control still renders panels into. Below this the
/// panels are narrower than their own content, so the screen says so
/// instead.
pub const MIN_WIDTH: u16 = 40;

/// Fewest rows the panels get below the help bar. A panel is a border plus
/// a line of content, so a shorter one shows a frame and nothing in it.
pub const MIN_PANEL_ROWS: u16 = 5;

/// Partition of the MC area into named panel rects.
///
/// In narrow mode exactly one panel rect is non-empty: the focused one,
/// which fills `panels`. The others are zero-sized, and callers skip them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McLayout {
    pub inbox: Rect,
    pub analytics: Rect,
    pub activity: Rect,
    pub schedule: Rect,
    pub help_bar: Rect,
    /// The area the panels were laid out in: the outer area minus the help
    /// bar. Used by the floor message.
    pub panels: Rect,
    /// True when the area is below the floor. Every panel rect is then
    /// empty and the caller draws the message instead.
    pub too_small: bool,
}

impl McLayout {
    /// The rect for one panel, whatever the current mode. Test-only: the
    /// renderer destructures the layout once and gates each panel on
    /// `rect.width > 0`.
    #[cfg(test)]
    pub fn rect(&self, panel: McPanel) -> Rect {
        match panel {
            McPanel::Inbox => self.inbox,
            McPanel::Analytics => self.analytics,
            McPanel::Activity => self.activity,
            McPanel::Schedule => self.schedule,
        }
    }

    /// Every panel rect, in `McPanel` order. Test-only helper, so the
    /// geometry tests can sweep all four panels without repeating the
    /// field list.
    #[cfg(test)]
    pub fn panels_in_order(&self) -> [(McPanel, Rect); 4] {
        [
            (McPanel::Inbox, self.inbox),
            (McPanel::Analytics, self.analytics),
            (McPanel::Activity, self.activity),
            (McPanel::Schedule, self.schedule),
        ]
    }

    fn all_empty(area: Rect, help_bar: Rect, panels: Rect) -> Self {
        let empty = Rect::new(area.x, area.y, 0, 0);
        Self {
            inbox: empty,
            analytics: empty,
            activity: empty,
            schedule: empty,
            help_bar,
            panels,
            too_small: true,
        }
    }
}

/// Compute the panel rectangles, showing `focus` full width when the area
/// is too narrow for the split.
///
/// This is the single entry point: the renderer passes the focused panel,
/// and the geometry tests pass one too, so the narrow mode is exercised by
/// the same code the screen runs.
pub fn compute_focused(area: Rect, focus: McPanel) -> McLayout {
    // Reserve the bottom rows for the help bar when there's room.
    let (panels_area, help_bar) = split_help_bar(area);

    if area.width < MIN_WIDTH || panels_area.height < MIN_PANEL_ROWS {
        return McLayout::all_empty(area, help_bar, panels_area);
    }

    if !responsive::mc_two_column(area.width) {
        let empty = Rect::new(area.x, area.y, 0, 0);
        return McLayout {
            inbox: if focus == McPanel::Inbox {
                panels_area
            } else {
                empty
            },
            analytics: if focus == McPanel::Analytics {
                panels_area
            } else {
                empty
            },
            activity: if focus == McPanel::Activity {
                panels_area
            } else {
                empty
            },
            schedule: if focus == McPanel::Schedule {
                panels_area
            } else {
                empty
            },
            help_bar,
            panels: panels_area,
            too_small: false,
        };
    }

    // Medium terminals give the left column more of the width; the wide
    // ones keep the 40/60 the panels were designed around.
    let left_percent = if area.width >= responsive::MC_WIDE_MIN {
        40
    } else {
        45
    };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(left_percent),
            Constraint::Percentage(100 - left_percent),
        ])
        .split(panels_area);

    // Left column: Inbox (small, usually short) over the Activity feed and the
    // Schedule. Right column: Analytics, full height, so its bars and tables
    // get the room the dense content needs.
    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(22),
            Constraint::Percentage(48),
            Constraint::Percentage(30),
        ])
        .split(cols[0]);

    McLayout {
        inbox: left_rows[0],
        activity: left_rows[1],
        schedule: left_rows[2],
        analytics: cols[1],
        help_bar,
        panels: panels_area,
        too_small: false,
    }
}

/// Rows reserved for the bottom commands bar. Two rows: panel navigation
/// keys on the first, the global D/W/M/A analytics filter on the second
/// (#900).
const HELP_BAR_ROWS: u16 = 2;

/// Split the bottom rows off as the help bar. Returns `(panels, help_bar)`.
/// When the area is too short to spare the bar, no help bar is reserved.
fn split_help_bar(area: Rect) -> (Rect, Rect) {
    if area.height <= HELP_BAR_ROWS {
        let empty = Rect { height: 0, ..area };
        return (area, empty);
    }
    let panels = Rect {
        height: area.height - HELP_BAR_ROWS,
        ..area
    };
    let help_bar = Rect {
        x: area.x,
        y: area.y + area.height - HELP_BAR_ROWS,
        width: area.width,
        height: HELP_BAR_ROWS,
    };
    (panels, help_bar)
}
