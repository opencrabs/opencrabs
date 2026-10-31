//! `/clear`: start the agent fresh at the current point, at no cost.
//!
//! A failed manual `/compact` on a large context is expensive: the whole
//! post-marker snapshot goes to the summariser, and on failure the walk
//! sends it again to every provider in the chain. When the user only wants
//! to move on there was no LLM-free way to do that; `/new` starts a
//! different session row and takes the visible history and title with it
//! (#1585).
//!
//! Clearing is one user row starting with the compaction-marker prefix. The
//! context loader already cuts at the last such row and the TUI already
//! hides it on reload, so history stays on screen and in the database, the
//! session keeps its id and title, and the next turn loads only the marker
//! body. That body names the session, points the agent at
//! `session_search`, and quotes the last six exchanges so the fresh start
//! keeps a warm tail of the conversation instead of none (#1905).

use uuid::Uuid;

use super::builder::AgentService;
use super::clear_snapshot::{EnvironmentSnapshot, PlanState, reply_instructions, snapshot_block};
use crate::brain::agent::error::{AgentError, Result};
use crate::services::{MessageService, SessionService};
use crate::utils::plan_files;

/// Prefix the context loader and the TUI reload both key on. Must start
/// with the compaction marker text, which is the only thing either checks.
pub(crate) const CLEAR_MARKER_PREFIX: &str = "[CONTEXT COMPACTION: cleared by the user]";

/// What a failed manual compaction tells the user about the way out.
pub(crate) const CLEAR_HINT: &str = "To continue fresh without a summary: cancel if it is \
     still running, then run /clear. The history stays in the session and the agent \
     starts from that point at no cost.";

/// How many user/assistant pairs the marker quotes as context.
const RECENT_PAIRS: usize = 6;

/// Per-message cap for the quoted tail, in characters so accented text
/// never splits a codepoint. Bounds the marker on chatty sessions.
const RECENT_MESSAGE_CAP: usize = 500;

/// Rows the quoted tail never includes: this prefix covers both `/clear`
/// markers and ordinary compaction summaries, neither of which is
/// conversation worth quoting back.
fn is_compaction_row(content: &str) -> bool {
    content.starts_with("[CONTEXT COMPACTION")
}

/// The pre-clear rows worth quoting: non-empty, not themselves markers,
/// newest last, capped at the last six pairs.
fn recent_tail(rows: &[(String, String)]) -> Vec<(String, String)> {
    let relevant: Vec<(String, String)> = rows
        .iter()
        .filter(|(_, content)| !content.trim().is_empty() && !is_compaction_row(content))
        .cloned()
        .collect();
    let start = relevant.len().saturating_sub(RECENT_PAIRS * 2);
    relevant[start..].to_vec()
}

/// One quoted message, trimmed and capped.
fn quoted(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.chars().count() <= RECENT_MESSAGE_CAP {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(RECENT_MESSAGE_CAP).collect();
    format!("{cut} [...]")
}

/// The pairs section appended under the instructions, if anything carried.
fn quoted_exchanges(recent: &[(String, String)]) -> Option<String> {
    if recent.is_empty() {
        return None;
    }
    let lines: Vec<String> = recent
        .iter()
        .map(|(role, content)| format!("{role}: {}", quoted(content)))
        .collect();
    Some(lines.join("\n\n"))
}

/// What the agent reads as its whole context after a clear.
pub(crate) fn clear_marker(session_title: Option<&str>, recent: &[(String, String)]) -> String {
    let title = session_title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("untitled");
    let exchanges = quoted_exchanges(&recent_tail(recent));
    // The opening sentence must match what follows: the empty-history
    // variant denies any carryover, the tailed one admits the quote.
    let carry = if exchanges.is_some() {
        "Only the last exchanges quoted below carry over; everything older is \
         out of your context."
    } else {
        "Nothing said or done before it is in your context."
    };
    let base = format!(
        "{CLEAR_MARKER_PREFIX}\n\n\
         The user cleared this session's context at this point. {carry} The full history \
         is still stored in this session, titled '{title}'. If you need something from \
         before, use the session_search tool: operation 'tail' with session '{title}' \
         reads the last messages, operation 'search' with a query and session '{title}' \
         finds specific content. Fetch only what the task needs. Continue from the \
         user's next message."
    );
    match exchanges {
        None => base,
        Some(exchanges) => format!(
            "{base}\n\n\
             The last exchanges before the clear, quoted as context only:\n\n\
             {exchanges}"
        ),
    }
}

/// Everything a `/clear` needs before its reply turn starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedClear {
    pub receipt: ClearReceipt,
    /// Persisted as the user row the loader cuts at. The TUI hides it on
    /// reload, and the history of the session stays intact.
    pub marker: String,
    /// What the model reads for the reply turn: the marker plus instructions
    /// that are never written to history.
    pub reply_context: String,
}

/// What happened, for the surface that asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearReceipt {
    pub session_title: Option<String>,
    /// A background summariser was in flight and has been aborted, so its
    /// summary of the old context can never land over the cleared one.
    pub aborted_background_compaction: bool,
}

impl ClearReceipt {
    /// One line for the user, the same on every surface.
    pub fn user_line(&self) -> String {
        let title = self
            .session_title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| format!(" (title '{t}')"))
            .unwrap_or_default();
        let aborted = if self.aborted_background_compaction {
            " A background compaction that was running has been cancelled."
        } else {
            ""
        };
        format!(
            "Context cleared. The history above stays in this session{title}; the agent \
             starts fresh from here and can search it with session_search when it needs \
             something. No summariser call was made.{aborted}"
        )
    }
}

impl AgentService {
    /// Cut the agent's context at the current point without a provider call.
    ///
    /// Aborts an in-flight background summariser first, exactly as
    /// `compact_context` does: its result is applied on a later visit and
    /// would otherwise overwrite the cleared context with a summary of the
    /// old one. Writes nothing: the caller starts the reply turn, and that
    /// turn persists `marker` as the row the loader cuts at.
    pub async fn prepare_clear(&self, session_id: Uuid) -> Result<PreparedClear> {
        let aborted_background_compaction = match self.take_pending_compaction(session_id) {
            Some(pending) => {
                tracing::info!("/clear: aborting the background compaction in flight");
                pending.abort();
                true
            }
            None => false,
        };

        let session = SessionService::new(self.context.clone())
            .get_session(session_id)
            .await
            .map_err(AgentError::db)?;
        let session_title = session.as_ref().and_then(|s| s.title.clone());
        let working_directory = session.and_then(|s| s.working_directory);

        let messages = MessageService::new(self.context.clone());

        // Read before the reply turn persists the marker, so the quote is
        // exactly what the user saw vanish. `clear_marker` filters out old
        // marker and summary rows and caps the quote at the last six pairs.
        let recent: Vec<(String, String)> = messages
            .list_messages_for_session(session_id)
            .await
            .map_err(AgentError::db)?
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect();

        let snapshot = environment_snapshot(session_id, working_directory).await;
        let marker = format!(
            "{}\n\n{}",
            clear_marker(session_title.as_deref(), &recent),
            snapshot_block(&snapshot)
        );
        let title = session_title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("untitled");
        let reply_context = format!("{marker}\n\n{}", reply_instructions(title, &snapshot));

        tracing::info!(
            "/clear: context cut for session {session_id} (title={:?}, plan={:?}, aborted_background={})",
            session_title,
            snapshot.plan_state,
            aborted_background_compaction
        );
        Ok(PreparedClear {
            receipt: ClearReceipt {
                session_title,
                aborted_background_compaction,
            },
            marker,
            reply_context,
        })
    }

    /// Cut the context and persist the marker, without starting a reply turn.
    /// For surfaces that do not stream a turn of their own (channels).
    pub async fn clear_context(&self, session_id: Uuid) -> Result<ClearReceipt> {
        let prepared = self.prepare_clear(session_id).await?;
        MessageService::new(self.context.clone())
            .create_message(session_id, "user".to_string(), prepared.marker)
            .await
            .map_err(AgentError::db)?;
        Ok(prepared.receipt)
    }
}

/// Reads where the session's state lives. Paths are reported only when the
/// file exists, so the agent is never pointed at a file that is not there.
async fn environment_snapshot(
    session_id: Uuid,
    working_directory: Option<String>,
) -> EnvironmentSnapshot {
    let plan_state = PlanState::from(plan_files::plan_mode_state(session_id).await);
    let (plan_md, plan_json) = if plan_state == PlanState::None {
        (None, None)
    } else {
        let md = plan_files::plan_md_path(session_id).await;
        let json = plan_files::plan_json_read_path(session_id).await;
        (
            md.exists().then(|| md.display().to_string()),
            json.exists().then(|| json.display().to_string()),
        )
    };
    EnvironmentSnapshot {
        working_directory,
        plan_state,
        plan_md,
        plan_json,
    }
}
