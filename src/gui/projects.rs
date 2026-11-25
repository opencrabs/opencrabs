//! State and service reads for the window's projects screen.
//!
//! Every read and write is the crab's (`ProjectList`, `ProjectCommand`), run on
//! the backend's 4 MB worker lane with its own current-thread Tokio runtime.
//! The window only receives owned snapshots, so rendering never touches SQLite
//! and a closed screen can reject a late result by generation.

use opencrabs::db::models::{Project, Session};
use opencrabs::services::ProjectService;
use std::collections::BTreeSet;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectsMode {
    #[default]
    List,
    Detail(Uuid),
    /// Pick an unassigned session for the contained project id.
    Assign(Uuid),
    /// Pick a project for the contained top-level session id.
    AssignSession(Uuid),
}

/// The screen's rows, actions and snapshot are the crab's types: the reads
/// and writes behind them run as `ProjectList` and `ProjectCommand`.
pub use opencrabs::backend::projects::{ProjectAction, ProjectRow, ProjectsData};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssignmentFilter {
    #[default]
    All,
    Unassigned,
    Project(Uuid),
}

#[derive(Debug, Clone)]
pub struct ProjectsState {
    pub mode: ProjectsMode,
    pub rows: Vec<ProjectRow>,
    pub sessions: Vec<Session>,
    pub unassigned: Vec<Session>,
    pub assignable: Vec<Session>,
    pub assignment_filter: AssignmentFilter,
    pub selected_sessions: BTreeSet<Uuid>,
    pub archive_confirm: bool,
    pub selected: usize,
    pub session_selected: usize,
    pub creating: bool,
    pub create_input: String,
    pub delete_confirm: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub generation: u64,
    pub directory: PathBuf,
}

impl Default for ProjectsState {
    fn default() -> Self {
        Self {
            mode: ProjectsMode::List,
            rows: Vec::new(),
            sessions: Vec::new(),
            unassigned: Vec::new(),
            assignable: Vec::new(),
            assignment_filter: AssignmentFilter::All,
            selected_sessions: BTreeSet::new(),
            archive_confirm: false,
            selected: 0,
            session_selected: 0,
            creating: false,
            create_input: String::new(),
            delete_confirm: false,
            loading: false,
            error: None,
            notice: None,
            generation: 0,
            directory: ProjectService::projects_dir(),
        }
    }
}

impl ProjectsMode {
    pub fn project_id(self) -> Option<Uuid> {
        match self {
            ProjectsMode::List | ProjectsMode::AssignSession(_) => None,
            ProjectsMode::Detail(id) | ProjectsMode::Assign(id) => Some(id),
        }
    }
}

impl ProjectsState {
    pub fn project(&self, id: Uuid) -> Option<&Project> {
        self.rows
            .iter()
            .find(|row| row.project.id == id)
            .map(|row| &row.project)
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.rows.get(self.selected).map(|row| &row.project)
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.get(self.session_selected)
    }

    pub fn selected_assignment(&self, target: Uuid) -> Option<Session> {
        self.visible_assignments(target)
            .into_iter()
            .nth(self.session_selected)
    }

    pub fn toggle_assignment(&mut self, target: Uuid, session_id: Uuid) {
        if !self
            .visible_assignments(target)
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }
        if !self.selected_sessions.insert(session_id) {
            self.selected_sessions.remove(&session_id);
        }
    }

    pub fn visible_assignments(&self, target: Uuid) -> Vec<Session> {
        self.assignable
            .iter()
            .filter(|session| session.project_id != Some(target))
            .filter(|session| match self.assignment_filter {
                AssignmentFilter::All => true,
                AssignmentFilter::Unassigned => session.project_id.is_none(),
                AssignmentFilter::Project(id) => session.project_id == Some(id),
            })
            .cloned()
            .collect()
    }

    pub fn move_selection(&mut self, delta: i64) {
        let len = match self.mode {
            ProjectsMode::List | ProjectsMode::AssignSession(_) => self.rows.len(),
            ProjectsMode::Detail(_) => self.sessions.len(),
            ProjectsMode::Assign(id) => self.visible_assignments(id).len(),
        };
        if len == 0 {
            self.selected = 0;
            self.session_selected = 0;
            return;
        }
        let index = match self.mode {
            ProjectsMode::List | ProjectsMode::AssignSession(_) => &mut self.selected,
            ProjectsMode::Detail(_) | ProjectsMode::Assign(_) => &mut self.session_selected,
        };
        *index = (*index as i64 + delta).clamp(0, len as i64 - 1) as usize;
    }

    pub fn clamp(&mut self) {
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        let session_len = match self.mode {
            ProjectsMode::Assign(id) => self.visible_assignments(id).len(),
            _ => self.sessions.len(),
        };
        self.session_selected = self.session_selected.min(session_len.saturating_sub(1));
    }

    pub fn apply(&mut self, data: ProjectsData) {
        self.rows = data.rows;
        self.sessions = data
            .detail
            .map(|(_, sessions)| sessions)
            .unwrap_or_default();
        self.unassigned = data.unassigned;
        self.assignable = data.assignable;
        self.notice = data.notice;
        self.clamp();
    }

    pub fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }
}

/// Run one read or write and return a complete snapshot for the screen.
pub async fn run(
    backend: crate::backend_glue::Backend,
    action: ProjectAction,
) -> Result<ProjectsData, String> {
    // A refresh only reads, so it asks with the read scope; a delete needs
    // the admin scope; every other action writes and is a command.
    let result = match action {
        ProjectAction::Refresh { detail } => {
            backend
                .call(opencrabs::backend::projects::ProjectList { detail })
                .await
        }
        ProjectAction::Delete { id } => {
            backend
                .call(opencrabs::backend::projects::ProjectDelete { id })
                .await
        }
        action => {
            backend
                .call(opencrabs::backend::projects::ProjectCommand { action })
                .await
        }
    }
    .map_err(opencrabs::backend::BackendError::into_message);
    if let Err(ref error) = result {
        crate::logging::error(&format!("projects service: {error}"));
    }
    result
}
