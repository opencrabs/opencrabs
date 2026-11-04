//! Plan-mode tool gate: the tool-loop enforcement of the Editing
//! write policy and the Active `.md` freeze.
//!
//! Checked on every tool execution (registry choke point). The gate is
//! driven entirely by the MCP-style [`ToolHints`] risk axes: a tool's
//! `read_only` flag is the sole signal. There are no name lists; the
//! hint model is the single source of truth.
//!
//! - **Pre-init Editing** (durable flag, no session `.md` yet):
//!   read-only tools pass; mutators (`read_only = false`) go to
//!   `RequireApproval` so the user can confirm any side effect during
//!   design.
//! - **Post-init Editing** (`.md` + `.json`): read-only tools pass;
//!   writes to the session `.md` pass (the design doc is editable);
//!   all other mutators go to `RequireApproval`.
//! - **Active**: the seed window (between Approve and first `start`)
//!   hard-denies mutators; otherwise the live design `.md` is frozen
//!   against write tools and everything else follows normal approval.
//! - **NoPlan**: no gate.
//!
//! Approval prompts fire only under the `/ask` policy (via
//! `requires_approval`) or user-initiated `/plan` Editing (via this
//! gate's `RequireApproval`). Under yolo (`auto_approve = true`),
//! `requires_approval` never prompts; this gate's `RequireApproval`
//! only fires during Editing, which is entered only when the user hits
//! `/plan` themselves (the interactive-prompt discriminator).
//!
//! Denials return deterministic, instructive strings so the model can
//! navigate to the allowed alternative instead of flailing.

use super::r#trait::ToolHints;
use crate::utils::plan_files::{self, PlanModeState};
use serde_json::Value;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Three-state decision from the plan-mode gate.
///
/// The gate is a filter that runs before the normal approval policy.
/// It can allow a call through, hard-deny it, or force an approval
/// prompt regardless of the session's auto-approve setting (used for
/// mutators during Editing, where the call is not outright forbidden
/// but must be explicitly confirmed).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GateDecision {
    /// Call proceeds under the normal approval policy.
    Allow,
    /// Call is hard-denied; the reason is returned to the model as the
    /// tool result.
    Deny(String),
    /// Call proceeds but REQUIRES user approval even under auto-approve
    /// (overrides yolo for the Editing window). The reason is shown in
    /// the approval prompt.
    RequireApproval(String),
}

#[allow(dead_code)]
impl GateDecision {
    pub(crate) fn is_allowed(&self) -> bool {
        matches!(self, GateDecision::Allow)
    }

    pub(crate) fn is_denied(&self) -> bool {
        matches!(self, GateDecision::Deny(_))
    }

    pub(crate) fn needs_approval(&self) -> bool {
        matches!(self, GateDecision::RequireApproval(_))
    }
}

/// Check a tool call against the session's plan-mode state.
///
/// Returns a [`GateDecision`]: `Allow` (proceed under normal approval
/// policy), `Deny(reason)` (hard block), or `RequireApproval(reason)`
/// (proceed but force an approval prompt even under auto-approve).
pub(crate) async fn check_plan_gate(
    session_id: Uuid,
    tool_name: &str,
    hints: &ToolHints,
    input: &Value,
) -> GateDecision {
    let state = plan_files::plan_mode_state(session_id).await;

    match state {
        PlanModeState::NoPlan => GateDecision::Allow,

        PlanModeState::Active => {
            // Seed window (locked): between user Approve and a successful
            // `start`, the design-track session may only read and call the
            // plan tool. Mutators stay blocked so a wandering seed turn
            // cannot start editing the project before the checklist exists.
            if crate::utils::plan_mode::in_seed_window(session_id).await {
                if hints.read_only {
                    return GateDecision::Allow;
                }
                return GateDecision::Deny(format!(
                    "Plan gate: '{tool_name}' is blocked until the approved plan's \
                     checklist is seeded. Call `plan` add_tasks with the steps from \
                     the session .md, then `plan` start; project work begins after \
                     start succeeds."
                ));
            }
            // Freeze the live design .md against generic write tools; the
            // checklist executes through the plan tool, not by rewriting
            // the approved design.
            if !hints.read_only && write_targets_session_md(session_id, input).await {
                GateDecision::Deny(
                    "Plan gate: the session plan .md is frozen while the checklist is \
                     Active. Execute tasks with the plan tool (start/complete); the \
                     design document is no longer editable."
                        .to_string(),
                )
            } else {
                GateDecision::Allow
            }
        }

        PlanModeState::PreInitEditing => {
            if hints.read_only {
                return GateDecision::Allow;
            }
            GateDecision::RequireApproval(format!(
                "Plan gate: '{tool_name}' has side effects. The session is in Plan \
                 mode (pre-init Editing). Approve to allow this action during design."
            ))
        }

        PlanModeState::PostInitEditing => {
            if hints.read_only {
                return GateDecision::Allow;
            }
            // The design doc itself stays editable during Editing.
            if write_targets_session_md(session_id, input).await {
                return GateDecision::Allow;
            }
            GateDecision::RequireApproval(format!(
                "Plan gate: '{tool_name}' has side effects and requires approval \
                 while the plan is being designed (Editing). Refine the session \
                 plan .md; project work begins after the user approves the plan."
            ))
        }
    }
}

/// Mechanical plan requirement.
///
/// Prose in the brain files asked for the plan tool and was routinely
/// skipped: research, audits and multi-file fixes ran as long ad-hoc tool
/// sequences with nothing to review, nothing to verify a completion
/// against, and no record of what was left. This makes the requirement
/// mechanical: once a turn has made more than `threshold` tool calls and
/// the session still has no plan, every further call is refused until the
/// agent calls `plan` init.
///
/// Deliberate escape hatches, in order:
/// - `threshold == 0` disables the gate (`[agent] plan_gate_after_tool_calls`).
/// - `exempt` is set for sub-agent sessions and cron runs, which execute a
///   task someone else already planned. A throwaway child plan nobody sees
///   would be worse than no plan.
/// - `plan` and `tool_search` always pass, so the agent can always reach the
///   tool that unblocks it. Without this the gate would deadlock itself.
/// - Any existing plan state allows the call: a live checklist, or a design
///   being drafted, is the requirement being met.
pub(crate) async fn check_plan_requirement(
    session_id: Uuid,
    tool_name: &str,
    turn_tool_calls: usize,
    threshold: usize,
    exempt: bool,
) -> GateDecision {
    if exempt || threshold == 0 || turn_tool_calls <= threshold {
        return GateDecision::Allow;
    }
    // The unblock path can never be gated, or the agent cannot comply.
    if matches!(tool_name, "plan" | "tool_search") {
        return GateDecision::Allow;
    }
    if plan_files::plan_mode_state(session_id).await != PlanModeState::NoPlan {
        return GateDecision::Allow;
    }
    GateDecision::Deny(format!(
        "Plan gate: {turn_tool_calls} tool calls in this turn and no plan. Work of \
         this size (research, audits, multi-file fixes) runs under the plan tool, \
         not ad hoc. Call `plan` init now with the steps you have so far and the \
         acceptance check for each, then continue. Reads and writes resume as soon \
         as the plan exists."
    ))
}

/// Whether a write-capable tool call targets the session plan `.md`.
///
/// The target is read from the conventional `path` / `file_path` input
/// keys. Absolute paths compare directly; relative paths are tried
/// against the OpenCrabs home (`write_opencrabs_file` semantics). A
/// write tool whose target cannot be determined does NOT match, so in
/// post-init Editing it is denied (safe default) and in Active it is
/// allowed (the freeze only guards the .md).
pub(crate) async fn write_targets_session_md(session_id: Uuid, input: &Value) -> bool {
    let Some(raw) = input
        .get("path")
        .or_else(|| input.get("file_path"))
        .and_then(|v| v.as_str())
    else {
        return false;
    };
    let md = plan_files::plan_md_path(session_id).await;
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        paths_match(&candidate, &md)
    } else {
        paths_match(&crate::config::opencrabs_home().join(&candidate), &md)
    }
}

/// Path equality that tolerates symlinked parents (e.g. /var on macOS):
/// canonicalize both sides when possible, falling back to lexical
/// comparison for not-yet-existing files.
fn paths_match(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// Strip every non-read-only tool from `registry`, leaving a read-only
/// surface (#649).
///
/// A subagent spawned while the parent session is in Plan-mode Editing must
/// not be a hole in the write-freeze: it runs under a fresh child session
/// that resolves to `NoPlan`, so [`check_plan_gate`] would not gate it. Rather
/// than thread the parent's Editing state through the child's every tool call,
/// we remove the mutating tools from the child registry outright. The child
/// can read, search, and analyze to inform or review the design, but cannot
/// write the project, run bash, send to channels, or spawn further agents
/// (which would let it escape the freeze transitively).
///
/// Driven by the MCP `read_only` hint, so the spawn-time filter and the
/// per-call gate can never drift.
pub(crate) fn restrict_registry_to_read_only(registry: &super::ToolRegistry) {
    for name in registry.list_tools() {
        let dominated = registry.get(&name).is_some_and(|t| !t.hints().read_only);
        if dominated {
            registry.unregister(&name);
        }
    }
}
