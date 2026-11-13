//! Project Repository
//!
//! Database operations for projects and project-session assignments.

use crate::db::Pool;
use crate::db::database::interact_err;
use crate::db::models::{Project, Session};
use anyhow::{Context, Result};
use rusqlite::params;
use uuid::Uuid;

/// Repository for project operations
#[derive(Clone)]
pub struct ProjectRepository {
    pool: Pool,
}

impl ProjectRepository {
    /// Create a new project repository
    pub fn new(pool: Pool) -> Self {
        Self { pool }
    }

    /// Find project by ID
    pub async fn find_by_id(&self, id: Uuid) -> Result<Option<Project>> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare_cached("SELECT * FROM projects WHERE id = ?1")?
                    .query_row(params![id_str], Project::from_row)
                    .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find project")
    }

    /// Find project by name (exact match)
    pub async fn find_by_name(&self, name: &str) -> Result<Option<Project>> {
        let n = name.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.prepare_cached("SELECT * FROM projects WHERE name = ?1")?
                    .query_row(params![n], Project::from_row)
                    .optional()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find project by name")
    }

    /// List all projects (most recently updated first)
    pub async fn list_all(&self) -> Result<Vec<Project>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt =
                    conn.prepare_cached("SELECT * FROM projects ORDER BY updated_at DESC")?;
                let rows = stmt.query_map([], Project::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to list projects")
    }

    /// Create a new project
    pub async fn create(&self, project: &Project) -> Result<()> {
        let p = project.clone();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO projects (id, name, description, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        p.id.to_string(),
                        p.name,
                        p.description,
                        p.created_at.timestamp(),
                        p.updated_at.timestamp(),
                    ],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to create project")?;

        tracing::debug!("Created project: {} ({})", project.name, project.id);
        Ok(())
    }

    /// Update an existing project
    pub async fn update(&self, project: &Project) -> Result<()> {
        let p = project.clone();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE projects SET name = ?1, description = ?2, updated_at = ?3
                     WHERE id = ?4",
                    params![
                        p.name,
                        p.description,
                        p.updated_at.timestamp(),
                        p.id.to_string(),
                    ],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to update project")?;

        tracing::debug!("Updated project: {}", project.id);
        Ok(())
    }

    /// Adopt the first remote ever recorded for a project, #1510.
    ///
    /// Writes only when the column is still NULL: once a project's identity is
    /// established, a later session cannot redefine it. Deliberately does not
    /// stamp `updated_at` — the project listing sorts by it and derived
    /// metadata must not float old projects to the top (the #1460 lesson).
    /// Returns whether this call was the one that recorded it.
    pub async fn set_repo_remote_if_none(&self, id: Uuid, remote: &str) -> Result<bool> {
        let id_str = id.to_string();
        let r = remote.to_string();
        let changed = self
            .pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE projects SET repo_remote = ?2 WHERE id = ?1 AND repo_remote IS NULL",
                    params![id_str, r],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to adopt project repo_remote")?;
        Ok(changed > 0)
    }

    /// Delete a project (sessions get project_id set to NULL via FK ON DELETE SET NULL)
    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let id_str = id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute("DELETE FROM projects WHERE id = ?1", params![id_str])
            })
            .await
            .map_err(interact_err)?
            .context("Failed to delete project")?;

        tracing::debug!("Deleted project: {}", id);
        Ok(())
    }

    /// Assign a session to a project.
    ///
    /// Membership is derived metadata, not conversation activity: the write
    /// deliberately leaves `updated_at` alone. Stamping here restamped every
    /// session the startup backfill linked, floating months-dead tombstones
    /// above genuinely recent sessions in `/sessions` (#1460) — the same
    /// lesson the scope-all model writes learned in #1367.
    pub async fn assign_session(&self, session_id: Uuid, project_id: Uuid) -> Result<()> {
        let sid = session_id.to_string();
        let pid = project_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET project_id = ?1
                     WHERE id = ?2",
                    params![pid, sid],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to assign session to project")?;

        tracing::debug!("Assigned session {} to project {}", session_id, project_id);
        Ok(())
    }

    /// Remove a session from its project (set project_id to NULL).
    /// Unassignment is metadata bookkeeping, not activity — `updated_at`
    /// is never stamped, mirroring `assign_session` (#1460).
    pub async fn unassign_session(&self, session_id: Uuid) -> Result<()> {
        let sid = session_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET project_id = NULL
                     WHERE id = ?1",
                    params![sid],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to unassign session from project")?;

        tracing::debug!("Unassigned session {} from project", session_id);
        Ok(())
    }

    /// List sessions belonging to a project (most recently updated first)
    pub async fn find_sessions_by_project(&self, project_id: Uuid) -> Result<Vec<Session>> {
        let pid = project_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM sessions WHERE project_id = ?1 AND archived_at IS NULL
                     ORDER BY updated_at DESC",
                )?;
                let rows = stmt.query_map(params![pid], Session::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find sessions by project")
    }

    /// List top-level sessions not assigned to any project, newest first.
    /// Topics belong to their parent session and are never independent picks.
    pub async fn find_unassigned_sessions(&self) -> Result<Vec<Session>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM sessions
                     WHERE project_id IS NULL AND archived_at IS NULL AND parent_id IS NULL
                     ORDER BY updated_at DESC",
                )?;
                let rows = stmt.query_map([], Session::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find unassigned sessions")
    }

    /// List every active top-level session for project assignment and repair.
    /// Topics inherit their parent's project and are never independent picks.
    pub async fn find_assignable_sessions(&self) -> Result<Vec<Session>> {
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT * FROM sessions
                     WHERE archived_at IS NULL AND parent_id IS NULL
                     ORDER BY updated_at DESC",
                )?;
                let rows = stmt.query_map([], Session::from_row)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .await
            .map_err(interact_err)?
            .context("Failed to find assignable sessions")
    }

    /// Count sessions in a project
    pub async fn count_sessions(&self, project_id: Uuid) -> Result<i64> {
        let pid = project_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM sessions WHERE project_id = ?1 AND archived_at IS NULL",
                    params![pid],
                    |row| row.get(0),
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to count project sessions")
    }

    /// Count files tracked across all sessions in a project
    pub async fn count_files(&self, project_id: Uuid) -> Result<i64> {
        let pid = project_id.to_string();
        self.pool
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM files f
                     JOIN sessions s ON f.session_id = s.id
                     WHERE s.project_id = ?1",
                    params![pid],
                    |row| row.get(0),
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to count project files")
    }
}

/// Extension trait for rusqlite to add `.optional()` to query results
trait OptionalExt<T> {
    fn optional(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn optional(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
