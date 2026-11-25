//! Starting, resuming and steering a turn.
//!
//! A turn is the crab's work: the dedup of a resent question, the call into
//! the agent service, the race against the stop and the shape of the answer
//! all run here. The surface keeps only what is its own: the event sink it
//! settles the card through and the boot accounting of its own launch.
//!
//! The progress of a running turn does not travel on the reply. It still
//! reaches the surface through the service's own forwarder, so the reply is
//! only the terminal outcome.
//!
//! A tool that needs approval asks a remote surface the way a channel asks
//! its chat: the turn publishes [`Event::ApprovalRequested`] on its session
//! and waits for [`ApprovalAnswer`]. Only a session somebody watches over
//! `gui/events` is asked; any other turn runs with no approval callback, as
//! the window's turn always has.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::a2a::server::gui_stream;
use crate::brain::agent::{AgentError, AgentService, ApprovalCallback, ToolApprovalInfo};
use crate::db::models::Message;

use super::{BackendError, Ctx, Event, Op, Scope};

ops! { TurnSend, TurnResume, ApprovalAnswer }

/// A turn that finished with an answer.
///
/// `None` in its place means the stop dropped the turn where it stood
/// (#2657); a turn that ran and failed answers `BackendError::Failed` with the
/// text the channels show.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnOutcome {
    /// The answer as the card draws it.
    pub content: String,
    /// Prompt tokens the model saw on the final call.
    pub context_tokens: u32,
    /// The service's own output rate for this turn, when it measured one.
    pub tokens_per_second: Option<f64>,
    /// Whether this turn's own input was a recording (#3096).
    pub voice: bool,
    /// The checklist this turn finished, when it finished one (#3142).
    pub completed_plan: Option<Message>,
}

/// Run one turn on `session_id`: the text the model reads, the line history
/// keeps when it differs (#2756), and whether the input was a recording.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnSend {
    pub session_id: Uuid,
    pub content: String,
    pub display: Option<String>,
    pub voice: bool,
    /// The stop. Only the local door can fire it; a remote caller gets a
    /// token nobody holds and stops its turn another way.
    #[serde(skip)]
    pub cancel: CancellationToken,
}

/// Re-run one turn a previous process died inside (#2627), from `content`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnResume {
    pub session_id: Uuid,
    pub content: String,
    /// The stop, local only, as on [`TurnSend`].
    #[serde(skip)]
    pub cancel: CancellationToken,
}

/// The TUI's `send_message_with_tools_and_mode` call with the
/// queue-completion handshake the TUI does around it.
///
/// `display` is the row the user should read for this turn: the receipt of a
/// command, or a channel's clean rendering of a wrapped message. The model
/// always receives `content`; history gets `display` when one is supplied
/// (#2756).
impl Op for TurnSend {
    const NAME: &'static str = "turn.send";
    const SCOPE: Scope = Scope::Send;
    type Reply = Option<TurnOutcome>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let TurnSend {
            session_id,
            content,
            display,
            voice,
            cancel,
        } = self;
        let service = ctx.service()?.clone();
        // One window turn per session: a second one is refused, never
        // interleaved. A channel turn on the same session does not refuse it,
        // as before this door existed (see `Ctx::claim_own_turn`).
        let _claim = ctx.claim_own_turn(session_id)?;
        // Before the turn writes its rows: a resend of the text an
        // unanswered turn left behind must not stack a second copy of
        // it in history (#2697).
        // Compare against the row history will actually hold, which is
        // the display line when a turn hides its prompt (#2756).
        let persisted = display.as_deref().unwrap_or(&content);
        let messages = crate::services::MessageService::new(service.context().clone());
        drop_unanswered_duplicate(&messages, session_id, persisted).await;
        let turn = service.send_message_with_tools_and_display(
            session_id,
            content,
            display,
            None,
            Some(cancel.clone()),
            remote_approval(&service, session_id),
            None,
            "tui",
            None,
            None,
        );
        // A cooperative token only lands where the service looks for
        // it, and inside one provider stream it never looks (#2657).
        let Some(outcome) = race_turn(turn, &cancel).await else {
            return Ok(None);
        };
        match outcome {
            Ok(response) => {
                // A text turn never carries a spoken block (#3096). The
                // model is told to write one only when the message
                // begins with the voice note, and a model that drifts
                // must not print it into the card or hand it to any
                // later reader. Stripped here, before the settle draws
                // the answer and before anything can speak it.
                let content = if voice {
                    response.content
                } else {
                    crate::channels::voice::spoken::visible(&response.content)
                };
                Ok(Some(TurnOutcome {
                    content,
                    context_tokens: response.context_tokens,
                    tokens_per_second: response.tokens_per_second,
                    voice,
                    completed_plan: response.completed_plan,
                }))
            }
            Err(e) => {
                let msg = crate::brain::agent::format_user_error(&e);
                Err(BackendError::Failed(msg))
            }
        }
    }
}

/// `resume_interrupted_turn`, not `send_message_with_tools_and_mode`: a resume
/// is a one-shot recovery and must never re-track itself. If this turn is
/// interrupted too (a stop, another crash) the session has already been
/// cleared from `pending_requests`, so it cannot resume forever (#729).
///
/// The boot summary is not counted here: it is the accounting of the launch
/// that found the row, and that surface counts the outcome it was handed.
impl Op for TurnResume {
    const NAME: &'static str = "turn.resume";
    const SCOPE: Scope = Scope::Send;
    type Reply = Option<TurnOutcome>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let TurnResume {
            session_id,
            content,
            cancel,
        } = self;
        let service = ctx.service()?.clone();
        // Only this crab's own turn refuses a resume (`Ctx::claim_own_turn`).
        let _claim = ctx.claim_own_turn(session_id)?;
        let turn = service.resume_interrupted_turn(
            session_id,
            content,
            None,
            Some(cancel.clone()),
            remote_approval(&service, session_id),
            None,
            "tui",
            None,
        );
        // A resumed turn is a turn: the same stop applies, and the
        // same cooperative gap (#2657).
        let Some(outcome) = race_turn(turn, &cancel).await else {
            return Ok(None);
        };
        match outcome {
            Ok(response) => Ok(Some(TurnOutcome {
                content: response.content,
                context_tokens: response.context_tokens,
                tokens_per_second: response.tokens_per_second,
                // A resumed turn re-runs a prompt parked by a dead
                // process, and that prompt is the words only: the
                // recording it came from is not in hand here. It
                // answers in text rather than guessing at speech
                // (#3096).
                voice: false,
                completed_plan: response.completed_plan,
            })),
            Err(e) => {
                let msg = crate::brain::agent::format_user_error(&e);
                Err(BackendError::Failed(msg))
            }
        }
    }
}

/// Race a turn against its stop token (#2657).
///
/// The service reads the token cooperatively: at tool-iteration boundaries,
/// and once a streaming call has returned. Inside one long provider stream
/// neither checkpoint runs, so a stop was invisible until the answer finished
/// (`turn cancelled by Esc`, then `turn done` tens of seconds later). Racing
/// the whole turn is the window's equivalent of the terminal's
/// `handle.abort()`: when the token fires the future is dropped where it
/// stands, which runs the service's own drop guards and keeps the text it
/// already streamed.
///
/// Biased on the token, so a stop that lands in the same poll as the answer
/// wins. The user asked for the turn to be gone; painting an answer over their
/// cancellation is what makes a stop look like it did nothing.
///
/// `None` means the stop fired and the turn was dropped. `Some(output)` is the
/// turn's own result, untouched.
pub async fn race_turn<F>(turn: F, cancel: &CancellationToken) -> Option<F::Output>
where
    F: std::future::Future,
{
    tokio::pin!(turn);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        output = &mut turn => Some(output),
    }
}

/// Cut the trailing unanswered pair before a resend duplicates it (#2697).
///
/// A turn writes its `user` row and an empty `assistant` placeholder at the
/// moment it starts, before the provider is asked. A stop that lands after the
/// first token cannot hand the text back (the `untouched()` hand-back only
/// covers a stop that beat the token), so that pair stays behind with no
/// answer, and sending the same text again appended a second copy of it.
///
/// The pair is cut only when it is provably leftover: the newest row is an
/// `assistant` row whose content is still empty, and the row before it is a
/// `user` row carrying exactly the outgoing text. Anything else ends the walk.
/// A real answer, a turn that ran a tool (tool activity lives on the assistant
/// row, so an empty one answered nothing), and a different question are
/// history, and history is never touched.
pub async fn drop_unanswered_duplicate(
    messages: &crate::services::MessageService,
    session_id: Uuid,
    outgoing: &str,
) {
    let rows = match messages
        .list_recent_messages_for_session(session_id, 2)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("gui dedup read: {e}");
            return;
        }
    };
    let Some(placeholder) = rows.last() else {
        return;
    };
    if placeholder.role != "assistant" || !placeholder.content.trim().is_empty() {
        return;
    }
    let Some(question) = rows.get(rows.len().saturating_sub(2)) else {
        return;
    };
    if question.role != "user" {
        return;
    }
    // The ledger keeps the redacted form of the text the turn was given.
    let stored = crate::utils::sanitize::redact_secrets(outgoing);
    if question.content.trim() != stored.trim() {
        return;
    }
    for row in [question, placeholder] {
        if let Err(e) = messages.delete_message(row.id).await {
            tracing::error!("gui dedup delete: {e}");
            return;
        }
    }
    tracing::info!(
        "gui dedup: dropped the unanswered pair a resend would have duplicated \
         (session {session_id})"
    );
}

/// How long a remote approval request waits for its answer. Past this the
/// tool is denied, so a surface that went quiet cannot hold a turn forever.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// The answer to one [`Event::ApprovalRequested`].
///
/// `request_id` is the one the event carried and `session_id` the session it
/// named: an answer for a request that is not pending on that session reaches
/// nothing. `always` approves this call and every later tool of the same
/// turn, the "always" of the window's own prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalAnswer {
    pub session_id: Uuid,
    pub request_id: Uuid,
    pub approved: bool,
    pub always: bool,
}

/// `true` when a waiting tool took the answer; `false` when the request had
/// already been answered, ran out of time, or never existed.
impl Op for ApprovalAnswer {
    const NAME: &'static str = "turn.approval_answer";
    const SCOPE: Scope = Scope::Send;
    type Reply = bool;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let ApprovalAnswer {
            session_id,
            request_id,
            approved,
            always,
        } = self;
        Ok(answer_approval(session_id, request_id, (approved, always)))
    }
}

/// One approval request waiting for its answer.
struct Pending {
    session_id: Uuid,
    answer: oneshot::Sender<(bool, bool)>,
}

/// Every approval request of this process still waiting, by `request_id`.
/// Global for the same reason the turn probes are: the answer arrives on a
/// `gui/call` that holds nothing of the turn it answers.
static PENDING: LazyLock<Mutex<HashMap<Uuid, Pending>>> = LazyLock::new(Mutex::default);

/// The pending map. A holder that panicked left the map itself intact, so a
/// poisoned lock is read through.
fn pending() -> MutexGuard<'static, HashMap<Uuid, Pending>> {
    PENDING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Hand `answer` to the request `request_id` of `session_id`, if it waits.
fn answer_approval(session_id: Uuid, request_id: Uuid, answer: (bool, bool)) -> bool {
    use std::collections::hash_map::Entry;

    match pending().entry(request_id) {
        Entry::Occupied(entry) if entry.get().session_id == session_id => {
            entry.remove().answer.send(answer).is_ok()
        }
        _ => false,
    }
}

/// Removes its request from [`PENDING`] however the wait ends: answered,
/// timed out, or dropped with the turn by a stop.
struct PendingGuard(Uuid);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        pending().remove(&self.0);
    }
}

/// The approval callback a turn on `session_id` runs with.
///
/// `None`, the window's turn as it always was, unless a remote surface is
/// watching the session and the service's policy does not already approve
/// every tool. A per-call callback makes the tool loop ask even under an
/// auto policy, so an auto policy keeps the turn without one.
fn remote_approval(service: &AgentService, session_id: Uuid) -> Option<ApprovalCallback> {
    if service.auto_approves_tools() || !gui_stream::hub().is_watched(session_id) {
        return None;
    }
    Some(approval_callback(APPROVAL_TIMEOUT))
}

/// A callback that asks the session's remote surfaces and waits `timeout`.
fn approval_callback(timeout: Duration) -> ApprovalCallback {
    Arc::new(move |info| Box::pin(ask_remote(info, timeout)))
}

/// Publish the request on the tool's session and wait for its answer.
///
/// Nobody watching, or no answer within `timeout`, is an error the tool loop
/// turns into a denied tool result. Its text names the policy and the two
/// ways out, as the denial of a surface with no approval at all does.
async fn ask_remote(
    info: ToolApprovalInfo,
    timeout: Duration,
) -> crate::brain::agent::Result<(bool, bool)> {
    let ToolApprovalInfo {
        session_id,
        tool_name,
        tool_description,
        tool_input,
        capabilities,
    } = info;
    let request_id = Uuid::new_v4();
    let (tx, rx) = oneshot::channel();
    pending().insert(
        request_id,
        Pending {
            session_id,
            answer: tx,
        },
    );
    let _pending = PendingGuard(request_id);
    let asked = gui_stream::publish(&Event::ApprovalRequested {
        session_id,
        request_id,
        tool_name: tool_name.clone(),
        tool_description,
        tool_input,
        capabilities,
    });
    if asked == 0 {
        return Err(unanswered(&tool_name, "no remote surface is watching"));
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(_)) | Err(_) => Err(unanswered(
            &tool_name,
            &format!("no remote surface answered within {}s", timeout.as_secs()),
        )),
    }
}

/// The denial of a remote approval nobody gave.
fn unanswered(tool_name: &str, why: &str) -> AgentError {
    tracing::warn!("Tool '{tool_name}' requires approval: {why}");
    AgentError::Internal(format!(
        "Tool '{tool_name}' requires approval. The current `agent.approval_policy` \
         does not auto-approve, and {why}. Set `agent.approval_policy` to \
         \"auto-always\", or re-run with `--auto-approve`."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};
    use crate::db::models::Session;
    use crate::db::repository::SessionRepository;
    use crate::services::ServiceContext;

    /// A gateway whose crab runs turns on the placeholder provider: every
    /// call fails with one fixed text, so both doors answer the same.
    async fn booted() -> Gateway {
        let gateway = Gateway::start().await;
        let db = gateway.ctx.db().await.expect("db");
        let provider: Arc<dyn crate::brain::provider::Provider> =
            Arc::new(crate::brain::provider::PlaceholderProvider);
        let service =
            AgentService::new_for_test(provider, ServiceContext::new(db.pool().clone())).await;
        gateway.ctx.set_service(Arc::new(service));
        gateway
    }

    async fn session(gateway: &Gateway) -> Uuid {
        let db = gateway.ctx.db().await.expect("db");
        let session = Session::new(Some("turn".into()), None, None);
        SessionRepository::new(db.pool().clone())
            .create(&session)
            .await
            .expect("create session");
        session.id
    }

    fn send(session_id: Uuid) -> TurnSend {
        TurnSend {
            session_id,
            content: "hello".into(),
            display: None,
            voice: false,
            cancel: CancellationToken::new(),
        }
    }

    fn resume(session_id: Uuid) -> TurnResume {
        TurnResume {
            session_id,
            content: "continue".into(),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn turn_send_has_parity() {
        let gateway = booted().await;
        let id = session(&gateway).await;
        let answer = parity!(
            gateway,
            TurnSend {
                session_id: id,
                content: "hello".into(),
                display: None,
                voice: false,
                cancel: CancellationToken::new(),
            }
        );
        assert!(answer.get("Err").is_some(), "{answer}");
        forbidden!(
            gateway,
            TurnSend {
                session_id: id,
                content: "hello".into(),
                display: None,
                voice: false,
                cancel: CancellationToken::new(),
            }
        );
    }

    #[tokio::test]
    async fn turn_resume_has_parity() {
        let gateway = booted().await;
        let id = session(&gateway).await;
        let answer = parity!(
            gateway,
            TurnResume {
                session_id: id,
                content: "continue".into(),
                cancel: CancellationToken::new(),
            }
        );
        assert!(answer.get("Err").is_some(), "{answer}");
        forbidden!(
            gateway,
            TurnResume {
                session_id: id,
                content: "continue".into(),
                cancel: CancellationToken::new(),
            }
        );
    }

    #[tokio::test]
    async fn a_turn_on_a_busy_session_is_refused_by_both_doors() {
        let gateway = booted().await;
        let id = session(&gateway).await;
        let _held = gateway.ctx.claim_turn(id).expect("an idle session");
        let answer = parity!(
            gateway,
            TurnSend {
                session_id: id,
                content: "hello".into(),
                display: None,
                voice: false,
                cancel: CancellationToken::new(),
            }
        );
        assert_eq!(answer["Err"]["kind"], "busy", "{answer}");
        let answer = parity!(
            gateway,
            TurnResume {
                session_id: id,
                content: "continue".into(),
                cancel: CancellationToken::new(),
            }
        );
        assert_eq!(answer["Err"]["kind"], "busy", "{answer}");
    }

    #[tokio::test]
    async fn a_channel_mid_turn_does_not_refuse_a_window_send() {
        use crate::brain::agent::service::session_routes::register_turn_probe;

        let gateway = booted().await;
        let id = session(&gateway).await;
        register_turn_probe(id, Arc::new(|| true));
        let result = gateway.local(send(id)).await;
        assert!(
            !matches!(result, Err(BackendError::Busy)),
            "another owner's turn must not refuse a window send"
        );
    }

    #[tokio::test]
    async fn the_crabs_own_turn_refuses_a_window_send() {
        let gateway = booted().await;
        let id = session(&gateway).await;
        let _held = gateway.ctx.claim_own_turn(id).expect("an idle session");
        assert!(matches!(
            gateway.local(send(id)).await,
            Err(BackendError::Busy)
        ));
    }

    #[tokio::test]
    async fn a_turn_frees_its_session_when_it_ends() {
        let gateway = booted().await;
        let id = session(&gateway).await;
        let _ = gateway.local(send(id)).await;
        assert!(!gateway.ctx.turn_in_flight(id));
        assert!(gateway.ctx.claim_turn(id).is_ok());
    }

    #[tokio::test]
    async fn a_fired_stop_drops_the_turn() {
        let gateway = booted().await;
        let id = session(&gateway).await;
        let op = send(id);
        op.cancel.cancel();
        assert!(matches!(gateway.local(op).await, Ok(None)));
        let op = resume(id);
        op.cancel.cancel();
        assert!(matches!(gateway.local(op).await, Ok(None)));
    }

    #[tokio::test]
    async fn a_failed_turn_answers_the_text_the_channels_show() {
        let gateway = booted().await;
        let id = session(&gateway).await;
        let Err(BackendError::Failed(text)) = gateway.local(send(id)).await else {
            panic!("the placeholder provider fails every turn");
        };
        assert!(!text.is_empty());
    }

    #[tokio::test]
    async fn approval_answer_has_parity() {
        let gateway = booted().await;
        let id = session(&gateway).await;
        let answer = parity!(
            gateway,
            ApprovalAnswer {
                session_id: id,
                request_id: Uuid::new_v4(),
                approved: true,
                always: false,
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": false }), "{answer}");
        forbidden!(
            gateway,
            ApprovalAnswer {
                session_id: id,
                request_id: Uuid::new_v4(),
                approved: true,
                always: false,
            }
        );
    }

    #[test]
    fn the_approval_answer_fixture_decodes() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/gui_backend/ApprovalAnswer.json"
        ))
        .expect("fixture is JSON");
        assert_eq!(fixture["op"], ApprovalAnswer::NAME);
        assert_eq!(fixture["scope"], ApprovalAnswer::SCOPE.as_str());
        let _: ApprovalAnswer =
            serde_json::from_value(fixture["params"].clone()).expect("params decode");
        let _: Result<bool, BackendError> =
            serde_json::from_value(fixture["reply"].clone()).expect("reply decodes");
    }

    /// A tool that needs approval and counts the times it ran.
    struct GatedTool {
        ran: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::brain::tools::Tool for GatedTool {
        fn name(&self) -> &str {
            "approval_tool"
        }

        fn description(&self) -> &str {
            "A tool that requires approval"
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }

        fn capabilities(&self) -> Vec<crate::brain::tools::ToolCapability> {
            Vec::new()
        }

        fn requires_approval(&self) -> bool {
            true
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &crate::brain::tools::ToolExecutionContext,
        ) -> crate::brain::tools::Result<crate::brain::tools::ToolResult> {
            self.ran.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::brain::tools::ToolResult::success("ran".to_string()))
        }
    }

    /// A gateway whose crab runs on `provider` with [`GatedTool`] and the
    /// "ask" policy of `new_for_test`. Returns the tool's run counter.
    async fn gated(
        provider: Arc<dyn crate::brain::provider::Provider>,
    ) -> (Gateway, Arc<std::sync::atomic::AtomicUsize>) {
        let gateway = Gateway::start().await;
        let db = gateway.ctx.db().await.expect("db");
        let ran = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry = crate::brain::tools::ToolRegistry::new();
        registry.register(Arc::new(GatedTool { ran: Arc::clone(&ran) }));
        let service = AgentService::new_for_test(provider, ServiceContext::new(db.pool().clone()))
            .await
            .with_tool_registry(Arc::new(registry));
        assert!(!service.auto_approves_tools(), "the test needs a gating policy");
        gateway.ctx.set_service(Arc::new(service));
        (gateway, ran)
    }

    /// The next approval request on `sub`, skipping any other event.
    async fn next_request(sub: &mut gui_stream::Subscription) -> (Uuid, Uuid, String) {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(30), sub.next())
                .await
                .expect("an approval request within 30s")
                .expect("the session channel stays open");
            if let Event::ApprovalRequested {
                session_id,
                request_id,
                tool_name,
                ..
            } = event
            {
                return (session_id, request_id, tool_name);
            }
        }
    }

    /// Watch `id`, run one turn on it, and answer its first approval request
    /// by the remote door with `approved` and `always`. Returns the turn's
    /// result and the subscription, which saw that one request.
    async fn turn_answered(
        gateway: &Gateway,
        id: Uuid,
        approved: bool,
        always: bool,
    ) -> (
        Result<Option<TurnOutcome>, BackendError>,
        gui_stream::Subscription,
    ) {
        let mut sub = gui_stream::hub().subscribe(id);
        let answer = async {
            let (session_id, request_id, tool_name) = next_request(&mut sub).await;
            assert_eq!(session_id, id);
            assert_eq!(tool_name, "approval_tool");
            let took = gateway
                .remote(
                    ApprovalAnswer {
                        session_id,
                        request_id,
                        approved,
                        always,
                    },
                    Scope::ALL,
                )
                .await;
            assert_eq!(took, Ok(true), "the waiting tool took the answer");
        };
        let (result, ()) = tokio::join!(gateway.local(send(id)), answer);
        (result, sub)
    }

    #[tokio::test]
    async fn a_watched_turn_asks_and_a_no_denies_the_tool() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let (gateway, ran) = gated(Arc::new(MockProviderWithNamedTool::new("approval_tool"))).await;
        let id = session(&gateway).await;
        let (result, _sub) = turn_answered(&gateway, id, false, false).await;
        assert!(matches!(result, Ok(Some(_))), "{result:?}");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0, "a no ran the tool");
    }

    #[tokio::test]
    async fn a_watched_turn_asks_and_a_yes_runs_the_tool() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let (gateway, ran) = gated(Arc::new(MockProviderWithNamedTool::new("approval_tool"))).await;
        let id = session(&gateway).await;
        let (result, _sub) = turn_answered(&gateway, id, true, false).await;
        assert!(matches!(result, Ok(Some(_))), "{result:?}");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn always_approves_the_rest_of_the_turn_without_asking() {
        use crate::tests::agent_service_mocks::MockProviderWithTwoToolCalls;

        let provider = MockProviderWithTwoToolCalls::new("approval_tool", "approval_tool");
        let (gateway, ran) = gated(Arc::new(provider)).await;
        let id = session(&gateway).await;
        let (result, mut sub) = turn_answered(&gateway, id, true, true).await;
        assert!(matches!(result, Ok(Some(_))), "{result:?}");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 2);
        let more = tokio::time::timeout(Duration::from_millis(200), sub.next()).await;
        assert!(more.is_err(), "a second request reached the subscriber: {more:?}");
    }

    #[tokio::test]
    async fn an_unwatched_turn_gets_no_approval_callback() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let (gateway, ran) = gated(Arc::new(MockProviderWithNamedTool::new("approval_tool"))).await;
        let id = session(&gateway).await;
        let service = gateway.ctx.service().expect("service");
        assert!(remote_approval(service, id).is_none());
        let result = gateway.local(send(id)).await;
        assert!(matches!(result, Ok(Some(_))), "{result:?}");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0, "denied, as before");
    }

    fn info(session_id: Uuid) -> ToolApprovalInfo {
        ToolApprovalInfo {
            session_id,
            tool_name: "approval_tool".into(),
            tool_description: "A tool that requires approval".into(),
            tool_input: serde_json::json!({}),
            capabilities: Vec::new(),
        }
    }

    #[tokio::test]
    async fn an_unanswered_request_is_denied_and_forgotten() {
        let id = Uuid::new_v4();
        let mut sub = gui_stream::hub().subscribe(id);
        let err = ask_remote(info(id), Duration::from_millis(50))
            .await
            .expect_err("nobody answered");
        let text = err.to_string();
        assert!(text.contains("Tool 'approval_tool' requires approval"), "{text}");
        assert!(text.contains("agent.approval_policy"), "{text}");
        let (_, request_id, _) = next_request(&mut sub).await;
        assert!(
            !answer_approval(id, request_id, (true, false)),
            "a late answer reached a request that already gave up"
        );
    }

    #[tokio::test]
    async fn a_request_nobody_watches_is_denied_at_once() {
        let err = ask_remote(info(Uuid::new_v4()), APPROVAL_TIMEOUT)
            .await
            .expect_err("nobody to ask");
        assert!(err.to_string().contains("no remote surface is watching"));
    }

    #[tokio::test]
    async fn an_answer_for_another_session_reaches_nothing() {
        let id = Uuid::new_v4();
        let mut sub = gui_stream::hub().subscribe(id);
        let ask = ask_remote(info(id), Duration::from_secs(30));
        let answer = async {
            let (_, request_id, _) = next_request(&mut sub).await;
            assert!(!answer_approval(Uuid::new_v4(), request_id, (true, true)));
            assert!(answer_approval(id, request_id, (false, false)));
        };
        let (asked, ()) = tokio::join!(ask, answer);
        assert_eq!(asked.expect("answered"), (false, false));
    }

    #[test]
    fn the_stop_never_crosses_the_wire() {
        let op = send(Uuid::new_v4());
        op.cancel.cancel();
        let wire = serde_json::to_value(&op).expect("encode");
        assert!(wire.get("cancel").is_none(), "{wire}");
        let back: TurnSend = serde_json::from_value(wire).expect("decode");
        assert!(!back.cancel.is_cancelled());
    }

    #[test]
    fn an_outcome_survives_the_wire() {
        let outcome = TurnOutcome {
            content: "done".into(),
            context_tokens: 12,
            tokens_per_second: Some(3.5),
            voice: true,
            completed_plan: None,
        };
        let wire = serde_json::to_string(&Some(outcome)).expect("encode");
        let back: Option<TurnOutcome> = serde_json::from_str(&wire).expect("decode");
        let back = back.expect("an answer");
        assert_eq!(back.content, "done");
        assert_eq!(back.context_tokens, 12);
        assert_eq!(back.tokens_per_second, Some(3.5));
        assert!(back.voice);
    }
}
