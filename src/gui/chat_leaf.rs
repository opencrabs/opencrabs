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
//! 1. [`Settle`] and [`Metrics`]: the reload handshake and the per-turn
//!    numbers waiting on it. Pure data, no window, no context. Moved.
//! 2. [`Turn`]: the live card, `busy`, the cancel handle, the prompt size and
//!    the handshake above, so the turn on screen and the ones parked under
//!    their own conversations (#2616) are one shape instead of two. Moved.
//! 3. The transcript: the rows, their scroll handle and the follow flag.
//! 4. The composer handles and the queue, and then the entity that draws all
//!    of it, which is the step that stops a window repaint from re-walking
//!    the conversation.

use std::collections::HashMap;
use std::time::Instant;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::transcript::{LiveTurn, TurnMetrics};

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

/// The turn a conversation is running, and the same turn parked under a
/// conversation the window is not painting (#2616).
///
/// One struct for both, because both hold exactly the same facts: the card,
/// whether anything is still moving, the handle that stops it, the prompt size
/// the provider last reported, and the reload handshake waiting for its rows.
/// The window used to keep the visible half in five loose fields and the
/// parked half in a near-identical `StashedTurn`, with `swap_visible_turn`
/// copying field by field between them through nine parameters. Moving the
/// whole value is the same swap with none of the copying, and it is the state
/// the chat leaf owns from here on (#2805).
#[derive(Default)]
pub(crate) struct Turn {
    /// The turn in flight: question, stream, tool rows, notices. It renders
    /// as the transcript's last entry and clears once the DB holds the rows,
    /// so a slow or silent turn never looks like an empty conversation.
    pub(crate) live: LiveTurn,
    /// A turn is running: sends queue instead of starting a second one.
    pub(crate) busy: bool,
    /// The running turn's cancellation token, so Esc twice stops it the way
    /// the TUI's second Esc does. `None` when no turn is in flight.
    pub(crate) cancel: Option<CancellationToken>,
    /// Prompt tokens the provider last reported (`ProgressEvent::TokenCount`
    /// and the finished turn's own `context_tokens`). `None` until the first
    /// answer, which is why the TUI shows a dash there too.
    pub(crate) ctx_tokens: Option<u32>,
    /// The reload handshake that keeps a finished turn's card up until the
    /// ledger holds its rows (#2705).
    pub(crate) settle: Settle,
}

impl Turn {
    /// A turn already running in a conversation the window is not painting:
    /// a recovered turn at boot (#2627) and a completion that resumed a
    /// session nobody is looking at (#2704).
    ///
    /// Both park the same shape, so both build it here instead of spelling out
    /// five fields and hoping the next one added to `Turn` is remembered.
    pub(crate) fn running(live: LiveTurn, cancel: CancellationToken) -> Self {
        Self {
            live,
            busy: true,
            cancel: Some(cancel),
            ..Self::default()
        }
    }
}
