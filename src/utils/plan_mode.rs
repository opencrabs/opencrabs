//! Plan-mode command state machine, shared by TUI and Telegram: `/plan`,
//! `/show-plan`, `/execute` / Approve, and `/discard` semantics over the
//! plan lifecycle engine in [`crate::utils::plan_files`].
//!
//! Surfaces own the busy check (Approve and `/execute` are FORBIDDEN while
//! a turn is running: refuse immediately, never queue) and the dispatch of
//! the visible seed turn; everything idle-path and deterministic lives
//! here so the two surfaces cannot drift.

use crate::brain::goal::GoalManager;
use crate::services::ServiceContext;
use crate::tui::plan::{ApprovalSource, PlanStatus};
use crate::utils::plan_files::{self, PlanModeState};
use uuid::Uuid;

/// Result of an idle `/execute` / Approve attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApproveOutcome {
    /// The plan transitioned Editing -> Active (or an empty-tasks seed
    /// retry was accepted): dispatch this synthetic user message as one
    /// VISIBLE agent turn now.
    ///
    /// `prompt` is what the model reads. `display` is the one-line receipt
    /// the user sees, in the live bubble and in the history row that
    /// outlives the turn: the prompt itself is scaffolding and must never be
    /// painted as something the user typed (#2756).
    SeedTurn { prompt: String, display: String },
    /// The approve was refused; show this text to the user.
    Refused(String),
}

/// Approve validator (strictness locked): a lightweight scan of the
/// session `.md` headings and field labels. Any non-empty text after a
/// label passes; no placeholder heuristics.
pub fn validate_for_approve(md_body: &str) -> Result<(), String> {
    if md_body.trim().is_empty() {
        return Err("the session plan .md is empty".to_string());
    }
    let warnings = plan_files::template_section_warnings(md_body);
    if warnings.is_empty() {
        Ok(())
    } else {
        Err(warnings.join("; "))
    }
}

/// The locked implement-turn prompt dispatched as a synthetic, visible
/// user message after Approve. History and compaction can recover the
/// intent from it.
pub(crate) fn seed_prompt(md_path: &std::path::Path) -> String {
    format!(
        "[SYSTEM: PLAN APPROVED] The user approved the SESSION PLAN at {}. \
         Read its ## Implementation steps section. Emit exactly ONE `plan` \
         add_tasks call with ALL tasks, 1:1 with the numbered steps. Map \
         'Done when:' bullets to acceptance_criteria when present; prefer \
         criteria that name a runnable command and their expected outcome. \
         For steps without a 'Done when:' bullet, derive one checkable \
         criterion from the step's verifiable outcome instead of seeding \
         none (criteria-less completions log Uncertain under the Ralph \
         verification gate). Omit \
         dependencies unless step prose explicitly requires ordering \
         (depends on / after / blocked by). Then call `plan` start and \
         continue executing the checklist in this same turn. Do NOT edit \
         project files until start succeeds. The approval was already \
         acknowledged to the user — do NOT restate that the plan is approved or \
         announce that you are starting; go straight to the work.",
        md_path.display()
    )
}

/// Implement-turn prompt for approving a CHECKLIST-mode plan whose tasks were
/// authored during Editing. The tasks already exist (no design `.md` to seed
/// from), so the agent must NOT `add_tasks` again — just start and execute.
fn start_prompt() -> String {
    "[SYSTEM: PLAN APPROVED] The user approved the checklist. Its tasks are \
     already defined — do NOT call `plan` add_tasks again. Call `plan` start to \
     begin the first task, then execute the checklist in this same turn, \
     calling `plan` complete as each task finishes. Do NOT edit project files \
     until start succeeds. The approval was already acknowledged to the user — \
     do NOT restate that the plan is approved or announce that you are starting; \
     go straight to the work and report results as you go."
        .to_string()
}

/// The receipt a surface shows when a checklist plan is approved. One line,
/// the same words everywhere: the window, the terminal and the channels all
/// read this, so a user never has to learn a second vocabulary for the same
/// action (#2756).
pub const CHECKLIST_APPROVED_RECEIPT: &str =
    "✅ Plan approved with /execute. Checklist is now running.";

/// The receipt for the design track, where approval seeds the checklist from
/// the `.md` instead of running tasks that already exist.
pub const DESIGN_APPROVED_RECEIPT: &str =
    "✅ Plan approved with /execute. Seeding the checklist from the design document.";

/// Idle `/execute` / Approve. Allowed paths (locked):
///
/// 1. Checklist approve (post-init Editing, `tasks` already present): the
///    reviewed task list IS the plan, so set Active and dispatch a start turn;
///    no design-`.md` validation, no seed.
/// 2. Design approve (post-init Editing, no tasks, `.md` passes the validator):
///    set Active, stamp `approved_at`, return the seed turn that authors tasks
///    from the `.md`.
/// 3. Seed retry (Active, design `.md` present, `tasks` still empty):
///    re-dispatch the seed turn only; no second approve, no transition.
///
/// Everything else refuses with a deterministic message. The caller MUST
/// have already refused when a turn is in flight.
pub async fn try_approve(session_id: Uuid, source: ApprovalSource) -> ApproveOutcome {
    let md_path = plan_files::plan_md_path(session_id).await;
    match plan_files::plan_mode_state(session_id).await {
        PlanModeState::NoPlan => ApproveOutcome::Refused(
            "No plan to approve: this session has no live plan. Start one with /plan \
             (design) or ask for a checklist."
                .to_string(),
        ),
        PlanModeState::PreInitEditing => ApproveOutcome::Refused(
            "Nothing approvable yet: Plan mode is waiting for `plan init` to create \
             the design document. Let the agent draft it first (or /discard to leave \
             Plan mode)."
                .to_string(),
        ),
        PlanModeState::PostInitEditing => {
            let Some(mut plan) = plan_files::load_plan(session_id).await else {
                return ApproveOutcome::Refused(
                    "Plan JSON is unreadable; cannot approve. /discard and start over.".to_string(),
                );
            };
            // Checklist-mode plan: the user authored a task list during Editing
            // (init mode="checklist") and just reviewed it. The deliverable is
            // the checklist, not a design `.md` — the placeholder scaffold `.md`
            // only exists to hold the plan in Editing for approval. Validating
            // that empty scaffold as design prose wrongly refused a ready plan
            // (#573 audit: 6 real tasks, empty template). Approve straight to
            // Active and start executing; there is nothing to seed.
            if !plan.tasks.is_empty() {
                plan.approve(source);
                if let Err(e) = plan_files::save_plan(&plan).await {
                    return ApproveOutcome::Refused(format!(
                        "Failed to persist the approval: {e}. Try again."
                    ));
                }
                return ApproveOutcome::SeedTurn {
                    prompt: start_prompt(),
                    display: CHECKLIST_APPROVED_RECEIPT.to_string(),
                };
            }
            // Design track: no tasks yet, so the `.md` prose IS the plan.
            // Validate it, approve, and seed tasks from its numbered steps.
            let body = std::fs::read_to_string(&md_path).unwrap_or_default();
            if let Err(why) = validate_for_approve(&body) {
                return ApproveOutcome::Refused(format!(
                    "Plan not ready to approve: {why}. Fill the template in {} first.",
                    md_path.display()
                ));
            }
            // First approve: Editing -> Active + approved_at + source.
            // The .md freezes automatically (the gate keys off Active status).
            plan.approve(source);
            if let Err(e) = plan_files::save_plan(&plan).await {
                return ApproveOutcome::Refused(format!(
                    "Failed to persist the approval: {e}. The .md is untouched; try again."
                ));
            }
            ApproveOutcome::SeedTurn {
                prompt: seed_prompt(&md_path),
                display: DESIGN_APPROVED_RECEIPT.to_string(),
            }
        }
        PlanModeState::Active => {
            let Some(plan) = plan_files::load_plan(session_id).await else {
                return ApproveOutcome::Refused(
                    "Plan JSON is unreadable; cannot retry. /discard and start over.".to_string(),
                );
            };
            if !plan.tasks.is_empty() {
                return ApproveOutcome::Refused(
                    "The checklist is already Active: /execute is not applicable. \
                     Continue the checklist (or /discard to drop the plan)."
                        .to_string(),
                );
            }
            if !md_path.exists() {
                return ApproveOutcome::Refused(
                    "This checklist plan has no design document to seed from. \
                     Add tasks with the plan tool instead."
                        .to_string(),
                );
            }
            // Empty-tasks seed retry: re-validate the frozen .md, then
            // re-dispatch the seed turn. No status transition.
            let body = std::fs::read_to_string(&md_path).unwrap_or_default();
            if let Err(why) = validate_for_approve(&body) {
                return ApproveOutcome::Refused(format!(
                    "Cannot retry the seed: the frozen plan fails validation ({why}). \
                     /discard and re-plan."
                ));
            }
            ApproveOutcome::SeedTurn {
                prompt: seed_prompt(&md_path),
                display: DESIGN_APPROVED_RECEIPT.to_string(),
            }
        }
    }
}

/// `/plan`: set durable pre-init Editing. It does not create an
/// approvable plan; the agent must still call `plan init`.
pub async fn enter_plan_mode(session_id: Uuid) -> String {
    match plan_files::plan_mode_state(session_id).await {
        PlanModeState::PostInitEditing => {
            "A design plan is already being edited. Refine it, then approve with \
             /execute (or /discard it)."
                .to_string()
        }
        PlanModeState::Active => "A checklist is already Active for this session. Continue it, or \
             /discard it before planning something new."
            .to_string(),
        PlanModeState::PreInitEditing | PlanModeState::NoPlan => {
            // Explicit `/plan` slash: arm the flag with Slash origin so the
            // design track stays available under tool auto-approve (the yolo
            // review gate). A keyword soft-nudge keeps Nudge origin instead.
            match plan_files::set_pre_init_editing_with_origin(
                session_id,
                plan_files::PreInitOrigin::Slash,
            )
            .await
            {
                Ok(()) => "📋 Plan mode on. Describe what you want planned: the agent \
                           will explore, then draft a design document for your approval. \
                           Project writes stay blocked until you approve. Leave with \
                           /discard."
                    .to_string(),
                Err(e) => format!("Could not enter Plan mode: {e}"),
            }
        }
    }
}

/// `/discard`: clear the pre-init flag or delete plan artifacts,
/// returning the session to NoPlan. The caller cancels an in-flight turn
/// first when needed. Clears any goal a started task set, mirroring the
/// complete/skip paths, so discarding never leaves stale goal chrome.
pub async fn discard(session_id: Uuid, svc: &ServiceContext) -> String {
    match plan_files::plan_mode_state(session_id).await {
        PlanModeState::NoPlan => "No live plan to discard.".to_string(),
        PlanModeState::PreInitEditing => {
            plan_files::discard_plan(session_id).await;
            "Plan mode off (pre-init flag cleared).".to_string()
        }
        PlanModeState::PostInitEditing | PlanModeState::Active => {
            plan_files::discard_plan(session_id).await;
            if let Err(e) = GoalManager::new(svc.clone()).clear_goal(session_id).await {
                tracing::warn!("Failed to clear plan goal on discard: {e}");
            }
            "🗑️ Plan discarded: design document and checklist removed. The session \
             has no live plan."
                .to_string()
        }
    }
}

/// Cap for the design prose echoed by `/show-plan`. Telegram's hard message
/// limit is 4096 chars; leave headroom for the title, ready-line, and the
/// document path rendered around the body.
const PLAN_PROSE_CAP: usize = 3000;

/// `/show-plan`: a text summary of the current plan state. Surfaces may
/// additionally restick chrome (Telegram) or open the overlay (TUI).
///
/// Demoted on Telegram (ADR 0005 Decision 14): the flow message renders the
/// design prose per-heading, so this command is a thin fallback for an idle
/// or buried chat and for send-failure recovery — chrome never teaches it as
/// the way to read the plan. The TUI overlay keeps using it as before.
pub async fn show_plan(session_id: Uuid) -> String {
    show_plan_opt(session_id, false).await
}

pub async fn show_plan_opt(session_id: Uuid, full: bool) -> String {
    let summary = match plan_files::plan_mode_state(session_id).await {
        PlanModeState::NoPlan => "No active plan for this session.".to_string(),
        PlanModeState::PreInitEditing => {
            "Plan mode is on (pre-init): no design document yet. The agent still \
             needs to run `plan init`."
                .to_string()
        }
        PlanModeState::PostInitEditing => {
            let md = plan_files::plan_md_path(session_id).await;
            let body = std::fs::read_to_string(&md).unwrap_or_default();
            let title = plan_files::load_plan(session_id)
                .await
                .map(|p| p.title)
                .unwrap_or_default();
            let ready = match validate_for_approve(&body) {
                Ok(()) => "Ready to approve: /execute.".to_string(),
                Err(why) => format!("Not approvable yet: {why}."),
            };
            // Surface the design prose itself, not just its path: a reviewer on
            // a chat channel has no file access, so the document body IS the
            // thing under review (#540). Truncated to stay inside one chat
            // message; the untruncated text is always on disk at `md`.
            let trimmed = body.trim();
            let prose = if trimmed.is_empty() {
                "(design document is still empty)".to_string()
            } else {
                crate::utils::truncate_str(trimmed, PLAN_PROSE_CAP).to_string()
            };
            format!(
                "📋 Editing design plan{}\n{ready}\n\n{prose}\n\nDocument: {}",
                if title.is_empty() {
                    String::new()
                } else {
                    format!(": {title}")
                },
                md.display()
            )
        }
        PlanModeState::Active => {
            let Some(plan) = plan_files::load_plan(session_id).await else {
                return "Plan JSON is unreadable.".to_string();
            };
            format_active_checklist_windowed(&plan, full)
        }
    };
    match telegram_card_report(session_id).await {
        Some(report) => format!("{summary}\n\n{report}"),
        None => summary,
    }
}

/// #1684: the plan card this session owns on Telegram, plus any other card live
/// in the same chat.
///
/// The card is the plan as the user sees it, and until the render was recorded
/// the agent could not read it at all: an orphaned card stayed alive for days
/// because the only reader was a human with a sqlite prompt. `None` when there
/// is no database or no tracked card, so every other surface is unchanged.
///
/// Chat-scoped as well as session-scoped, on purpose: the incident's card was
/// tracked under the chat's no-topic fallback session while the plan work ran
/// in another session, so a session-only read would still have missed it.
async fn telegram_card_report(session_id: Uuid) -> Option<String> {
    let pool = crate::db::database::global_pool()?;
    let repo = crate::db::repository::PlanCardRepository::new(pool.clone());
    let card = repo.get(&session_id.to_string()).await.ok()??;
    let topic = match card.thread_id {
        Some(t) => format!("topic {t}"),
        None => "no topic".to_string(),
    };
    // The stored `signature` is the rendered text plus a `\u{1}`-separated
    // keyboard debug tail. That tail is a `{:?}` of the inline keyboard: never
    // something to quote to a model, so the report stops at the separator.
    let text = card.signature.split('\u{1}').next().unwrap_or_default();
    let mut lines = vec![format!(
        "Telegram plan card for this session: message {} in chat {} ({}).",
        card.message_id, card.chat_id, topic
    )];
    if let Ok(cards) = repo.list_by_chat(card.chat_id).await {
        let others: Vec<String> = cards
            .iter()
            .filter(|c| c.session_id != session_id.to_string())
            .map(|c| format!("session {} on message {}", c.session_id, c.message_id))
            .collect();
        if !others.is_empty() {
            lines.push(format!(
                "Other sessions also have a card in this chat: {}.",
                others.join(", ")
            ));
        }
    }
    if !text.trim().is_empty() {
        lines.push(format!("Card text as stored:\n{text}"));
    }
    Some(lines.join("\n"))
}

/// The Active-checklist summary shared by `/show-plan` and the TUI plan
/// overlay: the seed-incomplete notice when tasks are empty, otherwise the
/// checklist with progress. Pure (takes the already-loaded plan), so the
/// sync render path can reuse it without touching the async store.
/// When `full` is false and plan has >10 tasks, focal accordion windowing
/// is applied to avoid overflowing LLM context.
pub fn format_active_checklist(plan: &crate::tui::plan::PlanDocument) -> String {
    format_active_checklist_windowed(plan, false)
}

/// Variant of `format_active_checklist` with optional full uncollapsed rendering.
pub fn format_active_checklist_windowed(
    plan: &crate::tui::plan::PlanDocument,
    full: bool,
) -> String {
    if plan.tasks.is_empty() {
        return format!(
            "📋 {} is Active but the checklist is still empty (seed did not \
             finish). Retry with /execute, or /discard.",
            plan.title
        );
    }
    let done = plan
        .tasks
        .iter()
        .filter(|t| {
            matches!(
                t.status,
                crate::tui::plan::TaskStatus::Completed | crate::tui::plan::TaskStatus::Skipped
            )
        })
        .count();

    let total = plan.tasks.len();
    if full || total <= 10 {
        let lines = plan
            .tasks
            .iter()
            .map(|t| {
                format!(
                    "{} {}. {}",
                    crate::tui::plan::status_mark(&t.status),
                    t.order,
                    t.title
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return format!("📋 {} (Active, {done}/{total} done)\n{lines}", plan.title);
    }

    // Focal accordion windowing for > 10 tasks:
    // 1. Find active index (first InProgress or first Pending)
    let active_idx = plan
        .tasks
        .iter()
        .position(|t| matches!(t.status, crate::tui::plan::TaskStatus::InProgress))
        .or_else(|| {
            plan.tasks
                .iter()
                .position(|t| matches!(t.status, crate::tui::plan::TaskStatus::Pending))
        })
        .unwrap_or(total.saturating_sub(1));

    // Visible window around active: [active_idx - 1 ..= active_idx + 2]
    let win_start = active_idx.saturating_sub(1);
    let win_end = (active_idx + 3).min(total.saturating_sub(2)); // leave room for terminal 2

    let mut lines = Vec::new();

    // Completed collapsed prefix
    if win_start > 0 {
        lines.push(format!("✓ [Tasks #1–#{win_start} completed]"));
    }

    // Active horizon
    for t in &plan.tasks[win_start..win_end] {
        lines.push(format!(
            "{} {}. {}",
            crate::tui::plan::status_mark(&t.status),
            t.order,
            t.title
        ));
    }

    // Distant pending collapsed
    let terminal_start = total.saturating_sub(2).max(win_end);
    if terminal_start > win_end {
        let count = terminal_start - win_end;
        let start_order = win_end + 1;
        let end_order = terminal_start;
        lines.push(format!(
            "… [{count} pending tasks (#{start_order}–#{end_order}) collapsed]"
        ));
    }

    // Terminal bracket (final 1-2 tasks, e.g. gates / smoke / PR)
    for t in &plan.tasks[terminal_start..total] {
        lines.push(format!(
            "{} {}. {}",
            crate::tui::plan::status_mark(&t.status),
            t.order,
            t.title
        ));
    }

    format!(
        "📋 {} (Active, {done}/{total} done)\n{}",
        plan.title,
        lines.join("\n")
    )
}

/// Whether the session is in the seed window: Active design plan whose
/// checklist has not successfully started (empty tasks, or tasks present
/// but none ever left Pending). Surfaces show Building checklist… chrome
/// while a seed turn is in flight in this window.
pub async fn in_seed_window(session_id: Uuid) -> bool {
    if plan_files::plan_mode_state(session_id).await != PlanModeState::Active {
        return false;
    }
    if !plan_files::plan_md_path(session_id).await.exists() {
        return false;
    }
    plan_files::load_plan(session_id).await.is_some_and(|p| {
        p.status == PlanStatus::Active
            && p.tasks
                .iter()
                .all(|t| matches!(t.status, crate::tui::plan::TaskStatus::Pending))
    })
}
