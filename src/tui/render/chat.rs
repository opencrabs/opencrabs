//! Chat message rendering
//!
//! Main chat view and thinking indicator.

use super::super::app::dialog_keys::{DialogScope, dialog_keys};
use super::super::app::{App, DisplayMessage};
use super::super::markdown::parse_markdown;
use super::hints;
use super::theme::{self, Role};
use super::tools::{render_approve_menu, render_inline_approval, render_tool_group};
use super::utils::{apply_rtl, wrap_line_with_padding};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph},
};
use unicode_width::UnicodeWidthStr;

/// Render reasoning/thinking text as plain lines, preserving literal newlines.
/// Unlike `parse_markdown`, single `\n` is honoured instead of being collapsed.
///
/// Wrapped continuations use an empty padding so they align flush-left with
/// the paragraph's first line after the caller prepends its own outer indent.
/// Previously the continuation padding was "  " which, combined with the
/// caller's outer "  " prefix, produced an ugly 4-space indent on wrapped
/// continuations while paragraph starts sat at 2 — inconsistent and distracting
/// in long thinking blocks (screenshot 2026-04-19 08:15).
pub(crate) fn reasoning_to_lines(text: &str, max_width: usize) -> Vec<Line<'static>> {
    let mut result = Vec::new();
    for l in text.split('\n') {
        let line = Line::from(Span::raw(l.to_string()));
        for wrapped in wrap_line_with_padding(line, max_width, "") {
            result.push(wrapped);
        }
    }
    result
}

/// Given the anchored message's first line index (`line_top`) in the freshly
/// rendered transcript, the screen row its header should stay on (`anchor_row`),
/// and the maximum from-bottom scroll, return the `(scroll_offset, auto_scroll)`
/// that pins the header in place across an expand/collapse (#728).
///
/// `scroll_offset` is measured from the bottom, so it is derived from the
/// desired top visible line: `max_scroll - (line_top - anchor_row)`. When the
/// anchor resolves to the very bottom, `auto_scroll` is re-armed so subsequent
/// streaming keeps sticking to the bottom.
pub(crate) fn anchored_scroll_offset(
    line_top: usize,
    anchor_row: usize,
    max_scroll: usize,
) -> (usize, bool) {
    let desired_top = line_top.saturating_sub(anchor_row);
    (
        max_scroll.saturating_sub(desired_top),
        desired_top >= max_scroll,
    )
}

/// The screen row a header occupies, or `None` when it sits above the viewport.
///
/// `line_top` is the header's absolute line index and `scroll_top` the absolute
/// index of the first visible line. This used to be a `saturating_sub` straight
/// into a `u16`, so an off-screen header was recorded as row 0: the pin then
/// scrolled the header to the top row and threw the transcript up by
/// `scroll_top - line_top`, sometimes several pages (#2241). A header above the
/// viewport has no row to pin to, so the caller skips the pin and the viewport
/// holds still instead.
///
/// Exact whenever the header is on screen, which is the case the pin was built
/// for (#728, #758).
pub(crate) fn turn_anchor_row(line_top: usize, scroll_top: usize) -> Option<u16> {
    let row = line_top.checked_sub(scroll_top)?;
    Some(row.min(u16::MAX as usize) as u16)
}

/// Whether this frame's extra lines are content that arrived at the bottom,
/// so the from-bottom scroll offset must absorb them to keep the viewport
/// still.
///
/// `expand_pending` is the important one. A click or ctrl+o that opened a
/// block also grows `total_lines`, but that growth is the user's own doing and
/// happens where they are looking, not appended below them. Treating it as new
/// content scrolled the view up by exactly the number of lines the block added,
/// which for a large tool group is several pages. The anchor restore further
/// down overwrites the offset, but ONLY when the anchored row still has lines
/// this frame: a row hidden by the turn fold resolves to nothing, and then the
/// compensation was the last word and the jump stood (#743 follow-up).
pub(crate) fn should_compensate_for_growth(
    auto_scroll: bool,
    scroll_offset: usize,
    prev_lines: usize,
    total_lines: usize,
    expand_pending: bool,
) -> bool {
    !auto_scroll
        && !expand_pending
        && scroll_offset > 0
        && prev_lines > 0
        && total_lines > prev_lines
}

/// Live-turn spinner suffix. Pure so it can be tested directly.
///
/// `turn_tokens` is the whole turn's streamed output (reasoning + completion
/// text across every tool iteration), so it is labeled "this turn" — it is NOT
/// the size of the current thought, and pairing the bare number with
/// "is thinking..." made it read that way (#741). The ctx budget is
/// deliberately NOT shown here: it already lives in the footer under the input,
/// so repeating it on the spinner was redundant (#744).
pub fn format_turn_spinner_meta(elapsed_secs: u64, turn_tokens: u32) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if elapsed_secs > 0 {
        parts.push(format!("{elapsed_secs}s"));
    }
    if turn_tokens > 0 {
        parts.push(format!("{turn_tokens} tok this turn"));
    }
    (!parts.is_empty()).then(|| format!(" ({})", parts.join(" · ")))
}

/// A run of consecutive tool-call groups found by [`scan_tool_stack`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolStack {
    /// How many `tool_group` messages are in the run.
    pub count: usize,
    /// Total tool calls across every group in the run.
    pub total_calls: usize,
    /// Exclusive end index of the run in the message list.
    pub end: usize,
}

/// Scan forward from `start` over a run of consecutive tool-call groups.
///
/// Thinking-only assistant messages (empty visible text, reasoning in
/// `details`) sit BETWEEN tool groups during a multi-step turn, so they are
/// stepped over without breaking the run — otherwise a turn that thinks between
/// every tool call would never stack. Any other message ends the run.
///
/// Extracted from `render_chat` so the grouping is unit-testable on its own
/// (#743 step 1). The renderer previously inlined this loop, so the only test
/// coverage was a hand-maintained COPY of the logic in `tui_tool_stack_test`,
/// which could drift from the real renderer without failing. Pure: no `App`, no
/// terminal, no side effects.
///
/// `start` must index a message that has a `tool_group`; callers only reach
/// this after that check.
pub(crate) fn scan_tool_stack(messages: &[DisplayMessage], start: usize) -> ToolStack {
    let mut count = 0;
    let mut total_calls = 0;
    let mut end = start;
    // The run always begins with the group at `start`.
    if let Some(g) = messages.get(start).and_then(|m| m.tool_group.as_ref()) {
        count = 1;
        total_calls = g.calls.len();
        end = start + 1;
    }
    while end < messages.len() {
        let m = &messages[end];
        if let Some(ref g) = m.tool_group {
            count += 1;
            total_calls += g.calls.len();
            end += 1;
        } else if m.role == "assistant" && m.content.trim().is_empty() && m.details.is_some() {
            // Thinking-only: step over it, keep the run alive.
            end += 1;
        } else {
            break;
        }
    }
    ToolStack {
        count,
        total_calls,
        end,
    }
}

/// One turn's span in the message list: `[start, end)` (#757).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TurnRange {
    /// Index of the turn's first message (the user message that opened it).
    pub start: usize,
    /// Exclusive end — the index of the next turn's user message, or the list end.
    pub end: usize,
}

/// Split the message list into turns (#757).
///
/// `DisplayMessage` carries no turn marker, so a turn is inferred from the
/// structure of the conversation: it opens at a `user` message and runs until
/// the next `user` message, absorbing the assistant text, thinking-only
/// messages, tool groups, and any system/error rows the turn produced in
/// between. Every per-turn feature (one collapsible block, a per-turn header, a
/// single expand state) needs this.
///
/// Messages that precede the first user message — history-paging markers, a
/// restored transcript's leading rows — form their own leading range so nothing
/// is dropped. Pure: no `App`, no terminal, no side effects.
pub(crate) fn turn_ranges(messages: &[DisplayMessage]) -> Vec<TurnRange> {
    if messages.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<TurnRange> = Vec::new();
    let mut start = 0usize;
    for (i, m) in messages.iter().enumerate() {
        // A user message opens a new turn, closing the one before it. `i > start`
        // keeps the very first message (and each freshly-opened turn) from
        // closing a zero-length range, and makes back-to-back user messages
        // split into one turn each.
        if m.role == "user" && i > start {
            out.push(TurnRange { start, end: i });
            start = i;
        }
    }
    out.push(TurnRange {
        start,
        end: messages.len(),
    });
    out
}

/// The turn containing `msg_idx`, if any (#757). Used to resolve a clicked or
/// anchored row back to its turn.
#[allow(dead_code)]
pub(crate) fn turn_of(messages: &[DisplayMessage], msg_idx: usize) -> Option<TurnRange> {
    turn_ranges(messages)
        .into_iter()
        .find(|t| msg_idx >= t.start && msg_idx < t.end)
}

/// What a turn actually did, for its header line (#759).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TurnSummary {
    /// Total tool calls across every group in the turn.
    pub tool_calls: usize,
    /// How many of them failed.
    pub failed: usize,
    /// Wall time from the turn's first message to its last, in seconds.
    pub duration_secs: i64,
    /// Whether the turn produced an error row.
    pub has_error: bool,
}

/// Summarise a turn for its header (#759). Pure.
pub(crate) fn turn_summary(messages: &[DisplayMessage], turn: TurnRange) -> TurnSummary {
    let slice = &messages[turn.start.min(messages.len())..turn.end.min(messages.len())];
    let mut s = TurnSummary::default();
    for m in slice {
        if let Some(ref g) = m.tool_group {
            s.tool_calls += g.calls.len();
            s.failed += g.calls.iter().filter(|c| !c.success).count();
        }
        if m.role == "error" {
            s.has_error = true;
        }
    }
    // Prefer the duration stamped at turn end (#964). The fallback below
    // subtracts stored timestamps, which collapses to zero for ~94% of turns:
    // the assistant row is created at turn START and updated in place, so it
    // carries the same created_at as the user message that triggered it. That
    // is why the duration used to vanish the instant a turn settled.
    if let Some(secs) = slice.iter().rev().find_map(|m| m.duration_secs) {
        s.duration_secs = secs.max(0);
    } else if let (Some(first), Some(last)) = (slice.first(), slice.last()) {
        s.duration_secs = (last.timestamp - first.timestamp).num_seconds().max(0);
    }
    s
}

/// Format a turn's header line, or `None` when the turn isn't worth summarising
/// (#759).
///
/// A plain question-and-answer turn ran no tools, so a header would be pure
/// noise — only turns that actually did work get one. Mirrors the shape of the
/// Telegram flow header (status • N tool calls • duration); ctx is deliberately
/// absent because it already lives in the footer under the input (#744).
pub fn format_turn_header(summary: TurnSummary, steps: usize) -> Option<String> {
    if steps == 0 && summary.tool_calls == 0 {
        return None;
    }
    let status = if summary.has_error || summary.failed > 0 {
        "✗"
    } else {
        "✓"
    };
    // Steps and tool calls are independent facts, so the header carries BOTH.
    // They used to be alternatives: the tool count won when any tool ran, and
    // the step count only appeared as its fallback. A turn that narrated 17
    // times without calling anything therefore showed a plausible "17 steps"
    // and never said that nothing executed, which is the one thing worth
    // seeing at a glance.
    let mut parts = vec![format_step_count(steps)];
    parts.push(if summary.tool_calls == 1 {
        "1 tool call".to_string()
    } else {
        // Zero is stated, never omitted: "0 tool calls" IS the diagnostic.
        format!("{} tool calls", summary.tool_calls)
    });
    if summary.failed > 0 {
        parts.push(format!("{} failed", summary.failed));
    }
    if summary.duration_secs > 0 {
        parts.push(humanize_secs(summary.duration_secs));
    }
    Some(format!("{status} {}", parts.join(" · ")))
}

/// `45s` / `1m 30s` / `2h 5m`, matching the Telegram flow's duration style.
fn humanize_secs(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => {
            let (m, r) = (s / 60, s % 60);
            if r == 0 {
                format!("{m}m")
            } else {
                format!("{m}m {r}s")
            }
        }
        s => {
            let (h, m) = (s / 3600, (s % 3600) / 60);
            if m == 0 {
                format!("{h}h")
            } else {
                format!("{h}h {m}m")
            }
        }
    }
}

/// Index of the turn's final answer: the LAST message carrying visible
/// assistant text (#758). Everything before it is working-out.
pub(crate) fn final_answer_idx(messages: &[DisplayMessage], turn: TurnRange) -> Option<usize> {
    (turn.start..turn.end.min(messages.len())).rev().find(|&i| {
        let m = &messages[i];
        m.role == "assistant" && !m.content.trim().is_empty()
    })
}

/// The turn's final answer, but only once the turn has actually finished.
///
/// A running turn has not produced its answer yet: its newest text is the
/// latest STEP. [`final_answer_idx`] cannot tell the difference, since it just
/// takes the last non-empty assistant row, so mid-turn it kept promoting each
/// fresh intermediate text into the one slot the fold must always show. The
/// result was a single block of narration sitting exposed while the tool
/// groups and thinking on either side of it folded away.
///
/// Only the NEWEST turn can be unsettled; every earlier turn is finished by
/// definition and keeps its answer visible.
///
/// Nothing is hidden that the user cannot otherwise see: in-flight text renders
/// from the streaming buffer, with the reasoning excerpt and spinner beside it.
pub(crate) fn settled_final_answer_idx(
    messages: &[DisplayMessage],
    turn: TurnRange,
    is_newest: bool,
    is_processing: bool,
) -> Option<usize> {
    if is_newest && is_processing {
        return None;
    }
    final_answer_idx(messages, turn)
}

/// The one-line preview shown under a folded turn header: the description of
/// the last tool call the turn made, and whether it succeeded.
///
/// A collapsed tool group already renders its last call this way, so without
/// this a folded turn would be strictly LESS informative than the group it
/// replaced: the header alone says how many calls ran but never what they did.
/// `None` when the turn ran no tools, in which case there is no header either.
pub(crate) fn folded_turn_preview(
    messages: &[DisplayMessage],
    turn: TurnRange,
) -> Option<(String, bool)> {
    (turn.start..turn.end.min(messages.len()))
        .rev()
        .find_map(|i| {
            let last = messages[i].tool_group.as_ref()?.calls.last()?;
            Some((last.description.clone(), last.success))
        })
}

/// The transcript of an unfocused pane, drawn the way the focused pane draws
/// it by default (#2075): every turn folds to its header and last-call
/// preview, and only the rows that stay visible when folded are drawn. Tool
/// groups are never drawn as bare bullets, so a pane looks the same whether it
/// is focused or not.
pub(crate) fn folded_pane_lines(messages: &[DisplayMessage], width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for turn in turn_ranges(messages) {
        let summary = turn_summary(messages, turn);
        let final_idx = settled_final_answer_idx(messages, turn, false, false);
        let hideable = turn_hideable_count(messages, turn, final_idx);
        let header = format_turn_header(summary, hideable);
        let preview = folded_turn_preview(messages, turn);
        // The header sits above the turn's first non-user row, as in the focused pane.
        let anchor = if messages.get(turn.start).is_some_and(|m| m.role == "user") {
            turn.start + 1
        } else {
            turn.start
        };
        let end = turn.end.min(messages.len());
        for idx in turn.start..end {
            if idx == anchor
                && let Some(ref base) = header
            {
                push_folded_header(&mut out, base, preview.as_ref());
            }
            if messages[idx].tool_group.is_some() {
                continue;
            }
            if visible_when_folded(messages, idx, final_idx) {
                super::panes::render_simple_message(&mut out, &messages[idx], width);
            }
        }
        if anchor >= end
            && let Some(ref base) = header
        {
            push_folded_header(&mut out, base, preview.as_ref());
        }
    }
    out
}

/// One folded turn's header and last-call preview, styled as the focused pane
/// styles them.
fn push_folded_header(out: &mut Vec<Line<'static>>, base: &str, preview: Option<&(String, bool)>) {
    out.push(Line::from(vec![
        Span::styled(
            format!("  {base}"),
            Style::default()
                .fg(theme::role(Role::AccentTeal))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " (click to expand)".to_string(),
            Style::default().fg(theme::role(Role::GrayMid)),
        ),
    ]));
    if let Some((description, success)) = preview {
        let style = if *success {
            Style::default()
                .fg(theme::role(Role::GrayDim))
                .add_modifier(Modifier::ITALIC)
        } else {
            Style::default()
                .fg(theme::role(Role::Error))
                .add_modifier(Modifier::ITALIC)
        };
        out.push(Line::from(vec![
            Span::styled(
                "    └─ ".to_string(),
                Style::default().fg(theme::role(Role::GrayDim)),
            ),
            Span::styled(description.clone(), style),
        ]));
    }
}

/// Whether row `idx` still renders while its turn is folded (#758).
///
/// Folding hides the turn's working-out — thinking, tool groups, and the
/// intermediate narration between them — and keeps what the user must still
/// see: their own question, the final answer, errors, and anything
/// interactive (an approval prompt or menu, which would otherwise become
/// unreachable). This mirrors the Telegram flow block, where the log folds and
/// the final answer stays a clean bubble.
pub(crate) fn visible_when_folded(
    messages: &[DisplayMessage],
    idx: usize,
    final_idx: Option<usize>,
) -> bool {
    let Some(m) = messages.get(idx) else {
        return false;
    };
    // Never hide something the user has to act on, or an error they must see.
    if m.approval.is_some() || m.approve_menu.is_some() {
        return true;
    }
    match m.role.as_str() {
        // `system` sits with `error`: short harness status the user needs to
        // see — a model switch, a retry, a phantom warning. Folding it away
        // buried the warnings that matter most, and because the step count is
        // "rows the fold hides", it also reported a turn that did no work as
        // having taken a step (#786).
        "user" | "error" | "history_marker" | "system" => true,
        // A substantial report survives the fold even when it is not the single
        // final answer (#904). The fold kept exactly one assistant row, so a
        // turn that produced a real deliverable and then said anything after it
        // folded the deliverable away and exposed the trailing remark instead.
        // The user saw a collapsed turn and a completion that appeared eaten.
        //
        // Only ever shows MORE, never less: a row that already qualified as the
        // final answer is unaffected, and prose without a table is untouched.
        _ => Some(idx) == final_idx || is_deliverable_report(&m.content),
    }
}

/// Whether an assistant row carries a rendered report worth keeping visible
/// when its turn is folded.
///
/// A GFM table is the marker: it is structure the user asked for and cannot
/// reconstruct from a one-line summary. Length alone is not enough — a long
/// narration is still narration — so both a separator row and real bulk are
/// required.
pub fn is_deliverable_report(content: &str) -> bool {
    const MIN_REPORT_CHARS: usize = 200;
    if content.chars().count() < MIN_REPORT_CHARS {
        return false;
    }
    content.lines().any(|line| {
        let t = line.trim();
        t.starts_with('|')
            && t.ends_with('|')
            && t.contains("---")
            && t.chars().filter(|c| *c == '|').count() >= 2
    })
}

/// How many wrapped lines the live reasoning summary may occupy (#768). A hard
/// ceiling, so a long chain truncates instead of growing into a wall.
pub(crate) const THINKING_EXCERPT_LINES: usize = 3;

/// How many wrapped lines an intermediate narration row may occupy when its
/// turn is EXPANDED. A folded turn hides these rows entirely; the ceiling
/// exists for the expanded state, where every row renders and an agentic
/// night run would otherwise repaint the whole transcript as one wall of
/// model working-out. Same budget and spirit as THINKING_EXCERPT_LINES.
pub(crate) const NARRATION_EXCERPT_LINES: usize = 3;

/// The rows of one turn that count as working-out: exactly the complement of
/// [`visible_when_folded`]. A folded frame hides them; an expanded frame
/// renders them, capped at [`NARRATION_EXCERPT_LINES`] each, so expanding a
/// turn can never flood the screen no matter how chatty the run was.
pub(crate) fn turn_narration_rows(
    messages: &[DisplayMessage],
    turn: TurnRange,
    final_idx: Option<usize>,
) -> std::collections::HashSet<usize> {
    (turn.start..turn.end.min(messages.len()))
        .filter(|&idx| !visible_when_folded(messages, idx, final_idx))
        .collect()
}

/// Cap one rendered row's lines to [`NARRATION_EXCERPT_LINES`], appending an
/// explicit overflow marker. The marker must SAY what happened: a silent cut
/// reads as eaten content (the #904 trap), so the count of suppressed lines
/// is spelled out instead of leaving a mysterious truncation.
pub(crate) fn cap_narration_lines(mut content: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if content.len() <= NARRATION_EXCERPT_LINES {
        return content;
    }
    let hidden = content.len() - NARRATION_EXCERPT_LINES;
    content.truncate(NARRATION_EXCERPT_LINES);
    content.push(Line::from(vec![Span::styled(
        format!("  … +{hidden} more lines hidden (intermediate narration)"),
        Style::default()
            .fg(theme::role(Role::GrayDim))
            .add_modifier(Modifier::ITALIC),
    )]));
    content
}

/// How many rows the fold actually hides, which is what the header's step
/// count must report.
///
/// NOT the turn's message count. That includes the user's own message and the
/// final answer, rows the fold never touches, so a turn of question, one
/// thinking row and an answer announced "3 steps" when exactly one row sat
/// behind the header. Every count was inflated by two, and by more whenever a
/// turn carried extra always-visible rows such as errors.
pub(crate) fn turn_hideable_count(
    messages: &[DisplayMessage],
    turn: TurnRange,
    final_idx: Option<usize>,
) -> usize {
    (turn.start..turn.end.min(messages.len()))
        .filter(|&idx| !visible_when_folded(messages, idx, final_idx))
        .count()
}

/// `1 step` / `4 steps`, for a turn with no tool calls to summarise.
pub fn format_step_count(hidden: usize) -> String {
    if hidden == 1 {
        "1 step".to_string()
    } else {
        format!("{hidden} steps")
    }
}

/// Whether a turn renders folded.
///
/// Folded is the DEFAULT, always — including while the turn is still running.
/// The turn's working-out (thinking, tool groups, intermediate narration) lives
/// behind the header; nothing should ever render as a wall. Live progress is
/// unaffected because in-flight streaming text and the active tool group render
/// from their own state after the message loop, not from these finalized rows.
///
/// An explicit click on the header overrides this for one turn, in either
/// direction, remembered by anchor.
pub(crate) fn turn_is_folded(
    overrides: &std::collections::HashMap<String, bool>,
    anchor: &str,
) -> bool {
    match overrides.get(anchor) {
        Some(expanded) => !expanded,
        None => true,
    }
}

/// Stable identity for a turn, derived from its first message.
///
/// The fold override map used to be keyed by the message UUID, which does not
/// survive a reload: live rows carry a random UUID minted as they stream, and
/// `impl From<Message> for DisplayMessage` stamps every reloaded row with its
/// database id. The two never match, so every turn the user had opened came
/// back folded after a session switch (#2155).
///
/// Role and the message timestamp ARE preserved across that round trip: the
/// row is persisted with `created_at.timestamp()` and read back into the same
/// second. Content is deliberately excluded, because the persisted text
/// rewrites `<<IMG:...>>` markers and so differs from what the live row holds.
///
/// A send that straddles a second boundary can still miss its own reload and
/// come back folded. That is the pre-existing behaviour for every turn, so it
/// is never a regression, and two turns of the same role inside one second
/// would have to collide for the key to be ambiguous.
pub(crate) fn turn_anchor(msg: &DisplayMessage) -> String {
    format!("{}@{}", msg.role, msg.timestamp.timestamp())
}

/// One turn's header row, resolved before the render loop walks the messages.
struct TurnHeader {
    /// `✓ 39 tool calls · 3m 57s`
    base: String,
    /// The trailing affordance, which names ctrl+o only on the newest turn.
    hint: &'static str,
    /// Last call description and success, shown only while folded.
    preview: Option<(String, bool)>,
}

/// Render the chat messages
pub(super) fn render_chat(f: &mut Frame, app: &mut App, area: Rect) {
    // A theme switch invalidates every cached line: the cache holds spans whose
    // colours are already resolved, so `/theme` would otherwise repaint only the
    // chrome and leave the transcript on the previous palette (#1634).
    let theme_gen = theme::generation();
    if app.render_cache_theme_gen != theme_gen {
        app.render_cache.clear();
        app.streaming_render_cache = None;
        app.render_cache_theme_gen = theme_gen;
    }

    let mut lines: Vec<Line> = Vec::new();
    // Track which message index each rendered line belongs to (for click-to-copy)
    let mut line_to_msg: Vec<Option<usize>> = Vec::new();

    let content_width = area.width.saturating_sub(4) as usize; // borders + padding

    // Track how many messages to skip (already rendered as part of a stack)
    let mut skip_count: usize = 0;

    // Per-turn folding (#758) + header (#759). A turn's working-out — thinking,
    // tool groups, intermediate narration — folds into one header line, leaving
    // the question, the final answer, errors and anything interactive visible.
    // EVERY turn folds by default, the running one included; only an explicit
    // click or ctrl+o opens one.
    let all_turns = turn_ranges(&app.messages);
    // idx → header text, and idx → the turn anchor it toggles.
    let mut turn_headers: std::collections::HashMap<usize, TurnHeader> =
        std::collections::HashMap::new();
    // ctrl+o acts on the newest turn only, so only its header may advertise it.
    let newest_start = all_turns.last().map(|t| t.start);
    let mut header_anchor: std::collections::HashMap<usize, String> =
        std::collections::HashMap::new();
    // idx → false when that row is folded away this frame.
    let mut row_visible: std::collections::HashMap<usize, bool> = std::collections::HashMap::new();
    // Rows that are working-out (the fold's hide set). When their turn is
    // expanded they still render, but capped, so an expanded turn can never
    // paint a wall of intermediate narration however long the run was.
    let mut narration_rows: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for t in &all_turns {
        let Some(anchor) = app.messages.get(t.start).map(turn_anchor) else {
            continue;
        };
        // The running turn is never folded: it has no final answer yet, so
        // folding it hides the very text the user is waiting on (#2155).
        let is_newest = newest_start == Some(t.start);
        let folded = turn_is_folded(&app.turn_expanded, &anchor);
        let summary = turn_summary(&app.messages, *t);
        let final_idx = settled_final_answer_idx(&app.messages, *t, is_newest, app.is_processing);

        let hideable = turn_hideable_count(&app.messages, *t, final_idx);

        // Working-out rows of this turn: hidden entirely while folded, capped
        // while expanded (the two states share one definition, so the two
        // views can never disagree about what counts as working-out).
        for idx in turn_narration_rows(&app.messages, *t, final_idx) {
            narration_rows.insert(idx);
        }

        if folded {
            for idx in t.start..t.end.min(app.messages.len()) {
                if !visible_when_folded(&app.messages, idx, final_idx) {
                    row_visible.insert(idx, false);
                }
            }
        }
        // A header is worth showing when the turn did work, or when it has
        // working-out to fold away (so the user can get it back, and put it
        // back).
        if let Some(base) = format_turn_header(summary, hideable) {
            let newest = newest_start == Some(t.start);
            let hint = match (folded, newest) {
                (true, true) => " (click / ctrl+o to expand)",
                (true, false) => " (click to expand)",
                (false, true) => " (click / ctrl+o to fold)",
                (false, false) => " (click to fold)",
            };
            let at = if app.messages.get(t.start).is_some_and(|m| m.role == "user") {
                t.start + 1
            } else {
                t.start
            };
            if at < t.end {
                turn_headers.insert(
                    at,
                    TurnHeader {
                        base,
                        hint,
                        // Only a folded turn needs the preview: an open one is
                        // already showing the calls themselves.
                        preview: folded
                            .then(|| folded_turn_preview(&app.messages, *t))
                            .flatten(),
                    },
                );
                header_anchor.insert(at, anchor);
            }
        }
    }
    let mut line_to_turn: Vec<Option<String>> = Vec::new();

    // Iterate by index to allow mutable access to render_cache while reading messages
    for msg_idx in 0..app.messages.len() {
        // Skip messages already rendered as part of a stacked group
        if skip_count > 0 {
            skip_count -= 1;
            continue;
        }

        // Turn header, above the turn's first non-user row. Mapped to no
        // MESSAGE (so line→message routing for click/drag is untouched) but to
        // its turn anchor, so a click on this row folds/unfolds the turn (#758).
        if let Some(header) = turn_headers.get(&msg_idx) {
            // Same cyan weight the tool-group header carried before folding —
            // a folded turn must not be LESS visible than the rows it replaced.
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}", header.base),
                    Style::default()
                        .fg(theme::role(Role::AccentTeal))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    header.hint.to_string(),
                    Style::default().fg(theme::role(Role::GrayMid)),
                ),
            ]));
            line_to_msg.resize(lines.len(), None);
            line_to_turn.resize(lines.len(), None);
            if let (Some(anchor), Some(slot)) =
                (header_anchor.get(&msg_idx), line_to_turn.last_mut())
            {
                *slot = Some(anchor.clone());
            }
            // Last call under the header, styled exactly like the collapsed
            // tool group's preview so folding loses no information.
            if let Some((description, success)) = &header.preview {
                let style = if *success {
                    Style::default()
                        .fg(theme::role(Role::GrayDim))
                        .add_modifier(Modifier::ITALIC)
                } else {
                    Style::default()
                        .fg(theme::role(Role::Error))
                        .add_modifier(Modifier::ITALIC)
                };
                lines.push(Line::from(vec![
                    Span::styled("    └─ ", Style::default().fg(theme::role(Role::GrayDim))),
                    Span::styled(description.clone(), style),
                ]));
                line_to_msg.resize(lines.len(), None);
                line_to_turn.resize(lines.len(), None);
                if let (Some(anchor), Some(slot)) =
                    (header_anchor.get(&msg_idx), line_to_turn.last_mut())
                {
                    // The preview row toggles the turn too, so a click near the
                    // header does what it looks like it should.
                    *slot = Some(anchor.clone());
                }
            }
            // Breathing room: without it the header sits flush against the
            // answer below and reads as part of it.
            lines.push(Line::from(""));
            line_to_msg.resize(lines.len(), None);
            line_to_turn.resize(lines.len(), None);
        }

        // Folded away this frame — the turn header above can bring it back.
        if row_visible.get(&msg_idx) == Some(&false) {
            continue;
        }

        let lines_before = lines.len();

        // Render inline approval messages
        if let Some(ref approval) = app.messages[msg_idx].approval {
            render_inline_approval(&mut lines, approval, content_width);
            lines.push(Line::from(""));
            line_to_msg.resize(lines.len(), None);
            continue;
        }

        // Render /approve policy menu
        if let Some(ref menu) = app.messages[msg_idx].approve_menu {
            render_approve_menu(&mut lines, menu, content_width);
            lines.push(Line::from(""));
            line_to_msg.resize(lines.len(), None);
            continue;
        }

        // Render history paging marker
        if app.messages[msg_idx].role == "history_marker" {
            lines.push(Line::from(Span::styled(
                app.messages[msg_idx].content.clone(),
                Style::default()
                    .fg(theme::role(Role::GrayDim))
                    .add_modifier(Modifier::ITALIC),
            )));
            lines.push(Line::from(""));
            line_to_msg.resize(lines.len(), None);
            continue;
        }

        // Render tool call groups (finalized) — stack consecutive groups
        if let Some(ref group) = app.messages[msg_idx].tool_group {
            // Look ahead over the run of consecutive tool groups. The scan is
            // extracted so it can be unit-tested directly (#743 step 1).
            let stack = scan_tool_stack(&app.messages, msg_idx);
            let stack_count = stack.count;
            let stack_total_calls = stack.total_calls;
            let lookahead = stack.end;

            if stack_count >= 3 {
                // Stacked: render a single collapsed summary for all groups
                let dot = "●";
                let stack_expanded = group.expanded; // Use first group's expanded state
                let header = if stack_total_calls == stack_count {
                    format!("{} tool calls", stack_total_calls)
                } else {
                    format!("{} tool calls ({} groups)", stack_total_calls, stack_count)
                };
                let mut header_spans = vec![Span::styled(
                    format!("  {} {}", dot, header),
                    Style::default()
                        .fg(theme::role(Role::AccentTeal))
                        .add_modifier(Modifier::BOLD),
                )];
                header_spans.push(Span::styled(
                    if stack_expanded {
                        " (ctrl+o to collapse)"
                    } else {
                        " (ctrl+o to expand)"
                    },
                    Style::default().fg(theme::role(Role::GrayMid)),
                ));
                lines.push(Line::from(header_spans));

                if stack_expanded {
                    // Show each group's last call as a tree entry
                    let mut group_idx = 0;
                    for i in msg_idx..lookahead {
                        if let Some(ref g) = app.messages[i].tool_group {
                            let connector = if group_idx == stack_count - 1 {
                                "└─"
                            } else {
                                "├─"
                            };
                            if let Some(last) = g.calls.last() {
                                let style = if last.success {
                                    Style::default()
                                        .fg(theme::role(Role::GrayDim))
                                        .add_modifier(Modifier::ITALIC)
                                } else {
                                    Style::default()
                                        .fg(theme::role(Role::Error))
                                        .add_modifier(Modifier::ITALIC)
                                };
                                let mut spans = vec![
                                    Span::styled(
                                        format!("    {} ", connector),
                                        Style::default().fg(theme::role(Role::GrayDim)),
                                    ),
                                    Span::styled(last.description.clone(), style),
                                ];
                                if let Some(suffix) = super::tools::detached_state_suffix(last) {
                                    spans.push(Span::styled(
                                        suffix,
                                        Style::default().fg(theme::role(Role::GrayMid)),
                                    ));
                                }
                                lines.push(Line::from(spans));
                            }
                            group_idx += 1;
                        }
                    }
                } else {
                    // Collapsed: show last group's last call
                    if let Some(last) = group.calls.last() {
                        let style = if last.success {
                            Style::default()
                                .fg(theme::role(Role::GrayDim))
                                .add_modifier(Modifier::ITALIC)
                        } else {
                            Style::default()
                                .fg(theme::role(Role::Error))
                                .add_modifier(Modifier::ITALIC)
                        };
                        let mut spans = vec![
                            Span::styled(
                                "    └─ ".to_string(),
                                Style::default().fg(theme::role(Role::GrayDim)),
                            ),
                            Span::styled(last.description.clone(), style),
                        ];
                        if let Some(suffix) = super::tools::detached_state_suffix(last) {
                            spans.push(Span::styled(
                                suffix,
                                Style::default().fg(theme::role(Role::GrayMid)),
                            ));
                        }
                        lines.push(Line::from(spans));
                    }
                }

                lines.push(Line::from(""));
                // Map the stack's lines to THIS group's message (#726) so a click
                // toggles the block under the cursor (and toggles it back next
                // click). Mapping them to None made a tool-group click hit an
                // adjacent message's line instead — expanding a block elsewhere.
                line_to_msg.resize(lines.len(), Some(msg_idx));

                // Skip all messages in the stack (tool groups + thinking-only assistants)
                skip_count = lookahead - msg_idx - 1;
                continue;
            }

            render_tool_group(&mut lines, group, false, app.animation_frame, content_width);
            lines.push(Line::from(""));
            // Map the group's lines to its message so a click toggles it (#726).
            line_to_msg.resize(lines.len(), Some(msg_idx));
            continue;
        }

        if app.messages[msg_idx].role == "system" {
            // System messages: visible yellow label, split on newlines so
            // multi-line content actually renders (not clipped to one line).
            let system_style = Style::default()
                .fg(theme::role(Role::WarningMuted))
                .add_modifier(Modifier::ITALIC);

            for (i, text_line) in app.messages[msg_idx].content.lines().enumerate() {
                let mut spans = vec![Span::styled("  ", Style::default())];
                if i == 0 {
                    spans.push(Span::styled("⚡ ", system_style));
                } else {
                    spans.push(Span::styled("   ", Style::default()));
                }
                // #1897: system text bypasses parse_markdown, so reorder it here.
                let body = apply_rtl(text_line).unwrap_or_else(|| text_line.to_string());
                spans.push(Span::styled(body, system_style));

                // Show expand/collapse hint on the first line only
                if i == 0 && app.messages[msg_idx].details.is_some() {
                    let hint = if app.messages[msg_idx].expanded {
                        " (ctrl+o to collapse)"
                    } else {
                        " (ctrl+o to expand)"
                    };
                    spans.push(Span::styled(
                        hint,
                        Style::default().fg(theme::role(Role::Gray)),
                    ));
                }
                let line = Line::from(spans);
                for wrapped in wrap_line_with_padding(line, content_width, "    ") {
                    lines.push(wrapped);
                }
            }

            // Show expanded details (e.g. tool output, compaction summary)
            if app.messages[msg_idx].expanded
                && let Some(ref details) = app.messages[msg_idx].details
            {
                for detail_line in details.lines() {
                    // Check for diff lines (+/-) and color accordingly
                    let (style, line_text): (Style, &str) =
                        if let Some(stripped) = detail_line.strip_prefix("+ ") {
                            (Style::default().fg(theme::role(Role::Success)), stripped)
                        } else if let Some(stripped) = detail_line.strip_prefix("- ") {
                            (Style::default().fg(theme::role(Role::Error)), stripped)
                        } else {
                            (Style::default().fg(theme::role(Role::GrayDim)), detail_line)
                        };

                    lines.push(Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(line_text.to_string(), style),
                    ]));
                }
            }
            lines.push(Line::from(""));
            // System messages are mapped to their index for copy
            for _ in lines_before..lines.len() {
                line_to_msg.push(Some(msg_idx));
            }
            continue;
        }

        if app.messages[msg_idx].role == "error" {
            // Error messages: distinctive red treatment so a user
            // scrolling through chat history sees instantly that the
            // turn failed (vs. a turn that completed quietly with no
            // text). Without this, a 5xx-exhaustion looks identical
            // to a successful empty-completion turn — a user reports
            // "the agent silently dropped my request" when in fact
            // self-heal already gave up and tried to tell them.
            // Reported 2026-06-01: a 502 fallback chain exhausted,
            // `TuiEvent::Error` fired, `show_error` showed a 2.5s
            // toast that vanished, the user only saw their prompt +
            // tool call + Thinking with no completion.
            let err_style = Style::default()
                .fg(theme::role(Role::ErrorSoft))
                .add_modifier(Modifier::BOLD);
            let body_style = Style::default().fg(theme::role(Role::ErrorFaded));
            for (i, text_line) in app.messages[msg_idx].content.lines().enumerate() {
                let mut spans = vec![Span::styled("  ", Style::default())];
                if i == 0 {
                    spans.push(Span::styled("❌ Error: ", err_style));
                } else {
                    spans.push(Span::styled("   ", Style::default()));
                }
                spans.push(Span::styled(text_line.to_string(), body_style));
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
            for _ in lines_before..lines.len() {
                line_to_msg.push(Some(msg_idx));
            }
            continue;
        }

        // Dot/arrow message differentiation (no role labels needed)
        let is_user = app.messages[msg_idx].role == "user";
        // Highlight selected message with subtle background
        let is_selected = app.selected_message_idx == Some(msg_idx);
        let msg_bg: Option<Color> = if is_selected {
            Some(theme::role(Role::SurfaceCode))
        } else if is_user {
            Some(theme::role(Role::SurfaceCodeAlt))
        } else {
            None
        };

        // Parse and render message content as markdown (cached per message + width)
        let msg_id = app.messages[msg_idx].id;
        let cache_key = (msg_id, content_width as u16);
        if !app.render_cache.contains_key(&cache_key) {
            let parsed = parse_markdown(
                &app.messages[msg_idx].content,
                content_width.saturating_sub(2),
            );
            app.render_cache.insert(cache_key, parsed);
        }
        let content_lines = if narration_rows.contains(&msg_idx) {
            // Expanded-turn working-out: bounded excerpt, never the full
            // paragraph wall. The full text stays in the session log; only
            // the transcript render is capped.
            cap_narration_lines(app.render_cache[&cache_key].clone())
        } else {
            app.render_cache[&cache_key].clone()
        };
        for (i, line) in content_lines.into_iter().enumerate() {
            let mut padded_spans = if i == 0 {
                if is_user {
                    // User: arrow prefix
                    vec![Span::styled(
                        "\u{276F} ",
                        Style::default().fg(theme::role(Role::GrayMid)),
                    )]
                } else {
                    // Assistant: colored dot prefix
                    vec![Span::styled(
                        "\u{25CF} ",
                        Style::default()
                            .fg(theme::role(Role::Gray))
                            .add_modifier(Modifier::BOLD),
                    )]
                }
            } else {
                vec![Span::raw("  ")]
            };
            padded_spans.extend(line.spans);
            let padded_line = Line::from(padded_spans);
            for wrapped in wrap_line_with_padding(padded_line, content_width, "  ") {
                if let Some(bg) = msg_bg {
                    // Apply bg to all spans and pad to full line width.
                    // Force white text on user messages so the dark bg
                    // remains readable on light terminal themes.
                    let mut spans: Vec<Span> = wrapped
                        .spans
                        .into_iter()
                        .map(|s| Span::styled(s.content, s.style.bg(bg)))
                        .collect();
                    let line_width: usize = spans.iter().map(|s| s.content.width()).sum();
                    let remaining = content_width.saturating_sub(line_width);
                    if remaining > 0 {
                        spans.push(Span::styled(" ".repeat(remaining), Style::default().bg(bg)));
                    }
                    lines.push(Line::from(spans));
                } else {
                    lines.push(wrapped);
                }
            }
        }

        // Render reasoning details on assistant messages (collapsible).
        // Only show the label when details actually contain visible content —
        // an empty/whitespace `Some("")` produces a phantom Thinking label
        // that the user can't ever expand into anything useful.
        let has_reasoning = app.messages[msg_idx]
            .details
            .as_ref()
            .is_some_and(|d| !d.trim().is_empty());
        if !is_user && has_reasoning {
            lines.push(Line::from(""));
            // 3-state expand cycle for reasoning (#727): collapsed -> capped ->
            // full -> collapsed, via a click on the block or Ctrl+O.
            let expanded = app.messages[msg_idx].expanded;
            let full = app.messages[msg_idx].expanded_full;
            let hint_text = match (expanded, full) {
                (false, _) => "  ▸ Thinking (click / ctrl+o to expand)",
                (true, false) => "  ▾ Thinking (click / ctrl+o for full)",
                (true, true) => "  ▾ Thinking (click / ctrl+o to collapse)",
            };
            // Thinking label — no visible background
            let hint_span = Span::styled(
                hint_text.to_string(),
                Style::default()
                    .fg(theme::role(Role::GrayDim))
                    .add_modifier(Modifier::ITALIC),
            );
            lines.push(Line::from(vec![hint_span]));
            if expanded && let Some(ref details) = app.messages[msg_idx].details {
                lines.push(Line::from(""));
                // Reserve the outer 2-space indent in the wrap budget so
                // reasoning_to_lines produces lines that already fit
                // `content_width - 2`. Then we prepend the outer "  "
                // prefix and push as-is — no second wrap pass. Previously
                // the caller added the prefix, exceeded content_width,
                // triggered a re-wrap, and broke at wrong word boundaries
                // (e.g. `like.this.and.this` artifacts, inconsistent
                // 2-vs-4-space continuation indent).
                let inner_width = content_width.saturating_sub(2);
                let reasoning_lines = reasoning_to_lines(details, inner_width);
                let reasoning_style = Style::default()
                    .fg(theme::role(Role::GrayDim))
                    .add_modifier(Modifier::ITALIC);
                // Capped state: show only the first REASONING_CAP lines so a
                // 2-page thinking block never floods the viewport (#727). Full
                // state shows everything.
                const REASONING_CAP: usize = 10;
                let total = reasoning_lines.len();
                let show = if full {
                    total
                } else {
                    total.min(REASONING_CAP)
                };
                for line in reasoning_lines.into_iter().take(show) {
                    let mut padded_spans = vec![Span::styled("  ", Style::default())];
                    for span in line.spans {
                        padded_spans.push(Span::styled(span.content.to_string(), reasoning_style));
                    }
                    lines.push(Line::from(padded_spans));
                }
                if !full && total > show {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  … {} more lines (click / ctrl+o for full)", total - show),
                        Style::default()
                            .fg(theme::role(Role::GrayMid))
                            .add_modifier(Modifier::ITALIC),
                    )]));
                }
            }
        }

        // Map all lines from this message to its index
        let msg_lines_end = lines.len();
        for _ in lines_before..msg_lines_end {
            line_to_msg.push(Some(msg_idx));
        }

        // Spacing between messages
        lines.push(Line::from(""));
        line_to_msg.push(None);
    }

    let has_pending_approval = app.has_pending_approval();

    // Add streaming response if present (hide when approval is pending).
    // Also hide when user has scrolled up (auto_scroll=false) so the viewport
    // stays pinned to the content they are reading.
    if !has_pending_approval
        && app.auto_scroll
        && let Some(ref response) = app.streaming_response
    {
        // Render reasoning/thinking content above the response text (dimmed style)
        if let Some(ref reasoning) = app.streaming_reasoning {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(
                    "Thinking...",
                    Style::default()
                        .fg(theme::role(Role::GrayDim))
                        .add_modifier(Modifier::ITALIC | Modifier::BOLD),
                ),
            ]));
            // See comment at the expanded-message reasoning site above.
            // Cap thinking display to last N lines (rolling window) to avoid
            // swallowing the entire viewport during long thinking phases.
            const MAX_THINKING_LINES: usize = 12;
            let inner_width = content_width.saturating_sub(2);
            let reasoning_lines = reasoning_to_lines(reasoning, inner_width);
            let reasoning_style = Style::default()
                .fg(theme::role(Role::GrayDim))
                .add_modifier(Modifier::ITALIC);
            let start_idx = reasoning_lines.len().saturating_sub(MAX_THINKING_LINES);
            if start_idx > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  ⋯ {} more lines", start_idx),
                    Style::default().fg(theme::role(Role::GrayDim)),
                )));
            }
            for line in reasoning_lines.into_iter().skip(start_idx) {
                let mut padded_spans = vec![Span::styled("  ", Style::default())];
                for span in line.spans {
                    padded_spans.push(Span::styled(span.content.to_string(), reasoning_style));
                }
                lines.push(Line::from(padded_spans));
            }
            lines.push(Line::from("")); // separator between reasoning and response
        }

        // Cache parsed markdown for the streaming buffer — re-parse only when
        // the buffer grows. This avoids O(n²) markdown parsing during streaming
        // (previously re-parsed the entire growing buffer on every render frame).
        let current_len = response.len();
        let streaming_lines = if let Some((cached_len, ref cached)) = app.streaming_render_cache {
            if cached_len == current_len {
                cached.clone()
            } else {
                let clean = crate::utils::sanitize::strip_llm_artifacts(response);
                let parsed = parse_markdown(&clean, content_width.saturating_sub(2));
                app.streaming_render_cache = Some((current_len, parsed.clone()));
                parsed
            }
        } else {
            let clean = crate::utils::sanitize::strip_llm_artifacts(response);
            let parsed = parse_markdown(&clean, content_width);
            app.streaming_render_cache = Some((current_len, parsed.clone()));
            parsed
        };
        for line in streaming_lines {
            let mut padded_spans = vec![Span::raw("  ")];
            padded_spans.extend(line.spans);
            let padded_line = Line::from(padded_spans);
            for wrapped in wrap_line_with_padding(padded_line, content_width, "  ") {
                lines.push(wrapped);
            }
        }

        // The spinner claims a turn is running, so it is gated on the turn
        // actually running. The streamed text above is NOT gated: hiding it
        // would make content the user is reading vanish on cancel (#1342).
        if app.is_processing {
            // Blank line to separate content from status spinner
            lines.push(Line::from(""));

            // Spinner at BOTTOM of streaming content so it's always visible
            let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let frame = spinner_frames[app.animation_frame % spinner_frames.len()];

            let elapsed = app
                .processing_started_at
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);

            let mut spans = vec![
                Span::styled(
                    format!("{} ", frame),
                    Style::default()
                        .fg(theme::role(Role::Gray))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "🦀 OpenCrabs ",
                    Style::default()
                        .fg(theme::role(Role::Gray))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "is responding...",
                    Style::default().fg(theme::role(Role::Accent)),
                ),
            ];
            if let Some(meta) = format_turn_spinner_meta(elapsed, app.streaming_output_tokens) {
                spans.push(Span::styled(
                    meta,
                    Style::default().fg(theme::role(Role::GrayDim)),
                ));
            }
            lines.push(Line::from(spans));
        }
    }

    // Render standalone reasoning during thinking-only phase
    // (before first text token — Kimi K2.5, DeepSeek-R1, etc.)
    // streaming_response=None but reasoning is already streaming in
    // Hide when user has scrolled up so the viewport stays frozen.
    if !has_pending_approval
        && app.is_processing
        && app.auto_scroll
        && app.streaming_response.is_none()
        && let Some(ref reasoning) = app.streaming_reasoning
    {
        // Show a short latest-sentence excerpt of the live reasoning instead of
        // a scrolling 12-line window, so a long thought reads as one status line
        // rather than a wall (#742). The full reasoning stays available on the
        // finalized message's 3-state expand.
        let excerpt = crate::utils::string::thinking_excerpt_capped(
            reasoning,
            crate::utils::string::THINKING_EXCERPT_CHARS,
        )
        .unwrap_or_else(|| "Thinking…".to_string());
        // Wrap across at most 3 lines (#768). One line cut a multi-step chain
        // off mid-thought; the ceiling is what keeps this a status line and not
        // the scrolling window #742 replaced.
        let style = Style::default()
            .fg(theme::role(Role::GrayDim))
            .add_modifier(Modifier::ITALIC);
        let wrapped = wrap_line_with_padding(
            Line::from(Span::styled(excerpt, style)),
            content_width.saturating_sub(5),
            "",
        );
        let overflows = wrapped.len() > THINKING_EXCERPT_LINES;
        for (i, wrapped_line) in wrapped.into_iter().take(THINKING_EXCERPT_LINES).enumerate() {
            // Continuations align under the first line's text, past the emoji.
            let prefix = if i == 0 { "  🧠 " } else { "     " };
            let mut spans = vec![Span::styled(
                prefix,
                Style::default().fg(theme::role(Role::GrayDim)),
            )];
            spans.extend(wrapped_line.spans);
            if overflows && i == THINKING_EXCERPT_LINES - 1 {
                spans.push(Span::styled("…", style));
            }
            lines.push(Line::from(spans));
        }

        // Blank line to separate reasoning from status spinner
        lines.push(Line::from(""));

        // Spinner at BOTTOM of reasoning content so it's always visible
        let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame = spinner_frames[app.animation_frame % spinner_frames.len()];
        let elapsed = app
            .processing_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let mut header_spans = vec![
            Span::styled(
                format!("{} ", frame),
                Style::default()
                    .fg(theme::role(Role::Gray))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "🦀 OpenCrabs ",
                Style::default()
                    .fg(theme::role(Role::Gray))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "is thinking...",
                Style::default().fg(theme::role(Role::Accent)),
            ),
        ];
        if let Some(meta) = format_turn_spinner_meta(elapsed, app.streaming_output_tokens) {
            header_spans.push(Span::styled(
                meta,
                Style::default().fg(theme::role(Role::GrayDim)),
            ));
        }
        lines.push(Line::from(header_spans));
    }

    // Inline "OpenCrabs is thinking..." spinner during tool execution / waiting
    // (no streaming text or reasoning yet). Renders ABOVE the tool group so the
    // user always sees the spinner on top of the processing indicator.
    // Hide when user has scrolled up so the viewport stays frozen.
    if !has_pending_approval
        && app.auto_scroll
        && app.is_processing
        && app.streaming_response.is_none()
        && app.streaming_reasoning.is_none()
    {
        let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame = spinner_frames[app.animation_frame % spinner_frames.len()];
        let elapsed = app
            .processing_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let mut spans = vec![
            Span::styled(
                format!("  {} ", frame),
                Style::default()
                    .fg(theme::role(Role::Gray))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "OpenCrabs is thinking...",
                Style::default().fg(theme::role(Role::Accent)),
            ),
        ];
        if let Some(meta) = format_turn_spinner_meta(elapsed, app.streaming_output_tokens) {
            spans.push(Span::styled(
                meta,
                Style::default().fg(theme::role(Role::GrayMid)),
            ));
        }
        lines.push(Line::from(spans));
    }

    // Render active tool group (live, during processing) — below spinner
    // so it's always visible at the bottom with auto-scroll.
    // Hide when user has scrolled up so the viewport stays frozen.
    if app.auto_scroll
        && let Some(ref group) = app.active_tool_group
    {
        let is_active = group.calls.iter().any(|c| !c.completed);
        render_tool_group(
            &mut lines,
            group,
            is_active,
            app.animation_frame,
            content_width,
        );
    }

    // Show SSH password dialog inline (like approval dialogs).
    // Same shape as the sudo dialog, different label.
    if let Some(ref ssh_req) = app.ssh_pending {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "  \u{1F511} ",
                Style::default().fg(theme::role(Role::BlueSoft)),
            ),
            Span::styled(
                "SSH password required",
                Style::default()
                    .fg(theme::role(Role::BlueSoft))
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        let target_display = if ssh_req.target.len() > 60 {
            format!("{}...", ssh_req.target.chars().take(57).collect::<String>())
        } else {
            ssh_req.target.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(
                "  Target:   ",
                Style::default().fg(theme::role(Role::GrayDim)),
            ),
            Span::styled(
                target_display,
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled(
                "  Password: ",
                Style::default().fg(theme::role(Role::GrayDim)),
            ),
            Span::styled(
                "\u{2022}".repeat(app.ssh_input.len()),
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
            Span::styled("\u{2588}", Style::default().fg(theme::role(Role::Gray))),
        ]));
        lines.push(hints::footer_line(
            "  ",
            dialog_keys(DialogScope::SshPassword),
        ));
        lines.push(Line::from(""));
    }

    // Show sudo password dialog inline (like approval dialogs)
    if let Some(ref sudo_req) = app.sudo_pending {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(
                "  \u{1F512} ",
                Style::default().fg(theme::role(Role::Accent)),
            ),
            Span::styled(
                "sudo password required",
                Style::default()
                    .fg(theme::role(Role::Accent))
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        // Show the command being run
        let cmd_display = if sudo_req.command.len() > 60 {
            format!(
                "{}...",
                sudo_req.command.chars().take(57).collect::<String>()
            )
        } else {
            sudo_req.command.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(
                "  Command: ",
                Style::default().fg(theme::role(Role::GrayDim)),
            ),
            Span::styled(
                cmd_display,
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
        ]));
        // Password input (masked with dots)
        lines.push(Line::from(vec![
            Span::styled(
                "  Password: ",
                Style::default().fg(theme::role(Role::GrayDim)),
            ),
            Span::styled(
                "\u{2022}".repeat(app.sudo_input.len()),
                Style::default().fg(theme::role(Role::TextPrimary)),
            ),
            Span::styled("\u{2588}", Style::default().fg(theme::role(Role::Gray))),
        ]));
        // The one-time fix, so the prompt stops appearing at all (#2157). Dimmed:
        // it is the answer to "why am I typing this every time", not part of the
        // command the user is about to run.
        for hint_line in crate::brain::tools::bash::sudo_access_hint_cached().lines() {
            lines.push(Line::from(Span::styled(
                format!("  {}", hint_line),
                Style::default().fg(theme::role(Role::GrayDim)),
            )));
        }
        // Help line
        lines.push(hints::footer_line(
            "  ",
            dialog_keys(DialogScope::SshPassword),
        ));
        lines.push(Line::from(""));
    }

    // Pad line_to_msg for any remaining lines (streaming, errors, etc.)
    line_to_msg.resize(lines.len(), None);
    app.chat_line_to_msg = line_to_msg;
    // Same length as the line map so a row index is valid in both (#758).
    line_to_turn.resize(app.chat_line_to_msg.len(), None);
    app.chat_line_to_turn = line_to_turn;

    // Calculate scroll offset — lines are pre-wrapped so count is accurate
    let total_lines = lines.len();

    // Compensate for new content appended at the bottom while user is scrolled up.
    // Only add the delta (new lines since last render), not the total, to avoid
    // the accumulation bug that inflated scroll_offset by 1000+ in seconds.
    // Skip if prev_rendered_lines == 0 (first render) to avoid massive jump.
    // A toggle with no row to pin still counts as user-driven growth. The lines
    // a fold or unfold adds land where the user is looking, not appended below
    // the viewport, so compensating for them would move the transcript by
    // however many lines the block gained (#2241). Cleared here, once.
    let expand_pending = app.chat_expand_anchor.is_some()
        || app.chat_expand_anchor_turn.is_some()
        || app.chat_expand_growth_pending;
    app.chat_expand_growth_pending = false;

    if should_compensate_for_growth(
        app.auto_scroll,
        app.scroll_offset,
        app.prev_rendered_lines,
        total_lines,
        expand_pending,
    ) {
        let delta = total_lines - app.prev_rendered_lines;
        app.scroll_offset += delta;
        tracing::debug!(
            "[SCROLL] compensated: scroll_offset += {} (delta), now {}",
            delta,
            app.scroll_offset
        );
    }
    app.prev_rendered_lines = total_lines;

    // Only 1 row of top padding (Borders::NONE + Padding::new(1,1,1,0)); no border rows
    let visible_height = area.height.saturating_sub(1) as usize;
    let max_scroll = total_lines.saturating_sub(visible_height);

    // Expand/collapse anchor (#728): a click or ctrl+o that grew/shrank a block
    // pinned its header to the screen row it sat on. Restore it there instead of
    // letting the fixed-from-bottom offset jump the viewport — the block expands
    // in place. Overrides the streaming compensation above for this one frame.
    if let Some((anchor_idx, anchor_row)) = app.chat_expand_anchor.take()
        && let Some(line_top) = app
            .chat_line_to_msg
            .iter()
            .position(|m| *m == Some(anchor_idx))
    {
        let (offset, auto) = anchored_scroll_offset(line_top, anchor_row as usize, max_scroll);
        app.scroll_offset = offset;
        // At the very bottom, re-arm auto-scroll so streaming keeps sticking.
        app.auto_scroll = auto;
    }

    // Same pin for a turn header, resolved through the turn map because a
    // header line belongs to no message (#743 follow-up). Folding a turn hides
    // its whole working-out, so this is the case where an unpinned viewport
    // jumps furthest.
    if let Some((anchor, anchor_row)) = app.chat_expand_anchor_turn.take()
        && let Some(line_top) = app
            .chat_line_to_turn
            .iter()
            .position(|t| t.as_deref() == Some(anchor.as_str()))
    {
        let (offset, auto) = anchored_scroll_offset(line_top, anchor_row as usize, max_scroll);
        app.scroll_offset = offset;
        app.auto_scroll = auto;
    }

    // Cap scroll_offset at max_scroll so it never exceeds the scrollable range.
    // Without this, streaming compensation + mouse coalescing can inflate
    // scroll_offset to hundreds while max_scroll is ~80, making the user
    // scroll down hundreds of times to get back to the visible area.
    let before_cap = app.scroll_offset;
    app.scroll_offset = app.scroll_offset.min(max_scroll);
    if before_cap != app.scroll_offset {
        tracing::debug!(
            "[SCROLL] capped: {} -> {} (max_scroll={})",
            before_cap,
            app.scroll_offset,
            max_scroll
        );
    }

    let actual_scroll_offset = max_scroll.saturating_sub(app.scroll_offset);

    // Scroll state: ONE trace line, only when something actually changed.
    // The old per-frame DEBUG pair (start+end, ~10 lines/second while the
    // TUI is open) was over a million lines a day — 95% of the log file —
    // while carrying values that were identical frame after frame (#400).
    {
        {
            type ScrollSnapshot = (usize, usize, usize, bool, usize);
            static LAST_SCROLL_LOG: std::sync::Mutex<Option<ScrollSnapshot>> =
                std::sync::Mutex::new(None);
            let snapshot = (
                app.scroll_offset,
                total_lines,
                actual_scroll_offset,
                app.auto_scroll,
                max_scroll,
            );
            let mut last = LAST_SCROLL_LOG.lock().unwrap_or_else(|e| e.into_inner());
            if last.map(|l| l != snapshot).unwrap_or(true) {
                {
                    tracing::trace!(
                        "[SCROLL] offset={} total={} actual={} auto={} max={} height={}",
                        app.scroll_offset,
                        total_lines,
                        actual_scroll_offset,
                        app.auto_scroll,
                        max_scroll,
                        visible_height
                    );
                    *last = Some(snapshot);
                }
            }
        }
    }

    // Store render info for click-to-copy + drag-selection coordinate mapping
    app.chat_render_scroll = actual_scroll_offset;
    app.chat_area_y = area.y;
    app.chat_area_x = area.x;
    app.chat_area_width = area.width;
    app.chat_area_height = area.height;
    // Snapshot plain-text of each rendered line for drag-select extraction.
    app.chat_rendered_lines = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();

    let chat = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::NONE)
                .padding(Padding::new(1, 1, 1, 0)),
        )
        .scroll(((actual_scroll_offset.min(u16::MAX as usize)) as u16, 0));

    // Clear the area first to prevent stale buffer content from bleeding through.
    // Ratatui's Paragraph only writes cells where it has text; cells beyond line
    // ends or below the last line retain old content from the double-buffer.
    // Clear to the canvas, not the terminal default: a plain Clear wipes the
    // theme background that frame::paint_canvas painted, so the chat would
    // ignore the theme while the rest of the frame showed it (#2163).
    f.render_widget(Clear, area);
    if let Some(bg) = super::theme::background() {
        f.render_widget(Block::default().style(Style::default().bg(bg)), area);
    }
    f.render_widget(chat, area);

    // Drag-selection highlight overlay: invert fg/bg for cells inside the
    // selection rectangle (reading-order), clipped to the chat area.
    if let (Some(a), Some(b)) = (app.drag_anchor, app.drag_current) {
        let (start, end) = if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        };
        let buf = f.buffer_mut();
        let x0 = area.x;
        let y0 = area.y;
        let x1 = area.x.saturating_add(area.width);
        let y1 = area.y.saturating_add(area.height);
        let highlight = Style::default().add_modifier(Modifier::REVERSED);
        if start.1 == end.1 {
            let y = start.1;
            if y >= y0 && y < y1 {
                let sx = start.0.max(x0);
                let ex = end.0.min(x1.saturating_sub(1));
                for x in sx..=ex {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(highlight);
                    }
                }
            }
        } else {
            for y in start.1..=end.1 {
                if y < y0 || y >= y1 {
                    continue;
                }
                let sx = if y == start.1 { start.0 } else { x0 };
                let ex = if y == end.1 {
                    end.0.min(x1.saturating_sub(1))
                } else {
                    x1.saturating_sub(1)
                };
                for x in sx.max(x0)..=ex {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(highlight);
                    }
                }
            }
        }
    }
}
