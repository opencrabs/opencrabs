//! A stopped turn keeps the answer it had streamed (#2851).
//!
//! The stop drops the turn's future, so the row its own writer would have
//! written never lands. The session kept the question and the verdict and lost
//! the text between them, which is invisible while the card is on screen (the
//! window draws it from memory) and obvious the moment the session is read
//! again: switch away, switch back, and the turn is a question and "Stopped."
//! with nothing in between. A reload can only draw what the rows hold.
//!
//! The fix writes the streamed text as an ordinary assistant row before the
//! stop marker, from the one place that still has it: the card, captured as the
//! stop lands.

const AGENT_BRIDGE: &str = include_str!("../gui/agent_bridge.rs");
const DASHBOARD_TURNS: &str = include_str!("../gui/dashboard_turns.rs");
const MESSAGE_SERVICE: &str = include_str!("../services/message.rs");

use crate::db::Database;
use crate::services::ServiceContext;
use crate::services::SessionService;
use crate::services::message::MessageService;

/// The cancel arm, cut where the next event arm starts.
fn cancel_arm() -> &'static str {
    DASHBOARD_TURNS
        .split_once("agent_bridge::TurnEvent::Cancelled { session } =>")
        .expect("the cancel arm records the stop")
        .1
        .split_once("agent_bridge::TurnEvent::TurnReverted")
        .expect("the next event arm follows")
        .0
}

/// The same source with every run of whitespace removed, so a chain that rustfmt
/// breaks across lines still matches the shape it describes.
fn squeezed(src: &str) -> String {
    src.chars().filter(|ch| !ch.is_whitespace()).collect()
}

#[test]
fn the_cancel_arm_hands_the_streamed_answer_to_the_writer() {
    let arm = cancel_arm();
    let tight = squeezed(arm);
    assert!(
        arm.contains("let stopped_answer = {"),
        "the arm must capture what the card streamed before it settles the card"
    );
    assert!(
        arm.contains("self.turn.live.answer()"),
        "the window's own card is one of the three places a turn streams"
    );
    assert!(
        tight.contains("self.stashed.get(&session)"),
        "a switched-away turn streams into its parked copy, not the window"
    );
    assert!(
        arm.contains("pane.read(cx).live.answer()"),
        "and a split leaf streams into its own pane"
    );

    // The parked copy is removed later in the arm, so a capture that ran after
    // it would read a card that no longer exists.
    let captured_at = arm.find("let stopped_answer = {").unwrap();
    let parked_gone_at = arm.find("self.stashed.remove(&session)").unwrap();
    assert!(
        captured_at < parked_gone_at,
        "the answer has to be read while the card still holds it"
    );

    let call_at = arm
        .find("agent_bridge::spawn_record_turn_stopped(")
        .expect("the stop is recorded in one place");
    assert!(
        arm.split_at(call_at).1.contains("stopped_answer"),
        "the captured answer is what the writer persists, not a fresh empty card"
    );
}

#[test]
fn the_writer_puts_the_answer_before_the_stop_marker() {
    let body = AGENT_BRIDGE
        .split_once("pub fn spawn_record_turn_stopped(")
        .expect("the stop is recorded in one place")
        .1
        .split_once("/// Generate the personalized brain files")
        .expect("the brain generator follows the stop")
        .0;
    assert!(
        body.contains("record_stopped_turn(session_id, answer)"),
        "one call writes the answer and the verdict, in that order"
    );
    assert!(
        body.contains("pending.delete_session(session_id)"),
        "and the stop still drops its recovery row (#2850)"
    );

    let service = MESSAGE_SERVICE
        .split_once("pub async fn record_stopped_turn(")
        .expect("the service method exists")
        .1
        .split_once("/// The newest `limit` messages")
        .expect("the next service method follows")
        .0;
    let answer_at = service
        .find("self.keep_stopped_answer(session_id, answer)")
        .expect("the answer is written before the verdict");
    let marker_at = service
        .find("self.record_turn_stopped(session_id)")
        .expect("the verdict row");
    assert!(
        answer_at < marker_at,
        "the answer lands first, so the turn reads question, answer, verdict"
    );
    assert!(
        service.contains("\"assistant\".to_string()"),
        "the answer is kept on an assistant row"
    );
}

async fn service() -> (MessageService, SessionService) {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    (
        MessageService::new(context.clone()),
        SessionService::new(context),
    )
}

#[tokio::test]
async fn a_stopped_turn_reloads_with_the_answer_it_had_streamed() {
    let (messages, sessions) = service().await;
    let session = sessions
        .create_session(Some("Stopped".to_string()))
        .await
        .unwrap();
    messages
        .create_message(session.id, "user".to_string(), "the question".to_string())
        .await
        .unwrap();

    messages
        .record_stopped_turn(session.id, Some("half an answer".to_string()))
        .await
        .unwrap();

    // This is the read a session switch does: rows only, nothing from memory.
    let rows = messages
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    let shape: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| (row.role.as_str(), row.content.as_str()))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("user", "the question"),
            ("assistant", "half an answer"),
            ("turn_stopped", "Stopped."),
        ],
        "the reloaded turn keeps the text the user was reading, then the verdict"
    );
}

/// A turn opens its assistant row at the start and appends to it as it goes
/// (#964), so the usual stop finds an empty one waiting. Filling that row is
/// what keeps the turn one answer: a second row would draw the same reply
/// twice, once blank and once whole.
#[tokio::test]
async fn the_row_a_turn_opens_is_the_one_that_gets_the_answer() {
    let (messages, sessions) = service().await;
    let session = sessions
        .create_session(Some("Placeholder".to_string()))
        .await
        .unwrap();
    messages
        .create_message(session.id, "user".to_string(), "the question".to_string())
        .await
        .unwrap();
    let placeholder = messages
        .create_message(session.id, "assistant".to_string(), String::new())
        .await
        .unwrap();

    messages
        .record_stopped_turn(session.id, Some("half an answer".to_string()))
        .await
        .unwrap();

    let rows = messages
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    let assistant: Vec<_> = rows.iter().filter(|row| row.role == "assistant").collect();
    assert_eq!(assistant.len(), 1, "one turn, one answer row");
    assert_eq!(
        assistant[0].id, placeholder.id,
        "the row the turn already opened, filled in"
    );
    assert_eq!(assistant[0].content, "half an answer");
    assert_eq!(
        rows.last().map(|row| row.role.as_str()),
        Some("turn_stopped"),
        "the verdict still closes the turn"
    );
}

/// Appends land at iteration boundaries, so a stop can find part of the answer
/// already in the row. Only the missing tail goes in, never the whole text
/// again: a second copy would draw the same reply twice.
#[tokio::test]
async fn a_partly_written_answer_is_finished_not_repeated() {
    let (messages, sessions) = service().await;
    let session = sessions
        .create_session(Some("Partial".to_string()))
        .await
        .unwrap();
    messages
        .create_message(session.id, "user".to_string(), "the question".to_string())
        .await
        .unwrap();
    messages
        .create_message(session.id, "assistant".to_string(), "half an".to_string())
        .await
        .unwrap();

    messages
        .record_stopped_turn(session.id, Some("half an answer".to_string()))
        .await
        .unwrap();

    let rows = messages
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    let assistant: Vec<_> = rows.iter().filter(|row| row.role == "assistant").collect();
    assert_eq!(assistant.len(), 1, "still one answer row");
    assert_eq!(
        assistant[0].content, "half an answer",
        "the tail completes what the turn had already written"
    );
}

/// A row that already holds text is never rewritten, only extended. When the
/// card's text is not what the row already holds, dropping it there is exactly
/// the loss #2851 describes, so it goes in after what is there.
#[tokio::test]
async fn an_answer_the_row_did_not_hold_is_kept_not_dropped() {
    let (messages, sessions) = service().await;
    let session = sessions
        .create_session(Some("Earlier".to_string()))
        .await
        .unwrap();
    messages
        .create_message(
            session.id,
            "user".to_string(),
            "the first question".to_string(),
        )
        .await
        .unwrap();
    let earlier = messages
        .create_message(
            session.id,
            "assistant".to_string(),
            "the first answer".to_string(),
        )
        .await
        .unwrap();

    messages
        .record_stopped_turn(session.id, Some("a later answer".to_string()))
        .await
        .unwrap();

    let rows = messages
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    let first = rows
        .iter()
        .find(|row| row.id == earlier.id)
        .expect("the earlier answer is still there");
    assert_eq!(
        first.content, "the first answer\n\na later answer",
        "what the ledger held stays, and the card's text is added after it"
    );
    assert_eq!(
        rows.iter().filter(|row| row.role == "assistant").count(),
        1,
        "the answer joins the turn's row instead of opening a second one"
    );
    assert_eq!(
        rows.last().map(|row| row.content.as_str()),
        Some("Stopped."),
        "the verdict still closes the turn"
    );
}

/// The other side of the same rule: a row that already carries the card's text
/// needs nothing, and repeating it would draw the same reply twice. Appends
/// land at iteration boundaries, so the row can also run ahead of the card,
/// which puts the text inside the row instead of at its end.
#[tokio::test]
async fn a_row_that_already_carries_the_answer_is_not_repeated() {
    let (messages, sessions) = service().await;
    let session = sessions
        .create_session(Some("Repeated".to_string()))
        .await
        .unwrap();
    messages
        .create_message(session.id, "user".to_string(), "the question".to_string())
        .await
        .unwrap();
    messages
        .create_message(
            session.id,
            "assistant".to_string(),
            "the whole answer, and the tail the card had not drawn yet".to_string(),
        )
        .await
        .unwrap();

    for answer in ["the whole answer", "the tail the card had not drawn yet"] {
        messages
            .record_stopped_turn(session.id, Some(answer.to_string()))
            .await
            .unwrap();
    }

    let rows = messages
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    let assistant: Vec<_> = rows.iter().filter(|row| row.role == "assistant").collect();
    assert_eq!(assistant.len(), 1, "one answer row");
    assert_eq!(
        assistant[0].content, "the whole answer, and the tail the card had not drawn yet",
        "the text is already there, so the stop adds nothing"
    );
}

#[tokio::test]
async fn a_stop_before_the_first_token_writes_no_empty_answer() {
    let (messages, sessions) = service().await;
    let session = sessions
        .create_session(Some("Empty".to_string()))
        .await
        .unwrap();

    for answer in [None, Some(String::new()), Some("   \n ".to_string())] {
        messages
            .record_stopped_turn(session.id, answer)
            .await
            .unwrap();
    }

    let rows = messages
        .list_messages_for_session(session.id)
        .await
        .unwrap();
    assert_eq!(
        rows.iter().filter(|row| row.role == "assistant").count(),
        0,
        "nothing streamed means nothing to keep, and a blank row is not an answer"
    );
    assert_eq!(
        rows.iter().filter(|row| row.role == "turn_stopped").count(),
        3,
        "each stop still records its verdict"
    );
}
