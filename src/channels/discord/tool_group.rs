//! Collapsible tool-call group for Discord (#380), matching the Telegram
//! block and the Slack Block Kit port: ONE message per turn, collapsed to
//! a live summary with an Expand button, toggled in place via component
//! interaction. State lives in [`super::DiscordState`] keyed by message id
//! so the click handler can re-render after the turn's closures are gone.
//! Expansion is per-message: everyone in the channel shares it.
//!
//! With `trace_narration` enabled the same bubble also carries the turn's
//! intermediate narration as dim subtext notes (agent-disco-style live
//! trace): one editable work-log per turn instead of one message per
//! intermediate.

use serenity::builder::{CreateActionRow, CreateButton, CreateEmbed, CreateEmbedFooter};
use serenity::model::application::ButtonStyle;
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::DiscordState;
use super::tool_card::{
    card_color, card_footer, card_title, clamp_page, collapse_repeats, page_count, page_slice,
    row_line,
};
use crate::channels::background_work::FlowOutcome;

/// One tool row in a group.
#[derive(Debug, Clone)]
pub(crate) struct GroupEntry {
    pub name: String,
    pub context: String,
    /// The provider's id for this call; empty when unknown (#1880). A
    /// completion matches its own row by this id.
    pub call_id: String,
    /// None = running, Some(success) = finished.
    pub status: Option<bool>,
    /// Set only on a detached-task row (#2148): the background task's id.
    /// A detached task has no provider call id, and two of them can share a
    /// label, so this is what a finish matches on.
    pub task_id: Option<String>,
}

impl GroupEntry {
    /// The label this row shows. A detached row says so and says whether it
    /// is still running; a tool row shows its tool name.
    pub(crate) fn display_name(&self) -> String {
        crate::channels::background_work::detached_row_label(
            &self.name,
            self.task_id.as_deref(),
            self.status,
        )
    }
}

/// What a detached-task event does to the turn's group message (#2148).
/// Returned by [`detached_render`] so the caller never has to decide, which
/// is what keeps a finish from ever posting a second block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetachedRender {
    /// No group message yet: post it, then edit it in place from then on.
    Post,
    /// Rewrite the block at this message id.
    Edit(u64),
    /// Nothing to render: an orphan finish, or a row that is not there.
    Skip,
}

/// One detached-task event, in the shape the renderer needs.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DetachedEvent<'a> {
    Started { task_id: &'a str, label: &'a str },
    Finished { task_id: &'a str, success: bool },
}

/// Apply a detached-task event to the turn's rows and say where it renders.
///
/// Pure: no transport, no clock, no network. That is deliberate, because the
/// contract worth testing is "start and finish share one message id" (#2148),
/// and a bot cannot be stood up to prove it. A start posts only when the turn
/// has no block yet; a finish only ever edits, so the block keeps one id for
/// its whole life.
pub(crate) fn detached_render(
    entries: &mut Vec<GroupEntry>,
    group_mid: Option<u64>,
    event: DetachedEvent<'_>,
) -> DetachedRender {
    match event {
        DetachedEvent::Started { task_id, label } => {
            entries.push(GroupEntry {
                name: label.to_string(),
                context: String::new(),
                call_id: String::new(),
                status: None,
                task_id: Some(task_id.to_string()),
            });
            match group_mid {
                Some(mid) => DetachedRender::Edit(mid),
                None => DetachedRender::Post,
            }
        }
        DetachedEvent::Finished { task_id, success } => {
            // Match by task id, never by label: two detached tasks can both
            // be `cargo test`, and a label match would settle whichever row
            // it hit first, leaving a finished task showing as running.
            let Some(entry) = entries
                .iter_mut()
                .find(|e| e.task_id.as_deref() == Some(task_id))
            else {
                return DetachedRender::Skip;
            };
            entry.status = Some(success);
            match group_mid {
                Some(mid) => DetachedRender::Edit(mid),
                None => DetachedRender::Skip,
            }
        }
    }
}

/// A turn's tool group: contents plus display state.
#[derive(Debug, Clone)]
pub(crate) struct GroupState {
    pub entries: Vec<GroupEntry>,
    /// Narration lines folded into the bubble (live trace). Authoritative
    /// state lives in [`DiscordState`]; only [`DiscordState::append_note`]
    /// and [`DiscordState::drop_note_if`] mutate them —
    /// [`DiscordState::upsert_tool_group`] preserves the stored notes the
    /// way it preserves `expanded`.
    pub notes: Vec<String>,
    pub expanded: bool,
    /// Zero-based card page shown while expanded (#1880).
    pub page: usize,
    /// Turn-start anchor for the live `🕒` segment and the settled `⏱` one
    /// (#1841). Stamped at first insert and preserved across updates so the
    /// clock never restarts mid-turn.
    pub started_at: Instant,
    /// Post-delivery status (#1841). `None` while the turn is live; stamped
    /// once by [`DiscordState::settle_tool_group`] and preserved by every
    /// later upsert. `elapsed` freezes at settle so toggling Expand later
    /// never grows the clock.
    pub settled: Option<SettledStatus>,
}

/// Frozen post-delivery chrome (#1841): the Discord twin of Slack's
/// `SettledStatus`. The clock stops at settle and the ctx budget line moves
/// into the flow group, so the chrome owns it (the answer-message footer
/// goes away in #1842, leaving the settled line as the single home).
#[derive(Debug, Clone)]
pub(crate) struct SettledStatus {
    pub elapsed: Duration,
    pub ctx: Option<String>,
    /// Turn ended with background work still alive (#1987): the settled line
    /// reads `⏳ {verb}` instead of the finished icon. The completion path
    /// narrows this as work drains and clears it (flip to finished) once
    /// both registries are empty.
    pub waiting: Option<String>,
    /// Terminal state of a turn that ended anywhere but normally (#1911):
    /// `None` means the turn delivered its answer, so the line keeps the
    /// plain counts chrome. `Some` stamps the settled outcome's own icon and
    /// verb (`⏱ Timed out`, `❌ Failed`, `❌ Cancelled`) from the shared
    /// [`FlowOutcome`], the same vocabulary Telegram's settled header reads.
    /// Set by the Cancelled/Err delivery arms (#1987) so the group stops
    /// claiming success and the flow ticker's clock ends with the turn
    /// instead of running to its 30-minute orphan cap.
    pub outcome: Option<FlowOutcome>,
}

/// Keep at most this many narration lines in the bubble (newest win).
pub(crate) const NOTE_CAP: usize = 6;

/// Clip each narration line to this many chars — the bubble stays a glance,
/// not a transcript.
pub(crate) const NOTE_MAX_CHARS: usize = 160;

/// First non-empty line, trimmed to [`NOTE_MAX_CHARS`] — the bubble form of
/// one narration event.
pub(crate) fn clip_note(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(NOTE_MAX_CHARS).collect();
    if line.chars().count() > NOTE_MAX_CHARS {
        out.push('…');
    }
    out
}

/// Narration lines as Discord subtext (`-# ` renders dim and small).
fn notes_block(notes: &[String]) -> String {
    notes
        .iter()
        .map(|n| format!("-# {n}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn entry_icon(status: Option<bool>) -> &'static str {
    match status {
        None => "⚙️",
        Some(true) => "✅",
        Some(false) => "❌",
    }
}

/// `M:SS` elapsed clock (`H:MM:SS` past an hour), the Discord twin of
/// Telegram's flow clock. The glyph lives with the caller so the live and
/// settled segments can differ (`🕒` rolls, `⏱️` freezes at settle).
fn clock(elapsed: Duration) -> String {
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

/// Longest activity segment in the live flow line (#1844). Display-only
/// cap: the line stays a glance, not a transcript.
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

/// Live activity preview for the flow line (#1844): what the agent is doing
/// right now, the Discord twin of Slack's `latest_activity` (#1809) and
/// Telegram's `latest_activity_preview` (telegram/flow.rs). Same
/// priorities: latest human-readable narration note, then line-start `#`
/// comments from the latest bash command, then the latest tool label +
/// context. Discord keeps notes in their own `Vec` (`GroupState.notes`),
/// not as entries.
fn latest_activity(group: &GroupState) -> Option<String> {
    if let Some(text) = group
        .notes
        .iter()
        .rev()
        .find_map(|n| crate::channels::telegram::flow::human_readable_preview(n))
    {
        return Some(text);
    }
    if let Some(comments) = group.entries.iter().rev().find_map(|e| {
        if e.name == "bash" {
            crate::channels::telegram::flow::extract_status_from_text(&e.context)
        } else {
            None
        }
    }) {
        return Some(comments);
    }
    group
        .entries
        .iter()
        .rev()
        .map(|e| {
            let ctx = e.context.trim_start();
            if ctx.is_empty() {
                e.display_name()
            } else {
                // Contexts are stored with a leading space (` (arg0)`): trim,
                // then join with one canonical space (the #1809 double-space fix).
                format!("{} {ctx}", e.display_name())
            }
        })
        .next()
}

fn summary_line(group: &GroupState) -> String {
    let n = group.entries.len();
    let failed = group
        .entries
        .iter()
        .filter(|e| e.status == Some(false))
        .count();
    let counts = format!("**{n} tool call{}**", if n == 1 { "" } else { "s" });
    match &group.settled {
        Some(s) => {
            // Settled chrome (#1841): frozen clock, ctx budget as the last
            // word before it, mirroring the Telegram settled header order.
            // A settled turn has no running tools: the icon reads final
            // states only, ❌ when something failed, ✅ otherwise, never
            // the live "N running" tail (an entry left statusless at
            // settle is done, not running). #1987 puts two overrides ahead
            // of that default: a waiting verb (⏳, the turn ended with
            // background work alive) and a terminal outcome (#1911, the
            // Cancelled/Err arms settle the group so its clock stops). The
            // outcome prints its own icon and verb, so a timed-out turn now
            // reads `⏱ Timed out` where a crashed one reads `❌ Failed`,
            // matching the Telegram settled header instead of collapsing
            // both into one cross.
            let mut line = if let Some(verb) = &s.waiting {
                format!("⏳ {verb} · {counts}")
            } else if let Some(outcome) = s.outcome {
                let (icon, verb) = outcome.icon_verb();
                format!("{icon} {verb} · {counts}")
            } else if failed > 0 {
                format!("❌ {counts} · {failed} failed")
            } else {
                format!("✅ {counts}")
            };
            if let Some(ctx) = &s.ctx {
                line.push_str(&format!(" · {ctx}"));
            }
            line.push_str(&format!(" · ⏱️ {}", clock(s.elapsed)));
            line
        }
        None => {
            // Live line (#1844): activity text leads when there is any
            // (latest note > bash # comments > tool label), bare counts
            // shape otherwise (zero-entry turn-start shell). Settled arm
            // above stays clean.
            let running = group.entries.iter().filter(|e| e.status.is_none()).count();
            let (icon, tail) = if running > 0 {
                ("⚙️", format!(" · {running} running"))
            } else if failed > 0 {
                ("❌", format!(" · {failed} failed"))
            } else {
                ("✅", String::new())
            };
            let clock = format!("🕒 {}", clock(group.started_at.elapsed()));
            match activity_segment(group) {
                Some(activity) => format!("{icon} {activity} · {counts}{tail} · {clock}"),
                None => format!("{icon} {counts}{tail} · {clock}"),
            }
        }
    }
}

/// Discord hard-caps message content at 2000 chars (#1949): anything
/// longer is rejected on the wire, and a rejected toggle response leaves
/// the interaction unresolved — the client then blames a timeout. Every
/// render path (live edits, settle, Expand responses) fits this cap by
/// construction.
pub(crate) const CONTENT_MAX_CHARS: usize = 2000;

/// Final wire guard: hard-cut at the cap on a char boundary. Entry rows
/// and notes are clipped upstream, so this only exists for pathological
/// callers — an oversized message must never reach the API.
fn hard_clip(body: String) -> String {
    if body.chars().count() <= CONTENT_MAX_CHARS {
        return body;
    }
    let cut: String = body.chars().take(CONTENT_MAX_CHARS - 1).collect();
    format!("{cut}…")
}

/// Message body for the group in its current display state.
pub(crate) fn render_content(group: &GroupState) -> String {
    // The bare entry line is a LIVE-only shortcut. Once settled, the
    // final-state chrome (⏳ waiting verb, outcome icon+verb, ✅/❌ counts,
    // ctx, frozen clock) must render even for single-tool turns. #1987:
    // a lone aborted call otherwise kept showing a plain entry row and
    // hid the Cancelled/Error/Waiting state from the user entirely.
    let tools_part = if group.entries.len() == 1 && !group.expanded && group.settled.is_none() {
        let e = &group.entries[0];
        format!(
            "{} **{}**{}",
            entry_icon(e.status),
            e.display_name(),
            e.context
        )
    } else {
        summary_line(group)
    };
    let mut body = if group.notes.is_empty() {
        tools_part.clone()
    } else {
        format!("{tools_part}\n{}", notes_block(&group.notes))
    };
    // Notes ride after the rows; if the whole body still overshoots, drop
    // the oldest notes first (the newest is what the ticker just wrote),
    // and hard-cut as the last resort.
    let mut notes: Vec<String> = group.notes.clone();
    while body.chars().count() > CONTENT_MAX_CHARS && notes.len() > 1 {
        notes.remove(0);
        body = format!("{tools_part}\n{}", notes_block(&notes));
    }
    hard_clip(body)
}

/// Reply for a card click whose message aged out of retention (#1880).
pub(crate) fn stale_card_reply() -> serenity::builder::CreateInteractionResponse {
    serenity::builder::CreateInteractionResponse::Message(
        serenity::builder::CreateInteractionResponseMessage::new()
            .ephemeral(true)
            .content("This status bubble is too old to use."),
    )
}

/// Buttons under the group message. A lone line has nothing to reveal, so it
/// gets none. Collapsed: Expand and Log. Expanded: page back and forward,
/// Log, and Collapse (#1880).
pub(crate) fn render_components(group: &GroupState, message_id: u64) -> Vec<CreateActionRow> {
    if group.entries.len() < 2 {
        return Vec::new();
    }
    let log = CreateButton::new(format!("tglog:{message_id}"))
        .label("Log")
        .style(ButtonStyle::Secondary);
    if !group.expanded {
        return vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("toolgroup:{message_id}"))
                .label("Expand ▼")
                .style(ButtonStyle::Secondary),
            log,
        ])];
    }
    let rows = collapse_repeats(&group.entries);
    let page = clamp_page(group.page, rows.len());
    let last = page_count(rows.len()) - 1;
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("tgpage:{message_id}:{}", page.saturating_sub(1)))
            .label("◀")
            .style(ButtonStyle::Secondary)
            .disabled(page == 0),
        CreateButton::new(format!("tgpage:{message_id}:{}", (page + 1).min(last)))
            .label("▶")
            .style(ButtonStyle::Secondary)
            .disabled(page >= last),
        log,
        CreateButton::new(format!("toolgroup:{message_id}"))
            .label("Collapse ▲")
            .style(ButtonStyle::Secondary),
    ])]
}

/// The card embed for an expanded group: the current page of rows, the
/// header, the bar color, and the page footer. Empty when no card is shown
/// (collapsed, or a lone line). Rows live here and not in the message text,
/// so a long turn pages instead of being clipped (#1880).
pub(crate) fn render_embeds(group: &GroupState) -> Vec<CreateEmbed> {
    if !group.expanded || group.entries.len() < 2 {
        return Vec::new();
    }
    let rows = collapse_repeats(&group.entries);
    let page = clamp_page(group.page, rows.len());
    let body = page_slice(&rows, page)
        .iter()
        .map(row_line)
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        CreateEmbed::new()
            .title(card_title(&rows))
            .colour(card_color(&rows))
            .description(body)
            .footer(CreateEmbedFooter::new(card_footer(
                &rows,
                page,
                group.entries.len(),
            ))),
    ]
}

/// Render one detached-task event into the turn's group message (#2148).
///
/// A start draws its row and a finish settles the row it owns. Neither ever
/// posts a second block once one exists: both edit the same message id, which
/// is the whole contract. A finish with no block or no matching row does
/// nothing rather than opening a new one.
pub(crate) async fn apply_detached_event(
    dstate: &DiscordState,
    channel: serenity::model::id::ChannelId,
    http: &serenity::http::Http,
    tools: &tokio::sync::Mutex<Vec<GroupEntry>>,
    gmid: &tokio::sync::Mutex<Option<serenity::model::id::MessageId>>,
    event: DetachedEvent<'_>,
) {
    use serenity::builder::EditMessage;
    use serenity::model::id::MessageId;

    let (render, entries) = {
        let mut t = tools.lock().await;
        let mid = *gmid.lock().await;
        let render = detached_render(&mut t, mid.map(|m| m.get()), event);
        (render, t.clone())
    };

    match render {
        DetachedRender::Skip => {}
        DetachedRender::Edit(mid) => {
            let group = dstate
                .upsert_tool_group(
                    mid,
                    GroupState {
                        page: 0,
                        entries,
                        notes: Vec::new(),
                        expanded: false,
                        started_at: Instant::now(),
                        settled: None,
                    },
                )
                .await;
            let edit = EditMessage::new()
                .content(render_content(&group))
                .embeds(render_embeds(&group))
                .components(render_components(&group, mid));
            if let Some(Err(e)) = super::governor::edit_chrome(
                &dstate.governor,
                channel,
                http,
                MessageId::new(mid),
                edit,
            )
            .await
            {
                tracing::warn!("Discord: detached row edit failed: {e}");
            }
        }
        DetachedRender::Post => {
            let group = GroupState {
                page: 0,
                entries,
                notes: Vec::new(),
                expanded: false,
                started_at: Instant::now(),
                settled: None,
            };
            let content = render_content(&group);
            match super::governor::say(
                &dstate.governor,
                channel,
                http,
                super::governor::Surface::Send,
                &content,
            )
            .await
            {
                Ok(sent) => {
                    let comps = render_components(&group, sent.id.get());
                    if !comps.is_empty()
                        && let Some(Err(e)) = super::governor::edit_chrome(
                            &dstate.governor,
                            channel,
                            http,
                            sent.id,
                            EditMessage::new()
                                .embeds(render_embeds(&group))
                                .components(comps),
                        )
                        .await
                    {
                        tracing::warn!("Discord: detached row component fixup failed: {e}");
                    }
                    dstate.upsert_tool_group(sent.id.get(), group).await;
                    *gmid.lock().await = Some(sent.id);
                }
                Err(e) => tracing::warn!("Discord: failed to post detached row block: {e}"),
            }
        }
    }
}

impl DiscordState {
    /// Retained tool groups in memory; older ones are restored from SQLite on
    /// their next click (#1880), so a card outlives the cap as well as a restart.
    const TOOL_GROUP_CAP: usize = 20;

    /// The message ids held in memory, oldest first. Test-only: the retention
    /// test asserts on memory directly, because a click on an evicted card
    /// legitimately restores it from SQLite whenever a pool is present (#1880).
    #[cfg(test)]
    pub(crate) async fn memory_group_ids(&self) -> Vec<u64> {
        let guard = self.tool_groups.lock().await;
        guard.0.clone()
    }

    /// Write a card's state through to SQLite (#1880). Best effort: a failed
    /// write is logged and the in-memory card keeps working.
    async fn persist_tool_group(&self, message_id: u64, group: &GroupState) {
        let Some(pool) = crate::db::database::global_pool() else {
            return;
        };
        let json = match serde_json::to_string(&super::tool_card_store::snapshot(group)) {
            Ok(json) => json,
            Err(e) => {
                tracing::warn!("Discord: could not encode tool card {message_id}: {e}");
                return;
            }
        };
        let repo = crate::db::repository::DiscordToolCardRepository::new(pool.clone());
        if let Err(e) = repo.set(message_id, &json).await {
            tracing::warn!("Discord: could not save tool card {message_id}: {e}");
        }
    }

    /// The stored card for a message that is no longer in memory, restored
    /// into the live map so later clicks hit memory (#1880). None when there is
    /// no stored card or it cannot be read.
    async fn restore_tool_group(&self, message_id: u64) -> Option<GroupState> {
        let pool = crate::db::database::global_pool()?;
        let repo = crate::db::repository::DiscordToolCardRepository::new(pool.clone());
        let json = match repo.get(message_id).await {
            Ok(Some(json)) => json,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!("Discord: could not read tool card {message_id}: {e}");
                return None;
            }
        };
        let stored: super::tool_card_store::StoredCard = match serde_json::from_str(&json) {
            Ok(stored) => stored,
            Err(e) => {
                tracing::warn!("Discord: stored tool card {message_id} is unreadable: {e}");
                return None;
            }
        };
        let group = super::tool_card_store::restore(stored);
        let mut guard = self.tool_groups.lock().await;
        let (order, map) = &mut *guard;
        if !map.contains_key(&message_id) {
            order.push(message_id);
            while order.len() > Self::TOOL_GROUP_CAP {
                let oldest = order.remove(0);
                map.remove(&oldest);
            }
            map.insert(message_id, group);
        }
        map.get(&message_id).cloned()
    }

    /// Insert or update a group, PRESERVING the stored expanded/collapsed
    /// choice and page on updates (a completing tool must not snap an expanded
    /// group shut) and the stored narration notes (only `append_note` and
    /// `drop_note_if` mutate those). Returns the stored state so callers render
    /// what is kept.
    pub(crate) async fn upsert_tool_group(
        &self,
        message_id: u64,
        mut group: GroupState,
    ) -> GroupState {
        {
            let mut guard = self.tool_groups.lock().await;
            let (order, map) = &mut *guard;
            match map.get(&message_id) {
                Some(existing) => {
                    group.expanded = existing.expanded;
                    group.page = existing.page;
                    group.notes = existing.notes.clone();
                    group.started_at = existing.started_at;
                    group.settled = existing.settled.clone();
                }
                None => {
                    order.push(message_id);
                    while order.len() > Self::TOOL_GROUP_CAP {
                        let oldest = order.remove(0);
                        map.remove(&oldest);
                    }
                }
            }
            map.insert(message_id, group.clone());
        }
        self.persist_tool_group(message_id, &group).await;
        group
    }

    /// Set a group's card page; None when the card is in neither memory nor
    /// SQLite (#1880).
    pub(crate) async fn set_tool_group_page(
        &self,
        message_id: u64,
        page: usize,
    ) -> Option<GroupState> {
        let updated = self
            .edit_tool_group(message_id, |group| group.page = page)
            .await?;
        self.persist_tool_group(message_id, &updated).await;
        Some(updated)
    }

    /// Flip a group's expanded state; None when the card is in neither memory
    /// nor SQLite.
    pub(crate) async fn toggle_tool_group(&self, message_id: u64) -> Option<GroupState> {
        let updated = self
            .edit_tool_group(message_id, |group| group.expanded = !group.expanded)
            .await?;
        self.persist_tool_group(message_id, &updated).await;
        Some(updated)
    }

    /// Apply `edit` to a card in memory, restoring it from SQLite first when it
    /// is not there (#1880).
    async fn edit_tool_group<F>(&self, message_id: u64, edit: F) -> Option<GroupState>
    where
        F: FnOnce(&mut GroupState),
    {
        {
            let mut guard = self.tool_groups.lock().await;
            let (_, map) = &mut *guard;
            if let Some(group) = map.get_mut(&message_id) {
                edit(group);
                return Some(group.clone());
            }
        }
        let mut restored = self.restore_tool_group(message_id).await?;
        edit(&mut restored);
        let mut guard = self.tool_groups.lock().await;
        let (_, map) = &mut *guard;
        map.insert(message_id, restored.clone());
        Some(restored)
    }

    /// Append one narration line to the stored group, keeping only the
    /// newest [`NOTE_CAP`]. Returns the updated state, or None when the
    /// message has no stored group (aged out of retention).
    pub(crate) async fn append_note(&self, message_id: u64, note: String) -> Option<GroupState> {
        let updated = {
            let mut guard = self.tool_groups.lock().await;
            let (_, map) = &mut *guard;
            let group = map.get_mut(&message_id)?;
            group.notes.push(note);
            if group.notes.len() > NOTE_CAP {
                group.notes.remove(0);
            }
            group.clone()
        };
        self.persist_tool_group(message_id, &updated).await;
        Some(updated)
    }

    /// Stamp the post-delivery status (#1841): freeze the clock at now and
    /// record the ctx budget line for the settled chrome. A `None` ctx keeps
    /// whatever a previous settle stamped, so a re-settle never clears the
    /// budget. `waiting` (Some verb, #1987) settles the line to the ⏳ form
    /// because the turn ended with background work alive; `outcome` (Some,
    /// #1911) settles it to that state's own icon and verb, so the Cancelled
    /// and Err delivery arms stop the ticker's clock with the turn and the
    /// group never claims success. `None` is the normal delivery: the line
    /// keeps the plain ✅/❌ counts chrome. Returns the updated state, or
    /// None when the message has no stored group (aged out of retention).
    pub(crate) async fn settle_tool_group(
        &self,
        message_id: u64,
        ctx: Option<String>,
        waiting: Option<String>,
        outcome: Option<FlowOutcome>,
    ) -> Option<GroupState> {
        let updated = {
            let mut guard = self.tool_groups.lock().await;
            let (_, map) = &mut *guard;
            let group = map.get_mut(&message_id)?;
            let prev_ctx = group.settled.as_ref().and_then(|s| s.ctx.clone());
            group.settled = Some(SettledStatus {
                elapsed: group.started_at.elapsed(),
                ctx: ctx.or(prev_ctx),
                waiting,
                outcome,
            });
            group.clone()
        };
        self.persist_tool_group(message_id, &updated).await;
        Some(updated)
    }

    /// Narrow or clear the waiting line of a settled group (#1987): the
    /// background-completion path recomputes both registries and either
    /// replaces the verb (some work left) or clears it (flip to finished).
    /// The frozen clock and ctx budget are untouched. Returns the updated
    /// state, or None when the group aged out of retention.
    pub(crate) async fn refresh_waiting_line(
        &self,
        message_id: u64,
        waiting: Option<String>,
    ) -> Option<GroupState> {
        let updated = {
            let mut guard = self.tool_groups.lock().await;
            let (_, map) = &mut *guard;
            let group = map.get_mut(&message_id)?;
            group.settled.as_mut()?.waiting = waiting;
            group.clone()
        };
        self.persist_tool_group(message_id, &updated).await;
        Some(updated)
    }

    /// Remember that this session's turn settled with live background work
    /// (#1987), so the completion path can find the group after the turn's
    /// closures are gone. One group per session; the newest settle wins.
    pub(crate) async fn register_waiting_group(&self, session_id: Uuid, message_id: u64) {
        self.waiting_groups
            .lock()
            .await
            .insert(session_id, message_id);
    }

    /// The message id of a session's waiting group, if one is registered.
    pub(crate) async fn waiting_group_for(&self, session_id: Uuid) -> Option<u64> {
        self.waiting_groups.lock().await.get(&session_id).copied()
    }

    /// Drop a session's waiting-group registration (flipped to finished or
    /// the group aged out).
    pub(crate) async fn clear_waiting_group(&self, session_id: Uuid) -> Option<u64> {
        self.waiting_groups.lock().await.remove(&session_id)
    }

    /// Clone the live or settled group for out-of-loop renderers (#1843):
    /// the flow ticker snapshots under the lock, renders outside it, and
    /// re-snapshots after its edit so the settled line keeps the last word.
    /// A card that left memory is restored from SQLite (#1880).
    pub(crate) async fn tool_group_snapshot(&self, message_id: u64) -> Option<GroupState> {
        {
            let guard = self.tool_groups.lock().await;
            let (_, map) = &*guard;
            if let Some(group) = map.get(&message_id) {
                return Some(group.clone());
            }
        }
        self.restore_tool_group(message_id).await
    }

    /// Remove the LAST narration line matching `pred` — the final-response
    /// dedup drops the trailing note that mirrors the answer, so the trace
    /// does not double-post it as a clip. Returns the updated state, or None
    /// when nothing matched or no group is stored.
    pub(crate) async fn drop_note_if<F>(&self, message_id: u64, pred: F) -> Option<GroupState>
    where
        F: Fn(&str) -> bool,
    {
        let updated = {
            let mut guard = self.tool_groups.lock().await;
            let (_, map) = &mut *guard;
            let group = map.get_mut(&message_id)?;
            let idx = group.notes.iter().rposition(|n| pred(n))?;
            group.notes.remove(idx);
            group.clone()
        };
        self.persist_tool_group(message_id, &updated).await;
        Some(updated)
    }
}
