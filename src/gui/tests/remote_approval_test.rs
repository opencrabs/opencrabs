//! A remote crab's tool approval reaches the window as a card (C8).
//!
//! The class these pin: a crab event the window drops is a question nobody
//! answers. `Event::ApprovalRequested` was mapped to nothing, so every remote
//! approval ran out its wait and denied the tool unasked. Every event kind
//! the crab publishes now either reaches the window or sits on an explicit
//! allowlist with its reason, so a new kind cannot be dropped silently again.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use opencrabs::backend::turn::APPROVAL_TIMEOUT;
use opencrabs::backend::{BackendError, Event, Scope};
use opencrabs::brain::agent::ProgressEvent;
use uuid::Uuid;

use crate::agent_bridge::TurnEvent;
use crate::backend_glue::Backend;
use crate::backend_remote::{refreshes_cache, to_turn_event};
use crate::remote_approval::{
    Approvals, PendingApproval, SUMMARY_CHARS, Settled, ends_turn, from_event, settled_event,
    summarize,
};

/// How many kinds `Event` has. [`ordinal`] has no wildcard, so a new kind
/// fails to compile there; numbering it means raising this, and the test
/// below then fails until [`one_of_each`] carries a sample of it.
const EVENT_KINDS: usize = 8;

/// The kind's place in the enum. No wildcard on purpose.
fn ordinal(event: &Event) -> usize {
    match event {
        Event::Turn { .. } => 0,
        Event::TurnDone { .. } => 1,
        Event::TurnFailed { .. } => 2,
        Event::TurnCancelled { .. } => 3,
        Event::RailChanged => 4,
        Event::TranscriptChanged { .. } => 5,
        Event::TasksChanged => 6,
        Event::ApprovalRequested { .. } => 7,
    }
}

fn approval_event(session_id: Uuid, request_id: Uuid) -> Event {
    Event::ApprovalRequested {
        session_id,
        request_id,
        tool_name: "bash".into(),
        tool_description: "run a command".into(),
        tool_input: serde_json::json!({"command": "ls -la /tmp"}),
        capabilities: vec!["ExecuteShell".into()],
    }
}

fn one_of_each() -> Vec<Event> {
    let id = Uuid::new_v4();
    vec![
        Event::Turn {
            session_id: id,
            progress: ProgressEvent::Thinking,
        },
        Event::TurnDone { session_id: id },
        Event::TurnFailed {
            session_id: id,
            message: "provider: timeout".into(),
        },
        Event::TurnCancelled { session_id: id },
        Event::RailChanged,
        Event::TranscriptChanged { session_id: id },
        Event::TasksChanged,
        approval_event(id, Uuid::new_v4()),
    ]
}

/// The kinds the window reads neither as a window event nor as a cache
/// refresh, each with why that is right. Nothing else may be dropped.
const DROPPED_ON_PURPOSE: &[(&str, &str)] = &[
    (
        "turn_done",
        "the window's own call settles its turn; a second settle closes the card twice",
    ),
    (
        "turn_failed",
        "same as turn_done: the call's reply settles it",
    ),
    (
        "turn_cancelled",
        "same as turn_done: the call's reply settles it",
    ),
];

fn kind(event: &Event) -> String {
    serde_json::to_value(event).expect("encode")["kind"]
        .as_str()
        .expect("every event names its kind")
        .to_string()
}

#[test]
fn every_crab_event_reaches_the_window_or_is_dropped_on_purpose() {
    let events = one_of_each();
    let mut seen: Vec<usize> = events.iter().map(ordinal).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen,
        (0..EVENT_KINDS).collect::<Vec<_>>(),
        "one_of_each must carry one sample of every Event kind"
    );
    for event in events {
        let name = kind(&event);
        let allowed = DROPPED_ON_PURPOSE.iter().any(|(k, _)| *k == name);
        let handled = refreshes_cache(&event) || to_turn_event(event).is_some();
        assert!(
            handled != allowed,
            "{name}: handled={handled}, allowlisted={allowed}. A kind the window \
             drops must be on DROPPED_ON_PURPOSE with its reason, and a kind it \
             handles must leave the list"
        );
    }
}

#[test]
fn an_approval_request_is_never_on_the_drop_list() {
    let name = kind(&approval_event(Uuid::new_v4(), Uuid::new_v4()));
    assert!(!DROPPED_ON_PURPOSE.iter().any(|(k, _)| *k == name));
}

#[test]
fn an_approval_request_becomes_a_card_for_its_session() {
    let session = Uuid::new_v4();
    let request = Uuid::new_v4();
    let before = Instant::now();
    match to_turn_event(approval_event(session, request)) {
        Some(TurnEvent::ApprovalAsked {
            session: got,
            approval,
        }) => {
            assert_eq!(got, session);
            assert_eq!(approval.request_id, request);
            assert_eq!(approval.tool_name, "bash");
            assert_eq!(approval.summary, "ls -la /tmp");
            assert!(!approval.sending);
            assert!(approval.refusal.is_none());
            assert!(approval.deadline >= before + APPROVAL_TIMEOUT);
            assert!(approval.deadline <= Instant::now() + APPROVAL_TIMEOUT);
        }
        _ => panic!("an approval request must reach the window"),
    }
}

#[test]
fn no_other_event_is_read_as_an_approval() {
    let now = Instant::now();
    for event in one_of_each() {
        let is_approval = matches!(event, Event::ApprovalRequested { .. });
        assert_eq!(from_event(&event, now).is_some(), is_approval, "{event:?}");
    }
}

#[test]
fn the_args_line_is_short_and_one_line() {
    assert_eq!(summarize(&serde_json::json!({"path": "a/b.rs"})), "a/b.rs");
    assert_eq!(summarize(&serde_json::Value::Null), "");
    assert_eq!(
        summarize(&serde_json::json!({"command": "echo a\n  echo b"})),
        "echo a echo b"
    );
    assert_eq!(summarize(&serde_json::json!({"n": 1})), r#"{"n":1}"#);
    let long = "é".repeat(SUMMARY_CHARS + 10);
    let cut = summarize(&serde_json::json!({ "command": long }));
    assert_eq!(cut.chars().count(), SUMMARY_CHARS + 1);
    assert!(cut.ends_with('…'));
}

fn pending(request_id: Uuid, now: Instant) -> PendingApproval {
    PendingApproval {
        request_id,
        tool_name: "bash".into(),
        summary: "ls".into(),
        deadline: now + APPROVAL_TIMEOUT,
        sending: false,
        refusal: None,
    }
}

#[test]
fn a_local_backend_never_shows_an_approval_card() {
    let local = Backend::local(PathBuf::from("unused.db"));
    let remote = local.remote_key().is_some();
    assert!(!remote, "the crab on this machine is not remote");
    let mut approvals = Approvals::default();
    let session = Uuid::new_v4();
    let now = Instant::now();
    assert!(!approvals.ask(remote, session, pending(Uuid::new_v4(), now)));
    assert!(approvals.shown(session, now).is_none());
}

/// The poll hands the card to [`Approvals::ask`] with whether the backend is
/// remote, read from the backend itself, never assumed.
#[test]
fn the_poll_asks_the_backend_before_it_takes_a_card() {
    let src = include_str!("../dashboard_turns.rs");
    let arm = src
        .split("agent_bridge::TurnEvent::ApprovalAsked { session, approval } =>")
        .nth(1)
        .and_then(|rest| rest.split("agent_bridge::TurnEvent::").next())
        .expect("the poll handles ApprovalAsked");
    assert!(arm.contains("self.backend.remote_key().is_some()"), "{arm}");
    assert!(
        arm.contains("self.approvals.ask(remote, session, approval)"),
        "{arm}"
    );
}

#[test]
fn the_card_goes_when_the_crab_takes_the_answer() {
    let mut approvals = Approvals::default();
    let (session, request) = (Uuid::new_v4(), Uuid::new_v4());
    let now = Instant::now();
    assert!(approvals.ask(true, session, pending(request, now)));
    let answer = approvals
        .answering(session, request, true, false)
        .expect("an answer to send");
    assert_eq!(answer.session_id, session);
    assert_eq!(answer.request_id, request);
    assert!(answer.approved && !answer.always);
    assert!(
        approvals
            .shown(session, now)
            .expect("still shown")
            .0
            .sending
    );
    assert_eq!(approvals.settle(session, request, Ok(true)), Settled::Quiet);
    assert!(approvals.shown(session, now).is_none());
}

#[test]
fn a_double_click_sends_one_answer() {
    let mut approvals = Approvals::default();
    let (session, request) = (Uuid::new_v4(), Uuid::new_v4());
    assert!(approvals.ask(true, session, pending(request, Instant::now())));
    assert!(
        approvals
            .answering(session, request, false, false)
            .is_some()
    );
    assert!(approvals.answering(session, request, true, true).is_none());
    assert!(
        approvals
            .answering(session, Uuid::new_v4(), true, false)
            .is_none()
    );
}

#[test]
fn the_card_goes_when_the_wait_runs_out() {
    let mut approvals = Approvals::default();
    let (session, request) = (Uuid::new_v4(), Uuid::new_v4());
    let now = Instant::now();
    assert!(approvals.ask(true, session, pending(request, now)));
    assert!(approvals.shown(session, now).is_some());
    let past = now + APPROVAL_TIMEOUT + Duration::from_secs(1);
    assert!(approvals.shown(session, past).is_none());
}

#[test]
fn the_card_goes_when_the_turn_ends() {
    let session = Uuid::new_v4();
    for settle in [
        TurnEvent::Failed {
            session,
            text: "x".into(),
        },
        TurnEvent::Cancelled { session },
    ] {
        assert_eq!(ends_turn(&settle), Some(session));
    }
    assert_eq!(ends_turn(&TurnEvent::Refresh), None);
    let mut approvals = Approvals::default();
    assert!(approvals.ask(true, session, pending(Uuid::new_v4(), Instant::now())));
    assert!(approvals.turn_ended(session));
    assert!(approvals.shown(session, Instant::now()).is_none());
    assert!(!approvals.turn_ended(session));
}

#[test]
fn an_answer_the_crab_no_longer_waits_for_says_so() {
    let mut approvals = Approvals::default();
    let (session, request) = (Uuid::new_v4(), Uuid::new_v4());
    assert!(approvals.ask(true, session, pending(request, Instant::now())));
    assert!(approvals.answering(session, request, true, false).is_some());
    assert!(matches!(
        approvals.settle(session, request, Ok(false)),
        Settled::Stale(_)
    ));
    assert!(approvals.shown(session, Instant::now()).is_none());
}

#[test]
fn a_refused_answer_stays_on_the_card_in_the_crabs_words() {
    let mut approvals = Approvals::default();
    let (session, request) = (Uuid::new_v4(), Uuid::new_v4());
    let now = Instant::now();
    assert!(approvals.ask(true, session, pending(request, now)));
    let answer = approvals
        .answering(session, request, true, false)
        .expect("an answer to send");
    let refused = settled_event(
        &answer,
        Err(BackendError::Forbidden {
            scope: Scope::Manage,
        }),
    );
    let TurnEvent::ApprovalSettled { outcome, .. } = refused else {
        panic!("a reply settles the card");
    };
    let text = outcome.expect_err("refused");
    assert_eq!(
        text,
        "turn.approval_answer: needs a2a:manage: this crab's token does not grant it"
    );
    assert_eq!(
        approvals.settle(session, request, Err(text.clone())),
        Settled::Refused
    );
    let (card, _) = approvals.shown(session, now).expect("the card stays");
    assert_eq!(card.refusal.as_deref(), Some(text.as_str()));
    assert!(!card.sending, "the buttons come back");
}

#[test]
fn a_second_request_waits_behind_the_first() {
    let mut approvals = Approvals::default();
    let session = Uuid::new_v4();
    let (first, second) = (Uuid::new_v4(), Uuid::new_v4());
    let now = Instant::now();
    assert!(approvals.ask(true, session, pending(first, now)));
    assert!(approvals.ask(true, session, pending(second, now)));
    assert!(
        !approvals.ask(true, session, pending(second, now)),
        "no duplicate"
    );
    let (card, more) = approvals.shown(session, now).expect("shown");
    assert_eq!((card.request_id, more), (first, 1));
    assert_eq!(approvals.settle(session, first, Ok(true)), Settled::Quiet);
    let (card, more) = approvals.shown(session, now).expect("shown");
    assert_eq!((card.request_id, more), (second, 0));
}
