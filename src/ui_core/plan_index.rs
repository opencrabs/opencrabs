//! What a session's plan says that a session row draws (#2636, #2737).
//!
//! The GUI rail and the TUI session list answer the same two questions per
//! session: does a plan wait for the owner, and how far along is it. The plan
//! file resolution itself lives once in [`crate::utils::plan_files::PlanIndex`]
//! (#2715); this module adds the two paint answers on top of it, so neither
//! surface can word them differently.
//!
//! Nothing here draws. The index is file I/O plus the loader's own state rules,
//! which is why it can live in `ui_core` and be linked by a terminal frontend.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

use crate::tui::plan::{PlanDocument, PlanStatus, TaskStatus};
use crate::utils::plan_files::plan_state;

pub use crate::utils::plan_files::{PLAN_PREFIX, PLAN_SUFFIX, SWEEP_EVERY};

/// How long a plan file counts as live evidence after its own last write.
pub const FRESH_WINDOW: Duration = Duration::from_secs(30 * 60);

/// What a session's plan file says that a row draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanEvidence {
    /// Written and waiting for the owner: the row rings.
    pub ready: bool,
    /// Approved, with a task in flight: the row spins.
    pub running: bool,
    /// The file's own mtime, which is what "this plan" means for the ring's
    /// seen key: a new plan in the same session writes a new stamp, so the
    /// ring comes back for it even though the session id did not change.
    pub stamp: SystemTime,
}

/// The two paint answers both surfaces ask of the shared plan index.
///
/// Resolution, caching and the filter classification come from
/// [`crate::utils::plan_files::PlanIndex`]; what lives here is the decision a
/// row draws, which has to match between the rail card and the terminal row.
#[derive(Default)]
pub struct PlanIndex {
    inner: crate::utils::plan_files::PlanIndex,
}

impl PlanIndex {
    /// An index over explicit roots. Tests and alternate profiles use it;
    /// production resolves the profile's own directories through
    /// [`PlanIndex::default`].
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            inner: crate::utils::plan_files::PlanIndex::with_roots(roots),
        }
    }

    /// What a row should draw for `session`, or `None` when its plan says
    /// nothing worth animating.
    ///
    /// `live` is whether a turn for that session is running in this process.
    /// It outranks the freshness window: a build can hold a plan file still for
    /// longer than the window, and the work would then read as abandoned.
    pub fn evidence(&mut self, session: Uuid, live: bool, now: Instant) -> Option<PlanEvidence> {
        let (path, stamp) = self.inner.resolve(session, now)?;
        let doc = self.inner.doc_at(&path, stamp)?;
        match plan_state(doc, &path)? {
            // A design draft or a seeded checklist is written and waits for
            // the owner: the row rings until it is opened. Nothing executes
            // yet, so no spinner.
            PlanStatus::Editing => Some(PlanEvidence {
                ready: true,
                running: false,
                stamp,
            }),
            PlanStatus::Active => {
                let running_task = doc
                    .tasks
                    .iter()
                    .any(|t| matches!(t.status, TaskStatus::InProgress));
                if !running_task {
                    return None;
                }
                // Measured on this box: 26 of 27 plans with a task in flight
                // were abandoned, the oldest 100 days old. A badge lit from the
                // file alone would animate for months-old corpses, so the file
                // has to be recent or the turn has to be live here.
                let fresh = SystemTime::now()
                    .duration_since(stamp)
                    .is_ok_and(|age| age < FRESH_WINDOW);
                (live || fresh).then_some(PlanEvidence {
                    ready: false,
                    running: true,
                    stamp,
                })
            }
        }
    }

    /// The plan's checklist progress for a session row: `(done, total)`.
    ///
    /// Only an approved plan with tasks has a bar to draw: a design draft has
    /// no checklist yet, and an empty one would read as zero of nothing. The
    /// counts come from the strip's own rule, so the card and the composer
    /// strip agree about the same plan. No freshness gate here: the bar is
    /// state, not animation, and 4 of 8 done stays true while the badge sleeps.
    pub fn progress(&mut self, session: Uuid, now: Instant) -> Option<(usize, usize)> {
        let (path, stamp) = self.inner.resolve(session, now)?;
        let doc = self.inner.doc_at(&path, stamp)?;
        if !matches!(plan_state(doc, &path)?, PlanStatus::Active) || doc.tasks.is_empty() {
            return None;
        }
        Some(progress_counts(doc))
    }
}

/// How many tasks are finished, out of how many.
pub fn progress_counts(plan: &PlanDocument) -> (usize, usize) {
    let total = plan.tasks.len();
    let done = plan
        .tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::Completed | TaskStatus::Skipped))
        .count();
    (done, total)
}

/// The percentage and the ten-cell bar for a done/total pair.
///
/// Every surface that draws a plan bar goes through this, so the card, the
/// strip and the terminal row cannot show three different shapes for one plan.
pub fn progress_bar(done: usize, total: usize) -> (usize, String) {
    let percent = (done * 100).checked_div(total).unwrap_or(0);
    let filled = (done * 10).checked_div(total).unwrap_or(0);
    let filled = filled.min(10);
    (
        percent,
        format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// Write a plan file the way the plan tool does, and hand back its path.
    fn seed_plan(
        dir: &Path,
        session: Uuid,
        status: PlanStatus,
        task: Option<TaskStatus>,
        pending: bool,
    ) -> PathBuf {
        use crate::tui::plan::{PlanTask, TaskType};
        let mut doc = PlanDocument::new(session, "plan index test".to_string());
        doc.status = status;
        doc.pending_approval = pending;
        if let Some(task) = task {
            let mut t = PlanTask::new(1, "the task".to_string(), String::new(), TaskType::Edit);
            t.status = task;
            doc.tasks.push(t);
        }
        let path = dir.join(format!("{PLAN_PREFIX}{session}{PLAN_SUFFIX}"));
        std::fs::write(&path, serde_json::to_string(&doc).expect("plan json")).expect("write plan");
        path
    }

    #[test]
    fn plan_index_finds_a_plan_by_its_session_id() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(10);
        seed_plan(
            tmp.path(),
            session,
            PlanStatus::Active,
            Some(TaskStatus::InProgress),
            false,
        );
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        let evidence = index
            .evidence(session, false, Instant::now())
            .expect("a fresh running plan is evidence");
        assert!(evidence.running);
        assert!(!evidence.ready);
        assert!(index.evidence(sid(11), false, Instant::now()).is_none());
    }

    #[test]
    fn plan_index_an_editing_plan_is_ready_until_it_is_opened() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(12);
        seed_plan(
            tmp.path(),
            session,
            PlanStatus::Editing,
            Some(TaskStatus::Pending),
            true,
        );
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        let evidence = index
            .evidence(session, false, Instant::now())
            .expect("a plan waiting for approval is evidence");
        assert!(evidence.ready);
        assert!(!evidence.running);
    }

    #[test]
    fn plan_index_a_pre_init_flag_is_not_a_plan() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(13);
        let path = tmp
            .path()
            .join(format!("{PLAN_PREFIX}{session}{PLAN_SUFFIX}"));
        // What `/plan` writes before `plan init` lands: an intent flag, no
        // tasks, no design doc.
        let mut doc = PlanDocument::new(session, String::new());
        doc.pre_init_editing = true;
        std::fs::write(&path, serde_json::to_string(&doc).expect("json")).expect("write");
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        assert!(
            index.evidence(session, false, Instant::now()).is_none(),
            "an intent flag must not mark the row"
        );
    }

    #[test]
    fn plan_index_progress_reads_only_approved_plans_with_tasks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session = sid(16);
        let mut doc = PlanDocument::new(session, "progress".to_string());
        doc.status = PlanStatus::Active;
        doc.tasks = vec![
            crate::tui::plan::PlanTask::new(
                1,
                "one".to_string(),
                String::new(),
                crate::tui::plan::TaskType::Edit,
            ),
            crate::tui::plan::PlanTask::new(
                2,
                "two".to_string(),
                String::new(),
                crate::tui::plan::TaskType::Edit,
            ),
        ];
        doc.tasks[0].status = TaskStatus::Completed;
        let path = tmp
            .path()
            .join(format!("{PLAN_PREFIX}{session}{PLAN_SUFFIX}"));
        std::fs::write(&path, serde_json::to_string(&doc).expect("json")).expect("write");
        let mut index = PlanIndex::with_roots(vec![tmp.path().to_path_buf()]);
        assert_eq!(index.progress(session, Instant::now()), Some((1, 2)));

        // A design draft has no checklist, so it draws no bar.
        doc.status = PlanStatus::Editing;
        doc.tasks.clear();
        std::fs::write(&path, serde_json::to_string(&doc).expect("json")).expect("write");
        assert_eq!(index.progress(session, Instant::now()), None);
    }

    #[test]
    fn plan_bar_is_ten_cells_and_clamps() {
        assert_eq!(progress_bar(0, 4).1, "░░░░░░░░░░");
        assert_eq!(progress_bar(4, 4).1, "██████████");
        assert_eq!(progress_bar(2, 4).0, 50);
        assert_eq!(progress_bar(3, 4).1, "███████░░░");
        // A total of zero never panics and never divides by zero.
        assert_eq!(progress_bar(0, 0), (0, "░░░░░░░░░░".to_string()));
    }
}
