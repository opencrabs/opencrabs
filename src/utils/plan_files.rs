//! Shared session plan file store: the single loader/saver for the plan
//! lifecycle engine (NoPlan / Editing / Active).
//!
//! Plan artifacts live in the session's resolved directory (see
//! [`session_dir`]): `<project>/session/` when the session is bound to a
//! project, otherwise the profile/default `<home>/session/`:
//! - `.opencrabs_plan_{session_id}.json`: the live store (status, title,
//!   checklist). The minimal pre-init Editing sidecar uses the same path.
//! - `.opencrabs_plan_{session_id}.md`: canonical design prose while the
//!   plan is post-init Editing; frozen against generic writes once Active.
//! - `archive/`: completed plans move here with a timestamp; there is no
//!   lingering live "Done" status.
//!
//! Resolution is a DB lookup (session -> project), so the path helpers are
//! async. Reads fall back to the legacy flat `~/.opencrabs/agents/session/`
//! so plans written by an older binary are not orphaned; writes always go to
//! the resolved location.
//!
//! Legacy seven-status JSON is mapped on load (see [`PlanStatus`]'s
//! deserializer). Two terminal legacy statuses are resolved here, at the
//! file level, because they end the plan's life rather than describe it:
//! `Completed` archives silently and `Cancelled` deletes: both yield
//! `None` (NoPlan).

use crate::tui::plan::{ApprovalSource, PlanDocument, PlanStatus};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// The session's data directory, resolved by what the session is bound to
/// (project > profile > neither), holding its plan artifacts (and, going
/// forward, its attachments):
/// - project-bound: `~/.opencrabs/projects/<slug>/session/`
/// - named profile / default: `<profile home>/session/`
///
/// The project branch is a DB lookup (session -> project), so this is async.
/// It reads the process-global pool the same way the channel-send tools do
/// (`crate::db::global_pool`), and falls back to the profile/default home
/// when there is no pool (tests, early startup) or the session is not bound
/// to a project. `opencrabs_home()` is already profile-aware, so its
/// `session/` child covers both the named-profile and default cases.
pub async fn session_dir(session_id: Uuid) -> PathBuf {
    if let Some(pool) = crate::db::global_pool()
        && let Some(dir) = project_session_dir(session_id, pool).await
    {
        return dir;
    }
    crate::config::opencrabs_home().join("session")
}

/// `<project>/session/` when the session is bound to a project. Mirrors
/// [`crate::services::FileService::project_files_dir`] but resolves the
/// session subdir instead of `files/`, so a project's plan and attachments
/// live under one roof. `pub`: the GUI bin is its own crate and its bridge
/// thread resolves the same path on its own runtime (the global pool needs a
/// Tokio reactor the gpui executor does not have).
pub async fn project_session_dir(session_id: Uuid, pool: &crate::db::Pool) -> Option<PathBuf> {
    use crate::db::repository::{ProjectRepository, SessionRepository};
    let session = SessionRepository::new(pool.clone())
        .find_by_id(session_id)
        .await
        .ok()??;
    let project_id = session.project_id?;
    let project = ProjectRepository::new(pool.clone())
        .find_by_id(project_id)
        .await
        .ok()??;
    Some(
        crate::services::ProjectService::projects_dir()
            .join(crate::services::file::slugify_project_name(&project.name))
            .join("session"),
    )
}

/// Legacy pre-resolution location: the flat `~/.opencrabs/agents/session/`
/// every plan used before the location became project/profile-aware. Read
/// paths fall back here so plans written by an older binary aren't orphaned;
/// writes always go to the resolved [`session_dir`].
fn legacy_session_dir() -> PathBuf {
    crate::config::opencrabs_home()
        .join("agents")
        .join("session")
}

/// `<session_dir>/archive/`: where completed plans retire.
pub async fn archive_dir(session_id: Uuid) -> PathBuf {
    session_dir(session_id).await.join("archive")
}

/// Live plan JSON path for a session (resolved write location).
pub async fn plan_json_path(session_id: Uuid) -> PathBuf {
    session_dir(session_id)
        .await
        .join(format!(".opencrabs_plan_{session_id}.json"))
}

/// Session design markdown path (resolved write location; exists only after a
/// design-track `init`).
pub async fn plan_md_path(session_id: Uuid) -> PathBuf {
    session_dir(session_id)
        .await
        .join(format!(".opencrabs_plan_{session_id}.md"))
}

/// Durable pre-init marker path (resolved write location). Its mere presence
/// means the session entered Plan-mode intent (`/plan` / soft-nudge) before any
/// `plan init`. It carries no content — pre-init has no approvable document —
/// so this replaces the old stub plan JSON that materialized a fake
/// `PlanDocument` purely to hold this one bit (#569). Named as a sibling of the
/// plan `.json`/`.md` so the sync cleanup helpers can derive it path-only.
pub async fn pre_init_marker_path(session_id: Uuid) -> PathBuf {
    session_dir(session_id)
        .await
        .join(format!(".opencrabs_plan_{session_id}.preinit"))
}

/// The pre-init marker to READ: resolved location if present, else the legacy
/// flat dir, else the resolved path (mirrors [`plan_json_read_path`]).
async fn pre_init_marker_read_path(session_id: Uuid) -> PathBuf {
    let resolved = pre_init_marker_path(session_id).await;
    if resolved.exists() {
        return resolved;
    }
    let legacy = legacy_session_dir().join(format!(".opencrabs_plan_{session_id}.preinit"));
    if legacy.exists() { legacy } else { resolved }
}

/// The pre-init marker sibling of a plan JSON path (same stem, `.preinit`), for
/// the sync path-based cleanup helpers that hold a path but no session id.
fn pre_init_marker_for(json_path: &Path) -> PathBuf {
    json_path.with_extension("preinit")
}

/// Durable per-session plan-autonomy marker. Its presence means the user has
/// granted the agent autonomy to self-approve plans in this session ("go for
/// it" / no hand-holding), so `plan approve` is allowed without the user's
/// Approve button / `/execute` (#581). It is a SESSION policy, not tied to any
/// one plan, so it is NOT removed on plan discard/complete — only by an explicit
/// revoke. Default absent = today's behavior (user approval required).
async fn plan_autonomy_marker_path(session_id: Uuid) -> PathBuf {
    session_dir(session_id)
        .await
        .join(format!(".opencrabs_autonomy_{session_id}"))
}

/// True when plan self-approval is granted for the session (#581). Checks the
/// resolved location then the legacy flat dir.
pub async fn is_plan_autonomy(session_id: Uuid) -> bool {
    if plan_autonomy_marker_path(session_id).await.exists() {
        return true;
    }
    legacy_session_dir()
        .join(format!(".opencrabs_autonomy_{session_id}"))
        .exists()
}

/// Grant (`true`) or revoke (`false`) plan self-approval autonomy for the
/// session (#581). Durable across restart and across plans.
pub async fn set_plan_autonomy(session_id: Uuid, enabled: bool) -> std::io::Result<()> {
    let marker = plan_autonomy_marker_path(session_id).await;
    if enabled {
        if let Some(dir) = marker.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&marker, b"")
    } else {
        // Remove both the resolved and legacy markers so a revoke is complete.
        for p in [
            marker,
            legacy_session_dir().join(format!(".opencrabs_autonomy_{session_id}")),
        ] {
            if p.exists()
                && let Err(e) = std::fs::remove_file(&p)
            {
                tracing::warn!("Failed to clear plan-autonomy marker {}: {e}", p.display());
            }
        }
        Ok(())
    }
}

/// The JSON path to READ from: the resolved location if the file is present
/// there, else the legacy flat dir (so an older binary's plan is still
/// found), else the resolved path (the not-yet-created case). Writes never
/// use this: they always target [`plan_json_path`].
pub async fn plan_json_read_path(session_id: Uuid) -> PathBuf {
    let resolved = plan_json_path(session_id).await;
    if resolved.exists() {
        return resolved;
    }
    let legacy = legacy_session_dir().join(format!(".opencrabs_plan_{session_id}.json"));
    if legacy.exists() { legacy } else { resolved }
}

/// The live plan-mode state of a session, derived from the files on disk.
///
/// This is the engine's source of truth: implementers must not treat
/// "file exists" as "live plan" (the pre-init flag is a first-class bit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanModeState {
    /// No plan artifacts and no durable pre-init flag.
    NoPlan,
    /// Plan-mode intent entered (`/plan` / soft-nudge) but `plan init` has
    /// not succeeded yet: minimal JSON flag only, no approvable `.md`.
    PreInitEditing,
    /// Design track after a successful `plan init`: `.md` + `.json`,
    /// `tasks` empty, design prose only.
    PostInitEditing,
    /// Checklist is live. A design `.md`, if present, is frozen.
    Active,
}

impl PlanModeState {
    /// Either Editing sub-state.
    pub fn is_editing(&self) -> bool {
        matches!(
            self,
            PlanModeState::PreInitEditing | PlanModeState::PostInitEditing
        )
    }
}

/// Threshold for treating a pre-init marker as stale (#1109).
///
/// When plan creation fails mid-flight (provider timeout, network error), the
/// marker file is left behind with no cleanup path. Without staleness, the
/// session is locked out of plan operations indefinitely (6.6 hours observed
/// in Adi's audit). Five minutes is generous for a plan-init round-trip;
/// anything older is a failed attempt, not an in-flight one.
pub const PRE_INIT_STALE_THRESHOLD: Duration = Duration::from_secs(300);

/// Whether the pre-init marker at `path` is older than [`PRE_INIT_STALE_THRESHOLD`].
///
/// Returns `true` when the marker should be treated as stale (session is not
/// actually pre-init, the previous attempt failed). Returns `false` for a
/// fresh marker or when the mtime cannot be read (conservative: treat as
/// fresh rather than silently clearing an in-flight marker).
fn is_marker_stale(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(age) = modified.elapsed() else {
        return false;
    };
    age > PRE_INIT_STALE_THRESHOLD
}

/// Derive the session's plan-mode state from disk.
pub async fn plan_mode_state(session_id: Uuid) -> PlanModeState {
    let json = plan_json_read_path(session_id).await;
    // load_plan_from_path returns None for a missing/unreadable/terminal file
    // (Completed archives, Cancelled deletes), which maps to NoPlan below.
    let plan = load_plan_from_path(&json);
    // The `.md` sits next to whichever `.json` we actually found, so derive
    // it from that path rather than re-resolving (handles the legacy dir).
    let md_exists = md_path_for(&json).exists();
    let state = plan_mode_state_of(plan.as_ref(), md_exists);
    // Pre-init is a durable MARKER file, not a stored PlanDocument (#569). When
    // no real plan resolves, an existing marker means Plan-mode intent without
    // an approvable document yet. Stale markers (>5 min old) are treated as
    // failed attempts and map to NoPlan instead of locking the session (#1109).
    if state == PlanModeState::NoPlan {
        let marker_path = pre_init_marker_read_path(session_id).await;
        if marker_path.exists() {
            if is_marker_stale(&marker_path) {
                tracing::warn!(
                    "Plan: clearing stale pre-init marker at {} (>{:?} old, #1109)",
                    marker_path.display(),
                    PRE_INIT_STALE_THRESHOLD
                );
                let _ = std::fs::remove_file(&marker_path);
                return PlanModeState::NoPlan;
            }
            return PlanModeState::PreInitEditing;
        }
    }
    state
}

/// Map an already-loaded (and legacy-normalized) plan plus `.md` existence
/// onto the lifecycle state. Split out so sync surfaces (the TUI overlay,
/// which already holds the loaded `PlanDocument`) derive the same state
/// without re-reading or re-resolving through the async path helpers.
pub fn plan_mode_state_of(plan: Option<&PlanDocument>, md_exists: bool) -> PlanModeState {
    let Some(plan) = plan else {
        return PlanModeState::NoPlan;
    };
    match plan.status {
        PlanStatus::Active => PlanModeState::Active,
        PlanStatus::Editing if plan.pre_init_editing && !md_exists => PlanModeState::PreInitEditing,
        // Explicit approval-queue marker (#1145): derived from the flag, not
        // the design `.md` existing — checklist plans have no `.md`.
        PlanStatus::Editing if plan.pending_approval => PlanModeState::PostInitEditing,
        // Legacy Editing + `.md` (design track, or plans predating #1145):
        // the `.md`'s existence was the implicit marker.
        PlanStatus::Editing if md_exists => PlanModeState::PostInitEditing,
        // Editing without an .md or a pre-init flag is a legacy draft (the
        // old seven-status world had no design track). load_plan normalizes
        // drafts with tasks to Active; an empty legacy draft gates nothing.
        PlanStatus::Editing => PlanModeState::NoPlan,
    }
}

/// Load the session plan, applying the legacy lifecycle rules:
///
/// - legacy `Completed` → silently archive both files, return `None`
/// - legacy `Cancelled` → delete both files, return `None`
/// - legacy draft-shaped checklists (Editing after the status map, tasks
///   non-empty, no `.md`, no pre-init flag) are normalized to `Active` in
///   memory: they were executable in the old world and stay executable.
/// - anything else parses through [`PlanStatus`]'s legacy string map.
pub async fn load_plan(session_id: Uuid) -> Option<PlanDocument> {
    load_plan_from_path(&plan_json_read_path(session_id).await)
}

/// Maximum plan file size (10MB): guards every consumer of the loader.
pub const MAX_PLAN_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// [`load_plan`] for callers that already hold the JSON path (TUI).
/// #20: whether plan approval REQUIRES an explicit human signal. Reads
/// `[agent] plan_require_approval` fresh from disk each call (hot-reload
/// friendly, same pattern as the isolation flag in `plan_tool`). Defaults
/// TRUE — the tool auto-approve policy never satisfies the plan approval
/// gate unless the operator explicitly opts out. Shared by the plan tool's
/// init arms and the resume demotion below.
pub(crate) fn plan_require_approval_enabled() -> bool {
    crate::config::Config::load()
        .map(|c| c.agent.plan_require_approval)
        .unwrap_or(true)
}

pub fn load_plan_from_path(path: &Path) -> Option<PlanDocument> {
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > MAX_PLAN_FILE_SIZE
    {
        tracing::warn!(
            "Plan file too large ({} bytes) at {}; refusing to load",
            meta.len(),
            path.display()
        );
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    let raw: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Unreadable plan JSON at {}: {e}", path.display());
            return None;
        }
    };

    // Terminal legacy statuses end the plan's life at the file level.
    let raw_status = raw.get("status").and_then(|s| s.as_str()).unwrap_or("");
    match raw_status {
        "Completed" => {
            if let Err(e) = archive_plan_files(path) {
                tracing::warn!("Failed to archive completed plan: {e}");
            }
            return None;
        }
        "Cancelled" => {
            remove_plan_files(path);
            return None;
        }
        _ => {}
    }

    let mut plan: PlanDocument = match serde_json::from_value(raw) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to parse plan JSON at {}: {e}", path.display());
            return None;
        }
    };

    // Legacy checklist normalization: an old Draft/PendingApproval plan with
    // tasks was executable before the design/checklist split and must not be
    // trapped in Editing (there is no .md to approve).
    if plan.status == PlanStatus::Editing
        && !plan.pre_init_editing
        && !plan.pending_approval
        && !plan.tasks.is_empty()
        && !md_path_for(path).exists()
    {
        plan.status = PlanStatus::Active;
    }

    // #20: re-assert the approval gate on resume. Plans auto-activated
    // under the escape hatch (`[agent] plan_require_approval = false`)
    // are stamped `ApprovalSource::Auto`; if the gate is on at load time
    // (the default), such Active plans demote back to the approval queue
    // — a restart must resume the approval WAIT, not the execution of a
    // plan no human approved under the current policy.
    //
    // GRANDFATHERED: Active plans stamped by pre-fix binaries
    // (`approved_at` set, no `approval_source`) stay Active — they are
    // indistinguishable on disk from a genuine human approval, and
    // demoting them would stall legitimately approved work. Active plans
    // with no `approved_at` (legacy normalization above) predate the
    // stamp itself and pass as well.
    if plan.status == PlanStatus::Active
        && plan.approval_source == Some(ApprovalSource::Auto)
        && plan_require_approval_enabled()
    {
        tracing::warn!(
            "Plan {} at {}: Active via the auto escape-hatch stamp while \
             plan_require_approval is on — demoting to Editing pending \
             explicit approval (#20)",
            plan.id,
            path.display()
        );
        plan.status = PlanStatus::Editing;
        plan.approved_at = None;
        plan.approval_source = None;
        plan.pending_approval = true;
        // Persist the demotion so a crash before the next save cannot
        // resurrect the auto-activated Active state (atomic tmp + rename,
        // same shape as save_plan).
        if let Ok(json) = serde_json::to_string_pretty(&plan) {
            let tmp = path.with_extension("tmp");
            if let Err(e) = std::fs::write(&tmp, &json).and_then(|_| std::fs::rename(&tmp, path)) {
                tracing::warn!(
                    "Failed to persist plan approval demotion at {}: {e}",
                    path.display()
                );
            }
        }
    }

    Some(plan)
}

/// Save the plan atomically (temp file + rename), writing the canonical
/// `"Editing"` / `"Active"` status strings.
pub async fn save_plan(plan: &PlanDocument) -> std::io::Result<()> {
    let dir = session_dir(plan.session_id).await;
    std::fs::create_dir_all(&dir)?;
    let path = plan_json_path(plan.session_id).await;
    let json = serde_json::to_string_pretty(plan)
        .map_err(|e| std::io::Error::other(format!("serialize plan: {e}")))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    // A real plan supersedes any pre-init marker: `plan init` writing the first
    // JSON clears the intent bit so a stale marker can't later resurrect
    // pre-init after the plan is archived (#569).
    let marker = dir.join(format!(".opencrabs_plan_{}.preinit", plan.session_id));
    if marker.exists()
        && let Err(e) = std::fs::remove_file(&marker)
    {
        tracing::warn!("Failed to clear pre-init marker {}: {e}", marker.display());
    }
    Ok(())
}

/// Mark the session as pre-init Editing: the user entered Plan-mode intent
/// but `plan init` has not succeeded yet. Durable (survives restart) via a
/// marker file — no fake `PlanDocument`, no approvable content (#569).
/// Refused (Err) when a real plan is already live.
///
/// This is the keyword-nudge entry point; it never downgrades a flag that
/// an explicit `/plan` slash already armed (see `PreInitOrigin`).
pub async fn set_pre_init_editing(session_id: Uuid) -> std::io::Result<()> {
    set_pre_init_editing_with_origin(session_id, PreInitOrigin::Nudge).await
}

/// Where the pre-init flag came from. Only an explicit `/plan` slash arms
/// the yolo design-review gate: a keyword soft-nudge (or a legacy empty
/// marker) keeps the rush behavior under tool auto-approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreInitOrigin {
    /// The user typed `/plan` (TUI or channel) — the deliberate brake.
    Slash,
    /// Plan-keyword soft-nudge, legacy empty marker, or anything else.
    Nudge,
}

/// `set_pre_init_editing` with an explicit origin. Re-arming is allowed
/// while no real plan is live: a `/plan` slash upgrades a nudge flag to
/// slash-origin, and a nudge never downgrades a slash-armed flag.
pub async fn set_pre_init_editing_with_origin(
    session_id: Uuid,
    origin: PreInitOrigin,
) -> std::io::Result<()> {
    match plan_mode_state(session_id).await {
        PlanModeState::PostInitEditing | PlanModeState::Active => {
            return Err(std::io::Error::other(
                "a plan is already live for this session",
            ));
        }
        PlanModeState::NoPlan | PlanModeState::PreInitEditing => {}
    }
    if origin == PreInitOrigin::Nudge
        && matches!(pre_init_origin(session_id).await, PreInitOrigin::Slash)
    {
        return Ok(());
    }
    let dir = session_dir(session_id).await;
    std::fs::create_dir_all(&dir)?;
    let content: &[u8] = match origin {
        PreInitOrigin::Slash => b"slash",
        PreInitOrigin::Nudge => b"",
    };
    std::fs::write(
        dir.join(format!(".opencrabs_plan_{session_id}.preinit")),
        content,
    )
}

/// The origin of the durable pre-init flag. Missing, empty, or unknown
/// marker content maps to `Nudge` — conservative: no review gate without
/// an explicit `/plan` slash.
pub async fn pre_init_origin(session_id: Uuid) -> PreInitOrigin {
    let dir = session_dir(session_id).await;
    let marker = dir.join(format!(".opencrabs_plan_{session_id}.preinit"));
    match std::fs::read(&marker) {
        Ok(bytes) if bytes == b"slash" => PreInitOrigin::Slash,
        _ => PreInitOrigin::Nudge,
    }
}

/// Whether the durable pre-init Editing flag is set for the session.
pub async fn is_pre_init_editing(session_id: Uuid) -> bool {
    plan_mode_state(session_id).await == PlanModeState::PreInitEditing
}

/// Archive the session's plan artifacts (`.json` and `.md`) under
/// `archive/` with a timestamp, returning the session to NoPlan.
pub async fn archive_plan(session_id: Uuid) -> std::io::Result<()> {
    archive_plan_files(&plan_json_read_path(session_id).await)?;
    // Explicit "completed THIS settle" stamp, durable in the archive dir so a
    // flood-delayed settle (potentially minutes late) — or one that lands on
    // a detached-resume path driving stream_loop instead of the handler's own
    // settle gate — still observes that the completion happened now (#1231).
    mark_plan_just_archived(session_id).await;
    Ok(())
}

/// Path of the durable "just archived" sidecar for `session_id`.
async fn plan_just_archived_path(session_id: Uuid) -> std::path::PathBuf {
    archive_dir(session_id)
        .await
        .join(format!("just_archived_{session_id}.flag"))
}

/// Stamp that this session's plan was archived on THIS settle.
///
/// Durable file, not an in-memory flag, so it survives a flood-throttled
/// settle that lands minutes later AND a detached-resume settle that runs
/// `stream_loop.rs`/`resume.rs` (which construct fresh state and drive
/// `refresh_plan_card` directly, bypassing the handler settle gate). The old
/// mtime recency guess (`recent_archived_plan`, 120s) could not handle that:
/// a flood `retry_after` of 25–32s plus queued sends pushed completion
/// settlement past the window, the gate missed, and the archived card was
/// deleted instead of finalized.
pub async fn mark_plan_just_archived(session_id: Uuid) {
    let path = plan_just_archived_path(session_id).await;
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        tracing::warn!("plan just-archived flag mkdir failed: {e}");
        return;
    }
    if let Err(e) = std::fs::write(&path, "1") {
        tracing::warn!("plan just-archived flag write failed: {e}");
    }
}

/// Consume the "just archived" stamp for `session_id`, one-shot.
///
/// Returns `true` exactly once for the settle that archived the plan; every
/// later call returns `false` (and removes the stale sidecar). The single
/// consumer (the settle gate, or the `refresh_plan_card` no-live-plan re-stick
/// on the resume path) is the one that finalizes — the one-shot guard stops
/// both from finalizing the same completion into duplicate cards.
pub async fn take_plan_just_archived(session_id: Uuid) -> bool {
    let path = plan_just_archived_path(session_id).await;
    if path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::debug!("plan just-archived flag remove failed: {e}");
        }
        true
    } else {
        false
    }
}

/// Peek the "just archived" stamp WITHOUT consuming it (#62).
///
/// The settle restick gate re-checks every settle (per-turn restick), so a
/// consuming read there would starve every later turn. Gate sites peek; the
/// one-shot consumer that finalizes the completed card still calls
/// [`take_plan_just_archived`].
pub async fn peek_plan_just_archived(session_id: Uuid) -> bool {
    plan_just_archived_path(session_id).await.exists()
}

/// True when the newest file under this session's `archive/` was written
/// within `max_age` (#1158). tool_loop archives a plan at EVERY settling
/// plan-turn, so "an archive exists" cannot distinguish completion-now from
/// completion-hours-ago; callers needing "the settle that just happened
/// archived it" must gate on this recency window instead.
/// Dir-level core of [`recent_archived_plan`], split out so tests can point
/// it at a temp dir instead of the session's real archive location (#1158).
pub(crate) fn recent_archive_in_dir(dir: &std::path::Path, max_age: std::time::Duration) -> bool {
    std::fs::read_dir(dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .filter_map(|m| m.modified().ok())
                .filter_map(|t| t.elapsed().ok())
                .min()
                .map(|age| age <= max_age)
        })
        .unwrap_or(false)
}

pub async fn recent_archived_plan(session_id: Uuid, max_age: std::time::Duration) -> bool {
    let dir = archive_dir(session_id).await;
    recent_archive_in_dir(&dir, max_age)
}

/// Canonical archive stem for the plan living at `json_path` (#16 round 2).
///
/// `archive_plan_files` renames into `{stem}-{ts}.json` and
/// `latest_archived_plan_from_path` filters archive names by this same stem
/// prefix — BOTH must go through this one helper. The 2026-08-29 incident:
/// the writer trimmed the leading dot off `.opencrabs_plan_<sid>` while the
/// reader kept it, so the reader's `starts_with` prefix never matched any
/// archive on disk, every completed-plan finalize hit its no-doc branch, and
/// no completion card was ever posted again (zero successful finalizes
/// between c57de25c and this fix). One stem, one place, no drift.
fn plan_archive_stem(json_path: &Path) -> String {
    json_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("plan")
        .trim_start_matches('.')
        .to_string()
}

fn archive_plan_files(json_path: &Path) -> std::io::Result<()> {
    // Archive next to wherever the plan actually lives (resolved or legacy
    // dir), so this stays sync and path-based for the loader's terminal-status
    // path, which holds a path but no session id.
    let dir = json_path
        .parent()
        .map(|p| p.join("archive"))
        .unwrap_or_else(|| PathBuf::from("archive"));
    std::fs::create_dir_all(&dir)?;
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let stem = plan_archive_stem(json_path);
    if json_path.exists() {
        std::fs::rename(json_path, dir.join(format!("{stem}-{ts}.json")))?;
    }
    let md = md_path_for(json_path);
    if md.exists() {
        std::fs::rename(&md, dir.join(format!("{stem}-{ts}.md")))?;
    }
    // Defensive: a real plan's save already cleared any pre-init marker, but if
    // one somehow lingered, drop it so archiving can't leave the session
    // looking pre-init (#569).
    let marker = pre_init_marker_for(json_path);
    if marker.exists()
        && let Err(e) = std::fs::remove_file(&marker)
    {
        tracing::warn!("Failed to remove pre-init marker {}: {e}", marker.display());
    }
    Ok(())
}

/// Load the most recently archived plan sitting beside `json_path` (#810).
///
/// A completed plan archives at turn settle and vanishes from every surface,
/// so the checklist the user just watched finish disappears at the moment it
/// finishes. This reads it back so the final all-complete state can stay on
/// screen until the next plan starts.
///
/// Reads from disk rather than process memory, so the view survives a restart
/// exactly as plan state itself does. Archive names end in `-%Y%m%d-%H%M%S`,
/// which is lexicographically ordered, so the greatest name is the newest and
/// no timestamp parsing is needed.
///
/// Path-based and synchronous to match `archive_plan_files`, so the TUI's
/// synchronous reload can call it.
///
/// Scopes to THIS session's entries (#1239): every channel session shares one
/// flat `archive/` dir on this box, so a lexicographic max across ALL entries
/// picks whichever session id sorts last — observed live, session B's
/// completion rendered session A's checklist card.
pub fn latest_archived_plan_from_path(json_path: &Path) -> Option<PlanDocument> {
    let dir = json_path.parent()?.join("archive");
    // Only this session's own archives qualify (#1239). archive_plan_files
    // renames into `{stem}-{ts}.json`, and the stem comes from the SAME
    // `plan_archive_stem` helper the writer uses — the live file's stem
    // prefixes precisely its session's entries no matter which layout dir
    // holds them. (#16 round 2: computing this stem separately at the two
    // sites let a leading-dot trim drift between them — writer trimmed,
    // reader kept the dot, `starts_with` never matched again.)
    let prefix = format!("{}-", plan_archive_stem(json_path));
    let newest = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(&prefix))
        })
        .max_by_key(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        })?;
    read_archived_plan(&newest)
}

/// Parse an archived plan without the live loader's side effects.
///
/// `load_plan_from_path` acts on terminal statuses: `Completed` archives the
/// file and returns `None`, `Cancelled` deletes it. Correct for a live plan,
/// destructive for one already in `archive/` — it would move the file deeper on
/// every read and return nothing, so the finished checklist could never render.
///
/// Reading history must never mutate it, so this only parses.
fn read_archived_plan(path: &Path) -> Option<PlanDocument> {
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > MAX_PLAN_FILE_SIZE
    {
        tracing::warn!(
            "Archived plan too large ({} bytes) at {}; refusing to load",
            meta.len(),
            path.display()
        );
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&content) {
        Ok(plan) => Some(plan),
        Err(e) => {
            tracing::warn!("Unreadable archived plan at {}: {e}", path.display());
            None
        }
    }
}

/// The most recently archived plan for `session_id` (#809, #810).
pub async fn latest_archived_plan(session_id: Uuid) -> Option<PlanDocument> {
    latest_archived_plan_from_path(&plan_json_read_path(session_id).await)
}

/// Delete the session's plan artifacts (or clear the pre-init sidecar),
/// returning the session to NoPlan. The engine half of Discard; command
/// wiring is the UX layer's.
pub async fn discard_plan(session_id: Uuid) {
    remove_plan_files(&plan_json_read_path(session_id).await);
}

fn remove_plan_files(json_path: &Path) {
    if json_path.exists()
        && let Err(e) = std::fs::remove_file(json_path)
    {
        tracing::warn!("Failed to remove plan JSON {}: {e}", json_path.display());
    }
    let md = md_path_for(json_path);
    if md.exists()
        && let Err(e) = std::fs::remove_file(&md)
    {
        tracing::warn!("Failed to remove plan markdown {}: {e}", md.display());
    }
    // Pre-init discard: there is no JSON/`.md`, only the marker — clear it so
    // the session returns to NoPlan (#569).
    let marker = pre_init_marker_for(json_path);
    if marker.exists()
        && let Err(e) = std::fs::remove_file(&marker)
    {
        tracing::warn!("Failed to remove pre-init marker {}: {e}", marker.display());
    }
}

fn md_path_for(json_path: &Path) -> PathBuf {
    json_path.with_extension("md")
}

/// Create the session design `.md` with the light template B scaffold.
/// The model fills the sections with natural language; only the headings,
/// context labels, and step numbering are structural.
pub async fn create_design_md(session_id: Uuid, title: &str) -> std::io::Result<PathBuf> {
    let dir = session_dir(session_id).await;
    std::fs::create_dir_all(&dir)?;
    let path = plan_md_path(session_id).await;
    std::fs::write(&path, design_scaffold(title))?;
    Ok(path)
}

/// The light-template-B scaffold body for a design `.md`.
///
/// Shared with the write guard: when a refused draft has no accepted mirror
/// to restore (a fresh plan's `description` is empty), this is the pre-write
/// state the file goes back to. Refusal must never mean "wipe".
pub fn design_scaffold(title: &str) -> String {
    format!(
        "# {title}\n\n\
         ## Context\n\
         - **Problem:** \n\
         - **Target state:** \n\
         - **Intent:** \n\n\
         ## Implementation steps\n\
         1. \n   - Done when: \n"
    )
}

/// Sync the session `.md` body into the plan JSON `description` (the
/// Editing mirror). Tasks are never touched: Editing cannot persist a
/// checklist. Malformed template bodies are refused and restored from the
/// previous mirror, so callers can return the validator's exact guidance to
/// the writing model.
pub async fn sync_md_to_json(session_id: Uuid) -> Result<(), String> {
    // Read the `.md` and `.json` from the same active location (resolved or
    // legacy), so an in-place edit is mirrored regardless of which dir the
    // plan currently lives in.
    let json = plan_json_read_path(session_id).await;
    let md = md_path_for(&json);
    let Ok(body) = std::fs::read_to_string(&md) else {
        return Ok(());
    };
    let Some(mut plan) = load_plan_from_path(&json) else {
        return Ok(());
    };
    if plan.status != PlanStatus::Editing {
        return Ok(());
    }

    let warnings = template_section_warnings(&body);
    if !warnings.is_empty() {
        // The generic write tool has already updated the .md by the time the
        // registry reaches this mirror. Restore the pre-write state so a
        // refused write has no observable persistence side effect: the last
        // accepted body, or the scaffold when nothing has been mirrored yet.
        // A fresh plan's description is empty; restoring it would delete the
        // draft AND the scaffold the guard's own contract depends on.
        let restore = if plan.description.trim().is_empty() {
            design_scaffold(&plan.title)
        } else {
            plan.description.clone()
        };
        if let Err(e) = std::fs::write(&md, restore) {
            tracing::warn!("Failed to restore refused plan .md write: {e}");
        }
        return Err(format!(
            "PLAN TEMPLATE WRITE REFUSED: {}\n\nPlan template contract: each `**Label:**` must be a single line: label + space + text",
            warnings.join("; ")
        ));
    }

    #[cfg(feature = "telegram")]
    if crate::channels::telegram::rich::mermaid::should_render_mermaid(&body) {
        let parse_errors =
            crate::channels::telegram::rich::mermaid::preflight_parse_errors(&body).await;
        if !parse_errors.is_empty() {
            if let Err(e) = std::fs::write(&md, &plan.description) {
                tracing::warn!("Failed to restore refused plan .md write: {e}");
            }
            return Err(format!(
                "PLAN WRITE REFUSED: Mermaid diagram syntax error in plan markdown.\n\n\
                 Renderer diagnostic:\n{}\n\n\
                 Please fix the Mermaid diagram syntax in the plan design and try again.",
                parse_errors.join("\n")
            ));
        }
    }

    plan.description = body;
    plan.updated_at = chrono::Utc::now();
    if let Err(e) = save_plan(&plan).await {
        tracing::warn!("Failed to mirror plan .md into JSON description: {e}");
    }
    Ok(())
}

/// Advisory light-template-B checks for the design `.md`: `## Context`
/// (with Problem / Target state / Intent) and at least one numbered
/// `## Implementation steps` entry are required before Approve.
pub fn template_section_warnings(md: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    if !md.contains("## Context") {
        warnings.push("missing required `## Context` section".to_string());
    }
    for label in ["**Problem:**", "**Target state:**", "**Intent:**"] {
        let filled = md.lines().any(|l| {
            l.split_once(label)
                .is_some_and(|(_, rest)| !rest.trim().is_empty())
        });
        if !filled {
            warnings.push(format!("`{label}` needs non-empty text after the label"));
        }
    }
    if !md.contains("## Implementation steps") {
        warnings.push("missing required `## Implementation steps` section".to_string());
    } else {
        let has_step = md.lines().any(|l| {
            let t = l.trim_start();
            let rest = t.trim_start_matches(|c: char| c.is_ascii_digit());
            t.starts_with(|c: char| c.is_ascii_digit())
                && rest.starts_with('.')
                && !rest.trim_start_matches('.').trim().is_empty()
        });
        if !has_step {
            warnings.push("`## Implementation steps` needs at least one numbered step".to_string());
        }
    }
    warnings
}
