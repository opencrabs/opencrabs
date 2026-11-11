//! Message Service
//!
//! Provides business logic for message management operations.

use crate::db::{models::Message, repository::MessageRepository};
use crate::services::ServiceContext;
use crate::tui::plan::PlanDocument;
use crate::utils::completion_sound::announcement_pending;
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const PLAN_COMPLETED_ROLE: &str = "plan_completed";
/// A turn the user stopped before it answered (#2811).
///
/// Its own role, because the alternative is a lie: a cancelled turn writes no
/// error row, so every surface that asks "did this turn end badly?" answered
/// "no" and the card settled as Finished. The context builder skips this role,
/// so the row never reaches the model.
pub const TURN_STOPPED_ROLE: &str = "turn_stopped";
const PLAN_COMPLETED_PAYLOAD_VERSION: u8 = 1;

/// Versioned, structured history for a plan that reached its completion seam.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedPlanPayload {
    pub version: u8,
    pub plan: PlanDocument,
}

impl CompletedPlanPayload {
    /// Decode only a current, fully resolved payload.
    pub fn from_json(content: &str) -> Option<Self> {
        let payload: Self = serde_json::from_str(content).ok()?;
        (payload.version == PLAN_COMPLETED_PAYLOAD_VERSION
            && announcement_pending(Some(&payload.plan)).is_some())
        .then_some(payload)
    }

    /// Decode only the current payload shape on the dedicated history role.
    pub fn from_message(message: &Message) -> Option<Self> {
        (message.role == PLAN_COMPLETED_ROLE)
            .then(|| Self::from_json(&message.content))
            .flatten()
    }
}

fn validate_existing_completed_plan(message: Message, plan: &PlanDocument) -> Result<Message> {
    let payload = CompletedPlanPayload::from_message(&message)
        .context("Existing completed-plan row has an invalid payload")?;
    if message.session_id != plan.session_id || payload.plan.id != plan.id {
        bail!("Deterministic completed-plan ID belongs to a different plan");
    }
    Ok(message)
}

/// Everything recorded on an assistant row once its turn ends.
///
/// Grouped rather than passed as six positional arguments: the list had grown
/// past the point where a caller could get the order right by reading the call
/// site, and `duration_secs` (#964) would have been the eighth.
#[derive(Debug, Clone, Copy, Default)]
pub struct MessageUsage {
    pub token_count: i64,
    pub cost: f64,
    /// Server-reported prompt tokens for the request that produced the row.
    pub input_tokens: Option<i64>,
    pub cache_creation_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    /// Wall-clock seconds the turn took, from a monotonic clock (#964).
    pub duration_secs: Option<i64>,
}

/// Service for managing messages
#[derive(Clone)]
pub struct MessageService {
    context: ServiceContext,
}

impl MessageService {
    /// Create a new message service
    pub fn new(context: ServiceContext) -> Self {
        Self { context }
    }

    /// Persist the final checklist once, before the live plan is archived.
    pub async fn record_completed_plan(&self, plan: &PlanDocument) -> Result<Message> {
        if announcement_pending(Some(plan)).is_none() {
            bail!("Only an active plan whose tasks are all resolved can be recorded");
        }

        let id = Uuid::new_v5(
            &Uuid::NAMESPACE_OID,
            format!(
                "opencrabs.plan_completed.v1:{}:{}",
                plan.session_id, plan.id
            )
            .as_bytes(),
        );
        let repo = MessageRepository::new(self.context.pool());
        if let Some(existing) = repo
            .find_by_id(id)
            .await
            .context("Failed to check completed-plan history")?
        {
            return validate_existing_completed_plan(existing, plan);
        }

        let content = serde_json::to_string(&CompletedPlanPayload {
            version: PLAN_COMPLETED_PAYLOAD_VERSION,
            plan: plan.clone(),
        })
        .context("Failed to serialize completed plan")?;
        let message = Message {
            id,
            session_id: plan.session_id,
            role: PLAN_COMPLETED_ROLE.to_string(),
            content,
            sequence: self.get_next_sequence(plan.session_id).await?,
            // The repository stores Unix seconds. Return that same precision
            // so an immediate retry compares identical to the durable row.
            created_at: chrono::DateTime::from_timestamp(Utc::now().timestamp(), 0)
                .expect("current Unix seconds always form a valid UTC timestamp"),
            token_count: None,
            cost: None,
            input_tokens: None,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            thinking: None,
            duration_secs: None,
            // A row written by a turn is never a forward (#2608).
            forward: None,
        };

        match repo.create(&message).await {
            Ok(()) => {
                tracing::debug!(
                    "Recorded completed plan {} in session {} (seq: {})",
                    plan.id,
                    plan.session_id,
                    message.sequence
                );
                Ok(message)
            }
            Err(error) => {
                if let Some(existing) = repo
                    .find_by_id(id)
                    .await
                    .context("Failed to re-check completed-plan history")?
                {
                    return validate_existing_completed_plan(existing, plan);
                }
                Err(error).context("Failed to record completed plan")
            }
        }
    }

    /// Create a new message
    pub async fn create_message(
        &self,
        session_id: Uuid,
        role: String,
        content: String,
    ) -> Result<Message> {
        let repo = MessageRepository::new(self.context.pool());

        // Get the next sequence number for this session
        let sequence = self.get_next_sequence(session_id).await?;

        let message = Message {
            id: Uuid::new_v4(),
            session_id,
            role,
            content,
            sequence,
            created_at: Utc::now(),
            token_count: None,
            cost: None,
            input_tokens: None,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            thinking: None,
            duration_secs: None,
            // A row written by a turn is never a forward (#2608).
            forward: None,
        };

        repo.create(&message)
            .await
            .context("Failed to create message")?;

        tracing::debug!(
            "Created new message: {} in session {} (seq: {})",
            message.id,
            session_id,
            sequence
        );
        Ok(message)
    }

    /// Get a message by ID
    pub async fn get_message(&self, id: Uuid) -> Result<Option<Message>> {
        let repo = MessageRepository::new(self.context.pool());
        repo.find_by_id(id).await.context("Failed to get message")
    }

    /// Get a message by ID, returning an error if not found
    pub async fn get_message_required(&self, id: Uuid) -> Result<Message> {
        self.get_message(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Message not found: {}", id))
    }

    /// List all messages for a session
    pub async fn list_messages_for_session(&self, session_id: Uuid) -> Result<Vec<Message>> {
        let repo = MessageRepository::new(self.context.pool());
        repo.find_by_session(session_id)
            .await
            .context("Failed to list messages for session")
    }

    /// Record that the user stopped a turn (#2811).
    ///
    /// The cancel drops the turn's future where it stood: no `Done`, no
    /// `Failed`, and therefore no row that says the turn was cut short. This is
    /// that row, written after the fact so the verdict survives a reload, a
    /// session switch and a restart.
    pub async fn record_turn_stopped(&self, session_id: Uuid) -> Result<Message> {
        self.create_message(
            session_id,
            TURN_STOPPED_ROLE.to_string(),
            "Stopped.".to_string(),
        )
        .await
    }

    /// The newest `limit` messages of a session, oldest-first. Reads only the
    /// tail, so a session switch costs `limit` rows whatever the history size.
    pub async fn list_recent_messages_for_session(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let repo = MessageRepository::new(self.context.pool());
        repo.find_recent_by_session(session_id, limit)
            .await
            .context("Failed to list recent messages for session")
    }

    /// Up to `limit` messages strictly older than `before_sequence`,
    /// oldest-first. Backs PageUp paging.
    pub async fn list_messages_before_sequence(
        &self,
        session_id: Uuid,
        before_sequence: i32,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let repo = MessageRepository::new(self.context.pool());
        repo.find_recent_before_sequence(session_id, before_sequence, limit)
            .await
            .context("Failed to list messages before sequence")
    }

    /// Update a message
    pub async fn update_message(&self, message: &Message) -> Result<()> {
        let repo = MessageRepository::new(self.context.pool());
        repo.update(message)
            .await
            .context("Failed to update message")?;

        tracing::debug!("Updated message: {}", message.id);
        Ok(())
    }

    /// Update message usage statistics.
    /// `input_tokens` is the server-reported prompt token count for the
    /// request that produced this assistant response. It overrides the
    /// prior value if any (always the latest server reading).
    pub async fn update_message_usage(&self, id: Uuid, usage: MessageUsage) -> Result<()> {
        let MessageUsage {
            token_count,
            cost,
            input_tokens,
            cache_creation_tokens,
            cache_read_tokens,
            duration_secs,
        } = usage;
        let mut message = self.get_message_required(id).await?;
        message.token_count = Some(token_count);
        message.cost = Some(cost);
        if duration_secs.is_some() {
            message.duration_secs = duration_secs;
        }
        if input_tokens.is_some() {
            message.input_tokens = input_tokens;
        }
        if cache_creation_tokens.is_some() {
            message.cache_creation_tokens = cache_creation_tokens;
        }
        if cache_read_tokens.is_some() {
            message.cache_read_tokens = cache_read_tokens;
        }

        let repo = MessageRepository::new(self.context.pool());
        repo.update(&message)
            .await
            .context("Failed to update message usage")?;

        tracing::debug!(
            "Updated message usage: {} ({} output, {} input, {} cache_create, {} cache_read, ${:.4})",
            id,
            token_count,
            input_tokens
                .map(|t| t.to_string())
                .unwrap_or_else(|| "—".to_string()),
            cache_creation_tokens
                .map(|t| t.to_string())
                .unwrap_or_else(|| "—".to_string()),
            cache_read_tokens
                .map(|t| t.to_string())
                .unwrap_or_else(|| "—".to_string()),
            cost
        );
        Ok(())
    }

    /// Server-reported prompt tokens from the most recent assistant
    /// response in this session. Authoritative "last known context size"
    /// — no estimation, no mirror cache.
    pub async fn last_assistant_input_tokens(&self, session_id: Uuid) -> Result<Option<i32>> {
        let repo = MessageRepository::new(self.context.pool());
        repo.last_assistant_input_tokens(session_id)
            .await
            .context("Failed to read last assistant input_tokens")
    }

    /// Append content to an existing message (for real-time history persistence)
    pub async fn append_content(&self, id: Uuid, content_to_append: &str) -> Result<()> {
        let repo = MessageRepository::new(self.context.pool());
        repo.append_content(id, content_to_append)
            .await
            .context("Failed to append to message")?;
        Ok(())
    }

    /// Set thinking/reasoning content on a message (non-CLI providers).
    pub async fn set_thinking(&self, id: Uuid, thinking: &str) -> Result<()> {
        let repo = MessageRepository::new(self.context.pool());
        repo.set_thinking(id, thinking)
            .await
            .context("Failed to set thinking")?;
        Ok(())
    }

    /// Append thinking to a message (non-CLI providers, multi-iteration).
    pub async fn append_thinking(&self, id: Uuid, thinking: &str) -> Result<()> {
        let repo = MessageRepository::new(self.context.pool());
        repo.append_thinking(id, thinking)
            .await
            .context("Failed to append thinking")?;
        Ok(())
    }

    /// Delete a message
    pub async fn delete_message(&self, id: Uuid) -> Result<()> {
        let repo = MessageRepository::new(self.context.pool());
        repo.delete(id).await.context("Failed to delete message")?;

        tracing::debug!("Deleted message: {}", id);
        Ok(())
    }

    /// Delete all messages for a session
    pub async fn delete_messages_for_session(&self, session_id: Uuid) -> Result<()> {
        let repo = MessageRepository::new(self.context.pool());
        repo.delete_by_session(session_id)
            .await
            .context("Failed to delete messages for session")?;

        tracing::info!("Deleted messages for session {}", session_id);
        Ok(())
    }

    /// Count messages in a session
    pub async fn count_messages_in_session(&self, session_id: Uuid) -> Result<i64> {
        let repo = MessageRepository::new(self.context.pool());
        repo.count_by_session(session_id)
            .await
            .context("Failed to count messages in session")
    }

    /// Get the next sequence number for a session
    async fn get_next_sequence(&self, session_id: Uuid) -> Result<i32> {
        let count = self.count_messages_in_session(session_id).await?;
        Ok((count + 1) as i32)
    }

    /// Get the last message in a session
    pub async fn get_last_message(&self, session_id: Uuid) -> Result<Option<Message>> {
        let messages = self.list_messages_for_session(session_id).await?;
        Ok(messages.into_iter().last())
    }

    /// Get messages by role
    pub async fn get_messages_by_role(&self, session_id: Uuid, role: &str) -> Result<Vec<Message>> {
        let messages = self.list_messages_for_session(session_id).await?;
        Ok(messages.into_iter().filter(|m| m.role == role).collect())
    }

    /// Calculate total tokens for a session
    pub async fn calculate_total_tokens(&self, session_id: Uuid) -> Result<i64> {
        let messages = self.list_messages_for_session(session_id).await?;
        let total = messages.iter().filter_map(|m| m.token_count).sum();
        Ok(total)
    }

    /// Calculate total cost for a session
    pub async fn calculate_total_cost(&self, session_id: Uuid) -> Result<f64> {
        let messages = self.list_messages_for_session(session_id).await?;
        let total = messages.iter().filter_map(|m| m.cost).sum();
        Ok(total)
    }
}
