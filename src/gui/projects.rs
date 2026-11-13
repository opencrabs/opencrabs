//! State and service reads for the window's projects screen.
//!
//! Every database call runs on a named 4 MB thread with its own current-thread
//! Tokio runtime. The window only receives owned snapshots, so rendering never
//! touches SQLite and a closed screen can reject a late result by generation.

use opencrabs::db::Database;
use opencrabs::db::models::{Project, Session};
use opencrabs::services::{ProjectService, ServiceContext};
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

#[derive(Debug, Clone)]
pub struct ProjectRow {
    pub project: Project,
    pub sessions: i64,
    pub files: i64,
}

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

#[derive(Debug, Clone)]
pub enum ProjectAction {
    Refresh { detail: Option<Uuid> },
    Create { name: String },
    Delete { id: Uuid },
    Assign { project_id: Uuid, session_id: Uuid },
    Unassign { project_id: Uuid, session_id: Uuid },
}

#[derive(Debug, Clone)]
pub struct ProjectsData {
    pub rows: Vec<ProjectRow>,
    pub detail: Option<(Project, Vec<Session>)>,
    pub unassigned: Vec<Session>,
    pub assignable: Vec<Session>,
    pub notice: Option<String>,
}

async fn execute(db_path: PathBuf, action: ProjectAction) -> anyhow::Result<ProjectsData> {
    let db = Database::connect(&db_path).await?;
    let service = ProjectService::new(ServiceContext::new(db.pool().clone()));
    let mut detail_id = None;
    let notice = match action {
        ProjectAction::Refresh { detail } => {
            detail_id = detail;
            None
        }
        ProjectAction::Create { name } => {
            let name = name.trim().to_string();
            anyhow::ensure!(!name.is_empty(), "Project name cannot be empty");
            let project = service.create_project(name, None).await?;
            Some(format!("Project '{}' created", project.name))
        }
        ProjectAction::Delete { id } => {
            service.delete_project(id).await?;
            Some("Project deleted; its sessions are now unassigned".to_string())
        }
        ProjectAction::Assign {
            project_id,
            session_id,
        } => {
            service.assign_session(session_id, project_id).await?;
            detail_id = Some(project_id);
            Some("Session assigned to project".to_string())
        }
        ProjectAction::Unassign {
            project_id,
            session_id,
        } => {
            service.unassign_session(session_id).await?;
            detail_id = Some(project_id);
            Some("Session removed from project".to_string())
        }
    };

    let projects = service.list_projects().await?;
    let mut rows = Vec::with_capacity(projects.len());
    for project in projects {
        let stats = service.get_project_stats(project.id).await?;
        rows.push(ProjectRow {
            project,
            sessions: stats.session_count,
            files: stats.file_count,
        });
    }
    let detail = match detail_id {
        Some(id) => Some((
            service.get_project_required(id).await?,
            service.get_sessions_for_project(id).await?,
        )),
        None => None,
    };
    let unassigned = service.get_unassigned_sessions().await?;
    let assignable = service.get_assignable_sessions().await?;
    Ok(ProjectsData {
        rows,
        detail,
        unassigned,
        assignable,
        notice,
    })
}

/// Run one read or write and return a complete snapshot for the screen.
pub async fn run(db_path: PathBuf, action: ProjectAction) -> Result<ProjectsData, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .name("gui-projects".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let message = format!("projects runtime: {error}");
                    crate::logging::error(&message);
                    if tx.send(Err(message)).is_err() {
                        crate::logging::error("projects runtime receiver dropped");
                    }
                    return;
                }
            };
            let result = runtime
                .block_on(execute(db_path, action))
                .map_err(|error| format!("{error:#}"));
            if let Err(ref error) = result {
                crate::logging::error(&format!("projects service: {error}"));
            }
            if tx.send(result).is_err() {
                crate::logging::error("projects service receiver dropped");
            }
        });
    match spawned {
        Ok(_) => rx
            .await
            .unwrap_or_else(|_| Err("projects worker dropped its result".to_string())),
        Err(error) => {
            let message = format!("projects thread: {error}");
            crate::logging::error(&message);
            Err(message)
        }
    }
}
