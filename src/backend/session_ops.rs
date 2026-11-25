//! Per-session commands: create, delete, clear, model, cd, undo.
//!
//! Operations land here as their GUI doors move behind the contract.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::services::{FileService, MessageService, ServiceContext, SessionService};

use super::{BackendError, BlobRef, Ctx, Op, Scope};

/// Delete one session's messages, as the TUI's ctrl+k does. The session row
/// stays, so the conversation is kept and only its history clears (#2430).
///
/// The service example: the work goes through a crab service built on the
/// ledger, not through a repository by hand. Its failures keep the exact
/// `clear: {e}` text the window showed, which is why it opens the ledger
/// through [`Ctx::database`] rather than [`Ctx::db`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearMessages {
    /// The session whose messages go.
    pub session_id: Uuid,
}

impl Op for ClearMessages {
    const NAME: &'static str = "session_ops.clear_messages";
    const SCOPE: Scope = Scope::Manage;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx
            .database()
            .await
            .map_err(|e| BackendError::Failed(format!("clear: {e}")))?;
        let service = MessageService::new(ServiceContext::new(db.pool().clone()));
        service
            .delete_messages_for_session(self.session_id)
            .await
            .map_err(|e| BackendError::Failed(format!("clear: {e}")))
    }
}

/// Open a new conversation, titled "New Chat", on the running provider and
/// model, in the directory the crab was launched from. Mirrors the TUI's
/// `create_new_session`: a blank session that answers with another model
/// than the one the window shows is the drift this avoids.
///
/// Connects only, never migrates: migrations belong to the daemon and to the
/// agent bootstrap. Failures keep the `db connect: {e}` and
/// `create session: {e}` texts the window showed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSession {
    /// The provider the new row inherits.
    pub provider: Option<String>,
    /// The model the new row inherits.
    pub model: Option<String>,
}

impl Op for CreateSession {
    const NAME: &'static str = "session_ops.create";
    const SCOPE: Scope = Scope::Manage;
    type Reply = Uuid;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx
            .database()
            .await
            .map_err(|e| BackendError::Failed(format!("db connect: {e}")))?;
        let service = SessionService::new(ServiceContext::new(db.pool().clone()));
        let working_directory = Some(
            crate::utils::cwd::launch_cwd()
                .to_string_lossy()
                .to_string(),
        );
        service
            .create_session_with_provider(
                Some("New Chat".to_string()),
                self.provider,
                self.model,
                working_directory,
            )
            .await
            .map(|s| s.id)
            .map_err(|e| BackendError::Failed(format!("create session: {e}")))
    }
}

/// Delete one conversation through `SessionService::delete_session`, the call
/// the TUI's `d` key makes. The row is archived, never dropped, so the usage
/// ledger keeps its history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteSession {
    /// The conversation to delete.
    pub session_id: Uuid,
}

impl Op for DeleteSession {
    const NAME: &'static str = "session_ops.delete";
    const SCOPE: Scope = Scope::Admin;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx
            .database()
            .await
            .map_err(|e| BackendError::Failed(format!("db connect: {e}")))?;
        let service = SessionService::new(ServiceContext::new(db.pool().clone()));
        service
            .delete_session(self.session_id)
            .await
            .map_err(|e| BackendError::Failed(format!("delete session: {e}")))
    }
}

/// Register the files a message carried against its session, what the TUI
/// does before sending (`FileService::get_or_create_file`). The message text
/// already holds the markers; this is the bookkeeping half.
///
/// A file that cannot be tracked must never fail the turn, so each one that
/// fails comes back as the line the window logs, and the rest go on. Only a
/// ledger that does not open fails the operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackFiles {
    /// The session the files belong to.
    pub session_id: Uuid,
    /// The files, as attached.
    pub paths: Vec<PathBuf>,
}

impl Op for TrackFiles {
    const NAME: &'static str = "session_ops.track_files";
    const SCOPE: Scope = Scope::Send;
    /// One line per file that was not tracked, `file track <path>: <why>`.
    type Reply = Vec<String>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx
            .database()
            .await
            .map_err(|e| BackendError::Failed(format!("file track db connect: {e}")))?;
        let service = FileService::new(ServiceContext::new(db.pool().clone()));
        let mut untracked = Vec::new();
        for path in self.paths {
            // The name in the line is the file's own: a failure has to say
            // which attachment did not get tracked.
            let name = path.display().to_string();
            if let Err(e) = service
                .get_or_create_file(self.session_id, path, None)
                .await
            {
                untracked.push(format!("file track {name}: {e}"));
            }
        }
        Ok(untracked)
    }
}

/// One tracked file of a session: what the ctrl+g browser lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedFile {
    /// The tracked path.
    pub path: PathBuf,
    /// Its size in bytes, when the ledger has it.
    pub size: Option<i64>,
}

/// The files a conversation tracked, the fetch behind ctrl+g. Failures keep
/// the `db connect: {e:#}` and `{e:#}` texts the window showed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFiles {
    /// The conversation.
    pub session_id: Uuid,
}

impl Op for SessionFiles {
    const NAME: &'static str = "session_ops.files";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<TrackedFile>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx
            .database()
            .await
            .map_err(|e| BackendError::Failed(format!("db connect: {e:#}")))?;
        let service = FileService::new(ServiceContext::new(db.pool().clone()));
        let files = service
            .list_files_for_session(self.session_id)
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))?;
        Ok(files
            .into_iter()
            .map(|f| TrackedFile {
                path: f.path,
                size: f.size,
            })
            .collect())
    }
}

/// Where a conversation's plan file lives, and why the lookup fell back when
/// it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPathReply {
    /// The plan file. Always resolved: a ledger that does not open falls back
    /// to the profile's `session/` directory, as `plan_json_path` does.
    pub plan: BlobRef,
    /// The connect failure behind a fallback, for the window's log.
    pub db_error: Option<String>,
}

/// Resolve a conversation's plan-file path: the project directory when the
/// session is bound to one, the profile's `session/` fallback when it is not
/// or the ledger is unreachable. Never the global pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPath {
    /// The conversation.
    pub session_id: Uuid,
}

impl Op for PlanPath {
    const NAME: &'static str = "session_ops.plan_path";
    const SCOPE: Scope = Scope::Read;
    type Reply = PlanPathReply;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let (dir, db_error) = match ctx.database().await {
            Ok(db) => (
                crate::utils::plan_files::project_session_dir(self.session_id, db.pool()).await,
                None,
            ),
            Err(e) => (None, Some(e.to_string())),
        };
        let dir = dir.unwrap_or_else(|| crate::config::opencrabs_home().join("session"));
        let path = dir.join(format!(".opencrabs_plan_{}.json", self.session_id));
        Ok(PlanPathReply {
            plan: BlobRef::at(path.to_string_lossy().into_owned()),
            db_error,
        })
    }
}

/// Switch the model behind the live service, through
/// `channels::commands::direct_model_switch`: the terminal's own path, which
/// writes the config, recreates the provider and updates the session row.
/// The reply is the line the terminal prints for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchModel {
    /// The session that asked.
    pub session_id: Uuid,
    /// `provider/model`, with an optional trailing ` all`.
    pub pair: String,
}

impl Op for SwitchModel {
    const NAME: &'static str = "session_ops.switch_model";
    const SCOPE: Scope = Scope::Admin;
    type Reply = String;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let service = ctx.service()?;
        Ok(
            crate::channels::commands::direct_model_switch(service, self.session_id, &self.pair)
                .await,
        )
    }
}

/// How many sessions a scope-all pick of `pair` would change (#3035). A read:
/// the switch itself still goes through [`SwitchModel`]. A pair without a `/`
/// or a count that fails answers 0, as the window always did.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeCount {
    /// `provider/model`.
    pub pair: String,
}

impl Op for ScopeCount {
    const NAME: &'static str = "session_ops.scope_count";
    const SCOPE: Scope = Scope::Read;
    type Reply = usize;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let Some((provider, model)) = self.pair.split_once('/') else {
            return Ok(0);
        };
        let db = ctx.db().await?;
        let sessions = SessionService::new(ServiceContext::new(db.pool().clone()));
        Ok(sessions
            .count_provider_model_all_sessions(provider, model)
            .await
            .unwrap_or(0))
    }
}

/// What undoing a stopped turn did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoReply {
    /// The turn's rows are gone and the conversation should reload.
    pub reverted: bool,
    /// Each step that failed, as the line the window logs.
    pub errors: Vec<String>,
}

/// Delete the rows a turn wrote before it was stopped (#2635).
///
/// A turn persists its `user` row and an empty `assistant` placeholder when it
/// starts. A stop that beats the first token hands the text back to the
/// composer, so those rows would redraw the bubble the user just took back.
/// The trailing pair is cut only when it is the pair the turn opened: a
/// finished answer, or anything a tool wrote, stops the walk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoLastTurn {
    /// The conversation whose last turn goes.
    pub session_id: Uuid,
}

impl Op for UndoLastTurn {
    const NAME: &'static str = "session_ops.undo_last_turn";
    const SCOPE: Scope = Scope::Manage;
    type Reply = UndoReply;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let session_id = self.session_id;
        let db = ctx.db().await?;
        let messages = MessageService::new(ServiceContext::new(db.pool().clone()));
        let mut reply = UndoReply::default();
        let rows = match messages.list_messages_for_session(session_id).await {
            Ok(rows) => rows,
            Err(e) => {
                reply.errors.push(format!("turn undo read: {e}"));
                return Ok(reply);
            }
        };
        // Newest first: the placeholder, then the question it echoed.
        let Some(placeholder) = rows.last() else {
            return Ok(reply);
        };
        if placeholder.role != "assistant" || !placeholder.content.trim().is_empty() {
            // The turn had already answered, or wrote nothing yet. An answer
            // that landed between the stop and this call is the user's: only
            // the empty placeholder is ever cut.
            return Ok(reply);
        }
        if let Err(e) = messages.delete_message(placeholder.id).await {
            reply.errors.push(format!("turn undo assistant row: {e}"));
            return Ok(reply);
        }
        reply.reverted = true;
        let Some(question) = rows.get(rows.len().saturating_sub(2)) else {
            return Ok(reply);
        };
        if question.role == "user"
            && let Err(e) = messages.delete_message(question.id).await
        {
            reply.errors.push(format!("turn undo user row: {e}"));
        }
        Ok(reply)
    }
}

/// Record a stopped turn in the session's history, and drop its recovery row.
///
/// A cancel drops the turn's future, so nothing inside the turn writes the
/// row that says it was stopped (#2811), nor deletes its `pending_requests`
/// ticket (#2850). `answer` is what the card had streamed when the stop
/// landed (#2851); it goes in before the marker, so the turn reads question,
/// answer, verdict. The two steps are independent: each failure is a line in
/// the reply and does not stop the other.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordTurnStopped {
    /// The session whose turn was stopped.
    pub session_id: Uuid,
    /// What the card had streamed, if anything.
    pub answer: Option<String>,
}

impl Op for RecordTurnStopped {
    const NAME: &'static str = "session_ops.record_turn_stopped";
    const SCOPE: Scope = Scope::Manage;
    /// Each step that failed, as the line the window logs.
    type Reply = Vec<String>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let RecordTurnStopped { session_id, answer } = self;
        let db = ctx.db().await?;
        let mut errors = Vec::new();
        let messages = MessageService::new(ServiceContext::new(db.pool().clone()));
        // What the card streamed goes in first (#2851), so the turn reads
        // question, answer, verdict, the order the user saw.
        if let Err(e) = messages.record_stopped_turn(session_id, answer).await {
            errors.push(format!("turn stopped row: {e}"));
        }
        // The recovery ticket goes with it (#2850): the turn is over because
        // the user ended it, so no later boot may replay it.
        let pending = crate::db::repository::PendingRequestRepository::new(db.pool().clone());
        if let Err(e) = pending.delete_session(session_id).await {
            errors.push(format!("turn stopped recovery row: {e}"));
        }
        Ok(errors)
    }
}

/// Generate the personalized brain files from the onboarding notes.
///
/// A port of the terminal's `generate_brain_files_background`: one completion
/// through the service's provider, parsed into the five brain files and
/// written into `workspace` on the crab. The reply is how many landed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainGenerate {
    /// The onboarding prompt.
    pub prompt: String,
    /// The crab's brain workspace.
    pub workspace: PathBuf,
}

impl Op for BrainGenerate {
    const NAME: &'static str = "session_ops.brain_generate";
    const SCOPE: Scope = Scope::Admin;
    type Reply = usize;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let service = ctx.service()?;
        let provider = service.provider();
        let model = service.provider_model();
        let request = crate::brain::provider::LLMRequest::new(
            model,
            vec![crate::brain::provider::Message::user(self.prompt)],
        )
        .with_max_tokens(65536);
        match tokio::time::timeout(Duration::from_secs(120), provider.complete(request)).await {
            Ok(Ok(response)) => {
                let text: String = response
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        crate::brain::provider::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                let parsed = crate::tui::onboarding::parse_brain_sections(&text);
                // SOUL and USER are the two a setup cannot do without;
                // without them the reply was prose, not sections.
                if parsed[0].is_none() || parsed[1].is_none() {
                    return Err(BackendError::Failed(
                        "the answer had no brain sections".to_string(),
                    ));
                }
                let files = [
                    ("SOUL.md", &parsed[0]),
                    ("USER.md", &parsed[1]),
                    ("AGENTS.md", &parsed[2]),
                    ("TOOLS.md", &parsed[3]),
                    ("MEMORY.md", &parsed[4]),
                ];
                let mut written = 0;
                for (name, content) in &files {
                    if let Some(content) = content {
                        if let Err(e) = std::fs::write(self.workspace.join(name), content) {
                            return Err(BackendError::Failed(format!("writing {name}: {e}")));
                        }
                        written += 1;
                    }
                }
                Ok(written)
            }
            Ok(Err(e)) => Err(BackendError::Failed(format!("generation failed: {e}"))),
            Err(_) => Err(BackendError::Failed(
                "generation timed out after 120s".to_string(),
            )),
        }
    }
}

/// Apply an operator `/cd` to one session through
/// `AgentService::apply_operator_cd`, the applier the terminal calls too. The
/// reply is the canonical directory that was applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyCd {
    /// The session whose directory changes.
    pub session_id: Uuid,
    /// The directory asked for.
    pub dir: PathBuf,
}

impl Op for ApplyCd {
    const NAME: &'static str = "session_ops.apply_cd";
    const SCOPE: Scope = Scope::Admin;
    type Reply = PathBuf;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let service = ctx.service()?;
        Ok(service.apply_operator_cd(self.session_id, &self.dir).await)
    }
}

/// The crab's half of a text command. The window parses the line and words
/// the replies; what runs here is what touches the crab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrabCommand {
    /// `/discard`: drop the session's live plan.
    Discard,
    /// `/clear`: cut the context through `AgentService::clear_context`.
    Clear,
    /// `/rtk`: run `rtk gain`.
    Rtk,
    /// `/approve` with no argument: read the approval policy.
    ApprovalPolicy,
    /// `/approve <policy>`: write the policy and apply it to the live service.
    SetApprovalPolicy {
        /// A policy the window already validated.
        policy: String,
    },
}

/// What a [`TextCommand`] answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandAnswer {
    /// The finished line for the notices.
    Line(String),
    /// `rtk gain` ran; the window words its output.
    RtkGain {
        /// Whether it exited with success.
        success: bool,
        /// Its stdout, lossy.
        stdout: String,
        /// Its stderr, lossy.
        stderr: String,
    },
    /// The approval policy the config holds.
    ApprovalPolicy(String),
    /// The policy was written and the live service follows it.
    ApprovalSet,
}

/// Run the crab's half of one text command for a session (#2469).
///
/// The work is the engine's own: clear is `AgentService::clear_context`, the
/// call the channels make; the approval policy is the value
/// `Config::write_key` persists for the terminal; discard is
/// `utils::plan_mode::discard`. Nothing here re-implements a command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextCommand {
    /// The session the command was run for.
    pub session_id: Uuid,
    /// What to run.
    pub command: CrabCommand,
}

impl Op for TextCommand {
    const NAME: &'static str = "session_ops.text_command";
    const SCOPE: Scope = Scope::Admin;
    type Reply = CommandAnswer;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let session_id = self.session_id;
        Ok(match self.command {
            CrabCommand::Discard => match ctx.database().await {
                Ok(db) => {
                    let svc = ServiceContext::new(db.pool().clone());
                    CommandAnswer::Line(crate::utils::plan_mode::discard(session_id, &svc).await)
                }
                Err(e) => CommandAnswer::Line(format!("/discard could not open the database: {e}")),
            },
            CrabCommand::Clear => match ctx.service() {
                Ok(service) => match service.clear_context(session_id).await {
                    Ok(receipt) => CommandAnswer::Line(crate::tui::clear_notice::cleared(&receipt)),
                    Err(e) => CommandAnswer::Line(crate::tui::clear_notice::failed(&e)),
                },
                Err(_) => CommandAnswer::Line(
                    "/clear needs the agent running; it is offline right now.".to_string(),
                ),
            },
            CrabCommand::Rtk => rtk_gain().await,
            CrabCommand::ApprovalPolicy => CommandAnswer::ApprovalPolicy(
                crate::config::Config::load()
                    .map(|cfg| cfg.agent.approval_policy)
                    .unwrap_or_else(|_| "ask".to_string()),
            ),
            CrabCommand::SetApprovalPolicy { policy } => {
                match crate::config::Config::write_key("agent", "approval_policy", &policy) {
                    Ok(_) => {
                        // Both halves: the file and the live service. A
                        // written key alone would leave the running crab
                        // gating tools by the policy it started with.
                        if let Ok(service) = ctx.service() {
                            service.set_auto_approve_tools(
                                crate::utils::approval::policy_auto_approves(&policy),
                            );
                        }
                        CommandAnswer::ApprovalSet
                    }
                    Err(e) => {
                        CommandAnswer::Line(format!("/approve could not write the policy: {e}"))
                    }
                }
            }
        })
    }
}

/// `rtk gain` on the crab, or why it cannot run.
async fn rtk_gain() -> CommandAnswer {
    #[cfg(feature = "rtk")]
    {
        if !crate::rtk::is_rtk_available().await {
            return CommandAnswer::Line(crate::rtk::RTK_NOT_INSTALLED_HELP.to_string());
        }
        match tokio::process::Command::new("rtk")
            .arg("gain")
            .output()
            .await
        {
            Ok(output) => CommandAnswer::RtkGain {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            Err(e) => CommandAnswer::Line(format!("rtk gain: {e}")),
        }
    }
    #[cfg(not(feature = "rtk"))]
    {
        CommandAnswer::Line("RTK is not compiled into this build.".to_string())
    }
}

ops! {
    ClearMessages,
    CreateSession,
    DeleteSession,
    TrackFiles,
    SessionFiles,
    PlanPath,
    SwitchModel,
    ScopeCount,
    UndoLastTurn,
    RecordTurnStopped,
    BrainGenerate,
    ApplyCd,
    TextCommand,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ctx::temp_ctx;
    use crate::db::models::{Message, Session};
    use crate::db::repository::{MessageRepository, SessionRepository};

    #[tokio::test]
    async fn clear_messages_empties_the_transcript_and_keeps_the_session() {
        let (_dir, ctx) = temp_ctx().await;
        let db = ctx.db().await.expect("db");
        let sessions = SessionRepository::new(db.pool().clone());
        let messages = MessageRepository::new(db.pool().clone());
        let session = Session::new(Some("chat".into()), None, None);
        sessions.create(&session).await.expect("create session");
        let message = Message::new(session.id, "user".into(), "hello".into(), 1);
        messages.create(&message).await.expect("create message");

        ClearMessages {
            session_id: session.id,
        }
        .run(&ctx)
        .await
        .expect("clear");

        let left = messages
            .find_by_session(session.id)
            .await
            .expect("read back");
        assert!(left.is_empty(), "{} messages left", left.len());
        assert!(
            sessions
                .find_by_id(session.id)
                .await
                .expect("read session")
                .is_some(),
            "the session row stays"
        );
    }

    mod doors {
        use std::sync::Arc;

        use super::*;
        use crate::backend::parity::{Gateway, forbidden, parity};
        use crate::brain::agent::AgentService;

        /// A gateway whose crab has an agent service, on a mock provider that
        /// answers one fixed line and never reaches the network.
        async fn booted() -> Gateway {
            let gateway = Gateway::start().await;
            let db = gateway.ctx.db().await.expect("db");
            let provider: Arc<dyn crate::brain::provider::Provider> =
                Arc::new(crate::tests::agent_service_mocks::MockProvider);
            let service =
                AgentService::new_for_test(provider, ServiceContext::new(db.pool().clone())).await;
            gateway.ctx.set_service(Arc::new(service));
            gateway
        }

        async fn session(gateway: &Gateway) -> Session {
            let db = gateway.ctx.db().await.expect("db");
            let session = Session::new(Some("chat".into()), None, None);
            SessionRepository::new(db.pool().clone())
                .create(&session)
                .await
                .expect("create session");
            session
        }

        async fn messages(gateway: &Gateway) -> MessageRepository {
            let db = gateway.ctx.db().await.expect("db");
            MessageRepository::new(db.pool().clone())
        }

        #[tokio::test]
        async fn create_session_has_parity() {
            // A new row has a new id, so a write by both doors cannot answer
            // the same: parity is held on a ledger neither door can open.
            let mut broken = Gateway::start().await;
            broken.ctx = Ctx::new(broken.home().to_path_buf());
            let answer = parity!(
                broken,
                CreateSession {
                    provider: Some("anthropic".into()),
                    model: Some("claude-x".into()),
                }
            );
            assert!(answer.get("Err").is_some(), "{answer}");

            // On a working ledger each door creates its own row as asked.
            let gateway = Gateway::start().await;
            let op = CreateSession {
                provider: Some("anthropic".into()),
                model: Some("claude-x".into()),
            };
            let local = gateway.local(op.clone()).await.expect("local create");
            let remote = gateway.remote(op, Scope::ALL).await.expect("remote create");
            let db = gateway.ctx.db().await.expect("db");
            let repo = SessionRepository::new(db.pool().clone());
            for id in [local, remote] {
                let row = repo.find_by_id(id).await.expect("read").expect("the row");
                assert_eq!(row.title.as_deref(), Some("New Chat"));
                assert_eq!(row.provider_name.as_deref(), Some("anthropic"));
                assert_eq!(row.model.as_deref(), Some("claude-x"));
            }

            let before = repo.list(Default::default()).await.expect("list").len();
            forbidden!(
                gateway,
                CreateSession {
                    provider: None,
                    model: None,
                }
            );
            let after = repo.list(Default::default()).await.expect("list").len();
            assert_eq!(before, after, "a refused create wrote nothing");
        }

        #[tokio::test]
        async fn delete_session_has_parity() {
            let gateway = Gateway::start().await;
            let doomed = session(&gateway).await;
            parity!(
                gateway,
                DeleteSession {
                    session_id: doomed.id,
                }
            );

            let kept = session(&gateway).await;
            let messages = messages(&gateway).await;
            let row = Message::new(kept.id, "user".into(), "hello".into(), 1);
            messages.create(&row).await.expect("message");
            forbidden!(
                gateway,
                DeleteSession {
                    session_id: kept.id,
                }
            );
            assert_eq!(
                messages.find_by_session(kept.id).await.expect("read").len(),
                1,
                "a refused delete removed nothing"
            );
        }

        #[tokio::test]
        async fn track_files_and_session_files_have_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway).await;
            let file = gateway.home().join("note.txt");
            std::fs::write(&file, "hello").expect("seed file");

            let answer = parity!(
                gateway,
                TrackFiles {
                    session_id: chat.id,
                    paths: vec![file.clone()],
                }
            );
            assert_eq!(answer, serde_json::json!({ "Ok": [] }));

            let answer = parity!(
                gateway,
                SessionFiles {
                    session_id: chat.id
                }
            );
            let rows = answer["Ok"].as_array().expect("a list of files");
            assert!(!rows.is_empty(), "{answer}");

            forbidden!(
                gateway,
                SessionFiles {
                    session_id: chat.id
                }
            );
            forbidden!(
                gateway,
                TrackFiles {
                    session_id: chat.id,
                    paths: vec![file],
                }
            );
        }

        #[tokio::test]
        async fn plan_path_has_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway).await;
            let answer = parity!(
                gateway,
                PlanPath {
                    session_id: chat.id
                }
            );
            let path = answer["Ok"]["plan"]["path"].as_str().expect("a path");
            let want = gateway
                .home()
                .join("session")
                .join(format!(".opencrabs_plan_{}.json", chat.id));
            assert_eq!(
                path,
                want.to_string_lossy(),
                "an unbound session falls back"
            );
            assert!(answer["Ok"]["db_error"].is_null(), "{answer}");
            forbidden!(
                gateway,
                PlanPath {
                    session_id: chat.id
                }
            );
        }

        #[tokio::test]
        async fn switch_model_has_parity() {
            let gateway = booted().await;
            let chat = session(&gateway).await;
            let answer = parity!(
                gateway,
                SwitchModel {
                    session_id: chat.id,
                    pair: "no-such-provider/no-such-model".into(),
                }
            );
            assert!(answer["Ok"].is_string(), "{answer}");
            forbidden!(
                gateway,
                SwitchModel {
                    session_id: chat.id,
                    pair: "no-such-provider/no-such-model".into(),
                }
            );
        }

        #[tokio::test]
        async fn scope_count_has_parity() {
            let gateway = Gateway::start().await;
            let db = gateway.ctx.db().await.expect("db");
            let repo = SessionRepository::new(db.pool().clone());
            for title in ["one", "two"] {
                let row = Session::new(Some(title.into()), Some("old".into()), Some("p".into()));
                repo.create(&row).await.expect("create");
            }
            let answer = parity!(
                gateway,
                ScopeCount {
                    pair: "p/new".into()
                }
            );
            assert!(answer["Ok"].is_u64(), "{answer}");
            let answer = parity!(
                gateway,
                ScopeCount {
                    pair: "no-slash".into()
                }
            );
            assert_eq!(answer, serde_json::json!({ "Ok": 0 }));
            forbidden!(
                gateway,
                ScopeCount {
                    pair: "p/new".into()
                }
            );
        }

        #[tokio::test]
        async fn record_turn_stopped_has_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway).await;
            let messages = messages(&gateway).await;
            for (seq, (role, content)) in [("user", "question"), ("assistant", "")]
                .into_iter()
                .enumerate()
            {
                let row = Message::new(chat.id, role.into(), content.into(), seq as i32 + 1);
                messages.create(&row).await.expect("message");
            }
            let answer = parity!(
                gateway,
                RecordTurnStopped {
                    session_id: chat.id,
                    answer: Some("half an answer".into()),
                }
            );
            assert_eq!(answer, serde_json::json!({ "Ok": [] }));
            let rows = messages.find_by_session(chat.id).await.expect("read");
            assert!(
                rows.iter()
                    .any(|row| row.role == "assistant" && row.content.contains("half an answer")),
                "the streamed answer reached the ledger: {rows:?}"
            );

            let before = messages.find_by_session(chat.id).await.expect("read").len();
            forbidden!(
                gateway,
                RecordTurnStopped {
                    session_id: chat.id,
                    answer: None,
                }
            );
            assert_eq!(
                messages.find_by_session(chat.id).await.expect("read").len(),
                before,
                "a refused record wrote nothing"
            );
        }

        #[tokio::test]
        async fn undo_last_turn_has_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway).await;
            let messages = messages(&gateway).await;
            // Two stopped turns: each door cuts one, so both answer the same.
            for (seq, (role, content)) in [
                ("user", "first"),
                ("assistant", ""),
                ("user", "second"),
                ("assistant", ""),
            ]
            .into_iter()
            .enumerate()
            {
                let row = Message::new(chat.id, role.into(), content.into(), seq as i32 + 1);
                messages.create(&row).await.expect("message");
            }
            let answer = parity!(
                gateway,
                UndoLastTurn {
                    session_id: chat.id
                }
            );
            assert_eq!(
                answer,
                serde_json::json!({ "Ok": { "reverted": true, "errors": [] } })
            );
            assert!(
                messages
                    .find_by_session(chat.id)
                    .await
                    .expect("read")
                    .is_empty(),
                "both stopped turns are gone"
            );

            for (seq, role) in ["user", "assistant"].into_iter().enumerate() {
                let row = Message::new(chat.id, role.into(), String::new(), seq as i32 + 1);
                messages.create(&row).await.expect("message");
            }
            forbidden!(
                gateway,
                UndoLastTurn {
                    session_id: chat.id
                }
            );
            assert_eq!(
                messages.find_by_session(chat.id).await.expect("read").len(),
                2,
                "a refused undo cut nothing"
            );
        }

        #[tokio::test]
        async fn brain_generate_has_parity() {
            // The mock answers prose, not sections: both doors refuse it with
            // the same reason and write nothing.
            let gateway = booted().await;
            let workspace = gateway.home().join("brain");
            std::fs::create_dir_all(&workspace).expect("workspace");
            let answer = parity!(
                gateway,
                BrainGenerate {
                    prompt: "notes".into(),
                    workspace: workspace.clone(),
                }
            );
            assert_eq!(
                answer,
                serde_json::json!({
                    "Err": { "kind": "failed", "detail": "the answer had no brain sections" }
                })
            );
            forbidden!(
                gateway,
                BrainGenerate {
                    prompt: "notes".into(),
                    workspace: workspace.clone(),
                }
            );
            assert!(!workspace.join("SOUL.md").exists(), "nothing was written");
        }

        #[tokio::test]
        async fn apply_cd_has_parity() {
            let gateway = booted().await;
            let chat = session(&gateway).await;
            let dir = gateway.home().to_path_buf();
            let answer = parity!(
                gateway,
                ApplyCd {
                    session_id: chat.id,
                    dir: dir.clone(),
                }
            );
            let canonical = dir.canonicalize().expect("canonical home");
            assert_eq!(
                answer,
                serde_json::json!({ "Ok": canonical.to_string_lossy() })
            );
            forbidden!(
                gateway,
                ApplyCd {
                    session_id: chat.id,
                    dir,
                }
            );
        }

        #[tokio::test]
        async fn text_command_has_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway).await;
            // No service yet: /clear says so instead of failing.
            let answer = parity!(
                gateway,
                TextCommand {
                    session_id: chat.id,
                    command: CrabCommand::Clear,
                }
            );
            assert_eq!(
                answer,
                serde_json::json!({
                    "Ok": { "line": "/clear needs the agent running; it is offline right now." }
                })
            );
            let answer = parity!(
                gateway,
                TextCommand {
                    session_id: chat.id,
                    command: CrabCommand::ApprovalPolicy,
                }
            );
            assert!(answer["Ok"]["approval_policy"].is_string(), "{answer}");
            forbidden!(
                gateway,
                TextCommand {
                    session_id: chat.id,
                    command: CrabCommand::SetApprovalPolicy {
                        policy: "auto-always".into(),
                    },
                }
            );
        }
    }
}
