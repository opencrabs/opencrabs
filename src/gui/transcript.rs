//! The transcript: the selected session's stored conversation, rendered the
//! way the TUI renders it. Three hard rules carry over from the TUI's own
//! message pipeline:
//!
//! 1. Only the newest `WINDOW` rows are read, `LIMIT` pushed into SQL via
//!    `find_recent_by_session`, and the hidden count comes from
//!    `count_by_session`. Reading a 10k-message session no longer pulls the
//!    whole history to show the tail of it (`DISPLAY_MESSAGE_WINDOW` in
//!    `tui/app/messaging.rs` is the same contract).
//! 2. Assistant rows are expanded into text / thinking / tool-ledger blocks
//!    by the crate's own parsers (`tui::app::ledger_scan`,
//!    `tui::app::reasoning_split`), the exact grammar the TUI uses. The gui
//!    mirrors the TUI; it does not fork its parsers.
//! 3. Answer text renders as markdown (`gui/markdown.rs`), not raw lines.
//!
//! Phase-0 lessons still bind: 4 MB fetch thread, no migrations, char-safe
//! truncation everywhere (#2271).

use crate::palette;
use crate::topic_icons;
use opencrabs::services::message::{CompletedPlanPayload, PLAN_COMPLETED_ROLE};
use opencrabs::ui_core::card::{CARD_RADIUS, concentric_radius, glass_fill};
use opencrabs::ui_core::copy_select::{self, TextPoint};
use opencrabs::ui_core::links::{self, ClickTarget};
use opencrabs::ui_core::plan_history::CompletedPlanView;
use opencrabs::ui_core::rail;
use opencrabs::utils::is_internal_system_message;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AbsoluteLength, AnyElement, App, ClickEvent, Div, HighlightStyle, MouseButton, Pixels, Point,
    ScrollHandle, SharedString, Stateful, Styled, StyledText, TextLayout, TextStyle, Window, div,
    prelude::*, px, rems, rgb, rgba,
};

use opencrabs::db::Database;
use opencrabs::db::models::Message;
use opencrabs::db::repository::message::MessageRepository;
use opencrabs::db::repository::session::SessionRepository;
use opencrabs::tui::app::ledger_scan::next_ledger;
use opencrabs::tui::app::reasoning_split::{self, Segment, is_intermediate, split_segments};
// The fold header's wording is the TUI's own (#759), re-exported from the lib
// rather than re-spelled here: two dialects of the same line would drift the
// moment either client changed one.
use opencrabs::tui::render::{
    TurnSummary, format_turn_header, format_turn_spinner_meta, is_deliverable_report,
};

use crate::attachment_view;
use crate::editor::{Attachment, marker_paths, strip_markers};
use crate::markdown;

/// Newest rows shown per load, matching the TUI display window. Older
/// history stays behind the `Marker` block.
const WINDOW: usize = 200;

/// Char-safe truncation for collapsed content (#2271: never slice bytes).
fn fit(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for c in s.chars().take(max_chars) {
        out.push(c);
    }
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// One expanded tool call: description, whether it succeeded, and whether it
/// has finished at all. The last flag only matters while a turn runs: a call
/// that has started and not returned is neither success nor failure, and
/// drawing it as one would lie about a tool that is still working.
#[derive(Clone)]
pub struct ToolCall {
    /// The tool's own name (`bash`, `read_file`), kept so a completion event
    /// finds the row its start opened. Empty for ledger-parsed calls, which
    /// are already finished and never need matching.
    pub name: String,
    pub description: String,
    pub success: bool,
    pub done: bool,
    /// Set only on a detached background command's row. The TUI's finish event
    /// names the task it settles (`app::state`), so the row carries the same
    /// id and a finish finds its own row instead of the newest open one. Every
    /// other row is `None`: a provider call or a ledger entry has no task.
    pub task_id: Option<String>,
}

impl ToolCall {
    /// A call that has started and not returned yet: the live turn's row.
    pub fn running(name: String, description: String) -> Self {
        Self {
            name,
            description,
            success: false,
            done: false,
            task_id: None,
        }
    }

    /// A detached background command's row: still running, keyed by the id its
    /// finish will name. The description says `detached` so a row that outlives
    /// the turn cannot be mistaken for a call that is blocking it.
    pub fn detached(task_id: String, label: String) -> Self {
        Self {
            name: "bash".to_string(),
            description: format!("detached · {label}"),
            success: false,
            done: false,
            task_id: Some(task_id),
        }
    }
}

/// Settle the row the start of `task_id` drew: `true` when a row was found.
///
/// Matching by id, never by name or position: parallel calls in one turn mean
/// "the newest open bash row" can belong to a different command. A finish with
/// no row is not an error (the turn may have been reloaded), so the caller only
/// logs it.
pub fn settle_detached(
    tools: &mut [ToolCall],
    task_id: &str,
    success: bool,
    elapsed_secs: f32,
) -> bool {
    let Some(call) = tools
        .iter_mut()
        .find(|c| c.task_id.as_deref() == Some(task_id))
    else {
        return false;
    };
    call.done = true;
    call.success = success;
    call.description.push_str(&format!(" · {elapsed_secs:.1}s"));
    true
}

/// A click listener the view attaches to one fold header. The view cannot
/// build these itself: they need the owning view's `cx`, so the caller mints
/// one per turn anchor and hands them in.
///
/// `Rc` and not `Box`: a transcript can carry the same path in more than one
/// message, and each of those tiles has to open the file. One handler per
/// unique path, cloned per tile, keeps a re-sent picture clickable (#2395).
/// gpui's `on_click` takes an `impl Fn` and `Rc<dyn Fn>` does not implement
/// `Fn`, so a tile forwards through a small closure of its own.
pub type ClickHandler = std::rc::Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A right press on a row. Minted by the window for the same reason as
/// [`ClickHandler`]: only the owning view can build one (#2613).
pub type PressHandler = std::rc::Rc<dyn Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static>;

/// A wheel tick over the transcript, so a reader can release the follow by
/// hand and get it back by scrolling down (#2698). Minted by the owning view
/// for the same reason as [`ClickHandler`]: only it holds the state to change.
pub type ScrollHandler =
    std::rc::Rc<dyn Fn(&gpui::ScrollWheelEvent, &mut Window, &mut App) + 'static>;

/// A rendered piece of the conversation. The TUI's `DisplayMessage` kinds,
/// narrowed to what the gui pane draws.
/// What a forwarded row shows above its text (#2608): the source session's
/// name, frozen when the copy was written, and whether this row opens the
/// batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardMark {
    pub label: String,
    /// Every row of one multi-select forward shares it.
    pub batch: uuid::Uuid,
    /// The batch's first row carries the header; the rest only continue the
    /// accent bar, so N rows read as one block instead of N stacked headers.
    pub first: bool,
}

/// The line above a forwarded block. Pure, so the wording is pinned by test
/// rather than by the div that draws it.
pub fn forward_header(label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        "Forwarded message".to_string()
    } else {
        format!("Forwarded from {label}")
    }
}

pub enum Block {
    /// A user or assistant text turn; assistant content renders markdown.
    Msg {
        role: String,
        content: String,
        time: String,
        /// The "(tool calls only)" filler a ledger-only row needs. It is
        /// structure, not text the model wrote, so it must never be promoted
        /// to the turn's final answer and left exposed by the fold.
        placeholder: bool,
        /// The stored row this block came from, so a click on it can name the
        /// message instead of a position that shifts as history loads
        /// (#2608). `None` for the live turn, whose row is not written yet.
        message_id: Option<uuid::Uuid>,
        /// Set on a forwarded copy: who forwarded it and whether it opens a
        /// batch (#2608).
        forward: Option<ForwardMark>,
    },
    /// Model thinking or intermediate narration, dim and collapsed.
    Thinking { content: String },
    /// A tool ledger: calls with their status.
    Tools { calls: Vec<ToolCall> },
    /// A durable, read-only completed checklist. The database UUID keeps its
    /// identity stable across reloads; the shared view keeps GUI and TUI text
    /// identical without exposing the stored JSON.
    CompletedPlan {
        message_id: uuid::Uuid,
        plan: CompletedPlanView,
    },
    /// History marker: the rows the window hid.
    Marker { hidden: usize },
}

/// The parts of a settled turn the Telegram card carries and the shared
/// turn header deliberately does not (#744 keeps ctx out of the terminal
/// header): the context the turn ran with, and its output rate.
///
/// In memory only (#2324 D3): after a restart there is no record for the
/// turn, so the card shows no metrics instead of borrowing the session's
/// current number, which would be a lie about what that turn cost.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TurnMetrics {
    /// Prompt tokens the model saw on the turn's final call.
    pub ctx_used: u32,
    /// The conversation's context window at the time.
    pub ctx_max: u32,
    /// Provider-reported output rate for the turn; `None` when it could not be
    /// measured. This is the turn's own rate, not the session's live one the
    /// status bar prints (#2632).
    pub tps: Option<f64>,
    /// How many calls the turn made, counted the way the live card counts
    /// them. A persisted row can lose its ledger (the tool-only rows prove
    /// it), and the settled card then claimed a turn did no work at all. This
    /// is the number the user watched, so it is the floor the card prints
    /// (#2632).
    pub tool_calls: usize,
}

impl TurnMetrics {
    /// The card's ctx segment, built by the same formatter the Telegram
    /// footer uses (`format_ctx_footer`), so the two surfaces cannot drift
    /// into different spellings of the same number (#2324 D2).
    pub fn segment(&self) -> Option<String> {
        (self.ctx_max > 0).then(|| {
            opencrabs::utils::string::format_ctx_footer(self.ctx_used, self.ctx_max, self.tps)
        })
    }
}

/// One conversation turn, folded the way the TUI folds it (#758/#759): the
/// question and the answer stay, the working-out hides behind a header line.
pub struct Turn {
    /// `\u{2713} 4 steps \u{b7} 9 tool calls \u{b7} 1m 20s`, or `None` for a turn
    /// that did no work (a plain question and answer earns no card at all).
    /// The card's head prints its own verdict and its footer prints the numbers
    /// (#2632), so this string is the marker that the turn worked and the
    /// terminal's own wording, kept for the TUI-shaped readers and the tests.
    pub header: Option<String>,
    /// What the turn did: its calls, failures and wall time. The footer prints
    /// these next to the ctx budget, the way the live card does (#2632).
    pub summary: TurnSummary,
    /// The turn's own ctx and rate, when this process settled it and still
    /// remembers. Drawn after the header, in the Telegram card's wording.
    pub metrics: Option<TurnMetrics>,
    /// Last call description and whether it succeeded, shown under a folded
    /// header so a collapsed turn still says what it did.
    pub preview: Option<(String, bool)>,
    /// Stable identity across reloads, the same key the TUI uses (#2155):
    /// role plus the second the turn opened. The expanded set is keyed by it
    /// so an opened turn survives a transcript reload.
    pub anchor: String,
    pub blocks: Vec<Block>,
    /// The turn's final answer, the one assistant block the fold never hides.
    pub final_idx: Option<usize>,
    /// Which topic this turn came from, `None` outside a merged `All` view
    /// (#2762). The transcript draws a divider whenever the label changes, so
    /// a forum read whole labels its rows the way Telegram does instead of
    /// showing one undifferentiated stream.
    pub origin: Option<String>,
}

/// One entry in the live card's body, in the order it happened. The Telegram
/// status block grows this way: tool rows, narration and reasoning land inside
/// ONE card as the turn runs (#2442).
pub enum LiveEntry {
    /// Index into [`LiveTurn::tools`]. A completion settles that row, so it
    /// never appends a second entry for the same call.
    Tool(usize),
    /// Text the model said between tool calls, drawn as markdown.
    Narration(String),
    /// Reasoning, dimmed and capped, inside the card rather than as a loose
    /// "thinking" line under it.
    Reasoning(String),
}

/// The turn in flight: the question just sent, the model's streamed answer,
/// the tool rows it opened and any notice. It renders as the conversation's
/// last entry, not in a panel above the editor, so the answer appears exactly
/// where the conversation ends, the way the TUI's live turn does.
///
/// It is provisional by design: once the turn's rows persist, the DB load
/// takes over and this clears. Until then it is the only thing keeping a
/// slow or silent turn from looking like an empty session.
#[derive(Default)]
pub struct LiveTurn {
    /// The question just sent, echoed until the row lands.
    pub user: Option<String>,
    /// The provider is thinking, before the first token arrives.
    pub thinking: bool,
    /// Reasoning the provider streamed before (and between) answer text.
    /// The TUI shows it live; ignoring it made a reasoning-heavy turn look
    /// silent until the final answer landed in one burst.
    pub reasoning: String,
    /// Streamed answer text, appended chunk by chunk.
    pub stream: String,
    /// Tool calls this turn opened, in order, with their live status.
    pub tools: Vec<ToolCall>,
    /// The card's body in order: tools, narration and reasoning interleaved
    /// exactly as they happened, so the block combines in place (#2442).
    pub entries: Vec<LiveEntry>,
    /// Errors and queue notices: amber, never folded.
    pub notices: Vec<String>,
    /// Queued messages the agent took between tool calls, in the order they
    /// landed. Drawn as user rows in the live turn until the reload has them (#2441).
    pub injected: Vec<String>,
    /// The final answer once the turn reports `Done`. Kept on screen until
    /// the reload proves the row is persisted, so a turn that streamed
    /// nothing still shows its answer instead of blanking the pane.
    pub settled: Option<String>,
    /// Output tokens produced this turn, counted with the same tokenizer the
    /// TUI runs over every chunk (`brain::tokenizer`), so the live counter and
    /// the TUI's agree instead of being two estimates.
    pub tokens: u32,
    /// The turn's streaming rate, measured by the same tracker the terminal
    /// footer runs (#2703). It holds the active streaming seconds (tool runs,
    /// approval waits and between-response round-trips excluded) and keeps the
    /// last finalized rate across idle, so the footer shows the previous turn's
    /// number instead of blanking.
    tps: opencrabs::ui_core::rate::StreamingTpsTracker,
    /// When the turn opened. The status line's clock reads from here, which is
    /// the only reason a silent provider no longer looks like a frozen window.
    pub started: Option<std::time::Instant>,
    /// Chunks seen, bytes streamed, and how long the first chunk took. Kept so
    /// the turn's one-line log receipt can prove whether the provider actually
    /// streamed, instead of that being guessed from a screenshot.
    pub chunks: usize,
    pub chunk_bytes: usize,
    pub first_chunk: Option<std::time::Duration>,
    /// When the question was sent, wall clock: the live bubble's timestamp, so
    /// a fresh turn reads the same as the row it becomes.
    pub sent_at: Option<chrono::DateTime<chrono::Local>>,
    /// Whether a reload has already read this turn's question back out of the
    /// DB. Set once, never before the row exists, so the card can stop asking
    /// whether the newest persisted turn is its own: it has seen it land.
    /// The question itself stays here, because the card is what draws it
    /// while the persisted copy is skipped.
    pub landed: bool,
    /// When the answer's first chunk arrived. Stamped once, not per repaint,
    /// or the timestamp would drift every frame.
    pub answer_at: Option<chrono::DateTime<chrono::Local>>,
}

impl LiveTurn {
    /// Nothing in flight: the transcript draws only what the DB holds.
    pub fn is_empty(&self) -> bool {
        self.user.is_none()
            && !self.thinking
            && self.stream.is_empty()
            && self.tools.is_empty()
            && self.entries.is_empty()
            && self.notices.is_empty()
            && self.injected.is_empty()
            && self.settled.is_none()
    }

    /// The model has not said anything yet: no reasoning, no text, no tool
    /// call, no queued message taken. A stop inside that window can hand the
    /// question back to the composer, because there is no answer to lose
    /// (#2635).
    pub fn untouched(&self) -> bool {
        self.stream.is_empty()
            && self.reasoning.is_empty()
            && self.tools.is_empty()
            && self.entries.is_empty()
            && self.injected.is_empty()
            && self.settled.is_none()
    }

    /// Start a fresh turn: the previous one is gone, so only the question
    /// carries over. Notices go with it, or a stale "agent offline" would
    /// outlive the failure it described.
    pub fn begin(&mut self, user: String) {
        self.clear();
        self.user = Some(user);
        self.started = Some(std::time::Instant::now());
        self.sent_at = Some(chrono::Local::now());
    }

    /// Start a turn the ledger already owns: a restart resumed it (#2740).
    ///
    /// The question's row was written before the previous process died, so this
    /// card is drawing an exchange the rows can also produce. Saying so once, up
    /// front, is the whole point: without it the card stamps the resume time,
    /// the time window in [`newest_live_turn_matches`] misses a row that can be
    /// minutes older, and the question is painted twice, once from the row and
    /// once from the echo.
    pub fn resume(&mut self, user: String) {
        self.begin(user);
        self.landed = true;
    }

    /// The DB has the turn: drop the live copy.
    pub fn clear(&mut self) {
        self.user = None;
        self.injected.clear();
        self.thinking = false;
        self.reasoning.clear();
        self.stream.clear();
        self.tools.clear();
        self.entries.clear();
        self.notices.clear();
        self.settled = None;
        self.tokens = 0;
        self.started = None;
        self.chunks = 0;
        self.chunk_bytes = 0;
        self.first_chunk = None;
        self.sent_at = None;
        self.answer_at = None;
        self.landed = false;
        // The accumulator belongs to the turn that just ended; the finalized
        // rate deliberately survives, so an idle footer keeps the number
        // instead of blanking until the next turn streams.
        self.tps.reset_window();
    }

    /// Active streaming seconds so far, including the open window. Measured
    /// to the LAST chunk, never to now, so idle ticks cannot inflate it.
    pub fn streaming_active_secs(&self) -> f64 {
        self.tps.active_secs()
    }

    /// Finish the turn's rate (#2325): prefer the service's own
    /// `tokens_per_second` (provider-reported tokens over summed active
    /// streaming time, the number the Telegram card prints), else the local
    /// active-window estimate, and reset the accumulator. A turn that produced
    /// nothing keeps the previous number rather than reporting a fake zero.
    pub fn finalize(&mut self, authoritative: Option<f64>) {
        self.tps.finalize(self.tokens, authoritative);
    }

    /// Seconds since the turn opened; zero before one has.
    pub fn elapsed_secs(&self) -> u64 {
        self.started.map(|t| t.elapsed().as_secs()).unwrap_or(0)
    }

    /// Output tokens per second, measured the way every other surface measures
    /// it (#2325): this turn's tokens over its ACTIVE streaming seconds, never
    /// over wall clock, so a tool run cannot drag the number down. When no
    /// window is open the last finalized rate is returned, which is what keeps
    /// an idle footer from blanking the moment a turn settles.
    pub fn tokens_per_sec(&self) -> f32 {
        // The same guard the service and the terminal use (#2703): a window
        // that collapsed below the floor, or a rate over the ceiling, is not a
        // measurement, so the last credible number stays on screen instead.
        opencrabs::ui_core::rate::credible_rate(self.tokens, self.tps.active_secs())
            .or(self.tps.last_tps)
            .map_or(0.0, |t| t as f32)
    }

    /// Count one answer chunk: text plus its tokens, stamped with the instant
    /// it arrived. Counting here keeps the live counter honest for every
    /// caller (a chunk that never renders still cost the tokens).
    ///
    /// The stamp is a parameter rather than a `now()` read because `drain`
    /// hands the GUI a whole batch at once (#2703). Reading the clock in here
    /// stamped every chunk in the batch with one instant, collapsed the window
    /// the rate is measured over, and is how 4640 tok/s reached the status bar.
    pub(crate) fn note_chunk_at(&mut self, text: &str, now: std::time::Instant) {
        let tokens = opencrabs::brain::tokenizer::count_tokens(text) as u32;
        self.tokens = self.tokens.saturating_add(tokens);
        self.chunks += 1;
        self.chunk_bytes += text.len();
        if self.first_chunk.is_none() {
            self.first_chunk = self.started.map(|t| t.elapsed());
        }
        if self.answer_at.is_none() {
            self.answer_at = Some(chrono::Local::now());
        }
        if tokens > 0 {
            self.tps.advance(now);
        }
        self.stream.push_str(text);
    }

    /// Append narration that may already have arrived as streamed chunks.
    /// The provider reports intermediate text both ways, and appending both
    /// is what put the same paragraph on screen twice. A suffix match is
    /// enough: the streamed copy of the same text always ends the buffer.
    pub fn push_narration(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        // The provider reports the same narration twice: streamed as chunks and
        // again as intermediate text. When the streamed copy is what ends the
        // pending stream, this text IS that copy, so it moves into the card
        // instead of being drawn twice (#2442).
        if self.stream.trim_end().ends_with(text) {
            self.stream.clear();
        }
        self.push_narration_entry(text);
    }

    /// Put one narration paragraph in the card, unless the newest narration
    /// entry already ends with it.
    fn push_narration_entry(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if let Some(LiveEntry::Narration(last)) = self.entries.last()
            && last.trim_end().ends_with(text)
        {
            return;
        }
        self.entries.push(LiveEntry::Narration(text.to_string()));
    }

    /// Whatever streamed since the last tool boundary was narration, not the
    /// answer, so it moves into the card and the answer bubble starts clean.
    fn flush_stream_into_narration(&mut self) {
        let text = self.stream.trim().to_string();
        self.stream.clear();
        self.push_narration_entry(&text);
    }

    /// A tool started: the narration before it joins the card first, then the
    /// row takes its place in the card's own order. The count lives in the
    /// footer, so a call is never listed twice.
    pub fn push_tool(&mut self, call: ToolCall) {
        self.flush_stream_into_narration();
        self.entries.push(LiveEntry::Tool(self.tools.len()));
        self.tools.push(call);
    }

    /// The same guard for reasoning, which travels as `IntermediateText`.
    ///
    /// The clock is a parameter for the same reason as [`Self::note_chunk_at`]
    /// (#2703): reasoning arrives inside a drained batch, so a clock read in
    /// here would measure the fold loop instead of the stream and collapse the
    /// rate window. The stamp travels from the bridge with the event.
    pub fn push_reasoning_at(&mut self, text: &str, now: std::time::Instant) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self.reasoning.trim_end().ends_with(text) {
            self.thinking = false;
            return;
        }
        if !self.reasoning.trim_end().is_empty() {
            self.reasoning.push_str("\n\n");
        }
        self.note_reasoning_at(text, now);
    }

    /// Count one reasoning chunk, stamped with the instant it arrived.
    ///
    /// Reasoning advances the streaming window the same way the answer does,
    /// because it is model output and the window must cover the time it took.
    /// The stamp is injected for the same reason as `note_chunk_at` (#2703):
    /// the GUI folds a drained batch in one loop, so a clock read here would
    /// measure the loop rather than the stream. The terminal reads its clock
    /// per event because its bridge mints `StreamingOutputTokens` one at a
    /// time (`cli/ui.rs`); the GUI has no such event and must carry arrival.
    pub(crate) fn note_reasoning_at(&mut self, text: &str, now: std::time::Instant) {
        let tokens = opencrabs::brain::tokenizer::count_tokens(text) as u32;
        self.tokens = self.tokens.saturating_add(tokens);
        if self.first_chunk.is_none() {
            self.first_chunk = self.started.map(|t| t.elapsed());
        }
        self.thinking = false;
        if tokens > 0 {
            self.tps.advance(now);
        }
        self.reasoning.push_str(text);
        match self.entries.last_mut() {
            Some(LiveEntry::Reasoning(open)) => open.push_str(text),
            _ => self.entries.push(LiveEntry::Reasoning(text.to_string())),
        }
    }

    /// The answer text on screen right now: the stream if any arrived, else
    /// the settled response. `None` when the turn has produced nothing.
    pub fn answer(&self) -> Option<&str> {
        if !self.stream.trim().is_empty() {
            Some(self.stream.as_str())
        } else {
            self.settled.as_deref().filter(|t| !t.trim().is_empty())
        }
    }

    /// The paragraphs the crab actually spoke between tool calls, in order.
    ///
    /// They live in the card's body, which is folded by default, so a turn
    /// with 27 tool calls read as a header and a count while the rail's
    /// one-line preview quoted the sentence the reader could not find in the
    /// chat. The prose is the turn's own words, not working-out, so the pane
    /// prints it beside the card instead of hiding it behind a click.
    pub fn narration(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().filter_map(|entry| match entry {
            LiveEntry::Narration(text) => Some(text.as_str()),
            _ => None,
        })
    }
}

/// The transcript's live state, owned by the window's view.
#[derive(Default)]
pub struct TranscriptState {
    /// The session whose messages are on screen; `None` before any selection.
    pub session: Option<uuid::Uuid>,
    /// Rows the window hid, announced above the transcript.
    pub hidden: usize,
    pub turns: Vec<Turn>,
    /// Anchors the user has opened. Folding is the default, so absence means
    /// folded, exactly like `turn_is_folded` in the TUI.
    pub expanded: HashSet<String>,
    /// Turns whose details are shown in full, past the first expansion: the
    /// reasoning is no longer cut short. The second ctrl+o sets it, as the TUI's
    /// second press does (#2440).
    pub details: HashSet<String>,
    /// Whether the live card's body is open. Folding is the default, exactly
    /// like a settled turn, and a click on the card or ctrl+o toggles it
    /// (#2442).
    pub live_expanded: bool,
    /// Set when the fetch failed; the pane says why instead of lying empty.
    pub error: Option<SharedString>,
    /// The stored rows the turns were built from. Kept so a forward can copy
    /// the picked messages themselves, not the blocks they were rendered as
    /// (#2608): a block carries text, not the row's id or provenance.
    pub rows: Vec<Message>,
}

impl TranscriptState {
    /// The session's true row total: the rows on screen plus the ones the
    /// window hid, which is the count `load_recent` reads straight from the
    /// ledger (#2705).
    ///
    /// This is what a settle test has to measure. The windowed turn count
    /// stops growing once a session passes `WINDOW`, so a turn that appends
    /// rows could never be seen to land.
    pub fn row_total(&self) -> usize {
        self.hidden + self.rows.len()
    }

    /// Newest turn's anchor: the one ctrl-o acts on, as in the TUI where
    /// ctrl+o toggles the newest turn only.
    pub fn newest_anchor(&self) -> Option<&str> {
        self.turns.last().map(|t| t.anchor.as_str())
    }

    /// The ids of every stored row on screen, so the window can mint a click
    /// and a right-press listener per row (#2613). Live rows carry no id and
    /// are absent by construction: they are not selectable.
    pub fn message_ids(&self) -> Vec<uuid::Uuid> {
        self.rows.iter().map(|m| m.id).collect()
    }

    /// Every anchor on screen, so the caller can mint one click listener each.
    pub fn anchors(&self) -> Vec<String> {
        self.turns.iter().map(|t| t.anchor.clone()).collect()
    }

    /// Every attachment the tiles can open, in the marker's own path form and
    /// without repeats. The caller mints one click listener per entry for the
    /// same reason it does for anchors: only its own `cx` can build one, and a
    /// tile must open its own file, not whichever was planned last.
    pub fn attachment_paths(&self) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        for turn in &self.turns {
            for block in &turn.blocks {
                if let Block::Msg { content, .. } = block {
                    for path in marker_paths(content) {
                        let key = path.to_string_lossy().to_string();
                        if !seen.contains(&key) {
                            seen.push(key);
                        }
                    }
                }
            }
        }
        seen
    }
}

/// Every path whose tile must carry a click handler: the stored turns, plus
/// the rows the live turn draws through that same block renderer. Only stored
/// rows were counted before, so a picture sent a second ago was tiled but dead
/// until the turn settled and its row got written (#2395).
pub fn attachment_paths_with_live(state: &TranscriptState, live: &LiveTurn) -> Vec<String> {
    let mut keys = state.attachment_paths();
    for text in live.user.iter().chain(live.injected.iter()) {
        for path in marker_paths(text) {
            let key = path.to_string_lossy().to_string();
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    keys
}

/// One transcript read: the rows, the total the family holds, and which topic
/// each row came from when the read merged several sessions (#2762).
pub struct Loaded {
    pub rows: Vec<Message>,
    pub total: i64,
    /// Row id to topic label. Empty for a single session, which is every read
    /// that is not a forum's `All` view.
    pub origins: HashMap<uuid::Uuid, String>,
}

/// Fetch the newest window of the conversation plus the session's total row
/// count, on a thread with a real stack (crash receipt in `main.rs`).
/// `Database::connect` runs pragmas only, never migrations.
///
/// A session that has topics comes back as its whole family: the parent plus
/// every topic under it, merged by time, with each row's topic recorded in
/// [`Loaded::origins`] (#2762). That is what `All` shows. A topic opened on its
/// own has no children, so it reads alone and carries no labels at all.
pub async fn load_recent(
    db_path: std::path::PathBuf,
    session_id: uuid::Uuid,
) -> Result<Loaded, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<Loaded, String>>();
    let spawned = std::thread::Builder::new()
        .name("transcript-fetch".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = MessageRepository::new(db.pool().clone());
                        let sessions = SessionRepository::new(db.pool().clone());
                        let family = sessions.family_of(session_id).await?;
                        // One row in the family is the ordinary read this
                        // function has always made.
                        if family.len() <= 1 {
                            let rows = repo.find_recent_by_session(session_id, WINDOW).await?;
                            let total = repo.count_by_session(session_id).await?;
                            return Ok::<_, anyhow::Error>(Loaded {
                                rows,
                                total,
                                origins: HashMap::new(),
                            });
                        }
                        let mut rows: Vec<Message> = Vec::new();
                        let mut origins: HashMap<uuid::Uuid, String> = HashMap::new();
                        let mut total: i64 = 0;
                        for (id, title) in family {
                            let label = topic_family_label(&title);
                            let window = repo.find_recent_by_session(id, WINDOW).await?;
                            total += repo.count_by_session(id).await?;
                            for row in &window {
                                // A label is only worth carrying when the
                                // family has more than one name to tell apart,
                                // which is exactly this branch.
                                if let Some(label) = &label {
                                    origins.insert(row.id, label.clone());
                                }
                            }
                            rows.extend(window);
                        }
                        // Same order the TUI's reader gets from SQL. The id
                        // breaks a tie between two rows stamped the same
                        // second, so the union is stable across reloads.
                        rows.sort_by_key(|m| (m.created_at, m.id));
                        Ok::<_, anyhow::Error>(Loaded {
                            rows,
                            total,
                            origins,
                        })
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("transcript fetch thread dropped its result".into()),
        },
        Err(e) => Err(format!("transcript fetch thread failed to spawn: {e}")),
    }
}

/// What a stored session title reads as once the bookkeeping is off it. `All`
/// labels every row with this, and the label names the conversation the row
/// came from, never the transport (#2766): a topic reads `Memes`, and a row
/// from the base chat reads `OpenCrabs Dev`. Before this the platform word and
/// its key came through whole, so every divider printed `Telegram: OpenCrabs
/// Dev / Memes [chat:-100:topic:42]` above the rows it was labelling.
fn topic_family_label(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(topic) = rail::topic_name(Some(trimmed)) {
        return Some(topic);
    }
    Some(rail::chat_name(Some(trimmed)))
}

/// A model-switch receipt used to be persisted with the user role. Draw the
/// legacy rows as the harness warning they are, so an old chat does not keep
/// them in user bubbles after the writer is fixed.
fn is_model_change_notice(content: &str) -> bool {
    let text = content.trim();
    text.starts_with("[Model changed to ") && text.ends_with(']')
}

/// Write the picked rows into another session (#2608), off the UI thread the
/// same way [`load_recent`] reads them: a std thread carrying its own tokio
/// runtime, so the window's executor never blocks on the pool.
///
/// The label names the source, and it is the source's own title as the rail
/// shows it. Returns how many rows landed.
pub async fn forward_rows(
    db_path: std::path::PathBuf,
    source: Vec<Message>,
    target: uuid::Uuid,
    label: String,
) -> Result<usize, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<usize, String>>();
    let spawned = std::thread::Builder::new()
        .name("transcript-forward".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt
                    .block_on(async {
                        let db = Database::connect(&db_path).await?;
                        let repo = MessageRepository::new(db.pool().clone());
                        let ids = repo.forward(&source, target, &label).await?;
                        Ok::<_, anyhow::Error>(ids.len())
                    })
                    .map_err(|e| format!("{e:#}")),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("forward thread dropped its result".into()),
        },
        Err(e) => Err(format!("forward thread failed to spawn: {e}")),
    }
}

/// Expand ONE stored row into its renderable blocks. Mirrors
/// `TuiApp::expand_message` (`tui/app/messaging.rs`): compaction markers are
/// context and never shown; tool ledgers become `Tools` blocks; reasoning
/// segments and intermediate narration become `Thinking`; plain rows stay
/// `Msg`. The `before[..ledger.start]` slices are byte-indexed but the scanner
/// only reports marker offsets it verified sit on line starts, so every offset
/// it returns is a char boundary (#1587 grammar).
fn row_blocks(m: &Message, forward: Option<ForwardMark>) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    if m.role == PLAN_COMPLETED_ROLE {
        if let Some(payload) = CompletedPlanPayload::from_message(m) {
            blocks.push(Block::CompletedPlan {
                message_id: m.id,
                plan: CompletedPlanView::new(&payload.plan),
            });
        }
        return blocks;
    }
    if m.content.starts_with("[CONTEXT COMPACTION") {
        return blocks;
    }
    // A hidden prompt is not a conversation row (#2756). Rows written before
    // the display line existed still hold the `[SYSTEM: ...]` scaffolding, and
    // a row persisted while a turn was in flight can too; neither may render.
    if m.role == "user" && is_internal_system_message(&m.content) {
        return blocks;
    }
    if m.role != "assistant" {
        let role = if m.role == "user" && is_model_change_notice(&m.content) {
            "system"
        } else {
            m.role.as_str()
        };
        blocks.push(Block::Msg {
            role: role.to_string(),
            content: m.content.clone(),
            time: stamp(m),
            placeholder: false,
            message_id: Some(m.id),
            forward,
        });
        return blocks;
    }
    let lower = m.content.to_lowercase();
    let has_db_thinking = m.thinking.as_deref().is_some_and(|t| !t.trim().is_empty());
    let has_tool_tag = m.content.contains("\u{fe0f}\u{20e3}") || m.content.contains("<tool_call>");
    let expandable = m.content.contains("<!-- tools")
        || m.content.contains("<!-- reasoning -->")
        || lower.contains("<antthinking")
        || lower.contains("<think")
        || lower.contains("<mm:think")
        || has_db_thinking
        || has_tool_tag;
    if !expandable {
        blocks.push(Block::Msg {
            role: "assistant".into(),
            content: m.content.clone(),
            time: stamp(m),
            placeholder: false,
            message_id: Some(m.id),
            forward,
        });
        return blocks;
    }
    let row_start = blocks.len();
    let mut before: &str = m.content.as_str();
    while let Some(ledger) = next_ledger(before) {
        if let Some(head) = before.get(..ledger.start) {
            let text_before = head.trim();
            if !text_before.is_empty() {
                push_text(&mut blocks, text_before);
            }
        }
        blocks.push(Block::Tools {
            calls: parse_calls(ledger.body, ledger.is_v2),
        });
        before = before.get(ledger.end..).unwrap_or_default();
    }
    let trailing = before.trim();
    if !trailing.is_empty() {
        push_text(&mut blocks, trailing);
    }
    if has_db_thinking {
        let thinking = m.thinking.as_deref().unwrap_or_default().to_string();
        blocks.push(Block::Thinking { content: thinking });
    }
    if !has_db_thinking
        && trailing.is_empty()
        && blocks[row_start..]
            .iter()
            .all(|b| !matches!(b, Block::Msg { .. }))
    {
        // Tool-ledger-only row: the placeholder says so instead of a blank gap
        // between bubbles. Flagged so the fold never mistakes it for an answer.
        blocks.insert(
            row_start,
            Block::Msg {
                role: "assistant".into(),
                content: "\u{b7} (tool calls only)".into(),
                time: stamp(m),
                placeholder: true,
                message_id: Some(m.id),
                forward: None,
            },
        );
    }
    blocks
}

/// Split the stored rows into turns and fold-ready headers, the same shape
/// `tui::render::chat::turn_ranges` + `turn_summary` + `format_turn_header`
/// produce for the terminal. A turn opens at a user row and runs to the next
/// one, so the question, the working-out and the answer belong together.
///
/// Returns the count of rows the window hid, and the turns in order.
///
/// `metrics` carries the per-turn ctx and rate this process still remembers,
/// keyed by turn anchor (#2324): a turn keeps them across reloads of the same
/// session and forgets them only when the process does.
///
/// `origins` is the per-row provenance of a merged `All` view (#2762): it maps
/// a row id to the topic that row lives in, so every turn carries the label its
/// first row was written under. A change of label is a hard turn boundary,
/// because without one the previous topic's answer would be drawn as the next
/// topic's output.
pub fn turns_with_origins(
    rows: &[Message],
    total: i64,
    metrics: &HashMap<String, TurnMetrics>,
    origins: &HashMap<uuid::Uuid, String>,
) -> (usize, Vec<Turn>) {
    let hidden = total.saturating_sub(rows.len() as i64).max(0) as usize;
    if rows.is_empty() {
        return (hidden, Vec::new());
    }
    // Same range rule as the TUI: a user row opens a turn, `i > start` keeps
    // back-to-back user rows in turns of their own.
    let mut bounds: Vec<(usize, usize)> = Vec::new();
    let mut start = 0usize;
    for (i, m) in rows.iter().enumerate() {
        // A forwarded row is stored as `user` so the destination's next turn
        // never reads another session's answer as its own output, but it must
        // not open a turn of its own: every row of one batch belongs to the
        // same block, under one header (#2608).
        let continues_batch = i > 0
            && m.forward.is_some()
            && m.forward.as_ref().map(|f| f.batch) == rows[i - 1].forward.as_ref().map(|f| f.batch);
        // A merged read sorts several topics into one stream, and the seam
        // between them is a boundary like any question is (#2762).
        let origin_changed = i > start && origins.get(&rows[i - 1].id) != origins.get(&m.id);
        if i > start && (origin_changed || (m.role == "user" && !continues_batch)) {
            bounds.push((start, i));
            start = i;
        }
    }
    bounds.push((start, rows.len()));

    let mut out = Vec::with_capacity(bounds.len());
    // A forward batch is contiguous, so the row that opens it is the one whose
    // batch differs from the row before (#2608).
    let mut prev_batch: Option<uuid::Uuid> = None;
    for (s, e) in bounds {
        let mut blocks: Vec<Block> = Vec::new();
        for m in &rows[s..e] {
            let mark = m.forward.as_ref().map(|f| ForwardMark {
                label: f.label.clone(),
                batch: f.batch,
                first: prev_batch != Some(f.batch),
            });
            prev_batch = m.forward.as_ref().map(|f| f.batch);
            blocks.extend(row_blocks(m, mark));
        }
        // The final answer: the LAST assistant block that carries real text.
        // The "(tool calls only)" filler is excluded, exactly as the TUI's
        // `final_answer_idx` requires non-empty content.
        let final_idx = blocks.iter().enumerate().rev().find_map(|(i, b)| match b {
            Block::Msg {
                role,
                content,
                placeholder,
                ..
            } if role == "assistant" && !*placeholder && !content.trim().is_empty() => Some(i),
            _ => None,
        });
        // The turn's anchor and remembered numbers are read before the
        // summary so the tally the live card showed can floor it (#2632).
        let anchor = format!("{}@{}", rows[s].role, rows[s].created_at.timestamp());
        // The label the turn opens under. A turn cannot straddle two topics
        // (the boundary above forbids it), so the first row speaks for the
        // whole turn (#2762).
        let origin = origins.get(&rows[s].id).cloned();
        let metrics = metrics.get(&anchor).copied();
        let mut summary = TurnSummary::default();
        for b in &blocks {
            match b {
                Block::Tools { calls } => {
                    summary.tool_calls += calls.len();
                    summary.failed += calls.iter().filter(|c| !c.success).count();
                }
                Block::Msg { role, .. } if role == "error" => summary.has_error = true,
                _ => {}
            }
        }
        // Prefer the duration stamped at turn end (#964, #2323), the same
        // order `tui::render::chat::turn_summary` uses. The assistant row is
        // created at turn START and updated in place, so subtracting stored
        // timestamps collapses to zero for almost every turn: that subtraction
        // was the whole reason a settled turn lost its timer.
        if let Some(secs) = rows[s..e].iter().rev().find_map(|m| m.duration_secs) {
            summary.duration_secs = secs.max(0);
        } else if let (Some(first), Some(last)) = (rows.get(s), rows.get(e - 1)) {
            summary.duration_secs = (last.created_at - first.created_at).num_seconds().max(0);
        }
        // A persisted row that lost its ledger must not erase the calls the
        // user watched in flight: the settled card keeps the live count
        // (#2632).
        if let Some(m) = metrics {
            summary.tool_calls = summary.tool_calls.max(m.tool_calls);
        }
        // Steps are the rows the fold actually hides, never the turn's row
        // count: the question and the answer are never folded away.
        let steps = (0..blocks.len())
            .filter(|&i| !visible_when_folded(&blocks[i], final_idx, i))
            .count();
        let header = format_turn_header(summary, steps);
        let preview = blocks.iter().rev().find_map(|b| match b {
            Block::Tools { calls } => calls.last().map(|c| (c.description.clone(), c.success)),
            _ => None,
        });
        out.push(Turn {
            header,
            summary,
            metrics,
            preview,
            anchor,
            blocks,
            final_idx,
            origin,
        });
    }
    (hidden, out)
}

/// Whether the persisted rows of one turn stay out of the conversation
/// because the live card is already drawing that same exchange.
///
/// Only the newest turn can be the one on screen, and only while the card
/// still owns it. Every other turn prints from its rows, including the turn
/// the reader just finished: a new request sets `busy` for its whole
/// duration, and keying this on `busy` alone hid the previous answer until
/// the new turn ended (#2709).
pub fn persisted_turn_is_hidden(ti: usize, last: usize, newest_is_live: bool) -> bool {
    ti == last && newest_is_live
}

/// Whether this persisted turn is the exchange the live card is drawing.
///
/// The window keeps a live turn beside the rows it reloads, so the newest
/// persisted turn and the live card can describe the same exchange. Identity
/// is the question plus the second it was sent, never the row's position: a
/// reload can install a different newest turn while a card keeps running, and
/// the same text asked again later is a different turn.
pub fn newest_live_turn_matches(turn: &Turn, live: &LiveTurn) -> bool {
    let Some(question) = live.user.as_deref() else {
        return false;
    };
    let question = question.trim();
    if question.is_empty() {
        return false;
    }
    let Some(Block::Msg {
        role,
        content,
        time,
        ..
    }) = turn.blocks.first()
    else {
        return false;
    };
    if role != "user" || content.trim() != question {
        return false;
    }
    let Some(sent) = live.sent_at else {
        return true;
    };
    // A reload already read this question back out of the rows, so the turn
    // whose question it is has been identified once and the seconds no longer
    // have to agree: a write that landed late must not hand the same exchange
    // back to the persisted copy while the card is still drawing it.
    if live.landed {
        return true;
    }
    // The row is stamped when the write landed, which can cross a second
    // boundary, so a couple of seconds either way still reads as the same
    // exchange. Text alone would also swallow a question the reader asked
    // twice, and the previous answer would vanish while the new turn ran.
    time.is_empty()
        || [-2, -1, 0, 1, 2].iter().any(|offset| {
            sent.checked_add_signed(chrono::TimeDelta::seconds(*offset))
                .is_some_and(|at| live_time(Some(at)) == *time)
        })
}

/// Whether one block still renders while its turn is folded. Mirrors
/// `tui::render::chat::visible_when_folded`: the question, errors, harness
/// status and the final answer stay; thinking, tool ledgers and intermediate
/// narration fold away.
pub fn visible_when_folded(block: &Block, final_idx: Option<usize>, idx: usize) -> bool {
    match block {
        Block::Msg {
            role,
            content,
            placeholder,
            ..
        } => match role.as_str() {
            // Harness status sits with errors: a model switch, a retry or a
            // phantom warning must never be hidden behind a fold (#786).
            "user" | "error" | "system" | "history_marker" | "tool" => true,
            "assistant" => {
                (!*placeholder && Some(idx) == final_idx) || is_deliverable_report(content)
            }
            _ => true,
        },
        Block::Thinking { .. } | Block::Tools { .. } => false,
        Block::CompletedPlan { .. } | Block::Marker { .. } => true,
    }
}

/// Split a text region into thinking / narration / answer segments, the same
/// walk `push_segments` does in the TUI.
fn push_text(blocks: &mut Vec<Block>, region: &str) {
    let segments = split_segments(region);
    for (i, seg) in segments.iter().enumerate() {
        match seg {
            Segment::Text(t) => {
                if is_intermediate(&segments, i) {
                    if !t.trim().is_empty() {
                        blocks.push(Block::Thinking {
                            content: t.to_string(),
                        });
                    }
                } else {
                    blocks.push(Block::Msg {
                        role: "assistant".into(),
                        content: t.to_string(),
                        time: String::new(),
                        placeholder: false,
                        message_id: None,
                        forward: None,
                    });
                }
            }
            Segment::Reasoning(r) => blocks.push(Block::Thinking { content: r.clone() }),
            reasoning_split::Segment::Blocked(b) => blocks.push(Block::Thinking {
                content: format!("{}\n\n{}", reasoning_split::BLOCKED_LABEL, b),
            }),
        }
    }
}

/// Ledger body to calls: v2 carries JSON with status, v1 is descriptions.
fn parse_calls(body: &str, is_v2: bool) -> Vec<ToolCall> {
    if is_v2 {
        serde_json::from_str::<Vec<serde_json::Value>>(body)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| ToolCall {
                name: String::new(),
                description: entry["d"].as_str().unwrap_or("?").to_string(),
                success: entry["s"].as_bool().unwrap_or(true),
                done: true,
                task_id: None,
            })
            .collect()
    } else {
        body.split(" | ")
            .map(|d| ToolCall {
                name: String::new(),
                description: d.to_string(),
                success: true,
                done: true,
                task_id: None,
            })
            .collect()
    }
}

fn stamp(m: &Message) -> String {
    m.created_at
        .with_timezone(&chrono::Local)
        .format("%m-%d %H:%M:%S")
        .to_string()
}

/// Role accent: the same three-colour grammar the dashboard bars use.
fn role_color(role: &str) -> u32 {
    match role {
        "user" => palette::current().blue,
        "assistant" => palette::current().green,
        "system" | "tool" => palette::current().warn,
        _ => palette::current().text_muted,
    }
}

/// Mix two packed RGB colors without introducing a theme-specific literal.
/// The panel supplies the base tone and the role accent supplies a small tint.
fn mix_rgb(base: u32, tint: u32, percent: u32) -> u32 {
    let percent = percent.min(100);
    let channel = |shift: u32| {
        let base = (base >> shift) & 0xff;
        let tint = (tint >> shift) & 0xff;
        (base * (100 - percent) + tint * percent) / 100
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

fn palette_is_dark(color: u32) -> bool {
    let r = (color >> 16) & 0xff;
    let g = (color >> 8) & 0xff;
    let b = color & 0xff;
    (r * 299 + g * 587 + b * 114) / 1000 < 128
}

/// The user bubble is one small neutral step off the history canvas: a touch
/// lighter on dark themes, a touch darker on light ones. No role colour is
/// mixed in, so the bubble reads as depth and never competes with the text
/// (#2570).
fn message_surface_for(palette: &palette::Palette, role: &str) -> u32 {
    if role != "user" {
        return palette.bg;
    }
    let step = 6;
    if palette_is_dark(palette.bg) {
        mix_rgb(palette.bg, 0xff_ff_ff, step)
    } else {
        mix_rgb(palette.bg, 0x00_00_00, step)
    }
}

fn message_surface(role: &str) -> u32 {
    message_surface_for(&palette::current(), role)
}

fn body_style(size: f32) -> TextStyle {
    TextStyle {
        font_family: crate::fonts::current_ui().into(),
        color: rgb(palette::current().text).into(),
        font_size: AbsoluteLength::Pixels(px(size)),
        ..Default::default()
    }
}

/// A small line of text in a fixed colour, for the rows that are not messages:
/// tool calls, the completed plan card, the live card's ledger.
pub(crate) fn small_style(size: f32, color: u32) -> TextStyle {
    TextStyle {
        font_family: crate::fonts::current_ui().into(),
        color: rgb(color).into(),
        font_size: AbsoluteLength::Pixels(px(size)),
        ..Default::default()
    }
}

fn mono_style() -> TextStyle {
    TextStyle {
        // The mono walk, not a fixed name (#2786): `Menlo` ships on macOS
        // only, and the platform resolves an unknown family through a chain
        // whose entries are proportional, so a code block used to lose column
        // alignment on Windows and Linux.
        font_family: crate::fonts::current_mono().into(),
        color: rgb(palette::current().text_normal).into(),
        font_size: AbsoluteLength::Pixels(px(12.0)),
        ..Default::default()
    }
}

/// Heading size ladder (#2521): h1 20px, h2 17px, h3+ 15px, body 13px.
fn heading_size(level: u8) -> f32 {
    match level {
        1 => 20.0,
        2 => 17.0,
        3..=6 => 15.0,
        _ => 13.0,
    }
}

/// Headings carry more weight than the 13px regular body text.
fn heading_weight(level: u8) -> gpui::FontWeight {
    match level {
        1..=2 => gpui::FontWeight::BOLD,
        3..=6 => gpui::FontWeight::MEDIUM,
        _ => gpui::FontWeight::NORMAL,
    }
}

/// One styled markdown line; indent in list/quote depth steps.
fn md_line(l: &markdown::MdLine, selection: Option<&SelectionContext>) -> Div {
    let size = heading_size(l.heading_level);
    let mut style = body_style(size);
    style.font_weight = heading_weight(l.heading_level);
    let text = inline_crabs(
        l.text.clone(),
        style,
        l.highlights.clone(),
        &l.links,
        selection,
    );
    let base = div().pl(rems(f32::from(l.indent) * 0.9));
    if l.heading {
        base.mt(px(RHYTHM_HEADING_ABOVE))
            .mb(px(RHYTHM_HEADING_BELOW))
            .child(text)
    } else {
        base.child(text)
    }
}

/// One table cell. Every cell is `flex_1` with `min_w_0`, so the grid takes
/// the width the window actually has and the columns share it: a wide window
/// gets wide columns, a narrow one wraps the text instead of clipping it.
fn table_cell(cell: &markdown::MdCell, header: bool, selection: Option<&SelectionContext>) -> Div {
    let style = TextStyle {
        font_family: crate::fonts::current_ui().into(),
        color: rgb(if header {
            palette::current().text_strong
        } else {
            palette::current().text_normal
        })
        .into(),
        font_size: AbsoluteLength::Pixels(px(13.0)),
        ..Default::default()
    };
    let text = selectable_styled_text(
        cell.text.clone(),
        style,
        cell.highlights.clone(),
        &cell.links,
        selection,
    );
    div()
        .flex_1()
        .min_w_0()
        .px(px(RHYTHM_CELL_PAD_X))
        .py(px(RHYTHM_CELL_PAD_Y))
        .child(text)
}

/// The topic a run of rows belongs to, drawn above the first turn of that
/// topic in a merged `All` view (#2762): the name, then a hairline out to the
/// edge. `rule` is false only at the very top of the transcript, where nothing
/// sits above the label to be separated from.
fn origin_divider(label: &str, rule: bool) -> Div {
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .min_w_0()
        .pt(px(10.0))
        .pb(px(2.0));
    if rule {
        row = row.border_t_1().border_color(rgb(palette::current().track));
    }
    row.child(
        div()
            .text_xs()
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(rgb(palette::current().text_muted))
            .flex_shrink_0()
            .child(SharedString::from(label.to_string())),
    )
    .child(table_rule().flex_1())
}

/// A rule between two rows, drawn as its own element so no table depends on
/// border-side helpers the way the TUI's box drawing does not either.
fn table_rule() -> Div {
    div()
        .h(px(1.0))
        .w_full()
        .flex_none()
        .bg(rgb(palette::current().track))
}

/// A markdown table as a real grid: header band, a rule under it, then one
/// flex row per record with the same column count. This is the shape the TUI
/// draws in box characters; the GUI draws it in panels.
fn md_table(t: &markdown::MdTable, selection: Option<&SelectionContext>) -> Div {
    let cols = t.columns().max(1);
    let empty = markdown::MdCell {
        text: String::new(),
        highlights: Vec::new(),
        links: Vec::new(),
    };
    let mut grid = div()
        .flex()
        .flex_col()
        .w_full()
        .border_1()
        .border_color(rgb(palette::current().track))
        .rounded(px(CARD_RADIUS))
        .overflow_hidden()
        .mt(px(RHYTHM_BLOCK_MARGIN))
        .mb(px(RHYTHM_BLOCK_MARGIN));

    let mut head = div()
        .flex()
        .flex_row()
        .items_start()
        .w_full()
        // Match the table's own top corners exactly. A smaller inner radius
        // leaves a visible step between the header band and the card edge.
        .rounded_tl(px(CARD_RADIUS))
        .rounded_tr(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().panel_hi));
    for i in 0..cols {
        head = head.child(table_cell(
            t.header.get(i).unwrap_or(&empty),
            true,
            selection,
        ));
    }
    grid = grid.child(head).child(table_rule());

    for (n, row) in t.rows.iter().enumerate() {
        if n > 0 {
            grid = grid.child(table_rule());
        }
        let mut line = div().flex().flex_row().items_start().w_full();
        for i in 0..cols {
            line = line.child(table_cell(row.get(i).unwrap_or(&empty), false, selection));
        }
        grid = grid.child(line);
    }
    grid
}

/// Vertical rhythm ladder (#2519): one table for every block gap so the
/// chat reads as typeset content instead of a text dump. Base gap for
/// paragraphs and list lines; headings and big blocks (tables, code fences)
/// carry real margins above and below; table cells breathe inside the grid.
const RHYTHM_BASE_GAP: f32 = 4.0;
const RHYTHM_HEADING_ABOVE: f32 = 12.0;
const RHYTHM_HEADING_BELOW: f32 = 4.0;
const RHYTHM_BLOCK_MARGIN: f32 = 10.0;
const RHYTHM_CELL_PAD_X: f32 = 16.0;
const RHYTHM_CELL_PAD_Y: f32 = 6.0;

/// Message text sits inside the rounded surface, never on its curved edge.
const MESSAGE_PAD_X: f32 = 16.0;
const MESSAGE_PAD_Y: f32 = 12.0;

/// The message surface carries a tighter corner than the cards around it, so a
/// column of chat reads as conversation and not as a stack of panels. This
/// token belongs to the message bubble alone and never reaches `CARD_RADIUS`
/// or `SEEKER_RADIUS` (#2571).
/// A forwarded copy gets a bar, not a bubble (#2608): the bar is what tells a
/// reader at a glance that this text came from somewhere else, and 3px is thin
/// enough to sit inside the row without pushing the text off its rhythm.
const FORWARD_BAR_WIDTH: f32 = 3.0;
/// The gap between that bar and the forwarded text.
const FORWARD_BAR_GAP: f32 = 12.0;
const MESSAGE_RADIUS: f32 = 12.0;
const USER_TEXT_SIZE: f32 = 16.0;
const PLAIN_TEXT_SIZE: f32 = 13.0;
/// Tool calls and card lines: the `text_xs` size the pane draws them at, so a
/// selectable segment measures exactly the box it paints in.
pub(crate) const SMALL_TEXT_SIZE: f32 = 12.0;

/// How many distinct markdown sources the cache holds before the oldest are
/// dropped (#2688).
const MD_CACHE_ENTRIES: usize = 512;

/// The source bytes the cache may hold. Parsed blocks cost more than the
/// source, so this is a budget on the input, not the resident set.
const MD_CACHE_BYTES: usize = 16 * 1024 * 1024;

/// Parsed markdown per source string (#2688).
///
/// The transcript rebuilds every row of every pane on each frame, and the
/// running spinner notifies the whole window twenty times a second. Parsing
/// was the expensive half of that: a 4 MB session re-ran `markdown::blocks`
/// over every visible answer at frame rate. Parsing depends on the source
/// text alone, so the parse is cached and only the cheap element tree is
/// rebuilt per frame.
#[derive(Default)]
struct MdCache {
    by_key: HashMap<(u64, usize), Rc<Vec<markdown::MdBlock>>>,
    /// Insertion order, oldest first, for eviction.
    order: VecDeque<(u64, usize)>,
    /// Source bytes currently held, for the byte budget.
    bytes: usize,
}

impl MdCache {
    fn get(&mut self, key: (u64, usize)) -> Option<Rc<Vec<markdown::MdBlock>>> {
        self.by_key.get(&key).cloned()
    }

    fn insert(&mut self, key: (u64, usize), blocks: Rc<Vec<markdown::MdBlock>>) {
        let weight = key.1;
        if self.by_key.insert(key, blocks).is_none() {
            self.order.push_back(key);
            self.bytes += weight;
        }
        while (self.order.len() > MD_CACHE_ENTRIES || self.bytes > MD_CACHE_BYTES)
            && self.order.len() > 1
        {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if self.by_key.remove(&old).is_some() {
                self.bytes = self.bytes.saturating_sub(old.1);
            }
        }
    }
}

thread_local! {
    static MD_CACHE: RefCell<MdCache> = RefCell::new(MdCache::default());
}

/// The parsed blocks for `src`, from the cache when this source was already
/// parsed (#2688). Keyed on a hash of the whole string plus its length, so a
/// different answer never reads another one's parse.
fn md_blocks(src: &str) -> Rc<Vec<markdown::MdBlock>> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut hasher);
    let key = (hasher.finish(), src.len());
    if let Some(hit) = MD_CACHE.with(|c| c.borrow_mut().get(key)) {
        return hit;
    }
    let parsed = Rc::new(markdown::blocks(src));
    MD_CACHE.with(|c| c.borrow_mut().insert(key, parsed.clone()));
    parsed
}

/// Assistant content as markdown. Consecutive fence lines group under one
/// padded mono container so a code block reads as one shape, while a table
/// block keeps its grid instead of being flattened into text rows.
fn md_block(src: &str, selection: Option<&SelectionContext>) -> Div {
    let parsed = md_blocks(src);
    let blocks = parsed.as_slice();
    let mut out = div().flex().flex_col().gap(px(RHYTHM_BASE_GAP));
    let mut i = 0;
    while i < blocks.len() {
        match &blocks[i] {
            markdown::MdBlock::Table(table) => {
                out = out.child(md_table(table, selection));
                i += 1;
            }
            markdown::MdBlock::Line(line) if line.mono => {
                let mut code = div()
                    .flex()
                    .flex_col()
                    .bg(rgb(palette::current().panel_hi))
                    .rounded_sm()
                    .p_2()
                    .mt(px(RHYTHM_BLOCK_MARGIN))
                    .mb(px(RHYTHM_BLOCK_MARGIN))
                    .gap_0p5();
                while i < blocks.len() {
                    match &blocks[i] {
                        markdown::MdBlock::Line(l) if l.mono => {
                            code = code.child(selectable_styled_text(
                                l.text.clone(),
                                mono_style(),
                                l.highlights.clone(),
                                &[],
                                selection,
                            ));
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out = out.child(code);
            }
            markdown::MdBlock::Line(line) => {
                out = out.child(md_line(line, selection));
                i += 1;
            }
        }
    }
    out
}

/// User/system content stays plain: the TUI renders input verbatim, markdown
/// styling is an answer-text treatment. A line whose shortcodes name the crab
/// pack still draws those pictures (#2771), so a user who picks an emoji from
/// the composer sees the emoji, not its name.
fn plain_block(content: &str, size: f32, selection: Option<&SelectionContext>) -> Div {
    let mut lines = div().flex().flex_col().gap_0p5();
    for line in content.split('\n') {
        let style = body_style(size);
        lines = lines.child(inline_crabs(
            line.to_string(),
            style,
            Vec::new(),
            &[],
            selection,
        ));
    }
    lines
}

/// An inline crab is a touch larger than the body text it sits in, so it reads
/// as an emoji rather than a character glued into the line (#2771).
const CRAB_INLINE_SIZE: f32 = 1.1;

/// The crab a line of text draws at: the size of that line's own glyphs, a
/// little taller, so a heading keeps its scale.
fn inline_crab_size(style: &TextStyle) -> f32 {
    // The picture is laid out in rems, so a pixel size is converted at the
    // 16px base and a rem size is already in the unit the picture wants.
    match style.font_size {
        AbsoluteLength::Pixels(pixels) => (f32::from(pixels) * 1.15 / 16.0).max(CRAB_INLINE_SIZE),
        AbsoluteLength::Rems(rems) => (rems.0 * 1.15).max(CRAB_INLINE_SIZE),
    }
}

/// Draw one line of text, with the shortcodes that name a crab pack emoji
/// standing in as pictures (#2771).
///
/// A line with no token takes the plain `StyledText` path untouched, so nothing
/// about an ordinary row changes. A line that does carry tokens is laid out as
/// a wrapping flex row: a run of words is one text segment, and a token is the
/// pack's picture sitting in the text flow. Code fences and table cells keep
/// calling `selectable_styled_text` directly: a shortcode inside a fence is
/// source, not an emoji.
///
/// Every segment declares the length the whole line would have given it, never
/// its own byte count, because `SelectionContext` separates its segments with
/// one byte each: splitting a line into three pieces would otherwise move every
/// later offset down by two and drag-select would copy the wrong characters.
fn inline_crabs(
    text: String,
    style: TextStyle,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    links: &[(Range<usize>, String)],
    selection: Option<&SelectionContext>,
) -> Div {
    let Some(pieces) = topic_icons::inline_pieces(&text) else {
        return selectable_styled_text(text, style, highlights, links, selection);
    };
    // The row takes the width its parent gave it, so the wrap below has a
    // width to wrap at. Without `w_full` a flex row measures its children
    // against an indefinite width, and every chunk collapses to one character
    // wide and stacks the whole line into a vertical column (#2794).
    let mut row = div().flex().flex_wrap().items_center().w_full().min_w_0();
    let crab_size = inline_crab_size(&style);
    for piece in pieces {
        if let Some(bytes) = piece.crab {
            // A picture is not a segment: the run ahead of it already claims
            // the token's bytes, and one extra separator here would shift
            // every offset after the picture.
            row = row.child(topic_icons::picture(bytes, crab_size));
            continue;
        }
        if piece.range.is_empty() {
            // The spacer draws nothing: it only carries the bytes the pictures
            // ahead of the first word took.
            if let Some(context) = selection {
                context.begin_segment(piece.accounted);
            }
            continue;
        }
        let chunks = inline_chunks(&text, piece.range.clone(), piece.accounted);
        for (range, length) in chunks {
            let covered = local_highlights(&highlights, &range);
            let chunk_links = local_links(links, &range);
            let chunk = text[range].to_string();
            let drawn =
                selectable_text_segment(chunk, length, &style, covered, &chunk_links, selection);
            // A chunk keeps the width its own text needs and the row wraps
            // between chunks. `min_w_0` here was the bug: it let every chunk
            // shrink under its content, and a shrinking `StyledText` wraps one
            // character per line, so a whole message came out as vertical
            // columns of single letters (#2794). A word wider than the pane
            // now overflows its row instead of shredding the line.
            row = row.child(drawn.flex_none());
        }
    }
    row
}

/// One chunk of a run of text: the bytes it draws, and the length the selection
/// accounting consumes for it (#2771).
type Chunk = (Range<usize>, usize);

/// Cut one run of an inline-crab line into word-sized chunks, each with the
/// length the selection accounting must consume for it (#2771).
///
/// A flex row breaks between its children, never inside one, so a single long
/// run would overflow the pane instead of wrapping. Cutting at the spaces puts
/// the breaks where the text itself would have wrapped. A chunk ends after its
/// space, so a break never eats the gap between two words.
fn inline_chunks(text: &str, span: Range<usize>, accounted: usize) -> Vec<Chunk> {
    let (start, end) = (span.start, span.end);
    let mut chunks = Vec::new();
    let mut cursor = start;
    for (offset, byte) in text[start..end].bytes().enumerate() {
        if byte == b' ' {
            let cut = start + offset + 1;
            chunks.push(cursor..cut);
            cursor = cut;
        }
    }
    if cursor < end {
        chunks.push(cursor..end);
    }
    // The run ends where its own accounting says it does: one byte short of
    // the next segment, or at the end of the line for the last run on it.
    let run_end = start + accounted;
    let accounting: Vec<usize> = chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| match chunks.get(index + 1) {
            Some(next) => next.start.saturating_sub(chunk.start + 1),
            None => run_end.saturating_sub(chunk.start),
        })
        .collect();
    chunks.into_iter().zip(accounting).collect()
}

/// Keep the highlights that fall inside one chunk, rebased onto it, so a bold
/// span that crosses a crab keeps its weight on both sides of it (#2771).
fn local_highlights(
    highlights: &[(Range<usize>, HighlightStyle)],
    range: &Range<usize>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    highlights
        .iter()
        .filter_map(|(span, style)| {
            let from = span.start.max(range.start);
            let to = span.end.min(range.end);
            (from < to).then(|| (from - range.start..to - range.start, *style))
        })
        .collect()
}

/// Keep the link labels that fall inside one chunk, rebased onto it, so a link
/// split across a crab picture still opens from either side of it (#2317).
fn local_links(
    links: &[(Range<usize>, String)],
    range: &Range<usize>,
) -> Vec<(Range<usize>, String)> {
    links
        .iter()
        .filter_map(|(span, dest)| {
            let from = span.start.max(range.start);
            let to = span.end.min(range.end);
            if from < to {
                Some((from - range.start..to - range.start, dest.clone()))
            } else {
                None
            }
        })
        .collect()
}

/// Only the user message paints a bubble. The assistant answer is drawn
/// straight onto the history background, so a long reply does not read as one
/// more card (#2569).
fn bubble_has_surface(role: &str) -> bool {
    role == "user"
}

/// One message, drawn. The timestamp and content keep the same padding in both
/// roles so a folded history holds one rhythm, and only the user message
/// carries a surface behind them. That surface takes `MESSAGE_RADIUS`, so the
/// message corners stay tighter than the cards around them (#2571).
fn bubble(
    role: &str,
    content: &str,
    time: &str,
    content_width: f32,
    handlers: &HashMap<String, ClickHandler>,
    selection: Option<&SelectionContext>,
) -> Div {
    let paths = marker_paths(content);
    let text = strip_markers(content);
    // `min_w_0` lets a wide row shrink to the pane instead of pushing it wide,
    // and `flex_shrink_0` stops the column squeezing a row below the height its
    // tiles reserved. Before #2395 the row had neither, so a tile laid out
    // taller than `TILE` painted over the line above it.
    let attachment_width = (content_width - 2.0 * MESSAGE_PAD_X).max(0.0);
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .min_w_0()
        .flex_shrink_0()
        .px(px(MESSAGE_PAD_X))
        .py(px(MESSAGE_PAD_Y));
    if bubble_has_surface(role) {
        col = col
            .rounded(px(MESSAGE_RADIUS))
            .bg(glass_fill(message_surface(role)));
    }
    if !time.is_empty() {
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(role_color(role)))
                .child(SharedString::from(time.to_string())),
        );
    }
    if !text.trim().is_empty() {
        col = col.child(if role == "assistant" {
            md_block(&text, selection)
        } else {
            plain_block(
                &text,
                if role == "user" {
                    USER_TEXT_SIZE
                } else {
                    PLAIN_TEXT_SIZE
                },
                selection,
            )
        });
    }
    if !paths.is_empty() {
        col = col.child(history_attachments(&paths, attachment_width, handlers));
    }
    col
}

/// A forwarded copy (#2608): an accent bar down the left edge instead of a
/// bubble, the source named above the text, and the batch's later rows
/// continuing the same bar without repeating the header. That is the Telegram
/// shape the owner asked for, and it is what makes a forward distinguishable
/// from something the reader typed into this session.
fn forward_block(
    mark: &ForwardMark,
    content: &str,
    _time: &str,
    content_width: f32,
    handlers: &HashMap<String, ClickHandler>,
    selection: Option<&SelectionContext>,
) -> Div {
    let paths = marker_paths(content);
    let text = strip_markers(content);
    let attachment_width = (content_width - 2.0 * MESSAGE_PAD_X).max(0.0);
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .min_w_0()
        .flex_shrink_0()
        .pl(px(FORWARD_BAR_GAP))
        .pr(px(MESSAGE_PAD_X))
        .py(px(MESSAGE_PAD_Y))
        .border_l(px(FORWARD_BAR_WIDTH))
        .border_color(rgb(palette::current().accent));
    // The header names the source once per batch. Repeating it on every row
    // would turn a five-message forward into five stacked labels.
    if mark.first {
        col = col.child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(rgb(palette::current().teal))
                .child(SharedString::from(forward_header(&mark.label))),
        );
    }
    if !text.trim().is_empty() {
        col = col.child(plain_block(&text, PLAIN_TEXT_SIZE, selection));
    }
    if !paths.is_empty() {
        col = col.child(history_attachments(&paths, attachment_width, handlers));
    }
    col
}

/// The files a stored message carries, drawn as the files themselves: images
/// show their real pixels through `img`, videos and documents show their kind
/// plus the name. Filled from the `<<IMG:...>>` / `<<VID:...>>` markers the
/// send path writes into the message, which is why a reloaded session shows
/// exactly what was sent instead of a text stand-in for it.
///
/// `content_width` is the width the bubble's content gets. `attachment_view`
/// turns it into rows and one square tile box, and the strip reserves that
/// height BEFORE the images lay out. That reservation is the fix for #2395:
/// gpui re-derives an image's height from its own aspect ratio whenever the
/// height is left `Auto`, so every tile pins both sides here and lets
/// `object_fit` crop inside the box instead of growing it.
fn history_attachments(
    paths: &[std::path::PathBuf],
    content_width: f32,
    handlers: &HashMap<String, ClickHandler>,
) -> Div {
    let plan = attachment_view::plan(paths.len(), content_width);
    if plan.is_empty() {
        return div();
    }
    let side = plan.tile.w;
    let drawn = plan.cells() - usize::from(plan.folded > 0);
    let mut strip = div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .overflow_hidden()
        .gap_2()
        .pt_1();
    let mut index = 0usize;
    for cells in &plan.rows {
        let mut row = div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_start()
            .gap_2();
        for _ in 0..*cells {
            row = row.child(if index < drawn {
                let path = &paths[index];
                let key = path.to_string_lossy().to_string();
                // Cloned, not taken: the same file can appear in more than
                // one message, and every tile of it must open it.
                match handlers.get(&key).cloned() {
                    // A tile you can see is a tile you can open: the click
                    // goes to the overlay, which decides what the file is.
                    Some(handler) => attachment_cell(path, side)
                        .id(SharedString::from(format!("attachment-{key}")))
                        .cursor_pointer()
                        // gpui wants a real `Fn`, and `Rc<dyn Fn>` is not one,
                        // so each tile forwards into the shared handler.
                        .on_click(move |event, window, cx| handler(event, window, cx))
                        .into_any_element(),
                    None => attachment_cell(path, side).into_any_element(),
                }
            } else {
                fold_cell(plan.folded, side)
            });
            index += 1;
        }
        strip = strip.child(row);
    }
    strip
}

/// One attachment: the picture (or its kind glyph) in a square box, the file
/// name under it. The box owns the size, so the caption can never be the thing
/// that decides how tall the row is.
fn attachment_cell(path: &std::path::Path, side: f32) -> Div {
    let attachment = Attachment::for_display(path.to_path_buf());
    let tile: AnyElement = if attachment.is_image() {
        div()
            .w(px(side))
            .h(px(side))
            .flex_shrink_0()
            .overflow_hidden()
            .bg(glass_fill(palette::current().panel))
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(rgb(palette::current().track))
            // A square crop painted with `Fill`, so the painted quad is the
            // box and the radius lands inside the tile's own arc. `Cover` on
            // the original painted a larger quad and its arc sat outside the
            // tile's, which is what let pixels past the corner (#2796).
            .child(crate::thumbs::picture(path, px(concentric_radius(1.0))))
            .into_any_element()
    } else {
        // A document and a clip have nothing to draw, so the box states what a
        // thumbnail would have shown: the kind by its glyph, then how big it
        // is and the day it changed. One `stat`, on the same pass that already
        // loads the images beside it.
        let caption = crate::preview::FileFacts::probe(path).tile_caption();
        let mut boxed = div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .w(px(side))
            .h(px(side))
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(px(CARD_RADIUS))
            .border_1()
            .border_color(rgb(palette::current().track))
            .bg(glass_fill(palette::current().panel_hi))
            .text_lg()
            .child(if attachment.is_video {
                "\u{1f3ac}"
            } else {
                "\u{1f4c4}"
            });
        if let Some(line) = caption {
            boxed = boxed.child(
                div()
                    .px_1()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(palette::current().text_muted))
                    .child(SharedString::from(line)),
            );
        }
        boxed.into_any_element()
    };
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .w(px(side))
        .gap_1()
        .child(tile)
        .child(
            div()
                .truncate()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(attachment.name)),
        )
}

/// The last cell: how many files the strip did not draw.
fn fold_cell(folded: usize, side: f32) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .w(px(side))
        .h(px(side))
        .flex_shrink_0()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().track))
        .bg(glass_fill(palette::current().panel_hi))
        .text_sm()
        .text_color(rgb(palette::current().text_muted))
        .child(SharedString::from(format!("+{folded}")))
        .into_any_element()
}

/// A thinking block: dim, italic, collapsed to a bounded preview so a long
/// reasoning run cannot take over the pane. It draws as plain rows, never as a
/// box of its own: a nested surface rounder than the card that holds it read as
/// a second widget inside the first, and the caption above the rows said only
/// what the italic type already says (#2639, #2640).
fn thinking_block(content: &str, full: bool) -> Div {
    div().flex().flex_col().gap_0p5().min_w_0().child(
        div()
            .text_xs()
            .italic()
            .min_w_0()
            .text_color(rgb(palette::current().text_muted))
            .child(SharedString::from(if full {
                content.trim().to_string()
            } else {
                fit(content.trim(), 600)
            })),
    )
}

/// A tool ledger: each call with its status mark. The turn header already
/// counts the calls, so the expanded body does not repeat the count. Every call
/// is a row in one list, with no panel of its own: a fill and a radius rounder
/// than the card around it turned three bash calls into three widgets (#2639).
fn tools_block(calls: &[ToolCall], selection: Option<&SelectionContext>) -> Div {
    let mut col = div().flex().flex_col().gap_0p5().min_w_0();
    for call in calls {
        let (mark, color) = tool_mark(call);
        // The status mark is a glyph, not part of the copy text, so it is drawn
        // on its own. The description is the selectable segment, which is what
        // makes one call's name copyable on its own (#2678).
        col = col.child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .min_w_0()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(color))
                        .child(SharedString::from(mark)),
                )
                .child(selectable_styled_text(
                    tool_line(call),
                    small_style(SMALL_TEXT_SIZE, color),
                    Vec::new(),
                    &[],
                    selection,
                )),
        );
    }
    col
}

/// One call's drawn text, which is exactly the line its row copies. Both the
/// renderer and `block_text` go through here so a character offset can never
/// point at a different string than the one on screen.
fn tool_line(call: &ToolCall) -> String {
    fit(call.description.trim(), 160)
}

/// One call's status mark and the colour that carries it. Shared with the
/// ledger's text so a test cannot read a mark the pane never draws.
fn tool_mark(call: &ToolCall) -> (&'static str, u32) {
    let color = match (call.done, call.success) {
        (false, _) => palette::current().text_muted,
        (true, true) => palette::current().green,
        (true, false) => palette::current().red,
    };
    // The glyph is the Telegram trio, not the terminal's tick and cross: the
    // live card has to read like the card the other surfaces draw (#2442).
    (activity_glyph(call), color)
}

/// The expanded ledger as text. The count lives in the turn header, so this
/// must never mention it again (#2327).
#[cfg(test)]
fn tool_ledger_text(calls: &[ToolCall]) -> String {
    calls
        .iter()
        .map(|call| {
            let (mark, _) = tool_mark(call);
            format!("{mark} {}", fit(call.description.trim(), 160))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The chevron a fold header leads with: right when the turn is collapsed,
/// down when it is open. The glyph carries the affordance, so the header no
/// longer spells out which input folds it.
fn fold_chevron(folded: bool) -> &'static str {
    if folded { "\u{25B8}" } else { "\u{25BE}" }
}

/// The card a settled turn draws, the live card's own frame (#2632): one
/// rounded box holding the head row, the working-out rows when the turn is
/// open, and the footer that keeps the count, the ctx and the clock after the
/// turn ends. `None` for a turn that did no work, which earns no card (#758).
fn settled_card(turn: &Turn, folded: bool, body: Vec<AnyElement>) -> Option<Stateful<Div>> {
    turn.header.as_ref()?;
    let (glyph, verb) = settled_verdict(turn);
    let failed = glyph == "\u{274c}";
    let accent = if failed {
        palette::current().red
    } else {
        palette::current().accent
    };
    let head = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .w_full()
        .min_w_0()
        .px_2()
        .pt_1()
        .pb_1()
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(glyph)),
        )
        .child(
            div()
                .text_xs()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(rgb(accent))
                .child(SharedString::from(verb)),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(fold_chevron(folded))),
        );
    let mut card = div()
        .id(SharedString::from(format!("card-{}", turn.anchor)))
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .rounded(px(MESSAGE_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel))
        .cursor_pointer()
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .child(head);
    if folded && let Some((description, success)) = &turn.preview {
        let color = if *success {
            palette::current().text_muted
        } else {
            palette::current().red
        };
        card = card.child(
            div()
                .px_2()
                .pb_1()
                .text_xs()
                .italic()
                .text_color(rgb(color))
                .child(SharedString::from(format!(
                    "\u{2514}\u{2500} {}",
                    fit(description.trim(), 160)
                ))),
        );
    }
    if !body.is_empty() {
        card = card.child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .px_2()
                .pb_1()
                .min_w_0()
                .overflow_hidden()
                .children(body),
        );
    }
    if let Some(footer) = turn_footer_text(turn) {
        card = card.child(
            div()
                .px_2()
                .pb_1()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(footer)),
        );
    }
    Some(card)
}

/// The verdict a settled turn's head carries: the Telegram flow block's own
/// pair (#480), which is what the live card becomes on `Done`. One word, never
/// a step count the reader has to total (#2632).
///
/// The tally is not the verdict (#2707). `summary.failed` counts every call
/// that returned `success: false`, and a refusal the model then worked around
/// is exactly that: a failed call inside a turn that finished its job. Reading
/// the tally painted an 85-call turn red over two self-corrected refusals and
/// a plan-gate advisory, which teaches the reader to ignore the cross. So the
/// question asked here is the one the reader is asking: did the turn end on a
/// failure? A failed call with assistant text after it was answered, and the
/// footer still reports the count for anyone who wants it.
fn settled_verdict(turn: &Turn) -> (&'static str, &'static str) {
    if turn.summary.has_error || (turn.summary.failed > 0 && !answered_after_the_last_failure(turn))
    {
        ("\u{274c}", "Failed")
    } else {
        ("\u{2705}", "Finished")
    }
}

/// Whether the turn's last failed call was followed by assistant text, the
/// shape of a wall the model climbed over instead of one it hit.
///
/// Only the LAST failed call counts: an answer between two failures does not
/// excuse the second, and a failure after the final text is still a dead end.
/// A placeholder row is structure, not an answer, so it never counts.
fn answered_after_the_last_failure(turn: &Turn) -> bool {
    let Some(last_failed) = turn.blocks.iter().rposition(|b| match b {
        Block::Tools { calls } => calls.iter().any(|c| !c.success),
        _ => false,
    }) else {
        return false;
    };
    turn.blocks[last_failed + 1..].iter().any(|b| match b {
        Block::Msg {
            role,
            content,
            placeholder,
            ..
        } => role == "assistant" && !*placeholder && !content.trim().is_empty(),
        _ => false,
    })
}

/// One settled turn's footer, wording and order taken from the Telegram flow
/// footer (#2632): the calls it made, what they cost in context, and the
/// clock. The live card prints the same line before the turn settles, so
/// finishing changes the verb and nothing else (#2638). The clock and the ctx
/// segment are dropped when this process has no record for the turn instead of
/// borrowing the session's current number (#2324 D3).
fn turn_footer_text(turn: &Turn) -> Option<String> {
    let summary = turn.summary;
    let mut parts: Vec<String> = Vec::new();
    if summary.tool_calls > 0 {
        parts.push(tool_count_label(summary.tool_calls));
    }
    if summary.failed > 0 {
        parts.push(format!("{} failed", summary.failed));
    }
    if let Some(segment) = turn.metrics.as_ref().and_then(|m| m.segment()) {
        parts.push(segment);
    }
    if summary.duration_secs > 0 {
        parts.push(clock_glyph(summary.duration_secs.max(0) as u64));
    }
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// `\u{23f1} M:SS`, the clock that closes every Telegram flow footer (ADR
/// 0005 Decision 13). `flow_chrome::clock_glyph` is `pub(crate)` in the library
/// crate and this file builds the `opencrabs-gui` binary, the same reason
/// `activity_glyph` is local (#2632).
fn clock_glyph(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("\u{23f1} {h}:{m:02}:{s:02}")
    } else {
        format!("\u{23f1} {m}:{s:02}")
    }
}

/// One drawn line of the card's body, in the order it happened. Pure data, so
/// a test can read exactly the order and the words the pane paints (#2442).
pub enum LiveCardLine {
    /// A tool row: its Telegram mark, the colour that carries it, and the call.
    Tool {
        mark: &'static str,
        color: u32,
        text: String,
    },
    /// Narration between calls, drawn as markdown.
    Narration(String),
    /// Reasoning, dimmed and capped.
    Reasoning(String),
}

/// The card's body as data: every entry in order, resolved against the rows it
/// names. An entry whose row is gone is skipped instead of drawn blank.
fn live_card_lines(live: &LiveTurn) -> Vec<LiveCardLine> {
    let mut lines = Vec::new();
    for entry in &live.entries {
        match entry {
            LiveEntry::Tool(index) => {
                let Some(call) = live.tools.get(*index) else {
                    continue;
                };
                let (mark, color) = tool_mark(call);
                lines.push(LiveCardLine::Tool {
                    mark,
                    color,
                    text: fit(call.description.trim(), 160),
                });
            }
            LiveEntry::Narration(text) => lines.push(LiveCardLine::Narration(text.clone())),
            LiveEntry::Reasoning(text) => {
                lines.push(LiveCardLine::Reasoning(fit(text.trim(), 600)))
            }
        }
    }
    lines
}

/// The card's body: the same lines, as elements. The body is one copy row whose
/// segments are drawn in order, so a drag over a single tool line copies that
/// line's name and nothing else (#2678).
fn live_card_body(live: &LiveTurn, drag: Option<&Rc<CopyDrag>>) -> Div {
    let lines = live_card_lines(live);
    let mut body = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .px_2()
        .pb_1()
        .min_w_0()
        .overflow_hidden();
    let row_text = lines
        .iter()
        .map(live_card_line_text)
        .collect::<Vec<_>>()
        .join("\n");
    let Some(d) = drag.filter(|_| !row_text.trim().is_empty()) else {
        for line in lines {
            body = body.child(live_card_line(line, None));
        }
        return body;
    };
    let ordinal = {
        let mut texts = d.texts.borrow_mut();
        texts.push(row_text);
        texts.len() - 1
    };
    let selection = SelectionContext::new(d, ordinal);
    for line in lines {
        body = body.child(live_card_line(line, Some(&selection)));
    }
    body
}

/// One body line's copy text. It matches what the line paints, so a character
/// offset in the row always points at the character under the pointer.
fn live_card_line_text(line: &LiveCardLine) -> String {
    match line {
        LiveCardLine::Tool { mark, text, .. } => format!("{mark} {text}"),
        LiveCardLine::Narration(text) => markdown_copy_text(text),
        LiveCardLine::Reasoning(text) => text.clone(),
    }
}

/// One body line, drawn. With a selection it becomes a selectable segment, which
/// is what lets a single name be copied out of the ledger.
fn live_card_line(line: LiveCardLine, selection: Option<&SelectionContext>) -> AnyElement {
    match line {
        LiveCardLine::Tool { mark, color, text } => selectable_styled_text(
            format!("{mark} {text}"),
            small_style(SMALL_TEXT_SIZE, color),
            Vec::new(),
            &[],
            selection,
        )
        .into_any_element(),
        LiveCardLine::Narration(text) => md_block(&text, selection).into_any_element(),
        LiveCardLine::Reasoning(text) => selectable_styled_text(
            text,
            small_style(SMALL_TEXT_SIZE, palette::current().text_muted),
            Vec::new(),
            &[],
            selection,
        )
        .into_any_element(),
    }
}

/// The live turn as ONE card, in the Telegram grammar (#2442): the header is
/// the live status, the body is every entry in the order it happened, and the
/// footer carries the count, the ctx budget, the rate and the clock. The pane
/// used to draw a ledger, then a status line BELOW it, then a bare reasoning
/// line, which is what made a busy turn read as a wall of rows with the
/// summary underneath them.
///
/// The corners are the message radius, not the card radius (#2618): this card
/// becomes the message row once the turn persists, so it must not be rounder
/// than the row it turns into.
fn live_card(
    live: &LiveTurn,
    expanded: bool,
    ctx: (Option<u32>, u32),
    drag: Option<&Rc<CopyDrag>>,
) -> Stateful<Div> {
    let frame = if live.settled.is_some() {
        "\u{2705}".to_string()
    } else {
        match live.started {
            Some(t) => SPINNER_FRAMES
                [(t.elapsed().as_millis() / 80) as usize % SPINNER_FRAMES.len()]
            .to_string(),
            None => SPINNER_FRAMES[0].to_string(),
        }
    };
    let (lead, _) = live_status_line(live);
    // #2468: the head row owns the card's width and the lead takes the slack
    // as a single line. Without a width of its own the row sized to its
    // content, the content was the lead, and the lead was measured against the
    // width it was about to define: gpui resolved that circle by giving the
    // text one character of room, so the lead stacked vertically until a zoom
    // forced a fresh measure.
    let mut head = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .w_full()
        .min_w_0()
        .px_2()
        .pt_1()
        .pb_1()
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(frame)),
        );
    if !lead.is_empty() {
        head = head.child(
            div()
                .text_xs()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(rgb(palette::current().accent))
                .child(SharedString::from(lead)),
        );
    }
    head = head.child(
        div()
            .text_xs()
            .text_color(rgb(palette::current().text_muted))
            .child(SharedString::from(fold_chevron(!expanded))),
    );
    let mut card = div()
        .id("live-card")
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .rounded(px(MESSAGE_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel))
        .cursor_pointer()
        .hover(|d| d.bg(rgb(palette::current().panel_hi)))
        .child(head);
    if expanded {
        card = card.child(live_card_body(live, drag));
    } else if let Some(paragraph) = collapsed_narration(live) {
        // Narration stays in the card, never in the chat history. The collapsed
        // card previews what the turn just said under its header, the way a
        // summary rides beneath its title; opening the card prints every
        // paragraph in order.
        card = card.child(
            div().px_2().pb_1().w_full().min_w_0().child(
                selectable_styled_text(
                    paragraph,
                    small_style(SMALL_TEXT_SIZE, palette::current().text_muted),
                    Vec::new(),
                    &[],
                    None,
                )
                .line_clamp(3),
            ),
        );
    }
    if let Some(footer) = live_card_footer(live, ctx) {
        card = card.child(
            div()
                .px_2()
                .pb_1()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(footer)),
        );
    }
    card
}

/// The card's footer, segment for segment the Telegram one: how many calls,
/// the ctx budget, the rate it is streaming at, and the clock. `None` before
/// the turn has anything to say, so an empty bar never sits under the header.
fn live_card_footer(live: &LiveTurn, ctx: (Option<u32>, u32)) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if !live.tools.is_empty() {
        parts.push(tool_count_label(live.tools.len()));
    }
    if ctx.1 > 0 {
        let tps = live.tokens_per_sec();
        let rate = (tps > 0.0).then_some(f64::from(tps));
        parts.push(opencrabs::utils::string::format_ctx_footer(
            ctx.0.unwrap_or(0),
            ctx.1,
            rate,
        ));
    }
    let elapsed = live.elapsed_secs();
    if elapsed > 0 {
        // The same clock the settled card prints, so finishing changes the
        // verdict and nothing else (#2632/#2638).
        parts.push(clock_glyph(elapsed));
    }
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// One block, drawn. The fold decides WHETHER a block renders; this decides
/// only what it looks like.
fn block_view(
    block: &Block,
    content_width: f32,
    handlers: &HashMap<String, ClickHandler>,
    selection: Option<&SelectionContext>,
) -> Div {
    match block {
        Block::Msg {
            content,
            time,
            forward: Some(mark),
            ..
        } => forward_block(mark, content, time, content_width, handlers, selection),
        Block::Msg {
            role,
            content,
            time,
            ..
        } => bubble(role, content, time, content_width, handlers, selection),
        Block::Thinking { content } => thinking_block(content, false),
        Block::Tools { calls } => tools_block(calls, selection),
        Block::CompletedPlan { message_id, plan } => div().child(
            crate::plan_strip::completed_plan_card(*message_id, plan, selection),
        ),
        Block::Marker { hidden } => div()
            .text_xs()
            .text_color(rgb(palette::current().text_muted))
            .child(SharedString::from(format!(
                "\u{2191} {hidden} older messages hidden",
            ))),
    }
}

/// The TUI's spinner frames, in its own order
/// (`tui/render/chat.rs`), so the crab blinks at the same cadence.
///
/// The sessions rail reads the same list (`rail_activity::frame`), so a row
/// and the turn card it reports blink in phase instead of at two tempos.
pub(crate) const SPINNER_FRAMES: [&str; 10] = [
    "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280f}",
];

/// A live timestamp rendered exactly like a persisted row's, so the bubble
/// does not change shape when the DB reload replaces it.
fn live_time(at: Option<chrono::DateTime<chrono::Local>>) -> String {
    at.map(|t| t.format("%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// The status glyph for a live call, the trio the Telegram flow uses
/// (`channels/telegram/flow.rs::tool_status_icon`): running, succeeded,
/// failed. It cannot be imported here because that helper is `pub(crate)` in
/// the library crate and this file builds the `opencrabs-gui` binary.
fn activity_glyph(call: &ToolCall) -> &'static str {
    match (call.done, call.success) {
        (false, _) => "\u{2699}\u{fe0f}",
        (true, true) => "\u{2705}",
        (true, false) => "\u{274c}",
    }
}

/// The turn's latest activity, worded like the card's leading segment
/// (#2326): the call running now, else the last one that ran. `None` until a
/// tool has started.
pub(crate) fn live_activity(live: &LiveTurn) -> Option<String> {
    let call = live
        .tools
        .iter()
        .rev()
        .find(|c| !c.done)
        .or_else(|| live.tools.last())?;
    let text = if call.description.trim().is_empty() {
        call.name.trim()
    } else {
        call.description.trim()
    };
    (!text.is_empty()).then(|| format!("{} {}", activity_glyph(call), fit(text, 160)))
}

/// `1 tool call`, `9 tool calls`: one spelling for the live line and for the
/// expanded turn's own count.
fn tool_count_label(count: usize) -> String {
    format!("{count} tool call{}", if count == 1 { "" } else { "s" })
}

/// The live line split into its accented lead and its muted tail (#2326).
///
/// The lead is what the turn is doing: the running call, else the last one
/// that ran, else "OpenCrabs is thinking..." while nothing has arrived at all.
/// The tail is how much it did (the call count) and the clock the terminal
/// builds with `format_turn_spinner_meta`.
pub(crate) fn live_status_line(live: &LiveTurn) -> (String, String) {
    let mut lead = live_activity(live);
    if lead.is_none()
        && live.tools.is_empty()
        && live.chunks == 0
        && live.stream.is_empty()
        && live.reasoning.is_empty()
    {
        lead = Some("OpenCrabs is thinking...".to_string());
    }
    let mut tail: Vec<String> = Vec::new();
    if !live.tools.is_empty() {
        tail.push(tool_count_label(live.tools.len()));
    }
    if let Some(meta) = format_turn_spinner_meta(live.elapsed_secs(), live.tokens) {
        tail.push(meta.trim().to_string());
    }
    (lead.unwrap_or_default(), tail.join(" \u{b7} "))
}

/// The line's whole text, as drawn (#2326), so a test reads exactly what the
/// user sees instead of inferring it from two halves. Test-only: the pane
/// draws the two halves separately so the lead can carry the accent colour.
#[cfg(test)]
fn live_status_text(live: &LiveTurn) -> String {
    match live_status_line(live) {
        (lead, rest) if lead.is_empty() => rest,
        (lead, rest) if rest.is_empty() => lead,
        (lead, rest) => format!("{lead} \u{b7} {rest}"),
    }
}

/// One block of the running turn, selectable while a drag is in flight. The
/// ordinal comes from the same counter the settled rows use, in build order, so
/// a drag that starts in the live card and ends in the transcript above (or the
/// other way round) stays one continuous range. Returns None when there is no
/// drag or the block carries no text, and the caller draws it the old way.
fn live_block(
    drag: Option<&Rc<CopyDrag>>,
    block: &Block,
    content_width: f32,
    handlers: &HashMap<String, ClickHandler>,
) -> Option<AnyElement> {
    let text = block_text(block);
    let d = drag.filter(|_| !text.trim().is_empty())?;
    let ordinal = {
        let mut texts = d.texts.borrow_mut();
        texts.push(text);
        texts.len() - 1
    };
    let selection = SelectionContext::new(d, ordinal);
    Some(
        selectable_row(
            d,
            ordinal,
            block_view(block, content_width, handlers, Some(&selection)),
        )
        .into_any_element(),
    )
}

/// How much of the newest narration the collapsed card previews. The preview is
/// a summary line, not the paragraph: the body carries the full text once the
/// card is opened.
const COLLAPSED_NARRATION_CHARS: usize = 280;

/// The narration a collapsed card previews, char-safe clipped. Narration is the
/// card's own prose and never a chat row of its own: a busy turn speaks between
/// dozens of calls, and printing those paragraphs into the history turns one
/// card into a wall. The body still prints every paragraph in order when the
/// card is expanded.
fn collapsed_narration(live: &LiveTurn) -> Option<String> {
    live.narration()
        .last()
        .map(|text| {
            // One source line only (#2731). gpui's line_clamp shapes each
            // newline-separated line against the budget left over from the
            // lines before it, and subtracts with overflow when an earlier
            // line already wrapped past the clamp, aborting the window. A
            // single source line is shaped once against the full budget, so
            // collapsing every whitespace run to one space removes the trigger.
            let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
            fit(&flat, COLLAPSED_NARRATION_CHARS)
        })
        .filter(|text| !text.is_empty())
}

/// The running turn, drawn with the transcript's own block grammar: the
/// question as a user bubble, tool rows with their live status, the streamed
/// answer as markdown, notices in amber. Nothing here folds: the turn has no
/// settled answer yet, and hiding the working-out would blank the pane
/// exactly when the user wants to see that something is happening.
fn live_view(
    live: &LiveTurn,
    content_width: f32,
    expanded: bool,
    ctx: (Option<u32>, u32),
    live_click: Option<&ClickHandler>,
    handlers: &HashMap<String, ClickHandler>,
    drag: Option<&Rc<CopyDrag>>,
) -> Div {
    let mut col = div().flex().flex_col().gap_2();
    if let Some(user) = &live.user {
        let block = Block::Msg {
            role: "user".into(),
            content: user.clone(),
            time: live_time(live.sent_at),
            placeholder: false,
            message_id: None,
            forward: None,
        };
        // A live turn stays on screen often enough, and for long enough, that
        // its text has to copy like any other: while the transcript is short of
        // a finished turn the live card is the only copy of the answer the user
        // can see, so it is the only copy there is to drag.
        col = col.child(match live_block(drag, &block, content_width, handlers) {
            Some(row) => row,
            None => block_view(&block, content_width, handlers, None).into_any_element(),
        });
    }
    for notice in &live.notices {
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().amber))
                .child(SharedString::from(notice.clone())),
        );
    }
    // Queued messages the agent took between tool calls, as user rows in
    // order, so a message typed mid-turn shows up where it entered the turn
    // rather than after it (#2441).
    for text in &live.injected {
        let block = Block::Msg {
            role: "user".into(),
            content: text.clone(),
            time: live_time(live.sent_at),
            placeholder: false,
            message_id: None,
            forward: None,
        };
        col = col.child(match live_block(drag, &block, content_width, handlers) {
            Some(row) => row,
            None => block_view(&block, content_width, handlers, None).into_any_element(),
        });
    }
    // ONE card for the whole turn: header, body when open, footer. The three
    // loose children it replaces (a ledger, a status line below it, and a bare
    // reasoning line) are what made a busy turn unreadable (#2442).
    let card = live_card(live, expanded, ctx, drag);
    col = col.child(match live_click {
        Some(click) => {
            let click = click.clone();
            card.on_click(move |event, window, cx| click(event, window, cx))
        }
        None => card,
    });
    if let Some(answer) = live.answer() {
        let block = Block::Msg {
            role: "assistant".into(),
            content: answer.to_string(),
            time: live_time(live.answer_at.or(live.sent_at)),
            placeholder: false,
            message_id: None,
            forward: None,
        };
        col = col.child(match live_block(drag, &block, content_width, handlers) {
            Some(row) => row,
            None => block_view(&block, content_width, handlers, None).into_any_element(),
        });
    }
    col
}

/// The transcript pane: scrollable, opening at the bottom via the caller's
/// handle, with the empty and error states rendered honestly. Each turn folds
/// by default, exactly as in the TUI; `busy` keeps the running turn's
/// in-flight text out of the fold so the answer is never hidden while it is
/// still being written.
/// Selection uses the same blue as the active theme, with enough alpha to
/// remain visible over both role surfaces. No role-specific colour is pinned.
fn selection_rgba() -> u32 {
    (palette::current().blue << 8) | 0x55
}

/// What a successful copy tells the view that owns the composer (#2587).
type CopiedListener = Rc<dyn Fn(&mut App)>;

/// The press that may turn into an open (#2317): the text under the pointer,
/// the link labels on it, and the byte offset the press landed on.
///
/// Resolving at press time and opening at release is what keeps the gesture
/// honest. Opening on press would fire before a drag had a chance to start,
/// so selecting a url to copy it would also launch the browser. Only a press
/// that ends without a selection opens anything.
struct PendingClick {
    /// The drawn text the pointer was over, so a click offset can be turned
    /// back into a token without re-rendering the row.
    text: String,
    /// Link labels on that text with their destinations, already rebased onto
    /// it. A label wins over token resolution because `docs` carries no url.
    links: Vec<(Range<usize>, String)>,
    /// Byte offset into `text`, as the shaped layout reported it.
    offset: usize,
}

impl PendingClick {
    /// The destination this press named, if any: the label it landed on
    /// first, then any path or url the token at that offset resolves to.
    fn target(&self) -> Option<ClickTarget> {
        links::target_for_click(&self.text, &self.links, self.offset)
    }

    /// The word the pointer was on, for the log line a miss writes. Clipped by
    /// characters, never by bytes (#2271).
    fn word(&self) -> String {
        let start = self.offset.min(self.text.len());
        let tail = self.text.get(start..).unwrap_or_default();
        tail.chars()
            .take_while(|c| !c.is_whitespace())
            .take(40)
            .collect()
    }
}

/// Drag-to-copy over transcript rows (#2383) and their UTF-8 text ranges
/// (#2529). Each row records the text its selectable segments render. GPUI's
/// `TextLayout` turns a pointer into a byte offset; this state orders the two
/// endpoints and produces one clipboard payload for mouse release and keys.
#[derive(Default)]
pub struct CopyDrag {
    texts: RefCell<Vec<String>>,
    /// The press under the pointer, cleared by every release (#2317).
    pending_click: RefCell<Option<PendingClick>>,
    anchor: Cell<Option<TextPoint>>,
    end: Cell<Option<TextPoint>>,
    dragging: Cell<bool>,
    /// Fired once a release or a copy chord actually put text on the
    /// clipboard, so the composer can say so (#2587). Empty until the view
    /// that owns the composer hands one over.
    on_copied: RefCell<Option<CopiedListener>>,
}

impl CopyDrag {
    /// The owning view wires the acknowledgement here. The transcript cannot
    /// reach the composer, so the composer's parent is the only place that
    /// knows both halves of the gesture.
    pub fn set_copied_listener(&self, listener: CopiedListener) {
        *self.on_copied.borrow_mut() = Some(listener);
    }

    /// Tell the owner a copy landed. Only call this after a successful write:
    /// a click copies nothing and must stay silent.
    pub fn announce_copied(&self, app: &mut App) {
        let listener = self.on_copied.borrow().clone();
        if let Some(listener) = listener {
            listener(app);
        }
    }

    /// Drop the highlight. A release that copied has nothing left to keep lit
    /// (#2587); an unmoved click already holds nothing.
    pub fn clear_selection(&self) {
        self.anchor.set(None);
        self.end.set(None);
        self.dragging.set(false);
    }

    /// The release path: write the dragged text and, when it landed, drop the
    /// highlight. Returns whether the clipboard took it so the caller
    /// acknowledges exactly once.
    pub fn finish_copy(&self, app: &mut App) -> bool {
        let Some(item) = self.clipboard_item() else {
            return false;
        };
        app.write_to_clipboard(item);
        self.clear_selection();
        true
    }

    /// The one release path for a drag: end it, copy what it holds, and tell
    /// the owner so the composer can confirm. Every element that can start a
    /// drag also ends it here, because the row under the pointer is the only
    /// element guaranteed to still be hovered at mouse up. Calls after a copy
    /// find the selection already cleared, so one gesture copies once.
    pub fn release(&self, window: &mut Window, app: &mut App) {
        self.dragging.set(false);
        let held = self.copied_text().map_or(0, |text| text.chars().count());
        let copied = self.finish_copy(app);
        crate::logging::info(&format!(
            "transcript release: selected_chars={held} copied={copied}"
        ));
        if copied {
            self.announce_copied(app);
        } else {
            // Nothing was selected, so the press was a click: open whatever it
            // landed on. A drag that copied must never also launch (#2317).
            self.open_clicked_target();
        }
        window.refresh();
    }

    /// Open the target the press under the pointer named (#2317). Returns
    /// whether anything opened, so a caller can tell a miss from a hit.
    ///
    /// The pending press is taken whether or not it resolves, so one press can
    /// never open twice and a stale candidate cannot survive to the next
    /// click.
    pub fn open_clicked_target(&self) -> bool {
        let Some(pending) = self.pending_click.borrow_mut().take() else {
            return false;
        };
        let Some(target) = pending.target() else {
            // A click that found nothing still gets a line, so "I clicked and
            // nothing happened" can be told apart from a click that never
            // arrived instead of guessed at.
            crate::logging::info(&format!(
                "transcript link: nothing to open at {:?}",
                pending.word()
            ));
            return false;
        };
        crate::logging::info(&format!("transcript link: open {}", target.label()));
        match &target {
            ClickTarget::Url(url) => crate::preview::launch(&crate::preview::url_command(url)),
            ClickTarget::File(path) => crate::preview::launch(&crate::preview::open_command(path)),
        }
        true
    }

    fn points(&self) -> Option<(TextPoint, TextPoint)> {
        copy_select::ordered_points(self.anchor.get()?, self.end.get()?)
    }

    /// The selected rows, including one row when only its character range is
    /// selected. An unmoved click still has no range.
    fn range(&self) -> Option<(usize, usize)> {
        let (start, end) = self.points()?;
        Some((start.row, end.row))
    }

    pub fn has_selection(&self) -> bool {
        self.points().is_some()
    }

    /// The exact selected text, or None for a click or a stale row range.
    fn copied_text(&self) -> Option<String> {
        copy_select::copied_range(&self.texts.borrow(), self.anchor.get()?, self.end.get()?)
    }

    /// One payload path for release, cmd-c and ctrl-c. Tests can inspect the
    /// item without claiming that a headless test wrote the system clipboard.
    pub fn clipboard_item(&self) -> Option<gpui::ClipboardItem> {
        self.copied_text().map(gpui::ClipboardItem::new_string)
    }

    /// Intersect the selection with one rendered text segment. The segment's
    /// offset is in the row's copy text, so markdown and plain rows share the
    /// same hit-testing and highlighting rule.
    fn segment_range(&self, row: usize, start: usize, len: usize) -> Option<Range<usize>> {
        let (selection_start, selection_end) = self.points()?;
        if row < selection_start.row || row > selection_end.row {
            return None;
        }
        let row_len = self.texts.borrow().get(row)?.len();
        let selected_start = if row == selection_start.row {
            selection_start.offset
        } else {
            0
        };
        let selected_end = if row == selection_end.row {
            selection_end.offset
        } else {
            row_len
        };
        let end = start.checked_add(len)?;
        let lo = start.max(selected_start);
        let hi = end.min(selected_end);
        (lo < hi).then(|| lo - start..hi - start)
    }
}

/// Selection state threaded through one message renderer. Every selectable
/// `StyledText` is one segment; joining segments with newlines gives both the
/// copy string and a stable coordinate space for endpoint offsets.
/// One drawn text segment of a selectable row: where the segment starts in the
/// row's copy text, advanced in render order. `plain_block` and the card
/// renderers share it so a multi-line row keeps exact character offsets.
pub(crate) struct SelectionContext<'a> {
    drag: &'a Rc<CopyDrag>,
    row: usize,
    offset: Cell<usize>,
    segments: Cell<usize>,
}

impl<'a> SelectionContext<'a> {
    fn new(drag: &'a Rc<CopyDrag>, row: usize) -> Self {
        Self {
            drag,
            row,
            offset: Cell::new(0),
            segments: Cell::new(0),
        }
    }

    pub(crate) fn begin_segment(&self, len: usize) -> usize {
        let start = if self.segments.get() == 0 {
            0
        } else {
            self.offset.get() + 1
        };
        self.offset.set(start + len);
        self.segments.set(self.segments.get() + 1);
        start
    }
}

fn layout_offset(layout: &TextLayout, position: gpui::Point<gpui::Pixels>, len: usize) -> usize {
    match layout.index_for_position(position) {
        Ok(index) | Err(index) => index.min(len),
    }
}

/// Add a selection background without discarding markdown's existing runs.
/// Splitting at every run and selection boundary keeps ranges ordered and
/// non-overlapping, which is what `StyledText::with_default_highlights` wants.
fn highlights_with_selection(
    text_len: usize,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    selected: Option<Range<usize>>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(selected) = selected else {
        return highlights;
    };
    let mut boundaries = vec![0, text_len, selected.start, selected.end];
    for (range, _) in &highlights {
        boundaries.push(range.start);
        boundaries.push(range.end);
    }
    boundaries.retain(|offset| *offset <= text_len);
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut out = Vec::new();
    for pair in boundaries.windows(2) {
        let [start, end] = pair else { continue };
        if start == end {
            continue;
        }
        let existing = highlights
            .iter()
            .find(|(range, _)| range.start <= *start && range.end >= *end);
        let in_selection = *start >= selected.start && *end <= selected.end;
        if existing.is_none() && !in_selection {
            continue;
        }
        let mut style = existing.map(|(_, style)| *style).unwrap_or_default();
        if in_selection {
            style.background_color = Some(rgba(selection_rgba()).into());
        }
        out.push((*start..*end, style));
    }
    out
}

/// Render one real `StyledText` and retain its layout handle for pointer hit
/// testing. Text handlers stop propagation so the row-level fallback cannot
/// overwrite a precise offset with a block endpoint.
pub(crate) fn selectable_styled_text(
    text: String,
    style: TextStyle,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    links: &[(Range<usize>, String)],
    selection: Option<&SelectionContext>,
) -> Div {
    let accounted = text.len();
    selectable_text_segment(text, accounted, &style, highlights, links, selection)
}

/// The same, for a segment that draws less text than the row's copy string
/// accounts for (#2771). A line split around a crab picture hands each run the
/// length the whole line would have given it, so the offsets after the picture
/// still land on the characters the unsplit line put there. `accounted` only
/// moves the bookkeeping; hit testing stays inside the text that is drawn.
fn selectable_text_segment(
    text: String,
    accounted: usize,
    style: &TextStyle,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    links: &[(Range<usize>, String)],
    selection: Option<&SelectionContext>,
) -> Div {
    let segment_start = selection.map(|context| context.begin_segment(accounted));
    let selected = selection.and_then(|context| {
        context
            .drag
            .segment_range(context.row, segment_start?, text.len())
    });
    let highlights = highlights_with_selection(text.len(), highlights, selected);
    let styled = StyledText::new(text.clone()).with_default_highlights(style, highlights);
    let layout = styled.layout().clone();
    let base = div().child(styled);
    let Some(context) = selection else {
        return base;
    };
    let row = context.row;
    let start = segment_start.unwrap_or(0);
    let len = text.len();
    let begin = context.drag.clone();
    let extend = context.drag.clone();
    let pressed = context.drag.clone();
    let move_layout = layout.clone();
    // What the pointer is over, kept for the release so a click can open it.
    let click_text = text.clone();
    let click_links = links.to_vec();
    base.on_mouse_down(MouseButton::Left, move |event, window, cx| {
        let local = layout_offset(&layout, event.position, len);
        *pressed.pending_click.borrow_mut() = Some(PendingClick {
            text: click_text.clone(),
            links: click_links.clone(),
            offset: local,
        });
        let offset = start + local;
        begin.anchor.set(Some(TextPoint::new(row, offset)));
        begin.end.set(Some(TextPoint::new(row, offset)));
        begin.dragging.set(true);
        crate::logging::info(&format!("transcript drag start: row={row} offset={offset}"));
        cx.stop_propagation();
        window.refresh();
    })
    .on_mouse_move(move |event, window, cx| {
        if extend.dragging.get() {
            let offset = start + layout_offset(&move_layout, event.position, len);
            extend.end.set(Some(TextPoint::new(row, offset)));
            cx.stop_propagation();
            window.refresh();
        }
    })
}

/// One stored message row: its own right-press handler and select-mode
/// checkbox (#2613).
///
/// The row owns no menu card: the open menu is drawn at the window root, at
/// the point the press landed (#2690), so it is never pushed around by the
/// row's own frame.
fn message_row(
    id: uuid::Uuid,
    child: Div,
    click: Option<&ClickHandler>,
    press: Option<&PressHandler>,
    picked: Option<bool>,
) -> Stateful<Div> {
    let mut row = div()
        .id(SharedString::from(format!("msg-{id}")))
        .flex()
        .flex_row()
        .items_start()
        .gap_1()
        .w_full()
        .rounded_sm();
    if let Some(on) = picked {
        // A pick box, not a tint: the drag selector already owns background
        // tint on these rows, and two selection languages on one row would
        // read as one state with two meanings.
        row = row.child(
            div()
                .flex_none()
                .w(rems(1.1))
                .pt(px(2.0))
                .text_color(rgb(if on {
                    palette::current().teal
                } else {
                    palette::current().text_faint
                }))
                .child(SharedString::from(if on { "\u{2611}" } else { "\u{2610}" })),
        );
    }
    // The body is stateful so the row can carry its own click listener: gpui
    // hangs `on_click` off a stateful element, not a bare `Div`.
    let mut body = div()
        .id(SharedString::from(format!("msg-body-{id}")))
        .flex_1()
        .min_w_0()
        .child(child);
    if let Some(handler) = click {
        let handler = handler.clone();
        body = body
            .cursor_pointer()
            .on_click(move |ev, window, app| handler(ev, window, app));
    }
    if let Some(handler) = press {
        let handler = handler.clone();
        body = body.on_mouse_down(MouseButton::Right, move |ev, window, app| {
            handler(ev, window, app);
        });
    }
    row = row.child(body);
    row
}

/// The menu card itself: one row per resolved item, drawn at the pointer that
/// opened it. It reuses the rail's card and popover so every menu is one
/// surface with one placement rule (#2690).
pub fn message_menu_popover(items: &[(String, ClickHandler)], at: Point<Pixels>) -> AnyElement {
    let palette = palette::current();
    let rows: Vec<Stateful<Div>> = items
        .iter()
        .map(|(label, pick)| {
            let pick = pick.clone();
            div()
                .id(SharedString::from(format!("msg-menu-{label}")))
                .px_2()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .text_sm()
                .text_color(rgb(palette.text))
                .hover(|d| d.bg(rgb(palette.selection)))
                .child(SharedString::from(label.clone()))
                .on_click(move |ev, window, app| pick(ev, window, app))
        })
        .collect();
    // The card stops its own presses, so the transcript's dismiss handler
    // never removes it before its item's click lands: the pick fires on
    // mouse-up, the dismiss on mouse-down (#2613).
    crate::menu::menu_popover("message-context-menu", rows, at)
}

/// A row that takes part in a drag. Text segments provide exact offsets; these
/// row handlers preserve selection over padding and non-text blocks.
fn selectable_row<E: gpui::IntoElement>(
    drag: &Rc<CopyDrag>,
    ordinal: usize,
    child: E,
) -> Stateful<Div> {
    let selected = drag
        .range()
        .is_some_and(|(lo, hi)| (lo..=hi).contains(&ordinal));
    let text_len = drag.texts.borrow().get(ordinal).map_or(0, String::len);
    let start = drag.clone();
    let extend = drag.clone();
    let release = drag.clone();
    div()
        .id(SharedString::from(format!("tblock-{ordinal}")))
        .when(selected, |d| d.bg(rgba(selection_rgba())))
        .on_mouse_down(MouseButton::Left, move |_ev, window, _app| {
            start.pending_click.borrow_mut().take();
            start.anchor.set(Some(TextPoint::new(ordinal, 0)));
            start.end.set(Some(TextPoint::new(ordinal, 0)));
            start.dragging.set(true);
            window.refresh();
        })
        .on_mouse_move(move |_ev, window, _app| {
            if extend.dragging.get() {
                let offset = match extend.anchor.get() {
                    Some(anchor) if anchor.row > ordinal => 0,
                    Some(anchor) if anchor.row == ordinal && anchor.offset == text_len => 0,
                    _ => text_len,
                };
                extend.end.set(Some(TextPoint::new(ordinal, offset)));
                window.refresh();
            }
        })
        // The row ends the drag it started. gpui only runs a bubble mouse-up
        // handler on a hovered hitbox, and the row under the pointer is the one
        // hitbox guaranteed to be hovered, while the pane behind it may not be.
        // A second release for the same gesture finds nothing left to copy.
        .on_mouse_up(MouseButton::Left, move |_ev, window, app| {
            release.release(window, app)
        })
        .child(child)
}

/// The visible text of a markdown block in exactly the segment order used by
/// `md_block`: table cells follow their header and rows, and every segment is
/// separated by one newline in the copy coordinate space.
fn markdown_copy_text(src: &str) -> String {
    let mut segments = Vec::new();
    for block in markdown::blocks(src) {
        match block {
            markdown::MdBlock::Line(line) => segments.push(line.text),
            markdown::MdBlock::Table(table) => {
                let cols = table.columns().max(1);
                for i in 0..cols {
                    segments.push(
                        table
                            .header
                            .get(i)
                            .map_or("", |cell| cell.text.as_str())
                            .to_string(),
                    );
                }
                for row in table.rows {
                    for i in 0..cols {
                        segments.push(row.get(i).map_or("", |cell| cell.text.as_str()).to_string());
                    }
                }
            }
        }
    }
    segments.join("\n")
}

/// The plain text a row copies: what the row shows, without markers or folds.
/// Assistant rows use the same visible markdown segments as the renderer so a
/// character offset never points at stripped `##` or backtick source bytes.
fn block_text(block: &Block) -> String {
    match block {
        Block::Msg { role, content, .. } => {
            let text = strip_markers(content);
            if role == "assistant" {
                markdown_copy_text(&text)
            } else {
                text
            }
        }
        Block::Thinking { content } => content.trim().to_string(),
        Block::Tools { calls } => calls.iter().map(tool_line).collect::<Vec<_>>().join("\n"),
        Block::CompletedPlan { plan, .. } => plan.copy_text(),
        Block::Marker { .. } => String::new(),
    }
}

/// What the transcript view listens to besides its turns: the attachment
/// tiles' handlers, and the drag-to-copy state of the pane that copies (#2383).
#[derive(Default)]
pub struct ViewHooks {
    pub attachments: HashMap<String, ClickHandler>,
    pub drag: Option<Rc<CopyDrag>>,
    /// The wheel over the transcript, minted by the owning view so a tick up
    /// releases its follow and a tick down re-arms it (#2698).
    pub wheel: Option<ScrollHandler>,
    /// Left click per stored message row, keyed by message id. Select mode
    /// mints one per row so a click toggles it; outside the mode the map is
    /// empty and every row stays inert (#2613).
    pub message_click: HashMap<uuid::Uuid, ClickHandler>,
    /// Right press per stored message row: opens the message menu (#2613).
    pub message_press: HashMap<uuid::Uuid, PressHandler>,
    /// The picked rows while select mode is on, or `None` when it is off.
    pub selection: Option<HashSet<uuid::Uuid>>,
    /// A left press on the transcript's own background, so an open message
    /// menu closes on a click outside it (#2613).
    pub menu_dismiss: Option<PressHandler>,
    /// The session's ctx budget, so the live card's footer can carry it the way
    /// the Telegram footer does (#2442).
    pub ctx_used: Option<u32>,
    pub ctx_max: u32,
    /// The live card's own click, minted by the caller for the same reason the
    /// folds are: only the owning view can build one.
    pub live_click: Option<ClickHandler>,
}

pub fn view(
    state: &TranscriptState,
    scroll: &ScrollHandle,
    live: &LiveTurn,
    busy: bool,
    content_width: f32,
    mut handlers: HashMap<String, ClickHandler>,
    hooks: ViewHooks,
) -> Stateful<Div> {
    let ViewHooks {
        attachments: attachment_handlers,
        drag,
        wheel,
        ctx_used,
        ctx_max,
        live_click,
        message_click,
        message_press,
        selection: selected,
        menu_dismiss,
    } = hooks;
    if let Some(d) = &drag {
        d.texts.borrow_mut().clear();
    }
    let mut col = div()
        .id("transcript")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .gap_2()
        .p_2()
        .bg(rgb(palette::current().bg));
    // The wheel is the reader's own hand on the transcript: the owning view
    // decides whether a tick releases its follow or gives it back (#2698).
    if let Some(wheel) = wheel {
        col = col.on_scroll_wheel(move |ev, window, cx| wheel(ev, window, cx));
    }
    // A press on the transcript's background closes the open message menu
    // (#2613). Rows that handle their own press and the menu card itself stop
    // the event before it reaches here, so this only fires for a click outside.
    if let Some(dismiss) = menu_dismiss {
        col = col.on_mouse_down(MouseButton::Left, move |ev, window, cx| {
            dismiss(ev, window, cx);
        });
    }

    if state.session.is_none() {
        col = col.child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text_muted))
                .child("Select a session to read its transcript"),
        );
        return col;
    }
    if let Some(err) = &state.error {
        col = col.child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text_muted))
                .child(err.clone()),
        );
        return col;
    }
    if state.turns.is_empty() && live.is_empty() {
        col = col.child(
            div()
                .text_sm()
                .text_color(rgb(palette::current().text_muted))
                .child("No messages in this session"),
        );
        return col;
    }

    if state.hidden > 0 {
        col = col.child(block_view(
            &Block::Marker {
                hidden: state.hidden,
            },
            content_width,
            &attachment_handlers,
            None,
        ));
    }
    let last = state.turns.len().saturating_sub(1);
    // A turn still being drawn by the live card is not drawn again from the
    // rows a reload just installed: the persisted copy has the same question,
    // the same header and the same answer, and the screen showed all three
    // twice. The card owns the exchange until `live` clears.
    let live_on_screen = busy || !live.is_empty();
    let newest_is_live = live_on_screen
        && state
            .turns
            .last()
            .is_some_and(|turn| newest_live_turn_matches(turn, live));
    // A merged read labels its rows by topic the way Telegram's All view does
    // (#2762): the name shows once, then again whenever the topic changes. A
    // single session carries no label, so nothing is drawn there.
    let mut shown_origin: Option<&str> = None;
    for (ti, turn) in state.turns.iter().enumerate() {
        if persisted_turn_is_hidden(ti, last, newest_is_live) {
            continue;
        }
        if let Some(origin) = turn.origin.as_deref()
            && shown_origin != Some(origin)
        {
            col = col.child(origin_divider(origin, shown_origin.is_some()));
            shown_origin = Some(origin);
        }
        let folded = !state.expanded.contains(&turn.anchor);
        let full = state.details.contains(&turn.anchor);
        // The card's head sits above the turn's first non-user row, as in the
        // TUI (#759). Everything the turn did goes inside that same box, so a
        // settled turn keeps the shape it had while it ran (#2632): the
        // question above the card, the working-out in it, the answer below.
        let header_at = match turn.blocks.first() {
            Some(Block::Msg { role, .. }) if role == "user" => 1,
            _ => 0,
        };
        let carded = turn.header.is_some();
        let mut pre: Vec<AnyElement> = Vec::new();
        let mut body: Vec<AnyElement> = Vec::new();
        let mut post: Vec<AnyElement> = Vec::new();
        for (i, block) in turn.blocks.iter().enumerate() {
            // A folded turn hides its working-out; the running turn's answer
            // is not shown twice because the stream owns it until it settles.
            if folded && !visible_when_folded(block, turn.final_idx, i) {
                continue;
            }
            // Every row here belongs to a turn the live card is NOT drawing,
            // which is why it is on screen at all. Narration and thinking stay
            // inside the card; the answer a settled turn already produced
            // draws here, and keeps drawing while the next request runs. The
            // skip used to key on `busy` alone, so the previous answer
            // disappeared for the whole duration of a new turn (#2709).
            let text = block_text(block);
            let mut selectable = None;
            let child: Div = match &drag {
                Some(d) if !text.trim().is_empty() => {
                    let ordinal = {
                        let mut texts = d.texts.borrow_mut();
                        texts.push(text);
                        texts.len() - 1
                    };
                    selectable = Some(ordinal);
                    let selection = SelectionContext::new(d, ordinal);
                    match block {
                        Block::Thinking { content } => thinking_block(content, full),
                        _ => {
                            block_view(block, content_width, &attachment_handlers, Some(&selection))
                        }
                    }
                }
                _ => match block {
                    Block::Thinking { content } => thinking_block(content, full),
                    _ => block_view(block, content_width, &attachment_handlers, None),
                },
            };
            // A stored row carries its own menu anchor and pick box (#2613).
            // While select mode is on the row is not wrapped for drag-to-copy:
            // a click there toggles the row, and two gestures competing for
            // the same press would make both unreliable.
            let child: AnyElement = match block {
                Block::Msg {
                    message_id: Some(id),
                    ..
                } => {
                    let picked = selected.as_ref().map(|picks| picks.contains(id));
                    let row = message_row(
                        *id,
                        child,
                        message_click.get(id),
                        message_press.get(id),
                        picked,
                    )
                    .into_any_element();
                    match (&drag, selectable, picked) {
                        (_, _, Some(_)) => row,
                        (Some(d), Some(ordinal), None) => {
                            selectable_row(d, ordinal, row).into_any_element()
                        }
                        _ => row,
                    }
                }
                _ => match (&drag, selectable) {
                    (Some(d), Some(ordinal)) => {
                        selectable_row(d, ordinal, child).into_any_element()
                    }
                    _ => child.into_any_element(),
                },
            };
            // Working-out rows ride inside the card; conversation rows keep
            // their place around it (#2632).
            if !carded {
                col = col.child(child);
            } else if i < header_at {
                pre.push(child);
            } else if folded || matches!(block, Block::Msg { .. }) {
                post.push(child);
            } else {
                body.push(child);
            }
        }
        col = col.children(pre);
        if carded && let Some(card) = settled_card(turn, folded, body) {
            // The listener is per turn and used exactly here, so it is taken
            // out of the map instead of cloned.
            col = col.child(match handlers.remove(&turn.anchor) {
                Some(click) => card.on_click(move |event, window, cx| click(event, window, cx)),
                None => card,
            });
        }
        col = col.children(post);
    }
    if !live.is_empty() {
        col = col.child(live_view(
            live,
            content_width,
            state.live_expanded,
            (ctx_used, ctx_max),
            live_click.as_ref(),
            &attachment_handlers,
            drag.as_ref(),
        ));
    }
    // Releasing the mouse ends the drag and copies the selected range (#2383,
    // #2529). A click that never moved selects nothing, so it copies nothing
    // and says nothing. A copy that landed drops the highlight and tells the
    // composer, which is what the terminal does on release (#2587).
    if let Some(d) = drag {
        // Rows end the drag under the pointer (see `selectable_row`). The pane
        // keeps the same path as a fallback for a release over its padding and
        // for a release past its last pixel, which is still the end of the
        // drag: without it the drag stayed armed and the next click extended a
        // stale selection instead of starting a new one (#2587).
        let on_up = d.clone();
        col = col.on_mouse_up(MouseButton::Left, move |_ev, window, app| {
            on_up.release(window, app)
        });
        let on_out = d.clone();
        col = col.on_mouse_up_out(MouseButton::Left, move |_ev, window, app| {
            on_out.release(window, app)
        });
    }
    col
}

/// The owner's test feedback, pinned as logic: a stored assistant row with a
/// reasoning block, a tool ledger and a final answer must expand into
/// thinking, tool status and text, in that order. Before this seat the three
/// halves were only ever read, never executed.
#[cfg(test)]
mod tests {
    use super::*;

    /// Perceived brightness of a packed RGB colour, 0..=255.
    fn luma(color: u32) -> u32 {
        let r = (color >> 16) & 0xff;
        let g = (color >> 8) & 0xff;
        let b = color & 0xff;
        (r * 299 + g * 587 + b * 114) / 1000
    }

    /// The bubble's visible tone once the glass alpha lands on the canvas.
    fn over(ink: u32, canvas: u32, alpha: u32) -> u32 {
        let a = alpha.min(255);
        let mix = |shift: u32| {
            let ink = (ink >> shift) & 0xff;
            let canvas = (canvas >> shift) & 0xff;
            (ink * a + canvas * (255 - a)) / 255
        };
        (mix(16) << 16) | (mix(8) << 8) | mix(0)
    }

    #[test]
    fn drag_copy_segment_range_does_not_overflow_when_segment_is_past_selection() {
        let drag = CopyDrag::default();
        drag.texts.borrow_mut().push("0123456789extra".into());
        drag.anchor.set(Some(TextPoint::new(0, 0)));
        drag.end.set(Some(TextPoint::new(0, 3)));

        // A segment past the selection release point must safely return None without panicking.
        assert_eq!(drag.segment_range(0, 4, 2), None);

        // Drag with anchor 10 and end 3 normalizes to 3..10.
        let drag_rev = CopyDrag::default();
        drag_rev.texts.borrow_mut().push("0123456789extra".into());
        drag_rev.anchor.set(Some(TextPoint::new(0, 10)));
        drag_rev.end.set(Some(TextPoint::new(0, 3)));
        assert_eq!(drag_rev.segment_range(0, 12, 2), None);

        // Fully covered segment returns the exact segment range.
        assert_eq!(drag_rev.segment_range(0, 4, 3), Some(0..3));

        // Partial overlap at the start.
        assert_eq!(drag_rev.segment_range(0, 2, 3), Some(1..3));

        // Partial overlap at the end.
        assert_eq!(drag_rev.segment_range(0, 8, 4), Some(0..2));
    }

    #[test]
    fn drag_copy_within_block_has_a_visible_range_and_clipboard_payload() {
        let drag = CopyDrag::default();
        drag.texts.borrow_mut().push("select a small range".into());
        drag.anchor.set(Some(TextPoint::new(0, 7)));
        drag.end.set(Some(TextPoint::new(0, 16)));
        assert_eq!(drag.range(), Some((0, 0)));
        assert!(drag.has_selection());
        assert_eq!(drag.copied_text().as_deref(), Some("a small r"));
        assert_eq!(
            drag.clipboard_item()
                .and_then(|item| item.text())
                .as_deref(),
            Some("a small r")
        );
    }

    #[test]
    fn drag_copy_across_blocks_preserves_partial_rows_and_blank_lines() {
        let drag = CopyDrag::default();
        drag.texts.borrow_mut().extend([
            "first".to_string(),
            "second".to_string(),
            "third".to_string(),
        ]);
        drag.anchor.set(Some(TextPoint::new(2, 4)));
        drag.end.set(Some(TextPoint::new(0, 2)));
        assert_eq!(drag.copied_text().as_deref(), Some("rst\n\nsecond\n\nthir"));
    }

    #[test]
    fn drag_copy_cmd_and_ctrl_chords_are_equivalent() {
        assert!(copy_select::is_copy_chord("c", true, false));
        assert!(copy_select::is_copy_chord("c", false, true));
        assert!(!copy_select::is_copy_chord("c", false, false));
        assert!(!copy_select::is_copy_chord("x", true, false));
    }

    #[test]
    fn drag_copy_markdown_copy_text_matches_visible_segments() {
        assert_eq!(markdown_copy_text("## Head\n\nbody"), "Head\nbody");
        assert_eq!(
            markdown_copy_text("| A | B |\n| --- | --- |\n| 1 | 2 |"),
            "A\nB\n1\n2"
        );
    }

    /// The ladder steps down strictly and the body stays smallest, so a
    /// heading can never look like prose again (#2521).
    #[test]
    fn headings_step_down_from_h1_to_body() {
        assert!(heading_size(1) > heading_size(2));
        assert!(heading_size(2) > heading_size(3));
        assert!(heading_size(3) > heading_size(0), "h3 still beats body");
        assert_eq!(heading_size(0), 13.0, "body stays 13px");
        assert_eq!(heading_weight(1), gpui::FontWeight::BOLD);
        assert_eq!(heading_weight(0), gpui::FontWeight::NORMAL);
    }

    /// #2569: only the user message paints a bubble. The assistant answer is
    /// drawn straight onto the history background, so a long reply is not one
    /// more card.
    #[test]
    fn only_user_messages_paint_a_bubble_surface() {
        assert!(bubble_has_surface("user"));
        assert!(!bubble_has_surface("assistant"));
        assert!(!bubble_has_surface("system"));
        assert!(!bubble_has_surface("tool"));

        let src = include_str!("transcript.rs");
        let body = src
            .split("fn bubble(")
            .nth(1)
            .and_then(|rest| rest.split("/// The files").next())
            .expect("message bubble renderer");
        let (shared, guarded) = body
            .split_once("if bubble_has_surface(role)")
            .expect("the user-only surface guard");
        assert!(
            !shared.contains(".bg("),
            "the shared column stays transparent for every role"
        );
        assert!(
            guarded.contains(".bg(glass_fill(message_surface(role)))"),
            "the user surface is painted behind the guard"
        );
    }

    /// #2569: the user bubble is a neutral step off the history canvas, lighter
    /// on dark themes and darker on light ones, and it never borrows a role
    /// accent, so no hue is introduced.
    #[test]
    fn the_user_bubble_is_a_neutral_step_off_the_history_canvas() {
        let dark = palette::ORGN;
        let light = palette::Palette {
            bg: 0xf5f5f5,
            panel: 0xffffff,
            ..dark
        };
        // A non-default light palette proves the direction follows the canvas
        // rather than one hardcoded theme.
        let warm_light = palette::Palette {
            bg: 0xfaf4e8,
            panel: 0xfffdf8,
            ..dark
        };

        for (label, p) in [("dark", dark), ("light", light), ("warm light", warm_light)] {
            let surface = message_surface_for(&p, "user");
            assert!(
                surface <= 0x00ff_ffff,
                "{label}: the surface stays valid RGB, got {surface:#08x}"
            );
            assert_ne!(surface, p.bg, "{label}: the bubble has a visible step");
            let canvas = luma(p.bg);
            let bubble = luma(surface);
            assert!(
                bubble.abs_diff(canvas) <= 20,
                "{label}: the step stays subtle, {bubble} against {canvas}"
            );
            if palette_is_dark(p.bg) {
                assert!(
                    bubble > canvas,
                    "{label}: the bubble lifts off a dark canvas, {bubble} against {canvas}"
                );
            } else {
                assert!(
                    bubble < canvas,
                    "{label}: the bubble sinks into a light canvas, {bubble} against {canvas}"
                );
            }
            // The glass thins the fill, so check the composite at the most
            // transparent stop: the step must still be visible and still point
            // the same way.
            let visible = luma(over(
                surface,
                p.bg,
                opencrabs::ui_core::card::GLASS_ALPHA_BOTTOM,
            ));
            assert!(
                visible.abs_diff(canvas) >= 6,
                "{label}: the glassed step stays visible, {visible} against {canvas}"
            );
            assert!(
                visible.abs_diff(canvas) <= 20,
                "{label}: the glassed step stays subtle, {visible} against {canvas}"
            );
            if palette_is_dark(p.bg) {
                assert!(
                    visible > canvas,
                    "{label}: the glassed bubble lifts off a dark canvas, {visible} against {canvas}"
                );
            } else {
                assert!(
                    visible < canvas,
                    "{label}: the glassed bubble sinks into a light canvas, {visible} against {canvas}"
                );
            }
        }

        // Recolouring every role accent must not move the surface: the bubble
        // is depth, not colour.
        let recoloured = palette::Palette {
            blue: 0x00ff00,
            green: 0xff00ff,
            amber: 0x123456,
            ..dark
        };
        assert_eq!(
            message_surface_for(&dark, "user"),
            message_surface_for(&recoloured, "user"),
            "the bubble does not read role colours"
        );
        assert_eq!(message_surface_for(&dark, "assistant"), dark.bg);
    }

    /// #2569: the bubble keeps its inner breathing room, readable user text,
    /// and an attachment budget that excludes the bubble's own padding.
    /// #2571: its corner is the message token, tighter than a card.
    #[test]
    fn message_bubbles_pad_and_shrink_attachment_budget() {
        let src = include_str!("transcript.rs");
        let bubble = src
            .split("fn bubble(")
            .nth(1)
            .and_then(|rest| rest.split("/// The files").next())
            .expect("message bubble renderer");
        assert!(bubble.contains(".rounded(px(MESSAGE_RADIUS))"));
        assert!(!bubble.contains(".rounded(px(CARD_RADIUS))"));
        assert!(bubble.contains(".px(px(MESSAGE_PAD_X))"));
        assert!(bubble.contains(".py(px(MESSAGE_PAD_Y))"));
        assert!(bubble.contains("glass_fill(message_surface(role))"));
        assert!(bubble.contains("content_width - 2.0 * MESSAGE_PAD_X"));
        assert!(bubble.contains("history_attachments(&paths, attachment_width, handlers)"));
        const {
            assert!(USER_TEXT_SIZE > PLAIN_TEXT_SIZE);
        }
    }

    /// #2571: only the message surface gets the tighter corner. The shared card
    /// and Seeker radii keep their values, so no other panel, table, ledger,
    /// composer, sidebar or plan strip moves with the chat change.
    ///
    /// #2618: the live card is the same surface, painted before the turn
    /// persists, so it takes the message radius too. The settled card it turns
    /// into keeps it, or the corner visibly jumps the moment a turn finishes.
    /// Three uses, all the message surface.
    #[test]
    fn only_the_message_surface_uses_the_tighter_radius() {
        let src = include_str!("transcript.rs");
        const {
            assert!(MESSAGE_RADIUS < CARD_RADIUS);
            assert!(opencrabs::ui_core::card::SEEKER_RADIUS > CARD_RADIUS);
        }
        assert_eq!(MESSAGE_RADIUS, 12.0);
        assert_eq!(CARD_RADIUS, 24.0);
        assert_eq!(opencrabs::ui_core::card::SEEKER_RADIUS, 32.0);
        // The token is declared once and applied where a message is painted:
        // the settled bubble, the settled card and the live card that becomes
        // it. Every other rounded surface in this file still reads
        // CARD_RADIUS. Count on the production half only: this file includes
        // itself, so the literals in this test would otherwise match
        // themselves.
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        assert_eq!(production.matches("const MESSAGE_RADIUS").count(), 1);
        let message_radius_uses = production
            .lines()
            .filter(|line| line.contains("px(MESSAGE_RADIUS)"))
            .count();
        assert_eq!(
            message_radius_uses, 3,
            "the bubble, the settled card and the live card apply the message radius, found {message_radius_uses} uses"
        );
        let settled = production
            .split("fn settled_card(")
            .nth(1)
            .and_then(|rest| rest.split("fn live_card(").next())
            .expect("settled turn card body");
        assert!(
            settled.contains(".rounded(px(MESSAGE_RADIUS))"),
            "the settled card keeps the radius the live card painted"
        );
        let live = production
            .split("fn live_card(")
            .nth(1)
            .and_then(|rest| rest.split("fn live_card_footer(").next())
            .expect("live card body");
        assert!(
            live.contains(".rounded(px(MESSAGE_RADIUS))"),
            "the live card takes the message radius"
        );
        assert!(
            !live.contains(".rounded(px(CARD_RADIUS))"),
            "the live card must not keep the pudgy card radius"
        );
        let cell = src
            .split("fn attachment_cell(")
            .nth(1)
            .and_then(|rest| rest.split("/// The last cell").next())
            .expect("attachment cell");
        assert!(
            cell.contains(".rounded(px(CARD_RADIUS))"),
            "attachment tiles keep the card radius"
        );
    }

    /// #2639: the working-out rows are rows, not boxes. The panels nested a
    /// card-radius (24) surface inside a card that draws the message radius
    /// (12), so the inner corners were twice as round as the outer ones, and
    /// every ledger painted its own fill on top of the card's.
    #[test]
    fn the_working_out_rows_draw_no_inner_panel() {
        let src = include_str!("transcript.rs");
        let production = src
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .expect("production source before the test module");
        for (name, end) in [
            ("fn thinking_block(", "/// A tool ledger:"),
            ("fn tools_block(", "/// One call's status mark"),
        ] {
            let body = production
                .split(name)
                .nth(1)
                .and_then(|rest| rest.split(end).next())
                .unwrap_or_else(|| panic!("{name} body"));
            for forbidden in [".rounded(", ".bg(", ".border_", ".p_3()"] {
                assert!(
                    !body.contains(forbidden),
                    "{name} must not draw {forbidden}: the card owns the frame"
                );
            }
        }
        assert!(
            !production.contains("\u{b7} thinking \u{b7}"),
            "the reasoning block lost its caption"
        );
    }

    /// #2796: the picture is painted from the square cache, so the radius it is
    /// given lands on a quad that is exactly the tile's box. #2536 gave the
    /// picture the concentric radius but left `Cover` painting a larger quad,
    /// so the arc sat outside the tile's and its test, which only looked for
    /// the string, stayed green over a tile that still bled.
    #[test]
    fn attachment_media_clips_inside_the_card_radius() {
        let src = include_str!("transcript.rs");
        let cell = src
            .split("fn attachment_cell(")
            .nth(1)
            .and_then(|rest| rest.split("/// The last cell").next())
            .expect("attachment cell");
        assert!(cell.contains(".rounded(px(CARD_RADIUS))"));
        assert!(cell.contains(".overflow_hidden()"));
        assert!(
            cell.contains("crate::thumbs::picture(") && cell.contains("px(concentric_radius(1.0))"),
            "the tile paints the square crop at the concentric radius"
        );
        // The comment above the call names `Cover` while explaining why it was
        // wrong, so the guard has to read code rather than prose. Same trap the
        // #2793 scans hit, one layer down.
        let code = cell
            .lines()
            .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains("Cover"),
            "a cover fit paints outside the tile, whatever radius it is given"
        );
        assert_eq!(concentric_radius(1.0), CARD_RADIUS - 1.0);
    }

    /// The stored-rows split with nothing remembered, which is what most
    /// seats here exercise. Metrics have their own seats.
    fn turns(rows: &[Message], total: i64) -> (usize, Vec<Turn>) {
        turns_with_origins(rows, total, &HashMap::new(), &HashMap::new())
    }

    fn row(role: &str, content: &str, sequence: i32) -> Message {
        Message::new(
            uuid::Uuid::nil(),
            role.to_string(),
            content.to_string(),
            sequence,
        )
    }

    fn completed_history_row(sequence: i32, task_count: usize) -> Message {
        use opencrabs::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskStatus, TaskType};

        let session_id = uuid::Uuid::new_v4();
        let mut plan = PlanDocument::new(session_id, "Ship GUI history".to_string());
        plan.status = PlanStatus::Active;
        for order in 1..=task_count {
            let mut task = PlanTask::new(
                order,
                format!("Resolved task {order}"),
                "description".to_string(),
                TaskType::Test,
            );
            task.status = if order % 2 == 0 {
                TaskStatus::Skipped
            } else {
                TaskStatus::Completed
            };
            plan.add_task(task);
        }
        let content = serde_json::to_string(&CompletedPlanPayload { version: 1, plan })
            .expect("completed plan fixture serializes");
        let mut message = row(PLAN_COMPLETED_ROLE, &content, sequence);
        message.id = uuid::Uuid::new_v4();
        message.session_id = session_id;
        message
    }

    /// Every block of every turn, in order, which is what the pre-fold pane
    /// drew and what the ordering seats below assert on.
    fn flat(rows: &[Message], total: i64) -> Vec<Block> {
        turns(rows, total)
            .1
            .into_iter()
            .flat_map(|t| t.blocks)
            .collect()
    }

    #[test]
    fn gui_completed_plan_history_decodes_after_answer_in_older_turn() {
        let completed = completed_history_row(3, 8);
        let completed_id = completed.id;
        let rows = [
            row("user", "ship it", 1),
            row("assistant", "Final answer", 2),
            completed,
            row("user", "next question", 4),
        ];

        let (_, turns) = turns(&rows, 4);
        assert_eq!(turns.len(), 2, "the next user row opens a new turn");
        let older = &turns[0];
        assert_eq!(older.final_idx, Some(1));
        assert!(matches!(
            &older.blocks[1],
            Block::Msg { role, content, .. }
                if role == "assistant" && content == "Final answer"
        ));

        let (card_index, message_id, plan) = older
            .blocks
            .iter()
            .enumerate()
            .find_map(|(index, block)| match block {
                Block::CompletedPlan { message_id, plan } => Some((index, *message_id, plan)),
                _ => None,
            })
            .expect("valid durable row becomes a completed-plan block");
        assert_eq!(card_index, 2, "the checklist follows the final answer");
        assert_eq!(message_id, completed_id);
        assert_eq!(plan.tasks.len(), 8);
        assert_eq!(plan.header(), "Plan · Ship GUI history · 8 tasks · 100%");

        let text = block_text(&older.blocks[card_index]);
        for order in 1..=8 {
            assert!(
                text.contains(&format!("☑ {order}. Resolved task {order}")),
                "historical rendering must not window task {order}: {text}"
            );
        }
        assert!(text.ends_with(opencrabs::ui_core::plan_history::COMPLETED_PLAN_FOOTER));
        assert!(!text.contains("\"tasks\""));
        assert!(!text.contains("session_id"));
    }

    #[test]
    fn gui_completed_plan_history_fold_and_copy_keep_full_checklist() {
        let rows = [
            row("user", "ship it", 1),
            row("assistant", "Final answer", 2),
            completed_history_row(3, 8),
        ];
        let (_, turns) = turns(&rows, 3);
        let turn = &turns[0];
        let card_index = turn
            .blocks
            .iter()
            .position(|block| matches!(block, Block::CompletedPlan { .. }))
            .expect("completed card exists");

        assert!(visible_when_folded(
            &turn.blocks[card_index],
            turn.final_idx,
            card_index
        ));
        assert!(
            turn.header.is_none(),
            "a read-only card is not a hidden working-out step"
        );

        let copied = block_text(&turn.blocks[card_index]);
        assert!(copied.contains("☑ 7. Resolved task 7"));
        assert!(copied.contains("☑ 8. Resolved task 8"));
        assert!(copied.ends_with("Plan completed and archived."));
        assert!(!copied.contains("\"plan\""));

        let drag = CopyDrag::default();
        drag.texts.borrow_mut().push(copied.clone());
        drag.anchor.set(Some(TextPoint::new(0, 0)));
        drag.end.set(Some(TextPoint::new(0, copied.len())));
        assert_eq!(
            drag.clipboard_item()
                .and_then(|item| item.text())
                .as_deref(),
            Some(copied.as_str()),
            "drag copy returns the visible checklist, never stored JSON"
        );
    }

    #[test]
    fn gui_completed_plan_history_malformed_rows_never_render_json() {
        let malformed = row(
            PLAN_COMPLETED_ROLE,
            "{\"secret\":\"GUI-PLAN-JSON-SENTINEL\"}",
            2,
        );
        assert!(row_blocks(&malformed, None).is_empty());

        let mut unsupported = completed_history_row(3, 2);
        unsupported.content = unsupported
            .content
            .replace("\"version\":1", "\"version\":2");
        assert!(row_blocks(&unsupported, None).is_empty());

        let blocks = flat(&[row("user", "question", 1), malformed], 2);
        assert_eq!(blocks.len(), 1, "only the ordinary user row remains");
        assert!(!block_text(&blocks[0]).contains("GUI-PLAN-JSON-SENTINEL"));
    }

    /// #2756: a seed that still sits in the ledger must not paint. The plan
    /// approval prompt is uppercase and the old filter only knew the title
    /// case spelling, so 145 of them rendered as user bubbles.
    #[test]
    fn gui_transcript_never_paints_an_internal_seed() {
        for seed in [
            "[SYSTEM: PLAN APPROVED] The user approved the checklist.",
            "[System: You just rebuilt yourself from source and restarted]",
        ] {
            assert!(
                row_blocks(&row("user", seed, 1), None).is_empty(),
                "{seed} is scaffolding, not a conversation row"
            );
        }
        // A message the user typed is still a row of its own.
        assert_eq!(row_blocks(&row("user", "hello", 1), None).len(), 1);
    }

    /// The card's narration lines, in order, so a test reads the words the
    /// pane paints instead of the buffer they came from.
    fn narration_lines(live: &LiveTurn) -> Vec<String> {
        live_card_lines(live)
            .into_iter()
            .filter_map(|line| match line {
                LiveCardLine::Narration(text) => Some(text),
                _ => None,
            })
            .collect()
    }

    /// The settle test measures the session, not the window (#2705): a
    /// conversation past `WINDOW` rows keeps growing in the ledger while the
    /// windowed turn count stays flat.
    #[test]
    fn row_total_counts_the_rows_the_window_hid() {
        let state = TranscriptState {
            hidden: 135,
            rows: vec![row("user", "in the window", 1)],
            ..Default::default()
        };
        assert_eq!(
            state.row_total(),
            136,
            "the total the ledger reports is what a settle test has to read"
        );
    }

    #[test]
    fn preview_handlers_cover_the_live_turn() {
        // #2395: the live turn draws its user rows through the same block
        // renderer as stored turns, so those tiles need the same handlers.
        // Counting only stored rows left a picture sent a second ago tiled
        // but dead until the turn settled.
        let state = TranscriptState {
            turns: turns(&[row("user", "old <<IMG:/tmp/old.png>>", 1)], 1).1,
            ..Default::default()
        };
        // A queued message the agent took mid-turn is drawn as a user row too,
        // and it can carry the same file as the stored turn behind it.
        let live = LiveTurn {
            user: Some("fresh <<IMG:/tmp/fresh.png>>".to_string()),
            injected: vec!["queued <<IMG:/tmp/old.png>>".to_string()],
            ..Default::default()
        };

        let keys = attachment_paths_with_live(&state, &live);
        assert!(
            keys.iter().any(|k| k.ends_with("old.png")),
            "a stored tile keeps its handler"
        );
        assert!(
            keys.iter().any(|k| k.ends_with("fresh.png")),
            "the live row's tile gets one too"
        );
        assert_eq!(
            keys.iter().filter(|k| k.ends_with("old.png")).count(),
            1,
            "a path shown twice is listed once, so one handler serves both tiles"
        );
    }

    #[test]
    fn intermediate_text_is_not_appended_twice() {
        // The provider sends the same narration as streamed chunks and again
        // as intermediate text. Both paths land in the card, and the paragraph
        // must appear once (#2442).
        let mut live = LiveTurn::default();
        live.begin("question".into());
        live.note_chunk_at("Checking the parser now.", std::time::Instant::now());
        live.push_narration("Checking the parser now.");
        assert_eq!(
            live.stream, "",
            "the streamed copy moved into the card instead of staying in the answer"
        );
        assert_eq!(
            narration_lines(&live),
            vec!["Checking the parser now.".to_string()],
            "the paragraph appears once"
        );

        // New narration becomes its own card entry.
        live.push_narration("And the writer.");
        assert_eq!(
            narration_lines(&live),
            vec![
                "Checking the parser now.".to_string(),
                "And the writer.".to_string()
            ],
            "the card keeps both paragraphs, in the order they arrived"
        );

        // Reasoning takes the same guard.
        live.push_reasoning_at("think about it", std::time::Instant::now());
        live.push_reasoning_at("think about it", std::time::Instant::now());
        assert_eq!(live.reasoning, "think about it");

        // Anything still streaming when a tool starts is narration, so it
        // joins the card before the row rather than leaking into the answer.
        live.note_chunk_at("one more line", std::time::Instant::now());
        live.push_tool(ToolCall::running("bash".into(), "bash: ls".into()));
        assert_eq!(
            narration_lines(&live),
            vec![
                "Checking the parser now.".to_string(),
                "And the writer.".to_string(),
                "one more line".to_string()
            ],
            "the pending stream flushed into the card"
        );
        assert_eq!(live.stream, "", "the answer bubble starts clean");
    }

    #[test]
    fn tokens_per_second_needs_a_running_turn() {
        let mut live = LiveTurn::default();
        assert_eq!(live.tokens_per_sec(), 0.0);
        live.begin("question".into());
        live.note_chunk_at("hello", std::time::Instant::now());
        assert!(live.tokens_per_sec() >= 0.0);
    }

    #[test]
    fn reasoning_ledger_and_answer_expand_in_order() {
        let ledger = "\n<!-- tools-v2: [{\"d\":\"bash: cargo test\",\"s\":true},{\"d\":\"read_file: src/main.rs\",\"s\":false}] -->\n";
        let content = format!(
            "<!-- reasoning -->I should check the parser.<!-- /reasoning -->{ledger}Done: the fix is in."
        );
        let blocks = flat(&[row("assistant", &content, 1)], 1);

        let thinking = blocks.iter().position(
            |b| matches!(b, Block::Thinking { content } if content.contains("check the parser")),
        );
        let tools = blocks.iter().position(|b| matches!(b, Block::Tools { .. }));
        let answer = blocks.iter().position(
            |b| matches!(b, Block::Msg { content, .. } if content.contains("the fix is in")),
        );
        let thinking = thinking.expect("reasoning block must become thinking");
        let tools = tools.expect("tool ledger must become a Tools block");
        let answer = answer.expect("the final answer must stay a message");
        assert!(
            thinking < tools && tools < answer,
            "order must follow the row"
        );

        let calls = match &blocks[tools] {
            Block::Tools { calls } => calls,
            _ => unreachable!(),
        };
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].description, "bash: cargo test");
        assert!(calls[0].success);
        assert_eq!(calls[1].description, "read_file: src/main.rs");
        assert!(
            !calls[1].success,
            "a failed call must not render as success"
        );
    }

    #[test]
    fn hidden_history_is_announced_not_silently_dropped() {
        let (hidden, _) = turns(&[row("user", "hi", 1)], 250);
        assert_eq!(
            hidden, 249,
            "a windowed load must announce the older rows it hid"
        );
    }

    /// The owner's complaint, pinned: a turn that ran tools must fold behind
    /// the TUI's own header line, keep the question and the answer on screen,
    /// and hide the thinking and the ledger until it is opened.
    #[test]
    fn a_worked_turn_folds_to_the_tui_header() {
        // The exact shape the tool loop persists: every marker opens a line
        // (`messaging.rs` writes `\n<!-- tools-v2: ... -->\n`), and the
        // ledger scan only accepts an opener at a line start (#1587). A
        // single-line fixture would test a grammar that never occurs.
        let ledger = "\n<!-- tools-v2: [{\"d\":\"bash: cargo test\",\"s\":true},{\"d\":\"read_file: src/main.rs\",\"s\":false}] -->\n";
        let assistant = format!(
            "<!-- reasoning -->\nthinking hard.\n<!-- /reasoning -->\n{ledger}Done: the fix is in."
        );
        let rows = [row("user", "fix it", 1), row("assistant", &assistant, 2)];
        let (_, mut turns) = turns(&rows, 2);
        assert_eq!(turns.len(), 1, "one question plus its answer is one turn");
        let turn = turns.pop().unwrap();

        let header = turn
            .header
            .as_deref()
            .expect("a turn that ran tools must carry a fold header");
        // `format_turn_header` flips the mark to a cross when any call
        // failed, so the fixture (one success, one failure) must read
        // "\u{2717} 2 steps \u{b7} 2 tool calls \u{b7} 1 failed".
        assert!(
            header.starts_with("\u{2717}")
                && header.contains("2 steps")
                && header.contains("2 tool calls")
                && header.contains("1 failed"),
            "the header is the TUI's grammar, got {header:?}"
        );
        assert_eq!(
            turn.preview.as_ref().map(|(d, ok)| (d.as_str(), *ok)),
            Some(("read_file: src/main.rs", false)),
            "a folded turn previews its last call"
        );

        let shown: Vec<bool> = turn
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| visible_when_folded(b, turn.final_idx, i))
            .collect();
        // user, (thinking hidden), (tools hidden), answer
        assert_eq!(
            shown,
            vec![true, false, false, true],
            "fold hides exactly the working-out"
        );
        let answer = turn.blocks.last().unwrap();
        assert!(
            matches!(answer, Block::Msg { content, .. } if content.contains("the fix is in")),
            "the final answer must stay visible when folded"
        );
    }

    /// A plain question and answer did no work, so it earns no header at all,
    /// the way `format_turn_header` returns None for zero steps.
    #[test]
    fn a_plain_exchange_gets_no_fold_header() {
        let rows = [row("user", "hi", 1), row("assistant", "hello", 2)];
        let (_, turns) = turns(&rows, 2);
        assert!(turns[0].header.is_none());
    }

    /// #2328: the chevron carries the affordance, so the header says whether
    /// the body is open instead of spelling out which input folds it.
    #[test]
    fn the_fold_chevron_points_at_the_body_while_collapsed() {
        assert_eq!(fold_chevron(true), "\u{25B8}");
        assert_eq!(fold_chevron(false), "\u{25BE}");
    }

    /// #2323: the timer used to vanish on every settled turn because the
    /// header subtracted two timestamps that are equal by construction. The
    /// duration stamped at turn end wins.
    #[test]
    fn the_turn_timer_reads_the_stamped_duration() {
        let ledger = "\n<!-- tools-v2: [{\"d\":\"bash: cargo test\",\"s\":true}] -->\n";
        let content = format!("{ledger}Done, the suite is green.");
        let mut answer = row("assistant", &content, 2);
        answer.duration_secs = Some(165);
        let rows = [row("user", "fix it", 1), answer];
        let (_, mut turns) = turns(&rows, 2);
        let header = turns
            .pop()
            .and_then(|t| t.header)
            .expect("a turn that ran tools carries a header");
        assert!(
            header.contains("2m 45s"),
            "the stamped 165s must reach the header, got {header:?}"
        );
    }

    /// A turn written before the stamp existed still gets a timer: the row
    /// span is the fallback, exactly as in the TUI.
    #[test]
    fn an_unstamped_turn_falls_back_to_the_row_span() {
        let ledger = "\n<!-- tools-v2: [{\"d\":\"bash: cargo test\",\"s\":true}] -->\n";
        let content = format!("{ledger}Done.");
        let mut question = row("user", "fix it", 1);
        question.created_at -= chrono::TimeDelta::seconds(90);
        let rows = [question, row("assistant", &content, 2)];
        let (_, mut turns) = turns(&rows, 2);
        let header = turns
            .pop()
            .and_then(|t| t.header)
            .expect("a turn that ran tools carries a header");
        assert!(
            header.contains("1m 30s"),
            "the span between the rows is 90s, got {header:?}"
        );
    }

    /// #2468: the live lead must never wrap. It was the only child of the head
    /// row that could shrink, and the row sized itself from the lead it was
    /// about to contain, so the text was measured against roughly one
    /// character and stacked vertically until a zoom forced a fresh measure.
    /// The card takes its width from the pane, the head owns that width, and
    /// the lead takes the slack on one line and clips with an ellipsis.
    #[test]
    fn the_thinking_lead_is_drawn_on_one_line() {
        const SRC: &str = include_str!("transcript.rs");
        // Substring scans, never byte offsets: this file is the thing being
        // read, so a hand-rolled index is one rustfmt pass away from a panic
        // mid-codepoint.
        let card = SRC
            .split_once("fn live_card(")
            .expect("the live card")
            .1
            .split_once("fn live_card_footer(")
            .expect("the card footer")
            .0;
        assert!(
            card.contains(".w_full()"),
            "the card takes the pane's width, not its own content's"
        );

        let head_and_lead = card
            .split_once("let mut head = div()")
            .expect("the head row")
            .1;
        let head = head_and_lead
            .split_once("if !lead.is_empty() {")
            .expect("the lead arm")
            .0;
        let lead_arm = head_and_lead
            .split_once("if !lead.is_empty() {")
            .expect("the lead arm")
            .1
            .split_once("let mut card = div()")
            .expect("the card follows the head")
            .0;
        assert!(
            lead_arm.contains(".flex_1()") || lead_arm.contains(".flex_grow()"),
            "the lead takes the row's slack instead of hugging its content"
        );
        assert!(
            lead_arm.contains(".truncate()"),
            "a status line clips with an ellipsis; it never wraps"
        );
        assert!(
            head.contains(".w_full()"),
            "the head row owns the card's width"
        );
    }

    /// Zero is not a duration worth printing: the segment is omitted rather
    /// than reading "0s".
    #[test]
    fn a_zero_duration_is_omitted_from_the_header() {
        let ledger = "\n<!-- tools-v2: [{\"d\":\"bash: cargo test\",\"s\":true}] -->\n";
        let content = format!("{ledger}Done.");
        let rows = [row("user", "fix it", 1), row("assistant", &content, 2)];
        let (_, mut turns) = turns(&rows, 2);
        let header = turns
            .pop()
            .and_then(|t| t.header)
            .expect("a turn that ran tools carries a header");
        assert_eq!(
            header, "\u{2713} 1 step \u{b7} 1 tool call",
            "an equal stamp must drop the duration segment, not print 0s"
        );
    }

    /// #2327: the fold header counts the calls, so the expanded ledger must
    /// list them without repeating the count it already shows.
    #[test]
    fn the_expanded_ledger_does_not_repeat_the_header_count() {
        let calls = [
            ToolCall {
                name: "bash".to_string(),
                task_id: None,
                description: "bash: cargo test".to_string(),
                success: true,
                done: true,
            },
            ToolCall {
                name: "read_file".to_string(),
                task_id: None,
                description: "read_file: src/gui/main.rs".to_string(),
                success: false,
                done: true,
            },
        ];
        let ledger = tool_ledger_text(&calls);
        assert!(
            !ledger.contains("tool call"),
            "the count belongs to the header, not the body, got {ledger:?}"
        );
        assert_eq!(
            ledger, "\u{2705} bash: cargo test\n\u{274c} read_file: src/gui/main.rs",
            "each call keeps its own mark, in the Telegram trio the card uses"
        );
    }

    /// #2324: the settled card carries the turn's OWN ctx and rate, in the
    /// Telegram footer's wording, because the shared turn header keeps ctx
    /// out (#744).
    #[test]
    fn a_settled_turn_prints_its_own_ctx_and_rate() {
        let ledger = "\n<!-- tools-v2: [{\"d\":\"bash: cargo test\",\"s\":true}] -->\n";
        let content = format!("{ledger}Done.");
        let rows = [row("user", "fix it", 1), row("assistant", &content, 2)];
        let (_, plain) = turns(&rows, 2);
        let anchor = plain[0].anchor.clone();

        let mut known: HashMap<String, TurnMetrics> = HashMap::new();
        known.insert(
            anchor,
            TurnMetrics {
                ctx_used: 68_000,
                ctx_max: 200_000,
                tps: Some(58.0),
                tool_calls: 0,
            },
        );
        let (_, mut turns) = turns_with_origins(&rows, 2, &known, &HashMap::new());
        let turn = turns.pop().expect("one turn");
        let segment = turn
            .metrics
            .and_then(|m| m.segment())
            .expect("a remembered turn prints its metrics");
        assert_eq!(
            segment, "ctx: 68K/200K 34% | 58 tok/s",
            "the card must read exactly as the Telegram footer does"
        );
    }

    /// #2632: a persisted row that lost its ledger must not turn the settled
    /// card into "this turn did nothing". What the live card counted is the
    /// floor, so finishing changes the verb and never the numbers.
    #[test]
    fn a_row_that_lost_its_ledger_keeps_the_live_tally() {
        let mut rows = [
            row("user", "check the logs", 1),
            row("assistant", "All clear.", 2),
        ];
        // The clock is part of the same footer, so the turn carries one.
        rows[1].duration_secs = Some(1);
        let (_, plain) = turns(&rows, 2);
        let anchor = plain[0].anchor.clone();

        let mut known: HashMap<String, TurnMetrics> = HashMap::new();
        known.insert(
            anchor,
            TurnMetrics {
                ctx_used: 53_000,
                ctx_max: 200_000,
                tps: Some(31.0),
                tool_calls: 4,
            },
        );
        let (_, mut turns) = turns_with_origins(&rows, 2, &known, &HashMap::new());
        let turn = turns.pop().expect("one turn");
        assert_eq!(
            turn.summary.tool_calls, 4,
            "a row with no ledger still knows how many calls the turn made"
        );
        assert_eq!(
            turn_footer_text(&turn).as_deref(),
            Some("4 tool calls \u{b7} ctx: 53K/200K 26% | 31 tok/s \u{b7} \u{23f1} 0:01"),
            "the settled footer prints the live card's numbers, not fewer"
        );
    }

    /// A provider that reports no rate still gets the honest half: ctx only,
    /// never a fabricated tok/s (#2632).
    #[test]
    fn a_turn_without_a_rate_prints_ctx_only() {
        let metrics = TurnMetrics {
            ctx_used: 12_000,
            ctx_max: 200_000,
            tps: None,
            tool_calls: 0,
        };
        assert_eq!(metrics.segment().as_deref(), Some("ctx: 12K/200K 6%"));
    }

    /// No record means no segment: borrowing the session's current number
    /// would be a lie about what an older turn cost (#2324 D3).
    #[test]
    fn a_turn_with_no_record_prints_no_metrics() {
        let ledger = "\n<!-- tools-v2: [{\"d\":\"bash: cargo test\",\"s\":true}] -->\n";
        let content = format!("{ledger}Done.");
        let rows = [row("user", "fix it", 1), row("assistant", &content, 2)];
        let (_, mut turns) = turns(&rows, 2);
        let turn = turns.pop().expect("one turn");
        assert!(turn.metrics.is_none());
        assert_eq!(turn.metrics.and_then(|m| m.segment()), None);
    }

    /// The owner's "the session I sent the request to was empty afterwards",
    /// pinned as state: a turn that streams nothing still keeps the answer
    /// the agent returned, and a turn that streamed keeps its stream until
    /// the DB load takes over.
    #[test]
    fn the_live_turn_keeps_the_answer_until_the_db_has_it() {
        let mut live = LiveTurn::default();
        assert!(live.is_empty(), "nothing in flight draws nothing");
        assert_eq!(live.answer(), None);

        live.begin("fix it".into());
        assert_eq!(live.user.as_deref(), Some("fix it"));
        assert!(
            live.answer().is_none(),
            "a question with no answer yet shows no answer"
        );

        // A turn that never streamed a chunk: the response is the answer.
        live.settled = Some("done: the fix is in".into());
        assert_eq!(live.answer(), Some("done: the fix is in"));

        // A turn that streamed: the stream wins while it is the newest text.
        live.stream.push_str("streamed");
        assert_eq!(live.answer(), Some("streamed"));

        // Once the DB has the rows the live copy goes, and with it the
        // answer that would otherwise draw twice.
        live.clear();
        assert!(live.is_empty());
        assert_eq!(live.answer(), None);
    }

    /// A started tool is neither success nor failure: the live row says so,
    /// and the settled ledger row keeps the tick the TUI draws.
    #[test]
    fn a_running_call_is_not_drawn_as_a_failure() {
        let live = LiveTurn {
            tools: vec![ToolCall::running("bash".into(), "bash: cargo test".into())],
            ..Default::default()
        };
        assert!(!live.is_empty(), "a running tool keeps the turn visible");
        assert!(!live.tools[0].done && !live.tools[0].success);

        let ledger = "<!-- tools-v2: [{\"d\":\"bash: cargo test\",\"s\":true}] -->";
        let rows = [row("user", "run it", 1), row("assistant", ledger, 2)];
        let calls = flat(&rows, 2)
            .into_iter()
            .find_map(|b| match b {
                Block::Tools { calls } => Some(calls),
                _ => None,
            })
            .expect("the ledger expands into a tool block");
        assert!(
            calls[0].done && calls[0].success,
            "a persisted call is settled"
        );
    }

    #[test]
    fn markdown_structure_survives_parsing() {
        let blocks = crate::markdown::blocks(
            "## Heading\n\n- one\n- two\n\nuse `speed` here\n\n```\ncode line\n```",
        );
        let lines: Vec<_> = blocks
            .into_iter()
            .filter_map(|b| match b {
                crate::markdown::MdBlock::Line(l) => Some(l),
                crate::markdown::MdBlock::Table(_) => None,
            })
            .collect();
        assert!(
            lines
                .iter()
                .any(|l| l.heading && l.text.contains("Heading")),
            "headings must be flagged for the renderer"
        );
        assert!(
            lines.iter().any(|l| l.mono && l.text.contains("code line")),
            "fenced code must be mono"
        );
        assert!(
            lines.iter().any(|l| !l.highlights.is_empty()),
            "inline emphasis must carry highlight runs"
        );
    }

    /// The regression this module was rebuilt for: a table used to arrive as
    /// one line per cell, so the window drew a column of words where the TUI
    /// drew a grid. It must parse as a table block with real columns.
    #[test]
    fn table_parses_as_a_grid_not_as_loose_lines() {
        let blocks = crate::markdown::blocks(
            "| model | cost | note |\n|---|---|---|\n| a | $1 | fast |\n| b | $2 | slow |\n",
        );
        let table = blocks
            .iter()
            .find_map(|b| match b {
                crate::markdown::MdBlock::Table(t) => Some(t),
                crate::markdown::MdBlock::Line(_) => None,
            })
            .expect("a markdown table must parse as a table block, not as lines");
        assert_eq!(table.columns(), 3, "three columns, never three lines");
        assert_eq!(table.header.len(), 3);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.header[1].text, "cost");
        assert_eq!(table.rows[1][2].text, "slow");
    }

    /// #2541: the header band follows the table's default radius, and every
    /// cell shares enough horizontal inset to keep text off the edges.
    #[test]
    fn table_header_matches_card_radius_and_cells_breathe() {
        let src = include_str!("transcript.rs");
        let table = src
            .split("fn md_table(")
            .nth(1)
            .and_then(|rest| rest.split("/// Vertical rhythm ladder").next())
            .expect("markdown table renderer");
        assert!(table.contains(".rounded_tl(px(CARD_RADIUS))"));
        assert!(table.contains(".rounded_tr(px(CARD_RADIUS))"));
        assert!(!table.contains("concentric_radius(0.0)"));
        assert_eq!(RHYTHM_CELL_PAD_X, 16.0);
    }

    /// #2325: reasoning is output, so its time belongs in the denominator the
    /// same way the channel bridge counts its tokens in the numerator.
    /// Excluding it would report a reasoning-heavy turn at its final answer's
    /// speed alone, which is the mirror image of the tool-wait bug.
    #[test]
    fn reasoning_time_counts_toward_the_streaming_window() {
        let base = std::time::Instant::now();
        let at = |secs: u64| base + std::time::Duration::from_secs(secs);

        let mut live = LiveTurn::default();
        live.begin("think about it".into());

        // 0s..3s: reasoning streams.
        for secs in 0..=3 {
            live.note_reasoning_at("hmm ", at(secs));
        }
        // 3s..5s: the answer follows without a pause.
        for secs in 4..=5 {
            live.note_chunk_at("answer ", at(secs));
        }

        let active = live.streaming_active_secs();
        assert!(
            (active - 5.0).abs() < 1e-6,
            "reasoning and answer share one window of five seconds, got {active}"
        );
        assert!(live.tokens > 0, "reasoning tokens count toward the turn");
    }

    /// #2325: the live rate divides by ACTIVE streaming time only. Four
    /// seconds of stream, a forty second tool run, four more seconds of
    /// stream is eight seconds of streaming, not forty eight, so a fast turn
    /// stops reporting itself as a crawl the moment a tool runs.
    #[test]
    fn a_tool_wait_is_not_billed_to_the_streaming_rate() {
        let base = std::time::Instant::now();
        let at = |secs: u64| base + std::time::Duration::from_secs(secs);

        let mut live = LiveTurn::default();
        live.begin("fix it".into());

        // 0s..4s: the first response streams, one chunk per second.
        for secs in 0..=4 {
            live.note_chunk_at("word ", at(secs));
        }
        let after_first = live.streaming_active_secs();
        assert!(
            (after_first - 4.0).abs() < 1e-6,
            "the first window is four seconds, got {after_first}"
        );

        // 40s tool run, then the second response streams 44s..48s.
        for secs in 44..=48 {
            live.note_chunk_at("word ", at(secs));
        }
        let active = live.streaming_active_secs();
        assert!(
            (active - 8.0).abs() < 1e-6,
            "two four second windows are eight seconds of streaming, not wall clock: got {active}"
        );

        // The finalized rate is the turn's tokens over those eight seconds.
        let tokens = live.tokens as f64;
        live.finalize(None);
        let rate = live.tokens_per_sec();
        let expected = (tokens / 8.0) as f32;
        assert!(
            (rate - expected).abs() < 0.01,
            "rate must be {tokens} tokens over 8s, got {rate}"
        );
        assert!(
            rate > (tokens / 48.0) as f32 * 2.0,
            "wall clock would have split the rate roughly six ways"
        );
    }

    /// A settled turn keeps showing its rate: the footer must not blank the
    /// number the instant the response finishes.
    #[test]
    fn a_finalized_rate_survives_the_idle_turn() {
        let base = std::time::Instant::now();
        let mut live = LiveTurn::default();
        live.begin("fix it".into());
        live.note_chunk_at("word ", base);
        live.note_chunk_at("word ", base + std::time::Duration::from_secs(2));

        live.finalize(Some(58.0));
        assert_eq!(live.tokens_per_sec(), 58.0, "the service rate wins");
        assert_eq!(
            live.streaming_active_secs(),
            0.0,
            "the accumulator resets with the turn"
        );

        live.clear();
        assert_eq!(
            live.tokens_per_sec(),
            58.0,
            "an idle footer keeps the last rate instead of blanking"
        );
        assert_eq!(live.streaming_active_secs(), 0.0);
    }

    /// #2703: the window must survive how the UI happens to poll.
    ///
    /// `agent_bridge::drain` hands the whole batch over at once and the fold
    /// walks it in one loop, so every chunk in a burst was stamped with a fold
    /// time microseconds after the one before it. Measured on the owner's
    /// screen that read 4640 tok/s where the log's own numbers imply ~180.
    ///
    /// The bound is not invented: the service refuses to report a rate above
    /// `ui_core::rate::MAX_PLAUSIBLE_TOK_S` (2000), so a surface that prints
    /// more than that is printing a number no other surface would.
    #[test]
    fn tps_a_folded_batch_does_not_print_a_rate_the_service_would_refuse() {
        const SERVICE_CEILING: f32 = 2000.0;

        // The receipt this reproduces (#2703): one turn logged 18382 tokens
        // beside 592s of wall clock, and the surface printed 4640 tok/s. The
        // token count was honest; the WINDOW was the lie. `agent_bridge::drain`
        // hands the UI a batch and the fold stamps every chunk with
        // `Instant::now()` as it walks that batch, so the whole burst lands
        // inside a few dozen milliseconds of clock no matter how long the
        // provider actually spent streaming it.
        //
        // The clock is injected here so the collapse is exact instead of
        // depending on how fast this machine happens to fold 12 strings.
        let base = std::time::Instant::now();
        let mut live = LiveTurn::default();
        live.begin("explain the design".into());
        for i in 0..400u64 {
            let text = "the quick brown fox jumps ";
            live.note_chunk_at(text, base + std::time::Duration::from_micros(i * 250));
        }

        let tokens = live.tokens;
        let active = live.streaming_active_secs();
        let rate = live.tokens_per_sec();
        assert!(
            tokens > 0,
            "the fold counted no tokens, so the rate is moot"
        );
        assert!(
            active < 1.0,
            "this test must model a collapsed window, got {active:.3}s"
        );
        assert!(
            rate <= SERVICE_CEILING,
            "{tokens} tokens over {active:.4}s of folded clock reads {rate:.0} tok/s, \
             over the service's own {SERVICE_CEILING:.0} ceiling: the window collapsed \
             because the clock is read at fold time, not at arrival"
        );
    }

    /// #2703: a burst that collapses the window keeps the rate already on
    /// screen instead of adopting an unbounded estimate.
    ///
    /// The first half of the bug is the too-short window; the second half is
    /// that nothing bounded what the too-short window produced. Both halves
    /// are pinned here: the burst is refused, and the footer keeps the number
    /// it was already showing rather than blanking to zero.
    #[test]
    fn tps_a_collapsed_window_keeps_the_last_credible_rate() {
        let base = std::time::Instant::now();
        let mut live = LiveTurn::default();

        // A turn that streamed: one chunk every 100ms for four seconds.
        live.begin("first turn".into());
        for i in 0..40u64 {
            live.note_chunk_at(
                "the quick brown fox jumps ",
                base + std::time::Duration::from_millis(i * 100),
            );
        }
        live.finalize(None);
        let credible = live.tokens_per_sec();
        assert!(
            credible > 0.0 && credible <= 2000.0,
            "a turn that streamed must measure a believable rate, got {credible}"
        );

        // The next turn arrives as one burst, so the window collapses.
        live.begin("second turn".into());
        for i in 0..400u64 {
            live.note_chunk_at(
                "the quick brown fox jumps ",
                base + std::time::Duration::from_secs(30)
                    + std::time::Duration::from_micros(i * 250),
            );
        }
        assert_eq!(
            live.tokens_per_sec(),
            credible,
            "a collapsed window must keep the rate already on screen"
        );
    }

    /// #2703: two bursts three seconds apart still measure a believable rate.
    ///
    /// This is the shape the tracker exists for. A provider that answers in
    /// bursts leaves gaps the idle rule must exclude, and the time inside each
    /// burst is still streaming time. With arrival stamps the two windows
    /// accumulate; with fold-time stamps the same batch lands in microseconds
    /// and the window reads zero.
    #[test]
    fn tps_two_bursts_three_seconds_apart_measure_a_believable_rate() {
        let base = std::time::Instant::now();
        let mut live = LiveTurn::default();
        live.begin("summarise the design".into());

        // Burst one: five chunks 100ms apart, so 0.4s of streaming.
        for i in 0..5u64 {
            live.note_chunk_at(
                "the quick brown fox jumps ",
                base + std::time::Duration::from_millis(i * 100),
            );
        }
        // Burst two, three seconds later: the gap exceeds the 1.0s idle rule,
        // so it closes the first window and opens a second one.
        for i in 0..5u64 {
            live.note_chunk_at(
                "the quick brown fox jumps ",
                base + std::time::Duration::from_millis(3400 + i * 100),
            );
        }

        let active = live.streaming_active_secs();
        assert!(
            (active - 0.8).abs() < 0.01,
            "both bursts must count: 0.4s + 0.4s of streaming, got {active:.3}s"
        );

        let rate = live.tokens_per_sec();
        assert!(
            rate > 0.0 && rate <= 2000.0,
            "ten chunks over 0.8s is a believable rate, got {rate:.0} tok/s"
        );
    }

    /// #2703: the window reports exactly what the shared tracker measures.
    ///
    /// The terminal and the window run one tracker now (`ui_core::rate`), so
    /// the same event sequence must produce the same number and the same
    /// window. Before the move the window kept its own copy, and that copy is
    /// what drifted: it read the clock when it drained a batch instead of when
    /// the chunks arrived.
    #[test]
    fn tps_the_window_reports_exactly_what_the_shared_tracker_measures() {
        let base = std::time::Instant::now();
        let mut live = LiveTurn::default();
        live.begin("same events both sides".into());
        let mut shared = opencrabs::ui_core::rate::StreamingTpsTracker::default();

        for i in 0..60u64 {
            let at = base + std::time::Duration::from_millis(i * 100);
            live.note_chunk_at("the quick brown fox jumps ", at);
            shared.advance(at);
        }

        assert_eq!(
            live.streaming_active_secs(),
            shared.active_secs(),
            "the window must measure the same streaming time the tracker does"
        );
        let shared_rate =
            opencrabs::ui_core::rate::credible_rate(live.tokens, shared.active_secs())
                .expect("six seconds of streaming must measure");
        assert_eq!(
            live.tokens_per_sec(),
            shared_rate as f32,
            "the window must report the shared tracker's number, not its own"
        );
    }

    /// A turn that produced no tokens must not overwrite the last good rate
    /// with a fake zero, and a rate of zero from the provider is not a
    /// measurement either.
    #[test]
    fn a_tokenless_turn_keeps_the_previous_rate() {
        let mut live = LiveTurn::default();
        live.begin("fix it".into());
        live.finalize(Some(0.0));
        assert_eq!(live.tokens_per_sec(), 0.0, "nothing measured yet");

        live.begin("again".into());
        live.finalize(Some(41.5));
        live.begin("and again".into());
        live.finalize(None);
        assert_eq!(
            live.tokens_per_sec(),
            41.5,
            "an empty turn must not erase the last real rate"
        );
    }

    /// #2326: while a call runs, the live line names that call instead of
    /// claiming the crab is still thinking, and it counts the call it is
    /// running.
    #[test]
    fn a_running_tool_leads_the_live_line() {
        let mut live = LiveTurn::default();
        live.begin("run it".into());
        live.tools
            .push(ToolCall::running("bash".into(), "bash: cargo test".into()));

        let text = live_status_text(&live);
        assert!(text.contains("bash: cargo test"), "got {text}");
        assert!(text.contains("1 tool call"), "got {text}");
        assert!(!text.contains("thinking"), "got {text}");
    }

    /// #2326: with nothing running, the line holds the last call that ran
    /// and the total count, so a long turn still says what it last did.
    #[test]
    fn the_last_finished_call_holds_the_line_when_nothing_runs() {
        let mut live = LiveTurn::default();
        live.begin("read them".into());
        for n in 0..9 {
            live.tools.push(ToolCall {
                name: "read_file".into(),
                task_id: None,
                description: format!("Read file{n}.rs"),
                success: true,
                done: true,
            });
        }

        let text = live_status_text(&live);
        assert!(text.contains("Read file8.rs"), "got {text}");
        assert!(text.contains("9 tool calls"), "got {text}");
        assert!(
            !text.contains("Read file0.rs"),
            "the newest call leads: {text}"
        );
        assert!(!text.contains("thinking"), "got {text}");
    }

    /// #2326: the running call outranks every finished one, even when the
    /// finished one is newer in the vector.
    #[test]
    fn a_running_call_outranks_the_last_finished_one() {
        let mut live = LiveTurn::default();
        live.begin("run it".into());
        live.tools
            .push(ToolCall::running("bash".into(), "bash: cargo test".into()));
        live.tools.push(ToolCall {
            name: "read_file".into(),
            task_id: None,
            description: "Read Cargo.toml".into(),
            success: true,
            done: true,
        });
        live.tools.push(ToolCall {
            name: "bash".into(),
            task_id: None,
            description: "bash: cargo clippy".into(),
            success: true,
            done: true,
        });

        let text = live_status_text(&live);
        assert!(text.contains("bash: cargo test"), "got {text}");
        assert!(text.contains("3 tool calls"), "got {text}");
    }

    /// #2326: a turn that has produced nothing yet keeps the crab's own name
    /// on the line, so a cold provider still looks alive.
    #[test]
    fn an_idle_live_turn_still_says_it_is_thinking() {
        let mut live = LiveTurn::default();
        live.begin("hello".into());
        assert_eq!(live_status_text(&live), "OpenCrabs is thinking...");
    }

    /// #2326: once answer text arrives the line stops saying "thinking" even
    /// though no tool has started: the stream itself is the activity.
    #[test]
    fn a_streaming_turn_without_tools_stops_saying_thinking() {
        let mut live = LiveTurn::default();
        live.begin("hello".into());
        live.note_chunk_at("hello there, streaming now ", std::time::Instant::now());

        let text = live_status_text(&live);
        assert!(!text.contains("thinking"), "got {text}");
        assert!(text.contains("tok this turn"), "got {text}");
    }

    /// A detached row keeps the id its finish will name, and says so in the
    /// description: a row that outlives the turn must not read like a call
    /// that is blocking it.
    #[test]
    fn a_detached_row_carries_its_task_id_and_label() {
        let call = ToolCall::detached("task-7".into(), "cargo test".into());
        assert_eq!(call.task_id.as_deref(), Some("task-7"));
        assert!(!call.done, "a detached command starts unresolved");
        assert!(
            call.description.contains("cargo test") && call.description.contains("detached"),
            "description names the label and the detachment: {}",
            call.description
        );
    }

    /// The finish settles the row it opened, with the elapsed time the event
    /// reported.
    #[test]
    fn settle_detached_marks_the_matching_row_done() {
        let mut rows = vec![ToolCall::detached("task-7".into(), "cargo test".into())];
        assert!(settle_detached(&mut rows, "task-7", true, 12.34));
        assert!(rows[0].done && rows[0].success);
        assert!(
            rows[0].description.contains("12.3s"),
            "the elapsed time lands in the row: {}",
            rows[0].description
        );
    }

    /// A finish for a task this turn never drew settles nothing and says so,
    /// instead of closing some other row.
    #[test]
    fn settle_detached_ignores_an_unknown_task_id() {
        let mut rows = vec![ToolCall::detached("task-7".into(), "cargo test".into())];
        assert!(!settle_detached(&mut rows, "task-9", true, 3.0));
        assert!(!rows[0].done, "an unrelated finish leaves the row running");
    }

    /// Provider calls carry no task id, so a detached finish can never settle
    /// one: which command finished is answered by identity, not by position.
    #[test]
    fn settle_detached_never_touches_a_provider_row() {
        let mut rows = vec![ToolCall::running("bash".into(), "bash: ls".into())];
        assert!(!settle_detached(&mut rows, "", true, 1.0));
        assert!(!rows[0].done);
        assert!(rows[0].task_id.is_none());
    }

    /// Where a needle sits in this file's own source. Pins read the renderer
    /// they describe instead of trusting a comment about it.
    fn src_at(src: &str, needle: &str) -> usize {
        src.find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?} in the source"))
    }

    /// #2442: the card draws its header first, its body second and its footer
    /// last. The three loose children it replaced had the status line BELOW
    /// the rows, which is the shape the owner read as a wall of tools.
    #[test]
    fn the_live_card_puts_the_header_first_and_the_footer_last() {
        let mut live = LiveTurn::default();
        live.begin("look at it".into());
        live.push_tool(ToolCall {
            name: "bash".into(),
            task_id: None,
            description: "bash: git status".into(),
            success: true,
            done: true,
        });
        live.push_narration("Branch is clean");
        live.push_tool(ToolCall::running(
            "read_file".into(),
            "read_file: src/gui/main.rs".into(),
        ));

        let lines = live_card_lines(&live);
        assert_eq!(lines.len(), 3, "one line per entry, none for the echo");
        assert!(
            matches!(lines[0], LiveCardLine::Tool { .. }),
            "the first call leads the body"
        );
        assert!(
            matches!(lines[1], LiveCardLine::Narration(ref t) if t == "Branch is clean"),
            "narration keeps the place it happened in"
        );
        assert!(
            matches!(lines[2], LiveCardLine::Tool { .. }),
            "the call after the narration follows it"
        );
        let footer =
            live_card_footer(&live, (Some(12_000), 200_000)).expect("a footer once a tool ran");
        assert!(footer.contains("2 tool calls"), "got {footer}");

        // And the drawing order itself: header, then body, then footer.
        const SRC: &str = include_str!("transcript.rs");
        let head = [".child(", "head)"].concat();
        let body = ["live_card_", "body(live, drag)"].concat();
        let foot = ["live_card_", "footer(live, ctx)"].concat();
        assert!(
            src_at(SRC, &head) < src_at(SRC, &body),
            "the header is built before the body"
        );
        assert!(
            src_at(SRC, &body) < src_at(SRC, &foot),
            "the footer is drawn after the body"
        );
    }

    /// #2442: the card combines while the turn is still in flight. Waiting for
    /// `settled` is what made the running block look nothing like the one that
    /// replaced it a second later.
    #[test]
    fn the_live_card_combines_in_flight_without_waiting_for_settle() {
        let mut live = LiveTurn::default();
        live.begin("run it".into());
        live.push_tool(ToolCall::running("bash".into(), "bash: cargo test".into()));
        assert!(live.settled.is_none(), "nothing settled yet");

        let lines = live_card_lines(&live);
        assert_eq!(lines.len(), 1, "the running row is already in the body");

        live.settled = Some("done".into());
        assert_eq!(
            live_card_lines(&live).len(),
            1,
            "settling adds no second copy of the same row"
        );
    }

    /// #2442: the rows wear Telegram's marks, not the terminal's tick and cross,
    /// so the block reads like the card the other surfaces draw.
    #[test]
    fn tool_rows_use_the_telegram_icons() {
        let running = ToolCall::running("bash".into(), "bash: ls".into());
        let done = ToolCall {
            name: "bash".into(),
            task_id: None,
            description: "bash: ls".into(),
            success: true,
            done: true,
        };
        let failed = ToolCall {
            name: "bash".into(),
            task_id: None,
            description: "bash: ls".into(),
            success: false,
            done: true,
        };
        assert_eq!(
            tool_mark(&running).0,
            "\u{2699}\u{fe0f}",
            "gear while running"
        );
        assert_eq!(tool_mark(&done).0, "\u{2705}", "check when it worked");
        assert_eq!(tool_mark(&failed).0, "\u{274c}", "cross when it failed");
        assert!(
            !tool_mark(&done).0.contains('\u{2713}'),
            "the terminal's tick must not leak into the card"
        );
    }

    /// #2632: the settled card prints the live card's footer, in the live
    /// card's own words, so finishing changes the verb and nothing else. The
    /// header stays a name, never a cost (#2442).
    #[test]
    fn the_settled_card_keeps_the_live_cards_footer() {
        let mut live = LiveTurn::default();
        live.begin("go".into());
        live.push_tool(ToolCall::running("bash".into(), "bash: ls".into()));
        let footer = live_card_footer(&live, (Some(72_000), 200_000)).expect("footer");
        assert!(footer.contains("ctx: 72K/200K 36%"), "got {footer}");
        let (lead, _) = live_status_line(&live);
        assert!(
            !lead.contains("ctx:"),
            "the header must stay ctx free: {lead}"
        );

        // Settled: same segment, plus the count it made and the clock it took,
        // and a head that states the verdict instead of the step tally.
        let turn = Turn {
            header: Some("\u{2705} 1 step \u{b7} 1 tool call".into()),
            summary: TurnSummary {
                tool_calls: 1,
                duration_secs: 24,
                ..Default::default()
            },
            metrics: Some(TurnMetrics {
                ctx_used: 72_000,
                ctx_max: 200_000,
                tps: Some(58.0),
                tool_calls: 1,
            }),
            preview: None,
            anchor: "a".into(),
            blocks: Vec::new(),
            final_idx: None,
            origin: None,
        };
        assert_eq!(
            turn_footer_text(&turn).as_deref(),
            Some("1 tool call \u{b7} ctx: 72K/200K 36% | 58 tok/s \u{b7} \u{23f1} 0:24"),
            "the footer is the live card's line, settled: calls, ctx, its own rate, clock"
        );
        assert_eq!(
            settled_verdict(&turn),
            ("\u{2705}", "Finished"),
            "a clean turn says Finished, not its step count"
        );
        assert!(
            !turn.header.as_deref().unwrap_or_default().contains("ctx:"),
            "the header never carries ctx"
        );
    }

    /// #2632: a turn this process never measured claims no ctx, no rate and no
    /// clock rather than borrowing the session's current numbers (#2324 D3).
    #[test]
    fn a_turn_with_no_record_claims_no_numbers() {
        let measured = Turn {
            header: Some("\u{2705} 2 steps".into()),
            summary: TurnSummary {
                tool_calls: 2,
                duration_secs: 0,
                ..Default::default()
            },
            metrics: None,
            preview: None,
            anchor: "b".into(),
            blocks: Vec::new(),
            final_idx: None,
            origin: None,
        };
        assert_eq!(
            turn_footer_text(&measured).as_deref(),
            Some("2 tool calls"),
            "the count it knows, nothing it does not"
        );
        let unmeasured = Turn {
            header: Some("x".into()),
            summary: TurnSummary::default(),
            metrics: None,
            preview: None,
            anchor: "c".into(),
            blocks: Vec::new(),
            final_idx: None,
            origin: None,
        };
        assert!(
            turn_footer_text(&unmeasured).is_none(),
            "no record, no number"
        );
        // The tally alone is not the verdict (#2707): a failed call with no
        // answer after it is the shape of a turn that ended on the failure.
        let dead_end = Turn {
            header: Some("\u{274c} 1 step \u{b7} 1 tool call".into()),
            summary: TurnSummary {
                tool_calls: 1,
                failed: 1,
                ..Default::default()
            },
            metrics: None,
            preview: None,
            anchor: "d".into(),
            blocks: vec![Block::Tools {
                calls: vec![ToolCall {
                    name: "bash".into(),
                    task_id: None,
                    description: "bash: exit 1".into(),
                    success: false,
                    done: true,
                }],
            }],
            final_idx: None,
            origin: None,
        };
        assert_eq!(
            settled_verdict(&dead_end),
            ("\u{274c}", "Failed"),
            "one failed call and no answer after it, the head says Failed"
        );
        assert_eq!(clock_glyph(9), "\u{23f1} 0:09", "the clock pads seconds");
        assert_eq!(clock_glyph(3_725), "\u{23f1} 1:02:05", "and rolls hours");
    }

    /// #2707: the verdict asks whether the turn ENDED on a failure, not how
    /// many calls failed along the way. A refusal the model worked around
    /// leaves assistant text after its failed call, and that turn finished its
    /// job. Only the LAST failure counts: text between two failures excuses the
    /// first and not the second.
    #[test]
    fn answered_after_last_failure() {
        fn failed_call() -> ToolCall {
            ToolCall {
                name: "bash".into(),
                task_id: None,
                description: "bash: exit 1".into(),
                success: false,
                done: true,
            }
        }
        fn answer(text: &str) -> Block {
            Block::Msg {
                role: "assistant".into(),
                content: text.into(),
                time: String::new(),
                placeholder: false,
                message_id: None,
                forward: None,
            }
        }
        fn turn(blocks: Vec<Block>, failed: usize) -> Turn {
            Turn {
                header: Some("x".into()),
                summary: TurnSummary {
                    tool_calls: failed,
                    failed,
                    ..Default::default()
                },
                metrics: None,
                preview: None,
                anchor: "f".into(),
                blocks,
                final_idx: None,
                origin: None,
            }
        }

        let recovered = turn(
            vec![
                Block::Tools {
                    calls: vec![failed_call()],
                },
                answer("done"),
            ],
            1,
        );
        assert!(
            answered_after_the_last_failure(&recovered),
            "text after the failure is the answer the turn was working toward"
        );
        assert_eq!(
            settled_verdict(&recovered),
            ("\u{2705}", "Finished"),
            "a self-corrected refusal is not a failed turn"
        );

        let dead_end = turn(
            vec![
                answer("trying"),
                Block::Tools {
                    calls: vec![failed_call()],
                },
            ],
            1,
        );
        assert!(
            !answered_after_the_last_failure(&dead_end),
            "a failure after the last text is a dead end"
        );
        assert_eq!(
            settled_verdict(&dead_end),
            ("\u{274c}", "Failed"),
            "a turn that ends on a failure says Failed"
        );

        let two_failures = turn(
            vec![
                Block::Tools {
                    calls: vec![failed_call()],
                },
                answer("let me try another way"),
                Block::Tools {
                    calls: vec![failed_call()],
                },
            ],
            2,
        );
        assert_eq!(
            settled_verdict(&two_failures),
            ("\u{274c}", "Failed"),
            "the LAST failure is the one the verdict reads"
        );

        let clean = turn(vec![answer("all good")], 0);
        assert!(
            !answered_after_the_last_failure(&clean),
            "a turn with no failed call has nothing to answer"
        );
        assert_eq!(settled_verdict(&clean), ("\u{2705}", "Finished"));

        let errored = Turn {
            summary: TurnSummary {
                has_error: true,
                ..Default::default()
            },
            ..turn(vec![answer("the answer arrived anyway")], 0)
        };
        assert_eq!(
            settled_verdict(&errored),
            ("\u{274c}", "Failed"),
            "a harness error row still means Failed, whatever came after it"
        );
    }

    /// #2442: the live card is the only block now, so the pane never grows a
    /// separate tools ledger with its own count beside it.
    #[test]
    fn the_live_pane_draws_one_card_not_a_ledger_plus_a_status_line() {
        const SRC: &str = include_str!("transcript.rs");
        let (_, after) = SRC
            .split_once(["fn live_", "view("].concat().as_str())
            .expect("live_view exists");
        let (live_src, _) = after
            .split_once(["/// The transcript pane"].concat().as_str())
            .or_else(|| after.split_once("\n}\n"))
            .expect("live_view ends");
        assert!(
            !live_src.contains(["Block::", "Tools"].concat().as_str()),
            "the live pane must not draw the tools ledger any more"
        );
        assert!(
            !live_src.contains(["status_", "line(live)"].concat().as_str()),
            "nor a status line below the rows"
        );
    }

    /// The live turn is the only copy of the answer the user can see while the
    /// transcript is short of a settled turn, so its rows have to take part in
    /// a drag exactly like the settled rows do. They were the one renderer left
    /// out, which is how a stranded live panel became a pane with nothing
    /// selectable in it.
    #[test]
    fn the_live_pane_copies_like_the_settled_rows() {
        const SRC: &str = include_str!("transcript.rs");
        let (_, after) = SRC
            .split_once(["fn live_", "view("].concat().as_str())
            .expect("live_view exists");
        let (live_src, _) = after
            .split_once(["/// The transcript pane"].concat().as_str())
            .expect("live_view ends");
        assert!(
            live_src.contains(["drag: Option<&Rc<", "CopyDrag>>,"].concat().as_str()),
            "the live pane takes the pane's own drag state"
        );
        assert_eq!(
            live_src
                .matches(["live_", "block(drag, &block"].concat().as_str())
                .count(),
            3,
            "the question, the queued rows and the answer all copy"
        );
        let (_, helper) = SRC
            .split_once(["fn live_", "block("].concat().as_str())
            .expect("live_block exists");
        let (helper_body, _) = helper.split_once("\n}\n").expect("live_block ends");
        assert!(
            helper_body.contains(["selectable_", "row("].concat().as_str()),
            "a live row is the same selectable row the transcript draws"
        );
    }

    /// #2678: the rows that are not messages have to copy too. A tool call, the
    /// completed plan card and the live card's ledger each draw their text
    /// through the same selectable segment, and each one's copy text is the
    /// string it paints, so a drag over one name copies that name alone.
    #[test]
    fn every_text_row_copies_not_just_the_messages() {
        const SRC: &str = include_str!("transcript.rs");
        let (_, tools) = SRC
            .split_once(["fn tools_", "block("].concat().as_str())
            .expect("tools_block exists");
        let (tools_body, _) = tools.split_once("\n}\n").expect("tools_block ends");
        assert!(
            tools_body.contains(["selectable_", "styled_text("].concat().as_str()),
            "a tool call's description is a selectable segment"
        );
        assert!(
            tools_body.contains(["tool_", "line(call)"].concat().as_str()),
            "the row draws exactly the text it copies"
        );
        let (_, card) = SRC
            .split_once(["fn live_card_", "body("].concat().as_str())
            .expect("live_card_body exists");
        let (card_src, _) = card.split_once("\n}\n").expect("live_card_body ends");
        assert!(
            card_src.contains(["texts.push(row_", "text)"].concat().as_str()),
            "the live card's ledger registers its own copy row"
        );
        assert!(
            card_src.contains(
                ["live_card_line(line, Some(&", "selection))"]
                    .concat()
                    .as_str()
            ),
            "every ledger line is drawn as a segment of that row"
        );
        let (_, block) = SRC
            .split_once(["fn block_", "text("].concat().as_str())
            .expect("block_text exists");
        let (block_src, _) = block.split_once("\n}\n").expect("block_text ends");
        assert!(
            block_src.contains(["map(tool_", "line)"].concat().as_str()),
            "the copy text of a tool block is the lines the pane draws"
        );
    }

    #[test]
    fn drag_copy_release_clears_only_a_selection_it_copied() {
        let drag = CopyDrag::default();
        drag.texts.borrow_mut().push("drag to copy".into());

        // An unmoved click: anchor and end sit on the same point, so there is
        // no range to copy and the release stays silent (#2587).
        drag.anchor.set(Some(TextPoint::new(0, 4)));
        drag.end.set(Some(TextPoint::new(0, 4)));
        assert!(!drag.has_selection(), "a click selects no range");
        assert_eq!(drag.clipboard_item(), None, "so it has nothing to copy");

        // A drag over real characters copies, and the release clears the
        // highlight the moment that text lands.
        drag.end.set(Some(TextPoint::new(0, 8)));
        assert!(drag.has_selection());
        assert!(drag.clipboard_item().is_some());
        drag.clear_selection();
        assert!(!drag.has_selection(), "the copied range stops painting");
        assert_eq!(drag.range(), None, "so no row stays highlighted");
        assert_eq!(
            drag.clipboard_item(),
            None,
            "and there is nothing left to copy twice"
        );
    }

    #[test]
    fn drag_copy_announces_through_one_listener_and_ends_outside_too() {
        const SRC: &str = include_str!("transcript.rs");
        // One release path, wherever the gesture ended: it copies, then tells
        // the composer, so a landed copy is acknowledged exactly once.
        let (_, release) = SRC
            .split_once(
                [
                    "pub fn release(&self, window: &mut Window",
                    ", app: &mut App) {",
                ]
                .concat()
                .as_str(),
            )
            .expect("CopyDrag owns the release path");
        let (body, _) = release
            .split_once("\n    }\n")
            .expect("the release path ends");
        let copied = body
            .find(["finish", "_copy(app)"].concat().as_str())
            .expect("the release copies");
        let announced = body
            .find(["announce", "_copied(app)"].concat().as_str())
            .expect("the release announces");
        assert!(
            copied < announced,
            "a copy has to land before the composer confirms it"
        );
        assert_eq!(
            SRC.matches(
                ["let copied = self.", "finish_copy(app);"]
                    .concat()
                    .as_str()
            )
            .count(),
            1,
            "the clipboard is written from one place, not once per caller"
        );
        assert!(
            SRC.contains(["on_mouse_up_", "out(MouseButton::Left"].concat().as_str()),
            "a release past the pane's edge still ends the drag"
        );
        assert!(
            !SRC.contains(["d.", "write_to_clipboard(app);"].concat().as_str()),
            "no caller writes the clipboard without clearing the selection"
        );
    }
    fn forwarded_row(content: &str, sequence: i32, batch: uuid::Uuid, label: &str) -> Message {
        let mut m = row("user", content, sequence);
        m.id = uuid::Uuid::new_v4();
        m.session_id = uuid::Uuid::new_v4();
        m.forward = Some(Box::new(opencrabs::db::models::ForwardProvenance {
            from_session: uuid::Uuid::new_v4(),
            from_message: uuid::Uuid::new_v4(),
            label: label.to_string(),
            batch,
        }));
        m
    }

    #[test]
    fn one_forward_batch_is_one_turn_with_one_header() {
        let batch = uuid::Uuid::new_v4();
        let rows = vec![
            forwarded_row("picked one", 0, batch, "Release planning"),
            forwarded_row("picked two", 1, batch, "Release planning"),
            forwarded_row("picked three", 2, batch, "Release planning"),
        ];
        let (_, turns) = turns(&rows, 3);
        assert_eq!(turns.len(), 1, "a batch must not open one turn per row");
        let marks: Vec<&ForwardMark> = turns[0]
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Msg { forward, .. } => forward.as_ref(),
                _ => None,
            })
            .collect();
        assert_eq!(marks.len(), 3);
        assert_eq!(
            marks.iter().filter(|m| m.first).count(),
            1,
            "exactly one row of the batch carries the header"
        );
        assert!(marks[0].first, "the header sits on the batch's first row");
    }

    #[test]
    fn two_separate_forwards_are_two_batches() {
        let first = uuid::Uuid::new_v4();
        let second = uuid::Uuid::new_v4();
        let rows = vec![
            forwarded_row("a", 0, first, "Session A"),
            forwarded_row("b", 1, first, "Session A"),
            forwarded_row("c", 2, second, "Session B"),
        ];
        let (_, turns) = turns(&rows, 3);
        assert_eq!(turns.len(), 2);
        let headers: Vec<String> = turns
            .iter()
            .flat_map(|t| t.blocks.iter())
            .filter_map(|b| match b {
                Block::Msg {
                    forward: Some(mark),
                    ..
                } if mark.first => Some(forward_header(&mark.label)),
                _ => None,
            })
            .collect();
        assert_eq!(
            headers,
            vec!["Forwarded from Session A", "Forwarded from Session B"]
        );
    }

    #[test]
    fn a_forwarded_row_is_stored_as_a_user_row_so_the_turn_never_reads_it_as_its_own() {
        let batch = uuid::Uuid::new_v4();
        let rows = vec![forwarded_row(
            "answer copied from elsewhere",
            0,
            batch,
            "Other",
        )];
        let (_, turns) = turns(&rows, 1);
        let block = &turns[0].blocks[0];
        assert!(
            matches!(block, Block::Msg { role, .. } if role == "user"),
            "a forwarded copy is user content, not assistant output"
        );
    }

    #[test]
    fn the_forward_header_names_the_source_and_falls_back_when_unnamed() {
        assert_eq!(
            forward_header("Release planning"),
            "Forwarded from Release planning"
        );
        assert_eq!(forward_header("   "), "Forwarded message");
    }

    #[test]
    fn drag_copy_row_release_ends_the_drag_where_it_started() {
        const SRC: &str = include_str!("transcript.rs");
        // Match the name alone, not the opening paren: the row builder is
        // generic over its child, so `fn selectable_row(` never appears as a
        // definition and the first match would be this very string literal.
        let (_, after) = SRC
            .split_once("fn selectable_row")
            .expect("the drag rows exist");
        let (row, _) = after.split_once("\n}\n").expect("the row builder ends");
        assert!(
            row.contains(["on_mouse_up(MouseButton::", "Left"].concat().as_str()),
            "the row that starts a drag has to end it too: gpui runs a bubble \
             mouse-up handler only on a hitbox that is hovered, and the pane \
             behind the row is not guaranteed to be that hitbox"
        );
        assert!(
            row.contains(["release.", "release(window, app)"].concat().as_str()),
            "the row release goes through the one copy path"
        );
    }

    /// The duplicate the reader sees mid-turn: a reload installs the rows for
    /// the turn the card is still drawing, so the screen holds two copies of
    /// the question, the header and the answer. Identity is the question plus
    /// its second, never the row's position.
    #[test]
    fn the_running_turn_is_recognised_in_the_rows_it_reloads() {
        let mut live = LiveTurn::default();
        live.begin("audit the repo config".into());
        let stamped = live_time(live.sent_at);
        let turn_with = |question: &str, time: String| Turn {
            header: None,
            summary: TurnSummary::default(),
            metrics: None,
            preview: None,
            anchor: format!("{question}@{time}"),
            blocks: vec![Block::Msg {
                role: "user".into(),
                content: question.into(),
                time,
                placeholder: false,
                message_id: None,
                forward: None,
            }],
            final_idx: None,
            origin: None,
        };
        let turn = turn_with("audit the repo config", stamped.clone());
        assert!(
            newest_live_turn_matches(&turn, &live),
            "the newest row is the question the card is drawing"
        );

        // The write can land a second after the send stamped the card.
        let later = live
            .sent_at
            .and_then(|at| at.checked_add_signed(chrono::TimeDelta::seconds(1)))
            .map(|at| live_time(Some(at)))
            .unwrap_or_default();
        assert!(
            newest_live_turn_matches(&turn_with("audit the repo config", later), &live),
            "a row stamped a second later is still the same exchange"
        );

        // A different question is a different turn, however close in time.
        assert!(
            !newest_live_turn_matches(&turn_with("something else", stamped), &live),
            "a different question must not hide its answer"
        );

        // A row read back once identifies the exchange for good. A write that
        // landed minutes late still belongs to this card, and matching by
        // seconds alone would put the persisted turn and the card on screen
        // together, which is the duplicate this fixes.
        let mut known = LiveTurn::default();
        known.begin("audit the repo config".into());
        known.landed = true;
        let drifted = known
            .sent_at
            .and_then(|at| at.checked_add_signed(chrono::TimeDelta::minutes(3)))
            .map(|at| live_time(Some(at)))
            .unwrap_or_default();
        assert!(
            newest_live_turn_matches(&turn_with("audit the repo config", drifted.clone()), &known),
            "a question already read back is this exchange however late it was stamped"
        );
        assert!(
            !newest_live_turn_matches(&turn_with("something else", drifted), &known),
            "the text still decides, so a late stamp cannot hide another turn"
        );
    }

    /// A turn resumed after a restart is drawn from a row the previous process
    /// already wrote, so the card has to claim that row from its first frame.
    /// The row keeps the original send time while the card stamps the resume
    /// time, which can be minutes later, and matching on seconds alone painted
    /// the question twice: once from the row, once from the echo (#2740).
    #[test]
    fn a_resumed_turn_hides_the_question_it_already_owns() {
        let mut live = LiveTurn::default();
        live.resume("audit the repo config".into());
        let resumed_at = live.sent_at.expect("a resumed turn is stamped");
        let original = resumed_at
            .checked_sub_signed(chrono::TimeDelta::minutes(2))
            .map(|at| live_time(Some(at)))
            .unwrap_or_default();
        let turn = Turn {
            header: None,
            summary: TurnSummary::default(),
            metrics: None,
            preview: None,
            anchor: format!("audit the repo config@{original}"),
            blocks: vec![Block::Msg {
                role: "user".into(),
                content: "audit the repo config".into(),
                time: original,
                placeholder: false,
                message_id: None,
                forward: None,
            }],
            final_idx: None,
            origin: None,
        };
        let newest_is_live = newest_live_turn_matches(&turn, &live);
        assert!(
            newest_is_live,
            "the resumed card draws the row the crashed process already wrote"
        );
        assert!(
            persisted_turn_is_hidden(0, 0, newest_is_live),
            "the row stays out while the resumed card draws the same question"
        );
    }

    /// Resuming is not a fresh send: the row exists, so the card says so once
    /// and stops asking whether the newest persisted turn is its own.
    #[test]
    fn resume_marks_the_turn_landed_and_keeps_the_question() {
        let mut live = LiveTurn::default();
        assert!(!live.landed, "nothing is in flight yet");
        live.resume("audit the repo config".into());
        assert_eq!(live.user.as_deref(), Some("audit the repo config"));
        assert!(live.landed, "the ledger already holds the resumed question");
        assert!(
            live.sent_at.is_some(),
            "the card still shows when the turn resumed"
        );
        // A fresh send must keep asking: its row does not exist yet.
        live.begin("audit the repo config".into());
        assert!(!live.landed, "begin is a new send, not a resume");
    }

    /// A finished answer keeps its place in the conversation while the next
    /// request runs. The card owns one exchange at a time, so only the turn it
    /// is drawing may be hidden; the previous turn prints from its rows even
    /// though the new turn holds the busy flag for its whole duration (#2709).
    #[test]
    fn the_previous_answer_is_drawn_while_the_next_turn_runs() {
        // Four turns on screen, the fourth being the one the card draws.
        let last = 3;
        assert!(
            !persisted_turn_is_hidden(2, last, true),
            "the answer just finished must not vanish when a new request starts"
        );
        assert!(
            !persisted_turn_is_hidden(0, last, true),
            "older turns are never hidden by the live card"
        );
        assert!(
            persisted_turn_is_hidden(last, last, true),
            "the exchange the card is drawing stays out of the rows"
        );
        assert!(
            !persisted_turn_is_hidden(last, last, false),
            "with no card on screen the newest turn prints from its rows"
        );
    }

    /// Narration is the card's own summary, never a chat row of its own: the
    /// collapsed card previews the newest paragraph inside its frame, and the
    /// body prints every paragraph in order once the card is opened.
    #[test]
    fn narration_previews_inside_the_collapsed_card() {
        let mut live = LiveTurn::default();
        live.begin("go".into());
        live.push_tool(ToolCall::running("bash".into(), "bash: ls".into()));
        live.push_narration("I'll audit the repo config first.");
        live.push_narration("Then I'll check the runner setup.");
        let said: Vec<&str> = live.narration().collect();
        assert_eq!(
            said,
            vec![
                "I'll audit the repo config first.",
                "Then I'll check the runner setup."
            ],
            "narration keeps the order it was spoken in"
        );
        assert_eq!(
            collapsed_narration(&live).as_deref(),
            Some("Then I'll check the runner setup."),
            "the collapsed card previews the newest paragraph only"
        );
        assert_eq!(
            live.answer(),
            None,
            "narration is not the answer: the bubble below the card stays empty"
        );

        let mut quiet = LiveTurn::default();
        quiet.begin("go".into());
        quiet.push_tool(ToolCall::running("bash".into(), "bash: ls".into()));
        assert_eq!(
            collapsed_narration(&quiet),
            None,
            "a card that has said nothing previews nothing"
        );
    }

    /// The preview clips on a character boundary, so a long paragraph in a
    /// non-ASCII language never splits a glyph.
    #[test]
    fn a_collapsed_preview_clips_on_a_character_boundary() {
        let mut live = LiveTurn::default();
        live.begin("go".into());
        live.push_narration(&"é".repeat(COLLAPSED_NARRATION_CHARS + 40));
        let preview = collapsed_narration(&live).expect("a preview");
        assert_eq!(
            preview.chars().count(),
            COLLAPSED_NARRATION_CHARS + 1,
            "the budget plus the ellipsis"
        );
        assert!(preview.ends_with('\u{2026}'), "a clipped preview says so");
    }

    /// The preview renders under `.line_clamp(3)`. gpui shapes each
    /// newline-separated line against the budget the earlier lines left, and
    /// subtracts with overflow once a line wraps past the clamp, aborting the
    /// window (#2731). The preview must stay a single source line so that
    /// subtraction can never run.
    #[test]
    fn a_collapsed_preview_is_a_single_source_line() {
        let mut live = LiveTurn::default();
        live.begin("go".into());
        let first = "word ".repeat(40);
        live.push_narration(&format!("{first}\nand a second paragraph"));
        let preview = collapsed_narration(&live).expect("a preview");
        assert!(
            !preview.contains('\n') && !preview.contains('\r'),
            "a clamped preview carries no line break gpui could underflow on"
        );
        assert!(
            !preview.contains("  "),
            "whitespace runs collapse to a single space"
        );
    }

    /// One drawn text segment of an inline-crab line: the offset the row's copy
    /// string gives it. This is the transcript's own `begin_segment` rule,
    /// written out so the arithmetic below can be replayed without a window.
    fn segment_start(offset: &mut usize, segments: &mut usize, accounted: usize) -> usize {
        let start = if *segments == 0 { 0 } else { *offset + 1 };
        *offset = start + accounted;
        *segments += 1;
        start
    }

    /// A flex row breaks between children, never inside one, so a long run of
    /// words is cut at its spaces (#2771). Each chunk ends after its space, and
    /// the chunk lengths are what the selection accounting consumes.
    #[test]
    fn an_inline_line_wraps_at_its_spaces() {
        let chunks = inline_chunks("one two three", 0..13, 13);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].0, 0..4);
        assert_eq!(chunks[1].0, 4..8);
        assert_eq!(chunks[2].0, 8..13);
        // The drawn bytes of the run are exactly the chunks, in order.
        assert_eq!(
            chunks.iter().map(|(range, _)| range.len()).sum::<usize>(),
            13
        );
        // A single word is one chunk that claims the whole run.
        assert_eq!(inline_chunks("hi ", 0..3, 3), vec![(0..3, 3)]);
    }

    /// The reason `inline_chunks` carries lengths at all: replay what
    /// `inline_crabs` declares to `SelectionContext` and every text chunk must
    /// start at the offset the whole, unsplit line puts it at. A picture takes
    /// no segment, and its bytes ride on the run ahead of it. Drift here moves
    /// drag-select and copy by one character per split.
    #[test]
    fn split_chunks_keep_the_offsets_of_the_whole_line() {
        for line in [
            "hi :cry: there",
            "one two :ok: three four five",
            ":cry: a long line of words still wraps",
            "a :ok: b",
            ":cry:",
            "plain words only",
        ] {
            let Some(pieces) = topic_icons::inline_pieces(line) else {
                continue;
            };
            let mut offset = 0;
            let mut segments = 0;
            for piece in pieces {
                if piece.crab.is_some() {
                    continue;
                }
                if piece.range.is_empty() {
                    segment_start(&mut offset, &mut segments, piece.accounted);
                    continue;
                }
                let chunks = inline_chunks(line, piece.range.clone(), piece.accounted);
                assert!(!chunks.is_empty(), "{line}: a run draws nothing");
                for (range, accounted) in chunks {
                    let start = segment_start(&mut offset, &mut segments, accounted);
                    assert_eq!(
                        start, range.start,
                        "{line}: a chunk starts at {start}, not {}",
                        range.start
                    );
                }
            }
            assert_eq!(offset, line.len(), "{line}: the row ends short");
        }
    }

    /// A bold span crossing a crab stays bold on both sides of it: each chunk
    /// keeps the part of the span it covers, moved onto its own bytes (#2771).
    #[test]
    fn a_highlight_follows_the_chunk_it_lands_in() {
        let bold = HighlightStyle::default();
        let highlights = vec![(0..9, bold)];
        assert!(local_highlights(&highlights, &(9..12)).is_empty());
        assert_eq!(local_highlights(&highlights, &(4..9)), vec![(0..5, bold)]);
        assert_eq!(local_highlights(&highlights, &(2..6)), vec![(0..4, bold)]);
    }

    /// Cutting a run at its spaces must keep every chunk on a char boundary: a
    /// chunk range becomes a slice of the line at paint time, and accented text
    /// or an emoji sitting next to a crab would panic there instead of failing
    /// here (#2771, #2298).
    #[test]
    fn multi_byte_runs_cut_on_char_boundaries() {
        for line in [
            "ação pedaço 🦀 fim",
            "日本語 と :cry: と 漢字",
            "olá :cry: mundo",
        ] {
            for chunks in [
                inline_chunks(line, 0..line.len(), line.len()),
                topic_icons::inline_pieces(line)
                    .map(|pieces| {
                        pieces
                            .iter()
                            .filter(|piece| piece.crab.is_none() && !piece.range.is_empty())
                            .flat_map(|piece| {
                                inline_chunks(line, piece.range.clone(), piece.accounted)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            ] {
                for (range, _) in chunks {
                    assert!(
                        line.is_char_boundary(range.start),
                        "{line}: a chunk starts mid-character at {}",
                        range.start
                    );
                    assert!(
                        line.is_char_boundary(range.end),
                        "{line}: a chunk ends mid-character at {}",
                        range.end
                    );
                    line.get(range.clone())
                        .unwrap_or_else(|| panic!("{line}: chunk range is not a slice"));
                }
            }
        }
    }
}
