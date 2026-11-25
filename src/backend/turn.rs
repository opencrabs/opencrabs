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

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::models::Message;

use super::{BackendError, Ctx, Op, Scope};

ops! { TurnSend, TurnResume }

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
            None,
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
            None,
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};
    use crate::brain::agent::AgentService;
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
