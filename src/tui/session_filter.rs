//! The session-list filter shared by the TUI dialog's keys and its render
//! (#2723).
//!
//! Pure: the title's kind comes from the core classifier and the plan state
//! from the shared index, so the same inputs always give the same answer and
//! the rule is testable without a terminal. It mirrors the GUI rail filter
//! (`gui::sessions::rail_filter_allows`) but reads the core three-way
//! [`PlanFilterState`], so one session reads as the same plan state on both
//! surfaces.

use crate::db::repository::session::{SessionKind, classify_session_title};
use crate::utils::plan_files::PlanFilterState;

/// The plan-state chip the list filter cycles through (#2723). `All` is the
/// untouched default; the other three map one-to-one onto the core
/// [`PlanFilterState`] a session resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanChip {
    /// Every session, plan or not.
    #[default]
    All,
    /// A live plan: written and waiting for approval, or approved and running.
    Wip,
    /// A finished plan: completed or archived.
    Finished,
    /// No plan worth drawing.
    NoPlan,
}

impl PlanChip {
    /// The next chip in the cycle the `s` key walks:
    /// All -> Wip -> Finished -> NoPlan -> All.
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Wip,
            Self::Wip => Self::Finished,
            Self::Finished => Self::NoPlan,
            Self::NoPlan => Self::All,
        }
    }

    /// Header label for the chip, shown while the filter is not the default.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all plans",
            Self::Wip => "plan wip",
            Self::Finished => "plan done",
            Self::NoPlan => "no plan",
        }
    }
}

/// The TUI session-list filter (#2723): a plan-state chip plus whether the
/// automation sessions (cron + RSI) show. The default hides automation per
/// A1 and leaves the chip on All, so an untouched list reads the way it always
/// did minus the background noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionFilter {
    pub plan: PlanChip,
    pub show_automation: bool,
}

impl SessionFilter {
    /// Whether the filter narrows the browsing list at all. The render shows
    /// the chip header only when this is true, so the default list keeps its
    /// clean look.
    pub fn is_active(&self) -> bool {
        self.plan != PlanChip::All || self.show_automation
    }
}

/// Whether one row passes the browsing filter (#2723). Automation (cron +
/// RSI) is hidden unless the toggle is on; the plan chip then narrows what
/// remains. A subagent is not automation for this toggle: it stays visible.
pub fn session_filter_allows(filter: &SessionFilter, title: &str, plan: PlanFilterState) -> bool {
    if !filter.show_automation
        && matches!(
            classify_session_title(title),
            SessionKind::Cron | SessionKind::Rsi
        )
    {
        return false;
    }
    match filter.plan {
        PlanChip::All => true,
        PlanChip::Wip => plan == PlanFilterState::Wip,
        PlanChip::Finished => plan == PlanFilterState::Finished,
        PlanChip::NoPlan => plan == PlanFilterState::None,
    }
}

/// The whole per-row visibility decision (#2723). An explicit keyword search
/// spans every session (A1): the automation hide and the plan chip gate only
/// the browsing list, never a search the user typed to find something. With no
/// search, the browsing filter decides.
pub fn session_row_visible(
    filter: &SessionFilter,
    title: &str,
    plan: PlanFilterState,
    search: &str,
) -> bool {
    let search = search.trim().to_lowercase();
    if !search.is_empty() {
        return title.to_lowercase().contains(&search);
    }
    session_filter_allows(filter, title, plan)
}
