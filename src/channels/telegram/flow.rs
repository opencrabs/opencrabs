//! Telegram processing-log flow: the collapsed, edited-in-place block that
//! folds tool calls and intermediate narration into one growing message.
//!
//! Moved VERBATIM out of handler.rs (#471 phase 1, pure decomposition —
//! only visibility widened to pub(crate) so the handler glob re-export
//! keeps every existing call site and test import stable). Covers the
//! streaming state types, the three renderers (classic HTML, rich details,
//! rich markdown), and the flow operations (open/refresh/append/restick/
//! freeze/fold).

use crate::config::Config;
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::{MessageId, ParseMode};

use super::handler::{escape_html, format_inline, send_html_or_plain};
use super::state::TelegramState;

/// Individual tool call — each gets its own Telegram message.
pub(crate) struct ToolMsg {
    pub(crate) msg_id: Option<MessageId>,
    pub(crate) name: String,
    pub(crate) context: String,
    /// Raw, untruncated status source (the redacted bash command) for the
    /// flow's `#`-comment status extractor (#488). `context` is the decorated,
    /// middle-truncated display hint — wrong input for extraction because the
    /// wrapper hides a first-line `#` and truncation drops comments. Empty for
    /// non-bash tools.
    pub(crate) raw_context: String,
    /// None = running, Some(true) = success, Some(false) = failed
    pub(crate) completed: Option<bool>,
    pub(crate) dirty: bool,
    /// Detached-task identity (#2147). `Some` when this row was drawn from a
    /// `DetachedTaskStarted` event, so the matching finish finds it by id and
    /// edits this row instead of sending a second message. `None` for ordinary
    /// tool calls, which are keyed by name and position in the turn.
    pub(crate) task_id: Option<String>,
}

/// Per-message streaming state shared between the progress callback and the edit loop.
/// Each tool call gets its own message above; response streams in a separate message below.
/// Ordered display event — preserves chronological ordering of tools and intermediate texts.
#[derive(Clone)]
pub(crate) enum DisplayItem {
    /// New tool at this index in tool_msgs (needs send_message)
    NewTool(usize),
    /// Intermediate text between tool rounds, authored by the MODEL.
    Intermediate(String),
    /// Chrome authored HERE, not by the model: self-healing alerts, retry
    /// counters, provider switches. Renders identically to `Intermediate`
    /// and is kept apart only so it can never be mistaken for the turn's
    /// answer (#1253).
    System(String),
}

/// One entry in the in-place processing log (the growing `<blockquote
/// expandable>` message). Tool entries reference `tool_msgs` by index so a
/// status flip (⚙️ → ✅/❌) re-renders live; text entries hold the already
/// sanitized intermediate text (escaped at render time). Interleaving both in
/// one ordered flow lets tool calls and intermediate text share a single
/// collapsed block instead of each landing as a separate message.
#[derive(Clone, Debug)]
pub(crate) enum FlowEntry {
    /// A tool call at this index in `tool_msgs`.
    Tool(usize),
    /// Sanitized intermediate text authored by the MODEL (plain; escaped
    /// when rendered).
    Text(String),
    /// System chrome: self-healing alerts, retry counters, provider
    /// switches. Rendered exactly like `Text`, but never reclaimed as the
    /// turn's answer (#1253) — it is scaffolding, not a completion.
    System(String),
}

pub(crate) struct StreamingState {
    /// Whether this session's chat is a DM (owner-private). Drives scope-aware
    /// redaction: secrets show in DMs, scrub in group/channel chats (#677).
    pub(crate) is_dm: bool,
    /// Buffered `suggest_options` options (#724). The tool fires its event
    /// mid-turn, but the buttons must be the LAST thing in the chat, so we stash
    /// the options here and render them once, after the final delivery. Only the
    /// latest set is kept if the tool fires more than once.
    pub(crate) pending_suggestions:
        Option<Vec<crate::brain::tools::suggest_options::SuggestionItem>>,
    /// Trailing text reclaimed from the flow AFTER the option-surface Tool
    /// entry (#31): the model's post-halt sign-off iteration. Stashed by the
    /// options-pending reclaim in `deliver_final_response`, rendered by
    /// `render_suggestions` — rich: paragraph after the in-body button rows;
    /// classic: its own bubble. Keep-never-discard: the ack must neither leak
    /// into the answer bubble nor vanish with the flow block.
    pub(crate) pending_trailer: Option<String>,
    /// Response/thinking message (always at bottom)
    pub(crate) msg_id: Option<MessageId>,
    /// Reasoning/thinking text — streamed live, cleared before tool calls or response
    pub(crate) thinking: String,
    /// Each tool call = its own individual message
    pub(crate) tool_msgs: Vec<ToolMsg>,
    /// Ordered queue of new display items (tools + intermediates in chronological order)
    pub(crate) display_queue: Vec<DisplayItem>,
    /// Message ID of the open processing-log message. Tool calls and
    /// intermediate text are appended to this one message (edited in place) so
    /// the whole procedural trace of a turn collapses into a single growing
    /// `<blockquote expandable>` block instead of one message per event. Set
    /// when the first entry lands; stays open for the rest of the turn so the
    /// final response is the only clean message at the bottom.
    pub(crate) open_group_msg_id: Option<MessageId>,
    /// Consecutive transport failures on the open block's rich edit (#1323).
    /// A dropped socket is temporary, so the edit is retried on the next tick
    /// rather than downgrading the block to HTML for the rest of its life.
    /// Counted so an endpoint that is genuinely unreachable still reaches HTML
    /// instead of spinning; reset by the first edit that lands.
    pub(crate) rich_transport_failures: u8,
    /// Ordered entries in the open processing-log message (tool calls +
    /// intermediate text, in chronological order). Rendered together into the
    /// `open_group_msg_id` message on every append/status change.
    pub(crate) flow_entries: Vec<FlowEntry>,
    /// Live duration shown in the open block's header while the turn runs
    /// ("45s"), rendered as the trailing duration segment after the status
    /// message and count (#509). The block is the SINGLE progress surface
    /// (#360): no standalone status ticker exists while a block is open.
    /// HTML-safe (escaped at build time). None once the final response lands.
    pub(crate) flow_status: Option<String>,
    /// True when the open flow block lives on the rich API (#420 path A):
    /// edits must ride edit_rich_html; false = classic HTML blockquote.
    pub(crate) flow_rich: bool,
    /// Response text from streaming chunks — own message at bottom
    pub(crate) response: String,
    /// The bubble the FINAL response landed in (id + exact HTML), captured on
    /// the classic HTML delivery path so `suggest_options` can merge its
    /// keyboard onto the answer instead of posting a separate "Suggested
    /// next" message. Table-free rich deliveries capture their markdown;
    /// voice stays None. None = suggestions fall back to standalone.
    pub(crate) final_bubble: Option<super::state::MergeBubble>,
    pub(crate) dirty: bool,
    /// When true, the edit loop deletes the response message and creates a fresh one
    /// at the bottom of the chat (so it appears below tool/approval messages).
    pub(crate) recreate: bool,
    /// Pre-activity header preview (thinking excerpt / Working-on line),
    /// shown in the flow header while no flow entry yields an activity
    /// preview of its own. The flow message is the ONLY status surface: the
    /// legacy pre-block status bubble is gone, so early-turn status rides
    /// here from the first activity tick.
    pub(crate) header_preview: Option<String>,
    /// Auto-compaction in flight (#29): while set, `tick_flow_header` pins
    /// the header to `COMPACTING_HEADER_TEXT` regardless of the computed
    /// preview — no streaming chunks arrive during the silent window, so
    /// the ordinary preview would go stale. Cleared by the CompactionSummary
    /// arm; the next tick recomputes from live data.
    pub(crate) compacting: bool,
    /// Always-visible flow sections (plan title, checklist progress, active
    /// goal, ctx footer). Rolled by the edit loop from live data; ctx is set
    /// once at final delivery.
    pub(crate) sections: super::flow_chrome::FlowSections,
    /// Last active goal text sighted this turn (ADR 0005 Decision 10): the
    /// engine deletes the goal row when a plan task completes, so the chrome
    /// retains the text here and keeps the Goal section until settle. Per-turn
    /// state — a fresh StreamingState next turn drops any retained goal.
    pub(crate) retained_goal: Option<String>,
    /// Number of tool rounds completed (for display)
    pub(crate) tool_round_count: usize,
    /// When tool execution started (for elapsed time)
    pub(crate) tools_started_at: Option<std::time::Instant>,
    /// Instant the turn started (first user message), set once at construction
    /// and never reset — the wall-clock anchor for the header duration (#480),
    /// both live and settled. Distinct from `tools_started_at`, which is
    /// cleared on settle and re-armed per tool phase.
    pub(crate) turn_started_at: std::time::Instant,
    /// Terminal outcome once the turn ends, driving the settled block header
    /// (`✅ Finished (N tool calls, 45s)` / `❌ Failed` / `⏱ Timed out`, #480).
    /// `None` while the turn is live.
    pub(crate) flow_outcome: Option<FlowOutcome>,
    /// Background-work indicator for the settled footer (#1054): computed at
    /// settle time from `agent.background_manager().running_tasks(session_id)`
    /// — `Some("<label> running")` for one task, `Some("N tasks running")`
    /// for several, `None` when nothing is detached. Rendered as the final
    /// footer segment after the clock.
    pub(crate) bg_indicator: Option<String>,
    /// Background-task count at settle time (#1144): `None` when no manager is
    /// wired, `Some(n)` otherwise. Drives the settled-header override so a turn
    /// that ends with detached work reads "Waiting for N background task(s)"
    /// instead of "✅ Finished". Parallel to [`Self::bg_indicator`] (the footer
    /// label) but carries the numeric count the header needs.
    pub(crate) bg_count: Option<usize>,
    /// Alive sub-agent counts at settle time (#1183), from the session-scoped
    /// read of the (process-global) sub-agent manager: `working` vs `awaiting`
    /// collection. Zero while the turn is live; stamped once at settle next to
    /// [`Self::bg_count`] so the waiting header covers BOTH background
    /// registries — sub-agents live outside `BackgroundTaskManager`, which the
    /// pre-#1183 header read exclusively.
    pub(crate) subagent_counts: SubagentCounts,
    /// Intermediate texts already sent — used to dedup final response
    pub(crate) sent_intermediates: Vec<String>,
    /// Every intermediate bubble delivered to Telegram this turn, as
    /// (message id, text sent) pairs. The pairing is what makes cleanup
    /// possible: a cancelled in-flight call deletes these bubbles, and the
    /// final leg deletes the ones whose text the answer already delivers
    /// (#2156). The ids alone could not answer "which bubble said this", so
    /// a prefix duplicate had no address to remove.
    pub(crate) intermediate_bubbles: Vec<(MessageId, String)>,
    /// Local files the intermediate plane already put in the chat, each
    /// with the id of the bubble that carries it (#1918). The final leg
    /// consumes these paths before shipping its own scan, so a reply that
    /// repeats a link an intermediate already delivered does not put a
    /// second copy of the file in the chat, and links each surviving
    /// marker to the recorded bubble: the document's real address (#1939).
    pub(crate) delivered_files: Vec<super::delivery::DeliveredFile>,
    /// Rich intermediates whose media array carried documents, as
    /// (bubble id, text sent) pairs (#1939). The array is the only copy of
    /// those documents in the chat, so these ids stay out of
    /// `intermediate_bubbles` (deleting them would destroy the file) and a
    /// body they already carry must not be re-sent by the rich fallback.
    pub(crate) media_intermediates: Vec<(teloxide::types::MessageId, String)>,
    /// Message IDs of every voice note delivered to Telegram via `send_voice`
    /// (TTS responses to voice-input turns). This field exists purely as a
    /// load-bearing invariant: voice-reply IDs live here and MUST NEVER be
    /// iterated for deletion by any cleanup, cancellation, or rebuild path. If a
    /// future contributor adds a bulk cleanup over message IDs they have to
    /// consciously skip this field. The user's TTS voice note is the most
    /// expensive artefact to reproduce — it's a real synthesis call, not a
    /// cheap text render — so losing it to a sweep that "looked reasonable
    /// at the time" is a regression we've deliberately made hard to introduce.
    pub(crate) voice_msg_ids: Vec<MessageId>,
    /// Last plan keyboard actually applied to the open flow message via
    /// editMessageReplyMarkup (rich path only; the HTML edit path re-sends
    /// the markup inline on every edit).
    pub(crate) applied_plan_kb: super::flow_chrome::PlanKb,
    /// True from start until first response text arrives — enables rolling messages for CLI providers
    /// where tools complete instantly (ToolStarted+ToolCompleted back-to-back)
    pub(crate) processing: bool,
    /// True when the session's provider runs tools inside the CLI
    /// (`cli_handles_tools()`), i.e. claude-cli. CLI turns fold the whole model
    /// turn as intermediate narration into the block, so folded entries are
    /// capped to protect the 30K rich-block budget. API providers keep their
    /// answer in `response.content` and only fold brief interstitial narration,
    /// so they skip the per-entry cap and show full reasoning; the block-level
    /// 30K freeze still guards a pathological turn (#532 / upstream #531).
    pub(crate) is_cli: bool,
}

impl StreamingState {
    /// A turn with nothing on it yet.
    ///
    /// Every surface that shows a turn starts from this: the regular Telegram
    /// path and a guest reply differ in what they do with the state, not in
    /// what an empty turn holds. Anything a caller has to decide per chat is a
    /// parameter instead of a field each caller has to remember to set.
    /// `is_dm` picks the redaction scope (a chat we do not control never shows
    /// a credential), and `is_cli` the narration cap (#532: a CLI provider
    /// runs the tools itself, so its whole turn folds into the block, while an
    /// API provider shows full reasoning).
    pub(crate) fn new_turn(is_dm: bool, is_cli: bool) -> Self {
        Self {
            is_dm,
            compacting: false,
            pending_suggestions: None,
            pending_trailer: None,
            delivered_files: Vec::new(),
            media_intermediates: Vec::new(),
            msg_id: None,
            thinking: String::new(),
            tool_msgs: Vec::new(),
            display_queue: Vec::new(),
            open_group_msg_id: None,
            rich_transport_failures: 0,
            flow_entries: Vec::new(),
            flow_status: None,
            flow_rich: false,
            response: String::new(),
            final_bubble: None,
            dirty: false,
            recreate: false,
            header_preview: None,
            sections: Default::default(),
            retained_goal: None,
            applied_plan_kb: Default::default(),
            tool_round_count: 0,
            tools_started_at: Some(std::time::Instant::now()),
            turn_started_at: std::time::Instant::now(),
            flow_outcome: None,
            bg_indicator: None,
            bg_count: None,
            subagent_counts: Default::default(),
            sent_intermediates: Vec::new(),
            intermediate_bubbles: Vec::new(),
            voice_msg_ids: Vec::new(),
            processing: true,
            is_cli,
        }
    }
}

/// A resolved line in the processing-log flow, ready to render. Tool lines
/// carry the status label (icon + name) and context; text lines carry the
/// sanitized intermediate text. Both are HTML-escaped at render time.
pub(crate) enum FlowLine {
    Tool {
        label: String,
        /// Decorated display hint (`` (`cmd`) ``, middle-truncated).
        context: String,
        /// Raw command for `#`-comment status extraction (#488); empty for
        /// non-bash tools.
        raw_context: String,
    },
    Text(String),
}

/// Render resolved flow lines into final Telegram HTML. A lone tool line with
/// no other content stays a plain one-liner (mirrors #296); anything else
/// collapses into a single `<blockquote expandable>` block (Bot API 7.3+) that
/// renders with a tap-to-expand arrow in groups, DMs, and all official clients
/// with no rich-API dependency. Tool calls and intermediate text share the same
/// block so only the final response stays clean at the bottom (#300). Output is
/// final HTML — send via `send_html_or_plain`, never through
/// `markdown_to_telegram_html` (it would double-process the HTML).
/// Plain-text preview of the latest activity for the collapsed block header
/// (#405). Telegram's collapsed expandable blockquote shows the header plus the
/// first content line, and entries render chronologically — so without this a
/// long turn pins its FIRST narration line on screen forever while only the
/// header counters tick.
///
/// Priority chain: (1, #481 amended) the most recent HUMAN-READABLE
/// intermediary text, returned WHOLE — all paragraphs, newlines preserved, NOT
/// truncated — after skipping entries that are JSON, code blocks, or raw
/// output; (2, #482) line-start `#` comments from the most recent bash command
/// when there is no narration; (3) the most recent tool label + context. Each
/// renderer escapes/styles the returned text.
pub(crate) fn latest_activity_preview(lines: &[FlowLine]) -> Option<String> {
    // Priority 1: the whole most-recent human-readable intermediary text.
    if let Some(text) = lines.iter().rev().find_map(|l| match l {
        FlowLine::Text(t) => human_readable_preview(t),
        FlowLine::Tool { .. } => None,
    }) {
        return Some(text);
    }
    // Priority 2: line-start `#` comments from the most recent bash command
    // (the agent narrates its steps in the command itself, no separate text).
    // Reads raw_context, NOT the decorated/truncated display context (#488):
    // the wrapper prefix hides a first-line `#` and truncation drops comments.
    if let Some(comments) = lines.iter().rev().find_map(|l| match l {
        FlowLine::Tool {
            label, raw_context, ..
        } if is_bash_tool(label) => extract_status_from_text(raw_context),
        _ => None,
    }) {
        return Some(comments);
    }
    // Fallback: the most recent tool label + context.
    lines.iter().rev().find_map(|l| match l {
        FlowLine::Tool { label, context, .. } => Some(if context.is_empty() {
            label.clone()
        } else {
            format!("{label} {context}")
        }),
        FlowLine::Text(_) => None,
    })
}

/// A flow tool line is a bash call when its name (the last word of the
/// `{icon} {name}` label) is `bash`.
fn is_bash_tool(label: &str) -> bool {
    label.split_whitespace().last() == Some("bash")
}

/// Extract line-start `#` comments from a bash command as status text (#482).
/// A comment is a line whose first non-whitespace char is `#` — no shell-aware
/// parsing of inline `#` (amendment). The `#` and any `---`/`===` decoration
/// are stripped, so `# --- Setup environment ---` yields `Setup environment`.
/// Multiple comments join by newlines, untruncated (amendment). Shebang lines
/// (`#!`) are ignored. `None` when the command has no line-start comments.
pub(crate) fn extract_status_from_text(command: &str) -> Option<String> {
    let comments: Vec<String> = command
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('#') && !l.starts_with("#!"))
        .map(|l| {
            l.trim_start_matches('#')
                .trim()
                .trim_matches(|c: char| c == '-' || c == '=' || c.is_whitespace())
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .collect();
    (!comments.is_empty()).then(|| comments.join("\n"))
}

/// Return an intermediary text entry as a preview when it is human-readable
/// narration, else `None` so the caller skips backward to an earlier entry
/// (#481). Skips JSON (starts with `{`/`[`), code blocks (starts with a triple
/// backtick), and raw output (a single token with no internal whitespace — a
/// bare number or path). Human-readable text is returned WHOLE: trimmed, inline
/// markdown markers (`*`/`` ` ``/`_`) stripped so the preview never shows raw
/// source, newlines preserved, no truncation (amendment: whole intermediary
///
/// `pub(crate)` since #1809: the Slack flow line reuses this exact filter for
/// its live activity segment (channels/slack/tool_group.rs `latest_activity`),
/// the twin-file pattern reactions.rs already uses for `build_reaction_prompt`.
/// text as the status source).
pub(crate) fn human_readable_preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    // Raw output: one bare token, no internal whitespace, that reads as a path
    // or number (has a `/` or no letters at all) — e.g. `src/foo.rs` or `12345`.
    // A one-word sentence like `Done.` keeps its letters and is NOT raw.
    let looks_raw = !trimmed.chars().any(char::is_whitespace)
        && (trimmed.contains('/') || !trimmed.chars().any(char::is_alphabetic));
    if trimmed.is_empty()
        || trimmed.starts_with('{')
        || trimmed.starts_with('[')
        || trimmed.starts_with("```")
        || looks_raw
    {
        return None;
    }
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '_'))
        .collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

/// Longest folded narration entry kept in the collapsed flow block (#489).
/// The block is a PROGRESS view, not the full transcript: capping each
/// folded `Text` entry keeps the block compact so far more tool rounds fit
/// before the 30K rich size freeze. Matters most for Claude CLI, whose
/// answer streams as intermediate text folded into the block (API keeps it
/// in response.content). Display-only: the renderers read `flow_entries`
/// without mutating them, so `take_folded_final` still reclaims the FULL
/// final answer.
const FOLDED_NARRATION_CAP: usize = 300;

/// Per-entry cap for API providers. Their answer lives in `response.content`,
/// not folded text, so only brief interstitial narration folds into the block;
/// skipping the tight CLI cap keeps that reasoning readable. `usize::MAX` means
/// "no per-entry truncation" — the block-level 30K rich freeze remains the
/// guard against a pathological many-round turn (#532 / upstream #531).
const API_NARRATION_CAP: usize = usize::MAX;

/// The folded-narration cap for a turn: the tight [`FOLDED_NARRATION_CAP`] for
/// CLI providers (whose whole turn folds into the block and would fill the 30K
/// budget), or [`API_NARRATION_CAP`] (uncapped) for API providers.
fn narration_cap_for(is_cli: bool) -> usize {
    if is_cli {
        FOLDED_NARRATION_CAP
    } else {
        API_NARRATION_CAP
    }
}

/// Truncate a folded narration entry to `cap` chars on a char boundary,
/// appending an ellipsis when cut. Short entries (and any entry when
/// `cap == usize::MAX`) pass through unchanged.
fn cap_narration(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let capped: String = text.chars().take(cap).collect();
    format!("{capped}…")
}

// Channel code renders through the `_chrome` variants; these no-chrome
// wrappers stay as the test entry points pinning the renderer contract.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_flow_html(lines: &[FlowLine], live_status: Option<&str>) -> String {
    render_flow_html_with(lines, &FlowHeader::Live(live_status))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_flow_html_with(lines: &[FlowLine], header: &FlowHeader) -> String {
    render_flow_html_chrome(
        lines,
        header,
        None,
        &super::flow_chrome::FlowSections::default(),
        0,
    )
}

pub(crate) fn render_flow_html_chrome(
    lines: &[FlowLine],
    header: &FlowHeader,
    fallback_status: Option<&str>,
    sections: &super::flow_chrome::FlowSections,
    elapsed_secs: u64,
) -> String {
    render_flow_html_chrome_pref(
        lines,
        header,
        fallback_status,
        sections,
        FOLDED_NARRATION_CAP,
        elapsed_secs,
        None,
        false,
    )
}

/// Decompose the renderer's header / sections / activity into the merged-footer
/// inputs (ADR 0005 Decision 12), shared by the classic and rich paths so the
/// footer join can never drift between surfaces.
#[allow(clippy::too_many_arguments)] // one primitive per footer input; the
// decomposition IS the point (ADR 0005 Decision 12)
fn footer_parts<'a>(
    header: &'a FlowHeader,
    fallback_status: Option<&'a str>,
    sections: &'a super::flow_chrome::FlowSections,
    activity: Option<&'a str>,
    tool_count: usize,
    has_log: bool,
    elapsed_secs: u64,
    bg: Option<&'a str>,
) -> super::flow_chrome::FooterParts<'a> {
    let outcome = match header {
        FlowHeader::Settled { icon, verb, .. } => Some((*icon, *verb)),
        FlowHeader::Live(_) => None,
    };
    super::flow_chrome::FooterParts {
        outcome,
        plan_state: sections.plan_state.as_deref(),
        working_on: fallback_status,
        activity,
        tool_count,
        has_log,
        ctx: sections.ctx.as_deref(),
        elapsed_secs,
        bg,
    }
}

/// Build the flow-message body entries (tool + intermediate-text lines) shared
/// by the classic and rich renderers, plus the tool count. `Text` narration is
/// inline-formatted and capped per `narration_cap` (tight for CLI, uncapped for
/// API — #532).
fn flow_body_entries(lines: &[FlowLine], narration_cap: usize) -> (Vec<String>, usize) {
    let mut out: Vec<String> = Vec::new();
    let mut tool_count = 0usize;
    for line in lines {
        match line {
            FlowLine::Tool { label, context, .. } => {
                tool_count += 1;
                if context.is_empty() {
                    out.push(format!("<b>{}</b>", escape_html(label)));
                } else {
                    // Context (path / command / query) as monospace so it reads
                    // as code, not prose, inside the expanded block (#306).
                    out.push(format!(
                        "<b>{}</b> <code>{}</code>",
                        escape_html(label),
                        escape_html(context)
                    ));
                }
            }
            FlowLine::Text(text) => {
                let text = text.trim();
                if !text.is_empty() {
                    // Same inline markdown as the final completion so the
                    // expanded log is formatted, not raw source (#306); capped
                    // for CLI (#489), uncapped for API (#532). Display-only.
                    out.push(format_inline(&escape_html(&cap_narration(
                        text,
                        narration_cap,
                    ))));
                }
            }
        }
    }
    (out, tool_count)
}

/// [`flow_body_entries`] in the rich API's markdown dialect: same entries, same
/// narration cap, `**bold**` / backtick markup instead of `<b>` / `<code>`.
///
/// The rich API parses markdown itself, so the same list that renders inside an
/// HTML `<details>` body renders here, on the plane whose server-side parse
/// keeps pipe tables native (#679).
fn flow_body_entries_markdown(lines: &[FlowLine], narration_cap: usize) -> (Vec<String>, usize) {
    let mut out: Vec<String> = Vec::new();
    let mut tool_count = 0usize;
    for line in lines {
        match line {
            FlowLine::Tool { label, context, .. } => {
                tool_count += 1;
                if context.is_empty() {
                    out.push(format!("**{label}**"));
                } else {
                    out.push(format!("**{label}** `{context}`"));
                }
            }
            FlowLine::Text(text) => {
                let text = text.trim();
                if !text.is_empty() {
                    out.push(cap_narration(text, narration_cap));
                }
            }
        }
    }
    (out, tool_count)
}

/// Classic HTML flow message (ADR 0005): an uncollapsed shell, never one outer
/// expandable. The always-visible plan chrome (title / progress / goal) leads;
/// the processing log, when it has entries, sits in its own
/// `<blockquote expandable>` (full body, no summary line above it — Decision
/// 11/12); the merged footer is the plain final line (Decision 3).
/// `elapsed_secs` drives the footer clock (Decision 13).
#[allow(clippy::too_many_arguments)] // chrome-pref signature keeps render knobs explicit (#29 adds compacting)
pub(crate) fn render_flow_html_chrome_pref(
    lines: &[FlowLine],
    header: &FlowHeader,
    fallback_status: Option<&str>,
    sections: &super::flow_chrome::FlowSections,
    narration_cap: usize,
    elapsed_secs: u64,
    bg: Option<&str>,
    compacting: bool,
) -> String {
    let (out, tool_count) = flow_body_entries(lines, narration_cap);
    let has_log = !out.is_empty();
    // Dedupe (#29): during the silent window the newest log line IS the ⏳
    // body entry while the pinned header_preview carries the dedicated
    // compacting state — rendering both doubles the compaction text in the
    // footer (the owner-sighted live-fire bug). Suppress the activity-derived
    // segment while the flag is set; the pinned constant stays the sole
    // compaction string, and the #1052 split moves the gear onto the count.
    let activity = if compacting {
        None
    } else {
        latest_activity_preview(lines)
    };
    let footer = super::flow_chrome::merged_footer(
        &footer_parts(
            header,
            fallback_status,
            sections,
            activity.as_deref(),
            tool_count,
            has_log,
            elapsed_secs,
            bg,
        ),
        HeaderMarkup::Html,
    );
    // Always-visible plan chrome in the locked vertical order (title, prose
    // expandables, ☐/☑ checklist rows, goal — ADR 0005 Decision 3), assembled
    // for the classic Bot API HTML dialect (blank lines stand in for the rich
    // <hr> boundaries — Decision 13). Settled renders swap a completed goal's
    // icon to ✅ (Decision 10).
    let chrome = sections.chrome_classic(matches!(header, FlowHeader::Settled { .. }));

    let mut msg = String::new();
    if !chrome.is_empty() {
        msg.push_str(&chrome);
    }
    // A blank line separates any chrome above from the log/footer cluster
    // (Decision 13, classic uses blank lines). The log body is the full entry
    // list in one expandable with no summary line above it; the merged footer
    // is the plain final line under it.
    if has_log {
        if !msg.is_empty() {
            msg.push_str("\n\n");
        }
        msg.push_str(&format!(
            "<blockquote expandable>{}</blockquote>\n{footer}",
            out.join("\n\n")
        ));
    } else {
        if !msg.is_empty() {
            msg.push_str("\n\n");
        }
        msg.push_str(&footer);
    }
    msg
}

/// Render resolved flow lines as a `<details><summary>` collapsible for the
/// rich API's HTML input mode (#420 path A): the server parses it into a
/// native RichBlockDetails (summary/blocks/is_open), giving collapse parity
/// with the classic `<blockquote expandable>` PLUS the 32K rich limit, so
/// long tool chains stop splitting into multiple blocks. The summary carries
/// the live header (#360) and the latest-activity preview (#405); the
/// chronological log is the collapsed body. A lone tool line stays a plain
/// one-liner (mirrors #296, same as the HTML renderer).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_flow_details(lines: &[FlowLine], live_status: Option<&str>) -> String {
    render_flow_details_with(lines, &FlowHeader::Live(live_status))
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_flow_details_with(lines: &[FlowLine], header: &FlowHeader) -> String {
    render_flow_details_chrome(
        lines,
        header,
        None,
        &super::flow_chrome::FlowSections::default(),
        0,
    )
}

pub(crate) fn render_flow_details_chrome(
    lines: &[FlowLine],
    header: &FlowHeader,
    fallback_status: Option<&str>,
    sections: &super::flow_chrome::FlowSections,
    elapsed_secs: u64,
) -> String {
    render_flow_details_chrome_pref(
        lines,
        header,
        fallback_status,
        sections,
        FOLDED_NARRATION_CAP,
        elapsed_secs,
        None,
        false,
    )
}

/// Rich `<details>` flow message (#420 path A) reshaped for ADR 0005: an
/// uncollapsed shell, never one outer expandable. Always-visible plan chrome
/// leads as a `<p>` block; a `<p>&nbsp;</p>` spacer precedes the footer when
/// chrome is present (Decision 13); the merged footer is a `<sub>` line, and
/// when the log has entries that `<sub>` becomes the processing-log `<summary>`
/// with the full entry list as the collapsed body (Decision 12). `elapsed_secs`
/// drives the footer clock.
#[allow(clippy::too_many_arguments)] // same as html chrome-pref (#29 adds compacting)
pub(crate) fn render_flow_details_chrome_pref(
    lines: &[FlowLine],
    header: &FlowHeader,
    fallback_status: Option<&str>,
    sections: &super::flow_chrome::FlowSections,
    narration_cap: usize,
    elapsed_secs: u64,
    bg: Option<&str>,
    compacting: bool,
) -> String {
    let (out, tool_count) = flow_body_entries(lines, narration_cap);
    let has_log = !out.is_empty();
    // Dedupe (#29): same suppression as the classic renderer — the footer
    // join must never drift between surfaces (ADR 0005 Decision 12), so the
    // compacting flag kills the activity segment here too.
    let activity = if compacting {
        None
    } else {
        latest_activity_preview(lines)
    };
    let footer = super::flow_chrome::merged_footer(
        &footer_parts(
            header,
            fallback_status,
            sections,
            activity.as_deref(),
            tool_count,
            has_log,
            elapsed_secs,
            bg,
        ),
        HeaderMarkup::Html,
    );

    let mut msg = String::new();
    let chrome = sections.chrome_rich(matches!(header, FlowHeader::Settled { .. }));
    if !chrome.is_empty() {
        // Always-visible plan chrome in the locked vertical order: title flush
        // against the per-heading prose <details>, <hr> boundaries, ☐/☑
        // checklist rows, goal (ADR 0005 Decision 3/12/13); a kept spacer
        // follows before the footer.
        msg.push_str(&chrome);
        msg.push_str("<p>&nbsp;</p>");
    }
    if has_log {
        // The merged footer is the processing-log summary; the body is the full
        // <p>-wrapped entry list (one <p> per entry so the rich parser keeps
        // them separated).
        let body: String = out.iter().map(|e| format!("<p>{e}</p>")).collect();
        msg.push_str(&format!(
            "<details><summary><sub>{footer}</sub></summary>{body}</details>"
        ));
    } else {
        // No log yet: a plain <sub> footer line.
        msg.push_str(&format!("<sub>{footer}</sub>"));
    }
    msg
}

/// Render resolved flow lines into markdown for the rich API
/// (`sendRichMessage`). The rich API supports 32K chars (vs 4096 for HTML),
/// so long tool chains fit in a single message without splitting (#393).
/// Output is markdown — send via `send_rich_markdown` / `edit_rich_markdown`.
/// Falls back to `render_flow_html` on rich API failure.
///
/// The bare header-plus-entries render: no chrome, no footer, no collapse.
/// Every shipping surface now goes through
/// [`render_flow_rich_details_chrome_pref`] (via [`render_flow_rich_state`]),
/// so this stays as the primitive its tests pin.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_flow_rich(
    lines: &[FlowLine],
    live_status: Option<&str>,
    compacting: bool,
) -> String {
    render_flow_rich_with(lines, &FlowHeader::Live(live_status), compacting)
}

/// [`render_flow_rich`] with an explicit header.
///
/// The guest path needs the settled header on the markdown plane: a guest
/// reply is ONE message carrying the block and the answer, and the answer only
/// renders as a real grid there (#679: the HTML plane flattens tables).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_flow_rich_with(
    lines: &[FlowLine],
    header: &FlowHeader,
    compacting: bool,
) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut tool_count = 0usize;
    for line in lines {
        match line {
            FlowLine::Tool { label, context, .. } => {
                tool_count += 1;
                if context.is_empty() {
                    out.push(format!("**{label}**"));
                } else {
                    out.push(format!("**{label}** `{context}`"));
                }
            }
            FlowLine::Text(text) => {
                let text = text.trim();
                if !text.is_empty() {
                    out.push(text.to_string());
                }
            }
        }
    }
    if out.is_empty() {
        // Header-only render: the markdown path allows empty entries too so
        // the three renderers agree on the header-only contract.
        return flow_header_text(tool_count, header, None, HeaderMarkup::Markdown);
    }
    if let FlowHeader::Live(live_status) = header
        && out.len() == 1
        && tool_count == 1
    {
        // Lone tool line stays plain (#296); the live status rides on it so
        // the single surface still shows progress from the first call (#360).
        // A settled header always renders: its tool count and duration are the
        // point, so collapsing to the bare line would hide them.
        return match live_status {
            Some(st) => format!("{} • {}", out.remove(0), st),
            None => out.remove(0),
        };
    }
    // Same latest-activity preview as the HTML renderer (#405), leading the
    // header status-first (#509). Raw text, no escaping: the markdown dialect
    // keeps narration verbatim. Dedupe (#29): while the compacting flag is set
    // the newest line is the ⏳ body entry, so suppress it here too and the
    // three renderers never drift. A settled render drops it, matching the
    // HTML path.
    let status_msg = match header {
        FlowHeader::Live(_) if !compacting => latest_activity_preview(lines),
        _ => None,
    };
    let header_text = flow_header_text(
        tool_count,
        header,
        status_msg.as_deref(),
        HeaderMarkup::Markdown,
    );
    format!("{header_text}\n\n{}", out.join("\n\n"))
}

/// [`render_flow_details_chrome_pref`] on the MARKDOWN plane.
///
/// Same sections, same footer segments, same collapse: only the dialect
/// changes. The rich API's markdown parser reads `<details>` / `<summary>`
/// natively (`rich::parse`), so a guest reply keeps the collapsible processing
/// log AND the markdown answer whose pipe tables render as a real grid (#679).
/// The HTML renderer cannot be reused for it: its `<p>` / `<hr>` wrappers are
/// not part of the markdown dialect and would land as literal text.
#[allow(clippy::too_many_arguments)] // same shape as the HTML chrome-pref
pub(crate) fn render_flow_rich_details_chrome_pref(
    lines: &[FlowLine],
    header: &FlowHeader,
    fallback_status: Option<&str>,
    sections: &super::flow_chrome::FlowSections,
    narration_cap: usize,
    elapsed_secs: u64,
    bg: Option<&str>,
    compacting: bool,
) -> String {
    let (out, tool_count) = flow_body_entries_markdown(lines, narration_cap);
    let has_log = !out.is_empty();
    // Dedupe (#29): the compacting flag kills the activity segment here too, so
    // no surface can drift from the others (ADR 0005 Decision 12).
    let activity = if compacting {
        None
    } else {
        latest_activity_preview(lines)
    };
    let footer = super::flow_chrome::merged_footer(
        &footer_parts(
            header,
            fallback_status,
            sections,
            activity.as_deref(),
            tool_count,
            has_log,
            elapsed_secs,
            bg,
        ),
        HeaderMarkup::Markdown,
    );

    let mut msg = String::new();
    let chrome = sections.chrome_markdown(matches!(header, FlowHeader::Settled { .. }));
    if !chrome.is_empty() {
        msg.push_str(&chrome);
        msg.push_str("\n\n");
    }
    if has_log {
        // The merged footer is the summary; the body is the full entry list.
        // Blank lines between entries keep each one its own markdown block.
        msg.push_str(&format!(
            "<details><summary><sub>{footer}</sub></summary>\n\n{}\n\n</details>",
            out.join("\n\n")
        ));
    } else {
        // No log yet: a plain small-text footer line.
        msg.push_str(&format!("<sub>{footer}</sub>"));
    }
    msg
}

/// Resolve the flow into markdown for the rich API's markdown plane, with the
/// same live/settled header split as [`render_flow`] and the same chrome and
/// footer as [`render_flow_details_state`].
///
/// This is the guest surface's block: one message carries the block and the
/// answer, so the block has to be collapsible, carry the ctx / tok-s footer and
/// the plan chrome exactly like a regular chat's block does, while the answer
/// stays markdown so tables render. [`render_flow_rich_with`] renders the bare
/// header plus entries, which is what the older markdown path shipped; that is
/// why a guest block looked like a stripped-down copy of a real one.
pub(crate) fn render_flow_rich_state(s: &StreamingState) -> String {
    let narration_cap = narration_cap_for(s.is_cli);
    let elapsed = s.turn_started_at.elapsed().as_secs();
    match s.flow_outcome {
        Some(outcome) => {
            let (icon, verb) = settled_icon_verb(s.bg_count, s.subagent_counts, outcome);
            let duration = humanize_duration(elapsed);
            render_flow_rich_details_chrome_pref(
                &flow_lines(s),
                &FlowHeader::Settled {
                    icon,
                    verb: verb.as_str(),
                    duration: &duration,
                },
                None,
                &s.sections,
                narration_cap,
                elapsed,
                s.bg_indicator.as_deref(),
                false, // settled renders drop the activity segment regardless
            )
        }
        None => render_flow_rich_details_chrome_pref(
            &flow_lines(s),
            &FlowHeader::Live(s.flow_status.as_deref()),
            s.header_preview.as_deref(),
            &s.sections,
            narration_cap,
            elapsed,
            s.bg_indicator.as_deref(),
            s.compacting,
        ),
    }
}

/// Wall-clock duration for the flow-block header (#480): precise seconds under
/// a minute (`45s`), then `X min Ys` (`1 min 30s`, `5 min 0s`). Used for both
/// the live header and the settled outcome header, anchored at turn start.
/// Replaces `humanize_elapsed_coarse`: the block is the design, so a manually
/// expanded block collapsing on a header edit is a client-side (Desktop)
/// behavior we can't detect, and precise progress time is worth more.
pub(crate) fn humanize_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{} min {}s", secs / 60, secs % 60)
    }
}

/// Header text pinned for the whole silent window (#29). No percentage on
/// purpose: compaction progress is unknowable (one summarizer call), so a
/// number here would read as a fake progress bar. The START line carries the
/// fill level instead — that IS knowable.
pub(crate) const COMPACTING_HEADER_TEXT: &str = "⏳ Compacting context…";

/// Flow-block body line posted when auto-compaction starts (#29). The
/// percentage is the context FILL LEVEL at trigger time — a "how full was
/// it" fact, never a progress indicator. The ETA hint, when present, is the
/// duration the session's PREVIOUS compaction actually took (E2) — grounded
/// in observation, never a static guess: the old `≈10–60s` constant was
/// retired because it was never right (observed live range 10s → 29 min),
/// so a session with no compaction history renders no parenthetical at all.
pub(crate) fn compacting_flow_line(
    usage_pct: f64,
    predicted: Option<std::time::Duration>,
) -> String {
    match predicted {
        Some(d) => format!(
            "⏳ Compacting context — {:.0}% full (≈{})…",
            usage_pct,
            humanize_duration(d.as_secs().max(1))
        ),
        None => format!("⏳ Compacting context — {:.0}% full…", usage_pct),
    }
}

/// Flow-block body line posted when compaction finishes (#29). Doubles as
/// the definitive completion signal: arrival means the silent window is
/// over, so a FAILED compaction never emits one (no false ✅).
pub(crate) fn compacted_flow_line(
    before_pct: f64,
    after_pct: f64,
    elapsed: std::time::Duration,
) -> String {
    format!(
        "✅ Compacted: {:.0}% → {:.0}% in {}",
        before_pct,
        after_pct,
        humanize_duration(elapsed.as_secs().max(1))
    )
}

/// The terminal state of a turn and its settled-header icon+verb live in the
/// shared [`crate::channels::background_work`] module (#480, lifted there for
/// the Discord port in #1911 so both surfaces read one vocabulary). They are
/// re-exported here so the flow renderer keeps its path, next to the
/// waiting-verb decision the same module owns (#1985).
pub(crate) use crate::channels::background_work::{FlowOutcome, SubagentCounts, waiting_verb};

/// Settled header icon+verb, overridden to a waiting state when the turn
/// finished with background work still alive (#1144, #1183). A settled card
/// that ends with detached shell tasks reads "✅ Finished" up top and "N tasks
/// running" in the footer — the exact split #1144 fixed — and #1183 extended
/// the same override to sub-agents, which live in a separate registry the
/// background-task count never read: a turn ending with two agents mid-work
/// still said "Finished". The verb folds both registries, e.g. "Waiting for
/// 1 background task + 2 working agents"; the folding itself is the shared
/// [`waiting_verb`], this wrapper only pairs it with the Telegram icon. The
/// icon is a static ref; the verb is an owned [`String`] because it carries
/// the counts, so callers pass `verb.as_str()` into [`FlowHeader::Settled`].
pub(crate) fn settled_icon_verb(
    bg_count: Option<usize>,
    agents: SubagentCounts,
    outcome: FlowOutcome,
) -> (&'static str, String) {
    if outcome == FlowOutcome::Finished
        && let Some(verb) = waiting_verb(bg_count, agents)
    {
        return ("⏳", verb);
    }
    let (icon, verb) = outcome.icon_verb();
    (icon, verb.to_string())
}

/// How the block header renders: live during a turn, or settled to a terminal
/// outcome at the end (#480). The shared [`flow_header_text`] turns this plus
/// the tool count into the header string every renderer wraps.
pub(crate) enum FlowHeader<'a> {
    /// Turn in progress; the payload is the elapsed duration. With a status
    /// message the header reads `⚙️ {status} • N tool calls • {duration}` (#509);
    /// with neither status nor duration it is the plain `N tool calls` /
    /// `Processing log`.
    Live(Option<&'a str>),
    /// Turn settled: `{icon} {verb} (N tool calls, {duration})`, dropping the
    /// `N tool calls` clause when no tools ran.
    Settled {
        icon: &'a str,
        verb: &'a str,
        duration: &'a str,
    },
}

/// Inline-markup dialect for the header: the classic/rich-details HTML paths
/// want `<b>`/`<i>`, the rich-markdown path wants `**`/`_`. Kept here so the
/// bold/italic emphasis is applied inside the shared builder and the three
/// renderers can never disagree on where the emphasis falls.
#[derive(Clone, Copy)]
pub(crate) enum HeaderMarkup {
    Html,
    Markdown,
}

impl HeaderMarkup {
    pub(crate) fn bold(self, s: &str) -> String {
        match self {
            HeaderMarkup::Html => format!("<b>{s}</b>"),
            HeaderMarkup::Markdown => format!("**{s}**"),
        }
    }

    pub(crate) fn italic(self, s: &str) -> String {
        match self {
            HeaderMarkup::Html => format!("<i>{s}</i>"),
            HeaderMarkup::Markdown => format!("_{s}_"),
        }
    }
}

/// Strip a leading running-gear (`⚙️`, base `⚙` plus its optional variation
/// selector) and any following whitespace from a status message, so the live
/// header's own gear is never doubled when the status is the bare-tool fallback
/// (#509 follow-up). Text that does not start with a gear is returned unchanged.
fn strip_leading_gear(s: &str) -> &str {
    s.trim_start_matches(['⚙', '\u{fe0f}']).trim_start()
}

/// True when `s` starts with an icon glyph (emoji / symbol / dingbat) rather
/// than a word character. The standing `⚙️` chrome prefix is dropped before
/// such text — the gear never fronts another icon (#29 fix round, owner
/// directive). Detection is deliberately simple: a non-ASCII first char that
/// is not alphanumeric reads as an icon — covers ⏳ ✅ ❌ ⚙ ⏱ ❕ and friends
/// while Latin, Cyrillic, and CJK text keeps the gear.
pub(crate) fn starts_with_icon(s: &str) -> bool {
    match s.chars().next() {
        Some(c) => !c.is_ascii() && !c.is_alphanumeric(),
        None => false,
    }
}

/// Build the fully-styled header shared by all three renderers so the classic
/// HTML, rich-details, and rich-markdown headers can never drift (#480, #509).
/// The live header leads with the status message (bold), then the tool-call
/// count, then the duration (both italic), `•`-separated: `⚙️ status • count •
/// duration`. `status_msg` is the activity preview the renderer already escapes
/// for its own dialect; the `Live` payload carries the duration. The
/// plain-count (nothing running yet) and settled headers stay fully bold.
pub(crate) fn flow_header_text(
    tool_count: usize,
    header: &FlowHeader,
    status_msg: Option<&str>,
    markup: HeaderMarkup,
) -> String {
    let base = if tool_count > 0 {
        format!("{tool_count} tool calls")
    } else {
        "Processing log".to_string()
    };
    match header {
        FlowHeader::Live(duration) => {
            // No status message and no elapsed duration → the just-started case:
            // the plain bold count / "Processing log", no gear (#509).
            if status_msg.is_none() && duration.is_none() {
                return markup.bold(&base);
            }
            // Ordered live header: status message FIRST (bold), then the count,
            // then the duration (both italic), all `•`-separated (#509).
            let mut segs: Vec<String> = Vec::new();
            let mut icon_led = false;
            if let Some(status) = status_msg {
                // The header already prints one live gear; strip a leading
                // running-gear from the status message so the bare-tool fallback
                // ("⚙️ bash …") does not render a double gear (#509 follow-up).
                // Settled ✅/❌ tool icons are left alone: they read as the tool's
                // own outcome, not a duplicate of the header gear.
                let status = strip_leading_gear(status);
                icon_led = starts_with_icon(status);
                segs.push(markup.bold(status));
            }
            segs.push(markup.italic(&base));
            if let Some(dur) = duration {
                segs.push(markup.italic(dur));
            }
            // The standing gear is dropped when the status already leads with
            // an icon (#29 fix round, owner directive) — the pinned
            // `⏳ Compacting context…` header and icon-led tool activity render
            // bare, never `⚙️ ⏳ …`.
            if icon_led {
                segs.join(" • ")
            } else {
                format!("⚙️ {}", segs.join(" • "))
            }
        }
        FlowHeader::Settled {
            icon,
            verb,
            duration,
        } => {
            let text = if tool_count > 0 {
                format!("{icon} {verb} ({tool_count} tool calls, {duration})")
            } else {
                format!("{icon} {verb} ({duration})")
            };
            markup.bold(&text)
        }
    }
}

/// Status glyph for a tool call: running, succeeded, or failed.
pub(crate) fn tool_status_icon(completed: Option<bool>) -> &'static str {
    match completed {
        None => "⚙️",
        Some(true) => "✅",
        Some(false) => "❌",
    }
}

/// The name a row shows. A detached task says so, and says whether it is
/// still running, because a background task outlives the turn that started
/// it: the row is the only place its liveness is visible (#2147).
///
/// The wording lives in [`crate::channels::background_work::detached_row_label`]
/// so every surface renders the same string (#2148-#2150).
pub(crate) fn row_display_name(
    name: &str,
    task_id: Option<&str>,
    completed: Option<bool>,
) -> String {
    crate::channels::background_work::detached_row_label(name, task_id, completed)
}

/// Resolve the open processing-log flow (tool calls + intermediate text, in
/// order) into renderable lines.
pub(crate) fn flow_lines(s: &StreamingState) -> Vec<FlowLine> {
    s.flow_entries
        .iter()
        .filter_map(|entry| match entry {
            FlowEntry::Tool(idx) => s.tool_msgs.get(*idx).map(|t| FlowLine::Tool {
                label: format!(
                    "{} {}",
                    tool_status_icon(t.completed),
                    row_display_name(&t.name, t.task_id.as_deref(), t.completed)
                ),
                context: t.context.clone(),
                raw_context: t.raw_context.clone(),
            }),
            FlowEntry::Text(text) | FlowEntry::System(text) => Some(FlowLine::Text(text.clone())),
        })
        .collect()
}

/// Resolve the flow into final Telegram HTML. Live turn → the plain live
/// header; a settled turn → the terminal outcome header with wall-clock
/// duration from turn start (#480).
pub(crate) fn render_flow(s: &StreamingState) -> String {
    let narration_cap = narration_cap_for(s.is_cli);
    let elapsed = s.turn_started_at.elapsed().as_secs();
    match s.flow_outcome {
        Some(outcome) => {
            let (icon, verb) = settled_icon_verb(s.bg_count, s.subagent_counts, outcome);
            let duration = humanize_duration(elapsed);
            render_flow_html_chrome_pref(
                &flow_lines(s),
                &FlowHeader::Settled {
                    icon,
                    verb: verb.as_str(),
                    duration: &duration,
                },
                None,
                &s.sections,
                narration_cap,
                elapsed,
                s.bg_indicator.as_deref(),
                false, // settled renders drop the activity segment regardless
            )
        }
        None => render_flow_html_chrome_pref(
            &flow_lines(s),
            &FlowHeader::Live(s.flow_status.as_deref()),
            s.header_preview.as_deref(),
            &s.sections,
            narration_cap,
            elapsed,
            s.bg_indicator.as_deref(),
            s.compacting,
        ),
    }
}

/// Resolve the flow into the rich-API details HTML (#420 path A), with the same
/// live/settled header split as [`render_flow`].
pub(crate) fn render_flow_details_state(s: &StreamingState) -> String {
    let narration_cap = narration_cap_for(s.is_cli);
    let elapsed = s.turn_started_at.elapsed().as_secs();
    match s.flow_outcome {
        Some(outcome) => {
            let (icon, verb) = settled_icon_verb(s.bg_count, s.subagent_counts, outcome);
            let duration = humanize_duration(elapsed);
            render_flow_details_chrome_pref(
                &flow_lines(s),
                &FlowHeader::Settled {
                    icon,
                    verb: verb.as_str(),
                    duration: &duration,
                },
                None,
                &s.sections,
                narration_cap,
                elapsed,
                s.bg_indicator.as_deref(),
                false, // settled renders drop the activity segment regardless
            )
        }
        None => render_flow_details_chrome_pref(
            &flow_lines(s),
            &FlowHeader::Live(s.flow_status.as_deref()),
            s.header_preview.as_deref(),
            &s.sections,
            narration_cap,
            elapsed,
            s.bg_indicator.as_deref(),
            s.compacting,
        ),
    }
}

/// Re-render the open processing-log flow and edit its message in place. Used
/// after appending an entry and after a tool status flip (⚙️ → ✅/❌). A no-op
/// edit ("message is not modified") and transient errors are ignored: the
/// message already shows the correct content and the next tick retries.
/// Edits ride the surface the block was opened on: rich details (#420 path
/// A) with classic-HTML fallback, or the classic HTML path directly.
pub(crate) async fn refresh_flow(
    bot: &Bot,
    chat: ChatId,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    class: super::governor::EditClass,
) {
    let (mid, rich) = {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        match s.open_group_msg_id {
            Some(mid) => (mid, s.flow_rich),
            None => return,
        }
    };
    if rich {
        refresh_flow_rich_details(bot, chat, mid, streaming, class).await;
    } else {
        refresh_flow_html(bot, chat, mid, streaming, class).await;
    }
}

/// How many consecutive transport failures a rich block absorbs before giving
/// up and downgrading to HTML (#1323).
///
/// The edit loop ticks about every 1.5s, so this is a couple of seconds of
/// unreachability. Long enough to ride out the socket-exhaustion blips that
/// produced the measured downgrades, short enough that a genuinely dead
/// endpoint still delivers something promptly.
pub(crate) const MAX_RICH_TRANSPORT_RETRIES: u8 = 3;

/// How a failed rich-details edit should be recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RichEditError {
    /// Telegram reports the content is unchanged — nothing to do.
    NotModified,
    /// A 429 rate limit — skip and retry rich next tick (never fall back to
    /// HTML, whose smaller limit would split the block; #580).
    RateLimited,
    /// The request never reached Telegram: a dropped connection, a socket the
    /// OS would not allocate, DNS failing. Temporary, like a 429, and for the
    /// same reason must not fall back — but bounded, so an endpoint that is
    /// genuinely unreachable still ends up delivering something (#1323).
    Transport,
    /// Any other failure — fall back to the HTML edit path.
    Fallback,
}

/// Classify a rich-edit error string into a recovery action. Kept pure so the
/// rate-limit-vs-fallback decision is unit-testable without a live Bot API.
pub(crate) fn classify_rich_edit_error(msg: &str) -> RichEditError {
    if msg.contains("message is not modified") {
        RichEditError::NotModified
    } else if msg.contains("429") || msg.contains("Too Many Requests") {
        RichEditError::RateLimited
    } else if is_transport_failure(msg) {
        RichEditError::Transport
    } else {
        RichEditError::Fallback
    }
}

/// Whether a failure happened below HTTP: the request never reached Telegram,
/// so no response exists and the content was never judged.
///
/// `reqwest` raises "error sending request for url (…)" for the whole class,
/// which is why that phrase carries the decision. The others are the shapes
/// seen when a connection is established and then lost mid-flight.
///
/// This is deliberately a small allowlist rather than "anything not
/// recognised". A malformed-content error is a permanent property of the
/// message and must keep falling back to HTML; treating the unknown as
/// transient would turn every real content bug into a retry loop.
pub(crate) fn is_transport_failure(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("error sending request")
        || m.contains("connection closed")
        || m.contains("connection reset")
        || m.contains("connection refused")
        || m.contains("broken pipe")
        || m.contains("dns error")
        || m.contains("operation timed out")
}

/// Rich-details edit path (#420 path A). 32K char limit, 30K freeze
/// threshold. A not-modified response is a no-op; a 429 is retried on the rich
/// path next tick; any other failure falls back to the classic HTML edit so the
/// block never silently stops updating.
///
/// G2 flood governor (#1211): `class` places this edit on the priority drop
/// ladder. Chrome classes are dropped (counted, self-healing on the next
/// full-state render) when the per-forum edit bucket is empty; the `Final`
/// class queues the payload latest-wins instead of dropping it, and the
/// governor's drainer lands it on refill.
pub(crate) async fn refresh_flow_rich_details(
    bot: &Bot,
    chat: ChatId,
    mid: MessageId,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    class: super::governor::EditClass,
) {
    let details = {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        render_flow_details_state(&s)
    };
    if details.is_empty() {
        return;
    }
    if details.chars().count() > 30000 {
        freeze_flow_block_and_strip_kb(bot, chat, streaming, mid, "rich size limit reached").await;
        return;
    }
    if !super::governor::edit_admission(bot, chat, mid, class, details.clone(), true).await {
        return;
    }
    match super::rich::api::edit_rich_html(
        bot.api_url().as_str(),
        bot.token(),
        chat.0,
        mid.0,
        &details,
        None,
        "turn",
        "-",
    )
    .await
    {
        // The plan Approve/Discard keyboard now rides the persistent plan card,
        // not the flow block (#580), so the flow block carries no reply_markup.
        Ok(_) => {
            // The block is reachable again; forget the transport streak so a
            // later blip gets its own full budget.
            streaming
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .rich_transport_failures = 0;
        }
        Err(e) => match classify_rich_edit_error(&e.to_string()) {
            RichEditError::NotModified => {}
            // A transient 429 must NOT fall back to HTML. The rich API holds 32K
            // chars; the HTML path caps at 4096, so a large block that fits rich
            // gets frozen and split into a second message on the smaller HTML
            // limit — that is what split a long plan-execution block into "…23
            // tool calls" + a separate "Finished" block (#580). Skip this tick
            // instead: the ~1.5s edit loop retries the rich edit, and once the
            // rate limit clears the block updates whole, never splitting.
            RichEditError::RateLimited => {
                tracing::warn!(
                    "Telegram: rich details edit rate-limited for mid={:?} — skipping this tick, \
                     will retry rich (not HTML, to avoid a size-split)",
                    mid
                );
            }
            // The request never reached Telegram, so the content was never
            // judged: nothing about the message is known to be wrong. Treat it
            // like a 429 and retry rich on the next tick, because downgrading
            // a block permanently over a briefly unavailable socket is the
            // same bad trade #580 rejected for rate limits. Bounded, so an
            // endpoint that is genuinely unreachable still delivers through
            // HTML rather than retrying forever.
            RichEditError::Transport => {
                let streak = {
                    let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                    s.rich_transport_failures = s.rich_transport_failures.saturating_add(1);
                    s.rich_transport_failures
                };
                if streak <= MAX_RICH_TRANSPORT_RETRIES {
                    tracing::warn!(
                        "Telegram: rich details edit hit a transport failure for mid={:?}                          (attempt {}/{}) — retrying rich next tick, not downgrading",
                        mid,
                        streak,
                        MAX_RICH_TRANSPORT_RETRIES
                    );
                } else {
                    tracing::warn!(
                        "Telegram: rich details edit transport failures exhausted for mid={:?}                          after {} attempts — falling back to HTML",
                        mid,
                        MAX_RICH_TRANSPORT_RETRIES
                    );
                    refresh_flow_html(bot, chat, mid, streaming, class).await;
                }
            }
            // Non-rate-limit errors (malformed content, etc.) fall back to HTML,
            // which is the right recovery there.
            RichEditError::Fallback => {
                tracing::warn!(
                    "Telegram: rich details edit failed for mid={:?}: {} — falling back to HTML",
                    mid,
                    e
                );
                refresh_flow_html(bot, chat, mid, streaming, class).await;
            }
        },
    }
}

/// HTML edit path for the processing-log flow. 4096-char limit.
///
/// G2 flood governor (#1211): same ladder contract as the rich path above —
/// `class` decides whether an empty per-forum edit bucket drops (chrome,
/// self-healing) or queues (finals, latest-wins).
pub(crate) async fn refresh_flow_html(
    bot: &Bot,
    chat: ChatId,
    mid: MessageId,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    class: super::governor::EditClass,
) {
    let html = {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        render_flow(&s)
    };
    if html.is_empty() {
        return;
    }
    // Proactive freeze: past Telegram's 4096-char edit limit the edit can
    // only fail. Keep the message as last rendered and start a new block.
    if html.chars().count() > 4000 {
        freeze_flow_block_and_strip_kb(bot, chat, streaming, mid, "size limit reached").await;
        return;
    }
    // The plan Approve/Discard keyboard now rides the persistent plan card, not
    // the flow block (#580), so no reply_markup is attached here.
    if !super::governor::edit_admission(bot, chat, mid, class, html.clone(), false).await {
        return;
    }
    let req = bot
        .edit_message_text(chat, mid, html)
        .parse_mode(ParseMode::Html);
    match req.await {
        Ok(_) => {}
        // Transient rate limit: wait it out and retry once with fresh
        // content. Deleting here used to wipe a fully rendered report off
        // the screen over a 9-second throttle (#356).
        Err(teloxide::RequestError::RetryAfter(secs)) => {
            super::rate_limit::wait_out(
                "refresh_flow",
                secs.duration(),
                &format!(" for mid={mid:?}, then retrying"),
                Some(chat.0),
            )
            .await;
            let retry_html = {
                let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                if s.open_group_msg_id != Some(mid) {
                    return; // block closed/replaced while waiting
                }
                render_flow(&s)
            };
            if let Err(e) = bot
                .edit_message_text(chat, mid, retry_html)
                .parse_mode(ParseMode::Html)
                .await
            {
                tracing::warn!(
                    "Telegram: refresh_flow retry failed for mid={:?}: {} — keeping message, next tick retries",
                    mid,
                    e
                );
            }
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("message is not modified") {
                // Content already correct — nothing to do.
            } else if msg.contains("MESSAGE_TOO_LONG") {
                freeze_flow_block_and_strip_kb(bot, chat, streaming, mid, "MESSAGE_TOO_LONG").await;
            } else if msg.contains("message to edit not found") {
                // Genuinely gone (deleted externally) — forget the id.
                tracing::warn!(
                    "Telegram: refresh_flow target mid={:?} no longer exists — starting a new block",
                    mid
                );
                let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                if s.open_group_msg_id == Some(mid) {
                    s.open_group_msg_id = None;
                    s.flow_entries.clear();
                }
            } else {
                // Parse error or anything else: NEVER delete displayed
                // content over a failed update — the message still shows the
                // last successful render and the next tick retries (#356).
                tracing::warn!(
                    "Telegram: refresh_flow edit failed for mid={:?}: {} — keeping message",
                    mid,
                    e
                );
            }
        }
    }
}

/// A mid-turn user follow-up landed (queued-message injection). Since #475 the
/// open processing-log block is NOT closed or frozen and its entries are NOT
/// dropped: the block stays open with its content visible above the follow-up,
/// and only the response placeholder is marked for re-post. #451's restick then
/// relocates the SAME block below the newest message on the next tool round, so
/// the chat keeps flowing bottom-down with one block per turn (#404, #475).
pub(crate) fn detach_flow_for_followup(streaming: &Arc<std::sync::Mutex<StreamingState>>) {
    // The block is NOT closed here (#475). The original #404 freeze existed
    // to make the next round appear below the user's follow-up, but in a
    // busy group every incoming message froze the block and shredded one
    // turn into a dozen fragments. #451's restick achieves the ordering the
    // freeze was for — the SAME block relocates below the newest message on
    // the next round — so grouping survives: one block per turn, always at
    // the bottom. Only the response placeholder re-posts.
    let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
    if s.open_group_msg_id.is_some() {
        tracing::info!(
            "Telegram: mid-turn follow-up — flow block stays open; restick moves it \
             below on the next round (#475)"
        );
    }
    if s.msg_id.is_some() {
        s.recreate = true;
    }
}

pub(crate) fn freeze_flow_block(
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    mid: MessageId,
    reason: &str,
) {
    tracing::info!(
        "Telegram: freezing processing-log block mid={:?} ({reason}) — content stays visible, next entries start a new block",
        mid
    );
    let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
    if s.open_group_msg_id == Some(mid) {
        s.open_group_msg_id = None;
        s.flow_entries.clear();
    }
}

/// Freeze the current flow block AND strip any plan keyboard from the sealed
/// prior message, so only the next live flow message can own Approve/Discard
/// (ADR 0005 Decision 6). [`freeze_flow_block`] is sync with no bot handle, so
/// the keyboard strip lives here where `bot`/`chat` are in scope. No-op strip
/// when the message never carried a plan keyboard (the common non-plan freeze).
async fn freeze_flow_block_and_strip_kb(
    bot: &Bot,
    chat: ChatId,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    mid: MessageId,
    reason: &str,
) {
    let had_kb = {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        s.sections.plan_kb != super::flow_chrome::PlanKb::None
            || s.applied_plan_kb != super::flow_chrome::PlanKb::None
    };
    freeze_flow_block(streaming, mid, reason);
    if had_kb {
        // Omitting reply_markup clears the inline keyboard on the sealed message.
        if let Err(e) = bot.edit_message_reply_markup(chat, mid).await {
            tracing::debug!("Failed to strip plan keyboard from frozen flow {mid}: {e}");
        }
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        s.applied_plan_kb = super::flow_chrome::PlanKb::None;
    }
}

/// Send the open processing-log message for the first time and record its id.
/// A newly landed message re-posts the streaming placeholder next tick so the
/// response stays at the bottom (the only flow-driven recreate; subsequent
/// entries merely edit this message in place — #299).
/// When `rich_messages` is enabled, sends via the rich API (32K limit) with
/// HTML fallback (#393).
pub(crate) async fn open_flow(
    bot: &Bot,
    chat: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
) {
    // Already open: edit in place, never post a second block.
    //
    // This function's contract is "send the open processing-log message for
    // the FIRST time", and nothing enforced it. append_tool_group guarded at
    // its call site, but the chrome tick calls open_flow directly on every
    // activity update, so a long turn posted a new block per tick. Once plan
    // title, prose and checklist moved to the card, the only content left was
    // the elapsed time, so the chat filled with timer-only bubbles seconds
    // apart. Guarding here covers every caller instead of relying on each one
    // to remember.
    let already_open = {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        s.open_group_msg_id.is_some()
    };
    if already_open {
        refresh_flow(bot, chat, streaming, super::governor::EditClass::Status).await;
        return;
    }

    // Rich-first WITH collapse parity (#420 path A): the flow renders as a
    // <details><summary> collapsible through the rich API's HTML input mode
    // (native RichBlockDetails, 32K limit — no block splitting). Any rich
    // failure falls back to the classic HTML <blockquote expandable> path,
    // which stays the proven baseline (#421: the markdown-input rich path
    // shipped flat, with no collapse at all, and was reverted).
    if Config::current().channels.telegram.rich_messages {
        let details = {
            let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
            render_flow_details_state(&s)
        };
        if !details.is_empty() {
            // G3 send pacing (#1211): opening the flow posts a new message.
            super::governor::pace_send(chat).await;
            match super::rich::api::send_rich_html_id(
                bot.api_url().as_str(),
                bot.token(),
                chat.0,
                thread_id,
                &details,
                None,
                "turn",
                "-",
            )
            .await
            {
                Ok(mid) => {
                    let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                    s.open_group_msg_id = Some(MessageId(mid));
                    s.flow_rich = true;
                    if s.msg_id.is_some() {
                        s.recreate = true;
                    }
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        "Telegram: rich details flow open failed: {e} — falling back to HTML"
                    );
                }
            }
        }
    }
    let html = {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        render_flow(&s)
    };
    if html.is_empty() {
        return;
    }
    if let Ok(mid) = send_html_or_plain(bot, chat, thread_id, &html, "turn", None).await {
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        s.open_group_msg_id = Some(mid);
        s.flow_rich = false;
        if s.msg_id.is_some() {
            s.recreate = true;
        }
    }
}

/// Append buffered tool calls to the open processing-log flow, editing that one
/// message in place (or opening it if none is live yet) so consecutive tool
/// calls collapse into a single growing block.
pub(crate) async fn append_tool_group(
    bot: &Bot,
    chat: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    buffer: &[usize],
) {
    if buffer.is_empty() {
        return;
    }
    let open = {
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        for &idx in buffer {
            s.flow_entries.push(FlowEntry::Tool(idx));
        }
        s.open_group_msg_id
    };
    if open.is_some() {
        // Tag the tools with the open message so status flips find them.
        {
            let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
            let mid = s.open_group_msg_id;
            for &idx in buffer {
                if let Some(tool) = s.tool_msgs.get_mut(idx) {
                    tool.msg_id = mid;
                }
            }
        }
        refresh_flow(
            bot,
            chat,
            streaming,
            super::governor::EditClass::Intermediary,
        )
        .await;
    } else {
        open_flow(bot, chat, thread_id, streaming).await;
        let mid = streaming
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .open_group_msg_id;
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        for &idx in buffer {
            if let Some(tool) = s.tool_msgs.get_mut(idx) {
                tool.msg_id = mid;
            }
        }
    }
}

/// Stable key for a line that is *progress on one condition* rather than a new
/// event (#982).
///
/// Repeated progress used to stack: five `nudge 1/5 .. 5/5` lines and one line
/// per fallback attempt, nine messages for two facts. Everything else in this
/// block already rewrites itself, a tool row flips its icon and the footer
/// rewrites ctx and tok/s, so these were the exception. Lines sharing a key
/// supersede each other; ordinary narration returns `None` and always appends.
pub(crate) fn progress_key(text: &str) -> Option<&'static str> {
    let t = text.trim_start_matches(|c: char| !c.is_alphanumeric());
    if t.starts_with("Model reasoned without answering") {
        Some("empty-answer-nudge")
    } else if t.starts_with("Trying fallback") {
        Some("fallback-attempt")
    } else if t.starts_with("Retry ") {
        Some("provider-retry")
    } else if t.starts_with("Mermaid render failed") {
        // The regen-nudge counter (#37): 1/3 → 2/3 → 3/3 supersedes in
        // place like the empty-answer nudge counter.
        Some("mermaid-regen")
    } else {
        None
    }
}

/// Append sanitized MODEL text to the open processing-log flow, editing that
/// one message in place (or opening it if none is live yet). The text is
/// folded into the collapsed block instead of landing as its own message, so
/// only the final response stays clean at the bottom. Empty text (e.g. a
/// react-only intermediate) is ignored.
pub(crate) async fn append_intermediate_to_flow(
    bot: &Bot,
    chat: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    text: &str,
) {
    append_to_flow(bot, chat, thread_id, streaming, text, false).await;
}

/// Fold SYSTEM chrome into the same block: self-healing alerts, retry
/// counters, provider switches.
///
/// Renders identically to narration, differs only in provenance. Chrome is
/// authored here, so it is not a candidate answer and must never be
/// reclaimed as one (#1253): a react-only completion behind a provider
/// switch used to ship "🔧 Switched to …" as the reply, and swallow the
/// reaction with it, because the empty final looked non-empty once the
/// banner had been promoted.
pub(crate) async fn append_system_to_flow(
    bot: &Bot,
    chat: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    text: &str,
) {
    append_to_flow(bot, chat, thread_id, streaming, text, true).await;
}

async fn append_to_flow(
    bot: &Bot,
    chat: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    text: &str,
    system: bool,
) {
    if text.trim().is_empty() {
        return;
    }
    let open = {
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        push_or_supersede(&mut s.flow_entries, text, system);
        s.open_group_msg_id
    };
    if open.is_some() {
        refresh_flow(
            bot,
            chat,
            streaming,
            super::governor::EditClass::Intermediary,
        )
        .await;
    } else {
        open_flow(bot, chat, thread_id, streaming).await;
    }
}

/// Append `text`, or overwrite the previous line when it is progress on the
/// same condition, so a counter advances in place instead of stacking (#982).
///
/// Only ever the IMMEDIATELY preceding entry: anything in between means the
/// context moved on and the new line is genuinely new. Supersession is
/// same-kind only, so chrome never overwrites narration or vice versa.
pub(crate) fn push_or_supersede(entries: &mut Vec<FlowEntry>, text: &str, system: bool) {
    let key = progress_key(text);
    let superseded = key.is_some_and(|k| match entries.last_mut() {
        Some(FlowEntry::System(prev)) if system && progress_key(prev) == Some(k) => {
            *prev = text.to_string();
            true
        }
        Some(FlowEntry::Text(prev)) if !system && progress_key(prev) == Some(k) => {
            *prev = text.to_string();
            true
        }
        _ => false,
    });
    if !superseded {
        entries.push(if system {
            FlowEntry::System(text.to_string())
        } else {
            FlowEntry::Text(text.to_string())
        });
    }
}

/// Move a turn's queued display items into its flow log.
///
/// The regular path folds the same items into the same log in
/// [`crate::channels::telegram::delivery::drain_remaining_display`], which
/// also sends a chat message per item on the way. That send is exactly what a
/// surface with no per-tool messages cannot do: a guest reply is one edited
/// message and nothing else. The ordering rules are kept identical here (tools
/// in place, narration supersedable, chrome never reclaimed as an answer), so
/// the guest block reads like the block a regular chat shows.
///
/// `sanitize` runs on narration and chrome before it reaches the log, taking
/// `(text, is_system_chrome)`. The regular path applies the artifact and
/// marker passes to model text and only the secret scrub to our own chrome, so
/// the same split is a parameter here rather than a second copy of the
/// pipeline.
pub(crate) fn drain_display_queue<F>(s: &mut StreamingState, sanitize: F)
where
    F: Fn(&str, bool) -> String,
{
    for item in std::mem::take(&mut s.display_queue) {
        match item {
            DisplayItem::NewTool(idx) => s.flow_entries.push(FlowEntry::Tool(idx)),
            DisplayItem::Intermediate(text) => {
                let text = sanitize(&text, false);
                if !text.trim().is_empty() {
                    push_or_supersede(&mut s.flow_entries, &text, false);
                }
            }
            DisplayItem::System(text) => {
                let text = sanitize(&text, true);
                push_or_supersede(&mut s.flow_entries, &text, true);
            }
        }
    }
}

/// Re-stick the open processing-log block to the bottom of the chat when newer
/// chatter has buried it (#451). Called only on a new round (tools/intermediate
/// appended this tick), never on plain status ticks, so an idle chat sees no
/// churn. If a message with a higher id than the block landed, re-post the
/// block's current full content as a fresh message at the bottom on the SAME
/// surface (rich details or classic HTML), retag its tool entries to the new
/// message, then delete the old copy. On any re-send failure the old block is
/// kept untouched: relocation must never lose the block. `newest_incoming` is
/// the highest NON-STICKY message id recorded for this chat (#1150) — sticky
/// reposts never feed it, so one restick can't manufacture evidence against
/// the plan card. Draws the shared sticky-action budget before any API call
/// (#1150); returns whether the block actually relocated, so the caller can
/// coordinate the plan-card order under the same budget draw.
pub(crate) async fn restick_flow_if_buried(
    bot: &Bot,
    chat: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    tg: &TelegramState,
    newest_incoming: Option<i32>,
) -> bool {
    let (old_mid, rich) = {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        match s.open_group_msg_id {
            Some(mid) => (mid, s.flow_rich),
            None => return false,
        }
    };
    // Buried only if a chat message with a higher id than the block landed.
    match newest_incoming {
        Some(newest) if newest > old_mid.0 => {}
        _ => return false,
    }
    // Flood-control budget (#1150): a restick is a create+delete pair, and the
    // plan-card move that follows is another — one shared gate bounds the
    // burst instead of two independent timers (#814).
    if !tg.claim_sticky_action(chat.0, TelegramState::STICKY_STACK_MIN_INTERVAL) {
        return false;
    }

    // Re-post the current full flow at the bottom on the same surface.
    let new_mid: Option<MessageId> = if rich {
        let details = {
            let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
            render_flow_details_state(&s)
        };
        if details.is_empty() {
            return false;
        }
        match super::rich::api::send_rich_html_id(
            bot.api_url().as_str(),
            bot.token(),
            chat.0,
            thread_id,
            &details,
            None,
            "turn",
            "-",
        )
        .await
        {
            Ok(mid) => Some(MessageId(mid)),
            Err(e) => {
                tracing::warn!(
                    "Telegram: restick rich re-post failed: {e} — keeping buried block in place"
                );
                None
            }
        }
    } else {
        let html = {
            let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
            render_flow(&s)
        };
        if html.is_empty() {
            return false;
        }
        match send_html_or_plain(bot, chat, thread_id, &html, "turn", None).await {
            Ok(mid) => Some(mid),
            Err(e) => {
                tracing::warn!(
                    "Telegram: restick HTML re-post failed: {e} — keeping buried block in place"
                );
                None
            }
        }
    };
    let Some(new_mid) = new_mid else {
        return false;
    };

    // Swap the block id to the relocated message BEFORE deleting the old copy,
    // so a concurrent refresh edits the new message. Decide under the lock and
    // release it before any await (the guard is not Send). If something else
    // moved or closed the block while we were sending, our just-sent copy is a
    // stray duplicate: delete it instead of the old block.
    let relocated = {
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        if s.open_group_msg_id == Some(old_mid) {
            s.open_group_msg_id = Some(new_mid);
            for t in s.tool_msgs.iter_mut() {
                if t.msg_id == Some(old_mid) {
                    t.msg_id = Some(new_mid);
                }
            }
            true
        } else {
            false
        }
    };
    if relocated {
        // #721 E1: one line per request — the two arms are mutually
        // exclusive, so each carries its own target id.
        super::telemetry::log_request(
            "system",
            "flow restick old block",
            "-",
            "delete",
            "deleteMessage",
            chat.0,
            None,
            Some(i64::from(old_mid.0)),
        );
        if let Err(e) = bot.delete_message(chat, old_mid).await {
            tracing::warn!("Telegram: restick could not delete old block mid={old_mid:?}: {e}");
        }
    } else {
        super::telemetry::log_request(
            "system",
            "flow restick stray duplicate",
            "-",
            "delete",
            "deleteMessage",
            chat.0,
            None,
            Some(i64::from(new_mid.0)),
        );
        if let Err(e) = bot.delete_message(chat, new_mid).await {
            tracing::warn!("Telegram: restick could not delete stray duplicate: {e}");
        }
    }
    // The plan Approve/Discard keyboard rides the persistent plan card, not the
    // flow block (#580), so a relocated block re-posts bare — nothing to
    // re-attach here.
    relocated
}

/// Pull the trailing folded intermediate out of the collapsed processing-log
/// block so it can be delivered as its own message below.
///
/// For CLI providers the final assistant answer is emitted mid-stream as an
/// `IntermediateText` event (and cleared from the returned `response.content`),
/// so #300's fold buries it inside the expandable block and the completion
/// never lands as a separate bubble. Mid-turn narration is always followed by
/// more tool calls, so a `Text` entry sitting LAST in the flow is always the
/// final answer, never interstitial text. This pops it, re-renders the block
/// without it (header-only when it empties), and returns the text.
/// Returns `None` when the flow ended on a tool call — then the answer is in
/// `response.content` and the normal delivery path handles it.
///
/// `options_pending`: the turn ends on a `suggest_options` surface (#1226 K).
/// The substantive answer then sits in the Text run BEFORE the trailing Tool
/// entry — the halted turn never moves it into `response.content`, so the
/// stock "flow ended on a tool -> answer is in content" invariant breaks.
/// The popper lifts the trailing Tool run aside, reclaims that Text run, and
/// restores the Tool run on top: only the answer text leaves the block, the
/// tool history stays.
///
/// #31: a text-only iteration AFTER the halt leaves its sign-off run as a
/// Text entry past the Tool — the popper returns BOTH runs as
/// `(host, trailer)` and nothing is discarded. The host is the answer, the
/// trailer rides after the buttons; when both are `None` the entries are
/// untouched.
pub(crate) async fn take_folded_final(
    bot: &Bot,
    chat: ChatId,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    options_pending: bool,
) -> (Option<String>, Option<String>) {
    let (host, trailer, none_kind): (Option<String>, Option<String>, Option<&'static str>) = {
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        let (host, trailer) = pop_trailing_folded_texts(&mut s.flow_entries, options_pending);
        // #1226: a None here used to be silent, which made the K failure mode
        // (answer stranded in the collapsed flow) undiagnosable from logs.
        // Name what the flow ended on.
        let kind = (host.is_none() && trailer.is_none()).then(|| match s.flow_entries.last() {
            Some(FlowEntry::Tool(_)) => "Tool",
            Some(FlowEntry::Text(_)) => "Text",
            Some(FlowEntry::System(_)) => "System",
            None => "empty",
        });
        (host, trailer, kind)
    };
    if let Some(kind) = none_kind {
        tracing::info!(
            "Telegram: take_folded_final reclaimed nothing (flow ended on {kind}, \
             options_pending={options_pending}) (#1226)"
        );
    }
    if host.is_some() || trailer.is_some() {
        // Re-render the block without the promoted runs. An emptied block is
        // NOT deleted anymore: the flow message is the turn's chrome surface
        // (header, sections, ctx) and settles header-only at turn end, same as a
        // no-tool long turn.
        refresh_flow(bot, chat, streaming, super::governor::EditClass::Final).await;
    }
    (host, trailer)
}

/// Normalize a body for duplicate comparison (#2156): collapse whitespace so a
/// streamed run and the delivered answer compare on their words, not on where
/// the line breaks fell.
pub(crate) fn normalized_body(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Does `final_text` already deliver EVERY word of `candidate`? True when the
/// candidate's normalized form is a prefix of the final's, which covers exact
/// equality and the shape the reported turn hit: a 1962-char narration bubble
/// sitting above a 3761-char answer that opens with the same words (#2156).
///
/// This is deliberately STRICTER than [`folded_duplicates_final`], which treats
/// prefix overlap in EITHER direction as a duplicate. That symmetric form is
/// right when the decision is "suppress the final, the text already went out",
/// but it is unsafe for the two callers here, whose consequence is DELETION:
/// when the candidate is LONGER than the final and merely starts with it, the
/// candidate carries words the final never delivers, so removing it would drop
/// content. Only the one direction that cannot lose anything is allowed.
///
/// Also deliberately not containment: a candidate that appears mid-answer is a
/// separate report the final quotes, not a duplicate of it.
pub(crate) fn answer_already_delivers(final_text: &str, candidate: &str) -> bool {
    let cand = normalized_body(candidate);
    if cand.is_empty() {
        return false;
    }
    let fin = normalized_body(final_text);
    if fin.is_empty() {
        return false;
    }
    fin.starts_with(&cand)
}

/// Decide, and apply, the trailing-run reclaim for a NON-empty final (#2156).
///
/// Pops the trailing folded run and keeps the pop only when the final answer
/// already delivers it; otherwise the entries are restored untouched, so
/// narration the answer never repeats stays in the block. Pure over the entries
/// so the decision is testable without a bot.
pub(crate) fn pop_redundant_folded_texts(
    entries: &mut Vec<FlowEntry>,
    options_pending: bool,
    final_text: &str,
) -> (Option<String>, Option<String>) {
    let snapshot = entries.clone();
    let (host, trailer) = pop_trailing_folded_texts(entries, options_pending);
    let redundant = host
        .as_deref()
        .is_some_and(|h| answer_already_delivers(final_text, h));
    if redundant {
        (host, trailer)
    } else {
        *entries = snapshot;
        (None, None)
    }
}

/// Reclaim the trailing folded run only when the final answer already delivers
/// it (#2156). A NON-empty final ships as its own bubble, and the old code
/// reclaimed nothing on that path, so the block kept its own copy of the
/// answer: the reported turn ended with the resticked block (3651 chars) above
/// a final bubble (3761) repeating it, and the block later grew to 5503.
///
/// The decision is made BEFORE the block is re-rendered: when the trailing run
/// is narration the final never repeats, the entries are restored untouched and
/// no edit is issued.
pub(crate) async fn take_folded_final_if_redundant(
    bot: &Bot,
    chat: ChatId,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    options_pending: bool,
    final_text: &str,
) -> (Option<String>, Option<String>) {
    let kept = {
        let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        pop_redundant_folded_texts(&mut s.flow_entries, options_pending, final_text)
    };
    if kept.0.is_some() || kept.1.is_some() {
        refresh_flow(bot, chat, streaming, super::governor::EditClass::Final).await;
    }
    kept
}

/// Pop the trailing folded `Text` runs, split into host and trailer runs
/// (#478, #31). Mid-turn narration is always followed by more tool calls, so
/// the trailing text run after the last tool IS the final answer — and
/// since #475 keeps ONE block across queued follow-ups, that answer can
/// be multi-part. Popping only the last entry left earlier parts
/// imprisoned in the block.
///
/// `System` chrome inside that run stays where it is (#1253). The run is
/// the final answer only when the model wrote it; a self-healing banner or
/// a provider-switch line is ours, and promoting it to the final bubble
/// ships scaffolding as the reply. That is exactly what a react-only turn
/// behind a provider switch produced: the banner became the answer, and
/// because the answer then read as non-empty, the react-only path never
/// ran and the reaction was never sent either.
///
/// `options_pending` (#1226 K): the turn halted on a `suggest_options`
/// call, so a Tool entry sits LAST and the answer is the Text run
/// immediately BEFORE it. Lift the trailing Tool run aside, pop that Text
/// run, then restore the Tool run in its original order — the block keeps
/// its tool history; only the answer text is reclaimed. When no Text run
/// precedes the tools nothing is popped and the entries are untouched.
///
/// #31: when the model's text-only iteration FOLLOWED the halt, its
/// sign-off run sits AFTER the Tool entry as the trailer. That run is
/// popped FIRST, then the K lift runs — the return is `(host, trailer)`
/// and nothing is discarded. `trailer` is always `None` without
/// `options_pending` (the stock pop never looks past a Tool entry).
pub(crate) fn pop_trailing_folded_texts(
    entries: &mut Vec<FlowEntry>,
    options_pending: bool,
) -> (Option<String>, Option<String>) {
    if !options_pending {
        // The candidate run is everything after the last tool call.
        let run_start = entries
            .iter()
            .rposition(|e| matches!(e, FlowEntry::Tool(_)))
            .map_or(0, |i| i + 1);
        let mut parts: Vec<String> = Vec::new();
        let mut chrome: Vec<FlowEntry> = Vec::new();
        for entry in entries.drain(run_start..) {
            match entry {
                FlowEntry::Text(t) => parts.push(t),
                other => chrome.push(other),
            }
        }
        entries.extend(chrome);
        if parts.is_empty() {
            return (None, None);
        }
        return (Some(parts.join("\n\n")), None);
    }

    // #31: the post-halt trailer run sits AFTER the trailing Tool entry —
    // pop it first, then run the K lift for the host run.
    let join = |mut parts: Vec<String>| {
        parts.reverse();
        parts.join("\n\n")
    };
    let mut trailer_parts: Vec<String> = Vec::new();
    while matches!(entries.last(), Some(FlowEntry::Text(_))) {
        match entries.pop() {
            Some(FlowEntry::Text(t)) => trailer_parts.push(t),
            other => {
                if let Some(e) = other {
                    entries.push(e);
                }
                break;
            }
        }
    }
    let mut aside: Vec<FlowEntry> = Vec::new();
    while matches!(entries.last(), Some(FlowEntry::Tool(_))) {
        if let Some(e) = entries.pop() {
            aside.push(e);
        }
    }
    let mut host_parts: Vec<String> = Vec::new();
    while matches!(entries.last(), Some(FlowEntry::Text(_))) {
        match entries.pop() {
            Some(FlowEntry::Text(t)) => host_parts.push(t),
            other => {
                if let Some(e) = other {
                    entries.push(e);
                }
                break;
            }
        }
    }
    let had_trailing_tools = !aside.is_empty();
    while let Some(e) = aside.pop() {
        entries.push(e);
    }

    if !had_trailing_tools {
        // No Tool entry trailed the run — the gate's precondition (flow ends
        // on the suggest Tool) does not hold, so the popped run is just the
        // stock trailing answer. Return it as the host, never as a trailer.
        let host = (!trailer_parts.is_empty()).then(|| join(trailer_parts));
        return (host, None);
    }
    let host = (!host_parts.is_empty()).then(|| join(host_parts));
    let trailer = (!trailer_parts.is_empty()).then(|| join(trailer_parts));
    (host, trailer)
}

/// The last model-authored folded text, looking past any system chrome
/// appended after it and stopping at the last tool call (#1253).
///
/// The duplicate check at delivery asks what the MODEL left in the block; a
/// banner landing after the answer must not hide it and make the answer
/// render twice.
pub(crate) fn last_folded_text(entries: &[FlowEntry]) -> Option<&String> {
    for entry in entries.iter().rev() {
        match entry {
            FlowEntry::Text(t) => return Some(t),
            FlowEntry::System(_) => continue,
            FlowEntry::Tool(_) => return None,
        }
    }
    None
}

/// Whether a folded intermediate is a duplicate of the final answer.
///
/// Streaming can fold only a truncated head of the final response into the
/// block (a mid-sentence prefix), so an exact match misses it: the copy left in
/// the block is usually a PREFIX of the delivered completion, not equal to it.
/// That gap is why an answer returned in `response.content` (API providers)
/// rendered both inside the collapsed block and as the completion below, while
/// the CLI path (answer reclaimed from the block) did not. Treat a substantial
/// prefix overlap in either direction as a duplicate, with a length guard so a
/// short distinct narration line that merely shares an opening is not mistaken
/// for the answer.
pub(crate) fn folded_duplicates_final(folded: &str, final_text: &str) -> bool {
    let norm_folded = normalized_body(folded);
    let norm_final = normalized_body(final_text);
    if norm_folded.is_empty() || norm_final.is_empty() {
        return false;
    }
    // Exact equality is a duplicate at ANY length — identical strings carry
    // zero false-positive risk. A short final answer folded verbatim used to
    // slip under the prefix length guard below and render twice: once inside
    // the collapsed block and once as the completion (#316).
    if norm_folded == norm_final {
        return true;
    }
    let overlap = norm_folded.len().min(norm_final.len());
    overlap >= 20 && (norm_final.starts_with(&norm_folded) || norm_folded.starts_with(&norm_final))
}

/// A post-halt run larger than this is not a sign-off — it is a substantive
/// answer misfiled by position (#58). Counted in chars, not bytes.
pub(crate) const MAX_TRAILER_CHARS: usize = 600;

/// #58 cap+reroute decision, split out pure so the threshold matrix is
/// unit-testable without a loaded config (`should_render_mermaid` reads
/// `Config::current()`). A mermaid fence in the trailer would ship as raw
/// fence text — the fence→PNG conversion only runs on the final-answer
/// send — and an oversized trailer is a second answer, not a sign-off.
pub(crate) fn trailer_promotes_to_answer(has_mermaid: bool, char_count: usize) -> bool {
    has_mermaid || char_count > MAX_TRAILER_CHARS
}

/// Settle the options-pending reclaim into `(final text, trailer)` (#31).
///
/// `content` is the response.content text — with an option-surface halt the
/// trailing text-only iteration usually leaks its sign-off ack here. `host`
/// is the reclaimed answer run (before the trailing Tool), `trailer` the
/// reclaimed post-halt run (after it). Pure so the #31 arbitration is unit-
/// testable without a Bot or a session.
///
/// Rules, per the owner-approved design:
/// - The host WINS the content slot. The old prepend (`host\n\ncontent`)
///   shipped the ack inside the answer bubble; the answer must not stay
///   imprisoned in the flow block and the ack must not ride inside it.
/// - The trailer survives unless it duplicates the final text (provider
///   double-copy observed inferhub-side in smoke v4/S4) — no double-send.
/// - Keep-never-discard: when the popper found no trailer run but content
///   carries a non-duplicate leftover (the ack never folded into the flow),
///   the content text BECOMES the trailer instead of vanishing.
/// - #58 cap+reroute (owner-approved Option A): a trailer carrying a
///   mermaid fence or exceeding [`MAX_TRAILER_CHARS`] is substantive answer
///   text misfiled by position, NOT a sign-off. It is promoted to the
///   answer slot — the normal final-answer send renders its fence to PNG
///   for free — and the host it displaces demotes to the trailer (nothing
///   dies). Live miss 2026-09-01 04:21Z: a 2668-byte design answer rode
///   after the buttons, its fence shipped as raw text, the pre-rendered
///   PNG died in the render cache (#58).
pub(crate) fn settle_options_reclaim(
    content: String,
    host: Option<String>,
    trailer: Option<String>,
) -> (String, Option<String>) {
    let text = match &host {
        Some(h) => h.clone(),
        None => content.clone(),
    };
    let trailer = match trailer {
        Some(t) if folded_duplicates_final(&t, &text) => None,
        Some(t) => Some(t),
        None => match host {
            Some(_) if !content.trim().is_empty() && !folded_duplicates_final(&content, &text) => {
                Some(content)
            }
            _ => None,
        },
    };
    // #58 cap+reroute (owner-approved Option A): reject a misfiled trailer —
    // promote it to the answer, demote the displaced host to the trailer.
    if let Some(t) = trailer.as_ref().filter(|t| {
        trailer_promotes_to_answer(
            super::rich::mermaid::should_render_mermaid(t),
            t.chars().count(),
        )
    }) {
        let demoted = if text.trim().is_empty() {
            None
        } else {
            Some(text)
        };
        // #92 demoted-host guard: re-run the promotion guard on the demoted
        // host. A fence-bearing (or oversized) displaced host riding the
        // trailer slot would ship its fence raw — the fence→PNG conversion
        // runs only on the final-answer send (#92). Concatenate it into the
        // answer slot (owner-approved fix shape) so BOTH texts ride the
        // render path; the promoted trailer keeps the answer head (#58
        // priority stands), the trailer slot dies with nothing left to ride.
        if let Some(d) = demoted.as_ref().filter(|d| {
            trailer_promotes_to_answer(
                super::rich::mermaid::should_render_mermaid(d),
                d.chars().count(),
            )
        }) {
            return (format!("{t}\n\n{d}"), None);
        }
        return (t.clone(), demoted);
    }
    (text, trailer)
}
