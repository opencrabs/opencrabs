//! The chat leaf's conversation state, one owner per fact.
//!
//! The window's own pane is the last one whose state still lives on
//! `Dashboard`: the split leaves are `PaneView` entities, so a frame that
//! touches one of them repaints that leaf, while anything that touches the
//! chat repaints the whole window (#2805). This module is where that state
//! moves to, one cohesive group at a time, so each step is a rename rather
//! than a rewrite.
//!
//! The groups, in the order they move:
//!
//! 1. [`Settle`] and [`Metrics`] here: the reload handshake and the per-turn
//!    numbers waiting on it. Pure data, no window, no context.
//! 2. The turn itself: the live card, its cancel handle, `busy`, the prompt
//!    size, and the stash that parks one conversation's turn under it while
//!    another is on screen (#2616).
//! 3. The transcript: the rows, their scroll handle and the follow flag.
//! 4. The composer handles and the queue, and then the entity that draws all
//!    of it, which is the step that stops a window repaint from re-walking
//!    the conversation.

use std::collections::HashMap;
use std::time::Instant;

use uuid::Uuid;

use super::transcript::TurnMetrics;

/// The reload handshake that keeps a finished turn's card on screen until the
/// ledger holds its rows.
///
/// A turn ends before its rows are written, so the card stays up and the
/// transcript is re-read until the session's row total reaches the count the
/// send expected (#2705). This is the whole state of that wait: what was
/// expected, how many reads it has cost, when the budget started and how many
/// late re-reads are left.
#[derive(Default)]
pub(crate) struct Settle {
    /// Rows the session must hold before the live card may be dropped. `None`
    /// means no turn is waiting on a write.
    pub(crate) expect: Option<usize>,
    /// Reads already spent on this turn, so a write that never lands costs a
    /// bounded number of them instead of a loop.
    pub(crate) retries: u8,
    /// When the current wait started. `None` between turns: the budget is
    /// measured from the first read that found the rows short, not from the
    /// send, because the write only exists once the turn has ended.
    pub(crate) deadline: Option<Instant>,
    /// Late re-reads already spent on this turn. Two is the whole allowance.
    pub(crate) rechecks: u8,
}

impl Settle {
    /// Arm the handshake for a send, a queue flush or a resumed turn.
    ///
    /// All three used to spell the same four lines out, and the count is the
    /// session's own row total plus the one the new turn appends: measured on
    /// the ledger, never on the windowed transcript, so a conversation longer
    /// than the display window still settles.
    pub(crate) fn begin(&mut self, row_total: usize) {
        self.retries = 0;
        self.deadline = None;
        self.rechecks = 0;
        self.expect = Some(row_total + 1);
    }
}

/// What each settled turn cost and how fast it answered (#2324).
///
/// In memory only, so a restarted window forgets instead of showing a
/// session's current numbers as if they belonged to an old turn. A row only
/// exists once the turn has persisted, so the numbers wait in `pending` for
/// the reload that lands the anchor they key on.
#[derive(Default)]
pub(crate) struct Metrics {
    /// Per-turn numbers, keyed by the anchor the row was given.
    pub(crate) by_anchor: HashMap<String, TurnMetrics>,
    /// Numbers for turns that just settled, waiting for their rows. One slot
    /// per session: a reload for another conversation must not claim them,
    /// and a split can settle two panes before either reload comes back
    /// (#2632).
    pub(crate) pending: HashMap<Uuid, TurnMetrics>,
}
