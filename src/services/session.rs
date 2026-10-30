//! Session Service
//!
//! Provides business logic for session management operations.

use crate::db::{
    models::Session,
    repository::{SessionListOptions, SessionRepository, UsageLedgerRepository},
};
use crate::services::ServiceContext;
use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

/// Service for managing sessions
#[derive(Clone)]
pub struct SessionService {
    context: ServiceContext,
}

impl SessionService {
    /// Create a new session service
    pub fn new(context: ServiceContext) -> Self {
        Self { context }
    }

    /// Access the underlying database pool
    pub fn pool(&self) -> crate::db::Pool {
        self.context.pool()
    }

    /// Create a new session
    pub async fn create_session(&self, title: Option<String>) -> Result<Session> {
        self.create_session_with_provider(title, None, None, None)
            .await
    }

    /// Create a new session with explicit provider and model
    pub async fn create_session_with_provider(
        &self,
        title: Option<String>,
        provider_name: Option<String>,
        model: Option<String>,
        working_directory: Option<String>,
    ) -> Result<Session> {
        let repo = SessionRepository::new(self.context.pool());

        let session = Session {
            id: Uuid::new_v4(),
            title,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            archived_at: None,
            model,
            provider_name,
            token_count: 0,
            total_cost: 0.0,
            working_directory,
            auto_title_attempted: false,
            project_id: None,
            channel_chat_key: None,
        };

        repo.create(&session)
            .await
            .context("Failed to create session")?;

        tracing::info!("Created new session: {}", session.id);
        Ok(session)
    }

    /// Atomically insert a channel session keyed on its stable `[chat:<id>]`
    /// identity (#1721). If another writer (another process sharing this
    /// database) already created a live session for the same chat key, that
    /// winner is returned so the caller's message routes into it instead of
    /// forking the chat into two parallel sessions.
    pub async fn insert_or_resolve_channel_session(
        &self,
        title: Option<String>,
        provider_name: Option<String>,
        model: Option<String>,
        working_directory: Option<String>,
        chat_key: &str,
    ) -> Result<Session> {
        self.insert_or_resolve_channel_session_reported(
            title,
            provider_name,
            model,
            working_directory,
            chat_key,
        )
        .await
        .map(|(session, _)| session)
    }

    /// Same as [`Self::insert_or_resolve_channel_session`], also reporting
    /// whether this call created the row (#2183). A caller that logs
    /// "created new channel session" needs to know which it was: the insert
    /// resolves an existing row for the same chat key, so the word "created"
    /// was printed on every message of a long-lived chat.
    pub async fn insert_or_resolve_channel_session_reported(
        &self,
        title: Option<String>,
        provider_name: Option<String>,
        model: Option<String>,
        working_directory: Option<String>,
        chat_key: &str,
    ) -> Result<(Session, bool)> {
        let repo = SessionRepository::new(self.context.pool());

        let session = Session {
            id: Uuid::new_v4(),
            title,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            archived_at: None,
            model,
            provider_name,
            token_count: 0,
            total_cost: 0.0,
            working_directory,
            auto_title_attempted: false,
            project_id: None,
            channel_chat_key: None,
        };

        let (result, created) = repo
            .insert_or_resolve_channel_reported(&session, chat_key)
            .await
            .context("Failed to create channel session")?;

        tracing::info!(
            "{} channel session: {}",
            if created {
                "Created new"
            } else {
                "Resolved existing"
            },
            result.id
        );
        Ok((result, created))
    }

    /// Get a session by ID
    pub async fn get_session(&self, id: Uuid) -> Result<Option<Session>> {
        let repo = SessionRepository::new(self.context.pool());
        repo.find_by_id(id).await.context("Failed to get session")
    }

    /// Get a session by ID, returning an error if not found
    pub async fn get_session_required(&self, id: Uuid) -> Result<Session> {
        self.get_session(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {}", id))
    }

    /// List all sessions
    pub async fn list_sessions(&self, options: SessionListOptions) -> Result<Vec<Session>> {
        let repo = SessionRepository::new(self.context.pool());
        repo.list(options).await.context("Failed to list sessions")
    }

    /// Update a session
    pub async fn update_session(&self, session: &Session) -> Result<()> {
        let repo = SessionRepository::new(self.context.pool());

        // Update the updated_at timestamp
        let mut updated_session = session.clone();
        updated_session.updated_at = Utc::now();

        repo.update(&updated_session)
            .await
            .context("Failed to update session")?;

        tracing::debug!("Updated session: {}", session.id);
        Ok(())
    }

    /// Update session title
    pub async fn update_session_title(&self, id: Uuid, title: Option<String>) -> Result<()> {
        let mut session = self.get_session_required(id).await?;
        session.title = title;
        session.updated_at = Utc::now();

        let repo = SessionRepository::new(self.context.pool());
        repo.update(&session)
            .await
            .context("Failed to update session title")?;

        tracing::info!("Updated session title: {}", id);
        Ok(())
    }

    /// Update session usage statistics and record to the cumulative usage ledger.
    /// The ledger persists even when sessions are deleted.
    ///
    /// `provider` and `model` must be the pair that ACTUALLY SERVED the request
    /// (#807), which is why they are passed in rather than read back off the
    /// session row. Re-deriving them from the row attributed cost to whatever
    /// pair happened to be stored, so a row left inconsistent by a sticky
    /// fallback or a partial switch (#705) minted a permanent ledger entry for
    /// a request that never happened, e.g. a Xiaomi row naming a GLM model. It
    /// also credited a fallback's work and spend to the primary that did not
    /// serve it. The ledger is append-only, so each such row is permanent.
    ///
    /// `cache_read_tokens` is the provider-reported cache-read count for the
    /// same turn. It rides along to the ledger so the By Model card can show
    /// the cache share of a model's tokens (#2027); `None` means the turn
    /// reported no cache usage, which is a different fact from a zero.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_session_usage(
        &self,
        id: Uuid,
        token_count: i64,
        cost: f64,
        provider: &str,
        model: &str,
        cache_read_tokens: Option<i64>,
        prompt_tokens: Option<i64>,
    ) -> Result<()> {
        // Atomic increment, not read-modify-write (#2180). The old shape loaded
        // the row, added in Rust and wrote the whole row back, so two turns
        // finishing on the same session at the same time lost one increment
        // with no error. Overlapping inbound turns are exactly when that
        // happens, which is why the loss went unnoticed for so long: the
        // counter just came out lower than the ledger said.
        let repo = SessionRepository::new(self.context.pool());
        repo.update_stats(id, token_count, cost)
            .await
            .context("Failed to update session usage")?;

        // Append to cumulative usage ledger (never deleted)
        let ledger = UsageLedgerRepository::new(self.context.pool());
        if let Err(e) = ledger
            .record(
                &id.to_string(),
                provider,
                model,
                token_count,
                cost,
                cache_read_tokens,
                prompt_tokens,
            )
            .await
        {
            tracing::warn!("Failed to record usage to ledger: {}", e);
        }

        tracing::debug!(
            "Updated session usage: {} (+{} tokens, +${:.4})",
            id,
            token_count,
            cost
        );
        Ok(())
    }

    /// Update session working directory
    pub async fn update_session_working_directory(
        &self,
        id: Uuid,
        dir: Option<String>,
    ) -> Result<()> {
        use crate::db::interact_err;
        use rusqlite::params;

        let id_str = id.to_string();
        let now = Utc::now().timestamp();
        self.context
            .pool()
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET working_directory = ?1, updated_at = ?2 WHERE id = ?3",
                    params![dir, now, id_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to update session working directory")?;
        Ok(())
    }

    /// Mark that auto-title generation has been attempted for this session.
    /// Prevents re-triggering auto-title on subsequent messages.
    pub async fn mark_auto_title_attempted(&self, id: Uuid) -> Result<()> {
        use crate::db::interact_err;
        use rusqlite::params;

        let id_str = id.to_string();
        self.context
            .pool()
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET auto_title_attempted = 1 WHERE id = ?1",
                    params![id_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to mark auto_title_attempted")?;
        Ok(())
    }

    /// Reset the auto-title attempt flag so the next user message can
    /// re-trigger auto-title generation. Called from the background task
    /// when the title-generation LLM call failed — without this the
    /// session would stay stuck on its default channel-generated title
    /// forever, because the attempt flag is set BEFORE the LLM call to
    /// prevent race conditions. Issue #118.
    pub async fn reset_auto_title_attempted(&self, id: Uuid) -> Result<()> {
        use crate::db::interact_err;
        use rusqlite::params;

        let id_str = id.to_string();
        self.context
            .pool()
            .get()
            .await
            .context("Failed to get connection")?
            .interact(move |conn| {
                conn.execute(
                    "UPDATE sessions SET auto_title_attempted = 0 WHERE id = ?1",
                    params![id_str],
                )
            })
            .await
            .map_err(interact_err)?
            .context("Failed to reset auto_title_attempted")?;
        Ok(())
    }

    /// Archive a session
    pub async fn archive_session(&self, id: Uuid) -> Result<()> {
        let repo = SessionRepository::new(self.context.pool());
        repo.archive(id)
            .await
            .context("Failed to archive session")?;

        tracing::info!("Archived session: {}", id);
        Ok(())
    }

    /// Unarchive a session
    pub async fn unarchive_session(&self, id: Uuid) -> Result<()> {
        let repo = SessionRepository::new(self.context.pool());
        repo.unarchive(id)
            .await
            .context("Failed to unarchive session")?;

        tracing::info!("Unarchived session: {}", id);
        Ok(())
    }

    /// Delete a session permanently
    pub async fn delete_session(&self, id: Uuid) -> Result<()> {
        let repo = SessionRepository::new(self.context.pool());
        repo.delete(id).await.context("Failed to delete session")?;

        tracing::info!("Deleted session: {}", id);
        Ok(())
    }

    /// Write `provider/model` to every non-archived session (#468 scope-all,
    /// shared semantics with the CLI --all path): rows already on the pair
    /// are skipped, archived rows are never touched. Returns how many rows
    /// changed. Implemented as one bulk UPDATE (#1367) so `updated_at` is
    /// never stamped — recency ordering and title-suffix lookups survive.
    /// Live sessions apply the pair on their next message via the
    /// existing sync path.
    /// Sessions an apply-to-all for this pair would change, for the confirmation prompt (#2042).
    pub async fn count_provider_model_all_sessions(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<usize> {
        SessionRepository::new(self.context.pool())
            .count_provider_model_for_all(provider.to_string(), model.to_string())
            .await
            .context("Failed to count sessions for apply-to-all")
    }

    pub async fn set_provider_model_all_sessions(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<usize> {
        let repo = SessionRepository::new(self.context.pool());
        let updated = repo
            .set_provider_model_for_all(provider.to_string(), model.to_string())
            .await
            .context("Failed to bulk-set provider/model on sessions")?;
        if updated > 0 {
            tracing::info!(
                updated,
                provider,
                model,
                "scope-all (#468): wrote pair, updated_at preserved (#1367)"
            );
        }
        Ok(updated)
    }

    /// Find most recent non-archived session by exact title (used for persistent channel sessions).
    pub async fn find_session_by_title(&self, title: &str) -> Result<Option<Session>> {
        let repo = SessionRepository::new(self.context.pool());
        repo.find_by_title(title).await
    }

    /// Find the most recent non-archived session whose title ends with
    /// `suffix`. Channel handlers embed a stable platform id
    /// (e.g. `[chat:12345]`) as the title suffix on creation so a
    /// rename of the user-visible label still resolves to the same
    /// session row.
    /// The live session carrying this stable `[chat:<id>]` key (#2183). The
    /// key is the unique identity, so this is a direct lookup; the title
    /// suffix is only a proxy for it and misses an auto-renamed row.
    pub async fn find_session_by_channel_chat_key(&self, key: &str) -> Result<Option<Session>> {
        let repo = SessionRepository::new(self.context.pool());
        repo.find_by_channel_chat_key(key).await
    }

    pub async fn find_session_by_title_suffix(&self, suffix: &str) -> Result<Option<Session>> {
        let repo = SessionRepository::new(self.context.pool());
        repo.find_by_title_suffix(suffix).await
    }

    /// Give up a session's claim on its chat key without archiving it (#2186).
    pub async fn release_channel_chat_key(&self, id: Uuid) -> Result<()> {
        let repo = SessionRepository::new(self.context.pool());
        repo.release_channel_chat_key(id)
            .await
            .context("Failed to release session chat key")
    }

    /// Get the most recent active session
    pub async fn get_most_recent_session(&self) -> Result<Option<Session>> {
        let repo = SessionRepository::new(self.context.pool());
        let options = SessionListOptions {
            include_archived: false,
            limit: Some(1),
            offset: 0,
            query: None,
            include_subagents: false,
        };

        let sessions = repo.list(options).await?;
        Ok(sessions.into_iter().next())
    }

    /// Count total sessions (excluding archived)
    pub async fn count_sessions(&self) -> Result<i64> {
        let repo = SessionRepository::new(self.context.pool());
        repo.count(false).await.context("Failed to count sessions")
    }

    /// Count archived sessions
    pub async fn count_archived_sessions(&self) -> Result<i64> {
        let repo = SessionRepository::new(self.context.pool());
        repo.count(true)
            .await
            .context("Failed to count archived sessions")
    }

    /// Delete sub-agent sessions untouched for longer than `ttl_days`, along
    /// with every row keyed to them and their on-disk plan files. Returns how
    /// many were removed.
    ///
    /// `ttl_days == 0` disables the sweep. Sub-agent sessions are created one
    /// per `spawn_agent` and never revisited, so without this they accumulate
    /// indefinitely (#931).
    ///
    /// Plan files resolve through the session's project, so each session's
    /// paths are read BEFORE its row is deleted — afterwards the lookup would
    /// silently fall back to the wrong directory and leave the files behind.
    pub async fn prune_expired_subagent_sessions(&self, ttl_days: u32) -> Result<usize> {
        if ttl_days == 0 {
            return Ok(0);
        }
        let cutoff = Utc::now().timestamp() - (ttl_days as i64) * 86_400;
        let repo = SessionRepository::new(self.context.pool());
        let expired = repo
            .subagent_sessions_older_than(cutoff)
            .await
            .context("Failed to list expired sub-agent sessions")?;
        if expired.is_empty() {
            return Ok(0);
        }

        let mut pruned = 0usize;
        for id in expired {
            // Resolve first, delete second — see the note above.
            let archive = crate::utils::plan_files::archive_dir(id).await;
            crate::utils::plan_files::discard_plan(id).await;
            if archive.exists()
                && let Err(e) = std::fs::remove_dir_all(&archive)
            {
                // Not fatal: the session row still goes, and a stale archive
                // directory is inert. Say so rather than dropping it silently.
                tracing::warn!(
                    "Failed to remove archived plans for sub-agent session {id} at {}: {e}",
                    archive.display()
                );
            }
            match repo.purge(id).await {
                Ok(()) => pruned += 1,
                Err(e) => tracing::warn!("Failed to purge sub-agent session {id}: {e:#}"),
            }
        }
        if pruned > 0 {
            tracing::info!("Pruned {pruned} sub-agent session(s) older than {ttl_days} day(s)");
        }
        Ok(pruned)
    }
}
