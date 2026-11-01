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

use std::collections::{HashMap, HashSet};

use gpui::{
    AbsoluteLength, AnyElement, App, ClickEvent, Div, ObjectFit, ScrollHandle, SharedString,
    Stateful, Styled, StyledText, TextStyle, Window, div, img, prelude::*, px, rems, rgb,
};

use opencrabs::db::Database;
use opencrabs::db::models::Message;
use opencrabs::db::repository::message::MessageRepository;
use opencrabs::tui::app::ledger_scan::next_ledger;
use opencrabs::tui::app::reasoning_split::{self, Segment, is_intermediate, split_segments};
// The fold header's wording is the TUI's own (#759), re-exported from the lib
// rather than re-spelled here: two dialects of the same line would drift the
// moment either client changed one.
use opencrabs::tui::render::{
    TurnSummary, format_turn_header, format_turn_spinner_meta, is_deliverable_report,
};

use crate::editor::{Attachment, marker_paths, strip_markers};
use crate::markdown;
use crate::theme::{
    ACCENT, AMBER, BG, BLUE, GREEN, PANEL, PANEL_HI, RED, TEAL, TEXT, TEXT_MUTED, TEXT_NORMAL,
    TEXT_STRONG, TEXT_TITLE, TRACK, WARN,
};

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
pub type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A rendered piece of the conversation. The TUI's `DisplayMessage` kinds,
/// narrowed to what the gui pane draws.
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
    },
    /// Model thinking or intermediate narration, dim and collapsed.
    Thinking { content: String },
    /// A tool ledger: calls with their status.
    Tools { calls: Vec<ToolCall> },
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
    /// Provider-reported output rate; `None` when it could not be measured.
    pub tps: Option<f64>,
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
    /// that did no work (a plain question and answer earns no header).
    pub header: Option<String>,
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
    /// Errors and queue notices: amber, never folded.
    pub notices: Vec<String>,
    /// The final answer once the turn reports `Done`. Kept on screen until
    /// the reload proves the row is persisted, so a turn that streamed
    /// nothing still shows its answer instead of blanking the pane.
    pub settled: Option<String>,
    /// Output tokens produced this turn, counted with the same tokenizer the
    /// TUI runs over every chunk (`brain::tokenizer`), so the live counter and
    /// the TUI's agree instead of being two estimates.
    pub tokens: u32,
    /// Seconds the provider spent ACTUALLY streaming this turn (#2325), the
    /// denominator every other surface divides by. Tool execution, approval
    /// waits and between-response round-trips are excluded: a 30s tool run
    /// used to turn a 200 tok/s burst into "8 tok/s".
    active_secs: f64,
    /// When the currently open streaming window started.
    window_start: Option<std::time::Instant>,
    /// When the newest chunk landed, which is where the open window ends.
    last_token_at: Option<std::time::Instant>,
    /// The last finalized rate, kept across idle so the footer shows the
    /// previous turn's number until the next turn produces its first token,
    /// exactly like the terminal's streaming tracker.
    last_tps: Option<f64>,
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
            && self.notices.is_empty()
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

    /// The DB has the turn: drop the live copy.
    pub fn clear(&mut self) {
        self.user = None;
        self.thinking = false;
        self.reasoning.clear();
        self.stream.clear();
        self.tools.clear();
        self.notices.clear();
        self.settled = None;
        self.tokens = 0;
        self.started = None;
        self.chunks = 0;
        self.chunk_bytes = 0;
        self.first_chunk = None;
        self.sent_at = None;
        self.answer_at = None;
        // The accumulator belongs to the turn that just ended; the finalized
        // rate deliberately survives, so an idle footer keeps the number
        // instead of blanking until the next turn streams.
        self.active_secs = 0.0;
        self.window_start = None;
        self.last_token_at = None;
    }

    /// A gap longer than this between chunks closes the active window: tool
    /// runs, approval prompts and between-response round-trips all exceed it,
    /// so they are excluded from the rate's denominator.
    const IDLE_GAP_SECS: f64 = 1.0;

    /// Feed the active-window accumulator one token event (#2325), the same
    /// rule `tui::app::state::StreamingTpsTracker::advance` applies. The clock
    /// is a parameter so a seat can drive it deterministically.
    fn advance_streaming(&mut self, now: std::time::Instant) {
        match (self.window_start, self.last_token_at) {
            (None, _) => self.window_start = Some(now),
            (Some(start), Some(last)) => {
                if (now - last).as_secs_f64() > Self::IDLE_GAP_SECS {
                    // Idle gap: close the old window, open a new one.
                    self.active_secs += (last - start).as_secs_f64();
                    self.window_start = Some(now);
                }
            }
            (Some(_), None) => self.window_start = Some(now),
        }
        self.last_token_at = Some(now);
    }

    /// Active streaming seconds so far, including the open window. Measured
    /// to the LAST chunk, never to now, so idle ticks cannot inflate it.
    pub fn streaming_active_secs(&self) -> f64 {
        match (self.window_start, self.last_token_at) {
            (Some(start), Some(last)) => self.active_secs + (last - start).as_secs_f64().max(0.0),
            _ => self.active_secs,
        }
    }

    /// Finish the turn's rate (#2325): prefer the service's own
    /// `tokens_per_second` (provider-reported tokens over summed active
    /// streaming time, the number the Telegram card prints), else the local
    /// active-window estimate, and reset the accumulator. A turn that produced
    /// nothing keeps the previous number rather than reporting a fake zero.
    pub fn finalize(&mut self, authoritative: Option<f64>) {
        let active = self.streaming_active_secs();
        if let Some(tps) = authoritative.filter(|t| t.is_finite() && *t > 0.0) {
            self.last_tps = Some(tps);
        } else if self.tokens > 0 && active > 0.0 {
            self.last_tps = Some(self.tokens as f64 / active);
        }
        self.active_secs = 0.0;
        self.window_start = None;
        self.last_token_at = None;
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
        let active = self.streaming_active_secs();
        let live = (active > 0.0).then(|| self.tokens as f64 / active);
        live.or(self.last_tps).map_or(0.0, |t| t as f32)
    }

    /// Count one answer chunk: text plus its tokens. Counting here keeps the
    /// live counter honest for every caller (a chunk that never renders still
    /// cost the tokens).
    pub fn note_chunk(&mut self, text: &str) {
        self.note_chunk_at(text, std::time::Instant::now());
    }

    /// [`note_chunk`](Self::note_chunk) with the clock injected, so tests can
    /// rehearse a stream with a tool wait in the middle.
    fn note_chunk_at(&mut self, text: &str, now: std::time::Instant) {
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
            self.advance_streaming(now);
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
        let tail = self.stream.trim_end();
        if tail.ends_with(text) {
            return;
        }
        if !tail.is_empty() {
            self.stream.push_str("\n\n");
        }
        self.note_chunk(text);
    }

    /// The same guard for reasoning, which travels as `ReasoningChunk` and
    /// again inside `IntermediateText`.
    pub fn push_reasoning(&mut self, text: &str) {
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
        self.note_reasoning(text);
    }

    /// Count one reasoning chunk. Reasoning IS model output and the TUI counts
    /// it toward the turn's total (2026-05-29 receipt: a 38s reasoning turn
    /// read "65 tok"), so excluding it here would understate the turn.
    pub fn note_reasoning(&mut self, text: &str) {
        self.note_reasoning_at(text, std::time::Instant::now());
    }

    /// [`note_reasoning`](Self::note_reasoning) with the clock injected.
    ///
    /// Reasoning advances the streaming window the same way the answer does:
    /// the channel bridge feeds `StreamingOutputTokens` for reasoning chunks
    /// too (`cli/ui.rs`, "reasoning chunks ARE model output"), so the terminal
    /// tracker counts that time, and counting the tokens here without the
    /// time would inflate the rate (#2325).
    fn note_reasoning_at(&mut self, text: &str, now: std::time::Instant) {
        let tokens = opencrabs::brain::tokenizer::count_tokens(text) as u32;
        self.tokens = self.tokens.saturating_add(tokens);
        if self.first_chunk.is_none() {
            self.first_chunk = self.started.map(|t| t.elapsed());
        }
        self.thinking = false;
        if tokens > 0 {
            self.advance_streaming(now);
        }
        self.reasoning.push_str(text);
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
    /// Set when the fetch failed; the pane says why instead of lying empty.
    pub error: Option<SharedString>,
}

impl TranscriptState {
    /// Newest turn's anchor: the one ctrl-o acts on, as in the TUI where
    /// ctrl+o toggles the newest turn only.
    pub fn newest_anchor(&self) -> Option<&str> {
        self.turns.last().map(|t| t.anchor.as_str())
    }

    /// Every anchor on screen, so the caller can mint one click listener each.
    pub fn anchors(&self) -> Vec<String> {
        self.turns.iter().map(|t| t.anchor.clone()).collect()
    }
}

/// Fetch the newest window of the conversation plus the session's total row
/// count, on a thread with a real stack (crash receipt in `main.rs`).
/// `Database::connect` runs pragmas only, never migrations.
pub async fn load_recent(
    db_path: std::path::PathBuf,
    session_id: uuid::Uuid,
) -> Result<(Vec<Message>, i64), String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<(Vec<Message>, i64), String>>();
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
                        let rows: Vec<Message> =
                            repo.find_recent_by_session(session_id, WINDOW).await?;
                        let total: i64 = repo.count_by_session(session_id).await?;
                        Ok::<_, anyhow::Error>((rows, total))
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

/// Expand ONE stored row into its renderable blocks. Mirrors
/// `TuiApp::expand_message` (`tui/app/messaging.rs`): compaction markers are
/// context and never shown; tool ledgers become `Tools` blocks; reasoning
/// segments and intermediate narration become `Thinking`; plain rows stay
/// `Msg`. The `before[..ledger.start]` slices are byte-indexed but the scanner
/// only reports marker offsets it verified sit on line starts, so every offset
/// it returns is a char boundary (#1587 grammar).
fn row_blocks(m: &Message) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    if m.content.starts_with("[CONTEXT COMPACTION") {
        return blocks;
    }
    if m.role != "assistant" {
        blocks.push(Block::Msg {
            role: m.role.clone(),
            content: m.content.clone(),
            time: stamp(m),
            placeholder: false,
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
pub fn turns_with_metrics(
    rows: &[Message],
    total: i64,
    metrics: &HashMap<String, TurnMetrics>,
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
        if m.role == "user" && i > start {
            bounds.push((start, i));
            start = i;
        }
    }
    bounds.push((start, rows.len()));

    let mut out = Vec::with_capacity(bounds.len());
    for (s, e) in bounds {
        let mut blocks: Vec<Block> = Vec::new();
        for m in &rows[s..e] {
            blocks.extend(row_blocks(m));
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
        let anchor = format!("{}@{}", rows[s].role, rows[s].created_at.timestamp());
        let metrics = metrics.get(&anchor).copied();
        out.push(Turn {
            header,
            metrics,
            preview,
            anchor,
            blocks,
            final_idx,
        });
    }
    (hidden, out)
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
        Block::Marker { .. } => true,
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
        "user" => BLUE,
        "assistant" => GREEN,
        "system" | "tool" => WARN,
        _ => TEXT_MUTED,
    }
}

fn body_style(size: f32) -> TextStyle {
    TextStyle {
        color: rgb(TEXT).into(),
        font_size: AbsoluteLength::Pixels(px(size)),
        ..Default::default()
    }
}

fn mono_style() -> TextStyle {
    TextStyle {
        font_family: "Menlo".into(),
        color: rgb(TEXT_NORMAL).into(),
        font_size: AbsoluteLength::Pixels(px(12.0)),
        ..Default::default()
    }
}

/// One styled markdown line; indent in list/quote depth steps.
fn md_line(l: &markdown::MdLine) -> Div {
    let size = if l.heading { 15.0 } else { 13.0 };
    let style = body_style(size);
    let text =
        StyledText::new(l.text.clone()).with_default_highlights(&style, l.highlights.clone());
    div().pl(rems(f32::from(l.indent) * 0.9)).child(text)
}

/// One table cell. Every cell is `flex_1` with `min_w_0`, so the grid takes
/// the width the window actually has and the columns share it: a wide window
/// gets wide columns, a narrow one wraps the text instead of clipping it.
fn table_cell(cell: &markdown::MdCell, header: bool) -> Div {
    let style = TextStyle {
        color: rgb(if header { TEXT_STRONG } else { TEXT_NORMAL }).into(),
        font_size: AbsoluteLength::Pixels(px(13.0)),
        ..Default::default()
    };
    let text =
        StyledText::new(cell.text.clone()).with_default_highlights(&style, cell.highlights.clone());
    div().flex_1().min_w_0().px_2().py_1().child(text)
}

/// A rule between two rows, drawn as its own element so no table depends on
/// border-side helpers the way the TUI's box drawing does not either.
fn table_rule() -> Div {
    div().h(px(1.0)).w_full().flex_none().bg(rgb(TRACK))
}

/// A markdown table as a real grid: header band, a rule under it, then one
/// flex row per record with the same column count. This is the shape the TUI
/// draws in box characters; the GUI draws it in panels.
fn md_table(t: &markdown::MdTable) -> Div {
    let cols = t.columns().max(1);
    let empty = markdown::MdCell {
        text: String::new(),
        highlights: Vec::new(),
    };
    let mut grid = div()
        .flex()
        .flex_col()
        .w_full()
        .border_1()
        .border_color(rgb(TRACK))
        .rounded_sm()
        .overflow_hidden();

    let mut head = div()
        .flex()
        .flex_row()
        .items_start()
        .w_full()
        .bg(rgb(PANEL_HI));
    for i in 0..cols {
        head = head.child(table_cell(t.header.get(i).unwrap_or(&empty), true));
    }
    grid = grid.child(head).child(table_rule());

    for (n, row) in t.rows.iter().enumerate() {
        if n > 0 {
            grid = grid.child(table_rule());
        }
        let mut line = div().flex().flex_row().items_start().w_full();
        for i in 0..cols {
            line = line.child(table_cell(row.get(i).unwrap_or(&empty), false));
        }
        grid = grid.child(line);
    }
    grid
}

/// Assistant content as markdown. Consecutive fence lines group under one
/// padded mono container so a code block reads as one shape, while a table
/// block keeps its grid instead of being flattened into text rows.
fn md_block(src: &str) -> Div {
    let blocks = markdown::blocks(src);
    let mut out = div().flex().flex_col().gap_1();
    let mut i = 0;
    while i < blocks.len() {
        match &blocks[i] {
            markdown::MdBlock::Table(table) => {
                out = out.child(md_table(table));
                i += 1;
            }
            markdown::MdBlock::Line(line) if line.mono => {
                let mut code = div()
                    .flex()
                    .flex_col()
                    .bg(rgb(PANEL_HI))
                    .rounded_sm()
                    .p_2()
                    .gap_0p5();
                while i < blocks.len() {
                    match &blocks[i] {
                        markdown::MdBlock::Line(l) if l.mono => {
                            let style = mono_style();
                            code = code.child(
                                div().child(
                                    StyledText::new(l.text.clone())
                                        .with_default_highlights(&style, l.highlights.clone()),
                                ),
                            );
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out = out.child(code);
            }
            markdown::MdBlock::Line(line) => {
                out = out.child(md_line(line));
                i += 1;
            }
        }
    }
    out
}

/// User/system content stays plain: the TUI renders input verbatim, markdown
/// styling is an answer-text treatment.
fn plain_block(content: &str) -> Div {
    let mut lines = div().flex().flex_col().gap_0p5();
    for line in content.split('\n') {
        lines = lines.child(
            div()
                .text_sm()
                .text_color(rgb(TEXT))
                .child(SharedString::from(line.to_string())),
        );
    }
    lines
}

/// One message: its timestamp, then the content. No role label and no panel
/// background, because both were noise: the TUI tells the two apart by colour
/// on the marker, and a grey card around every message made the transcript
/// read as a stack of boxes instead of a conversation. The timestamp stays on
/// BOTH sides, which is what the user needed to tell when an answer landed.
fn bubble(role: &str, content: &str, time: &str) -> Div {
    let paths = marker_paths(content);
    let text = strip_markers(content);
    let mut col = div().flex().flex_col().gap_1();
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
            md_block(&text)
        } else {
            plain_block(&text)
        });
    }
    if !paths.is_empty() {
        col = col.child(history_attachments(&paths));
    }
    col
}

/// The files a stored message carries, drawn as the files themselves: images
/// show their real pixels through `img`, videos and documents show their kind
/// plus the name. Filled from the `<<IMG:...>>` / `<<VID:...>>` markers the
/// send path writes into the message, which is why a reloaded session shows
/// exactly what was sent instead of a text stand-in for it.
fn history_attachments(paths: &[std::path::PathBuf]) -> Div {
    const TILE: f32 = 120.0;
    let mut row = div().flex().flex_row().flex_wrap().gap_2().pt_1();
    for path in paths {
        let attachment = Attachment::for_display(path.clone());
        let tile: AnyElement = if attachment.is_image() {
            img(path.clone())
                .w(px(TILE))
                .h(px(TILE))
                .rounded(px(6.0))
                .object_fit(ObjectFit::Cover)
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(TILE))
                .h(px(TILE))
                .rounded(px(6.0))
                .bg(rgb(PANEL_HI))
                .child(if attachment.is_video { "🎬" } else { "📄" })
                .into_any_element()
        };
        row = row.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .w(px(TILE))
                .child(tile)
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(TEXT_MUTED))
                        .child(SharedString::from(attachment.name)),
                ),
        );
    }
    row
}

/// A thinking block: dim, italic, collapsed to a bounded preview so a long
/// reasoning run cannot take over the pane.
fn thinking_block(content: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .p_2()
        .rounded_sm()
        .border_1()
        .border_color(rgb(PANEL_HI))
        .child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_MUTED))
                .child("· thinking ·"),
        )
        .child(
            div()
                .text_xs()
                .italic()
                .text_color(rgb(TEXT_MUTED))
                .child(SharedString::from(fit(content.trim(), 600))),
        )
}

/// A tool ledger: count line plus each call with its status mark.
fn tools_block(calls: &[ToolCall]) -> Div {
    let mut col = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .p_2()
        .rounded_sm()
        .bg(rgb(PANEL_HI))
        .child(
            div()
                .text_xs()
                .text_color(rgb(AMBER))
                .child(SharedString::from(tool_count_label(calls.len()))),
        );
    for call in calls {
        let (mark, color) = match (call.done, call.success) {
            (false, _) => ("\u{b7}\u{b7}\u{b7}", TEXT_MUTED),
            (true, true) => ("\u{2713}", GREEN),
            (true, false) => ("\u{2717}", RED),
        };
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(color))
                .child(SharedString::from(format!(
                    "{mark} {}",
                    fit(call.description.trim(), 160)
                ))),
        );
    }
    col
}

/// One turn's fold header: the TUI's own summary line, the affordance that
/// says how to open it, and, while folded, the last call as a preview so a
/// collapsed turn still says what it did.
fn fold_header(turn: &Turn, folded: bool, newest: bool) -> Option<Stateful<Div>> {
    let base = turn.header.as_ref()?;
    let hint = match (folded, newest) {
        (true, true) => " (click / ctrl-o to expand)",
        (true, false) => " (click to expand)",
        (false, true) => " (click / ctrl-o to fold)",
        (false, false) => " (click to fold)",
    };
    let mut row = div().flex().flex_row().gap_1().child(
        div()
            .text_xs()
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(rgb(TEAL))
            .child(SharedString::from(base.clone())),
    );
    // The card's ctx and rate, in the Telegram footer's own wording. It
    // rides HERE rather than in the shared header string, which the terminal
    // deliberately keeps ctx-free (#744, #2324 D1).
    if let Some(segment) = turn.metrics.as_ref().and_then(|m| m.segment()) {
        row = row.child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_MUTED))
                .child(SharedString::from(segment)),
        );
    }
    let mut col = div()
        .id(SharedString::from(format!("fold-{}", turn.anchor)))
        .flex()
        .flex_col()
        .gap_0p5()
        .px_2()
        .py_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|d| d.bg(rgb(PANEL)))
        .child(
            row.child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_TITLE))
                    .child(SharedString::from(hint)),
            ),
        );
    if folded && let Some((description, success)) = &turn.preview {
        let color = if *success { TEXT_MUTED } else { RED };
        col = col.child(
            div()
                .text_xs()
                .italic()
                .text_color(rgb(color))
                .child(SharedString::from(format!(
                    "\u{2514}\u{2500} {}",
                    fit(description.trim(), 160)
                ))),
        );
    }
    Some(col)
}

/// One block, drawn. The fold decides WHETHER a block renders; this decides
/// only what it looks like.
fn block_view(block: &Block) -> Div {
    match block {
        Block::Msg {
            role,
            content,
            time,
            ..
        } => bubble(role, content, time),
        Block::Thinking { content } => thinking_block(content),
        Block::Tools { calls } => tools_block(calls),
        Block::Marker { hidden } => {
            div()
                .text_xs()
                .text_color(rgb(TEXT_MUTED))
                .child(SharedString::from(format!(
                    "\u{2191} {hidden} older messages hidden",
                )))
        }
    }
}

/// The TUI's spinner frames, in its own order
/// (`tui/render/chat.rs`), so the crab blinks at the same cadence.
const SPINNER_FRAMES: [&str; 10] = [
    "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280f}",
];

/// A live timestamp rendered exactly like a persisted row's, so the bubble
/// does not change shape when the DB reload replaces it.
fn live_time(at: Option<chrono::DateTime<chrono::Local>>) -> String {
    at.map(|t| t.format("%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

/// One live status line: a spinner frame, what the turn is doing right now,
/// and how much it has done. The frame comes off the wall clock rather than a
/// stored frame counter, so a repaint alone advances it and the line is never
/// stale when the window wakes for another reason.
fn status_line(live: &LiveTurn) -> Div {
    let frame = match live.started {
        Some(t) => SPINNER_FRAMES[(t.elapsed().as_millis() / 80) as usize % SPINNER_FRAMES.len()],
        None => SPINNER_FRAMES[0],
    };
    let (lead, rest) = live_status_line(live);
    let mut row = div().flex().flex_row().items_center().gap_1().child(
        div()
            .text_xs()
            .text_color(rgb(TEXT_MUTED))
            .child(SharedString::from(frame.to_string())),
    );
    if !lead.is_empty() {
        row = row.child(
            div()
                .text_xs()
                .text_color(rgb(ACCENT))
                .child(SharedString::from(lead)),
        );
    }
    if !rest.is_empty() {
        row = row.child(
            div()
                .text_xs()
                .text_color(rgb(TEXT_MUTED))
                .child(SharedString::from(rest)),
        );
    }
    row
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
fn live_activity(live: &LiveTurn) -> Option<String> {
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
fn live_status_line(live: &LiveTurn) -> (String, String) {
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

/// The running turn, drawn with the transcript's own block grammar: the
/// question as a user bubble, tool rows with their live status, the streamed
/// answer as markdown, notices in amber. Nothing here folds: the turn has no
/// settled answer yet, and hiding the working-out would blank the pane
/// exactly when the user wants to see that something is happening.
fn live_view(live: &LiveTurn) -> Div {
    let mut col = div().flex().flex_col().gap_2();
    if let Some(user) = &live.user {
        col = col.child(block_view(&Block::Msg {
            role: "user".into(),
            content: user.clone(),
            time: live_time(live.sent_at),
            placeholder: false,
        }));
    }
    for notice in &live.notices {
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(AMBER))
                .child(SharedString::from(notice.clone())),
        );
    }
    if !live.tools.is_empty() {
        col = col.child(block_view(&Block::Tools {
            calls: live.tools.clone(),
        }));
    }
    // ONE status line for the whole turn, exactly the TUI's: a spinner frame,
    // the crab's name, and the same meta the terminal builds with
    // `format_turn_spinner_meta`. It stays up from the send until the answer
    // settles, so a streaming reply still shows the clock and the counter
    // climbing. The two renderers it replaced (a persisted block plus a bare
    // placeholder) are what made the window say "thinking" twice, with neither
    // a clock nor a token count.
    if live.settled.is_none() {
        col = col.child(status_line(live));
    }
    if !live.reasoning.is_empty() {
        col = col.child(
            div()
                .text_xs()
                .italic()
                .text_color(rgb(TEXT_MUTED))
                .child(SharedString::from(fit(live.reasoning.trim(), 600))),
        );
    }
    if let Some(answer) = live.answer() {
        col = col.child(block_view(&Block::Msg {
            role: "assistant".into(),
            content: answer.to_string(),
            time: live_time(live.answer_at.or(live.sent_at)),
            placeholder: false,
        }));
    }
    col
}

/// The transcript pane: scrollable, opening at the bottom via the caller's
/// handle, with the empty and error states rendered honestly. Each turn folds
/// by default, exactly as in the TUI; `busy` keeps the running turn's
/// in-flight text out of the fold so the answer is never hidden while it is
/// still being written.
pub fn view(
    state: &TranscriptState,
    scroll: &ScrollHandle,
    live: &LiveTurn,
    busy: bool,
    mut handlers: HashMap<String, ClickHandler>,
) -> Stateful<Div> {
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
        .bg(rgb(BG));

    if state.session.is_none() {
        col = col.child(
            div()
                .text_sm()
                .text_color(rgb(TEXT_MUTED))
                .child("Select a session to read its transcript"),
        );
        return col;
    }
    if let Some(err) = &state.error {
        col = col.child(
            div()
                .text_sm()
                .text_color(rgb(TEXT_MUTED))
                .child(err.clone()),
        );
        return col;
    }
    if state.turns.is_empty() && live.is_empty() {
        col = col.child(
            div()
                .text_sm()
                .text_color(rgb(TEXT_MUTED))
                .child("No messages in this session"),
        );
        return col;
    }

    if state.hidden > 0 {
        col = col.child(block_view(&Block::Marker {
            hidden: state.hidden,
        }));
    }
    let last = state.turns.len().saturating_sub(1);
    for (ti, turn) in state.turns.iter().enumerate() {
        let newest = ti == last;
        let folded = !state.expanded.contains(&turn.anchor);
        // The header sits above the turn's first non-user row, as in the TUI.
        let header_at = match turn.blocks.first() {
            Some(Block::Msg { role, .. }) if role == "user" => 1,
            _ => 0,
        };
        let mut drew_header = false;
        for (i, block) in turn.blocks.iter().enumerate() {
            if i == header_at
                && let Some(header) = fold_header(turn, folded, newest)
            {
                // The listener is per turn and used exactly here, so it is
                // taken out of the map instead of cloned.
                col = col.child(match handlers.remove(&turn.anchor) {
                    Some(click) => header.on_click(click),
                    None => header,
                });
                drew_header = true;
            }
            // A folded turn hides its working-out; the running turn's answer
            // is not shown twice because the stream owns it until it settles.
            if folded && !visible_when_folded(block, turn.final_idx, i) {
                continue;
            }
            // The live turn draws the running answer, and keeps drawing the
            // settled one until the reload proves the row persisted. While it
            // is on screen the persisted copy of that same answer stays out,
            // so the conversation never shows it twice.
            if (busy || !live.is_empty())
                && newest
                && let Block::Msg { role, .. } = block
                && role == "assistant"
            {
                continue;
            }
            col = col.child(block_view(block));
        }
        if !drew_header && let Some(header) = fold_header(turn, folded, newest) {
            col = col.child(match handlers.remove(&turn.anchor) {
                Some(click) => header.on_click(click),
                None => header,
            });
        }
    }
    if !live.is_empty() {
        col = col.child(live_view(live));
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

    /// The stored-rows split with nothing remembered, which is what most
    /// seats here exercise. Metrics have their own seats.
    fn turns(rows: &[Message], total: i64) -> (usize, Vec<Turn>) {
        turns_with_metrics(rows, total, &HashMap::new())
    }

    fn row(role: &str, content: &str, sequence: i32) -> Message {
        Message::new(
            uuid::Uuid::nil(),
            role.to_string(),
            content.to_string(),
            sequence,
        )
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
    fn intermediate_text_is_not_appended_twice() {
        // The provider sends the same narration as streamed chunks and again
        // as intermediate text. Both paths land in `stream`; the paragraph
        // must appear once.
        let mut live = LiveTurn::default();
        live.begin("question".into());
        live.note_chunk("Checking the parser now.");
        live.push_narration("Checking the parser now.");
        assert_eq!(live.stream, "Checking the parser now.");

        // New narration still joins as its own paragraph.
        live.push_narration("And the writer.");
        assert_eq!(live.stream, "Checking the parser now.\n\nAnd the writer.");

        // Reasoning takes the same guard.
        live.push_reasoning("think about it");
        live.push_reasoning("think about it");
        assert_eq!(live.reasoning, "think about it");
    }

    #[test]
    fn tokens_per_second_needs_a_running_turn() {
        let mut live = LiveTurn::default();
        assert_eq!(live.tokens_per_sec(), 0.0);
        live.begin("question".into());
        live.note_chunk("hello");
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
            },
        );
        let (_, mut turns) = turns_with_metrics(&rows, 2, &known);
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

    /// A provider that reports no rate still gets the honest half: ctx only,
    /// never a fabricated tok/s.
    #[test]
    fn a_turn_without_a_rate_prints_ctx_only() {
        let metrics = TurnMetrics {
            ctx_used: 12_000,
            ctx_max: 200_000,
            tps: None,
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
            description: "Read Cargo.toml".into(),
            success: true,
            done: true,
        });
        live.tools.push(ToolCall {
            name: "bash".into(),
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
        live.note_chunk("hello there, streaming now ");

        let text = live_status_text(&live);
        assert!(!text.contains("thinking"), "got {text}");
        assert!(text.contains("tok this turn"), "got {text}");
    }
}
