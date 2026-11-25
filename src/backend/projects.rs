//! Project commands.
//!
//! The projects screen reads and writes through two operations: a list
//! (`ProjectList`, read) and a command (`ProjectCommand`, manage). Both answer
//! the same complete snapshot, so the screen redraws from one value whatever
//! it asked for.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::models::{Project, Session};
use crate::db::repository::SessionRepository;
use crate::services::{ProjectService, ServiceContext};

use super::{BackendError, Ctx, Op, Scope};

/// One row of the projects list: the project and its counts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRow {
    pub project: Project,
    pub sessions: i64,
    pub files: i64,
}

/// What the projects screen asks of the crab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProjectAction {
    Refresh {
        detail: Option<Uuid>,
    },
    Create {
        name: String,
    },
    Delete {
        id: Uuid,
    },
    Assign {
        project_id: Uuid,
        session_id: Uuid,
    },
    Unassign {
        project_id: Uuid,
        session_id: Uuid,
    },
    AssignMany {
        project_id: Uuid,
        session_ids: Vec<Uuid>,
    },
    ClearMany {
        project_id: Uuid,
        session_ids: Vec<Uuid>,
    },
    ArchiveMany {
        project_id: Uuid,
        session_ids: Vec<Uuid>,
    },
}

/// The complete snapshot the screen draws after any action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectsData {
    pub rows: Vec<ProjectRow>,
    pub detail: Option<(Project, Vec<Session>)>,
    pub unassigned: Vec<Session>,
    pub assignable: Vec<Session>,
    pub notice: Option<String>,
}

/// Run one action against the crab's ledger and read the snapshot back.
async fn execute(ctx: &Ctx, action: ProjectAction) -> anyhow::Result<ProjectsData> {
    let db = ctx.database().await?;
    let service = ProjectService::new(ServiceContext::new(db.pool().clone()));
    let sessions = SessionRepository::new(db.pool().clone());
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
        ProjectAction::AssignMany {
            project_id,
            session_ids,
        } => {
            service.get_project_required(project_id).await?;
            for session_id in &session_ids {
                service.assign_session(*session_id, project_id).await?;
            }
            detail_id = Some(project_id);
            Some(format!(
                "{} sessions assigned to project",
                session_ids.len()
            ))
        }
        ProjectAction::ClearMany {
            project_id,
            session_ids,
        } => {
            for session_id in &session_ids {
                service.unassign_session(*session_id).await?;
            }
            detail_id = Some(project_id);
            Some(format!(
                "{} sessions removed from their projects",
                session_ids.len()
            ))
        }
        ProjectAction::ArchiveMany {
            project_id,
            session_ids,
        } => {
            for session_id in &session_ids {
                sessions.archive(*session_id).await?;
            }
            detail_id = Some(project_id);
            Some(format!("{} sessions archived", session_ids.len()))
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

/// Read the projects snapshot, with one project's sessions when `detail`
/// names it. A failure is its `{e:#}` text, the line the screen shows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectList {
    /// The project whose sessions the detail view lists.
    pub detail: Option<Uuid>,
}

impl Op for ProjectList {
    const NAME: &'static str = "projects.list";
    const SCOPE: Scope = Scope::Read;
    type Reply = ProjectsData;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        execute(
            ctx,
            ProjectAction::Refresh {
                detail: self.detail,
            },
        )
        .await
        .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

/// Run one project command (create, delete, assign, clear, archive) and
/// answer the snapshot after it. A failure is its `{e:#}` text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCommand {
    pub action: ProjectAction,
}

impl Op for ProjectCommand {
    const NAME: &'static str = "projects.command";
    const SCOPE: Scope = Scope::Manage;
    type Reply = ProjectsData;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        execute(ctx, self.action)
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

ops! { ProjectList, ProjectCommand }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};

    async fn seed(gateway: &Gateway) -> Project {
        let db = gateway.ctx.db().await.expect("db");
        let service = ProjectService::new(ServiceContext::new(db.pool().clone()));
        gateway
            .within(service.create_project("crab".into(), None))
            .await
            .expect("create a project")
    }

    #[tokio::test]
    async fn project_list_has_parity() {
        let gateway = Gateway::start().await;
        let project = seed(&gateway).await;
        let answer = parity!(
            gateway,
            ProjectList {
                detail: Some(project.id),
            }
        );
        let rows = answer["Ok"]["rows"].as_array().expect("a list of rows");
        assert_eq!(rows.len(), 1, "{answer}");
        assert_eq!(answer["Ok"]["notice"], serde_json::Value::Null);
        assert!(answer["Ok"]["detail"].is_array(), "{answer}");
        forbidden!(gateway, ProjectList { detail: None });
    }

    #[tokio::test]
    async fn project_command_has_parity() {
        let gateway = Gateway::start().await;
        let project = seed(&gateway).await;
        // Clearing nobody is the command both doors can repeat and still
        // answer alike; it reads the whole snapshot back all the same.
        let answer = parity!(
            gateway,
            ProjectCommand {
                action: ProjectAction::ClearMany {
                    project_id: project.id,
                    session_ids: Vec::new(),
                },
            }
        );
        assert_eq!(
            answer["Ok"]["notice"],
            serde_json::json!("0 sessions removed from their projects")
        );

        forbidden!(
            gateway,
            ProjectCommand {
                action: ProjectAction::Delete { id: project.id },
            }
        );
        let answer = parity!(gateway, ProjectList { detail: None });
        assert_eq!(
            answer["Ok"]["rows"].as_array().map(Vec::len),
            Some(1),
            "a refused delete removed nothing"
        );
    }

    #[tokio::test]
    async fn an_empty_name_is_refused_with_its_own_text() {
        let gateway = Gateway::start().await;
        let answer = parity!(
            gateway,
            ProjectCommand {
                action: ProjectAction::Create { name: "  ".into() },
            }
        );
        assert_eq!(
            answer,
            serde_json::json!({ "Err": { "kind": "failed", "detail": "Project name cannot be empty" } })
        );
    }
}
