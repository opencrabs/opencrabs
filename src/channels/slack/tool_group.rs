//! Collapsible tool-call group for Slack, the Telegram `<blockquote
//! expandable>` equivalent. Slack has no native collapsed quote, so the
//! grouped message renders a summary line with an Expand button; clicking
//! it flips the stored group state and re-renders the SAME message with
//! the full tool list and a Collapse button. State lives in
//! [`super::SlackState`] keyed by the group message's ts so the
//! interaction handler can re-render long after the turn's closures are
//! gone. Expansion is per-message, not per-viewer: everyone in the
//! channel sees the same state (one shared message).
//!
//! The summary line carries the flow status (#1797): an always-on
//! tool-call counter, a live `🕒` clock while the turn runs, and a settled
//! terminal line (`✅ Finished · N tool calls · ctx · ⏱`) stamped at
//! delivery: the Slack twin of Telegram's flow header/footer.

use std::time::Instant;

use slack_morphism::prelude::*;
use uuid::Uuid;

use super::SlackState;

/// One step in a group, in the order it happened.
///
/// Narration lives here rather than in its own chat message so the agent's
/// between-tool thinking folds away with the tools it sits between, the way
/// Telegram's flow block already does it. Posted standalone it read as an
/// answer, and on a turn that ended with an empty final it was promoted to
/// one (#943).
#[derive(Debug, Clone)]
pub(crate) enum GroupEntry {
    /// A tool invocation.
    Tool {
        name: String,
        context: String,
        /// None = running, Some(success) = finished.
        status: Option<bool>,
        /// Set for a detached background task row (#2145, #2149): the id the
        /// finish event matches on, so a finish settles its own row even when
        /// two tasks share a label. `None` for an ordinary tool row.
        task_id: Option<String>,
    },
    /// Inter-iteration narration: what the agent said between tool calls.
    Note(String),
}

impl GroupEntry {
    /// A running tool has no verdict yet. Notes are never "running" — they are
    /// a record of something already said.
    fn is_running(&self) -> bool {
        matches!(self, Self::Tool { status: None, .. })
    }

    fn is_failed(&self) -> bool {
        matches!(
            self,
            Self::Tool {
                status: Some(false),
                ..
            }
        )
    }

    fn is_tool(&self) -> bool {
        matches!(self, Self::Tool { .. })
    }

    /// How this row names itself in the rendered list (#2149).
    ///
    /// A detached background task row says what it is and where it stands;
    /// an ordinary tool row keeps its tool name untouched, so every existing
    /// render is unchanged.
    pub(crate) fn display_name(&self) -> String {
        match self {
            Self::Tool {
                name,
                status,
                task_id,
                ..
            } => crate::channels::background_work::detached_row_label(
                name,
                task_id.as_deref(),
                *status,
            ),
            Self::Note(text) => text.clone(),
        }
    }
}

/// What a detached-task event must do to the group message (#2149).
///
/// Returned instead of performed, because the contract that matters is "the
/// start and the finish share one message", and that is provable without a
/// bot only if the decision lives outside the spawned HTTP call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetachedRender {
    /// No group message exists yet: post one and keep its ts.
    Post,
    /// Rewrite the existing group message with this ts.
    Edit(String),
    /// Nothing to show: a finish whose start never landed in this group.
    Skip,
}

/// A detached-task lifecycle event, free of transport.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DetachedEvent<'a> {
    Started { task_id: &'a str, label: &'a str },
    Finished { task_id: &'a str, success: bool },
}

/// Fold a detached-task event into the group entries and say what the caller
/// must do with the message (#2149).
///
/// A finish never posts. If the row is missing the event is `Skip`, so a
/// late finish cannot open a second bubble carrying the outcome the group
/// already shows.
pub(crate) fn detached_render(
    entries: &mut Vec<GroupEntry>,
    group_ts: Option<&str>,
    event: DetachedEvent<'_>,
) -> DetachedRender {
    match event {
        DetachedEvent::Started { task_id, label } => {
            entries.push(GroupEntry::Tool {
                name: label.to_string(),
                context: String::new(),
                status: None,
                task_id: Some(task_id.to_string()),
            });
            match group_ts {
                Some(ts) => DetachedRender::Edit(ts.to_string()),
                None => DetachedRender::Post,
            }
        }
        DetachedEvent::Finished { task_id, success } => {
            // Match by task id, never by label: two detached tasks can both
            // be `cargo test`, and a label match would settle whichever row
            // it hit first, leaving a finished task showing as running.
            let row = entries.iter_mut().rev().find(|e| {
                matches!(
                    e,
                    GroupEntry::Tool { task_id: Some(id), .. } if id == task_id
                )
            });
            let Some(GroupEntry::Tool { status, .. }) = row else {
                return DetachedRender::Skip;
            };
            *status = Some(success);
            // A finish only ever rewrites. With no block to rewrite the
            // outcome is dropped rather than posted as a second bubble.
            match group_ts {
                Some(ts) => DetachedRender::Edit(ts.to_string()),
                None => DetachedRender::Skip,
            }
        }
    }
}

/// A turn's tool group: what it contains and how it is displayed.
#[derive(Debug, Clone)]
pub(crate) struct GroupState {
    pub channel: SlackChannelId,
    pub entries: Vec<GroupEntry>,
    pub expanded: bool,
    /// Turn-start anchor for the live `🕒` segment and the settled `⏱` one
    /// (#1797). Stamped at first insert and preserved across updates so the
    /// clock never restarts mid-turn.
    pub started_at: Instant,
    /// Post-delivery status (#1797). `None` while the turn is live; stamped
    /// once by `settle` and preserved by every later upsert.
    pub settled: Option<SettledStatus>,
}

impl GroupState {
    /// A live group at turn start: collapsed, clock anchored now.
    pub(crate) fn new(channel: SlackChannelId, entries: Vec<GroupEntry>) -> Self {
        Self {
            channel,
            entries,
            expanded: false,
            started_at: Instant::now(),
            settled: None,
        }
    }

    /// Stamp the delivery outcome (#1797). A Finished turn that ends with
    /// background work still running overrides to the waiting state (the
    /// Slack mirror of Telegram's #1144 header). Since #1988 the verb
    /// arrives precomputed from the shared `waiting_verb`, which folds
    /// detached tasks AND working sub-agents into one count; the gate
    /// below keeps it off every non-Finished outcome, and a `None` ctx
    /// keeps whatever was stamped at first settle so the later flip to
    /// Finished re-renders with the same budget.
    pub(crate) fn settle(
        &mut self,
        outcome: TurnOutcome,
        waiting: Option<String>,
        ctx: Option<String>,
    ) {
        let is_waiting = outcome == TurnOutcome::Finished && waiting.is_some();
        let prev_ctx = self.settled.as_ref().and_then(|s| s.ctx.clone());
        let (icon, verb) = match waiting {
            Some(verb) if is_waiting => ("⏳", verb),
            _ => {
                let (icon, verb) = outcome.icon_verb();
                (icon, verb.to_string())
            }
        };
        self.settled = Some(SettledStatus {
            icon,
            verb,
            ctx: ctx.or(prev_ctx),
            waiting: is_waiting,
        });
    }

    /// The flow ticker's stop condition (#1988): the group settled on a
    /// TERMINAL line. A waiting settle is not terminal, its `🕒` keeps
    /// rolling until the flip replaces it with the finished line.
    pub(crate) fn settled_terminal(&self) -> bool {
        self.settled.as_ref().is_some_and(|s| !s.waiting)
    }
}

/// Terminal outcome of a turn, stamped on the group at settle (#1797). The
/// Slack twin of Telegram's `FlowOutcome`, plus `Cancelled`: Slack posts a
/// distinct message for an interrupted turn, so its group says so too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TurnOutcome {
    Finished,
    Failed,
    TimedOut,
    Cancelled,
}

impl TurnOutcome {
    fn icon_verb(self) -> (&'static str, &'static str) {
        match self {
            Self::Finished => ("✅", "Finished"),
            Self::Failed => ("❌", "Failed"),
            Self::TimedOut => ("⏱", "Timed out"),
            Self::Cancelled => ("❌", "Cancelled"),
        }
    }
}

/// The group's post-delivery status (#1797): icon + verb, the ctx budget
/// captured at first settle, and whether the turn ended with background
/// work (detached tasks and/or working sub-agents, #1988) still running.
/// A waiting group keeps the live `🕒` glyph until the flip re-renders it
/// terminal.
#[derive(Debug, Clone)]
pub(crate) struct SettledStatus {
    pub icon: &'static str,
    pub verb: String,
    pub ctx: Option<String>,
    pub waiting: bool,
}

fn entry_icon(status: Option<bool>) -> &'static str {
    match status {
        None => "⚙️",
        Some(true) => "✅",
        Some(false) => "❌",
    }
}

/// The narration held in a group, joined, or `None` if there is none.
///
/// Used only when a turn's final response comes back empty: the folded text is
/// then the whole answer, and leaving it inside a collapsed group means posting
/// nothing at all (#951). Tool rows are excluded — they are a record of what
/// ran, not something to say back to the user.
pub(crate) fn notes_text(entries: &[GroupEntry]) -> Option<String> {
    let joined = entries
        .iter()
        .filter_map(|e| match e {
            GroupEntry::Note(text) => Some(text.trim()),
            GroupEntry::Tool { .. } => None,
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!joined.is_empty()).then_some(joined)
}

/// The group's entries minus the narration notes (#1805).
///
/// The empty-final salvage promotes the folded narration to the answer
/// message; notes left behind in the group then showed the same text twice
/// the moment the group was expanded. They are consumed, not copied: what
/// stays is the tool record only. An empty result means the group held
/// nothing but narration, so its message is a shell the caller can delete.
pub(crate) fn consume_notes(entries: &[GroupEntry]) -> Vec<GroupEntry> {
    entries
        .iter()
        .filter(|e| matches!(e, GroupEntry::Tool { .. }))
        .cloned()
        .collect()
}

/// One rendered line for a step.
fn entry_line(entry: &GroupEntry) -> String {
    match entry {
        GroupEntry::Tool {
            context, status, ..
        } => format!(
            "{} *{}*{}",
            entry_icon(*status),
            entry.display_name(),
            context
        ),
        GroupEntry::Note(text) => format!("💭 _{}_", text.trim()),
    }
}

/// Summary line: status icon + always-on tool-call count + live clock, or
/// the settled terminal segments (#1797).
///
/// The count is the TOOL-call count, never mixed with narration steps, and
/// present on every turn shape: Telegram's flow header counts tools even for
/// a single call, and a one-tool Slack turn used to render with no counter
/// at all. Live: `⚙️ *2 tool calls* · 1 running · 🕒 0:12`. Settled:
/// `✅ *Finished · 3 tool calls* · ctx: 84K/200K 42% · ⏱️ 4:25`. A waiting
/// settle keeps the live glyph: `⏳ *Waiting for 2 background tasks ·
/// 3 tool calls* · ctx · 🕒 5:01`, because the clock keeps rolling until
/// the flip.
/// Live activity preview for the flow line (#1809): what the agent is doing
/// right now, the Slack twin of Telegram's `latest_activity_preview`
/// (telegram/flow.rs). Same priorities: latest human-readable narration note,
/// then line-start `#` comments from the latest bash command, then the latest
/// tool label + context. This is the feedback Telegram's live footer leads
/// with (#1052) and Slack's flow was missing.
fn latest_activity(group: &GroupState) -> Option<String> {
    if let Some(text) = group.entries.iter().rev().find_map(|e| match e {
        GroupEntry::Note(text) => crate::channels::telegram::flow::human_readable_preview(text),
        _ => None,
    }) {
        return Some(text);
    }
    if let Some(comments) = group.entries.iter().rev().find_map(|e| match e {
        GroupEntry::Tool { name, context, .. } if name == "bash" => {
            crate::channels::telegram::flow::extract_status_from_text(context)
        }
        _ => None,
    }) {
        return Some(comments);
    }
    group.entries.iter().rev().find_map(|e| match e {
        GroupEntry::Tool { name, context, .. } => {
            let ctx = context.trim_start();
            Some(if ctx.is_empty() {
                name.clone()
            } else {
                format!("{name} {ctx}")
            })
        }
        GroupEntry::Note(_) => None,
    })
}

/// Longest activity segment in the live flow line (#1809). Display-only cap:
/// the line stays a glance, not a transcript.
const ACTIVITY_MAX_CHARS: usize = 100;

fn activity_segment(group: &GroupState) -> Option<String> {
    let text = latest_activity(group)?;
    let clipped: String = text.chars().take(ACTIVITY_MAX_CHARS).collect();
    let out = if text.chars().count() > ACTIVITY_MAX_CHARS {
        format!("{clipped}…")
    } else {
        clipped
    };
    (!out.is_empty()).then_some(out)
}

fn summary_line(group: &GroupState) -> String {
    let tools = group.entries.iter().filter(|e| e.is_tool()).count();
    let counts = format!("{tools} tool call{}", if tools == 1 { "" } else { "s" });
    match &group.settled {
        Some(s) => {
            let mut segs = vec![format!("{} {}", s.icon, s.verb)];
            if tools > 0 {
                segs.push(counts);
            }
            if let Some(ctx) = &s.ctx {
                segs.push(ctx.clone());
            }
            let glyph = if s.waiting { "🕒" } else { "⏱️" };
            segs.push(format!("{glyph} {}", clock(group.started_at.elapsed())));
            let rest = segs[1..].join(" · ");
            format!("{} *{rest}*", segs[0])
        }
        None => {
            // Live contract from the narration-fold work (#943): steps are
            // shown beside tool calls only when narration makes the two
            // counts differ, and the icon reads ✅ once nothing is running
            // or failed. #1797 adds the always-on clock.
            let steps = group.entries.len();
            let running = group.entries.iter().filter(|e| e.is_running()).count();
            let failed = group.entries.iter().filter(|e| e.is_failed()).count();
            let (icon, tail) = if running > 0 {
                ("⚙️", format!(" · {running} running"))
            } else if failed > 0 {
                ("❌", format!(" · {failed} failed"))
            } else {
                ("✅", String::new())
            };
            let counts = if steps == tools {
                counts
            } else {
                format!(
                    "{steps} step{} · {counts}",
                    if steps == 1 { "" } else { "s" }
                )
            };
            // #1809: the activity leads the live line, the Telegram
            // live-footer order (#1052): what it's doing, then counts,
            // then the clock. Absent at turn start (empty group) so the
            // shell keeps the bare counts shape.
            let lead = activity_segment(group)
                .map(|a| format!("{a} · "))
                .unwrap_or_default();
            format!(
                "{icon} {lead}*{counts}*{tail} · 🕒 {}",
                clock(group.started_at.elapsed())
            )
        }
    }
}

/// `M:SS` elapsed clock (`H:MM:SS` past an hour), the Slack twin of
/// Telegram's flow clock. The glyph lives with the caller so the live and
/// settled segments can differ (`🕒` rolls, `⏱️` freezes at settle).
fn clock(elapsed: std::time::Duration) -> String {
    let (h, m, s) = (
        elapsed.as_secs() / 3600,
        (elapsed.as_secs() % 3600) / 60,
        elapsed.as_secs() % 60,
    );
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Render the group as message content. Collapsed shows only the summary
/// line; expanded lists the summary plus every step. The toggle button is
/// always present (#1797): a single-tool turn must show its counter AND
/// still reveal the tool row on expand, which is exactly Telegram's
/// flow-block shape. One exception: a lone collapsed NOTE renders as its
/// own plain line (#943): it is speech, not a tool count, and
/// "0 tool calls" above a sentence is noise with nothing to reveal.
pub(crate) fn render(group: &GroupState, ts: &SlackTs) -> SlackMessageContent {
    let lone_collapsed_note =
        matches!(group.entries.as_slice(), [GroupEntry::Note(_)]) && !group.expanded;
    let text = if lone_collapsed_note {
        entry_line(&group.entries[0])
    } else {
        let mut lines = vec![summary_line(group)];
        if group.expanded {
            lines.extend(group.entries.iter().map(entry_line));
        }
        lines.join("\n")
    };

    let mut blocks = vec![SlackBlock::Section(SlackSectionBlock::new().with_text(
        SlackBlockText::MarkDown(SlackBlockMarkDownText::new(text.clone())),
    ))];
    if !lone_collapsed_note {
        let label = if group.expanded {
            "Collapse ▲"
        } else {
            "Expand ▼"
        };
        blocks.push(SlackBlock::Actions(SlackActionsBlock::new(vec![
            SlackActionBlockElement::Button(SlackBlockButtonElement::new(
                SlackActionId::new(format!("toolgroup:{}", ts)),
                SlackBlockPlainTextOnly::from(SlackBlockPlainText::new(label.to_string())),
            )),
        ])));
    }
    SlackMessageContent::new()
        .with_text(text)
        .with_blocks(blocks)
}

impl SlackState {
    /// Retained tool groups; older ones stop being toggleable (their last
    /// rendered state stays on screen, like Telegram's frozen blocks).
    const TOOL_GROUP_CAP: usize = 20;

    /// Insert or update the group for a message ts, PRESERVING the user's
    /// expanded/collapsed choice on updates (a completing tool must not
    /// snap an expanded group shut) and, once stamped, the settle state and
    /// turn-start anchor (#1797): a straggler status update after delivery
    /// must never un-settle the group or restart its clock. Prunes the
    /// oldest beyond the cap. Returns the stored state so callers render
    /// exactly what is kept.
    pub(crate) async fn upsert_tool_group(&self, ts: String, mut group: GroupState) -> GroupState {
        let mut guard = self.tool_groups.lock().await;
        let (order, map) = &mut *guard;
        match map.get(&ts) {
            Some(existing) => {
                group.expanded = existing.expanded;
                group.started_at = existing.started_at;
                group.settled = existing.settled.clone();
            }
            None => {
                order.push(ts.clone());
                while order.len() > Self::TOOL_GROUP_CAP {
                    let oldest = order.remove(0);
                    map.remove(&oldest);
                }
            }
        }
        map.insert(ts, group.clone());
        group
    }

    /// Flip a group's expanded state; returns the new state for re-render,
    /// or None when the group aged out of retention.
    pub(crate) async fn toggle_tool_group(&self, ts: &str) -> Option<GroupState> {
        let mut guard = self.tool_groups.lock().await;
        let (_, map) = &mut *guard;
        let group = map.get_mut(ts)?;
        group.expanded = !group.expanded;
        Some(group.clone())
    }

    /// Stamp the delivery outcome on the group (#1797), preserving the
    /// user's expansion choice and the turn-start anchor. Returns the stored
    /// state, or None when the ts aged out of retention.
    pub(crate) async fn settle_tool_group(
        &self,
        ts: &str,
        outcome: TurnOutcome,
        waiting: Option<String>,
        ctx: Option<String>,
    ) -> Option<GroupState> {
        let mut guard = self.tool_groups.lock().await;
        let (_, map) = &mut *guard;
        let group = map.get_mut(ts)?;
        group.settle(outcome, waiting, ctx);
        Some(group.clone())
    }

    /// Snapshot a group by ts for the flow ticker (#1807): live or settled,
    /// `None` only when missing (never stored or pruned by retention). The
    /// ticker decides on `settled`; keeping settled groups visible here is
    /// what makes the stop condition observable in tests.
    pub(crate) async fn tool_group_snapshot(&self, ts: &str) -> Option<GroupState> {
        let guard = self.tool_groups.lock().await;
        let (_, map) = &*guard;
        map.get(ts).cloned()
    }

    /// Record the channel's most recent background-waiting group (#1797),
    /// keyed per channel since #1988: one entry per channel (a channel has
    /// at most one live turn). The old single global slot let a second
    /// channel's note clobber the first waiting group, which then never
    /// flipped.
    pub(crate) async fn note_waiting_group(&self, channel: String, ts: String, session: Uuid) {
        self.waiting_groups
            .lock()
            .await
            .insert(channel, (ts, session));
    }

    /// Take the channel's waiting group, if any (#1797). Flip points: the
    /// background-completion path in `resume.rs` (#1988) and, as a
    /// backstop, the top of handle_message.
    pub(crate) async fn take_waiting_group_for(&self, channel: &str) -> Option<(String, Uuid)> {
        self.waiting_groups.lock().await.remove(channel)
    }
}
