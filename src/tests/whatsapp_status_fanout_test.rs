//! Tests for the per-recipient status fan-out (#2177).
//!
//! `status_update` used to charge and sleep once per recipient and then post
//! ONE status for the whole list: every recipient got it at the same instant
//! after a single delay, and a cap hit part-way through the loop spent the
//! earlier slots on nothing. The fan-out moves the charge and the send into
//! the same iteration and keeps the pace between two sends, so both halves
//! are pinned here without a WhatsApp connection.
//!
//! The wiring (that the dispatch arm actually goes through the fan-out) is
//! pinned in `whatsapp_status_media_test`, which scans the arm's source.

use crate::brain::tools::whatsapp_send::{StatusFanOut, StatusSink, fan_out_status, status_report};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wacore_binary::jid::Jid;

const PACE: Duration = Duration::from_secs(5);

fn jids(n: usize) -> Vec<Jid> {
    (1..=n)
        .map(|i| {
            format!("1555123{i:04}@s.whatsapp.net")
                .parse()
                .expect("test JID")
        })
        .collect()
}

/// One step of a fan-out, in the order the fan-out took it.
#[derive(Debug, PartialEq)]
enum Step {
    /// A slot of the shared budget was charged for this recipient.
    Charge(String),
    /// The status was delivered to this recipient at this offset.
    Send(String, Duration),
}

/// A [`StatusSink`] that records what happened and when, so the ordering of
/// charges and sends can be asserted instead of inferred.
struct Recorder {
    /// Slots the fake daily cap allows.
    cap: usize,
    charged: usize,
    start: tokio::time::Instant,
    steps: Arc<Mutex<Vec<Step>>>,
    /// Recipient the fake client refuses, if any.
    fail_for: Option<String>,
}

impl Recorder {
    fn new(cap: usize) -> (Self, Arc<Mutex<Vec<Step>>>) {
        let steps = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                cap,
                charged: 0,
                start: tokio::time::Instant::now(),
                steps: Arc::clone(&steps),
                fail_for: None,
            },
            steps,
        )
    }
}

#[async_trait]
impl StatusSink for Recorder {
    async fn charge(&mut self, jid: &Jid) -> bool {
        if self.charged >= self.cap {
            return false;
        }
        self.charged += 1;
        self.steps
            .lock()
            .expect("step log")
            .push(Step::Charge(jid.to_string()));
        true
    }

    async fn deliver(&mut self, jid: &Jid) -> Result<(), String> {
        let at = tokio::time::Instant::now().saturating_duration_since(self.start);
        self.steps
            .lock()
            .expect("step log")
            .push(Step::Send(jid.to_string(), at));
        match &self.fail_for {
            Some(target) if target == &jid.to_string() => Err("stanza rejected".to_string()),
            _ => Ok(()),
        }
    }
}

fn send_offsets(steps: &Arc<Mutex<Vec<Step>>>) -> Vec<Duration> {
    steps
        .lock()
        .expect("step log")
        .iter()
        .filter_map(|s| match s {
            Step::Send(_, at) => Some(*at),
            Step::Charge(_) => None,
        })
        .collect()
}

/// The whole point of the issue: the delay belongs BETWEEN two sends. Before
/// the fix the loop slept for every recipient and then sent once, so the
/// first (and only) status left after the entire delay.
#[tokio::test(start_paused = true)]
async fn the_pace_sits_between_sends_not_before_the_first() {
    let targets = jids(3);
    let (mut sink, steps) = Recorder::new(10);

    let out = fan_out_status(&mut sink, &targets, PACE).await;

    assert_eq!(out.sent.len(), 3, "every recipient got a status");
    assert_eq!(
        send_offsets(&steps),
        vec![Duration::ZERO, PACE, PACE * 2],
        "the first status goes out immediately and the rest one pace apart"
    );
}

/// A refusal at the cap must not spend a slot on a status that never left.
#[tokio::test(start_paused = true)]
async fn a_cap_refusal_spends_no_slot_on_an_undelivered_status() {
    let targets = jids(3);
    let (mut sink, steps) = Recorder::new(2);

    let out = fan_out_status(&mut sink, &targets, PACE).await;

    assert_eq!(out.sent.len(), 2, "the cap allowed two statuses");
    assert_eq!(
        out.refused,
        vec![targets[2].to_string()],
        "the third recipient is refused by name"
    );
    assert!(out.failed.is_empty(), "a refusal is not a send failure");
    assert_eq!(
        sink.charged, 2,
        "one slot per delivered status, none for the refused one"
    );
    let charges = steps
        .lock()
        .expect("step log")
        .iter()
        .filter(|s| matches!(s, Step::Charge(_)))
        .count();
    assert_eq!(charges, 2, "the refused recipient was never charged");
}

/// A cap refusal stops the fan-out instead of sleeping through recipients
/// that cannot receive anything, and the report names every one of them.
#[tokio::test(start_paused = true)]
async fn a_cap_refusal_names_every_skipped_recipient() {
    let targets = jids(3);
    let (mut sink, _steps) = Recorder::new(1);

    let out = fan_out_status(&mut sink, &targets, PACE).await;
    let report = status_report("text status", targets.len(), &out, PACE);

    assert_eq!(
        out.refused,
        vec![targets[1].to_string(), targets[2].to_string()],
        "both skipped recipients are listed"
    );
    assert!(report.success, "a partial status is still worth reporting");
    let text = report.output;
    assert!(
        text.contains("posted to 1 of 3"),
        "states the split instead of claiming a batch success: {text}"
    );
    assert!(
        text.contains(&targets[1].to_string()) && text.contains(&targets[2].to_string()),
        "names the recipients a retry still owes: {text}"
    );
}

/// One bad recipient must not cancel the statuses owed to the others.
#[tokio::test(start_paused = true)]
async fn one_failed_send_does_not_stop_the_rest() {
    let targets = jids(3);
    let (mut sink, _steps) = Recorder::new(10);
    sink.fail_for = Some(targets[1].to_string());

    let out = fan_out_status(&mut sink, &targets, PACE).await;

    assert_eq!(
        out.sent,
        vec![targets[0].to_string(), targets[2].to_string()],
        "the first and third still received it"
    );
    assert_eq!(out.failed.len(), 1, "the middle recipient failed");
    assert_eq!(out.failed[0].0, targets[1].to_string());
    assert!(
        out.refused.is_empty(),
        "a send failure is not a cap refusal"
    );
}

/// Nothing delivered is an error, and it says the cap is not a queue.
#[test]
fn a_fan_out_that_delivered_nothing_is_an_error() {
    let targets = jids(2);
    let out = StatusFanOut {
        sent: Vec::new(),
        refused: targets
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
        failed: Vec::new(),
    };

    let report = status_report("text status", targets.len(), &out, PACE);

    assert!(!report.success, "no delivery is a failure");
    let err = report.error.unwrap_or_default();
    assert!(err.contains("reached none of the 2"), "{err}");
    assert!(
        err.contains("not queued for retry"),
        "a status cannot ride the text queue, so say so: {err}"
    );
}

/// #2177's headline claim: every recipient gets its OWN send. The old shape
/// charged and slept once per recipient and then posted a single status for
/// the whole list, so the wire carried one send no matter how many recipients
/// were on the allowlist.
#[tokio::test(start_paused = true)]
async fn status_sends_once_per_recipient() {
    let targets = jids(3);
    let (mut sink, steps) = Recorder::new(10);

    let out = fan_out_status(&mut sink, &targets, PACE).await;

    let sends: Vec<String> = steps
        .lock()
        .expect("step log")
        .iter()
        .filter_map(|s| match s {
            Step::Send(jid, _) => Some(jid.clone()),
            Step::Charge(_) => None,
        })
        .collect();
    assert_eq!(
        sends,
        targets
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>(),
        "one send per recipient, in order, instead of one send for the whole list"
    );
    assert_eq!(out.sent, sends, "every send is reported as delivered");
}
