//! Flow-message chrome: the always-visible sections (plan title, checklist
//! progress, active goal, ctx footer) rendered onto the per-turn flow
//! message, plus the shared live-header tick used by the main handler and
//! the crash-recovery resume loop.
//!
//! Telegram turn chrome is the per-turn flow message (`open_group_msg_id`),
//! not a Bot API pin and not a separate pre-block status bubble. Sections
//! read live data as-is: the session plan JSON for title and checklist, and
//! `GoalManager` for the active goal one-liner. Empty sections are omitted.

use super::flow::{
    COMPACTING_HEADER_TEXT, HeaderMarkup, StreamingState, humanize_duration, open_flow,
    refresh_flow, starts_with_icon,
};
use super::handler::escape_html;
use crate::brain::agent::AgentService;
use crate::brain::goal::GoalManager;

use std::sync::Arc;
use teloxide::prelude::*;
use uuid::Uuid;

/// Longest plan-title / goal text shown in flow chrome before truncation.
/// Raised from 60 to 150 (#1053): 60 clipped meaningful plan titles and
/// checklist items mid-phrase. 150 shows near-full text for realistic titles
/// without making the card heavy with many long items. One cap for both
/// title and checklist rows — a separate per-surface cap was considered and
/// deferred until a real case asks for it.
const SECTION_TEXT_CAP: usize = 150;

/// Which plan keyboard the latest flow message owns. Keyboards attach only
/// after `plan init` succeeds: Approve + Discard while the design plan is
/// Editing, Discard only while a checklist is Active, none otherwise.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanKb {
    #[default]
    None,
    /// Editing design plan: ✅ Approve + 🗑 Discard.
    ApproveDiscard,
    /// Active checklist: 🗑 Discard only.
    DiscardOnly,
}

impl PlanKb {
    /// Inline keyboard for this state, `None` when no buttons apply.
    /// Callback data uses the `plan:` prefix, deliberately distinct from
    /// tool-approval `approve:{id}` so the two can never collide.
    pub(crate) fn keyboard(self) -> Option<teloxide::types::InlineKeyboardMarkup> {
        use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
        match self {
            PlanKb::None => None,
            PlanKb::ApproveDiscard => Some(InlineKeyboardMarkup::new(vec![vec![
                InlineKeyboardButton::callback("✅ Approve plan", "plan:ok"),
                InlineKeyboardButton::callback("🗑 Discard", "plan:no"),
            ]])),
            PlanKb::DiscardOnly => Some(InlineKeyboardMarkup::new(vec![vec![
                InlineKeyboardButton::callback("🗑 Discard plan", "plan:no"),
            ]])),
        }
    }
}

/// One top-level section of the session plan `.md` prose (ADR 0005
/// Decision 12): `heading` is the `##` text (`None` for the orphan preamble
/// before the first top-level heading) and `body` is the raw markdown under
/// it, nested `###` headings, lists, and tables included.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProseSection {
    pub(crate) heading: Option<String>,
    pub(crate) body: String,
}

/// Split session-plan markdown into per-top-level-heading prose sections
/// (ADR 0005 Decision 12): strip the leading `# …` H1 (the plan title block
/// already carries it), then cut on `##` headings. Text before the first
/// `##` becomes the orphan preamble (`heading: None`). Fenced code lines are
/// never treated as headings. Sections whose body is empty are dropped — a
/// heading with nothing under it has nothing to disclose.
pub(crate) fn split_plan_prose(md: &str) -> Vec<ProseSection> {
    let mut raw: Vec<(Option<String>, Vec<&str>)> = vec![(None, Vec::new())];
    let mut in_fence = false;
    let mut seen_content = false;
    let mut h1_stripped = false;
    for line in md.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            seen_content = true;
            raw.last_mut().expect("raw starts non-empty").1.push(line);
            continue;
        }
        if !in_fence {
            if !h1_stripped && !seen_content && trimmed.starts_with("# ") {
                h1_stripped = true;
                seen_content = true;
                continue;
            }
            if let Some(h) = trimmed.strip_prefix("## ") {
                let h = h.trim();
                if !h.is_empty() {
                    raw.push((Some(h.to_string()), Vec::new()));
                    seen_content = true;
                    continue;
                }
            }
        }
        if !trimmed.is_empty() {
            seen_content = true;
        }
        raw.last_mut().expect("raw starts non-empty").1.push(line);
    }
    raw.into_iter()
        .filter_map(|(heading, lines)| {
            let body = lines.join("\n").trim().to_string();
            (!body.is_empty()).then_some(ProseSection { heading, body })
        })
        .collect()
}

/// Format the markdown body of one prose section into per-line Telegram
/// HTML: fenced code lines as `<code>`, `#` headings as bold, list items as
/// bullets, inline markdown elsewhere. Blank source lines come through as
/// The flow-message Goal section (ADR 0005 Decision 10): the goal text plus
/// whether it is a retained completed goal (the live `GoalManager` entry
/// completed or cleared mid-turn). A completed goal keeps the `🎯 Goal:`
/// prefix while the turn runs and swaps only the icon to `✅ Goal:` on the
/// settled render; an active goal is `🎯 Goal:` everywhere.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GoalSection {
    pub(crate) text: String,
    pub(crate) completed: bool,
}

impl GoalSection {
    /// Bold section prefix with the Decision 10 icon: `✅` only for a
    /// completed goal on a settled render, `🎯` everywhere else. The `Goal:`
    /// word never changes.
    pub(crate) fn prefix(&self, settled: bool) -> String {
        let icon = if self.completed && settled {
            "✅"
        } else {
            "🎯"
        };
        format!("<b>{icon} Goal:</b>")
    }
}

/// Always-visible flow sections. Built from live data by [`refresh_sections`];
/// rendered by [`FlowSections::chrome_rich`] / [`FlowSections::chrome_classic`]
/// so the two flow surfaces can never drift on section formatting.
#[derive(Default, Clone, PartialEq)]
pub(crate) struct FlowSections {
    /// Plan-mode state line: Editing prose summary, Building checklist…,
    /// or seed-error chrome. Leads the chrome line when present.
    pub(crate) plan_state: Option<String>,
    /// Plan keyboard the flow message should carry (attached on every
    /// open/edit; Telegram clears reply_markup on edits that omit it).
    pub(crate) plan_kb: PlanKb,
    /// Plan title from the live session plan JSON, when set.
    pub(crate) plan_title: Option<String>,
    /// Per-top-level-heading prose sections from the session plan `.md`
    /// (ADR 0005 Decision 12), `None` when the session has no design prose.
    pub(crate) prose: Option<Vec<ProseSection>>,
    /// Full checklist rows from the plan JSON `tasks[]`, each pre-marked with
    /// the ballot glyph (`☑ done` / `☐ undone`), raw and unescaped. `None`
    /// until a checklist has tasks; the full list is kept even when every task
    /// is done, through the completing turn's settle (ADR 0005 Decision 9).
    pub(crate) checklist: Option<Vec<String>>,
    /// Goal section from `GoalManager` (ADR 0005 Decision 10): the active
    /// goal, or one that completed earlier this turn and is retained until
    /// settle. Never set while the plan is Editing.
    pub(crate) goal: Option<GoalSection>,
    /// Ctx budget footer (display-only), set at final delivery.
    pub(crate) ctx: Option<String>,
}

impl FlowSections {
    fn has_prose(&self) -> bool {
        self.prose.as_ref().is_some_and(|p| !p.is_empty())
    }

    /// Rich-path plan chrome in reading order (ADR 0005 Decision 3): title,
    /// per-heading prose `<details>`, `<hr>`, checklist rows, `<hr>`, goal.
    /// The title sits flush against the prose (Decision 13); `<hr>` appears
    /// before the checklist only when prose precedes it, and before the goal
    /// when prose or checklist precedes it. Section headings are inline
    /// `<summary>` text only — nested blocks inside a summary render as a
    /// blank disclosure (Decision 12). A one-paragraph goal stays plain; a
    /// multi-paragraph goal collapses with the first paragraph as its inline
    /// summary. `settled` drives the Decision 10 goal icon. Empty when no
    /// plan sections are present; `plan_state` and `ctx` live in the merged
    /// footer.
    pub(crate) fn chrome_rich(&self, settled: bool) -> String {
        let mut out = String::new();
        if let Some(ref t) = self.plan_title {
            out.push_str(&format!("<p>📋 <b>{}</b></p>", escape_html(t)));
        }
        if let Some(ref sections) = self.prose {
            for sec in sections {
                let body: String = super::rich::markdown_to_html_p(&sec.body);
                match &sec.heading {
                    Some(h) => out.push_str(&format!(
                        "<details><summary>{}</summary>{body}</details>",
                        escape_html(h)
                    )),
                    // Orphan preamble: plain always-visible blocks.
                    None => out.push_str(&body),
                }
            }
        }
        if let Some(ref rows) = self.checklist {
            if self.has_prose() {
                out.push_str("<hr>");
            }
            for row in rows {
                out.push_str(&format!("<p>{}</p>", escape_html(row)));
            }
        }
        if let Some(ref g) = self.goal {
            let paras: Vec<&str> = g
                .text
                .split("\n\n")
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect();
            if !paras.is_empty() {
                if self.checklist.is_some() || self.has_prose() {
                    out.push_str("<hr>");
                }
                let prefix = g.prefix(settled);
                if let [one] = paras.as_slice() {
                    out.push_str(&format!("<p>{prefix} {}</p>", escape_html(one)));
                } else {
                    let body: String = paras[1..]
                        .iter()
                        .map(|p| format!("<p>{}</p>", escape_html(p)))
                        .collect();
                    out.push_str(&format!(
                        "<details><summary>{prefix} {}</summary>{body}</details>",
                        escape_html(paras[0])
                    ));
                }
            }
        }
        out
    }

    /// Markdown-plane plan chrome: [`Self::chrome_rich`] in the rich API's
    /// markdown dialect. The rich parser reads `<details>` / `<summary>` out of
    /// markdown too, so the per-heading prose and a multi-paragraph goal keep
    /// their disclosure; the rich `<hr>` has no markdown equivalent, so a blank
    /// line stands in for it (Decision 13). Section headings stay inline
    /// `<summary>` text only, for the same reason as the rich path (nested
    /// blocks render as a blank disclosure). `settled` drives the Decision 10
    /// goal icon.
    pub(crate) fn chrome_markdown(&self, settled: bool) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(ref t) = self.plan_title {
            parts.push(format!("📋 **{}**", t.trim()));
        }
        if let Some(ref sections) = self.prose {
            for sec in sections {
                let body = sec.body.trim();
                match &sec.heading {
                    Some(h) => parts.push(format!(
                        "<details><summary>{}</summary>\n\n{body}\n\n</details>",
                        h.trim()
                    )),
                    // Orphan preamble: plain always-visible blocks.
                    None => parts.push(body.to_string()),
                }
            }
        }
        if let Some(ref rows) = self.checklist {
            parts.push(rows.join("\n"));
        }
        if let Some(ref g) = self.goal {
            let paras: Vec<&str> = g
                .text
                .split("\n\n")
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect();
            if !paras.is_empty() {
                let prefix = g.prefix(settled);
                if let [one] = paras.as_slice() {
                    parts.push(format!("{prefix} {one}"));
                } else {
                    parts.push(format!(
                        "<details><summary>{prefix} {}</summary>\n\n{}\n\n</details>",
                        paras[0],
                        paras[1..].join("\n\n")
                    ));
                }
            }
        }
        parts.join("\n\n")
    }

    /// Classic-path plan chrome: the same locked vertical order with blank
    /// lines standing in for the rich `<hr>` (Decision 13 — classic HTML has
    /// no divider primitive). Prose sections are `<blockquote expandable>`
    /// blocks whose first line is the bold heading, so Telegram's collapsed
    /// peek shows it (Decision 12); the orphan preamble stays plain. The
    /// goal always renders as its own expandable with Telegram's peek as the
    /// collapsed preview; `settled` drives the Decision 10 goal icon.
    pub(crate) fn chrome_classic(&self, settled: bool) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(ref t) = self.plan_title {
            parts.push(format!("📋 <b>{}</b>", escape_html(t)));
        }
        if let Some(ref sections) = self.prose {
            for sec in sections {
                let body = super::rich::markdown_to_html(&sec.body);
                match &sec.heading {
                    Some(h) => parts.push(format!(
                        "<blockquote expandable><b>{}</b>\n{body}</blockquote>",
                        escape_html(h)
                    )),
                    None => parts.push(body),
                }
            }
        }
        if let Some(ref rows) = self.checklist {
            if self.has_prose() {
                parts.push(String::new());
            }
            for row in rows {
                parts.push(escape_html(row));
            }
        }
        if let Some(ref g) = self.goal {
            let text = g.text.trim();
            if !text.is_empty() {
                if self.checklist.is_some() || self.has_prose() {
                    parts.push(String::new());
                }
                parts.push(format!(
                    "<blockquote expandable>{} {}</blockquote>",
                    g.prefix(settled),
                    escape_html(text)
                ));
            }
        }
        parts.join("\n")
    }
}

/// Format an elapsed duration as the locked flow clock glyph `⏱ M:SS`
/// (`⏱ H:MM:SS` past an hour) — ADR 0005 Decision 13. This is the last
/// segment of every merged footer; never render a bare `M:SS` without the
/// glyph.
pub(crate) fn clock_glyph(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("⏱ {h}:{m:02}:{s:02}")
    } else {
        format!("⏱ {m}:{s:02}")
    }
}

/// Inputs to the merged flow footer (ADR 0005 Decision 12). The renderer
/// decomposes its `FlowHeader` / lines / sections into these primitives so the
/// footer join lives in one place and both the classic and rich paths agree.
pub(crate) struct FooterParts<'a> {
    /// Settled outcome `(icon, verb)` (e.g. `("✅", "Finished")`) once the turn
    /// ends; `None` while live. Drives segment 1 and drops the in-flight cog.
    pub(crate) outcome: Option<(&'a str, &'a str)>,
    /// Plan-mode status line (Decision 7) when in Plan mode; second segment on
    /// a live turn (after the activity, #1052).
    pub(crate) plan_state: Option<&'a str>,
    /// Non-plan "Working on …" / thinking preview; live-turn fallback for the
    /// reasoning segment (#1052: after the activity, not before it).
    pub(crate) working_on: Option<&'a str>,
    /// Latest-activity preview; LEADS the live footer (#1052). Shown only
    /// while live and only when a log exists.
    pub(crate) activity: Option<&'a str>,
    /// Count of tool entries in the log (`N tool calls` when `>= 1`).
    pub(crate) tool_count: usize,
    /// Whether a processing log exists at all (drives segment 2 presence).
    pub(crate) has_log: bool,
    /// Ctx budget string (segment 3), display-only, before the clock.
    pub(crate) ctx: Option<&'a str>,
    /// Elapsed wall-clock seconds for the segment-4 clock glyph.
    pub(crate) elapsed_secs: u64,
    /// Background-work indicator (#1054): `Some(label)` when detached tasks
    /// are still running at settle time. Renders as the final footer segment
    /// `🔧 <label> running` (or `🔧 N tasks running`) after the clock.
    pub(crate) bg: Option<&'a str>,
}

/// Build the merged flow footer: one ` • `-joined string (ADR 0005 Decision
/// 12, amended by #1052). Settled: outcome → tool count → ctx → clock. Live:
/// latest activity → reasoning/status → tool count → ctx → clock, because the
/// narration (what the agent is DOING) is the progress signal and the
/// reasoning excerpt is supplementary (#1052). The renderer wraps it: rich as
/// `<sub>` (plain footer line, or the processing-log `<summary>`); classic as
/// a plain final line. In-flight the log summary carries the `⚙️` cog; a
/// settled footer never does (segment 1 carries the `✅`/`❌` outcome
/// instead).
pub(crate) fn merged_footer(parts: &FooterParts, markup: HeaderMarkup) -> String {
    let esc = |s: &str| match markup {
        HeaderMarkup::Html => escape_html(s),
        HeaderMarkup::Markdown => s.to_string(),
    };
    let settled = parts.outcome.is_some();
    let mut segs: Vec<String> = Vec::new();

    // Segment 1 — settled outcome leads. LIVE turns lead with the latest
    // activity (#1052), then the reasoning/status. Strip a leading cog from
    // the activity so the prefix is never doubled (#509 follow-up).
    let mut live_activity = String::new();
    if let Some((icon, verb)) = parts.outcome {
        segs.push(format!("{icon} {}", esc(verb)));
    } else {
        if parts.has_log
            && let Some(act) = parts.activity
        {
            let act = act.trim_start_matches(['⚙', '\u{fe0f}']).trim_start();
            if !act.is_empty() {
                // Gear is dropped when the activity already leads with its own
                // icon (#29 fix round, owner directive): `✅ bash git status`
                // and the `⏳ Compacting context — 66% full…` body entry render
                // bare. Plain-text activity keeps the running cog.
                live_activity = if starts_with_icon(act) {
                    esc(act)
                } else {
                    format!("⚙️ {}", esc(act))
                };
                segs.push(live_activity.clone());
            }
        }
        if let Some(ps) = parts.plan_state {
            segs.push(esc(ps));
        } else if let Some(w) = parts.working_on {
            segs.push(esc(w));
        }
    }

    // Segment 2 — progress-log summary, only when a log exists. Settled turns
    // show a bare tool-call count with no cog (the stale narration is dropped,
    // #498). Live turns show the count alone when the activity segment already
    // carries the cog, else the cog rides the count (#1052 split).
    if parts.has_log {
        // Once another segment already leads with an icon, the standing gear
        // has nothing left to signal (#29 fix round, owner directive): the
        // count renders bare and the bare-cog fallback is dropped.
        let gear_taken = segs.iter().any(|s| starts_with_icon(s));
        let mut seg2 = String::new();
        if parts.tool_count >= 1 {
            let count = format!("{} tool calls", parts.tool_count);
            seg2 = if settled || !live_activity.is_empty() || gear_taken {
                count
            } else {
                format!("⚙️ {count}")
            };
        } else if !settled && live_activity.is_empty() && !gear_taken {
            // In-flight log with no tools and no activity preview yet: a bare
            // cog beats an empty segment so the footer still reads as active.
            seg2 = "⚙️".to_string();
        }
        if !seg2.is_empty() {
            segs.push(seg2);
        }
    }

    // Segment 3 — ctx, before the clock.
    if let Some(c) = parts.ctx {
        segs.push(esc(c));
    }

    // Segment 4 — clock, always last (the #1054 background-task indicator
    // appends after it when present).
    segs.push(clock_glyph(parts.elapsed_secs));

    // Segment 5 — background-work indicator (#1054): a settled turn that ends
    // with detached work looks identical to a complete one without this, and
    // the typing indicator staying alive is too easy to miss.
    if let Some(bg) = parts.bg {
        segs.push(format!("🔧 {}", esc(bg)));
    }

    segs.join(" • ")
}

/// Read the plan title + full `☐`/`☑` checklist rows from the live session
/// plan JSON through the shared plan store, which maps legacy statuses onto
/// Editing/Active and resolves terminal ones (Completed archives, Cancelled
/// deletes) — so stale chrome never outlives the plan.
pub(crate) async fn load_plan_sections(session_id: Uuid) -> (Option<String>, Option<Vec<String>>) {
    // NO archive fallback here. An earlier attempt at #809 fell back to the
    // most recent archived plan whenever no live plan existed, which is true
    // FOREVER once a plan completes: the card resurrected a finished checklist
    // on every refresh and every restart, contaminating chats that had long
    // moved on. Rendering the final state has to be bounded to the completing
    // turn, not derived from "there is no live plan".
    let Some(plan) = crate::utils::plan_files::load_plan(session_id).await else {
        return (None, None);
    };
    plan_document_sections(&plan)
}

/// Title + checklist rows for a plan card, from any document — live or
/// archived (#1158). Same row shape either way: full ballot checklist
/// (ADR 0005 Decision 3, one `status_mark()` row per task), quality glyphs,
/// verification badges, section text cap. Empty `tasks` (Editing before the
/// seed) yield no checklist.
pub(crate) fn plan_document_sections(
    plan: &crate::tui::plan::PlanDocument,
) -> (Option<String>, Option<Vec<String>>) {
    let title = {
        let t = plan.title.trim();
        (!t.is_empty()).then(|| crate::utils::truncate_str(t, SECTION_TEXT_CAP).to_string())
    };
    let checklist = (!plan.tasks.is_empty()).then(|| {
        plan.tasks
            .iter()
            .map(|t| {
                let mark = crate::tui::plan::status_mark(&t.status);
                let title = crate::utils::truncate_str(t.title.trim(), SECTION_TEXT_CAP);
                let verification_badge = t
                    .verification
                    .map(|v| format!(" {}", v.badge()))
                    .unwrap_or_default();
                format!(
                    "{mark} {title}{}{}",
                    crate::tui::plan::quality_glyph_suffix(t),
                    verification_badge
                )
            })
            .collect()
    });
    (title, checklist)
}

/// Per-heading prose sections from the session plan `.md`, when it exists
/// (Editing, or Active where the approved design stays frozen on disk —
/// discard and archive both delete the file, so stale prose never outlives
/// the plan). `None` when the file is absent or yields no sections.
pub(crate) async fn load_plan_prose(session_id: Uuid) -> Option<Vec<ProseSection>> {
    let path = crate::utils::plan_files::plan_md_path(session_id).await;
    let body = match tokio::fs::read_to_string(&path).await {
        Ok(body) => body,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(
                    "Telegram flow chrome: plan prose read failed for {}: {e}",
                    path.display()
                );
            }
            return None;
        }
    };
    // Drop unfilled scaffold lines (empty `**Label:**` fields, empty `N.`
    // steps) so a checklist plan's blank `.md` template does not render as
    // hollow "Context / Problem: / Target state: / …" sections (#580). A filled
    // design plan keeps every non-empty line. Sections that become empty after
    // filtering are dropped entirely.
    prose_sections_from_md_body(&body)
}

/// Prose sections from session-plan markdown text: strips unfilled scaffold
/// lines (empty `**Label:**` fields, bare `N.` steps) and sections that end
/// up empty, so a blank template never renders as hollow headings (#580).
/// Shared by the live-prose loader and the archived-card finalizer (#1158).
pub(crate) fn prose_sections_from_md_body(body: &str) -> Option<Vec<ProseSection>> {
    let sections: Vec<ProseSection> = split_plan_prose(body)
        .into_iter()
        .filter_map(|mut sec| {
            let kept: Vec<&str> = sec
                .body
                .lines()
                .filter(|l| !is_empty_scaffold_line(l))
                .collect();
            let new_body = kept.join("\n").trim().to_string();
            (!new_body.is_empty()).then(|| {
                sec.body = new_body;
                sec
            })
        })
        .collect();
    (!sections.is_empty()).then_some(sections)
}

/// True for an unfilled session-plan scaffold line: a bold `**Label:**` field
/// with nothing after it, a bare numbered step `N.` with no text, or an empty
/// `- Done when:` criteria bullet. These are the `create_design_md` template
/// placeholders; hiding them keeps a **partially-filled design template**
/// from rendering as hollow sections while the model works through it (#580,
/// #1145). A filled `Done when: <criterion>` line is real content and stays.
pub(crate) fn is_empty_scaffold_line(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    let body = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .unwrap_or(t);
    // Empty `**Label:**` field (the colon sits inside the bold markers).
    if body.starts_with("**")
        && let Some(idx) = body.find(":**")
        && body[idx + 3..].trim().is_empty()
    {
        return true;
    }
    // Empty `Done when:` criteria bullet (the scaffold's per-step placeholder).
    if body == "Done when:" {
        return true;
    }
    // Empty `N.` numbered step.
    let digits: String = body.chars().take_while(char::is_ascii_digit).collect();
    if !digits.is_empty()
        && let Some(after) = body[digits.len()..].trim_start().strip_prefix('.')
        && after.trim().is_empty()
    {
        return true;
    }
    false
}

/// Live `GoalManager` entry for the session as `(text, completed)`: an
/// active goal, or one the turn-end judge already marked completed (the row
/// survives with `state = "completed"`). Full text — the Decision 12 goal
/// chrome handles one- vs multi-paragraph display. `None` when no row
/// exists (never set, cleared on discard, or deleted by `clear_task_goal`
/// on task complete — turn-local retention in [`refresh_sections`] covers
/// that last case).
pub(crate) async fn load_goal_section(
    agent: &AgentService,
    session_id: Uuid,
) -> Option<(String, bool)> {
    let mgr = GoalManager::new(agent.context().clone());
    match mgr.get_goal(session_id).await {
        Ok(Some(goal)) if goal.state == "active" || goal.state == "completed" => {
            let text = goal.goal_text.trim().to_string();
            (!text.is_empty()).then_some((text, goal.state == "completed"))
        }
        Ok(_) => None,
        Err(e) => {
            tracing::debug!("Telegram flow chrome: goal lookup failed: {e}");
            None
        }
    }
}

/// Pure plan-chrome decision: maps the plan state (plus whether the turn is
/// still running and whether we are inside the checklist-seed window) to the
/// header label and the keyboard the flow message should carry. Kept pure and
/// separate from the plan-file IO so the keyboard gating (#571) is unit-testable.
///
/// Editing keyboard: while the turn runs (`turn_active`) attach NO keyboard —
/// `/execute` and the Approve tap are both refused mid-turn (they would
/// fork/deadlock against the in-flight turn), so a button then only invites a
/// tap that bounces with "a turn is running". At settle, show Approve/Discard.
/// (An earlier attempt also gated Approve on the plan passing `validate_for_
/// approve`, but that HID the button on plans the user could actually approve —
/// far worse than the occasional early tap, which already gets a clear "not
/// ready" message. #574 follow-up.) The settle path re-runs `refresh_sections`
/// so the keyboard is recomputed with `turn_active == false` once the turn ends.
pub(crate) fn plan_state_chrome(
    mode: crate::utils::plan_files::PlanModeState,
    turn_active: bool,
    in_seed_window: bool,
) -> (Option<String>, PlanKb) {
    use crate::utils::plan_files::PlanModeState;
    match mode {
        PlanModeState::NoPlan => (None, PlanKb::None),
        PlanModeState::PreInitEditing => (Some("📝 Discussing plan".to_string()), PlanKb::None),
        PlanModeState::PostInitEditing => {
            let kb = if turn_active {
                PlanKb::None
            } else {
                PlanKb::ApproveDiscard
            };
            (Some("✍️ Editing plan".to_string()), kb)
        }
        PlanModeState::Active => {
            if in_seed_window {
                if turn_active {
                    (
                        Some("⏳ Building checklist…".to_string()),
                        PlanKb::DiscardOnly,
                    )
                } else {
                    (
                        Some("⚠️ Checklist seed incomplete • retry: /execute".to_string()),
                        PlanKb::DiscardOnly,
                    )
                }
            } else {
                (None, PlanKb::DiscardOnly)
            }
        }
    }
}

pub(crate) async fn load_plan_state_section(
    session_id: Uuid,
    turn_active: bool,
) -> (Option<String>, PlanKb) {
    use crate::utils::plan_files::{PlanModeState, plan_mode_state};
    let mode = plan_mode_state(session_id).await;
    // in_seed_window only matters (and only does IO) for the Active state.
    let in_seed_window = matches!(mode, PlanModeState::Active)
        && crate::utils::plan_mode::in_seed_window(session_id).await;
    plan_state_chrome(mode, turn_active, in_seed_window)
}

/// Reload the plan/goal sections from live data and store them on the
/// streaming state. Returns true when they changed (the flow needs a
/// re-render). The ctx section is owned by final delivery and preserved.
///
/// Goal retention (ADR 0005 Decision 10): the engine deletes the goal row
/// when a plan task completes, so the last active goal text sighted this
/// turn is kept on [`StreamingState::retained_goal`] and rendered as a
/// completed goal until settle. Retention renders only while a plan is
/// still live (discard goes to NoPlan and strips all plan chrome at once)
/// and never while Editing; the per-turn streaming state clears it for the
/// next turn.
pub(crate) async fn refresh_sections(
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    agent: &AgentService,
    session_id: Uuid,
) -> bool {
    use crate::utils::plan_files::{PlanModeState, plan_mode_state};
    // Plan title, prose, and checklist now live on the persistent plan card,
    // not in the per-turn flow block (#580, #621). The card reads them itself
    // via load_plan_sections / load_plan_prose, so they are not populated on
    // FlowSections here. The card is the single surface carrying title, prose,
    // checklist, and keyboard, so the flow block stays clean. Prose was still
    // loaded here after #621 folded it into the card, which duplicated the
    // design prose across two messages (Alexey, OC Dev 2026-07-27).
    let mode = plan_mode_state(session_id).await;
    let editing = matches!(
        mode,
        PlanModeState::PreInitEditing | PlanModeState::PostInitEditing
    );
    let live_goal = if editing {
        // The Goal section never renders during Editing (Decision 3).
        None
    } else {
        load_goal_section(agent, session_id).await
    };
    // Plan-state derivation reads plan files; keep that IO outside the
    // streaming lock (short double-lock beats file reads under the mutex).
    let turn_active = {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        s.flow_outcome.is_none()
    };
    let (plan_state, plan_kb) = load_plan_state_section(session_id, turn_active).await;
    let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
    let goal = match live_goal {
        Some((text, false)) => {
            // Active goal: show it and remember the text — several
            // completions in one turn keep the LAST goal only.
            s.retained_goal = Some(text.clone());
            Some(GoalSection {
                text,
                completed: false,
            })
        }
        // The turn-end judge marked it completed (row survives): retained
        // display, but only when it was sighted active earlier THIS turn —
        // a leftover completed row from a prior turn never re-renders.
        Some((text, true)) => s.retained_goal.is_some().then_some(GoalSection {
            text,
            completed: true,
        }),
        // Row gone. While a plan is Active that means clear_task_goal on a
        // task complete: keep the retained text until settle. In every
        // other state (Editing, or NoPlan after discard/clear) the goal
        // section is gone.
        None if mode == PlanModeState::Active => s.retained_goal.clone().map(|text| GoalSection {
            text,
            completed: true,
        }),
        None => None,
    };
    let next = FlowSections {
        plan_state,
        // plan_kb is still tracked here so the plan card can read it, but the
        // keyboard is attached to the CARD, not the flow block (#580).
        plan_kb,
        // Title, prose, and checklist moved to the persistent plan card
        // (#580, #621). The card owns the design prose entirely, so the flow
        // block never renders it (rendering it here duplicated the prose).
        plan_title: None,
        prose: None,
        checklist: None,
        goal,
        ctx: s.sections.ctx.clone(),
    };
    if s.sections == next {
        false
    } else {
        s.sections = next;
        true
    }
}

/// One live-header tick, shared by the main handler and resume edit loops so
/// they cannot drift: while the flow is open, roll the duration, the
/// thinking / Working-on preview, and the plan/goal sections, refreshing the
/// message when anything changed; while no flow is open and the turn is
/// still working, open the flow header-only on this first activity tick
/// (the pre-block status bubble is gone; the flow header owns early-turn
/// status).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn tick_flow_header(
    bot: &Bot,
    chat: ChatId,
    thread_id: Option<teloxide::types::ThreadId>,
    streaming: &Arc<std::sync::Mutex<StreamingState>>,
    agent: &AgentService,
    session_id: Uuid,
    show_status: bool,
    turn_done: bool,
    preview: Option<String>,
    mut needs_refresh: bool,
) {
    let open_block = {
        let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
        s.open_group_msg_id
    };
    // Compaction pin (#29): while the summarizer runs, nothing streams, so
    // pin the header to the dedicated compacting state instead of whatever
    // stale preview the caller computed. The CompactionSummary arm clears
    // the flag; the next tick recomputes normally.
    let preview = {
        let compacting = {
            let s = streaming.lock().unwrap_or_else(|e| e.into_inner());
            s.compacting
        };
        if compacting {
            Some(COMPACTING_HEADER_TEXT.to_string())
        } else {
            preview
        }
    };
    if open_block.is_some() {
        if show_status {
            let changed = {
                let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                let elapsed = s.turn_started_at.elapsed().as_secs();
                let mut changed = false;
                let duration = (elapsed > 0).then(|| humanize_duration(elapsed));
                if duration.is_some() && s.flow_status != duration {
                    s.flow_status = duration;
                    changed = true;
                }
                if s.header_preview != preview {
                    s.header_preview = preview;
                    changed = true;
                }
                changed
            };
            needs_refresh |= changed;
            needs_refresh |= refresh_sections(streaming, agent, session_id).await;
        }
        if needs_refresh {
            // Chrome-tick render (#1211 G2): the clock/preview ladder class.
            // Dropping one tick is invisible — the next tick re-renders full
            // state — which is exactly the self-healing the drop ladder
            // relies on for everything below `Final`.
            refresh_flow(bot, chat, streaming, super::governor::EditClass::Clock).await;
        }
    } else {
        if needs_refresh {
            refresh_flow(bot, chat, streaming, super::governor::EditClass::Clock).await;
        }
        if show_status && !turn_done {
            // Merge pre-flow into the flow message: first activity tick opens
            // the flow header-only, thinking / Working-on riding the header.
            {
                let mut s = streaming.lock().unwrap_or_else(|e| e.into_inner());
                s.header_preview = preview;
                let elapsed = s.turn_started_at.elapsed().as_secs();
                if elapsed > 0 {
                    s.flow_status = Some(humanize_duration(elapsed));
                }
            }
            refresh_sections(streaming, agent, session_id).await;
            open_flow(bot, chat, thread_id, streaming).await;
        }
    }
}
