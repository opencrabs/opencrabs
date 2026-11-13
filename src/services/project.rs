//! Project Service
//!
//! Business logic for project management: CRUD, session assignment,
//! projects directory, and stats.

use crate::db::models::{Project, Session};
use crate::db::repository::ProjectRepository;
use crate::services::ServiceContext;
use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

/// Service for managing projects
#[derive(Clone)]
pub struct ProjectService {
    context: ServiceContext,
}

/// Stats for a project
#[derive(Debug, Clone)]
pub struct ProjectStats {
    pub session_count: i64,
    pub file_count: i64,
}

impl ProjectService {
    /// Create a new project service
    pub fn new(context: ServiceContext) -> Self {
        Self { context }
    }

    /// Get the projects directory path — profile-aware: `~/.opencrabs/projects`
    /// for the default, `~/.opencrabs/profiles/<name>/projects` under `-p`.
    /// Uses `opencrabs_home()` (not the root `base_opencrabs_dir`) so a
    /// profile's project files live entirely under that profile.
    pub fn projects_dir() -> std::path::PathBuf {
        crate::config::opencrabs_home().join("projects")
    }

    /// Ensure the projects directory exists on disk
    pub fn ensure_projects_dir() -> Result<std::path::PathBuf> {
        let dir = Self::projects_dir();
        if !dir.exists() {
            std::fs::create_dir_all(&dir).with_context(|| {
                format!("Failed to create projects directory: {}", dir.display())
            })?;
            tracing::info!("Created projects directory: {}", dir.display());
        }
        Ok(dir)
    }

    /// `~/.opencrabs/projects/<slug>/` (the project ROOT, not `/files`) for the
    /// session's project, plus the project name for labelling. `None` when the
    /// session has no project, the project row is gone, or the lookup fails.
    ///
    /// This is the hook point for per-project brain overlays: a project may
    /// carry its own `AGENTS.md`, `SOUL.md`, etc. that load ON TOP of the
    /// profile's brain. Read-only resolution, the directory is NOT created here
    /// (overlays are optional; absence just means "no overlay").
    pub async fn project_brain_dir(
        &self,
        session_id: Uuid,
    ) -> Option<(String, std::path::PathBuf)> {
        use crate::db::repository::SessionRepository;
        let session = SessionRepository::new(self.context.pool())
            .find_by_id(session_id)
            .await
            .ok()??;
        let project_id = session.project_id?;
        let project = ProjectRepository::new(self.context.pool())
            .find_by_id(project_id)
            .await
            .ok()??;
        let dir =
            Self::projects_dir().join(crate::services::file::slugify_project_name(&project.name));
        Some((project.name, dir))
    }

    /// Create a new project
    pub async fn create_project(
        &self,
        name: String,
        description: Option<String>,
    ) -> Result<Project> {
        // Ensure projects dir exists
        Self::ensure_projects_dir()?;

        let repo = ProjectRepository::new(self.context.pool());

        // Check for duplicate name
        if let Some(existing) = repo.find_by_name(&name).await? {
            anyhow::bail!(
                "Project '{}' already exists (id: {})",
                existing.name,
                existing.id
            );
        }

        let project = Project::new(name, description);
        repo.create(&project).await?;
        Ok(project)
    }

    /// Get a project by ID
    pub async fn get_project(&self, id: Uuid) -> Result<Option<Project>> {
        let repo = ProjectRepository::new(self.context.pool());
        repo.find_by_id(id).await
    }

    /// Get a project by ID, returning error if not found
    pub async fn get_project_required(&self, id: Uuid) -> Result<Project> {
        self.get_project(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Project not found: {}", id))
    }

    /// List all projects
    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        let repo = ProjectRepository::new(self.context.pool());
        repo.list_all().await
    }

    /// Update a project
    pub async fn update_project(&self, project: &Project) -> Result<()> {
        let mut updated = project.clone();
        updated.updated_at = Utc::now();

        let repo = ProjectRepository::new(self.context.pool());
        repo.update(&updated).await
    }

    /// Rename a project
    pub async fn rename_project(&self, id: Uuid, new_name: String) -> Result<Project> {
        let mut project = self.get_project_required(id).await?;
        project.name = new_name;
        project.updated_at = Utc::now();

        let repo = ProjectRepository::new(self.context.pool());
        repo.update(&project).await?;
        Ok(project)
    }

    /// Delete a project (sessions are unassigned via FK ON DELETE SET NULL)
    pub async fn delete_project(&self, id: Uuid) -> Result<()> {
        let repo = ProjectRepository::new(self.context.pool());
        repo.delete(id).await
    }

    /// Assign a session to a project
    pub async fn assign_session(&self, session_id: Uuid, project_id: Uuid) -> Result<()> {
        // Verify project exists
        self.get_project_required(project_id).await?;

        let repo = ProjectRepository::new(self.context.pool());
        repo.assign_session(session_id, project_id).await
    }

    /// Remove a session from its project
    pub async fn unassign_session(&self, session_id: Uuid) -> Result<()> {
        let repo = ProjectRepository::new(self.context.pool());
        repo.unassign_session(session_id).await
    }

    /// Record a project's repository remote when it has none yet (#1510).
    /// Returns whether this call was the one that recorded it, so callers can
    /// tell an adoption from a no-op against a project that already has one.
    pub async fn set_project_repo_remote(&self, id: Uuid, remote: &str) -> Result<bool> {
        let repo = ProjectRepository::new(self.context.pool());
        repo.set_repo_remote_if_none(id, remote).await
    }

    /// Get all sessions in a project
    pub async fn get_sessions_for_project(&self, project_id: Uuid) -> Result<Vec<Session>> {
        let repo = ProjectRepository::new(self.context.pool());
        repo.find_sessions_by_project(project_id).await
    }

    /// Get sessions not assigned to any project
    pub async fn get_unassigned_sessions(&self) -> Result<Vec<Session>> {
        let repo = ProjectRepository::new(self.context.pool());
        repo.find_unassigned_sessions().await
    }

    /// Get every active top-level session, including sessions assigned to
    /// another project, so the assignment picker can repair wrong membership.
    pub async fn get_assignable_sessions(&self) -> Result<Vec<Session>> {
        let repo = ProjectRepository::new(self.context.pool());
        repo.find_assignable_sessions().await
    }

    /// Link a session to the project its working directory names, if it is not
    /// already linked and the directory matches one.
    ///
    /// Returns the project it linked to, or `None` when there was nothing to
    /// do: no working directory (channel sessions carry none), no match, or a
    /// session already assigned. Idempotent, so calling it twice is harmless
    /// and the backfill can be re-run.
    pub async fn link_session_by_directory(&self, session: &Session) -> Result<Option<Project>> {
        if session.project_id.is_some() {
            return Ok(None);
        }
        let Some(ref working_directory) = session.working_directory else {
            return Ok(None);
        };
        let projects = self.list_projects().await?;
        let identity =
            crate::services::project_match::resolve_directory_identity(working_directory);
        let Some(project) =
            crate::services::project_match::match_by_directory(&identity, &projects)
        else {
            return Ok(None);
        };
        self.assign_session(session.id, project.id).await?;
        let mut project = project.clone();
        // Adoption (#1510): the first session that proves which repository a
        // project is records its remote, so every later session can match on
        // it instead of the basename. Never overwrites a recorded remote.
        if project.repo_remote.is_none()
            && let Some(ref remote) = identity.remote
        {
            match self.set_project_repo_remote(project.id, remote).await {
                Ok(true) => {
                    project.repo_remote = Some(remote.clone());
                    tracing::info!(
                        "Adopted repo remote '{remote}' for project '{}'",
                        project.name
                    );
                }
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    error = %e,
                    "failed to adopt repo remote for project '{}'",
                    project.name
                ),
            }
        }
        tracing::info!(
            "Linked session {} to project '{}' from working directory {}",
            session.id,
            project.name,
            working_directory,
        );
        Ok(Some(project))
    }

    /// Link every unassigned session whose working directory names a project.
    ///
    /// Returns how many were linked. For sessions that predate the project
    /// they belong to, or that were created before anything linked them at all
    /// (#1445). Idempotent: an assigned session is never revisited, so a second
    /// run links only what the first could not.
    ///
    /// A single session failing to link does not abandon the rest. The failure
    /// is logged and the sweep continues, because the alternative is that one
    /// bad row keeps every later session unlinked forever.
    pub async fn backfill_unassigned_sessions(&self) -> Result<usize> {
        let sessions = self.get_unassigned_sessions().await?;
        let mut projects = self.list_projects().await?;
        if projects.is_empty() {
            return Ok(0);
        }
        // Two git subprocesses per directory is not free, and a real DB has
        // dozens of sessions sharing a handful of directories.
        let mut identities: std::collections::HashMap<
            String,
            crate::services::project_match::DirectoryIdentity,
        > = std::collections::HashMap::new();
        let mut linked = 0usize;
        for session in &sessions {
            let Some(ref working_directory) = session.working_directory else {
                continue;
            };
            let identity = identities
                .entry(working_directory.clone())
                .or_insert_with(|| {
                    crate::services::project_match::resolve_directory_identity(working_directory)
                })
                .clone();
            let Some(project) =
                crate::services::project_match::match_by_directory(&identity, &projects)
            else {
                continue;
            };
            let project_id = project.id;
            let project_name = project.name.clone();
            let adoption = project
                .repo_remote
                .is_none()
                .then(|| identity.remote.clone())
                .flatten();
            match self.assign_session(session.id, project_id).await {
                Ok(()) => linked += 1,
                Err(e) => tracing::warn!(
                    error = %e,
                    "backfill: could not link session {} to project '{}'",
                    session.id,
                    project_name,
                ),
            }
            // Mirror the adopted remote into the in-memory list: a later
            // session in the same sweep must see the collision guard armed.
            if let Some(remote) = adoption
                && let Ok(true) = self.set_project_repo_remote(project_id, &remote).await
            {
                if let Some(p) = projects.iter_mut().find(|p| p.id == project_id) {
                    p.repo_remote = Some(remote.clone());
                }
                tracing::info!("Adopted repo remote '{remote}' for project '{project_name}'");
            }
        }
        if linked > 0 {
            tracing::info!("Linked {linked} existing session(s) to projects by working directory");
        }
        Ok(linked)
    }

    /// Get stats for a project (session count, file count)
    pub async fn get_project_stats(&self, project_id: Uuid) -> Result<ProjectStats> {
        let repo = ProjectRepository::new(self.context.pool());
        let session_count = repo.count_sessions(project_id).await?;
        let file_count = repo.count_files(project_id).await?;
        Ok(ProjectStats {
            session_count,
            file_count,
        })
    }
}
