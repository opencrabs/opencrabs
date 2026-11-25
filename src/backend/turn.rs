//! Starting, resuming and steering a turn.
//!
//! A turn is the crab's work: the dedup of a resent question, the call into
//! the agent service, the race against the stop and the shape of the answer
//! all run here. The surface keeps only what is its own: the event sink it
//! settles the card through and the boot accounting of its own launch.
//!
//! The progress of a running turn does not travel on the reply. The window's
//! own turn reaches it through the service's own forwarder; a remote turn
//! publishes it as [`Event::Turn`] on its session's `gui/events` stream. The
//! reply is only the terminal outcome.
//!
//! A tool that needs approval asks a remote surface the way a channel asks
//! its chat: the turn publishes [`Event::ApprovalRequested`] on its session
//! and waits for [`ApprovalAnswer`]. Only a turn that came in by the remote
//! door (`gui/call`, see [`super::door`]) from a caller holding
//! `a2a:manage` asks, and only that caller's principal answers. The
//! window's turn runs with no approval callback, as it always has, whoever
//! watches the session.
//!
//! A remote turn is the remote caller's, not the window's: the server picks
//! its history line and its channel, it waits for any turn on the session,
//! and only the principal that opened it stops it ([`TurnCancel`]). Its voice
//! is the caller's: it only decides whether the answer is spoken back to the
//! caller that recorded the question.
//! A resume is the crab's own recovery and only the local door runs it.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::a2a::server::gui_stream;
use crate::brain::agent::{
    AgentError, AgentService, ApprovalCallback, ProgressCallback, ToolApprovalInfo,
};
use crate::db::models::Message;

use super::door::{self, Caller, Door};
use super::{BackendError, Ctx, Event, Op, Scope};

ops! { TurnSend, TurnResume, ApprovalAnswer, TurnCancel }

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
    /// The stop. Only the local door hands one in; a remote caller gets a
    /// fresh token, registered for its session, and fires it by
    /// [`TurnCancel`].
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
        let caller = door::caller();
        let (display, voice, channel) = at_door(caller.as_ref(), display, voice);
        let service = ctx.service()?.clone();
        // One window turn per session: a second one is refused, never
        // interleaved. A channel turn on the same session does not refuse it,
        // as before this door existed (see `Ctx::claim_own_turn`). A remote
        // turn keeps no such privilege: any turn mid-way refuses it.
        let _claim = match &caller {
            Some(_) => ctx.claim_turn(session_id)?,
            None => ctx.claim_own_turn(session_id)?,
        };
        // Only the principal that opened a remote turn stops it.
        let _running = caller
            .as_ref()
            .map(|caller| register_running(session_id, &caller.principal, &cancel));
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
            remote_approval(&service, crate::utils::check_approval_policy),
            caller.as_ref().map(|_| stream_progress()),
            channel,
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
        // A resume re-runs a prompt a dead process of this crab parked: the
        // crab's own recovery, never a remote caller's to start. No wire
        // scope carries `a2a:admin`, so the refusal names it.
        if door::current() == Door::Remote {
            return Err(BackendError::Forbidden {
                scope: Scope::Admin,
            });
        }
        let service = ctx.service()?.clone();
        // Only this crab's own turn refuses a resume (`Ctx::claim_own_turn`).
        let _claim = ctx.claim_own_turn(session_id)?;
        let turn = service.resume_interrupted_turn(
            session_id,
            content,
            None,
            Some(cancel.clone()),
            remote_approval(&service, crate::utils::check_approval_policy),
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

/// A turn's `display`, `voice` and channel as its door decides them.
///
/// The window's turn keeps what it asked for and runs as `tui`. A remote
/// turn does not choose its history line or its channel: history keeps the
/// words the model read, and the turn runs as `a2a`, so nothing a channel
/// policy grants the window reaches it. Its `voice` is kept: a window looking
/// at this crab records the question and is answered aloud, and the flag
/// only decides whether the answer keeps its spoken block for that caller
/// to have it spoken (`voice.speak`, scope `send` like this turn).
fn at_door(
    caller: Option<&Caller>,
    display: Option<String>,
    voice: bool,
) -> (Option<String>, bool, &'static str) {
    match caller {
        Some(_) => (None, voice, "a2a"),
        None => (display, voice, "tui"),
    }
}

/// Stop the remote turn running on `session_id`.
///
/// Scope `send`, like starting one. Only the principal that opened the turn
/// stops it: another caller is refused and the turn goes on. At the local
/// door the crab stops any of its sessions' remote turns. The reply is
/// `true` when a turn was running and its stop fired, `false` when the
/// session had no remote turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnCancel {
    pub session_id: Uuid,
}

impl Op for TurnCancel {
    const NAME: &'static str = "turn.cancel";
    const SCOPE: Scope = Scope::Send;
    type Reply = bool;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let from = door::caller().map(|caller| caller.principal);
        cancel_running(self.session_id, from.as_deref())
    }
}

/// One remote turn running, with the principal that opened it.
struct Running {
    turn: Uuid,
    principal: String,
    cancel: CancellationToken,
}

/// Every remote turn of this process still running, by session. Global for
/// the reason [`PENDING`] is: the stop arrives on a `gui/call` that holds
/// nothing of the turn it stops.
static RUNNING: LazyLock<Mutex<HashMap<Uuid, Running>>> = LazyLock::new(Mutex::default);

/// The running map, read through a poisoned lock as [`pending`] is.
fn running() -> MutexGuard<'static, HashMap<Uuid, Running>> {
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Removes its turn from [`RUNNING`] however the turn ends.
struct RunningGuard {
    session_id: Uuid,
    turn: Uuid,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let mut running = running();
        if running
            .get(&self.session_id)
            .is_some_and(|entry| entry.turn == self.turn)
        {
            running.remove(&self.session_id);
        }
    }
}

/// The principal of each session's running or last remote turn. Unlike
/// [`RUNNING`] an entry outlives its turn: the voice of an answer is asked
/// for after the turn settled, by the principal that opened it.
static TURN_PRINCIPAL: LazyLock<Mutex<HashMap<Uuid, String>>> = LazyLock::new(Mutex::default);

/// The principal map, read through a poisoned lock as [`pending`] is.
fn turn_principals() -> MutexGuard<'static, HashMap<Uuid, String>> {
    TURN_PRINCIPAL
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The principal that opened the remote turn running on `session_id`, or
/// the last one that ran there. `None` when no remote turn has run on it in
/// this process.
pub(crate) fn turn_principal(session_id: Uuid) -> Option<String> {
    turn_principals().get(&session_id).cloned()
}

/// Record `principal` as the one whose remote turn `session_id` runs.
fn note_turn_principal(session_id: Uuid, principal: &str) {
    turn_principals().insert(session_id, principal.to_string());
}

/// Register the remote turn on `session_id` that `principal` opened, with
/// the stop it runs under.
fn register_running(
    session_id: Uuid,
    principal: &str,
    cancel: &CancellationToken,
) -> RunningGuard {
    note_turn_principal(session_id, principal);
    let turn = Uuid::new_v4();
    running().insert(
        session_id,
        Running {
            turn,
            principal: principal.to_string(),
            cancel: cancel.clone(),
        },
    );
    RunningGuard { session_id, turn }
}

/// Fire the stop of the remote turn on `session_id` for `from`, the asking
/// principal (`None` at the local door, which stops any).
fn cancel_running(session_id: Uuid, from: Option<&str>) -> Result<bool, BackendError> {
    let cancel = {
        let running = running();
        let Some(turn) = running.get(&session_id) else {
            return Ok(false);
        };
        if let Some(from) = from
            && from != turn.principal
        {
            return Err(BackendError::Forbidden {
                scope: Scope::Admin,
            });
        }
        turn.cancel.clone()
    };
    cancel.cancel();
    Ok(true)
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
///
/// Scope `manage`: approving runs a tool on the crab, more than sending a
/// turn does. A remote answer counts only from the principal whose turn
/// asked; any other is refused and the request keeps waiting.
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
    const SCOPE: Scope = Scope::Manage;
    type Reply = bool;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let ApprovalAnswer {
            session_id,
            request_id,
            approved,
            always,
        } = self;
        let from = door::caller().map(|caller| caller.principal);
        answer_from(session_id, request_id, (approved, always), from.as_deref())
    }
}

/// One approval request waiting for its answer.
struct Pending {
    session_id: Uuid,
    /// The principal whose remote turn asked. `None` answers to anyone.
    principal: Option<String>,
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

/// A remote turn on `session_id` opened by `principal` and already over, for
/// the tests of what that turn grants.
#[cfg(test)]
pub(crate) fn turn_ran_for(session_id: Uuid, principal: &str) {
    note_turn_principal(session_id, principal);
}

/// Hand `answer` to the request `request_id` of `session_id` at the local
/// door, if it waits.
#[cfg(test)]
fn answer_approval(session_id: Uuid, request_id: Uuid, answer: (bool, bool)) -> bool {
    answer_from(session_id, request_id, answer, None).unwrap_or(false)
}

/// Hand `answer` from `from` (the remote principal, `None` at the local
/// door) to the request `request_id` of `session_id`, if it waits.
///
/// A remote principal that is not the one whose turn asked is refused, and
/// the request stays pending for the right one.
fn answer_from(
    session_id: Uuid,
    request_id: Uuid,
    answer: (bool, bool),
    from: Option<&str>,
) -> Result<bool, BackendError> {
    use std::collections::hash_map::Entry;

    match pending().entry(request_id) {
        Entry::Occupied(entry) if entry.get().session_id == session_id => {
            if let Some(from) = from
                && entry
                    .get()
                    .principal
                    .as_deref()
                    .is_some_and(|owner| owner != from)
            {
                return Err(BackendError::Forbidden {
                    scope: Scope::Admin,
                });
            }
            Ok(entry.remove().answer.send(answer).is_ok())
        }
        _ => Ok(false),
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

/// A read of the configured approval policy: `Some` grants the tool with
/// that answer and asks nobody, `None` leaves the call to a human.
type PolicyGrant = fn() -> Option<(bool, bool)>;

/// A remote turn's progress (the streamed answer, the reasoning, the tool
/// calls), handed to every surface watching its session on `gui/events`.
///
/// The window's own turn keeps the service's forwarder and passes no
/// override; a remote caller has no forwarder on this process, so without
/// this its card saw nothing until the reply settled it.
fn stream_progress() -> ProgressCallback {
    Arc::new(|session_id, progress| {
        gui_stream::publish(&Event::Turn {
            session_id,
            progress,
        });
    })
}

/// The approval callback a turn runs with.
///
/// `None`, the window's turn as it always was, unless the turn came in by
/// the remote door from a caller holding `a2a:manage`. A remote caller with
/// `send` alone keeps `None`, the window's path: a gated tool is denied. A
/// remote turn under a service that already approves every tool keeps
/// `None` too: a per-call callback makes the tool loop ask even then.
///
/// `policy` is the read a channel's callback makes before it asks its chat
/// (`check_approval_policy`, so an `auto-session` written after boot grants
/// here as it grants there). Only a policy that grants nothing asks.
fn remote_approval(service: &AgentService, policy: PolicyGrant) -> Option<ApprovalCallback> {
    let caller = door::caller()?;
    if !caller.holds(Scope::Manage) || service.auto_approves_tools() {
        return None;
    }
    Some(approval_callback(APPROVAL_TIMEOUT, policy, caller.principal))
}

/// A callback that grants what `policy` grants, and asks the session's
/// remote surfaces, waiting `timeout`, for the rest. Only `principal`
/// answers what it asks.
fn approval_callback(
    timeout: Duration,
    policy: PolicyGrant,
    principal: String,
) -> ApprovalCallback {
    Arc::new(move |info| {
        let principal = principal.clone();
        Box::pin(async move {
            match policy() {
                Some(granted) => Ok(granted),
                None => ask_for(info, timeout, Some(principal)).await,
            }
        })
    })
}

/// [`ask_for`] a request any door may answer.
#[cfg(test)]
async fn ask_remote(
    info: ToolApprovalInfo,
    timeout: Duration,
) -> crate::brain::agent::Result<(bool, bool)> {
    ask_for(info, timeout, None).await
}

/// Publish the request on the tool's session and wait for its answer from
/// `principal` (or the local door).
///
/// Nobody watching, or no answer within `timeout`, is an error the tool loop
/// turns into a denied tool result. Its text names the policy and the two
/// ways out, as the denial of a surface with no approval at all does.
async fn ask_for(
    info: ToolApprovalInfo,
    timeout: Duration,
    principal: Option<String>,
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
            principal,
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
        gated_with(provider, "ask").await
    }

    /// [`gated`], with `policy` written to the crab's `config.toml`, where a
    /// channel's callback reads it. The service itself still gates: the
    /// written policy is only what the callback finds on disk.
    async fn gated_with(
        provider: Arc<dyn crate::brain::provider::Provider>,
        policy: &str,
    ) -> (Gateway, Arc<std::sync::atomic::AtomicUsize>) {
        let gateway = Gateway::start().await;
        std::fs::write(
            gateway.home().join("config.toml"),
            format!("[agent]\napproval_policy = \"{policy}\"\n"),
        )
        .expect("write the crab's config");
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

    /// The first approval request `sub` sees within `wait`, skipping the
    /// turn's progress, which a remote turn streams to the same watchers.
    async fn request_within(
        sub: &mut gui_stream::Subscription,
        wait: Duration,
    ) -> Option<Event> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            match tokio::time::timeout_at(deadline, sub.next()).await {
                Ok(Some(event @ Event::ApprovalRequested { .. })) => return Some(event),
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => return None,
            }
        }
    }

    /// Watch `id`, run one turn on it by the remote door, and answer its
    /// first approval request by the remote door with `approved` and
    /// `always`. Returns the turn's result and the subscription, which saw
    /// that one request.
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
        let turn = door::remote(gateway.local(send(id)));
        let (result, ()) = tokio::join!(turn, answer);
        (result, sub)
    }

    #[tokio::test]
    async fn a_remote_turn_asks_and_a_no_denies_the_tool() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let (gateway, ran) = gated(Arc::new(MockProviderWithNamedTool::new("approval_tool"))).await;
        let id = session(&gateway).await;
        let (result, _sub) = turn_answered(&gateway, id, false, false).await;
        assert!(matches!(result, Ok(Some(_))), "{result:?}");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0, "a no ran the tool");
    }

    #[tokio::test]
    async fn a_remote_turn_asks_and_a_yes_runs_the_tool() {
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
        let more = request_within(&mut sub, Duration::from_millis(200)).await;
        assert!(more.is_none(), "a second request reached the subscriber: {more:?}");
    }

    /// The window's turn, with a remote surface watching its session: no
    /// request goes out and the tool is denied, exactly the path it always
    /// took. Watching a session is not a say in its local turns.
    #[tokio::test]
    async fn a_local_turn_never_asks_a_remote_watcher() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let (gateway, ran) = gated(Arc::new(MockProviderWithNamedTool::new("approval_tool"))).await;
        let id = session(&gateway).await;
        let mut sub = gui_stream::hub().subscribe(id);
        let service = gateway.ctx.service().expect("service");
        assert!(remote_approval(service, || None).is_none());
        let result = gateway.local(send(id)).await;
        assert!(matches!(result, Ok(Some(_))), "{result:?}");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0, "denied, as before");
        let asked = tokio::time::timeout(Duration::from_millis(200), sub.next()).await;
        assert!(asked.is_err(), "a local turn reached the watcher: {asked:?}");
    }

    /// Only the remote door gets the callback, and only while the policy
    /// leaves the call to a human.
    #[tokio::test]
    async fn only_a_remote_turn_gets_the_approval_callback() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let (gateway, _ran) =
            gated(Arc::new(MockProviderWithNamedTool::new("approval_tool"))).await;
        let service = gateway.ctx.service().expect("service");
        assert_eq!(door::current(), Door::Local);
        assert!(remote_approval(service, || None).is_none(), "local door");
        let remote = door::remote(async { remote_approval(service, || None).is_some() }).await;
        assert!(remote, "remote door with a gating service");
        service.set_auto_approve_tools(true);
        let granted = door::remote(async { remote_approval(service, || None).is_none() }).await;
        assert!(granted, "a service that approves every tool needs no callback");
    }

    /// `auto-session` written to config grants a remote turn's tool as it
    /// grants a channel's: the tool runs and nobody is asked.
    #[tokio::test]
    async fn a_remote_turn_under_an_auto_policy_runs_without_asking() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let provider = Arc::new(MockProviderWithNamedTool::new("approval_tool"));
        let (gateway, ran) = gated_with(provider, "auto-session").await;
        let id = session(&gateway).await;
        let mut sub = gui_stream::hub().subscribe(id);
        let result = door::remote(gateway.local(send(id))).await;
        assert!(matches!(result, Ok(Some(_))), "{result:?}");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1, "the policy granted it");
        let asked = request_within(&mut sub, Duration::from_millis(200)).await;
        assert!(asked.is_none(), "an auto policy still asked: {asked:?}");
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

    /// A remote caller named `principal` carrying `scopes`.
    fn as_caller(principal: &str, scopes: &[Scope]) -> Caller {
        Caller {
            principal: principal.to_string(),
            scopes: scopes.to_vec(),
        }
    }

    /// Every scope a credential carries over the wire: the owner's api_key.
    const WIRE: &[Scope] = &[Scope::Read, Scope::Send, Scope::Manage];

    /// The open loopback posture: read and send, nothing elevated.
    const LOOPBACK: &[Scope] = &[Scope::Read, Scope::Send];

    #[test]
    fn approving_a_tool_needs_manage() {
        assert_eq!(ApprovalAnswer::SCOPE, Scope::Manage);
    }

    #[tokio::test]
    async fn the_open_loopback_cannot_answer_an_approval() {
        let gateway = Gateway::start().await;
        let answer = gateway
            .remote(
                ApprovalAnswer {
                    session_id: Uuid::new_v4(),
                    request_id: Uuid::new_v4(),
                    approved: true,
                    always: true,
                },
                LOOPBACK,
            )
            .await;
        assert_eq!(
            answer,
            Err(BackendError::Forbidden {
                scope: Scope::Manage
            })
        );
    }

    /// A remote caller with `send` alone gets the window's path: no callback,
    /// no request, the gated tool denied.
    #[tokio::test]
    async fn a_send_only_remote_turn_never_asks_and_the_tool_is_denied() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let (gateway, ran) = gated(Arc::new(MockProviderWithNamedTool::new("approval_tool"))).await;
        let id = session(&gateway).await;
        let mut sub = gui_stream::hub().subscribe(id);
        let loopback = as_caller("loopback-open", LOOPBACK);
        let service = gateway.ctx.service().expect("service");
        let asks = door::remote_as(loopback.clone(), async {
            remote_approval(service, || None).is_some()
        })
        .await;
        assert!(!asks, "send alone got an approval callback");
        let result = door::remote_as(loopback, gateway.local(send(id))).await;
        assert!(matches!(result, Ok(Some(_))), "{result:?}");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0, "denied, as before");
        let asked = request_within(&mut sub, Duration::from_millis(200)).await;
        assert!(asked.is_none(), "a send-only turn reached the watcher: {asked:?}");
    }

    /// With `manage` the remote turn asks, as the turns above show; without
    /// the remote door no caller gets the callback at all.
    #[tokio::test]
    async fn only_a_remote_caller_with_manage_gets_the_approval_callback() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let (gateway, _ran) =
            gated(Arc::new(MockProviderWithNamedTool::new("approval_tool"))).await;
        let service = gateway.ctx.service().expect("service");
        assert!(remote_approval(service, || None).is_none(), "local door");
        let manage = door::remote_as(as_caller("api_key", WIRE), async {
            remote_approval(service, || None).is_some()
        })
        .await;
        assert!(manage, "a caller holding manage is asked");
    }

    #[tokio::test]
    async fn an_answer_from_another_principal_is_refused_and_the_request_waits() {
        let gateway = Gateway::start().await;
        let id = Uuid::new_v4();
        let mut sub = gui_stream::hub().subscribe(id);
        let ask = ask_for(info(id), Duration::from_secs(30), Some("alice".to_string()));
        let answer = async {
            let (_, request_id, _) = next_request(&mut sub).await;
            let op = |approved| ApprovalAnswer {
                session_id: id,
                request_id,
                approved,
                always: false,
            };
            let refused =
                door::remote_as(as_caller("bob", WIRE), gateway.local(op(true))).await;
            assert_eq!(
                refused,
                Err(BackendError::Forbidden {
                    scope: Scope::Admin
                })
            );
            let took = door::remote_as(as_caller("alice", WIRE), gateway.local(op(false))).await;
            assert_eq!(took, Ok(true), "the refused answer left the request waiting");
        };
        let (asked, ()) = tokio::join!(ask, answer);
        assert_eq!(asked.expect("answered"), (false, false));
    }

    /// The local door answers a request whoever's turn asked, as before.
    #[tokio::test]
    async fn the_local_door_answers_any_principals_request() {
        let gateway = Gateway::start().await;
        let id = Uuid::new_v4();
        let mut sub = gui_stream::hub().subscribe(id);
        let ask = ask_for(info(id), Duration::from_secs(30), Some("alice".to_string()));
        let answer = async {
            let (_, request_id, _) = next_request(&mut sub).await;
            let took = gateway
                .local(ApprovalAnswer {
                    session_id: id,
                    request_id,
                    approved: true,
                    always: false,
                })
                .await;
            assert_eq!(took, Ok(true));
        };
        let (asked, ()) = tokio::join!(ask, answer);
        assert_eq!(asked.expect("answered"), (true, false));
    }

    /// The server picks a remote turn's history line and channel; the
    /// caller's voice flag is kept, so a remote window's recording is
    /// answered aloud.
    #[test]
    fn the_server_picks_a_remote_turns_display_and_channel_and_keeps_its_voice() {
        let remote = as_caller("api_key", WIRE);
        assert_eq!(
            at_door(Some(&remote), Some("shown".into()), true),
            (None, true, "a2a")
        );
        assert_eq!(
            at_door(Some(&remote), Some("shown".into()), false),
            (None, false, "a2a")
        );
    }

    #[test]
    fn the_window_keeps_its_display_voice_and_channel() {
        assert_eq!(
            at_door(None, Some("shown".into()), true),
            (Some("shown".to_string()), true, "tui")
        );
        assert_eq!(at_door(None, None, false), (None, false, "tui"));
    }

    /// A remote send waits for a channel's turn on the session; the window's
    /// does not (`a_channel_mid_turn_does_not_refuse_a_window_send`).
    #[tokio::test]
    async fn a_channel_mid_turn_refuses_a_remote_send() {
        use crate::brain::agent::service::session_routes::register_turn_probe;

        let gateway = booted().await;
        let id = session(&gateway).await;
        register_turn_probe(id, Arc::new(|| true));
        let result = door::remote_as(as_caller("api_key", WIRE), gateway.local(send(id))).await;
        assert!(matches!(result, Err(BackendError::Busy)), "{result:?}");
    }

    #[tokio::test]
    async fn only_the_local_door_resumes_a_turn() {
        let gateway = booted().await;
        let id = session(&gateway).await;
        let remote = door::remote_as(as_caller("api_key", WIRE), gateway.local(resume(id))).await;
        assert!(
            matches!(
                remote,
                Err(BackendError::Forbidden {
                    scope: Scope::Admin
                })
            ),
            "{remote:?}"
        );
        let local = gateway.local(resume(id)).await;
        assert!(
            !matches!(local, Err(BackendError::Forbidden { .. })),
            "{local:?}"
        );
    }

    #[tokio::test]
    async fn turn_cancel_has_parity() {
        let gateway = booted().await;
        let id = session(&gateway).await;
        let answer = parity!(gateway, TurnCancel { session_id: id });
        assert_eq!(answer, serde_json::json!({ "Ok": false }), "{answer}");
        forbidden!(gateway, TurnCancel { session_id: id });
    }

    #[test]
    fn the_turn_cancel_fixture_decodes() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/gui_backend/TurnCancel.json"
        ))
        .expect("fixture is JSON");
        assert_eq!(fixture["op"], TurnCancel::NAME);
        assert_eq!(fixture["scope"], TurnCancel::SCOPE.as_str());
        let _: TurnCancel =
            serde_json::from_value(fixture["params"].clone()).expect("params decode");
        let _: Result<bool, BackendError> =
            serde_json::from_value(fixture["reply"].clone()).expect("reply decodes");
    }

    /// A remote turn waiting on its approval request is running. Another
    /// principal's stop is refused and the turn goes on; its own principal's
    /// stop drops it, and the session has no remote turn left to stop.
    #[tokio::test]
    async fn only_the_principal_that_opened_a_remote_turn_stops_it() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let (gateway, ran) = gated(Arc::new(MockProviderWithNamedTool::new("approval_tool"))).await;
        let id = session(&gateway).await;
        let mut sub = gui_stream::hub().subscribe(id);
        let alice = as_caller("alice", WIRE);
        let stop = async {
            let _ = next_request(&mut sub).await;
            let refused = door::remote_as(
                as_caller("bob", WIRE),
                gateway.local(TurnCancel { session_id: id }),
            )
            .await;
            assert_eq!(
                refused,
                Err(BackendError::Forbidden {
                    scope: Scope::Admin
                })
            );
            let stopped =
                door::remote_as(alice.clone(), gateway.local(TurnCancel { session_id: id })).await;
            assert_eq!(stopped, Ok(true));
        };
        let turn = door::remote_as(alice.clone(), gateway.local(send(id)));
        let (result, ()) = tokio::join!(turn, stop);
        assert!(matches!(result, Ok(None)), "{result:?}");
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0);
        let after = gateway.local(TurnCancel { session_id: id }).await;
        assert_eq!(after, Ok(false), "the stopped turn left the register");
    }

    /// The local door stops a remote turn whoever opened it: the crab's own
    /// stop, as the window's token always was.
    #[test]
    fn the_local_door_stops_any_remote_turn() {
        let id = Uuid::new_v4();
        let cancel = CancellationToken::new();
        let guard = register_running(id, "alice", &cancel);
        assert_eq!(cancel_running(id, None), Ok(true));
        assert!(cancel.is_cancelled());
        drop(guard);
        assert_eq!(cancel_running(id, None), Ok(false));
    }

    /// A window turn registers nothing: `turn.cancel` finds no remote turn.
    #[test]
    fn a_session_with_no_remote_turn_has_nothing_to_stop() {
        assert_eq!(cancel_running(Uuid::new_v4(), Some("alice")), Ok(false));
    }

    /// A remote turn leaves its principal on the session after it ends, and
    /// the next remote turn there replaces it.
    #[test]
    fn a_session_keeps_the_principal_of_its_last_remote_turn() {
        let id = Uuid::new_v4();
        assert_eq!(turn_principal(id), None, "no remote turn ran here");
        let guard = register_running(id, "alice", &CancellationToken::new());
        assert_eq!(turn_principal(id).as_deref(), Some("alice"));
        drop(guard);
        assert_eq!(
            turn_principal(id).as_deref(),
            Some("alice"),
            "the turn ended, its principal stays"
        );
        drop(register_running(id, "bob", &CancellationToken::new()));
        assert_eq!(turn_principal(id).as_deref(), Some("bob"));
    }

    /// A remote turn's progress reaches whoever watches its session: the
    /// stream is the only way a remote card sees the answer being written,
    /// the reasoning and the tool calls before the reply settles it.
    #[tokio::test]
    async fn a_remote_turns_progress_reaches_its_watchers() {
        use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

        let (gateway, _ran) =
            gated(Arc::new(MockProviderWithNamedTool::new("approval_tool"))).await;
        let id = session(&gateway).await;
        let (result, mut sub) = turn_answered(&gateway, id, true, false).await;
        assert!(matches!(result, Ok(Some(_))), "{result:?}");
        let mut progress = 0usize;
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(200), sub.next()).await
        {
            if let Event::Turn { session_id, .. } = event {
                assert_eq!(session_id, id, "progress landed on another session");
                progress += 1;
            }
        }
        assert!(progress > 0, "a remote turn streamed no progress to its watcher");
    }

    /// Every progress kind the service emits travels the same way: the
    /// callback publishes it unchanged, on its own session.
    #[tokio::test]
    async fn the_stream_progress_callback_publishes_each_event_on_its_session() {
        let id = Uuid::new_v4();
        let mut sub = gui_stream::hub().subscribe(id);
        let publish = stream_progress();
        publish(id, crate::brain::agent::ProgressEvent::Thinking);
        let event = tokio::time::timeout(Duration::from_secs(5), sub.next())
            .await
            .expect("an event within 5s")
            .expect("the session channel stays open");
        assert_eq!(
            event,
            Event::Turn {
                session_id: id,
                progress: crate::brain::agent::ProgressEvent::Thinking,
            }
        );
    }
}
