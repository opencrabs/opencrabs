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
//! 3. [`ChatState`]: the transcript, its scroll handle and the follow flag.
//!    Moved.
//! 4. [`Composer`]: the field the user types in, the drafts, the queue and the
//!    flags the hint and the spinner read. Moved.
//! 5. The entity that draws all of it, which is the step that stops a window
//!    repaint from re-walking the conversation.

use std::collections::HashMap;
use std::time::Instant;

use gpui::{App, Entity, ScrollHandle, SharedString};
use opencrabs::ui_core::composer::DraftStore;
use opencrabs::ui_core::scroll::Follow;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::agent_bridge::PendingMessage;
use super::editor::Editor;
use super::transcript::{LiveTurn, TranscriptState, TurnMetrics};

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

/// The conversation on screen: its rows, the viewport they scroll in, and
/// whether that viewport keeps following their bottom (#2805).
///
/// These three move together because every reader that touches one of them is
/// asking about the conversation itself: a reload lands rows, a send measures
/// the row total, a wheel releases the follow. None of them decides which
/// conversation the window is on, which is why the session id the rail, the
/// split fill and the plan strip read lives on `Dashboard::chat_session` as a
/// mirror instead of behind this value.
#[derive(Default)]
pub(crate) struct ChatState {
    /// The turns on screen and the stored rows they were built from.
    pub(crate) transcript: TranscriptState,
    /// The viewport the transcript scrolls in.
    pub(crate) scroll: ScrollHandle,
    /// Whether the transcript keeps following its newest row (#2685). A one
    /// shot snap could not: it is consumed by the next layout, and every row
    /// that lands afterwards is out of view. It is released when the reader
    /// scrolls up and re-armed when they send, open or come back (#2698).
    pub(crate) follow: Follow,
}

impl ChatState {
    /// The conversation these rows belong to, or `None` before any selection.
    pub(crate) fn session(&self) -> Option<Uuid> {
        self.transcript.session
    }

    /// The session's true row total, measured on the ledger and not on the
    /// display window, so a conversation longer than `WINDOW` still settles
    /// (#2705).
    pub(crate) fn row_total(&self) -> usize {
        self.transcript.row_total()
    }
}

/// The user row the composer is editing (#2826).
///
/// The next send replaces that row and drops everything after it, so the
/// conversation continues from the point the reader picked. It carries the
/// sequence rather than a position, because the rows on screen are a window
/// over a longer conversation and an index would point at the wrong row the
/// moment older history loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EditTarget {
    /// The stored row being replaced.
    pub(crate) id: Uuid,
    /// The conversation it belongs to. A send in any other conversation is
    /// not this edit.
    pub(crate) session: Uuid,
    /// The row's own sequence: the truncation keeps it and drops what is
    /// after it.
    pub(crate) sequence: i32,
}

/// The composer on screen and the messages waiting behind it (#2805).
///
/// The field the user types in, the drafts kept per conversation, the queue the
/// strip shows and the handful of flags the hint and the spinner read. They
/// belong together because every one of them answers the same question: what
/// does the next send do.
///
/// `history` is deliberately not here. It is one handle cloned into the window
/// and every split pane, so a message sent in one is recalled in all of them in
/// real time; moving it into this leaf would make every other leaf reach
/// through this one to get it.
pub(crate) struct Composer {
    /// The window's own message field. Typing goes to it while it holds focus,
    /// and its `Sent` events arrive through the subscription `main` makes.
    pub(crate) editor: Entity<Editor>,
    /// Half-typed text keyed by the conversation it was typed in, so switching
    /// away and back returns the same draft instead of throwing it away.
    pub(crate) drafts: DraftStore,
    /// Text typed before there is a conversation to key it to: while the agent
    /// stack is still booting, or before the first session exists. Everything
    /// else waits in `agent_bridge`'s per-session queue, the one the agent
    /// drains between tool calls; a second list here let the strip show
    /// messages the agent could not see, so they never went out (#2629).
    pub(crate) unsent: Vec<PendingMessage>,
    /// The stored user row this composer is editing (#2826). The send that
    /// follows replaces that row and drops what came after it; `None` for
    /// every ordinary send.
    pub(crate) edit: Option<EditTarget>,
    /// Whether the queue strip is showing every message or only its head. Five
    /// rows push the conversation off the screen, so past two the strip folds
    /// and a click opens it (#2629).
    pub(crate) queue_expanded: bool,
    /// Set when the agent stack failed to come up; sends are refused.
    pub(crate) agent_error: Option<String>,
    /// The hint last painted on the composers, so a tick only touches them when
    /// the state actually changed (#2630).
    pub(crate) hint_painted: Option<SharedString>,
    /// Poll ticks counted since the last repaint a live turn asked for. The
    /// spinner and the clock are the only things moving in that branch, and
    /// rebuilding every pane's transcript twenty times a second to advance them
    /// is what made a split screen crawl (#2688).
    pub(crate) anim_tick: u64,
}

impl Composer {
    /// Built around the field `main` creates. An entity has no default, so this
    /// value cannot either; everything else starts empty.
    pub(crate) fn new(editor: Entity<Editor>) -> Self {
        Self {
            editor,
            drafts: DraftStore::new(),
            unsent: Vec::new(),
            edit: None,
            queue_expanded: false,
            agent_error: None,
            hint_painted: None,
            anim_tick: 0,
        }
    }

    /// Paint the hint under the caret: the window's own field and each split
    /// pane's, so a pane that takes the keyboard already says the right thing.
    /// Only a real state change touches them, because the placeholder is an
    /// edit on every composer and a tick that repaints them all is a tick that
    /// steals the frame (#2630).
    ///
    /// The panes come in as an argument: this leaf owns the window's field, not
    /// the split tree, so the window hands it the fields it cannot reach.
    pub(crate) fn sync_composer_hint(
        &mut self,
        hint: SharedString,
        panes: &[Entity<Editor>],
        cx: &mut App,
    ) {
        if self.hint_painted.as_ref() == Some(&hint) {
            return;
        }
        self.hint_painted = Some(hint.clone());
        self.editor
            .update(cx, |ed, cx| ed.set_placeholder(hint.clone(), cx));
        for editor in panes {
            editor.update(cx, |ed, cx| ed.set_placeholder(hint.clone(), cx));
        }
    }
}
