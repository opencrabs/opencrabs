//! Plan Mode Data Structures
//!
//! Core data structures for plan mode, which enables structured task decomposition
//! and controlled execution for complex development tasks.
//!
//! ## Minimal Import Format
//!
//! Only 6 fields required: `title`, `description`, `tasks[]` with `title`, `description`, `task_type`.
//!
//! All other fields are auto-generated on import. See `~/.opencrabs/profiles/<profile>/plans/plan-json-spec.md`
//! for full schema documentation and `~/.opencrabs/profiles/<profile>/plans/coding-plans/` for reference examples.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Task dependency: either a 1-based index into the task list, or a direct UUID reference.
/// This allows both integer indices (easier for LLMs to write) and UUIDs (for explicit references).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "TaskDepDef", into = "TaskDepDef")]
pub enum TaskDep {
    /// 1-based index into the task list. Resolved to Uuid during import.
    Index(usize),
    /// Direct UUID reference to a task.
    Id(Uuid),
}

impl TaskDep {
    /// Convert to UUID, using the provided order-to-UUID mapping if this is an index.
    pub fn to_uuid(&self, order_to_id: &std::collections::HashMap<usize, Uuid>) -> Option<Uuid> {
        match self {
            TaskDep::Id(uuid) => Some(*uuid),
            TaskDep::Index(idx) => order_to_id.get(idx).copied(),
        }
    }

    /// Check if this is a UUID (already resolved)
    pub fn is_uuid(&self) -> bool {
        matches!(self, TaskDep::Id(_))
    }

    /// Get the UUID value if this is a UUID
    pub fn as_uuid(&self) -> Option<Uuid> {
        match self {
            TaskDep::Id(uuid) => Some(*uuid),
            TaskDep::Index(_) => None,
        }
    }
}

/// Serialization format for TaskDep - serializes as just the UUID (index is resolved before serialization)
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum TaskDepDef {
    Uuid(Uuid),
    Index(usize),
}

impl From<TaskDepDef> for TaskDep {
    fn from(def: TaskDepDef) -> Self {
        match def {
            TaskDepDef::Uuid(uuid) => TaskDep::Id(uuid),
            TaskDepDef::Index(idx) => TaskDep::Index(idx),
        }
    }
}

impl From<TaskDep> for TaskDepDef {
    fn from(dep: TaskDep) -> Self {
        match dep {
            TaskDep::Id(uuid) => TaskDepDef::Uuid(uuid),
            TaskDep::Index(idx) => TaskDepDef::Index(idx),
        }
    }
}

/// Custom deserializer for task dependencies - accepts both integer indices (1-based) and UUID strings.
/// Returns Vec<TaskDep> so we preserve the index value for later resolution.
pub fn deserialize_task_deps<'de, D>(deserializer: D) -> Result<Vec<TaskDep>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct TaskDepVisitor;

    impl<'de> serde::de::Visitor<'de> for TaskDepVisitor {
        type Value = Vec<TaskDep>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("an array of task indices (1-based integers) or UUID strings")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut deps = Vec::new();
            while let Some(val) = seq.next_element::<serde_json::Value>()? {
                match val {
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            if i < 1 {
                                return Err(serde::de::Error::custom("task indices start at 1"));
                            }
                            deps.push(TaskDep::Index(i as usize));
                        } else {
                            return Err(serde::de::Error::custom(
                                "task index must be a positive integer",
                            ));
                        }
                    }
                    serde_json::Value::String(s) => match Uuid::parse_str(&s) {
                        Ok(uuid) => deps.push(TaskDep::Id(uuid)),
                        Err(_) => {
                            return Err(serde::de::Error::custom(format!("invalid UUID: {}", s)));
                        }
                    },
                    _ => {
                        return Err(serde::de::Error::custom(
                            "task dependency must be an integer index or UUID string",
                        ));
                    }
                }
            }
            Ok(deps)
        }
    }

    deserializer.deserialize_seq(TaskDepVisitor)
}

/// Custom serializer for task type — writes lowercase string, matching the deserializer
pub fn serialize_task_type<S>(task_type: &TaskType, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let s = match task_type {
        TaskType::Research => "research",
        TaskType::Edit => "edit",
        TaskType::Create => "create",
        TaskType::Delete => "delete",
        TaskType::Test => "test",
        TaskType::Refactor => "refactor",
        TaskType::Documentation => "documentation",
        TaskType::Configuration => "configuration",
        TaskType::Build => "build",
        TaskType::Other(s) => s.as_str(),
    };
    serializer.serialize_str(s)
}

/// Custom deserializer for task type - case-insensitive. Values outside the
/// documented enum are preserved verbatim as `Other` (lossless fallback) and
/// logged so the mapping stays observable.
pub fn deserialize_task_type<'de, D>(deserializer: D) -> Result<TaskType, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    match s.to_lowercase().as_str() {
        "research" => Ok(TaskType::Research),
        "edit" => Ok(TaskType::Edit),
        "create" => Ok(TaskType::Create),
        "delete" => Ok(TaskType::Delete),
        "test" => Ok(TaskType::Test),
        "refactor" => Ok(TaskType::Refactor),
        "documentation" => Ok(TaskType::Documentation),
        "configuration" => Ok(TaskType::Configuration),
        "build" => Ok(TaskType::Build),
        "other" => Ok(TaskType::Other("other".to_string())),
        unknown => {
            tracing::debug!("Unknown task_type '{unknown}' mapped to the 'other' category");
            Ok(TaskType::Other(unknown.to_string()))
        }
    }
}

// Serde default helpers for auto-generated fields
fn default_uuid() -> Uuid {
    Uuid::new_v4()
}
fn default_now() -> DateTime<Utc> {
    Utc::now()
}

/// Who approved a plan (#20). Persisted on the plan JSON alongside
/// `approved_at` so a resume after restart can tell a genuine approval
/// from an activation the operator exempted via the escape hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSource {
    /// Explicit user approval: plan-card Approve button, `/execute`,
    /// TUI approve, positive-reaction consent signal, or the plan tool's
    /// `approve` operation (which is itself gated on a user-granted
    /// in-session autonomy).
    User,
    /// Auto-activated because the operator set
    /// `[agent] plan_require_approval = false` (#20 escape hatch). If the
    /// operator re-enables the gate, `load_plan_from_path` demotes Active
    /// plans carrying this stamp back to the approval queue on resume.
    Auto,
}

/// Plan document containing tasks and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanDocument {
    /// Unique plan ID
    #[serde(default = "default_uuid")]
    pub id: Uuid,

    /// Session this plan belongs to
    #[serde(default = "default_uuid")]
    pub session_id: Uuid,

    /// Plan title/goal. Defaults empty so the minimal pre-init Editing
    /// sidecar (flag + empty tasks, no approvable content) parses too.
    #[serde(default)]
    pub title: String,

    /// Detailed description. While post-init Editing, mirrors the full
    /// session `.md` body.
    #[serde(default)]
    pub description: String,

    /// List of tasks to complete
    pub tasks: Vec<PlanTask>,

    /// Plan status
    #[serde(default)]
    pub status: PlanStatus,

    /// When the plan was created
    #[serde(default = "default_now")]
    pub created_at: DateTime<Utc>,

    /// When the plan was last updated
    #[serde(default = "default_now")]
    pub updated_at: DateTime<Utc>,

    /// When the plan was approved (if applicable). Set on user Approve
    /// (or first `/execute`) on the design track — never auto-set by the
    /// plan tool under the auto-approve policy (#20).
    #[serde(default)]
    pub approved_at: Option<DateTime<Utc>>,

    /// Who approved the plan, when `approved_at` is set (#20). Absent on
    /// plans stamped by pre-fix binaries — those are GRANDFATHERED on
    /// resume (indistinguishable from a genuine approval, and demoting
    /// them would stall legitimately approved work). Plans stamped `Auto`
    /// (the `plan_require_approval = false` escape hatch) demote back to
    /// the approval queue on load whenever the gate is re-enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_source: Option<ApprovalSource>,

    /// Durable pre-init Editing flag: the user entered Plan-mode intent
    /// (`/plan` / soft-nudge) but `plan init` has not succeeded yet. Lives
    /// on the JSON sidecar (never on `AgentContext`, which is rebuilt every
    /// turn) so it survives restart.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pre_init_editing: bool,

    /// Durable "waiting on user Approve" flag: set on `plan init` when the
    /// plan is created in Editing (both tracks), cleared by
    /// [`PlanDocument::approve`]. This is the approval-queue marker the
    /// design `.md`'s existence was silently doubling as (#1145):
    /// `plan_mode_state_of` derives `PostInitEditing` from this flag, and
    /// the legacy-draft normalization must not Active-ify a plan that is
    /// genuinely waiting on approval. Neither may key on the `.md` —
    /// checklist plans do not have one. Survives restart like
    /// [`PlanDocument::pre_init_editing`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending_approval: bool,

    /// Repo the plan's work executes in (#1452). Optional: when set (an
    /// absolute path, validated at init), receipt binding (#1011) verifies
    /// claimed commit shas against THIS repo instead of the session cwd, so
    /// cross-repo plans (session in repo A, work in repo B) can carry
    /// mechanical commit receipts. Absent keeps the session-cwd behavior.
    #[serde(default)]
    pub working_directory: Option<String>,
}

impl PlanDocument {
    /// Create a new plan document
    pub fn new(session_id: Uuid, title: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            title,
            description: String::new(),
            tasks: Vec::new(),
            status: PlanStatus::Editing,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            approved_at: None,
            approval_source: None,
            pre_init_editing: false,
            pending_approval: false,
            working_directory: None,
        }
    }

    /// Add a task to the plan
    pub fn add_task(&mut self, task: PlanTask) {
        self.tasks.push(task);
        self.updated_at = Utc::now();
    }

    /// Insert a task at a specific 0-based vector index, shifting downstream
    /// task `order` numbers (1-based) accordingly.
    pub fn insert_task(&mut self, index: usize, mut task: PlanTask) {
        let insert_idx = index.min(self.tasks.len());
        task.order = insert_idx + 1;
        self.tasks.insert(insert_idx, task);
        for (i, t) in self.tasks.iter_mut().enumerate() {
            t.order = i + 1;
        }
        self.updated_at = Utc::now();
    }

    /// Resolve integer index dependencies to UUIDs.
    /// Integer indices (1-based) in dependencies are converted to the UUID of the task at that order.
    /// Note: This requires dependencies to use UUIDs, not integer indices.
    /// Call this after deserialization but before validation.
    pub fn resolve_index_deps(&mut self) {
        use std::collections::HashMap;

        // First pass: build order -> id mapping
        // Use 1-based indexing for tasks
        let mut order_to_id: HashMap<usize, Uuid> = HashMap::new();
        for (idx, task) in self.tasks.iter().enumerate() {
            let order = if task.order > 0 { task.order } else { idx + 1 };
            order_to_id.insert(order, task.id);
        }

        // Second pass: resolve any index dependencies to UUIDs
        for task in &mut self.tasks {
            let resolved: Vec<TaskDep> = task
                .dependencies
                .iter()
                .map(|dep| {
                    match dep {
                        TaskDep::Index(idx) => {
                            // Look up the UUID for this order
                            if let Some(uuid) = order_to_id.get(idx) {
                                TaskDep::Id(*uuid)
                            } else {
                                // Invalid index - keep as-is (will fail validation)
                                TaskDep::Index(*idx)
                            }
                        }
                        TaskDep::Id(_) => dep.clone(),
                    }
                })
                .collect();
            task.dependencies = resolved;
        }
    }

    /// Get tasks in dependency order using topological sort
    /// Returns None if there are circular dependencies
    pub fn tasks_in_order(&self) -> Option<Vec<&PlanTask>> {
        use std::collections::{HashMap, VecDeque};

        // Build dependency graph
        let mut in_degree: HashMap<Uuid, usize> = HashMap::new();
        let mut dependents: HashMap<Uuid, Vec<Uuid>> = HashMap::new();

        // Initialize in-degree for all tasks (count only UUID dependencies, skip indices)
        for task in &self.tasks {
            let uuid_deps: Vec<Uuid> = task
                .dependencies
                .iter()
                .filter_map(|d| d.as_uuid())
                .collect();
            in_degree.insert(task.id, uuid_deps.len());

            // Build reverse dependency map
            for dep_id in &uuid_deps {
                dependents.entry(*dep_id).or_default().push(task.id);
            }
        }

        // Kahn's algorithm for topological sort
        let mut queue: VecDeque<Uuid> = VecDeque::new();

        // Start with tasks that have no dependencies
        for task in &self.tasks {
            if task.dependencies.is_empty() {
                queue.push_back(task.id);
            }
        }

        let mut sorted_ids = Vec::new();

        while let Some(task_id) = queue.pop_front() {
            sorted_ids.push(task_id);

            // Process tasks that depend on this one
            if let Some(deps) = dependents.get(&task_id) {
                for &dependent_id in deps {
                    if let Some(degree) = in_degree.get_mut(&dependent_id) {
                        *degree -= 1;
                        if *degree == 0 {
                            queue.push_back(dependent_id);
                        }
                    }
                }
            }
        }

        // Check for cycles - if we didn't process all tasks, there's a cycle
        if sorted_ids.len() != self.tasks.len() {
            return None; // Circular dependency detected
        }

        // Convert sorted IDs back to task references
        let task_map: HashMap<Uuid, &PlanTask> = self.tasks.iter().map(|t| (t.id, t)).collect();

        Some(
            sorted_ids
                .iter()
                .filter_map(|id| task_map.get(id).copied())
                .collect(),
        )
    }

    /// Get task by ID
    pub fn get_task(&self, task_id: &Uuid) -> Option<&PlanTask> {
        self.tasks.iter().find(|t| t.id == *task_id)
    }

    /// Get mutable task by ID
    pub fn get_task_mut(&mut self, task_id: &Uuid) -> Option<&mut PlanTask> {
        self.updated_at = Utc::now();
        self.tasks.iter_mut().find(|t| t.id == *task_id)
    }

    /// Count tasks by status
    pub fn count_by_status(&self, status: TaskStatus) -> usize {
        self.tasks.iter().filter(|t| t.status == status).count()
    }

    /// Get progress percentage (0-100)
    pub fn progress_percentage(&self) -> f32 {
        if self.tasks.is_empty() {
            return 0.0;
        }
        let completed = self.count_by_status(TaskStatus::Completed);
        (completed as f32 / self.tasks.len() as f32) * 100.0
    }

    /// Check if all tasks are completed
    pub fn is_complete(&self) -> bool {
        !self.tasks.is_empty()
            && self
                .tasks
                .iter()
                .all(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
    }

    /// Check if the plan is currently Active with pending or incomplete tasks (#244).
    /// Returns false if the plan is in Editing status, pre-init editing, empty, or already complete.
    pub fn is_active_incomplete(&self) -> bool {
        self.status == PlanStatus::Active
            && !self.pre_init_editing
            && !self.tasks.is_empty()
            && !self.is_complete()
    }

    /// If the plan is finished except for a single trailing `Pending` task,
    /// complete it (#737). The model tends to leave the last "deliver the
    /// result" step unchecked precisely because delivering the answer IS that
    /// step — so the plan never reaches [`is_complete`](Self::is_complete) and
    /// its card lingers with 1/N unchecked. The caller invokes this only when
    /// the turn actually delivered a final response.
    ///
    /// Conservative: acts ONLY when every task before the last is already
    /// `Completed`/`Skipped`, the last task is `Pending`, and that last task
    /// declares NO acceptance criteria (#1932). A `Pending` task anywhere
    /// but the end, a `Failed`/`Blocked` last task, or a last task carrying
    /// a criteria contract is a genuine gap and is left untouched. Returns
    /// whether a task was completed.
    pub fn complete_trailing_delivery_task(&mut self) -> bool {
        let Some(last) = self.tasks.len().checked_sub(1) else {
            return false;
        };
        let head_resolved = self.tasks[..last]
            .iter()
            .all(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped));
        if head_resolved && matches!(self.tasks[last].status, TaskStatus::Pending) {
            // #1932: the auto-complete is for the DELIVERY shape only, and
            // delivery-shaped means the task declared no verifiable
            // contract. A trailing task with acceptance criteria says "this
            // box means the listed commands ran and their outcomes were
            // seen"; a turn that merely settled proves none of that. Before
            // this gate, a trailing "verify the publisher end to end" task
            // whose criterion awaited an external deploy was completed and
            // ARCHIVED as done on any turn's final text, fabricating the
            // record and making multi-turn plans structurally impossible.
            // Such tasks now stay Pending until their criteria are met by
            // an actual `complete`.
            if !self.tasks[last].acceptance_criteria.is_empty() {
                return false;
            }
            self.tasks[last].complete(Some(
                "Auto-completed at turn settle: the turn delivered its final response (#737)."
                    .to_string(),
            ));
            return true;
        }
        false
    }

    /// Approve the plan: Editing → Active, stamping `approved_at` and the
    /// approval source (#20). Called on user Approve (or first `/execute`)
    /// on the design track, or under the explicit
    /// `plan_require_approval = false` escape hatch — never by the tool
    /// auto-approve policy alone.
    pub fn approve(&mut self, source: ApprovalSource) {
        self.status = PlanStatus::Active;
        self.approved_at = Some(Utc::now());
        self.approval_source = Some(source);
        self.updated_at = Utc::now();
    }

    /// Mark the checklist live (without stamping `approved_at` — that is
    /// user Approve's job on the design track).
    pub fn start_execution(&mut self) {
        self.status = PlanStatus::Active;
        self.pending_approval = false;
        self.updated_at = Utc::now();
    }

    /// Validate task dependencies
    /// Returns Ok(()) if all dependencies are valid, or Err with description of issues
    pub fn validate_dependencies(&self) -> Result<(), String> {
        let task_ids: std::collections::HashSet<Uuid> = self.tasks.iter().map(|t| t.id).collect();

        // Check for invalid task references
        for task in &self.tasks {
            for dep in &task.dependencies {
                if dep.as_uuid().is_some_and(|id| !task_ids.contains(&id)) {
                    return Err(format!(
                        "❌ Invalid Dependency\n\n\
                         Task '{}' (#{}) depends on a task that doesn't exist.\n\n\
                         💡 Fix: Remove this dependency or ensure the referenced task is added first.",
                        task.title, task.order
                    ));
                }
            }
        }

        // Check for circular dependencies using topological sort
        let ordered = self.tasks_in_order();
        if ordered.is_none() {
            // Identify unprocessed tasks (those in the cycle)
            let unprocessed: Vec<&str> = self
                .tasks
                .iter()
                .filter(|task| !task.dependencies.is_empty())
                .map(|task| task.title.as_str())
                .collect();

            return Err(format!(
                "❌ Circular Dependency Detected\n\n\
                 Tasks with dependencies: {}\n\n\
                 💡 Fix: Review the dependency chain and remove circular references.\n\
                 Example: If Task A depends on B, B depends on C, and C depends on A,\n\
                 you need to break one of these dependency links.",
                unprocessed.join(", ")
            ));
        }

        Ok(())
    }

    /// Get the next task to execute (respecting dependencies)
    /// Returns the first pending task whose dependencies are all completed
    pub fn next_executable_task(&self) -> Option<&PlanTask> {
        let completed_ids: std::collections::HashSet<Uuid> = self
            .tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
            .map(|t| t.id)
            .collect();

        // Find first pending task with all dependencies satisfied
        self.tasks.iter().find(|task| {
            matches!(task.status, TaskStatus::Pending)
                && task
                    .dependencies
                    .iter()
                    .all(|dep| dep.as_uuid().is_some_and(|id| completed_ids.contains(&id)))
        })
    }

    /// Get mutable next executable task
    pub fn next_executable_task_mut(&mut self) -> Option<&mut PlanTask> {
        let completed_ids: std::collections::HashSet<Uuid> = self
            .tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
            .map(|t| t.id)
            .collect();

        self.updated_at = Utc::now();
        self.tasks.iter_mut().find(|task| {
            matches!(task.status, TaskStatus::Pending)
                && task
                    .dependencies
                    .iter()
                    .all(|dep| dep.as_uuid().is_some_and(|id| completed_ids.contains(&id)))
        })
    }

    /// Get task by order number (1-indexed)
    pub fn get_task_by_order(&self, order: usize) -> Option<&PlanTask> {
        self.tasks.iter().find(|t| t.order == order)
    }

    /// Get mutable task by order number (1-indexed)
    pub fn get_task_by_order_mut(&mut self, order: usize) -> Option<&mut PlanTask> {
        self.updated_at = Utc::now();
        self.tasks.iter_mut().find(|t| t.order == order)
    }

    /// Check if all dependencies for a task are satisfied
    pub fn dependencies_satisfied(&self, task: &PlanTask) -> bool {
        task.dependencies.iter().all(|dep| {
            dep.as_uuid()
                .and_then(|id| self.get_task(&id))
                .map(|dep| matches!(dep.status, TaskStatus::Completed | TaskStatus::Skipped))
                .unwrap_or(false)
        })
    }

    /// Get tasks that are ready to execute (dependencies satisfied, pending status)
    pub fn ready_tasks(&self) -> Vec<&PlanTask> {
        self.tasks
            .iter()
            .filter(|task| {
                matches!(task.status, TaskStatus::Pending) && self.dependencies_satisfied(task)
            })
            .collect()
    }
}

/// Status of a plan.
///
/// The live session model is NoPlan / Editing / Active; NoPlan means no
/// plan file exists, so the enum only carries the two live states. On
/// deserialization the seven legacy status strings map onto these two
/// (Draft / PendingApproval / Rejected → Editing; Approved / InProgress →
/// Active). Terminal legacy statuses (Completed archives, Cancelled
/// deletes) are resolved at the file level by
/// `crate::utils::plan_files::load_plan` before this map matters; the
/// serde fallbacks here keep direct parses lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanStatus {
    /// Design prose only — no checklist execution. Sub-states (pre-init
    /// vs post-init) are derived from the files on disk, not stored here.
    #[default]
    Editing,
    /// The checklist is live; a design `.md`, if present, is frozen.
    Active,
}

impl Serialize for PlanStatus {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            PlanStatus::Editing => "Editing",
            PlanStatus::Active => "Active",
        })
    }
}

impl<'de> Deserialize<'de> for PlanStatus {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "Active" | "Approved" | "InProgress" | "Completed" => PlanStatus::Active,
            // "Editing", "Draft", "PendingApproval", "Rejected", "Cancelled",
            // and anything unrecognized default to the non-executing state.
            _ => PlanStatus::Editing,
        })
    }
}

impl std::fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanStatus::Editing => write!(f, "Editing"),
            PlanStatus::Active => write!(f, "Active"),
        }
    }
}

/// Individual task within a plan
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanTask {
    /// Unique task ID
    #[serde(default = "default_uuid")]
    pub id: Uuid,

    /// Task number/order
    #[serde(default)]
    pub order: usize,

    /// Task title/summary
    pub title: String,

    /// Detailed description
    pub description: String,

    /// Task type (for categorization)
    #[serde(
        deserialize_with = "deserialize_task_type",
        serialize_with = "serialize_task_type"
    )]
    pub task_type: TaskType,

    /// Dependencies (task IDs or 1-based indices; indices resolved to UUIDs on import)
    #[serde(default, deserialize_with = "deserialize_task_deps")]
    pub dependencies: Vec<TaskDep>,

    /// Estimated complexity (1-5)
    #[serde(default)]
    pub complexity: u8,

    /// Acceptance criteria for task completion
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,

    /// Task status
    #[serde(default)]
    pub status: TaskStatus,

    /// Execution notes/results
    #[serde(default)]
    pub notes: Option<String>,

    /// Times this task was restarted from Failed state; feeds the Ralph
    /// loop iteration cap in the plan tool (mechanical, model cannot skip it).
    #[serde(default)]
    pub retry_count: u8,

    /// Machine-written verification badge (#1133). Written by the Ralph gate
    /// at `complete(action=success)`. The agent never types this field.
    /// `None` means no machine ran (exempt type, verification disabled, skip
    /// path, or legacy plan).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<VerificationVerdict>,

    /// Machine-locked per-task spec check (#1904). Written once at `start`
    /// from the task's own acceptance criteria and re-hashed at `complete`
    /// before a single command runs. `None` means no check was locked:
    /// policy `off`, nothing extractable from the criteria, or a plan
    /// written before this shipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec_check: Option<SpecCheck>,

    /// Optional structured scope contract. When set, `build_worker_brief`
    /// renders an explicit "MAY/MUST NOT" contract the worker must honour.
    /// The harness validates the scope at brief-build time (fail-closed on
    /// malformed) and does NOT enforce it post-hoc in v1. See
    /// `validate_task_scope` in `brain::tools::plan_tool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<TaskScope>,
}

/// Structured scope contract for an isolated plan worker (opt-in).
///
/// Paths are relative to the worker's `working_dir`. The harness renders
/// this as an explicit contract in the worker brief. The worker is trusted
/// to honour it; post-hoc enforcement is a separate work item.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskScope {
    /// Paths the task is expected to write/create.
    #[serde(default)]
    pub do_write: Vec<String>,

    /// Paths the task must NOT touch (typically sibling tasks' territory).
    #[serde(default)]
    pub do_not_write: Vec<String>,

    /// Tools the task should restrict itself to. `None` means inherit the
    /// parent's full tool set (minus `ALWAYS_EXCLUDED`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub do_call: Option<Vec<String>>,

    /// Tools explicitly forbidden. Naming an `ALWAYS_EXCLUDED` tool here is
    /// redundant but not wrong; the validator does not reject it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub do_not_call: Option<Vec<String>>,
}

// ── per-task spec checks (#1904) ──────────────────────────────────────
//
// Acceptance criteria are prose, and nothing in the completion path ever
// ran them: `verify_task_completion` resolved commands from the task TYPE,
// so "Task 1 compiles" was a claim, not a receipt. A spec check is the
// runnable half of a criterion, extracted once at `start` and frozen with
// a sha256, so a criterion loosened after the work began cannot execute as
// if it were the one the plan approver read.

/// Where a locked spec check's commands came from (#2063).
///
/// The provenance decides whether the check is allowed to REJECT a
/// completion. A command read out of the criteria is what the plan approver
/// read, so it may block. A command a model wrote is not, so it may only
/// advise until a human confirms it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecCheckSource {
    /// Read out of the task's own acceptance criteria by
    /// `extract_check_commands`. The default, so every plan written before
    /// #2063 reads as criteria-derived and keeps the behaviour it had.
    #[default]
    Criteria,
    /// Written by a model at `start`, from the task's title, description and
    /// criteria. Advisory until confirmed.
    Generated,
}

impl SpecCheckSource {
    /// Render-ready name for a message.
    pub fn label(&self) -> &'static str {
        match self {
            SpecCheckSource::Criteria => "criteria",
            SpecCheckSource::Generated => "generated",
        }
    }
}

/// A per-task spec check, locked at `start` and run at `complete` (#1904).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecCheck {
    /// Runnable commands, in criterion order.
    pub commands: Vec<String>,
    /// sha256 (hex) over `commands`, written when the check was locked.
    pub sha256: String,
    /// Where the commands came from (#2063). Defaults to `Criteria` for
    /// plans written before this field existed.
    #[serde(default)]
    pub source: SpecCheckSource,
    /// Whether a generated check has been confirmed (#2063). Always `false`
    /// for a criteria-derived check, which needs no confirmation: the plan
    /// approver already read it.
    #[serde(default)]
    pub confirmed: bool,
}

impl SpecCheck {
    /// Freeze a command list read out of the criteria: the commands plus the
    /// hash that locks them.
    pub fn lock(commands: Vec<String>) -> Self {
        let sha256 = spec_check_hash(&commands);
        Self {
            commands,
            sha256,
            source: SpecCheckSource::Criteria,
            confirmed: false,
        }
    }

    /// Freeze a command list a model wrote (#2063).
    pub fn lock_generated(commands: Vec<String>) -> Self {
        Self {
            source: SpecCheckSource::Generated,
            ..Self::lock(commands)
        }
    }

    /// Whether this lock still matches the commands now on the task.
    ///
    /// Deliberately independent of `source` and `confirmed`: the hash covers
    /// `commands` only, so confirming a generated check does NOT move it. A
    /// confirmation that voids the lock it just approved would be worse than
    /// no lock at all.
    pub fn matches(&self, commands: &[String]) -> bool {
        !self.sha256.is_empty() && self.sha256 == spec_check_hash(commands)
    }

    /// Whether this check may REJECT a completion (#2063).
    ///
    /// A criteria-derived check may: the approver read it. A generated check
    /// may not until a human confirms it, because a model asked for a command
    /// will invent one for "update the docs", and a gate that rejects correct
    /// work on invented criteria is worse than no gate.
    pub fn may_block(&self) -> bool {
        match self.source {
            SpecCheckSource::Criteria => true,
            SpecCheckSource::Generated => self.confirmed,
        }
    }
}

/// sha256 (hex) over a locked command list (#1904).
///
/// Domain separated and length prefixed: two lists that differ only in where
/// the command boundary falls (`"a b" + "c"` against `"a" + "b c"`) must not
/// hash alike, and a separator that happens to appear inside a command cannot
/// forge a collision between distinct lists.
pub fn spec_check_hash(commands: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"opencrabs.plan.spec_check.v1\0");
    for command in commands {
        hasher.update((command.len() as u64).to_le_bytes());
        hasher.update(command.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

impl PlanTask {
    /// Create a new task
    pub fn new(order: usize, title: String, description: String, task_type: TaskType) -> Self {
        Self {
            id: Uuid::new_v4(),
            order,
            title,
            description,
            task_type,
            dependencies: Vec::new(),
            complexity: 3, // Default medium complexity
            acceptance_criteria: Vec::new(),
            status: TaskStatus::Pending,
            notes: None,
            retry_count: 0,
            verification: None,
            spec_check: None,
            scope: None,
        }
    }

    /// Mark task as in progress. Idempotent and works from any prior state
    /// (Pending, InProgress, or Failed) so `start` can re-surface a task's
    /// details after a context compaction or to retry a failed task.
    pub fn start(&mut self) {
        if matches!(self.status, TaskStatus::Failed) {
            self.retry_count += 1;
        }
        self.status = TaskStatus::InProgress;
    }

    /// Complete the task
    pub fn complete(&mut self, notes: Option<String>) {
        self.status = TaskStatus::Completed;
        self.notes = notes;
    }

    /// Mark task as failed
    pub fn fail(&mut self, reason: String) {
        self.status = TaskStatus::Failed;
        self.notes = Some(reason);
    }

    /// Mark task as blocked
    pub fn block(&mut self, reason: String) {
        self.status = TaskStatus::Blocked(reason);
    }

    /// Skip the task
    pub fn skip(&mut self, reason: Option<String>) {
        self.status = TaskStatus::Skipped;
        if let Some(r) = reason {
            self.notes = Some(r);
        }
    }

    /// Get complexity stars (1-5)
    pub fn complexity_stars(&self) -> String {
        let filled = self.complexity.min(5);
        let empty = 5 - filled;
        "★".repeat(filled as usize) + &"☆".repeat(empty as usize)
    }
}

/// Types of tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskType {
    /// Research/exploration
    Research,
    /// File modification
    Edit,
    /// New file creation
    Create,
    /// File deletion
    Delete,
    /// Test creation/modification
    Test,
    /// Refactoring
    Refactor,
    /// Documentation
    Documentation,
    /// Configuration change
    Configuration,
    /// Build/deployment
    Build,
    /// Other
    Other(String),
}

impl std::fmt::Display for TaskType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskType::Research => write!(f, "Research"),
            TaskType::Edit => write!(f, "Edit"),
            TaskType::Create => write!(f, "Create"),
            TaskType::Delete => write!(f, "Delete"),
            TaskType::Test => write!(f, "Test"),
            TaskType::Refactor => write!(f, "Refactor"),
            TaskType::Documentation => write!(f, "Documentation"),
            TaskType::Configuration => write!(f, "Configuration"),
            TaskType::Build => write!(f, "Build"),
            TaskType::Other(s) => write!(f, "{}", s),
        }
    }
}

/// Status of individual tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum TaskStatus {
    /// Not started
    #[default]
    Pending,
    /// Currently being worked on
    InProgress,
    /// Task completed successfully
    Completed,
    /// Task skipped
    Skipped,
    /// Task failed
    Failed,
    /// Task blocked by dependencies or issues
    Blocked(String),
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "Pending"),
            TaskStatus::InProgress => write!(f, "In Progress"),
            TaskStatus::Completed => write!(f, "Completed"),
            TaskStatus::Skipped => write!(f, "Skipped"),
            TaskStatus::Failed => write!(f, "Failed"),
            TaskStatus::Blocked(reason) => write!(f, "Blocked: {}", reason),
        }
    }
}

/// Machine-written verification verdict (#1133). The agent never types this —
/// the Ralph gate writes it at `complete(action=success)`. Rendered as a badge
/// on completed rows: 🛡 Verified, 🟡 Uncertain, *(nothing)* NotRun.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum VerificationVerdict {
    /// Gate ran `task_type_commands`, all exit 0.
    Verified,
    /// Gate ran but could not confirm (failed command under downgrade, or
    /// criteria unrunnable).
    Uncertain,
}

impl VerificationVerdict {
    /// Render-ready badge for this verdict.
    pub fn badge(&self) -> &'static str {
        match self {
            VerificationVerdict::Verified => "🛡",
            VerificationVerdict::Uncertain => "🟡",
        }
    }
}

/// Render-ready status mark for a task (#1136). Single shared helper so
/// Telegram card and TUI widget produce identical marks for the same status.
///
/// Distinct marks per status (#1154):
/// - Completed: ☑ (checkmark)
/// - Skipped: ⏭ (skip forward)
/// - InProgress: ▶ (play)
/// - Pending: ☐ (empty checkbox — not started)
/// - Failed: ❌ (cross mark)
/// - Blocked: ⏸ (pause — deliberately stopped, reason attached)
pub fn status_mark(status: &TaskStatus) -> char {
    match status {
        TaskStatus::Completed => '☑',
        TaskStatus::Skipped => '⏭',
        TaskStatus::InProgress => '▶',
        TaskStatus::Pending => '☐',
        TaskStatus::Failed => '❌',
        TaskStatus::Blocked(_) => '⏸',
    }
}

// ── checklist quality glyphs ──────────────────────────────────────────
//
// A checklist row can look finished and mean nothing: a task with no
// acceptance criteria, criteria nobody can actually run, or a Completed
// box whose notes carry no receipt. These predicates mark that absence on
// the row itself, where the reader already looks, rather than in a summary
// line nobody reads.
//
// Absence-only, one glyph per task: the glyph reports what is MISSING, so
// a well-formed task renders exactly as it did before.

/// Command patterns that name something runnable. The trailing spaces are
/// load bearing: they keep a bare `git` from matching inside `digit`. Shared
/// by `is_verifiable` and `extract_check_commands` so the two definitions of
/// "this criterion names a command" cannot drift apart.
const COMMAND_PATTERNS: &[&str] = &[
    "cargo", "git ", "gh ", "npm ", "pnpm ", "make ", "curl ", "pytest", "python", "node ", "grep",
    "rg ", "sed ", "awk ", "docker", "psql", "sqlite3", "stat ", "ls ", "wc ",
];

/// Expectation stems, matched against trimmed words. Stems, not full forms:
/// the literal "exit 0" misses "exits 0" and "report" misses "shows", so one
/// stem covers the whole family.
const EXPECTATION_STEMS: &[&str] = &[
    "exit",
    "pass",
    "fail",
    "return",
    "print",
    "output",
    "show",
    "report",
    "match",
    "contain",
    "clean",
    "green",
    "no warning",
    "0 error",
];

/// A criterion is mechanically verifiable when it names something runnable
/// AND states what the run must produce. Either half alone is a wish.
pub fn is_verifiable(criterion: &str) -> bool {
    let lower = criterion.to_lowercase();
    let has_command = criterion.contains('`') || COMMAND_PATTERNS.iter().any(|p| lower.contains(p));

    let stems: Vec<String> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(|w| w.trim_end_matches("es").trim_end_matches('s').to_string())
        .collect();
    let has_expectation = EXPECTATION_STEMS
        .iter()
        .any(|p| stems.iter().any(|w| w == p) || lower.contains(p));

    has_command && has_expectation
}

/// Extract the runnable commands a task's acceptance criteria name (#1904).
///
/// Conservative on purpose: a command locked at `start` is executed at
/// `complete`, so a wrong extraction would turn a task that was done right
/// into a false failure. A criterion yields a command only when it names one
/// plainly:
///
/// - a backtick span that starts with a known runner (`cargo fmt --all`), or
/// - a known runner at a word boundary, cut at the expectation stem that
///   describes the outcome (`cargo test --lib foo reports 4 passed`).
///
/// Anything else yields nothing, and the task behaves exactly as it did
/// before spec checks existed. Order follows the criteria; duplicates drop.
pub fn extract_check_commands(criteria: &[String]) -> Vec<String> {
    let mut commands: Vec<String> = Vec::new();
    for criterion in criteria {
        if !is_verifiable(criterion) {
            continue;
        }
        if let Some(command) = extract_one_command(criterion)
            && !commands.contains(&command)
        {
            commands.push(command);
        }
    }
    commands
}

/// Shell operators a locked check may not contain. The gate RUNS this text,
/// so a compound line (a pipe, a redirect, a substitution, a second command)
/// is refused rather than parsed: one criterion, one simple command. It also
/// means a criterion cannot smuggle an extra command past the plan approver's
/// reading of the first one.
const COMPOUND_OPERATORS: &[&str] = &["&&", "||", "|", ";", ">", "<", "`", "$(", "&"];

/// Bare words that mark prose rather than a command line. Only tokens that do
/// not start with `-` are tested, so `--no-verify` and friends survive.
const PROSE_TOKENS: &[&str] = &[
    "then", "and", "the", "is", "are", "it", "its", "that", "which", "with", "output", "result",
    "when", "should", "must", "to", "of", "in", "for", "on", "no", "not", "be", "has", "have",
    "exits", "passes", "fails", "returns", "prints", "shows", "reports", "matches",
];

/// A runner with no subcommand is not a check: `cargo` alone prints help and
/// exits 0, and the gate would read that as a pass. Only a single-token
/// command that stands on its own gets through.
const SINGLE_TOKEN_OK: &[&str] = &["pytest"];

/// One criterion to one command, or nothing.
fn extract_one_command(criterion: &str) -> Option<String> {
    // 1. A backtick span that names a runner. The first such span wins, so
    //    `slugify(s)` is skipped while `cargo test` is taken.
    let mut rest = criterion;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else {
            break;
        };
        let span = after[..close].trim();
        if starts_with_runner(span)
            && let Some(command) = accept_candidate(span)
        {
            return Some(command);
        }
        rest = &after[close + 1..];
    }

    // 2. A runner at a word boundary, cut at the earliest expectation stem
    //    that still leaves a command behind it.
    let start = command_start_index(criterion)?;
    let tail = &criterion[start..];
    for cut in expectation_starts(tail).into_iter().chain([tail.len()]) {
        let candidate = tail[..cut]
            .trim()
            .trim_end_matches(&[',', '.', ';', ':'][..])
            .trim();
        if let Some(command) = accept_candidate(candidate) {
            return Some(command);
        }
    }
    None
}

/// Whether a string opens with a known runner.
fn starts_with_runner(candidate: &str) -> bool {
    let lower = candidate.to_lowercase();
    COMMAND_PATTERNS
        .iter()
        .map(|p| p.trim())
        .any(|p| lower.starts_with(p))
}

/// Index where a known runner starts, matched case-insensitively at a word
/// boundary. `digit` and `github` are not `git`; `python3` is.
fn command_start_index(haystack: &str) -> Option<usize> {
    let lower = haystack.to_lowercase();
    if lower.len() != haystack.len() {
        // Non-ASCII lowercasing can move byte offsets. Refuse rather than
        // slice a command out of the wrong place.
        return None;
    }
    let bytes = lower.as_bytes();
    let mut best: Option<usize> = None;
    for pattern in COMMAND_PATTERNS.iter().map(|p| p.trim()) {
        let mut from = 0;
        while let Some(offset) = lower[from..].find(pattern) {
            let at = from + offset;
            from = at + pattern.len();
            let before_ok = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
            let after_ok = match bytes.get(at + pattern.len()).copied() {
                None => true,
                Some(b) => !b.is_ascii_alphanumeric() || b.is_ascii_digit(),
            };
            if before_ok && after_ok {
                best = Some(best.map_or(at, |seen: usize| seen.min(at)));
            }
        }
    }
    best
}

/// Byte offsets where a word (or run of consecutive words) that matches an
/// expectation stem begins.
fn expectation_starts(haystack: &str) -> Vec<usize> {
    let words = word_runs(haystack);
    let mut starts: Vec<usize> = Vec::new();
    for stem in EXPECTATION_STEMS {
        let parts: Vec<&str> = stem.split_whitespace().collect();
        if parts.is_empty() || words.len() < parts.len() {
            continue;
        }
        for window in 0..=(words.len() - parts.len()) {
            let hit = parts.iter().enumerate().all(|(k, part)| {
                words[window + k]
                    .1
                    .trim_end_matches("es")
                    .trim_end_matches('s')
                    == *part
            });
            if hit {
                starts.push(words[window].0);
            }
        }
    }
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// `(byte offset, lowercased word)` for every alphanumeric run.
fn word_runs(haystack: &str) -> Vec<(usize, String)> {
    let mut words: Vec<(usize, String)> = Vec::new();
    let mut start: Option<usize> = None;
    for (idx, ch) in haystack.char_indices() {
        if ch.is_ascii_alphanumeric() {
            start.get_or_insert(idx);
        } else if let Some(from) = start.take() {
            words.push((from, haystack[from..idx].to_lowercase()));
        }
    }
    if let Some(from) = start {
        words.push((from, haystack[from..].to_lowercase()));
    }
    words
}

/// The final gate: a candidate becomes a locked command only if it is one
/// simple command, not prose and not a bare runner.
fn accept_candidate(candidate: &str) -> Option<String> {
    if candidate.is_empty() {
        return None;
    }
    if COMPOUND_OPERATORS
        .iter()
        .any(|operator| candidate.contains(operator))
    {
        return None;
    }
    let tokens: Vec<&str> = candidate.split_whitespace().collect();
    let is_prose = tokens.iter().any(|token| {
        !token.starts_with('-') && PROSE_TOKENS.contains(&token.to_lowercase().as_str())
    });
    if is_prose {
        return None;
    }
    if tokens.len() == 1 && !SINGLE_TOKEN_OK.contains(&tokens[0]) {
        return None;
    }
    Some(candidate.to_string())
}

/// Public face of the final gate, for callers outside this module (#2063).
///
/// A generated command must be held to the SAME rule a criteria-derived one
/// is: the gate runs the text either way, so a pipe or a compound line is
/// refused no matter who wrote it. Reusing this also keeps the runner
/// vocabulary in one place, so a model cannot introduce a runner the criteria
/// path would never have accepted.
pub fn is_acceptable_check_command(candidate: &str) -> bool {
    accept_candidate(candidate.trim()).is_some()
}

/// The single glyph a task has earned, or `None` when nothing is missing.
///
/// Exempt types (documentation, research, other) never earn ⚠️/🔓 because they
/// cannot reasonably name a runnable command (#1133). The ❔ arm was removed
/// in #1133 — replaced by machine-written `task.verification` badges.
pub fn quality_glyph(task: &PlanTask) -> Option<&'static str> {
    // Exempt types: no criteria warning (#1133)
    if !requires_checkable_criteria(&task.task_type) {
        return None;
    }
    if task.acceptance_criteria.is_empty() {
        return Some("⚠️");
    }
    if !task.acceptance_criteria.iter().any(|c| is_verifiable(c)) {
        return Some("🔓");
    }
    None
}

/// Whether a task type requires checkable criteria (#1133).
/// Mirror of `plan_tool::requires_checkable_criteria` for the TUI module.
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

/// [`quality_glyph`] as a render-ready suffix: a leading space plus the
/// glyph, or the empty string. Both surfaces append it verbatim, so the
/// spacing can never drift between the tool output and the chat card.
pub fn quality_glyph_suffix(task: &PlanTask) -> String {
    quality_glyph(task)
        .map(|g| format!(" {g}"))
        .unwrap_or_default()
}
