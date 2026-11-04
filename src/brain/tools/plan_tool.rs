//! Plan Management Tool
//!
//! Allows the LLM to create, update, and manage structured plans for complex tasks.

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use crate::brain::tools::subagent::agent_type::ALWAYS_EXCLUDED;
use crate::tui::plan::{
    ApprovalSource, PlanDocument, PlanStatus, PlanTask, SpecCheck, SpecCheckSource, TaskDep,
    TaskScope, TaskStatus, TaskType, extract_check_commands, is_acceptable_check_command,
};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Plan management tool
pub struct PlanTool;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum PlanOperation {
    /// Create a new plan (design or checklist track) OR import one from a
    /// JSON file. Allowed from NoPlan or pre-init Editing only; a live
    /// post-init or Active plan must be discarded first.
    Init {
        /// Plan title (create mode). One of `title` / `file_path` is required.
        #[serde(default)]
        title: Option<String>,
        /// Import mode: absolute path to a plan JSON file on disk. Takes
        /// precedence over `title` and `mode` when present.
        #[serde(default)]
        file_path: Option<String>,
        /// Track selector: "design" (session .md + user Approve) or
        /// "checklist" (inline tasks, Active immediately). When omitted:
        /// tasks present imply checklist, no tasks imply design.
        #[serde(default)]
        mode: Option<String>,
        /// Inline task definitions (checklist mode): plan + tasks in one call.
        #[serde(default)]
        tasks: Vec<InlineTask>,
        /// Optional absolute path to the repo the plan's work executes in
        /// (#1452). When set, receipt binding (#1011) verifies claimed
        /// commit shas against THIS repo instead of the session cwd, so a
        /// plan executed in a non-cwd repo can carry mechanical receipts.
        /// Validated at init: must be absolute and an existing directory.
        #[serde(default)]
        working_directory: Option<String>,
    },
    /// Append one or more tasks in a single call (primary append op).
    /// Active only: checklist operations are blocked while Editing.
    /// Can optionally specify `insert_before` or `insert_after` to insert at a specific position.
    AddTasks {
        tasks: Vec<InlineTask>,
        #[serde(default)]
        insert_before: Option<serde_json::Value>,
        #[serde(default)]
        insert_after: Option<serde_json::Value>,
    },
    /// Append a single task. Backward-compatible alias that behaves like
    /// `add_tasks` with one task.
    /// Can optionally specify `insert_before` or `insert_after` to insert at a specific position.
    AddTask {
        title: String,
        #[serde(default)]
        description: String,
        #[serde(default = "default_task_type")]
        task_type: String,
        #[serde(default)]
        dependencies: Vec<usize>, // Task order numbers
        #[serde(default = "default_complexity")]
        complexity: u8,
        #[serde(default)]
        acceptance_criteria: Vec<String>,
        #[serde(default)]
        insert_before: Option<serde_json::Value>,
        #[serde(default)]
        insert_after: Option<serde_json::Value>,
    },
    /// Find and start the next task, or a specific one via `task_order`.
    /// Active only. Returns full task details. Idempotent on an in-progress
    /// task (re-surfaces its details after a compaction); resets a failed
    /// task for retry.
    Start {
        #[serde(default)]
        task_order: Option<usize>,
        /// Confirm a MODEL-WRITTEN spec check so it may reject a completion
        /// (#2063). A criteria-derived check needs no confirmation; a
        /// generated one is advisory until this is passed. Ignored when
        /// there is nothing generated to confirm.
        #[serde(default)]
        confirm_spec_check: bool,
        /// Executor choice for this start (#908 option A). `Some(true)`
        /// spawns a dedicated subagent session that completes the task
        /// (overriding the InProgress-retry-inline rule, since `start`
        /// blocks and no live subagent can exist mid-call); `Some(false)`
        /// means no subagent - you do the work inline; `None` uses the
        /// config default (`agent.plan_isolated_execution`). Ralph loops
        /// pass their `fresh_context` value here.
        #[serde(default)]
        isolated: Option<bool>,
    },
    /// Finish a task (pure state transition). Auto-start of the next task is/// opt-in via `[agent] plan_auto_start` (#1195); default reports a hint only.
    Complete {
        task_order: usize,
        /// "success" (default), "fail", or "skip".
        #[serde(default = "default_action")]
        action: String,
        #[serde(default)]
        output: String,
    },
    /// Self-approve the plan (Editing -> Active) WITHOUT the user's Approve
    /// button / `/execute`. Allowed ONLY when the user has granted autonomy for
    /// this session (`grant_autonomy`); otherwise refused so the default stays
    /// user-gated (#581).
    Approve,
    /// Grant this session autonomy so the agent may `approve` its own plans.
    /// Call this ONLY when the user explicitly says to proceed without waiting
    /// for approval ("go for it", "no hand-holding", "don't wait for me").
    /// Durable until revoked (#581).
    GrantAutonomy,
    /// Revoke this session's self-approval autonomy — future plans require the
    /// user's Approve again. Call when the user asks to go back to approving
    /// plans themselves (#581).
    RevokeAutonomy,
    /// Discard the live plan (remove its files → NoPlan). This is a USER
    /// action: the agent's call is refused unless the session has granted
    /// plan autonomy, because a model that can shred its own plan can
    /// wiggle out of the review harness mid-gate — or be told to by a
    /// malicious message. The user discards via /discard or the plan
    /// card's Discard button; both bypass this tool.
    Discard,
    /// Return the current plan state (title, status, checklist progress) with no
    /// side effects. Use to answer "what's the plan / where are we"; unlike
    /// `start` it never mutates the plan (#585).
    /// Pass `full: true` to bypass windowing/accordion folding when the plan is long.
    ShowPlan {
        #[serde(default)]
        full: Option<bool>,
    },
}

/// Inline task definition accepted by `init` so a plan and its tasks can be
/// created in a single call.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct InlineTask {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_task_type")]
    pub task_type: String,
    #[serde(default)]
    pub dependencies: Vec<usize>,
    #[serde(default = "default_complexity")]
    pub complexity: u8,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

pub(crate) fn default_complexity() -> u8 {
    3
}

/// #20: whether plan approval REQUIRES an explicit human signal. Shared
/// with the resume demotion in `plan_files::load_plan_from_path` — the
/// single definition lives there so both the init arms and the loader
/// read the same default policy.
fn plan_require_approval_enabled() -> bool {
    crate::utils::plan_files::plan_require_approval_enabled()
}

fn default_task_type() -> String {
    "other".to_string()
}

fn default_action() -> String {
    "success".to_string()
}

/// Parse a task-type string into a `TaskType`, mapping anything unrecognized
/// to `Other` (lossless: the original string is preserved). The fallback is
/// logged so out-of-enum values stay observable.
fn parse_task_type(s: &str) -> TaskType {
    match s.to_lowercase().as_str() {
        "research" => TaskType::Research,
        "edit" => TaskType::Edit,
        "create" => TaskType::Create,
        "delete" => TaskType::Delete,
        "test" => TaskType::Test,
        "refactor" => TaskType::Refactor,
        "documentation" => TaskType::Documentation,
        "configuration" => TaskType::Configuration,
        "build" => TaskType::Build,
        "other" => TaskType::Other("other".to_string()),
        unknown => {
            tracing::debug!("Unknown task_type '{unknown}' mapped to the 'other' category");
            TaskType::Other(unknown.to_string())
        }
    }
}

/// Whether a task type requires checkable (runnable) acceptance criteria (#1133).
/// Returns `true` for types that can name a runnable command (test, build,
/// refactor, edit, delete, create, configuration); `false` for exempt types
/// (documentation, research, other) that cannot reasonably name `cargo test`.
fn requires_checkable_criteria(task_type: &TaskType) -> bool {
    matches!(
        task_type,
        TaskType::Test
            | TaskType::Build
            | TaskType::Refactor
            | TaskType::Edit
            | TaskType::Delete
            | TaskType::Create
            | TaskType::Configuration
    )
}

#[cfg(feature = "telegram")]
fn format_mermaid_plan_error(context: &str, errors: &[String]) -> String {
    let err = crate::channels::telegram::rich::mermaid::format_mermaid_error(context, errors);
    format!(
        "PLAN TASK REFUSED: {err}

Please fix the Mermaid diagram syntax in the task description and try again."
    )
}

/// Validate a task's acceptance criteria at creation time (#1133).
/// Returns `Ok(())` if the task may proceed, or an error message if refused.
/// Under `downgrade` policy, logs a belief and returns Ok. Under `off`, does nothing.
fn validate_task_criteria_at_creation(
    task_order: usize,
    task_title: &str,
    task_type: &TaskType,
    acceptance_criteria: &[String],
    policy: CriteriaPolicy,
) -> Result<()> {
    if policy == CriteriaPolicy::Off || !requires_checkable_criteria(task_type) {
        return Ok(());
    }

    // Check if criteria are empty
    if acceptance_criteria.is_empty() {
        if policy == CriteriaPolicy::Strict {
            return Err(ToolError::InvalidInput(format!(
                "Task #{task_order} '{task_title}' (type={task_type}) has no acceptance criteria. \
                 Under `criteria_policy = \"strict\"`, required task types must have checkable \
                 criteria naming a runnable command and expected outcome. \
                 Rewrite the criteria or change task_type to an exempt type (documentation, research, other)."
            )));
        }
        // downgrade: log and proceed
        tracing::info!(
            "Task #{task_order} '{task_title}' (type={task_type}) has no acceptance criteria — \
             logged as belief under downgrade policy"
        );
        return Ok(());
    }

    // Check if any criterion is verifiable (runnable)
    let has_verifiable = acceptance_criteria
        .iter()
        .any(|c| crate::tui::plan::is_verifiable(c));

    if !has_verifiable {
        if policy == CriteriaPolicy::Strict {
            let prose_examples: Vec<&str> = acceptance_criteria
                .iter()
                .take(2)
                .map(|s| s.as_str())
                .collect();
            return Err(ToolError::InvalidInput(format!(
                "Task #{task_order} '{task_title}' (type={task_type}) has prose-only criteria: \
                 {:?}. Under `criteria_policy = \"strict\"`, required task types must have at \
                 least one checkable criterion naming a runnable command (e.g., 'cargo test --lib \
                 <filter> reports 0 failed'). Rewrite the criteria or change task_type to an \
                 exempt type (documentation, research, other).",
                prose_examples
            )));
        }
        // downgrade: log and proceed
        tracing::info!(
            "Task #{task_order} '{task_title}' (type={task_type}) has prose-only criteria — \
             logged as belief under downgrade policy"
        );
    }

    Ok(())
}

/// Resolve insertion target index (0-based) in `plan.tasks` from `insert_before` or `insert_after`.
/// Rejects when both are specified, or when the insertion point would precede or displace completed tasks.
fn resolve_insertion_index(
    plan: &PlanDocument,
    insert_before: Option<&serde_json::Value>,
    insert_after: Option<&serde_json::Value>,
) -> std::result::Result<usize, String> {
    if insert_before.is_some() && insert_after.is_some() {
        return Err(
            "Cannot specify both 'insert_before' and 'insert_after'. Choose one.".to_string(),
        );
    }

    let total = plan.tasks.len();
    if total == 0 {
        return Ok(0);
    }

    // Find highest completed or skipped task order (1-based)
    let max_completed_order = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
        .map(|t| t.order)
        .max()
        .unwrap_or(0);

    let target_idx = if let Some(val) = insert_before {
        let order = if let Some(n) = val.as_u64() {
            n as usize
        } else if let Some(s) = val.as_str() {
            match s.to_lowercase().trim() {
                "final" | "last" => total,
                other => {
                    return Err(format!(
                        "Invalid 'insert_before' token '{other}'. Use a 1-based task number, 'final', or 'last'."
                    ));
                }
            }
        } else {
            return Err(
                "'insert_before' must be an integer (1-based task number) or string ('final' / 'last').".to_string(),
            );
        };

        if order == 0 {
            return Err("Task numbers start at 1, not 0.".to_string());
        }

        if order > total {
            total
        } else {
            if order <= max_completed_order {
                return Err(format!(
                    "Cannot insert before task #{order}: task #{order} is already completed/skipped. Earliest valid insertion slot is after task #{max_completed_order}."
                ));
            }
            order - 1
        }
    } else if let Some(val) = insert_after {
        let order = if let Some(n) = val.as_u64() {
            n as usize
        } else if let Some(s) = val.as_str() {
            match s.to_lowercase().trim() {
                "current" => {
                    // Find InProgress task, or first Pending task
                    plan.tasks
                        .iter()
                        .find(|t| matches!(t.status, TaskStatus::InProgress))
                        .map(|t| t.order)
                        .or_else(|| {
                            plan.tasks
                                .iter()
                                .find(|t| matches!(t.status, TaskStatus::Pending))
                                .map(|t| t.order)
                        })
                        .unwrap_or(max_completed_order)
                }
                other => {
                    return Err(format!(
                        "Invalid 'insert_after' token '{other}'. Use a 1-based task number or 'current'."
                    ));
                }
            }
        } else {
            return Err(
                "'insert_after' must be an integer (1-based task number) or string ('current')."
                    .to_string(),
            );
        };

        if order == 0 {
            return Err("Task numbers start at 1, not 0.".to_string());
        }

        if order < max_completed_order {
            return Err(format!(
                "Cannot insert after task #{order}: tasks through #{max_completed_order} are already completed/skipped. Earliest valid insertion slot is after task #{max_completed_order}."
            ));
        }

        order.min(total)
    } else {
        total
    };

    Ok(target_idx)
}

/// Insert a task into `plan` at 0-based vector `target_idx`, resolving dependencies and
/// renumbering downstream tasks. Returns the newly inserted task's order (1-based).
#[allow(clippy::too_many_arguments)]
fn insert_task_to_plan(
    plan: &mut PlanDocument,
    target_idx: usize,
    title: String,
    description: String,
    task_type: &str,
    dependencies: &[usize],
    complexity: u8,
    acceptance_criteria: Vec<String>,
) -> Result<usize> {
    validate_string(&title, MAX_TITLE_LENGTH, "Task title")?;
    validate_string(&description, MAX_DESCRIPTION_LENGTH, "Task description")?;

    let insert_idx = target_idx.min(plan.tasks.len());
    let order = insert_idx + 1;
    let mut task = PlanTask::new(order, title, description, parse_task_type(task_type));
    task.complexity = complexity.clamp(1, 5);
    task.acceptance_criteria = acceptance_criteria;

    for dep_order in dependencies {
        if *dep_order == 0 {
            return Err(ToolError::InvalidInput(
                "Task numbers start at 1, not 0".to_string(),
            ));
        }
        let dep_task = plan.tasks.get(*dep_order - 1).ok_or_else(|| {
            ToolError::InvalidInput(format!(
                "Invalid dependency: task {dep_order} does not exist"
            ))
        })?;
        task.dependencies.push(TaskDep::Id(dep_task.id));
    }

    plan.insert_task(insert_idx, task);
    Ok(order)
}

/// Append a task to `plan`, resolving 1-based dependency order numbers to the
/// referenced tasks' UUIDs. Returns the new task's order. Shared by `add_task`
/// and `init`'s inline tasks.
fn add_task_to_plan(
    plan: &mut PlanDocument,
    title: String,
    description: String,
    task_type: &str,
    dependencies: &[usize],
    complexity: u8,
    acceptance_criteria: Vec<String>,
) -> Result<usize> {
    insert_task_to_plan(
        plan,
        plan.tasks.len(),
        title,
        description,
        task_type,
        dependencies,
        complexity,
        acceptance_criteria,
    )
}

/// Deterministic refusal for checklist operations (`add_tasks`, `add_task`,
/// `start`, `complete`) attempted while the plan is not Active. `None`
/// means the operation may proceed (NoPlan falls through to the usual
/// "No active plan" error).
fn checklist_blocked_reason(state: crate::utils::plan_files::PlanModeState) -> Option<String> {
    use crate::utils::plan_files::PlanModeState;
    match state {
        PlanModeState::NoPlan | PlanModeState::Active => None,
        PlanModeState::PreInitEditing => Some(
            "No plan yet: the session is in Plan mode (pre-init Editing). Call 'init' \
             first: mode=\"checklist\" with inline tasks to execute now, or \
             mode=\"design\" to draft the plan for user Approve."
                .to_string(),
        ),
        PlanModeState::PostInitEditing => Some(
            "Checklist operations are blocked while the plan is being designed \
             (Editing). Refine the session plan .md and wait for the user to approve \
             the plan; the checklist goes live on Approve."
                .to_string(),
        ),
    }
}

/// Map a plan task outcome verb to an epistemic confidence level.
/// Pure — unit-testable without store access.
pub(crate) fn task_outcome_confidence(verb: &str) -> super::epistemic::Confidence {
    match verb {
        "completed" => super::epistemic::Confidence::Verified,
        "failed" => super::epistemic::Confidence::Contradicted,
        _ => super::epistemic::Confidence::Uncertain, // skipped
    }
}

/// Belief key prefix owning every task outcome of ONE plan (#1083).
///
/// The plan id is part of the key because the belief store is profile-global
/// and long-lived: without it, task #1 of every plan ever run shares one key,
/// so an unrelated plan's failure surfaces in a fresh plan's Orient gate and
/// two same-titled tasks in different plans mark each other Contradicted.
/// `PlanDocument.id` (not the session id) is the scope: two plans created and
/// completed sequentially in ONE session have distinct ids, while a retry in a
/// later session reloads the same plan file and keeps the same id — which is
/// exactly the cross-session warning #886 was built for.
pub(crate) fn plan_belief_prefix(plan_id: uuid::Uuid) -> String {
    format!("plan:{plan_id}:task:")
}

/// Stable belief key for a plan task (plan id + order + title hash) so a retry
/// that later succeeds supersedes the earlier failure on the same key rather
/// than duplicating it. Pure — unit-testable without store access.
pub(crate) fn task_outcome_key(plan_id: uuid::Uuid, task_order: usize, title: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    title.hash(&mut hasher);
    format!(
        "{}{}:{:016x}",
        plan_belief_prefix(plan_id),
        task_order,
        hasher.finish()
    )
}

/// Epistemic engine integration (#862): log a plan task's outcome as a
/// belief, letting the engine record fail→success transitions across retries.
fn log_task_outcome_belief(
    plan_id: uuid::Uuid,
    task_order: usize,
    title: &str,
    verb: &str,
    output: &Option<String>,
    confidence_override: Option<super::epistemic::Confidence>,
) {
    let key = task_outcome_key(plan_id, task_order, title);
    // A criteria-aware downgrade (#870) logs a success as Uncertain instead of
    // Verified when nothing mechanically checked the declared criteria.
    let confidence = confidence_override.unwrap_or_else(|| task_outcome_confidence(verb));

    let mut value = verb.to_string();
    if let Some(o) = output {
        let snippet: String = o.trim().chars().take(120).collect();
        if !snippet.is_empty() {
            value.push_str(&format!(" — {snippet}"));
        }
    }

    let result = super::epistemic::add_belief(&key, &value, confidence, "plan_tool:complete");
    if let super::epistemic::ContradictionResult::Contradicted {
        old_value,
        new_value,
    } = result
    {
        tracing::info!(
            "plan_tool: task #{task_order} outcome superseded — was '{old_value}', now '{new_value}'"
        );
    }
}

/// Clear the session goal a plan task set (on complete or skip).
async fn clear_task_goal(context: &ToolExecutionContext, session_id: uuid::Uuid) {
    let Some(svc) = context.service_context.as_ref() else {
        return;
    };
    if let Err(e) = crate::brain::goal::GoalManager::new(svc.clone())
        .clear_goal(session_id)
        .await
    {
        tracing::warn!("Failed to clear plan-task goal: {e}");
    }
}

/// One-line-per-task list (order, title, type) for the `init` confirmation.
fn render_task_list(plan: &PlanDocument) -> String {
    plan.tasks
        .iter()
        .map(|t| {
            let verification_badge = t
                .verification
                .map(|v| format!(" {}", v.badge()))
                .unwrap_or_default();
            format!(
                "  {}. {} [{}]{}{}",
                t.order,
                t.title,
                t.task_type,
                crate::tui::plan::quality_glyph_suffix(t),
                verification_badge
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Full task details (type, complexity, description, acceptance criteria,
/// dependency state, status) for `start` and `complete`'s next-task preview.
/// This is the recovery payload that survives a context compaction.
fn render_task_details(plan: &PlanDocument, task: &PlanTask) -> String {
    let criteria = if task.acceptance_criteria.is_empty() {
        String::new()
    } else {
        let lines = task
            .acceptance_criteria
            .iter()
            .map(|c| format!("  • {c}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\nAcceptance Criteria:\n{lines}")
    };
    let check = match &task.spec_check {
        Some(check) if !check.commands.is_empty() => format!(
            "\nLocked spec check (runs at 'complete'): {}",
            describe_commands(&check.commands)
        ),
        _ => String::new(),
    };
    let deps = if task.dependencies.is_empty() {
        String::new()
    } else {
        let parts = task
            .dependencies
            .iter()
            .filter_map(|d| d.as_uuid())
            .filter_map(|id| plan.get_task(&id))
            .map(|t| {
                let mark = if matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped) {
                    "✓"
                } else {
                    "✗"
                };
                format!("Task {} {}", t.order, mark)
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("\nDependencies: {parts}")
    };
    format!(
        "Type: {} | Complexity: {}\nDescription: {}{}{}{}\nStatus: {:?}",
        task.task_type,
        task.complexity_stars(),
        task.description,
        criteria,
        check,
        deps,
        task.status
    )
}

/// Validate plan file path for security
/// Prevents symlink attacks and path traversal
/// Validate the optional `working_directory` init param (#1452): the repo a
/// plan's work executes in, for receipt binding. Must be absolute and an
/// existing directory; a bad binding must fail init loudly, never silently
/// weaken the receipt gate (a nonexistent dir would make the `git`
/// probe fail and skip verification entirely).
pub(crate) fn validate_plan_working_directory(dir: &str) -> Result<()> {
    let path = std::path::Path::new(dir);
    if !path.is_absolute() {
        return Err(ToolError::InvalidInput(format!(
            "working_directory must be an absolute path (got '{dir}')"
        )));
    }
    if !path.is_dir() {
        return Err(ToolError::InvalidInput(format!(
            "working_directory does not exist or is not a directory: {dir}"
        )));
    }
    Ok(())
}

/// Resolve the repo receipt binding (#1011) verifies claimed shas against:
/// the plan's bound `working_directory` when set (#1452), else the session
/// working directory (previous behavior, unchanged for plans without a
/// binding). Pure so the resolution itself is unit-testable.
pub(crate) fn receipt_binding_dir(
    plan_working_directory: Option<&str>,
    session_working_dir: &Path,
) -> PathBuf {
    plan_working_directory
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| session_working_dir.to_path_buf())
}

pub(crate) fn validate_plan_file_path(path: &Path, base_dir: &Path) -> Result<()> {
    // Check if path is absolute and within the base directory
    if !path.starts_with(base_dir) {
        return Err(ToolError::InvalidInput(
            "Plan file must be within the session directory".to_string(),
        ));
    }

    // Check for symlinks (security risk)
    if path.exists() {
        let metadata = std::fs::symlink_metadata(path).map_err(ToolError::Io)?;
        if metadata.is_symlink() {
            return Err(ToolError::InvalidInput(
                "Plan file cannot be a symlink (security restriction)".to_string(),
            ));
        }
    }

    // Verify filename matches pattern .opencrabs_plan_{uuid}.json (no traversal)
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| ToolError::InvalidInput("Invalid plan filename".to_string()))?;

    if !file_name.starts_with(".opencrabs_plan_") || !file_name.ends_with(".json") {
        return Err(ToolError::InvalidInput(
            "Plan filename must match pattern .opencrabs_plan_{session_id}.json".to_string(),
        ));
    }

    // Extract and validate UUID portion
    let uuid_part = &file_name[16..file_name.len() - 5]; // Remove ".opencrabs_plan_" (16 chars) and ".json" (5 chars)
    uuid::Uuid::parse_str(uuid_part).map_err(|_| {
        ToolError::InvalidInput("Plan filename must contain a valid UUID".to_string())
    })?;

    Ok(())
}

/// Maximum plan file size (10MB)
pub(crate) const MAX_PLAN_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Input validation limits
pub(crate) const MAX_TITLE_LENGTH: usize = 200;
pub(crate) const MAX_DESCRIPTION_LENGTH: usize = 5000;

// ── Ralph Loop: Mechanical verification gate ────────────────────────
// Config resolved per-project (#947): a `ralph_loop.toml` at the
// session working dir wins, `~/.opencrabs/safety/ralph_loop.toml` is
// the fallback. The TOML defines verification commands per task type.
// Before accepting "success" on a task, the gate runs those commands
// and rejects the completion if any exit non-zero. This prevents the
// model from hallucinating "clippy passed" when it didn't.

use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
pub(crate) struct RalphLoopConfig {
    #[serde(default)]
    pub(crate) forward: RalphForward,
    /// Verification gate config. `pub(crate)` so the per-project resolution
    /// tests can pin the keys this section accepts, the same way they read
    /// `forward`. A key that parses but is never asserted on is how the
    /// `[epistemic]` section silently dropped its fields (#1640).
    #[serde(default)]
    pub(crate) verification: RalphVerification,
    /// Epistemic layer config (#1640). The `[epistemic]` section in
    /// ralph_loop.toml was silently dropped before this field existed —
    /// users edited `decay_enabled` or `decay_interval_hours` and got
    /// no error and no effect.
    #[serde(default)]
    pub(crate) epistemic: EpistemicConfig,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RalphForward {
    #[serde(default = "default_max_iterations")]
    pub(crate) max_iterations: u32,
    /// Run each plan task in a freshly spawned worker session (#908
    /// option A). When true (default), Ralph's fresh-by-construction
    /// policy applies and started tasks may run isolated; when false,
    /// Ralph asks for continuity and tasks run inline. Only bites when
    /// `agent.plan_isolated_execution` is on — the config flag is the
    /// master switch, this key is Ralph's policy within it. An explicit
    /// `isolated` on plan start overrides both.
    #[serde(default = "default_true")]
    fresh_context: bool,
    /// Plan state lives on disk and is threaded to workers (#908). When
    /// true (default), isolation is mechanically possible: the child
    /// gets the parent's plan file via `plan_session_override`. When
    /// false, a worker would run blind, so isolation is refused with an
    /// honest note and tasks run inline.
    #[serde(default = "default_true")]
    state_on_disk: bool,
}

impl Default for RalphForward {
    fn default() -> Self {
        Self {
            max_iterations: default_max_iterations(),
            fresh_context: true,
            state_on_disk: true,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct RalphVerification {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) require_all_pass: bool,
    #[serde(default)]
    pub(crate) task_type_commands: Vec<TaskTypeCommands>,
    /// What to do when a task declares acceptance criteria but its type has no
    /// verification commands (#870). `downgrade` (default) logs the completion
    /// as Uncertain instead of Verified; `strict` rejects it outright; `off`
    /// keeps the pre-#870 behaviour.
    #[serde(default)]
    pub(crate) criteria_policy: CriteriaPolicy,
    /// What to do with the per-task spec check a task locks when it starts
    /// (#1904). `advisory` (default) records a failure as Uncertain and still
    /// lets the completion through; `block` rejects the completion; `off` runs
    /// nothing, which is the pre-#1904 behaviour.
    ///
    /// Advisory is the default on purpose. The check is extracted from the
    /// task's own criteria by a conservative reader that refuses anything it
    /// cannot take as one simple command, so a failure means "look here", not
    /// "the human is wrong". `block` is opt-in for plans which are known to
    /// carry runnable criteria.
    #[serde(default)]
    pub(crate) spec_checks: SpecCheckPolicy,
    /// Whether a task whose criteria name no runnable command may have one
    /// written by a model at `start` (#2063). On by default.
    ///
    /// Costs at most one small model call per task, and only on a miss: a
    /// task whose criteria already yield a command never calls the model.
    /// The generated command is locked like any other but stays advisory
    /// until a human confirms it, because a model asked for a command will
    /// invent one for "update the docs".
    #[serde(default = "default_true")]
    pub(crate) spec_check_generation: bool,
}

/// Policy for completion claims made against unverified acceptance criteria (#870).
#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CriteriaPolicy {
    /// Accept the completion but log the belief as Uncertain, not Verified.
    #[default]
    Downgrade,
    /// Reject the completion: criteria were declared but nothing verifies them.
    Strict,
    /// Pre-#870 behaviour: criteria stay advisory, no enforcement.
    Off,
}

impl std::fmt::Display for CriteriaPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CriteriaPolicy::Downgrade => write!(f, "downgrade"),
            CriteriaPolicy::Strict => write!(f, "strict"),
            CriteriaPolicy::Off => write!(f, "off"),
        }
    }
}

/// Policy for the per-task spec check a task locks at `start` (#1904).
///
/// A spec check is one command read out of the task's own acceptance criteria
/// and hashed at `start`, then run at `complete`. The hash is what makes it
/// worth anything: the check is frozen at the moment the task begins, so a task
/// cannot complete against a criterion that was edited after the fact to match
/// whatever the code ended up doing.
#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SpecCheckPolicy {
    /// Run the locked check; a failure logs the completion as Uncertain.
    #[default]
    Advisory,
    /// Reject the completion when the locked check fails.
    Block,
    /// Pre-#1904 behaviour: no per-task checks are run.
    Off,
}

impl std::fmt::Display for SpecCheckPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpecCheckPolicy::Advisory => write!(f, "advisory"),
            SpecCheckPolicy::Block => write!(f, "block"),
            SpecCheckPolicy::Off => write!(f, "off"),
        }
    }
}

/// Track the last observed criteria_policy per working directory for audit trail (#1135).
///
/// Static storage so policy flips are detected across multiple `complete` calls
/// in the same session. Keyed by working directory path since different projects
/// can have different policies.
static LAST_OBSERVED_POLICY: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, CriteriaPolicy>>,
> = std::sync::OnceLock::new();

/// Log a criteria_policy flip as a belief (#1135).
///
/// Compares the current policy to the last observed value for this working
/// directory. If different, logs a belief recording the transition (old → new)
/// with timestamp. Updates the last observed value regardless.
///
/// Returns the current policy for convenience.
pub(crate) fn audit_criteria_policy_flip(
    working_dir: &std::path::Path,
    current: CriteriaPolicy,
) -> CriteriaPolicy {
    let lock = LAST_OBSERVED_POLICY
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let Ok(mut map) = lock.lock() else {
        tracing::warn!("Failed to acquire criteriaPolicy audit lock");
        return current;
    };

    let last = map.get(working_dir).copied();
    map.insert(working_dir.to_path_buf(), current);

    if let Some(prev) = last
        && prev != current
    {
        let timestamp = chrono::Utc::now().to_rfc3339();
        super::epistemic::add_belief(
            &format!("ralph_loop:criteria_policy:{}", working_dir.display()),
            &format!("{prev} → {current} (flipped at {timestamp})"),
            super::epistemic::Confidence::Verified,
            "plan_tool:criteria_policy_audit",
        );
        tracing::info!(
            "criteria_policy flipped: {prev} → {current} at {timestamp} (working_dir: {})",
            working_dir.display()
        );
    }

    current
}

#[derive(Debug, Deserialize)]
pub(crate) struct TaskTypeCommands {
    pub(crate) task_type: String,
    pub(crate) commands: Vec<String>,
}

fn default_max_iterations() -> u32 {
    20
}

fn default_true() -> bool {
    true
}

/// Epistemic layer configuration (#1640).
///
/// Maps the `[epistemic]` section of `ralph_loop.toml`. Before this struct
/// existed, serde silently dropped every key in that section, so users could
/// edit `decay_enabled` or `decay_interval_hours` and get no error and no
/// effect.
// Allow dead_code: fields are deserialized here but consumed by the epistemic
// engine wired in #1641. Removing the lint now would require deleting the
// struct, which defeats the purpose of fixing the silent config drop.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct EpistemicConfig {
    /// Master switch for the epistemic layer.
    #[serde(default = "default_true")]
    pub(crate) enabled: bool,
    /// Human-readable confidence level names (documentation, not consumed at runtime).
    #[serde(default)]
    pub(crate) confidence_levels: Vec<String>,
    /// Whether unverified beliefs decay over time.
    #[serde(default = "default_true")]
    pub(crate) decay_enabled: bool,
    /// Hours before an unverified belief decays one confidence level.
    #[serde(default = "default_decay_interval_hours")]
    pub(crate) decay_interval_hours: u64,
    /// Whether to detect contradictions when a new belief conflicts an existing one.
    #[serde(default = "default_true")]
    pub(crate) contradiction_detection: bool,
    /// Whether all beliefs require source attribution.
    #[serde(default = "default_true")]
    pub(crate) source_required: bool,
}

impl Default for EpistemicConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            confidence_levels: vec![
                "verified".into(),
                "inferred".into(),
                "uncertain".into(),
                "contradicted".into(),
            ],
            decay_enabled: true,
            decay_interval_hours: default_decay_interval_hours(),
            contradiction_detection: true,
            source_required: true,
        }
    }
}

fn default_decay_interval_hours() -> u64 {
    720 // 30 days
}

/// Which `ralph_loop.toml` governs a session (#947): the project's own file
/// at the session working dir wins outright when present; the machine-wide
/// safety copy is the fallback.
///
/// Pure so tests can exercise it with tempdirs.
pub(crate) fn ralph_config_path(working_dir: &Path) -> Option<PathBuf> {
    let project = working_dir.join("ralph_loop.toml");
    if project.is_file() {
        return Some(project);
    }
    super::toml_hot_reload::safety_path("ralph_loop.toml")
}

/// The Ralph loop config governing a session's project, reloaded when the
/// file changes on disk.
///
/// Per-project since #947: `<working_dir>/ralph_loop.toml` is authoritative
/// when present — including when it fails to parse, which yields `None`
/// rather than silently falling back to the machine-wide file (a silent
/// fallback is exactly how cargo verification commands leaked into non-Rust
/// projects). Otherwise `~/.opencrabs/safety/ralph_loop.toml`. One
/// mtime-keyed HotToml per resolved path, so hot reload (#852) works per
/// project independently.
pub(crate) fn ralph_loop_config(working_dir: &Path) -> Option<std::sync::Arc<RalphLoopConfig>> {
    static CONFIG: OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<PathBuf, super::toml_hot_reload::HotToml<RalphLoopConfig>>,
        >,
    > = OnceLock::new();
    let path = ralph_config_path(working_dir)?;
    let cache = CONFIG.get_or_init(Default::default);
    let Ok(mut guard) = cache.lock() else {
        return None;
    };
    guard
        .entry(path.clone())
        .or_insert_with(|| super::toml_hot_reload::HotToml::new(path, "Ralph loop config"))
        .get()
}

/// Run a shell command and return (exit_code, stdout+stderr).
pub(crate) fn run_verification_command(cmd: &str, working_dir: &std::path::Path) -> (i32, String) {
    // Without an explicit directory the child inherits the OpenCrabs process
    // cwd, i.e. wherever the binary was launched, so a plan in one repo was
    // gated on build results from another (#921). Every other tool resolves
    // this per session; this path shelled out around ToolExecutionContext.
    let output = {
        use crate::utils::shell::PushShellCommand;
        let (shell, shell_arg) = crate::utils::shell::shell_pair();
        std::process::Command::new(shell)
            .push_shell_command(shell_arg, cmd)
            .current_dir(working_dir)
            .output()
    };

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = format!("{stdout}{stderr}");
            (out.status.code().unwrap_or(-1), combined)
        }
        Err(e) => (-1, format!("Failed to execute verification command: {e}")),
    }
}

/// Truncate on a character boundary.
///
/// Slicing `&output[..500]` panics when byte 500 lands inside a multi-byte
/// character, which compiler output makes likely: rustc emits `^`, `─` and
/// smart quotes freely. A verification gate that panics on the failure it was
/// meant to report is worse than no gate.
pub(crate) fn truncate_output(output: &str, max_bytes: usize) -> String {
    if output.len() <= max_bytes {
        return output.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated]", &output[..end])
}

/// Parse the number of tests that actually ran, from any runner we recognise.
///
/// The guard this feeds catches two failure modes that both exit 0:
/// 1. A filter typo that matches 0 tests, so nothing runs
/// 2. A disabled suite that builds but skips everything
///
/// This used to read cargo's format alone, which made the guard silently
/// inert everywhere else: a pytest, go, vitest or flutter run that matched
/// nothing exited 0, parsed as `None`, and was stamped verified. In a
/// repository that is not Rust, the protection simply did not exist.
///
/// `None` still means "no test summary recognised", which is not the same as
/// zero and must never be treated as a failure: a lint, a build or a grep is a
/// legitimate verification command with no test count to report.
///
/// Pure, so tests exercise it without invoking a real runner.
pub(crate) fn parse_test_pass_count(output: &str) -> Option<usize> {
    // Ordered by specificity: a runner that names its own total is preferred
    // over a generic "N passed" that another tool might coincidentally print.
    parse_cargo_test_pass_count(output)
        .or_else(|| parse_jest_style_pass_count(output))
        .or_else(|| parse_pytest_pass_count(output))
        .or_else(|| parse_dart_pass_count(output))
        .or_else(|| parse_go_pass_count(output))
}

/// jest / vitest: "Tests:       5 passed, 5 total" or "Tests  5 passed (5)".
/// Also catches the zero case, which both print as "0 passed".
fn parse_jest_style_pass_count(output: &str) -> Option<usize> {
    let lower = output.to_lowercase();
    let line = lower
        .lines()
        .find(|l| l.trim_start().starts_with("tests:") || l.trim_start().starts_with("tests "))?;
    let idx = line.find("passed")?;
    digits_before(&line[..idx])
}

/// pytest: "===== 5 passed, 2 warnings in 0.31s =====", and the zero case it
/// spells differently: "no tests ran in 0.01s".
fn parse_pytest_pass_count(output: &str) -> Option<usize> {
    let lower = output.to_lowercase();
    if lower.contains("no tests ran") {
        return Some(0);
    }
    let line = lower
        .lines()
        .find(|l| l.contains(" passed") && (l.contains("=====") || l.contains(" in ")))?;
    let idx = line.find(" passed")?;
    digits_before(&line[..idx])
}

/// dart / flutter: "+5: All tests passed!", and "+0" when a filter matched
/// nothing. The counter is the number that actually ran.
fn parse_dart_pass_count(output: &str) -> Option<usize> {
    let lower = output.to_lowercase();
    if !lower.contains("all tests passed") {
        return None;
    }
    let line = lower.lines().find(|l| l.contains("all tests passed"))?;
    let plus = line.rfind('+')?;
    let after: String = line[plus + 1..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    after.parse().ok()
}

/// go: "ok  example/pkg  0.02s" counts as a run; "no test files" is the
/// zero case, which exits 0 and reports nothing else.
fn parse_go_pass_count(output: &str) -> Option<usize> {
    let lower = output.to_lowercase();
    if lower.contains("[no test files]") || lower.contains("no test files") {
        return Some(0);
    }
    // `go test` prints no count on success, so a passing run reports 1 to mean
    // "something ran". The guard only distinguishes zero from non-zero.
    lower.lines().any(|l| l.starts_with("ok  ")).then_some(1)
}

/// Read the trailing run of digits immediately before `haystack` ends,
/// skipping whitespace. Shared by the runners whose count precedes a word.
fn digits_before(haystack: &str) -> Option<usize> {
    let num: String = haystack
        .chars()
        .rev()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    num.parse().ok()
}

/// Parse "N passed" from cargo test output.
///
/// Cargo test emits "test result: ok. N passed; M failed; ..." on success.
/// Returns `Some(N)` if found, `None` if the pattern doesn't match.
pub(crate) fn parse_cargo_test_pass_count(output: &str) -> Option<usize> {
    // Cargo test output: "test result: ok. 42 passed; 0 failed; 3 ignored; ..."
    // The pattern is stable across cargo versions.
    let lower = output.to_lowercase();
    let marker = "test result:";
    let pos = lower.find(marker)?;
    let after = &lower[pos + marker.len()..];
    // Find "N passed" after "test result:"
    let passed_pos = after.find("passed")?;
    let before_passed = &after[..passed_pos];
    // Extract the number: scan backwards from "passed" for digits only
    let num_str: String = before_passed
        .chars()
        .rev()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    // Handle "42 passed" or "1,234 passed" (though cargo doesn't use commas)
    num_str.parse::<usize>().ok()
}

/// Which commands verify a task of this type, if verification is on.
///
/// Pure over the config section so the mapping can be tested without a file on
/// disk. A type with no entry, or an entry with no commands, verifies nothing.
pub(crate) fn commands_for_type(
    verification: &RalphVerification,
    task_type: &str,
) -> Option<Vec<String>> {
    if !verification.enabled {
        return None;
    }
    let type_lower = task_type.to_lowercase();
    verification
        .task_type_commands
        .iter()
        .find(|tc| tc.task_type.to_lowercase() == type_lower)
        .filter(|tc| !tc.commands.is_empty())
        .map(|tc| tc.commands.clone())
}

/// The gate itself, over an injected runner.
///
/// `run` is a seam: production passes `run_verification_command`, tests pass a
/// fake so the verdict logic can be exercised without shelling out. Without it
/// nothing here was testable, because every path ran `sh -c`.
///
/// With `require_all` false the first failure returns immediately, so the
/// runner is NOT called for the remaining commands. That short-circuit is part
/// of the contract, not an optimisation: a cheap check placed first is meant to
/// spare an expensive one after it.
pub(crate) fn verify_with(
    task_type: &str,
    task_order: usize,
    commands: &[String],
    require_all: bool,
    run: &mut dyn FnMut(&str) -> (i32, String),
) -> std::result::Result<(), String> {
    let mut failures: Vec<String> = Vec::new();

    for cmd in commands {
        let (exit_code, output) = run(cmd);
        if exit_code == 0 {
            // Vacuous-pass guard: a test run that exits 0 having run nothing
            // is not a pass. A filter typo or disabled suite exits 0, so the
            // gate would accept it as verified without this check. Every
            // runner we recognise is consulted, not just cargo: the guard was
            // inert in any repository that is not Rust.
            if let Some(passed) = parse_test_pass_count(&output)
                && passed == 0
            {
                let truncated = truncate_output(&output, 500);
                failures.push(format!(
                    "`{cmd}` exited 0 but ran 0 tests (vacuous pass)\n{truncated}"
                ));
                tracing::warn!(
                    "Verification vacuous for task #{task_order}: {cmd} exited 0 with 0 tests passed"
                );
                if !require_all {
                    return Err(format!(
                        "Verification gate REJECTED for task #{task_order} (type={task_type}):\n\
                         The command `{cmd}` exited with code 0 but ran 0 tests. A filter \
                         that matches nothing, or a disabled test suite, is not a verified \
                         completion. Fix the filter or re-enable the tests and try again.\n\n\
                         {truncated}"
                    ));
                }
            }
            continue;
        }
        let truncated = truncate_output(&output, 500);
        failures.push(format!("`{cmd}` exited {exit_code}\n{truncated}"));
        tracing::warn!("Verification failed for task #{task_order}: {cmd} exited {exit_code}");
        if !require_all {
            return Err(format!(
                "Verification gate REJECTED for task #{task_order} (type={task_type}):\n{truncated}\n\n\
                 The command `{cmd}` exited with code {exit_code}. Fix the issue and try \
                 completing again. The Ralph loop does not accept self-reported success \
                 when verification commands fail."
            ));
        }
    }

    if failures.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Verification gate REJECTED for task #{task_order} (type={task_type}):\n{}\n\n\
         Fix the issues and try completing again. The Ralph loop does not accept \
         self-reported success when verification commands fail.",
        failures.join("\n\n")
    ))
}

/// What the verification gate decided about a completion claim (#870).
///
/// `verify_task_completion` used to return `Ok(())` for BOTH "commands ran and
/// passed" and "nothing was configured," so the caller couldn't tell a proven
/// completion from an unchecked one. The criteria policy needs that
/// distinction, so the gate now reports which happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VerificationOutcome {
    /// Verification commands ran and all passed — the claim is proven.
    Verified,
    /// No commands configured for this task type — passed through unchecked.
    NotConfigured,
    /// Verification globally disabled (no config / `enabled = false`).
    Disabled,
}

/// Mechanical verification gate: run the verification commands for the task
/// type. `Ok(outcome)` reports how the claim was settled, `Err(message)` when a
/// configured command fails. This is the anti-hallucination gate — the model
/// cannot claim success when the shell says otherwise.
fn verify_task_completion(
    task_type: &str,
    task_order: usize,
    working_dir: &std::path::Path,
) -> std::result::Result<VerificationOutcome, String> {
    let Some(config) = ralph_loop_config(working_dir) else {
        return Ok(VerificationOutcome::Disabled);
    };
    if !config.verification.enabled {
        return Ok(VerificationOutcome::Disabled);
    }
    // A type the config never named used to verify nothing and the task was
    // skipped. That is how a multi-language box lost the gate everywhere but
    // Rust: the machine-wide file names cargo, so every other project either
    // matched no entry at all or was handed a toolchain it does not have.
    // Detect the project from its manifest and verify with the runner it
    // actually uses. An explicit entry still wins, because a project that has
    // said how it wants to be verified has said something no marker file can
    // contradict.
    let commands = match commands_for_type(&config.verification, task_type) {
        Some(cmds) => cmds,
        None => match super::project_runner::fallback_commands(working_dir, task_type) {
            Some(cmds) => {
                tracing::info!(
                    "Ralph loop: no configured commands for type={task_type}; verifying with \
                     the project's own runner instead of skipping: {cmds:?}"
                );
                cmds
            }
            None => return Ok(VerificationOutcome::NotConfigured),
        },
    };

    tracing::info!(
        "Ralph loop verification for task #{task_order} (type={task_type}) in {}: {} commands",
        working_dir.display(),
        commands.len()
    );

    verify_with(
        task_type,
        task_order,
        &commands,
        config.verification.require_all_pass,
        &mut |cmd| run_verification_command(cmd, working_dir),
    )
    .map(|()| VerificationOutcome::Verified)
}

/// What running a task's locked spec check did to a completion claim (#1904).
#[derive(Debug, Default)]
pub(crate) struct SpecCheckResult {
    /// The locked check ran here and exited 0. Real proof, which is what lets
    /// a type with no configured commands stop reading as silence.
    pub(crate) passed: bool,
    /// Why the completion is not verified: the check failed, or its criteria
    /// moved after it was locked. `None` when the check passed or none ran.
    pub(crate) note: Option<String>,
    /// The policy refuses the completion outright (`spec_checks = "block"`).
    pub(crate) rejected: bool,
    /// The commands the lock held when this ran, so the caller can report what
    /// passed. Empty when nothing was locked.
    pub(crate) commands: Vec<String>,
}

/// Render a command list for a message: `cargo test` then `cargo clippy`.
fn describe_commands(commands: &[String]) -> String {
    if commands.is_empty() {
        return "(none)".to_string();
    }
    commands
        .iter()
        .map(|c| format!("`{c}`"))
        .collect::<Vec<_>>()
        .join(" then ")
}

/// The prompt for a generated spec check (#2063).
///
/// Deliberately narrow: the title, the description and the criteria, and
/// nothing else. No diff, no repository contents, no plan history. It asks
/// for a COMMAND, not a judgement about whether the work was done, and it
/// says plainly that "none" is a real answer: a model asked for a command
/// will invent one for "update the docs" unless it is given a way out, and
/// an invented criterion that blocks a completion is worse than no check.
pub(crate) fn build_spec_check_prompt(task: &PlanTask) -> String {
    let criteria = if task.acceptance_criteria.is_empty() {
        "(none stated)".to_string()
    } else {
        task.acceptance_criteria
            .iter()
            .map(|c| format!("- {c}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "A software task is about to start. Write the shell command that would prove \
         its acceptance criteria were met, so that command can be run when the task \
         finishes.\n\n\
         Title: {title}\nDescription: {description}\nAcceptance criteria:\n{criteria}\n\n\
         Rules:\n\
         - One command per line, and nothing else on the line. No numbering, no \
         bullets, no commentary.\n\
         - One simple command. No pipes, no redirects, no `&&`, no `;`, no command \
         substitution.\n\
         - The command must exit non-zero when the criterion is not met and 0 when \
         it is.\n\
         - The command must be runnable in the project root as it stands today, \
         before any of this work is done.\n\
         - If a criterion cannot be checked by running a command, answer none for it.\n\
         - If none of the criteria can be checked by running a command, answer NONE.\n\n\
         Answer with the commands, one per line, or NONE.",
        title = task.title,
        description = task.description,
    )
}

/// Parse a model reply into commands the gate is allowed to run (#2063).
///
/// Every candidate goes through the same validator the criteria path uses, so
/// a generated pipe or compound line is refused by the same rule that refuses
/// a criteria-derived one. The answer is text and the model will format it,
/// so a leading bullet, a number and a wrapping code fence are stripped before
/// the check rather than rejected for their punctuation.
///
/// The validator is also what keeps the runner vocabulary in one place: a
/// model cannot introduce a runner the criteria path would never accept.
pub(crate) fn parse_generated_commands(reply: &str) -> Vec<String> {
    let mut commands: Vec<String> = Vec::new();
    for raw in reply.lines() {
        // Order matters: the bullet, number or fence has to come off before
        // the backticks are trimmed, or a backticked command keeps its opening
        // backtick and the compound-operator rule refuses it for punctuation
        // the model only added for formatting.
        let line = raw.trim();
        let line = line.trim_start_matches(['-', '*', '>']).trim();
        // "1. cargo test" is a numbered answer, not a command with a dot in
        // it, but only when the part before the dot is a bare number.
        let line = match line.split_once(". ") {
            Some((head, tail)) if !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()) => {
                tail.trim()
            }
            _ => line,
        };
        // Only the OUTER backticks come off. A backtick inside the command is
        // command substitution, and that stays refused.
        let line = line.trim().trim_matches('`').trim();
        if line.is_empty() || line.eq_ignore_ascii_case("none") {
            continue;
        }
        if !is_acceptable_check_command(line) {
            continue;
        }
        if !commands.iter().any(|c| c == line) {
            commands.push(line.to_string());
        }
    }
    commands
}

/// Ask the model to write a spec check for a task (#2063).
///
/// One-shot, temperature 0, small budget: this is a command proposal, not a
/// conversation. The task's own text is the entire input.
///
/// Every failure path returns an empty vector: no provider, a provider error,
/// or a reply that honours none of the rules. A failed model call must never
/// fail a `start`, and it must never leave a half-written check behind.
pub(crate) async fn generate_spec_check_commands(
    context: &ToolExecutionContext,
    task: &PlanTask,
) -> Vec<String> {
    use crate::brain::provider::{ContentBlock, LLMRequest, Message, Provider, factory};
    use std::sync::Arc;

    let config = match crate::config::Config::load() {
        Ok(config) => config,
        Err(e) => {
            tracing::warn!("spec check generation: config load failed ({e}); no check generated");
            return Vec::new();
        }
    };
    let provider: Arc<dyn Provider> = match &context.provider_override {
        Some(injected) => Arc::clone(injected),
        None => {
            let built = match context
                .session_provider
                .as_deref()
                .filter(|name| factory::is_known_provider_name(&config, name))
            {
                Some(name) => factory::create_provider_by_name(&config, name).await,
                None => factory::create_provider(&config).await,
            };
            match built {
                Ok(provider) => provider,
                Err(e) => {
                    tracing::warn!(
                        "spec check generation: no provider resolved ({e}); no check generated"
                    );
                    return Vec::new();
                }
            }
        }
    };
    let mut request = LLMRequest::new(
        provider.default_model().to_string(),
        vec![Message::user(build_spec_check_prompt(task))],
    );
    request.temperature = Some(0.0);
    request.max_tokens = Some(256);
    let response = match provider.complete(request).await {
        Ok(response) => response,
        Err(e) => {
            tracing::warn!("spec check generation: model call failed ({e}); no check generated");
            return Vec::new();
        }
    };
    let text = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let commands = parse_generated_commands(&text);
    if commands.is_empty() {
        tracing::info!(
            "spec check generation: model proposed no runnable command for task #{}",
            task.order
        );
    } else {
        tracing::info!(
            "spec check generation: model proposed {} command(s) for task #{}: {}",
            commands.len(),
            task.order,
            describe_commands(&commands)
        );
    }
    commands
}

/// Lock a task's per-task spec check (#1904), once, at `start`.
///
/// The criteria as they read right now become the check the task must pass,
/// and their commands are frozen with a hash. Two calls must not re-derive it:
/// the post-compaction recovery call is a bare `plan start`, so a regenerated
/// check would move its own hash and void the lock it exists to keep, and a
/// task retried after a failure keeps the check it failed against rather than
/// one re-read from a criterion rewritten to match the code.
///
/// Returns the note for the start result, or `None` when the policy is `off`,
/// the task already holds a lock, or its criteria yield no runnable command.
pub(crate) fn lock_spec_check_if_absent(
    plan: &mut PlanDocument,
    order: usize,
    working_dir: &Path,
) -> Option<String> {
    let policy = ralph_loop_config(working_dir)
        .map(|c| c.verification.spec_checks)
        .unwrap_or_default();
    if policy == SpecCheckPolicy::Off {
        return None;
    }
    let task = plan.get_task_by_order_mut(order)?;
    if task.spec_check.is_some() {
        return None;
    }
    let commands = extract_check_commands(&task.acceptance_criteria);
    if commands.is_empty() {
        return None;
    }
    let note = format!(
        "🔒 Locked spec check: {}. It hashes to {} and runs when this task completes. \
         Editing the criteria afterwards voids it.",
        describe_commands(&commands),
        &SpecCheck::lock(commands.clone()).sha256[..12],
    );
    task.spec_check = Some(SpecCheck::lock(commands));
    Some(note)
}

/// Lock a spec check a model wrote (#2063), once, at `start`.
///
/// Sits BESIDE `lock_spec_check_if_absent`, not inside it, because the two
/// answer different questions: that one asks "do the criteria already name a
/// command", this one runs only when they do not. Both are called from the
/// same guarded spot, and both keep the same rule, which is what matters: the
/// `is_some()` check happens before either is reached, so the post-compaction
/// recovery `plan start` and a retry after a failure cannot regenerate a check
/// and void the hash the lock exists to hold.
///
/// The generated check is advisory until confirmed: `SpecCheck::may_block`
/// reads the provenance, so this can never reject a completion on its own,
/// even under `spec_checks = "block"`.
///
/// Returns the note for the start result, or `None` when generation is off,
/// a lock already exists, or the model proposed nothing runnable.
pub(crate) async fn lock_generated_spec_check_if_absent(
    plan: &mut PlanDocument,
    order: usize,
    context: &ToolExecutionContext,
) -> Option<String> {
    let working_dir = context.working_dir();
    let config = ralph_loop_config(&working_dir)?;
    if config.verification.spec_checks == SpecCheckPolicy::Off
        || !config.verification.spec_check_generation
    {
        return None;
    }
    // The same guard the criteria path uses, for the same reason.
    if plan.get_task_by_order(order)?.spec_check.is_some() {
        return None;
    }
    let task = plan.get_task_by_order(order)?.clone();
    let commands = generate_spec_check_commands(context, &task).await;
    if commands.is_empty() {
        return None;
    }
    let lock = SpecCheck::lock_generated(commands.clone());
    let note = format!(
        "🔒 Locked a MODEL-WRITTEN spec check: {}. It hashes to {} and runs when this          task completes, but it is advisory only until you confirm it: a generated check          never rejects a completion. Call 'start' with confirm_spec_check=true to accept          it, or rewrite the task's acceptance criteria to name the command you want.",
        describe_commands(&commands),
        &lock.sha256[..12],
    );
    plan.get_task_by_order_mut(order)?.spec_check = Some(lock);
    Some(note)
}

/// Confirm a generated spec check so it may reject a completion (#2063).
///
/// Confirmation is a separate, explicit act because the check was not in front
/// of the plan approver when they read the plan. It is recorded on the lock and
/// deliberately NOT part of the hash: `SpecCheck::matches` covers `commands`
/// only, so confirming cannot move the lock it is approving.
///
/// Returns the note for the start result, or `None` when there is nothing to
/// confirm (no lock, a criteria-derived lock which needs no confirmation, or
/// one already confirmed).
pub(crate) fn confirm_generated_spec_check(
    plan: &mut PlanDocument,
    order: usize,
) -> Option<String> {
    let task = plan.get_task_by_order_mut(order)?;
    let lock = task.spec_check.as_mut()?;
    if lock.source != SpecCheckSource::Generated || lock.confirmed {
        return None;
    }
    lock.confirmed = true;
    Some(format!(
        "✅ Confirmed the generated spec check: {}. It may now reject a completion under \
         spec_checks = \"block\".",
        describe_commands(&lock.commands)
    ))
}

/// Run the check a task locked at `start` (#1904).
///
/// The lock is compared before anything is executed. The commands are
/// re-derived from the criteria as they read NOW; if they no longer hash to the
/// lock, the criterion moved after the work began, which is the case the lock
/// exists to catch, and the check is not run at all. Running it would staple an
/// old command's exit code to a new criterion.
///
/// Policy decides the consequence of a failure: `block` refuses the
/// completion, `advisory` (default) accepts it and logs the belief as
/// Uncertain. Executing goes through `verify_with`, the same gate the per-type
/// commands use, so the vacuous-pass guard applies here too: a check whose
/// filter matches no test must not read as a pass.
pub(crate) fn run_locked_spec_check(
    plan: &PlanDocument,
    task_order: usize,
    working_dir: &Path,
) -> SpecCheckResult {
    let policy = ralph_loop_config(working_dir)
        .map(|c| c.verification.spec_checks)
        .unwrap_or_default();
    if policy == SpecCheckPolicy::Off {
        return SpecCheckResult::default();
    }
    let Some(task) = plan.get_task_by_order(task_order) else {
        return SpecCheckResult::default();
    };
    let Some(lock) = task.spec_check.as_ref() else {
        return SpecCheckResult::default();
    };
    if lock.commands.is_empty() {
        return SpecCheckResult::default();
    }

    // Staleness is a question only a criteria-derived lock can answer. That
    // lock was DERIVED from the criteria, so re-deriving them and comparing is
    // exactly the check that catches a criterion rewritten after the work
    // began. A generated lock was never derived from anything: the criteria
    // are prose by definition, re-deriving yields nothing, and comparing
    // would fail every time and read as "the criterion moved" when it never
    // did. There is no derived command here to rewrite, so the approved
    // commands simply run as locked.
    let current = extract_check_commands(&task.acceptance_criteria);
    let stale = lock.source == SpecCheckSource::Criteria && !lock.matches(&current);
    let note = if !stale {
        match verify_with(
            &task.task_type.to_string(),
            task_order,
            &lock.commands,
            true,
            &mut |cmd| run_verification_command(cmd, working_dir),
        ) {
            Ok(()) => {
                tracing::info!(
                    "Ralph loop: locked spec check passed for task #{task_order}: {}",
                    describe_commands(&lock.commands)
                );
                return SpecCheckResult {
                    passed: true,
                    note: None,
                    rejected: false,
                    commands: lock.commands.clone(),
                };
            }
            Err(msg) => format!("the locked spec check failed:\n{msg}"),
        }
    } else {
        format!(
            "the check locked at 'start' no longer matches this task's acceptance criteria, \
             so it was not run. Locked: {}. Now: {}. Start the task again once the criteria \
             are final to lock the version you mean.",
            describe_commands(&lock.commands),
            describe_commands(&current),
        )
    };

    // Provenance decides whether this check may reject, not the policy alone
    // (#2063): a model-written check that no human confirmed advises under
    // every policy, including `block`.
    let policy_wants_reject = policy == SpecCheckPolicy::Block;
    let rejected = policy_wants_reject && lock.may_block();
    let note = if policy_wants_reject && !lock.may_block() {
        format!(
            "{note}\n\n(spec_checks is \"block\", but this check was written by a model and \
             has not been confirmed, so it cannot reject the completion. Pass \
             confirm_spec_check=true to 'start' to accept it, or rewrite the criteria to \
             name the command you want.)"
        )
    } else {
        note
    };
    tracing::warn!(
        "Ralph loop: spec check for task #{task_order} {} (spec_checks={policy})",
        if rejected {
            "rejected the completion"
        } else {
            "downgraded the completion"
        }
    );
    SpecCheckResult {
        passed: false,
        note: Some(note),
        rejected,
        commands: lock.commands.clone(),
    }
}

/// The verdict for a completion claim made against acceptance criteria (#870).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CriteriaVerdict {
    /// Accept normally; log the belief at the verb's usual confidence.
    Accept,
    /// Accept, but nothing mechanically verified the claim — log as Uncertain.
    Downgrade,
    /// Refuse the completion: nothing verifies the claim.
    Reject,
}

/// Decide how to treat a success claim given the criteria policy (#870).
///
/// Pure so the whole policy matrix is unit-testable without a plan, a context,
/// or a config file on disk. The verdict judges proof, not paperwork: what
/// matters is whether verification commands ran for the task's type
/// (`Verified`), not whether criteria were declared. A claim nothing
/// verified (`NotConfigured`) is downgraded to an Uncertain belief under the
/// default policy and refused under `strict`, whether or not the task
/// declared criteria: silence is not proof (maintainer ruling, 2026-08-17).
/// A globally disabled gate (`Disabled`, an explicit user choice) always
/// accepts.
pub(crate) fn criteria_verdict(
    policy: CriteriaPolicy,
    outcome: VerificationOutcome,
) -> CriteriaVerdict {
    match outcome {
        // Proof, or an explicit gate-off, accepts under every policy.
        VerificationOutcome::Verified | VerificationOutcome::Disabled => CriteriaVerdict::Accept,
        // Nothing verified the claim: silence is not proof. Strict refuses,
        // the default downgrades the belief to Uncertain, Off keeps the
        // pre-#870 advisory behaviour (maintainer ruling, 2026-08-17).
        VerificationOutcome::NotConfigured => match policy {
            CriteriaPolicy::Strict => CriteriaVerdict::Reject,
            CriteriaPolicy::Downgrade => CriteriaVerdict::Downgrade,
            CriteriaPolicy::Off => CriteriaVerdict::Accept,
        },
    }
}

// ── Receipt binding: commit claims need a receipt (#1011) ───────────
//
// The verification gate above runs commands keyed by task TYPE, so a type
// with no configured commands downgrades to Uncertain without anyone
// checking the factual claims inside the completion output. On 2026-08-11
// 04:14 a completion claimed "Committed 7c1856c9" on a type-'Edit' task;
// the sha never existed in the repo and the gate let it through as an
// Uncertain belief. A commit claim has a trivial mechanical receipt — the
// object exists or it does not — so this check runs whatever the type.

/// Words that turn a nearby hex token into a commit claim.
const SHA_CLAIM_KEYWORDS: [&str; 5] = ["committed", "commit", "pushed", "push", "sha"];

/// How far back (in bytes) a keyword may sit from a hex token and still
/// count as its claim context.
const SHA_CLAIM_WINDOW: usize = 32;

/// Hex-looking tokens that are names of other algorithms, not commits
/// (#1501). Selection criterion: a receipted false positive only — each
/// entry is a pure-hex identifier that follows a commit-ish keyword in
/// ordinary completion text ("signed ... with ED25519 key ..."). Do not
/// speculatively add tokens; every entry needs its own occurrence.
const NON_COMMIT_HEX_TOKENS: [&str; 1] = ["ed25519"];

/// Extract git-sha claims from free text (#1011).
///
/// A claim is a maximal alphanumeric run consisting only of hex digits,
/// 7..=40 chars long (git's abbrev floor through a full sha; the cap also
/// excludes sha256/sha512 digests), preceded within `SHA_CLAIM_WINDOW`
/// bytes by a commit-ish keyword. Pure so the extraction matrix is
/// unit-testable without a repo on disk.
pub(crate) fn extract_sha_claims(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let bytes = text.as_bytes();
    let mut claims: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        // Maximal alphanumeric run: a sha candidate must not be a slice of
        // a longer identifier (call ids, tokens, base64-ish blobs).
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
            i += 1;
        }
        let run = &text[start..i];
        let is_hex = run.bytes().all(|b| b.is_ascii_hexdigit());
        let is_non_commit = NON_COMMIT_HEX_TOKENS.contains(&run.to_ascii_lowercase().as_str());
        if is_hex && !is_non_commit && (7..=40).contains(&run.len()) {
            // Back up at most SHA_CLAIM_WINDOW bytes for the keyword
            // context, then realign to a char boundary: start-32 can land
            // inside a multi-byte character, and slicing there panics. A
            // gate that panics on the output it checks is worse than none.
            let mut window_start = start.saturating_sub(SHA_CLAIM_WINDOW);
            while window_start > 0 && !lower.is_char_boundary(window_start) {
                window_start += 1;
            }
            let before = &lower[window_start..start];
            // File-hash context ("sha256 <digest>") is not a commit claim.
            let file_hash_ctx = before.contains("sha256") || before.contains("sha512");
            let claimed = SHA_CLAIM_KEYWORDS.iter().any(|k| before.contains(k));
            if claimed && !file_hash_ctx && !claims.iter().any(|c| c.eq_ignore_ascii_case(run)) {
                claims.push(run.to_string());
            }
        }
    }
    claims
}

/// Verify commit claims in a completion output against the repo at
/// `working_dir` (#1011).
///
/// `Ok(())` when there are no claims or every claimed sha exists in the
/// object store; `Err(evidence)` names each claimed sha that does not
/// exist. A directory that is not a git repo (or a missing git binary)
/// skips the check: there is no receipt to demand, and completions in
/// non-git projects must keep working exactly as before.
pub(crate) fn verify_sha_receipts(
    output: &str,
    working_dir: &Path,
) -> std::result::Result<(), String> {
    let claims = extract_sha_claims(output);
    if claims.is_empty() {
        return Ok(());
    }
    // Not a git repo -> nothing to verify against.
    let probe = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(working_dir)
        .output();
    match probe {
        Ok(out) if out.status.success() => {}
        _ => return Ok(()),
    }
    let mut missing: Vec<String> = Vec::new();
    for sha in &claims {
        let out = std::process::Command::new("git")
            .args(["cat-file", "-e", sha])
            .current_dir(working_dir)
            .output();
        let exists = matches!(out, Ok(o) if o.status.success());
        if !exists {
            missing.push(sha.clone());
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "The completion claims commit(s) {} but they do not exist in the git object store \
         at {} (`git cat-file -e` failed for each; an ambiguous prefix also fails). \
         Receipt binding (#1011): a commit claim is only accepted when the commit exists. \
         If the work is done, actually commit it (or fix the sha), then complete again.",
        missing.join(", "),
        working_dir.display()
    ))
}

/// Validate string input
pub(crate) fn validate_string(s: &str, max_len: usize, field_name: &str) -> Result<()> {
    if s.is_empty() || s.trim().is_empty() {
        return Err(ToolError::InvalidInput(format!(
            "{} cannot be empty",
            field_name
        )));
    }

    if s.len() > max_len {
        return Err(ToolError::InvalidInput(format!(
            "{} exceeds maximum length of {} characters (got {})",
            field_name,
            max_len,
            s.len()
        )));
    }

    Ok(())
}

#[async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &str {
        "plan"
    }

    fn description(&self) -> &str {
        "Manage a structured task plan for multi-step work. TWO TRACKS on `init`: \
         checklist (`mode`=\"checklist\", or inline `tasks` present) goes to Editing first \
         so the user can review before execution; design (`mode`=\"design\", or no tasks) \
         creates a session plan .md to refine and WAITS for the user to Approve it. Both \
         tracks go Active only on user Approve. While a plan is Editing, checklist \
         operations are refused and only the session .md is writable (design track). \
         \n\nOPERATIONS: `init` (create a plan, or import from a JSON `file_path`; allowed only \
         when no plan is live), `add_tasks` (append one or more tasks in a single call — the \
         primary append op), `add_task` (alias appending a single task), `start` (begin the \
         next task, or a specific one via `task_order`), `complete` (finish a task and \
         next eligible row is reported as a hint; auto-start is opt-in via
         `[agent] plan_auto_start`), `discard` (USER-ONLY: refused unless the \
         session granted plan autonomy — the user discards via /discard or the plan card's \
         Discard button), `show_plan` (read-only: report the current plan + checklist progress, e.g. \
         to answer 'what's the plan / where are we'). `add_tasks`/`add_task`/`start`/`complete` \
         are Active-only. \
         \n\nAPPROVAL: by default a plan waits in Editing for the USER to Approve (button / \
         /execute) before `start` works. If the user grants autonomy ('go for it', 'no \
         hand-holding', 'don't wait for me'), call `grant_autonomy` then `approve` to \
         self-approve and proceed WITHOUT the user; `approve` is refused unless autonomy was \
         granted. Even with autonomy you may still leave the plan for the user to Approve when \
         you judge it needs review. `revoke_autonomy` turns self-approval back off. \
         \n\nFLOW (checklist, no autonomy): init with `tasks` → WAIT for user Approve → start → (do the work) → complete → \
         (next eligible reported as hint; cascade opt-in via plan_auto_start) → complete → … `start` and `complete` return the FULL task details \
         (description, acceptance criteria, dependencies), so the plan doubles as durable \
         memory across context compactions — call `start` with no args to re-surface the \
         in-progress task's details after a compaction. \
         \n\nWHEN TO USE: call `plan` BEFORE starting any task with 3+ distinct steps, dependencies \
         between steps, that touches multiple files, or that spans multiple commits; when the user \
         asks for a plan/roadmap; when a request describes >2 deliverables; or when the user will \
         step away while you work. Skip planning for trivial single-tool answers. \
         \n\nMARK PROGRESS AS YOU GO: after each task's work is VERIFIED done (command exited 0, file \
         written, tests/clippy pass), immediately call `complete` for it before moving on. The TUI \
         progress widget counts only completed tasks, so a stale 0/N while work is done means a \
         `complete` was skipped, not that progress is tracked some other way. \
         \n\nACCEPTANCE CRITERIA: criteria are the contract a completion is judged \
         against. Each criterion must name a runnable command and the outcome it \
         must produce, e.g. 'cargo test --all-features --lib backfill_sweep reports \
         4 passed, 0 failed'. Prose outcomes ('works correctly', 'edge cases \
         handled') cannot be re-checked by a third party. 1-3 checkable criteria \
         per task. A task with no criteria can still complete, but the Ralph \
         verification gate logs it as Uncertain: silence is not proof. \
         \n\nDETAILS: `start` is idempotent on an in-progress task and resets a failed task for \
         retry. `complete` takes action=\"success\" (default), \"fail\", or \"skip\". Ordering of \
         dependencies is by task order number (1-based). A started task's acceptance criteria \
         become the session goal until it completes. Completing the last task archives the plan. \
         \n\nIMPORT: `init` with an absolute `file_path` (non-empty tasks required) goes to \
         Editing for user review and waits for an explicit user Approve; the tool \
         auto-approve policy does NOT satisfy the plan approval gate unless the \
         operator opts out via `[agent] plan_require_approval = false` (#20). \
         BUNDLED REFERENCE PLANS: source at `src/docs/reference/plans/` (embedded), runtime at \
         `~/.opencrabs/profiles/<profile>/plans/`. See `coding-plans/rust-fast.json` etc. and \
         `plan-json-spec.md`. \
         \n\nRE-TESTING AFTER BUG FIX: plans are forward-only — a completed task stays completed. \
         If a later task introduces a bug an earlier test would catch, `add_tasks` a new test task \
         rather than re-opening the completed one."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["init", "add_tasks", "add_task", "start", "complete", "approve", "discard", "show_plan", "grant_autonomy", "revoke_autonomy"],
                    "description": "init (create/import a plan), add_tasks (append one or more tasks — primary), add_task (alias, single task), start (begin next/specific task), complete (finish a task; reports next eligible as a hint), approve (self-approve Editing→Active, only if the user granted autonomy), discard (abandon the live plan → no plan; call only when the user asks to scrap/replace it), show_plan (return the current plan state, read-only), grant_autonomy (allow self-approval this session — call only when the user says to proceed without approving), revoke_autonomy (require user Approve again)"
                },
                "mode": {
                    "type": "string",
                    "enum": ["design", "checklist"],
                    "description": "init track: design (session .md, wait for user Approve; requires empty tasks) or checklist (inline tasks, Active immediately). Omitted: tasks present imply checklist, none imply design."
                },
                "title": {
                    "type": "string",
                    "description": "Plan title (init create mode) or task title (add_task)"
                },
                "description": {
                    "type": "string",
                    "description": "Task description (add_task)"
                },
                "file_path": {
                    "type": "string",
                    "description": "Import mode: absolute path to a plan JSON file on disk (init). Takes precedence over title."
                },
                "working_directory": {
                    "type": "string",
                    "description": "Optional absolute path to the repo the plan's work executes in (init). Receipt binding verifies claimed commit shas against this repo instead of the session cwd (#1452). Must exist."
                },
                "tasks": {
                    "type": "array",
                    "items": { "type": "object" },
                    "description": "Task definitions for init checklist mode or add_tasks — each: {title, description?, task_type?, complexity?, dependencies?, acceptance_criteria?}. init with tasks creates the plan and all tasks in one call; add_tasks appends (at least one)."
                },
                "task_type": {
                    "type": "string",
                    "enum": ["research", "edit", "create", "delete", "test", "refactor", "documentation", "configuration", "build"],
                    "description": "Type of task (add_task; defaults to other)"
                },
                "dependencies": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "Task order numbers (1-based) that must complete first (add_task)"
                },
                "complexity": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 5,
                    "default": 3,
                    "description": "Task complexity from 1 (simple) to 5 (very complex) (add_task)"
                },
                "acceptance_criteria": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Checkable acceptance criteria: each names a runnable command and its expected outcome, e.g. 'cargo test --lib <filter> reports 0 failed' (init tasks + add_tasks)"
                },
                "task_order": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Task number (1-based). Required for complete; optional for start (omit to pick the next task)."
                },
                "confirm_spec_check": {
                    "type": "boolean",
                    "description": "start only: confirm a MODEL-WRITTEN spec check (#2063) so it may reject a completion under spec_checks = \"block\". A criteria-derived check needs no confirmation; a generated one stays advisory until this is passed."
                },
                "isolated": {
                    "type": "boolean",
                    "description": "start only: true = a DEDICATED SUBAGENT SESSION is spawned to complete this task; this call blocks until the subagent returns and its result is verified against the plan on disk. false = no subagent is spawned; you execute the task yourself inline. Omit = config decides (agent.plan_isolated_execution, default false = you execute inline; set that key true or pass isolated:true for a dedicated subagent session)."
                },
                "action": {
                    "type": "string",
                    "enum": ["success", "fail", "skip"],
                    "description": "How a task finished (complete): success (default), fail (retry later via start), or skip"
                },
                "output": {
                    "type": "string",
                    "description": "Task result / output, stored on the task (complete)"
                },
                "insert_before": {
                    "description": "Insert task(s) before a specific position. Can be a 1-based order number, or semantic tokens 'final' / 'last' (targets before the final acceptance/gate task). Rejects positions at or before completed tasks.",
                    "oneOf": [
                        { "type": "integer", "minimum": 1 },
                        { "type": "string" }
                    ]
                },
                "insert_after": {
                    "description": "Insert task(s) after a specific position. Can be a 1-based order number, or semantic token 'current' (targets after the in-progress or next task). Rejects positions before the latest completed task.",
                    "oneOf": [
                        { "type": "integer", "minimum": 1 },
                        { "type": "string" }
                    ]
                },
                "full": {
                    "type": "boolean",
                    "description": "show_plan only: when true, returns the entire uncollapsed checklist without accordion folding."
                }
            },
            "required": ["operation"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::PlanManagement]
    }

    fn requires_approval(&self) -> bool {
        false
    }

    fn requires_approval_for_input(&self, input: &Value) -> bool {
        // `init` is the one user-visible gate: it establishes (creates or imports)
        // the plan the agent is about to execute, the same role the old
        // create/import/finalize approval served. `start`/`complete` then flow
        // without per-task prompts.
        input
            .get("operation")
            .and_then(|v| v.as_str())
            .map(|op| op == "init")
            .unwrap_or(false)
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let _: PlanOperation = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;
        Ok(())
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let operation: PlanOperation = serde_json::from_value(input)?;

        // Plan-state session. Normally this session's own id, but a spawned
        // child carrying a parent plan override (#908 option A) resolves ALL
        // plan state against the parent's session id: the JSON, design .md,
        // pre-init and autonomy markers, task goal and archives are keyed by
        // session, so they move together or the two halves of one plan
        // disagree. Path validation below is unchanged — the override only
        // selects WHICH session's .opencrabs_plan_{uuid}.json resolves.
        let plan_sid = context.plan_session_override.unwrap_or(context.session_id);

        // Load or create plan state from context (session-scoped)
        let session_dir = crate::config::opencrabs_home()
            .join("agents")
            .join("session");
        let _ = std::fs::create_dir_all(&session_dir);
        let plan_filename = format!(".opencrabs_plan_{}.json", plan_sid);
        let plan_file = session_dir.join(&plan_filename);

        // Security: Validate plan file path
        validate_plan_file_path(&plan_file, &session_dir)?;

        // Load through the shared plan store: legacy statuses map onto
        // Editing/Active, terminal legacy files resolve (Completed archives,
        // Cancelled deletes), old draft checklists normalize to Active, and
        // the size guard applies. The engine's lifecycle state (NoPlan /
        // pre-init / post-init Editing / Active) is derived from the same
        // files.
        let mut plan: Option<PlanDocument> = crate::utils::plan_files::load_plan(plan_sid).await;
        let state = crate::utils::plan_files::plan_mode_state(plan_sid).await;

        // #1195: delegated plan workers have NO plan tool access — not for
        // mutations, not for reads. They do the assigned work and report the
        // outcome in their final message; the PARENT session owns every
        // checklist transition (single-writer invariant).
        if context.plan_session_override.is_some() {
            return Ok(ToolResult::error(
                "⛔ Plan tool is unavailable to delegated worker sessions. Do the assigned \
                 work with real tool calls, then report the outcome in your final message — \
                 your parent session records the verdict on the checklist."
                    .to_string(),
            ));
        }

        let result = match operation {
            PlanOperation::Init {
                title,
                file_path,
                mode,
                tasks,
                working_directory,
            } => {
                use crate::utils::plan_files::PlanModeState;
                // A live plan blocks re-init. Pre-init is NOT live for this
                // rule: the first successful init upgrades or replaces the
                // minimal sidecar, so users who typed /plan and changed
                // their mind are never trapped.
                match state {
                    PlanModeState::PostInitEditing => {
                        return Ok(ToolResult::error(
                            "A design plan is already live for this session (Editing). \
                             Refine the session plan .md and wait for user Approve, or \
                             ask the user to /discard it before creating a new plan."
                                .to_string(),
                        ));
                    }
                    PlanModeState::Active => {
                        return Ok(ToolResult::error(
                            "A checklist is already Active for this session. Complete \
                             its remaining tasks, or ask the user to /discard it before \
                             creating a new plan."
                                .to_string(),
                        ));
                    }
                    PlanModeState::NoPlan | PlanModeState::PreInitEditing => {}
                }

                // #20: tool auto-approve never satisfies plan approval unless
                // the operator opted out via `[agent] plan_require_approval =
                // false`. Read once for both arms below.
                let require_approval = plan_require_approval_enabled();

                // #1452: optional repo binding for receipt validation. The
                // explicit param is validated upfront; an imported JSON's
                // own binding is validated in the import arm (same rule: a
                // bad binding must fail init loudly, never silently weaken
                // the receipt gate).
                let validated_working_directory = match working_directory.as_deref() {
                    Some(dir) => {
                        validate_plan_working_directory(dir)?;
                        Some(dir.to_string())
                    }
                    None => None,
                };

                if let Some(path) = file_path {
                    // ===== import mode (mode param ignored) =====
                    let import_path = std::path::Path::new(&path);
                    if !import_path.is_absolute() {
                        return Err(ToolError::InvalidInput(
                            "Import path must be absolute".to_string(),
                        ));
                    }
                    // Reject a symlink AT the target (don't walk ancestors — on
                    // macOS /var is a symlink and would reject every temp path).
                    if import_path.exists() {
                        let meta = std::fs::symlink_metadata(import_path).map_err(ToolError::Io)?;
                        if meta.is_symlink() {
                            return Err(ToolError::InvalidInput(
                                "Import path contains a symlink (security restriction)".to_string(),
                            ));
                        }
                    }
                    let metadata = tokio::fs::metadata(&import_path)
                        .await
                        .map_err(ToolError::Io)?;
                    if metadata.len() > MAX_PLAN_FILE_SIZE {
                        return Err(ToolError::InvalidInput(format!(
                            "Import file too large: {} bytes (max: {} bytes)",
                            metadata.len(),
                            MAX_PLAN_FILE_SIZE
                        )));
                    }
                    let content = tokio::fs::read_to_string(&import_path)
                        .await
                        .map_err(ToolError::Io)?;
                    let mut imported: PlanDocument = serde_json::from_str(&content)
                        .map_err(|e| ToolError::InvalidInput(format!("Invalid plan JSON: {e}")))?;

                    // The spec requires 3 root fields (title, description,
                    // tasks). PlanDocument defaults title/description so the
                    // minimal pre-init sidecar still parses, so the required
                    // contract is enforced here at the import boundary.
                    let mut missing_root = Vec::new();
                    if imported.title.trim().is_empty() {
                        missing_root.push("'title'");
                    }
                    if imported.description.trim().is_empty() {
                        missing_root.push("'description'");
                    }
                    if !missing_root.is_empty() {
                        return Ok(ToolResult::error(format!(
                            "Import refused: root {} must be present and non-empty \
                             (plan-json-spec.md requires title, description, and tasks)",
                            missing_root.join(" and ")
                        )));
                    }

                    // Same contract per task: title and description are both
                    // required (ADR 0003). Serde requires the fields to be
                    // present, so only blank values need catching here.
                    for (idx, task) in imported.tasks.iter().enumerate() {
                        if task.title.trim().is_empty() || task.description.trim().is_empty() {
                            return Ok(ToolResult::error(format!(
                                "Import refused: task {} must have a non-empty title and \
                                 description (plan-json-spec.md requires both per task)",
                                idx + 1
                            )));
                        }
                    }

                    if let Some(existing_plan) = plan.as_ref() {
                        tracing::info!(
                            "Importing plan '{}' over existing plan '{}'",
                            imported.title,
                            existing_plan.title
                        );
                    }

                    // Reassign fresh UUIDs and remap dependency references.
                    let old_to_new: std::collections::HashMap<uuid::Uuid, uuid::Uuid> = imported
                        .tasks
                        .iter()
                        .map(|t| (t.id, uuid::Uuid::new_v4()))
                        .collect();

                    imported.id = uuid::Uuid::new_v4();
                    imported.session_id = plan_sid;
                    // Import is checklist-track: tasks are already structured.
                    // Default is Editing first so the user can review before
                    // execution starts (start/complete are blocked until
                    // approval); the tool auto-approve policy does NOT satisfy
                    // the approval gate (#20) — only the explicit
                    // `[agent] plan_require_approval = false` escape hatch
                    // auto-activates (mirrors the create arm).
                    imported.status = PlanStatus::Editing;
                    imported.created_at = Utc::now();
                    imported.updated_at = Utc::now();
                    imported.approved_at = None;
                    imported.approval_source = None;

                    // #1452: the explicit init param overrides a JSON-carried
                    // binding; with no param, a JSON-carried binding is kept
                    // but validated (an imported bad path must not silently
                    // skip receipt verification at complete time).
                    imported.working_directory = match validated_working_directory {
                        Some(dir) => Some(dir),
                        None => match imported.working_directory.take() {
                            Some(dir) => {
                                validate_plan_working_directory(&dir)?;
                                Some(dir)
                            }
                            None => None,
                        },
                    };

                    imported.resolve_index_deps();

                    for task in &imported.tasks {
                        for dep in &task.dependencies {
                            if let Some(dep_id) = dep.as_uuid()
                                && !old_to_new.contains_key(&dep_id)
                            {
                                return Err(ToolError::InvalidInput(format!(
                                    "Task '{}' depends on unknown task id {}",
                                    task.title, dep_id
                                )));
                            }
                        }
                    }

                    // Imported tasks start fresh.
                    for (idx, task) in imported.tasks.iter_mut().enumerate() {
                        let new_id = old_to_new[&task.id];
                        task.id = new_id;
                        // order is auto-assigned from array position (1-based):
                        // the schema marks it Do-NOT-Provide, and dependency
                        // resolution already ran above, so overwrite it here so
                        // an omitted order never lands a task at 0 (which would
                        // collide every task on get_task_by_order).
                        task.order = idx + 1;
                        // complexity defaults to 3 when omitted (deserializes to
                        // 0) and clamps to the 1-5 scale otherwise, matching the
                        // add_task path.
                        task.complexity = if task.complexity == 0 {
                            default_complexity()
                        } else {
                            task.complexity.clamp(1, 5)
                        };
                        task.status = TaskStatus::Pending;
                        task.retry_count = 0;
                        task.notes = None;
                        task.dependencies = task
                            .dependencies
                            .iter()
                            .filter_map(|dep| {
                                dep.as_uuid()
                                    .and_then(|old_id| old_to_new.get(&old_id).copied())
                                    .map(TaskDep::Id)
                            })
                            .collect();
                    }

                    imported.validate_dependencies().map_err(|e| {
                        ToolError::InvalidInput(format!(
                            "Imported plan has invalid dependencies: {e}"
                        ))
                    })?;

                    if imported.tasks.is_empty() {
                        return Ok(ToolResult::error(
                            "Empty import is refused: the plan file has no tasks. Import \
                             needs a structured checklist; to design a plan from scratch, \
                             call init with mode=\"design\" instead."
                                .to_string(),
                        ));
                    }

                    // #20: tool auto-approve (yolo / cron / run / a2a) does NOT
                    // satisfy plan approval by default — the imported checklist
                    // waits in Editing for a human Approve like any plan. Only
                    // the explicit `[agent] plan_require_approval = false`
                    // escape hatch keeps the legacy #581 auto-activation.
                    let auto_active = context.auto_approve && !require_approval;
                    if auto_active {
                        imported.approve(ApprovalSource::Auto);
                    } else {
                        // Durable approval-queue marker (#1145) — without it an
                        // imported checklist (Editing, no `.md`) derives NoPlan
                        // and the Approve surface never appears.
                        imported.pending_approval = true;
                    }

                    let count = imported.tasks.len();
                    let plan_title = imported.title.clone();
                    let list = render_task_list(&imported);
                    plan = Some(imported);

                    if auto_active {
                        format!(
                            "📋 Imported plan: {plan_title} ({count} tasks, Active — auto-approve). \
                             No user Approve step in this mode; call 'start' to begin executing."
                        )
                    } else {
                        format!(
                            "📋 Imported plan: {plan_title} ({count} tasks, Editing).\n\n{list}\n\n\
                             WAIT for the user to approve before calling 'start': checklist \
                             operations stay blocked until the plan is Active."
                        )
                    }
                } else {
                    // ===== create mode =====
                    let title = title.ok_or_else(|| {
                        ToolError::InvalidInput(
                            "init requires either 'title' (create) or 'file_path' (import)"
                                .to_string(),
                        )
                    })?;
                    validate_string(&title, MAX_TITLE_LENGTH, "Plan title")?;

                    // Track disambiguation: explicit mode wins; otherwise
                    // tasks present imply checklist and no tasks imply design.
                    let design = match mode.as_deref() {
                        Some("design") => {
                            if !tasks.is_empty() {
                                return Ok(ToolResult::error(
                                    "mode=\"design\" with inline tasks is refused: a design \
                                     plan starts as prose in the session .md and gets its \
                                     checklist after user Approve. Either drop the tasks, or \
                                     use mode=\"checklist\" to go Active with them now."
                                        .to_string(),
                                ));
                            }
                            true
                        }
                        Some("checklist") => {
                            if tasks.is_empty() {
                                return Ok(ToolResult::error(
                                    "mode=\"checklist\" with no tasks is refused: a checklist \
                                     init needs at least one inline task. Provide `tasks`, or \
                                     use mode=\"design\" to draft the plan as prose first."
                                        .to_string(),
                                ));
                            }
                            false
                        }
                        Some(other) => {
                            return Ok(ToolResult::error(format!(
                                "Unknown mode '{other}'. Use \"design\" (session .md, user \
                                 Approve) or \"checklist\" (inline tasks, Active now)."
                            )));
                        }
                        None => tasks.is_empty(),
                    };

                    // Yolo design gate: only an explicit `/plan` slash arms
                    // the review pause (PreInitEditing with Slash origin).
                    // Anything else under auto-approve — agent-initiated
                    // design, or a keyword soft-nudge — keeps the rush
                    // behavior and is refused toward the checklist track.
                    let slash_armed = matches!(state, PlanModeState::PreInitEditing)
                        && matches!(
                            crate::utils::plan_files::pre_init_origin(plan_sid).await,
                            crate::utils::plan_files::PreInitOrigin::Slash
                        );
                    if design && context.auto_approve && !require_approval && !slash_armed {
                        return Ok(ToolResult::error(
                            "The design track under tool auto-approve is available only \
                             when the user entered Plan mode with the /plan command (the \
                             review gate). Use mode=\"checklist\" with inline tasks to \
                             proceed without a review pause, or ask the user to type \
                             /plan first."
                                .to_string(),
                        ));
                    }

                    if let Some(existing_plan) = plan.as_ref() {
                        tracing::info!(
                            "Replacing pre-init sidecar '{}' with new plan '{}'",
                            existing_plan.title,
                            title
                        );
                    }

                    let mut new_plan = PlanDocument::new(plan_sid, title.clone());
                    new_plan.status = PlanStatus::Editing;
                    // #1452: repo binding for receipt validation, when given.
                    new_plan.working_directory = validated_working_directory;

                    // Get criteria_policy for validation (#1133)
                    let policy = ralph_loop_config(&context.working_dir())
                        .map(|c| c.verification.criteria_policy)
                        .unwrap_or_default();

                    #[cfg(feature = "telegram")]
                    for it in &tasks {
                        if !it.description.is_empty()
                            && crate::channels::telegram::rich::mermaid::should_render_mermaid(
                                &it.description,
                            )
                        {
                            let parse_errors =
                                crate::channels::telegram::rich::mermaid::preflight_parse_errors(
                                    &it.description,
                                )
                                .await;
                            if !parse_errors.is_empty() {
                                return Ok(ToolResult::error(format_mermaid_plan_error(
                                    &format!("task description ('{}')", it.title),
                                    &parse_errors,
                                )));
                            }
                        }
                    }

                    for it in &tasks {
                        let parsed_type = parse_task_type(&it.task_type);
                        let order = new_plan.tasks.len() + 1;
                        validate_task_criteria_at_creation(
                            order,
                            &it.title,
                            &parsed_type,
                            &it.acceptance_criteria,
                            policy,
                        )?;
                    }

                    for it in tasks {
                        add_task_to_plan(
                            &mut new_plan,
                            it.title,
                            it.description,
                            &it.task_type,
                            &it.dependencies,
                            it.complexity,
                            it.acceptance_criteria,
                        )?;
                    }

                    // #20: tool auto-approve (yolo / cron / run / a2a) does NOT
                    // satisfy plan approval by default — the checklist waits in
                    // Editing for a human Approve. Only the explicit
                    // `[agent] plan_require_approval = false` escape hatch keeps
                    // the legacy #581 auto-activation.
                    let auto_active = !design && context.auto_approve && !require_approval;
                    if auto_active {
                        new_plan.approve(ApprovalSource::Auto);
                    } else {
                        // Durable approval-queue marker (#1145): state
                        // derivation keys on this flag, not on the design
                        // `.md` existing — checklist plans have no `.md`.
                        new_plan.pending_approval = true;
                    }

                    let count = new_plan.tasks.len();
                    plan = Some(new_plan);

                    // The design `.md` is a design-track artifact (#1145): the
                    // checklist's deliverable is the checklist itself, and the
                    // placeholder scaffold (#573) used to be written for
                    // checklist plans too — the card then rendered its hollow
                    // `## Implementation steps` section as a phantom prose block.
                    let md_path = if design {
                        Some(
                            crate::utils::plan_files::create_design_md(plan_sid, &title)
                                .await
                                .map_err(ToolError::Io)?,
                        )
                    } else {
                        None
                    };

                    if let Some(md_path) = &md_path {
                        format!(
                            "📋 Created design plan: {title} (Editing)\n\n\
                             Plan document: {}\n\n\
                             Write the design there (fill ## Context and the numbered \
                             ## Implementation steps), then WAIT for the user to approve \
                             the plan. Do NOT call 'start': checklist operations stay \
                             blocked until the plan is Active.",
                            md_path.display()
                        )
                    } else if auto_active {
                        format!(
                            "📋 Created plan: {title} ({count} tasks, Active — auto-approve). \
                             No user Approve step in this mode; call 'start' to begin executing."
                        )
                    } else {
                        format!(
                            "📋 Created plan: {title} ({count} tasks, Editing).\n\n\
                             The task list is ALREADY shown to the user in the plan card \
                             (the message carrying the Approve/Discard buttons). Do NOT \
                             repeat the tasks in your reply — that duplicates the card. \
                             Confirm the plan is ready in one short line and ask the user \
                             to approve.\n\n\
                             WAIT for the user to approve the plan before calling 'start'. \
                             Checklist operations are blocked until approval."
                        )
                    }
                }
            }

            PlanOperation::AddTasks {
                tasks,
                insert_before,
                insert_after,
            } => {
                if let Some(reason) = checklist_blocked_reason(state) {
                    return Ok(ToolResult::error(reason));
                }
                let current_plan = plan.as_mut().ok_or_else(|| {
                    ToolError::InvalidInput(
                        "No active plan. Create one with 'init' first.".to_string(),
                    )
                })?;
                if tasks.is_empty() {
                    return Ok(ToolResult::error(
                        "add_tasks needs at least one task in `tasks`.".to_string(),
                    ));
                }

                let target_idx = match resolve_insertion_index(
                    current_plan,
                    insert_before.as_ref(),
                    insert_after.as_ref(),
                ) {
                    Ok(idx) => idx,
                    Err(reason) => return Ok(ToolResult::error(reason)),
                };

                // Get criteria_policy for validation (#1133)
                let policy = ralph_loop_config(&context.working_dir())
                    .map(|c| c.verification.criteria_policy)
                    .unwrap_or_default();

                #[cfg(feature = "telegram")]
                for it in &tasks {
                    if !it.description.is_empty()
                        && crate::channels::telegram::rich::mermaid::should_render_mermaid(
                            &it.description,
                        )
                    {
                        let parse_errors =
                            crate::channels::telegram::rich::mermaid::preflight_parse_errors(
                                &it.description,
                            )
                            .await;
                        if !parse_errors.is_empty() {
                            return Ok(ToolResult::error(format_mermaid_plan_error(
                                &format!("task description ('{}')", it.title),
                                &parse_errors,
                            )));
                        }
                    }
                }

                let mut added: Vec<String> = Vec::new();
                for (offset, it) in tasks.into_iter().enumerate() {
                    let task_title = it.title.clone();
                    let parsed_type = parse_task_type(&it.task_type);
                    let insertion_idx = target_idx + offset;
                    let order = insertion_idx + 1;

                    // Validate criteria at creation (#1133)
                    validate_task_criteria_at_creation(
                        order,
                        &task_title,
                        &parsed_type,
                        &it.acceptance_criteria,
                        policy,
                    )?;

                    let order = insert_task_to_plan(
                        current_plan,
                        insertion_idx,
                        it.title,
                        it.description,
                        &it.task_type,
                        &it.dependencies,
                        it.complexity,
                        it.acceptance_criteria,
                    )?;
                    added.push(format!("  {order}. {task_title}"));
                }
                let total = current_plan.tasks.len();
                let summary = format!(
                    "✓ Added {} task(s):\n{}\n  Plan now has {total} tasks.",
                    added.len(),
                    added.join("\n")
                );
                let checklist = crate::utils::plan_mode::format_active_checklist(current_plan);
                format!("{summary}\n\n{checklist}")
            }

            PlanOperation::AddTask {
                title,
                description,
                task_type,
                dependencies,
                complexity,
                acceptance_criteria,
                insert_before,
                insert_after,
            } => {
                if let Some(reason) = checklist_blocked_reason(state) {
                    return Ok(ToolResult::error(reason));
                }
                let current_plan = plan.as_mut().ok_or_else(|| {
                    ToolError::InvalidInput(
                        "No active plan. Create one with 'init' first.".to_string(),
                    )
                })?;

                let target_idx = match resolve_insertion_index(
                    current_plan,
                    insert_before.as_ref(),
                    insert_after.as_ref(),
                ) {
                    Ok(idx) => idx,
                    Err(reason) => return Ok(ToolResult::error(reason)),
                };

                // Get criteria_policy for validation (#1133)
                let policy = ralph_loop_config(&context.working_dir())
                    .map(|c| c.verification.criteria_policy)
                    .unwrap_or_default();

                #[cfg(feature = "telegram")]
                if !description.is_empty()
                    && crate::channels::telegram::rich::mermaid::should_render_mermaid(&description)
                {
                    let parse_errors =
                        crate::channels::telegram::rich::mermaid::preflight_parse_errors(
                            &description,
                        )
                        .await;
                    if !parse_errors.is_empty() {
                        return Ok(ToolResult::error(format_mermaid_plan_error(
                            &format!("task description ('{title}')"),
                            &parse_errors,
                        )));
                    }
                }

                let parsed_type = parse_task_type(&task_type);
                let order = target_idx + 1;

                // Validate criteria at creation (#1133)
                validate_task_criteria_at_creation(
                    order,
                    &title,
                    &parsed_type,
                    &acceptance_criteria,
                    policy,
                )?;

                let order = insert_task_to_plan(
                    current_plan,
                    target_idx,
                    title.clone(),
                    description,
                    &task_type,
                    &dependencies,
                    complexity,
                    acceptance_criteria,
                )?;
                let total = current_plan.tasks.len();
                let ttype = current_plan
                    .get_task_by_order(order)
                    .unwrap()
                    .task_type
                    .clone();
                let summary = format!(
                    "✓ Added task #{order}: {title}\n  Type: {ttype} | Complexity: {}★\n  Position: {order} of {total}",
                    complexity.clamp(1, 5)
                );
                let checklist = crate::utils::plan_mode::format_active_checklist(current_plan);
                format!("{summary}\n\n{checklist}")
            }

            PlanOperation::Start {
                task_order,
                isolated,
                confirm_spec_check,
            } => {
                if let Some(reason) = checklist_blocked_reason(state) {
                    return Ok(ToolResult::error(reason));
                }
                let current_plan = plan.as_mut().ok_or_else(|| {
                    ToolError::InvalidInput(
                        "No active plan. Create one with 'init' first.".to_string(),
                    )
                })?;
                if current_plan.tasks.is_empty() {
                    return Ok(ToolResult::error(
                        "Plan has no tasks yet. Add tasks with 'add_tasks' first.".to_string(),
                    ));
                }

                // Resolve which task to start.
                let target_order: Option<usize> = match task_order {
                    Some(o) => {
                        if current_plan.get_task_by_order(o).is_none() {
                            return Ok(ToolResult::error(format!("Task #{o} does not exist.")));
                        }
                        Some(o)
                    }
                    // No arg: resume an in-progress task first (compaction
                    // recovery), otherwise pick the next pending task.
                    None => current_plan
                        .tasks
                        .iter()
                        .find(|t| matches!(t.status, TaskStatus::InProgress))
                        .map(|t| t.order)
                        .or_else(|| current_plan.next_executable_task().map(|t| t.order)),
                };

                match target_order {
                    None => {
                        if current_plan.tasks.iter().all(|t| {
                            matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped)
                        }) {
                            format!(
                                "✅ Plan complete. All {} tasks done. The plan is archived; \
                                 the session has no live plan.",
                                current_plan.tasks.len()
                            )
                        } else {
                            let blocked = current_plan
                                .tasks
                                .iter()
                                .filter(|t| matches!(t.status, TaskStatus::Pending))
                                .map(|t| format!("  ⊘ Task #{}: {}", t.order, t.title))
                                .collect::<Vec<_>>()
                                .join("\n");
                            format!(
                                "No task is ready to start — remaining tasks are blocked by \
                                 incomplete dependencies or failed tasks:\n{blocked}"
                            )
                        }
                    }
                    Some(order) => {
                        let status = current_plan
                            .get_task_by_order(order)
                            .unwrap()
                            .status
                            .clone();
                        // Starting a pending task requires its dependencies done.
                        if matches!(status, TaskStatus::Pending) {
                            let task = current_plan.get_task_by_order(order).unwrap();
                            if !current_plan.dependencies_satisfied(task) {
                                let unmet = task
                                    .dependencies
                                    .iter()
                                    .filter_map(|d| d.as_uuid())
                                    .filter_map(|id| current_plan.get_task(&id))
                                    .filter(|dep| {
                                        !matches!(
                                            dep.status,
                                            TaskStatus::Completed | TaskStatus::Skipped
                                        )
                                    })
                                    .map(|dep| format!("Task {}", dep.order))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                return Ok(ToolResult::error(format!(
                                    "⊘ Task #{order} blocked: waiting on {unmet}."
                                )));
                            }
                        }

                        let already_done =
                            matches!(status, TaskStatus::Completed | TaskStatus::Skipped);
                        // #1904: set when this `start` locks the task's spec
                        // check. Reported with the task details so the agent
                        // sees the check it will be held to.
                        // #2063: set when this `start` confirms a generated
                        // check, so the result says what the check may now do.
                        let mut confirm_note: Option<String> = None;
                        let mut lock_note: Option<String> = None;
                        if !already_done {
                            // Ralph loop: iteration cap. If the task has been
                            // retried too many times, block the start
                            // mechanically. The model cannot hallucinate its
                            // way past this gate.
                            if matches!(status, TaskStatus::Failed) {
                                let task = current_plan.get_task_by_order(order).unwrap();
                                let current_retries = task.retry_count;
                                let max_iter = ralph_loop_config(&context.working_dir())
                                    .map(|c| c.forward.max_iterations)
                                    .unwrap_or(20);
                                if (current_retries as u32) >= max_iter {
                                    return Ok(ToolResult::error(format!(
                                        "🔒 Ralph Loop iteration cap REACHED for task #{order}.\n\n\
                                         This task has failed {current_retries} times (max: {max_iter}). \
                                         The task is mechanically blocked. Either redesign the approach, \
                                         break it into smaller tasks, or ask the user to override."
                                    )));
                                }
                                tracing::info!(
                                    "Ralph loop: task #{order} retry {}/{max_iter}",
                                    current_retries + 1
                                );
                            }
                            // start() sets the task InProgress (also resets a
                            // Failed task for retry). Increments retry_count
                            // when coming from Failed state.
                            current_plan.get_task_by_order_mut(order).unwrap().start();
                            current_plan.status = PlanStatus::Active;

                            // #1904: lock the task's spec check here, once,
                            // from the criteria as they read at this moment.
                            lock_note = lock_spec_check_if_absent(
                                current_plan,
                                order,
                                &context.working_dir(),
                            );

                            // #2063: the criteria named no runnable command,
                            // so ask the model for one. This sits AFTER the
                            // criteria lock and BEFORE the is_some() guard
                            // below, and the guard is why a recovery `start`
                            // cannot regenerate: `lock_spec_check_if_absent`
                            // returned early above only when a lock exists,
                            // so check for that lock here before generating.
                            if lock_note.is_none()
                                && current_plan
                                    .get_task_by_order(order)
                                    .is_some_and(|t| t.spec_check.is_none())
                            {
                                lock_note = lock_generated_spec_check_if_absent(
                                    current_plan,
                                    order,
                                    context,
                                )
                                .await;
                            }
                        }

                        // #2063: confirming a generated check is an explicit
                        // act, separate from locking it. Runs even on the
                        // recovery path, because that is exactly when a user
                        // has just read the proposed command and decided.
                        if confirm_spec_check {
                            confirm_note = confirm_generated_spec_check(current_plan, order);
                        }

                        // #908 option A — isolated execution branch. Routes
                        // the started task to a freshly spawned worker
                        // session when isolation resolves (decision table:
                        // resolve_task_execution); otherwise falls through
                        // to the long-standing inline path below.
                        let mut isolation_note: Option<String> = None;
                        if !already_done {
                            let config_enabled = crate::config::Config::load()
                                .map(|c| c.agent.plan_isolated_execution)
                                .unwrap_or(true);
                            // Ralph keys gate isolation from the loop side:
                            // fresh_context (default true) is part of the
                            // request-resolution default; state_on_disk
                            // (default true) is a mechanical prerequisite.
                            // Hot-reload preserved: ralph_loop_config re-reads
                            // the parsed toml each call (#852); per-project
                            // resolution since #947.
                            let (fresh_context, state_on_disk) =
                                ralph_loop_config(&context.working_dir())
                                    .map(|c| (c.forward.fresh_context, c.forward.state_on_disk))
                                    .unwrap_or((true, true));
                            let path = resolve_task_execution(
                                isolated,
                                config_enabled,
                                fresh_context,
                                state_on_disk,
                                context.plan_session_override.is_some(),
                                context.service_context.is_some(),
                                context.subagent_manager.is_some()
                                    && context.parent_tool_registry.is_some(),
                                matches!(status, TaskStatus::InProgress),
                            );
                            match path {
                                TaskExecutionPath::Isolated => {
                                    let snapshot = current_plan
                                        .get_task_by_order(order)
                                        .cloned()
                                        .expect("task resolved above");
                                    let brief = build_worker_brief(
                                        order,
                                        &snapshot,
                                        &context.working_dir(),
                                        &epistemic_task_flags(current_plan.id, order),
                                    );
                                    // Persist InProgress BEFORE the spawn so
                                    // the worker, the TUI and any observer
                                    // agree on the plan state on disk.
                                    if let Err(e) =
                                        crate::utils::plan_files::save_plan(current_plan).await
                                    {
                                        return Ok(ToolResult::error(format!(
                                            "could not persist plan before spawn: {e}"
                                        )));
                                    }
                                    tracing::info!(
                                        "plan start #{order}: routing to isolated worker session"
                                    );
                                    match spawn_plan_worker(context, plan_sid, order, brief).await {
                                        Ok(worker_output) => {
                                            // Collect, don't trust: workers
                                            // hold no plan pen (#1195), so
                                            // the parent reviews the summary
                                            // and records the verdict itself.
                                            let reloaded =
                                                crate::utils::plan_files::load_plan(plan_sid).await;
                                            let (ok, report) = report_after_worker(
                                                order,
                                                reloaded.as_ref(),
                                                &worker_output,
                                            );
                                            tracing::info!(
                                                "plan start #{order}: worker collected, verdict recording pending ok={ok}"
                                            );
                                            let notice = subagent_outcome_notice(ok);
                                            return Ok(if ok {
                                                ToolResult::success(format!("{notice}\n\n{report}"))
                                            } else {
                                                ToolResult::error(format!("{notice}\n\n{report}"))
                                            });
                                        }
                                        Err(e) => {
                                            // Spawn itself failed: honest
                                            // inline fallback. The plan keeps
                                            // its InProgress mark; the inline
                                            // path below picks the task up.
                                            tracing::warn!(
                                                "plan start #{order}: isolated spawn failed ({e}); falling back inline"
                                            );
                                            isolation_note = Some(format!(
                                                "⚠️ A subagent was supposed to complete this task, but its spawn failed: {e}. No subagent is running — the task stays in-progress; do the work inline."
                                            ));
                                        }
                                    }
                                }
                                TaskExecutionPath::Inline { reason } => {
                                    if isolated.is_some() {
                                        isolation_note = Some(format!(
                                            "⚠️ No subagent could be spawned for this task ({reason}). No subagent is running — executing inline."
                                        ));
                                    }
                                    tracing::debug!(
                                        "plan start #{order}: inline execution ({reason})"
                                    );
                                }
                            }
                        }

                        let done = current_plan
                            .tasks
                            .iter()
                            .filter(|t| matches!(t.status, TaskStatus::Completed))
                            .count();
                        let total = current_plan.tasks.len();
                        let task = current_plan.get_task_by_order(order).unwrap();
                        let details = render_task_details(current_plan, task);

                        // Epistemic Orient gate (#886): surface relevant
                        // low-confidence beliefs so the agent sees prior
                        // failures / uncertain outcomes before starting work.
                        let epistemic_note = epistemic_task_flags(current_plan.id, order);

                        let result = if already_done {
                            format!(
                                "Task #{order}: {} — already {status:?}.\n\n{details}{epistemic_note}\n\n\
                                 Progress: {done}/{total} done.",
                                task.title
                            )
                        } else {
                            let lock_text = match &lock_note {
                                Some(note) => format!("\n\n{note}"),
                                None => String::new(),
                            };
                            let confirm_text = match &confirm_note {
                                Some(note) => format!("\n\n{note}"),
                                None => String::new(),
                            };
                            format!(
                                "▶️ Task #{order}: {}\n\n{details}{epistemic_note}{lock_text}{confirm_text}\n\n\
                                 Progress: {done}/{total} done. Do the work, then call 'complete' \
                                 with task_order={order}.",
                                task.title
                            )
                        };
                        match isolation_note {
                            Some(note) => format!("{result}\n\n{note}"),
                            None => format!("{result}\n\n{}", inline_executor_suffix()),
                        }
                    }
                }
            }

            PlanOperation::Complete {
                task_order,
                action,
                output,
            } => {
                if let Some(reason) = checklist_blocked_reason(state) {
                    return Ok(ToolResult::error(reason));
                }
                let current_plan = plan
                    .as_mut()
                    .ok_or_else(|| ToolError::InvalidInput("No active plan.".to_string()))?;
                if current_plan.get_task_by_order(task_order).is_none() {
                    return Ok(ToolResult::error(format!(
                        "Task #{task_order} does not exist."
                    )));
                }

                let out = if output.trim().is_empty() {
                    None
                } else {
                    Some(output.clone())
                };

                // ── Ralph Loop: mechanical verification gate ────────
                // Before accepting "success," run verification commands
                // from ralph_loop.toml. Non-zero exit = rejection.
                // The model cannot hallucinate "tests passed" when the
                // shell says otherwise.
                //
                // Criteria-aware (#870): a success claim that nothing
                // mechanically verified is downgraded to an Uncertain belief
                // (default) or rejected outright under
                // `criteria_policy = "strict"`, whether or not the task
                // declared acceptance criteria: silence is not proof
                // (maintainer ruling, 2026-08-17).
                let mut criteria_downgraded = false;
                // Why the belief was downgraded, when something more specific
                // than "nothing ran for this type" applies (#1904).
                let mut downgrade_reason: Option<String> = None;
                // The locked check that ran and passed, for the report (#1904).
                let mut spec_passed: Option<String> = None;
                if action.to_lowercase() == "success" {
                    let (task_type_str, has_criteria) = current_plan
                        .get_task_by_order(task_order)
                        .map(|t| (t.task_type.to_string(), !t.acceptance_criteria.is_empty()))
                        .unwrap_or_default();
                    match verify_task_completion(&task_type_str, task_order, &context.working_dir())
                    {
                        Err(verify_msg) => {
                            return Ok(ToolResult::error(format!(
                                "🔒 Ralph Loop verification REJECTED task #{task_order}.\n\n{verify_msg}"
                            )));
                        }
                        Ok(outcome) => {
                            // Per-task spec check (#1904): the check locked
                            // at `start`, re-derived from the criteria as they
                            // read now, run before the verdict below so that a
                            // pass counts as proof.
                            let spec = run_locked_spec_check(
                                current_plan,
                                task_order,
                                &context.working_dir(),
                            );
                            if spec.rejected {
                                return Ok(ToolResult::error(format!(
                                    "🔒 Ralph Loop spec check REJECTED task #{task_order}.\n\n{}",
                                    spec.note.as_deref().unwrap_or("the locked check failed")
                                )));
                            }
                            // A locked check that ran and exited 0 is real
                            // proof, and proof is what a type with no
                            // configured commands was missing: it turns
                            // silence into Verified.
                            let outcome = if spec.passed {
                                VerificationOutcome::Verified
                            } else {
                                outcome
                            };
                            if let Some(note) = &spec.note {
                                criteria_downgraded = true;
                                downgrade_reason = Some(note.clone());
                            }
                            if spec.passed {
                                spec_passed = Some(describe_commands(&spec.commands));
                            }
                            let policy = ralph_loop_config(&context.working_dir())
                                .map(|c| c.verification.criteria_policy)
                                .unwrap_or_default();
                            let policy = audit_criteria_policy_flip(&context.working_dir(), policy);
                            match criteria_verdict(policy, outcome) {
                                CriteriaVerdict::Reject => {
                                    let reason = if has_criteria {
                                        "it declares acceptance criteria but no verification \
                                         commands are configured for this task type"
                                            .to_string()
                                    } else {
                                        "it declares no acceptance criteria and no verification \
                                         commands are configured for this task type"
                                            .to_string()
                                    };
                                    return Ok(ToolResult::error(format!(
                                        "🔒 Ralph Loop REJECTED task #{task_order} (type={task_type_str}): \
                                         {reason}. Under `criteria_policy = \"strict\"` a success claim \
                                         without proof is refused. Configure commands under [verification] \
                                         in ralph_loop.toml for the task type (or set \
                                         criteria_policy = \"downgrade\"), then complete again."
                                    )));
                                }
                                CriteriaVerdict::Downgrade => {
                                    criteria_downgraded = true;
                                    tracing::info!(
                                        "Ralph loop: task #{task_order} completion downgraded to Uncertain \
                                         (type '{task_type_str}' ran no verification commands; criteria \
                                         declared: {has_criteria})"
                                    );
                                }
                                CriteriaVerdict::Accept => {}
                            }
                        }
                    }

                    // Receipt binding (#1011): a commit claimed in the output
                    // must exist in the repo, whatever the task type. The
                    // type-keyed commands above cannot see claims inside the
                    // output. Part of the verification gate, so it only runs
                    // when the gate is active. The verification repo resolves
                    // from the plan's bound working_directory when set
                    // (#1452): shas are checked where the work actually
                    // lives, not where the session happens to run.
                    let gate_active = ralph_loop_config(&context.working_dir())
                        .is_some_and(|c| c.verification.enabled);
                    let receipt_dir = receipt_binding_dir(
                        current_plan.working_directory.as_deref(),
                        &context.working_dir(),
                    );
                    if gate_active
                        && let Err(receipt_msg) = verify_sha_receipts(&output, &receipt_dir)
                    {
                        return Ok(ToolResult::error(format!(
                            "🔒 Ralph Loop receipt binding REJECTED task #{task_order}.\n\n{receipt_msg}"
                        )));
                    }
                }

                let (verb, emoji) = {
                    let task = current_plan.get_task_by_order_mut(task_order).unwrap();
                    match action.to_lowercase().as_str() {
                        "skip" => {
                            task.skip(out.clone());
                            ("skipped", "⏭️")
                        }
                        "fail" => {
                            task.fail(out.clone().unwrap_or_else(|| "Task failed.".to_string()));
                            ("failed", "❌")
                        }
                        "success" => {
                            task.complete(out.clone());
                            ("completed", "✅")
                        }
                        other => {
                            return Ok(ToolResult::error(format!(
                                "Unknown action '{other}'. Use 'success', 'fail', or 'skip'."
                            )));
                        }
                    }
                };

                // A resolved task's criteria-goal ends with it (fail keeps
                // the goal: the retry via `start` re-surfaces it).
                let resolved = current_plan.get_task_by_order(task_order).unwrap();
                if verb != "failed" && !resolved.acceptance_criteria.is_empty() {
                    clear_task_goal(context, plan_sid).await;
                }

                let title = current_plan
                    .get_task_by_order(task_order)
                    .unwrap()
                    .title
                    .clone();
                // Epistemic engine (#862): log task outcome as a belief.
                // A criteria-aware downgrade (#870) records Uncertain, not Verified.
                let confidence_override =
                    criteria_downgraded.then_some(super::epistemic::Confidence::Uncertain);
                log_task_outcome_belief(
                    current_plan.id,
                    task_order,
                    &title,
                    verb,
                    &out,
                    confidence_override,
                );
                let mut msg = format!("{emoji} Task #{task_order} ({title}) {verb}.");
                if criteria_downgraded {
                    let reason = downgrade_reason.as_deref().unwrap_or(
                        "this task declares acceptance criteria but no verification commands ran \
                         for its type. Configure [verification] commands in ralph_loop.toml to \
                         earn a Verified belief.",
                    );
                    msg.push_str(&format!(
                        "\n⚠️ Criteria-aware gate: completion logged as UNCERTAIN, not Verified: \
                         {reason}"
                    ));
                }
                if let Some(commands) = &spec_passed {
                    msg.push_str(&format!("\n✅ Locked spec check passed: {commands}"));
                }
                if let Some(o) = &out {
                    msg.push_str(&format!("\nOutput: {o}"));
                }

                // Auto-start is OPT-IN (#1195): complete is a pure state
                // transition by default - mark done, report the next eligible
                // row as a passive hint, spawn nothing. Separating starting
                // from completing means parents ticking off work they did
                // inline can never trigger worker spawns by accident.
                let auto_start = crate::config::Config::load()
                    .map(|c| c.agent.plan_auto_start)
                    .unwrap_or(false);
                let next_order = current_plan.next_executable_task().map(|t| t.order);
                let all_done = current_plan
                    .tasks
                    .iter()
                    .all(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped));
                if all_done {
                    msg.push_str(&format!(
                        "\n\n\u{2705} Plan complete. All {} tasks done. The checklist stays \
                         visible until this turn settles, then the plan archives.",
                        current_plan.tasks.len()
                    ));
                } else if let Some(no) = next_order {
                    if auto_start {
                        current_plan.get_task_by_order_mut(no).unwrap().start();
                        current_plan.status = PlanStatus::Active;
                        let next = current_plan.get_task_by_order(no).unwrap();
                        let details = render_task_details(current_plan, next);
                        msg.push_str(&format!(
                            "\n\n\u{25b6}\u{fe0f} Started Task #{no}: {}\n{details}",
                            next.title
                        ));
                    } else {
                        msg.push_str(&format!(
                            "\n\nNext eligible: Task #{no}: call 'start' with task_order={no} to begin it."
                        ));
                    }
                } else {
                    msg.push_str(
                        "\n\nNo unblocked task is ready next: remaining tasks are blocked or \
                         failed. Use 'start' with a task_order to retry a failed task.",
                    );
                }
                msg
            }

            // These operations return early (they don't fall through to the
            // shared save below): Approve mutates + saves the plan via
            // try_approve, so re-saving the stale local copy would clobber it;
            // the autonomy ops touch a session marker, not the plan.
            PlanOperation::Approve => {
                if !crate::utils::plan_files::is_plan_autonomy(plan_sid).await {
                    return Ok(ToolResult::error(
                        "Self-approval is off for this session. The user approves the plan via \
                         the Approve button or /execute. If the user told you to proceed \
                         autonomously ('go for it', 'no hand-holding'), call operation \
                         'grant_autonomy' first, then 'approve'."
                            .to_string(),
                    ));
                }
                return match crate::utils::plan_mode::try_approve(
                    plan_sid,
                    // #20: the approve operation is gated on a user-granted
                    // in-session autonomy, so it counts as user approval.
                    ApprovalSource::User,
                )
                .await
                {
                    crate::utils::plan_mode::ApproveOutcome::Refused(msg) => {
                        Ok(ToolResult::error(msg))
                    }
                    crate::utils::plan_mode::ApproveOutcome::SeedTurn { prompt } => {
                        Ok(ToolResult::success(format!(
                            "✅ Plan self-approved (autonomy).\n\n{prompt}"
                        )))
                    }
                };
            }
            PlanOperation::GrantAutonomy => {
                crate::utils::plan_files::set_plan_autonomy(plan_sid, true)
                    .await
                    .map_err(|e| {
                        ToolError::InvalidInput(format!("Failed to grant autonomy: {e}"))
                    })?;
                return Ok(ToolResult::success(
                    "🔓 Autonomy granted for this session: I can self-approve plans with \
                     'approve' instead of waiting for the user's Approve button. Tell the user \
                     it is on and that they can revoke it any time (/plan-auto off, or ask me \
                     to stop)."
                        .to_string(),
                ));
            }
            PlanOperation::RevokeAutonomy => {
                crate::utils::plan_files::set_plan_autonomy(plan_sid, false)
                    .await
                    .map_err(|e| {
                        ToolError::InvalidInput(format!("Failed to revoke autonomy: {e}"))
                    })?;
                return Ok(ToolResult::success(
                    "🔒 Autonomy revoked: plans now require the user's Approve again.".to_string(),
                ));
            }
            PlanOperation::Discard => {
                if matches!(state, crate::utils::plan_files::PlanModeState::NoPlan) {
                    return Ok(ToolResult::error("No live plan to discard.".to_string()));
                }
                // Discarding a live plan is the user's call, not the agent's:
                // a model that can shred its own plan can wiggle out of the
                // review harness mid-gate, or be told to by a malicious
                // message. The user discards via /discard or the plan card's
                // Discard button — both call plan_mode::discard directly and
                // never touch this tool. Sessions with granted plan autonomy
                // keep the old behavior: the user explicitly handed the agent
                // the keys (cron / a2a / hands-off flows).
                if !crate::utils::plan_files::is_plan_autonomy(plan_sid).await {
                    return Ok(ToolResult::error(
                        "Discarding a plan is a user action: ask the user to run /discard \
                         or tap the plan card's Discard button. If the user asked you in \
                         their own words to scrap the plan, relay that request instead of \
                         discarding it yourself."
                            .to_string(),
                    ));
                }
                let reply = if let Some(ref svc) = context.service_context {
                    // Full discard (also clears the plan's goal).
                    crate::utils::plan_mode::discard(plan_sid, svc).await
                } else {
                    // No service context: just remove the plan files.
                    crate::utils::plan_files::discard_plan(plan_sid).await;
                    "🗑️ Plan discarded — back to no plan.".to_string()
                };
                return Ok(ToolResult::success(reply));
            }
            PlanOperation::ShowPlan { full } => {
                let show_full = full.unwrap_or(false);
                return Ok(ToolResult::success(
                    crate::utils::plan_mode::show_plan_opt(plan_sid, show_full).await,
                ));
            }
        };

        // Save through the shared store (atomic write, canonical
        // "Editing" / "Active" status strings).
        if let Some(ref current_plan) = plan {
            crate::utils::plan_files::save_plan(current_plan)
                .await
                .map_err(|e| ToolError::InvalidInput(format!("Failed to save plan: {e}")))?;
            tracing::info!(
                "💾 Plan saved to file: {} (status: {:?})",
                plan_file.display(),
                current_plan.status
            );

            // Archive does NOT run here on last complete (ADR 0005 Decision 9):
            // the completing turn keeps its live plan and full all-☑ checklist
            // through delivery. Archive + NoPlan run at turn settle in the
            // shared tool loop (run_tool_loop_inner), which every surface hits,
            // so TUI and Telegram both archive without a surface-specific hook.
        }

        Ok(ToolResult::success(result))
    }
}

/// Cap on the flags surfaced in one task brief (#1083). Even scoped to a
/// single plan, a long checklist with retries accumulates outcomes; an
/// uncapped list crowds out the task itself in every brief.
pub(crate) const MAX_EPISTEMIC_FLAGS: usize = 5;

/// Epistemic Orient gate (#886): collect contradicted / uncertain task
/// beliefs *of this plan* so the agent sees prior failures before starting
/// work. Used by both inline starts and the isolated-worker brief.
fn epistemic_task_flags(plan_id: uuid::Uuid, task_order: usize) -> String {
    let beliefs = super::epistemic::list_by_prefix(&plan_belief_prefix(plan_id));
    render_epistemic_flags(&beliefs, plan_id, task_order)
}

/// Pure renderer for the Orient-gate block: confidence filter, this-task-first
/// ordering, and the cap. Split out so the selection logic is testable without
/// touching the process-global belief store.
pub(crate) fn render_epistemic_flags(
    beliefs: &[super::epistemic::Belief],
    plan_id: uuid::Uuid,
    task_order: usize,
) -> String {
    let this_task = format!("{}{}:", plan_belief_prefix(plan_id), task_order);
    let mut actionable: Vec<&super::epistemic::Belief> = beliefs
        .iter()
        .filter(|b| {
            matches!(
                b.confidence,
                super::epistemic::Confidence::Contradicted
                    | super::epistemic::Confidence::Uncertain
            )
        })
        .collect();
    if actionable.is_empty() {
        return String::new();
    }
    // This task's own history first — it is the only flag guaranteed relevant
    // to the work about to start. Stable within each group so the rest keeps
    // the store's order.
    actionable.sort_by_key(|b| !b.key.starts_with(&this_task));

    let total = actionable.len();
    let lines: Vec<String> = actionable
        .iter()
        .take(MAX_EPISTEMIC_FLAGS)
        .map(|b| format!("  ⚠ [{}] {}: {}", b.confidence.label(), b.key, b.value))
        .collect();
    let mut block = format!("\n\nEpistemic flags ({total}):\n") + &lines.join("\n");
    if total > lines.len() {
        block.push_str(&format!(
            "\n  … {} more suppressed (showing the {} most relevant)",
            total - lines.len(),
            lines.len()
        ));
    }
    block
}

/// Parent-facing ontology (#1195): every start names its executor in
/// subagent terms — never "isolated", which hides that a separate
/// session does the work.
pub(crate) fn subagent_outcome_notice(completed: bool) -> String {
    if completed {
        "🤖 A subagent completed this task: a dedicated subagent \
         session was spawned, finished the work, and its result was \
         verified against the plan on disk."
            .to_string()
    } else {
        "🤖 A subagent was spawned for this task but did NOT complete \
         it — verdict below."
            .to_string()
    }
}

pub(crate) fn inline_executor_suffix() -> String {
    "🤖 executor=self: no subagent was spawned — you do this task \
     in your own session."
        .to_string()
}

// ── #908 option A: isolated plan-task execution ───────────────────────────
//
// A started plan task runs either INLINE (the current session executes it,
// the long-standing behavior) or ISOLATED: a freshly spawned child session
// receives ONLY the task brief plus the parent's plan file (threaded via
// `plan_session_override`) — never the parent's conversation. Spawn is
// fresh by construction and must stay fresh; the disk is the interface
// between iterations.

/// Where a started plan task executes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TaskExecutionPath {
    Isolated,
    Inline { reason: &'static str },
}

/// Decision table for task isolation (#908 option A). Pure function: every
/// row is a deterministic test, and the start handler only supplies facts
/// it already holds. Rows, in order:
///
/// 1. Recursion guard — a session already running as a plan worker (it
///    carries a plan override) executes inline; spawning again would nest
///    workers without bound.
/// 2. Request resolution — an explicit per-call flag wins; otherwise the
///    config default (`agent.plan_isolated_execution`) AND the Ralph
///    `fresh_context` key (default true). Ralph loops pass
///    `Some(fresh_context)`.
/// 3. Machinery — spawning needs session services.
/// 4. Machinery — spawning needs a sub-agent manager AND a parent registry.
/// 5. Ralph `state_on_disk` — isolation is mechanically impossible without
///    plan state threaded on disk, even when explicitly forced.
/// 6. Idempotent retry — an InProgress task resumes inline, UNLESS
///    isolation is explicitly forced. `start` blocks until the worker
///    returns, so when start is callable there is never a live worker: an
///    InProgress task under explicit isolation is a crashed leftover or an
///    explicitly started afterwards, both safe to hand to a fresh worker.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_task_execution(
    explicit_request: Option<bool>,
    config_enabled: bool,
    fresh_context: bool,
    state_on_disk: bool,
    override_set: bool,
    has_service_context: bool,
    has_spawn_machinery: bool,
    task_already_in_progress: bool,
) -> TaskExecutionPath {
    use TaskExecutionPath::*;
    if override_set {
        return Inline {
            reason: "already inside a plan worker session",
        };
    }
    if !explicit_request.unwrap_or(config_enabled && fresh_context) {
        return Inline {
            reason: "isolated execution not requested",
        };
    }
    if !has_service_context {
        return Inline {
            reason: "no session machinery (service context)",
        };
    }
    if !has_spawn_machinery {
        return Inline {
            reason: "no spawn machinery wired (manager/registry)",
        };
    }
    if !state_on_disk {
        return Inline {
            reason: "state_on_disk disabled — plan state cannot be threaded",
        };
    }
    if task_already_in_progress && explicit_request != Some(true) {
        return Inline {
            reason: "task already in progress — retry resumes inline",
        };
    }
    Isolated
}

/// Validate a `TaskScope`. Fails closed: a malformed scope surfaces an
/// error string instead of silently producing a weak contract. Rejects:
/// - `do_write` and `do_not_write` overlap on any path
/// - any path is absolute or contains `..` (escapes `working_dir`)
/// - `do_call` names a tool in `ALWAYS_EXCLUDED` (parent intent conflicts
///   with the harness's never-allow list)
///
/// `working_dir` is currently unused in v1 validation but is reserved for
/// future per-host checks (e.g. paths under `/etc` even if relative-looking
/// via symlinks). Pass it through so the signature does not need to change.
pub(crate) fn validate_task_scope(scope: &TaskScope, working_dir: &Path) -> Result<()> {
    use std::collections::HashSet;
    let _ = working_dir; // reserved for future per-host checks

    let do_write: HashSet<&str> = scope.do_write.iter().map(String::as_str).collect();
    let do_not_write: HashSet<&str> = scope.do_not_write.iter().map(String::as_str).collect();

    let overlap: Vec<&&str> = do_write.intersection(&do_not_write).collect();
    if !overlap.is_empty() {
        return Err(ToolError::InvalidInput(format!(
            "TaskScope overlap (paths in both do_write and do_not_write): {:?}",
            overlap
        )));
    }

    for path in do_write.iter().chain(do_not_write.iter()) {
        let p = std::path::Path::new(path);
        if p.is_absolute() {
            return Err(ToolError::InvalidInput(format!(
                "TaskScope path must be relative to working_dir ({} is absolute)",
                path
            )));
        }
        if path.split(['/', '\\']).any(|seg| seg == "..") {
            return Err(ToolError::InvalidInput(format!(
                "TaskScope path must not contain '..' ({} escapes working_dir)",
                path
            )));
        }
    }

    for tool in scope.do_call.iter().flatten() {
        if ALWAYS_EXCLUDED.contains(&tool.as_str()) {
            return Err(ToolError::InvalidInput(format!(
                "TaskScope.do_call cannot contain ALWAYS_EXCLUDED tool '{}' \
                 (harness always strips it; listing it here is a contradiction)",
                tool
            )));
        }
    }

    Ok(())
}

/// Task brief handed to an isolated plan worker. The child session is
/// FRESH: this brief plus the parent's plan file is everything it gets —
/// no parent conversation, never. Keep it self-contained.
pub(crate) fn build_worker_brief(
    order: usize,
    task: &PlanTask,
    working_dir: &Path,
    epistemic_note: &str,
) -> String {
    let mut brief = format!(
        "You are a plan-task worker running in a fresh session. You have NO plan tool \
         access: you do not touch any checklist. Your parent session reads your final \
         message and records the verdict for you.\n\n\
         ▶️ Task #{}: {}\n",
        order, task.title
    );
    if !task.description.trim().is_empty() {
        brief.push_str(&format!("\nDescription: {}\n", task.description));
    }
    if !task.acceptance_criteria.is_empty() {
        brief.push_str("\nAcceptance criteria:\n");
        for c in &task.acceptance_criteria {
            brief.push_str(&format!("- {c}\n"));
        }
    }
    brief.push_str(&format!("\nWorking directory: {}\n", working_dir.display()));

    // Optional structured scope contract — when present, render an
    // explicit MAY/MUST NOT table. The brief becomes a hard contract the
    // worker must honour; the harness does not enforce post-hoc in v1.
    if let Some(scope) = &task.scope {
        match validate_task_scope(scope, working_dir) {
            Ok(()) => {
                brief.push_str("\nScope contract (HARD — paths are relative to working_dir):\n");
                if !scope.do_write.is_empty() {
                    brief.push_str("  ✅ MAY write:\n");
                    for p in &scope.do_write {
                        brief.push_str(&format!("    - {p}\n"));
                    }
                }
                if !scope.do_not_write.is_empty() {
                    brief.push_str("  🚫 MUST NOT write:\n");
                    for p in &scope.do_not_write {
                        brief.push_str(&format!("    - {p}\n"));
                    }
                }
                if let Some(do_call) = &scope.do_call
                    && !do_call.is_empty()
                {
                    brief.push_str("  ✅ MAY call tools:\n");
                    for t in do_call {
                        brief.push_str(&format!("    - {t}\n"));
                    }
                }
                if let Some(do_not_call) = &scope.do_not_call
                    && !do_not_call.is_empty()
                {
                    brief.push_str("  🚫 MUST NOT call tools:\n");
                    for t in do_not_call {
                        brief.push_str(&format!("    - {t}\n"));
                    }
                }
                brief.push_str(
                    "\nThe harness does NOT enforce this contract post-hoc in v1; \
                     treat it as a hard rule. Writes outside the MAY list will be \
                     reviewed by the parent and may cause this task to fail.\n",
                );
            }
            Err(e) => {
                brief.push_str(&format!(
                    "\n⚠️ TaskScope validation FAILED: {e}\n\
                     Falling back to free-text 'Work ONLY this task' rule.\n"
                ));
            }
        }
    }

    if !epistemic_note.trim().is_empty() {
        brief.push_str(&format!("\n{epistemic_note}\n"));
    }
    // Drift guard (#1195/#1229): pure workers verify + report; they never
    // mutate - duplicated fixes across siblings are exactly what killed the
    // Aug-25 checklist. Write-capable workers keep the original work mode.
    let work_rule = if crate::config::Config::load()
        .map(|c| !c.agent.plan_worker_allow_write)
        .unwrap_or(true)
    {
        "1. READ-ONLY WORKER: investigate, run checks, verify with real commands - do NOT edit files, stage, or commit. Discovered defects go in your FINAL MESSAGE; the parent routes fixes.\n"
    } else {
        "1. Do the work with real tool calls. Verify with real commands before claiming anything.\n"
    };
    brief.push_str(&format!(
        "\nRules:\n\
         {work_rule}\
         2. You have NO plan tool access. When done, report the outcome in your FINAL MESSAGE: \
         success or failure plus an honest summary. Never claim success you did not verify; \
         your parent session records the verdict on the checklist.\n\
         3. Work ONLY this task. Do not start other tasks; do not touch plans at all.\n",
    ));
    brief
}

/// Post-spawn collection. Workers have NO plan access (#1195): they cannot
/// mark, skip, or fail rows, so the row is expected to still be InProgress
/// when the subagent returns. False is reserved for real anomalies (plan
/// vanished / task missing) — a pending verdict is NOT an error; the parent
/// records it after reviewing the worker's summary.
pub(crate) fn report_after_worker(
    order: usize,
    reloaded: Option<&PlanDocument>,
    worker_output: &str,
) -> (bool, String) {
    let Some(plan) = reloaded else {
        return (
            false,
            format!(
                "❌ Plan file vanished after the worker run for task #{order}. Nothing verified."
            ),
        );
    };
    let Some(task) = plan.get_task_by_order(order) else {
        return (
            false,
            format!(
                "❌ Task #{order} missing from the plan after the worker run. Nothing verified."
            ),
        );
    };
    let done = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed))
        .count();
    let progress = format!("Progress: {done}/{} done.", plan.tasks.len());
    let record_hint = format!(
        "Record the verdict yourself: `complete` with task_order={order} and \
         action=success|fail (skip only if genuinely obsolete), passing this \
         summary as the output."
    );
    match &task.status {
        TaskStatus::InProgress | TaskStatus::Pending => (
            true,
            format!(
                "🤖 Task #{} ({}) — isolated subagent finished; the row is intentionally left \
                 InProgress because workers hold no plan pen (#1195).\n{progress}\n\
                 {record_hint}\n\nWorker summary:\n{worker_output}",
                order, task.title
            ),
        ),
        other => (
            true,
            format!(
                "🤖 Task #{} ({}) — isolated subagent finished; the row already records {other:?}.\n\
                 {progress}\n\nWorker summary:\n{worker_output}",
                order, task.title
            ),
        ),
    }
}

/// Spawn a fresh worker session for one plan task (#908 option A). The
/// child gets ONLY the brief plus the parent's plan file (`plan_session`
/// input → `plan_session_override` on the child context) — no parent
/// conversation. Blocks until the worker finishes. Returns the worker's
/// final output; Err means the spawn itself failed (caller falls back to
/// inline execution).
async fn spawn_plan_worker(
    context: &ToolExecutionContext,
    plan_sid: uuid::Uuid,
    order: usize,
    brief: String,
) -> std::result::Result<String, ToolError> {
    let manager = context
        .subagent_manager
        .clone()
        .ok_or_else(|| ToolError::Execution("no sub-agent manager wired".to_string()))?;
    let registry = context
        .parent_tool_registry
        .clone()
        .ok_or_else(|| ToolError::Execution("no parent tool registry wired".to_string()))?;
    let spawn_tool = crate::brain::tools::subagent::SpawnAgentTool::new(manager, registry);
    // Worker purity (#1195): plan workers cannot nest further sub-agents or
    // background tasks, and run READ-ONLY by default (drift guard: workers
    // must not re-execute changes their siblings may have made). They verify
    // and report; file mutations stay with the parent session. Opt-outs:
    // [agent] plan_worker_allow_nested / plan_worker_allow_write.
    let cfg = crate::config::Config::load().ok();
    let worker_nesting = cfg
        .as_ref()
        .map(|c| c.agent.plan_worker_allow_nested)
        .unwrap_or(false);
    let worker_read_only = !cfg
        .as_ref()
        .map(|c| c.agent.plan_worker_allow_write)
        .unwrap_or(false);
    let input = serde_json::json!({
        "prompt": brief,
        "label": format!("plan-task-{order}"),
        "agent_type": "general",
        "plan_session": plan_sid.to_string(),
        "allow_nested": worker_nesting,
        "read_only": worker_read_only,
    });
    let res = spawn_tool.execute(input, context).await?;
    if !res.success {
        let msg = res
            .error
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or(res.output);
        return Err(ToolError::Execution(format!("spawn failed: {msg}")));
    }
    Ok(res.output)
}
