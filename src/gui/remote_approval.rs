//! A remote crab asking before it runs a tool, as a card in its conversation.
//!
//! A turn sent to a remote crab by a caller holding `a2a:manage` runs with an
//! approval callback on the crab: a gated tool publishes
//! [`Event::ApprovalRequested`] on the session and waits for
//! `turn.approval_answer` ([`ApprovalAnswer`]). Until this card existed the
//! window dropped the event, so the wait always ran out and the tool was
//! denied without the user ever being asked.
//!
//! Remote only. A crab on this machine answers its own approvals through its
//! configured policy and never publishes the event into the window's sink, and
//! [`Approvals::ask`] refuses a card for a backend that is not remote, so the
//! local behaviour does not change.
//!
//! The card goes away when the answer lands, when the crab's wait runs out
//! ([`APPROVAL_TIMEOUT`] after the request reached the window), or when the
//! turn that asked settles. A refused answer (a token without `manage`, or
//! another principal's turn) stays on the card in the crab's own words.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::{AnyElement, Context, Div, SharedString, div, prelude::*, px, rgb, rgba};
use opencrabs::backend::Event;
use opencrabs::backend::cap;
use opencrabs::backend::turn::{APPROVAL_TIMEOUT, ApprovalAnswer};
use opencrabs::backend::{BackendError, Op};
use serde_json::Value;
use uuid::Uuid;

use crate::agent_bridge::TurnEvent;
use crate::dashboard_state::Dashboard;
use crate::{palette, transcript};

/// The longest args line the card prints, in characters.
pub(crate) const SUMMARY_CHARS: usize = 160;

/// One tool waiting for a yes or a no on a remote crab.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingApproval {
    /// What the answer names to reach the request.
    pub(crate) request_id: Uuid,
    /// The tool that wants to run.
    pub(crate) tool_name: String,
    /// One line of what the model gave the tool.
    pub(crate) summary: String,
    /// When the crab stops waiting and denies the tool. The event carries no
    /// deadline, so it is the crab's own wait counted from the moment the
    /// request reached this window.
    pub(crate) deadline: Instant,
    /// An answer is on its way to the crab: the buttons stand down.
    pub(crate) sending: bool,
    /// The crab refused the answer; its words, shown on the card.
    pub(crate) refusal: Option<String>,
}

/// The pending approval `event` asks for, with its session, or `None` for
/// every other event. `now` is when the event reached the window.
pub(crate) fn from_event(event: &Event, now: Instant) -> Option<(Uuid, PendingApproval)> {
    let Event::ApprovalRequested {
        session_id,
        request_id,
        tool_name,
        tool_input,
        ..
    } = event
    else {
        return None;
    };
    Some((
        *session_id,
        PendingApproval {
            request_id: *request_id,
            tool_name: tool_name.clone(),
            summary: summarize(tool_input),
            deadline: now + APPROVAL_TIMEOUT,
            sending: false,
            refusal: None,
        },
    ))
}

/// One line of a tool's input: the field that says what the call does when
/// there is one (a command, a path, a url, a query), the compact JSON
/// otherwise, cut at [`SUMMARY_CHARS`] on a character boundary.
pub(crate) fn summarize(input: &Value) -> String {
    const TELLING: &[&str] = &[
        "command",
        "cmd",
        "path",
        "file_path",
        "url",
        "query",
        "pattern",
    ];
    let telling = input.as_object().and_then(|map| {
        TELLING
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_str))
    });
    let raw = match telling {
        Some(text) => text.to_string(),
        None if input.is_null() => String::new(),
        None => input.to_string(),
    };
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = flat.chars();
    let short: String = chars.by_ref().take(SUMMARY_CHARS).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
}

/// The session a settle of the turn in `event` ends, if it is one: a finished,
/// failed or stopped turn takes its approval cards with it.
pub(crate) fn ends_turn(event: &TurnEvent) -> Option<Uuid> {
    match event {
        TurnEvent::Done { session, .. }
        | TurnEvent::Failed { session, .. }
        | TurnEvent::Cancelled { session } => Some(*session),
        _ => None,
    }
}

/// What a settled answer leaves for the window to say.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Settled {
    /// The crab took the answer, or there was no card to settle.
    Quiet,
    /// The crab no longer waited for this request; a line for the notices.
    Stale(String),
    /// The crab refused the answer; the card keeps it.
    Refused,
}

/// The approval cards of the window, by the session they belong to.
#[derive(Debug, Default)]
pub(crate) struct Approvals {
    by_session: HashMap<Uuid, Vec<PendingApproval>>,
}

impl Approvals {
    /// Take `approval` for `session`. `remote` is whether the window holds a
    /// remote crab: a local one never gets a card, whatever reached the sink.
    /// `true` when the card was taken.
    pub(crate) fn ask(&mut self, remote: bool, session: Uuid, approval: PendingApproval) -> bool {
        if !remote {
            return false;
        }
        let cards = self.by_session.entry(session).or_default();
        if cards.iter().any(|c| c.request_id == approval.request_id) {
            return false;
        }
        cards.push(approval);
        true
    }

    /// The card `session` shows at `now`, and how many more wait behind it.
    /// A card past its deadline is gone: the crab already denied the tool.
    pub(crate) fn shown(
        &mut self,
        session: Uuid,
        now: Instant,
    ) -> Option<(&PendingApproval, usize)> {
        let cards = self.by_session.get_mut(&session)?;
        cards.retain(|c| c.deadline > now);
        if cards.is_empty() {
            self.by_session.remove(&session);
            return None;
        }
        let cards = self.by_session.get(&session)?;
        cards.first().map(|first| (first, cards.len() - 1))
    }

    /// The answer to send for `request_id` of `session`, marking the card as
    /// sending. `None` when there is no such card or an answer is already on
    /// its way, so a double click sends one answer.
    pub(crate) fn answering(
        &mut self,
        session: Uuid,
        request_id: Uuid,
        approved: bool,
        always: bool,
    ) -> Option<ApprovalAnswer> {
        let card = self
            .by_session
            .get_mut(&session)?
            .iter_mut()
            .find(|c| c.request_id == request_id && !c.sending)?;
        card.sending = true;
        card.refusal = None;
        Some(ApprovalAnswer {
            session_id: session,
            request_id,
            approved,
            always,
        })
    }

    /// The crab's reply to an answer. `Ok(true)` took it, `Ok(false)` means
    /// the request was no longer waiting; either way the card goes. `Err` is
    /// the refusal text, kept on the card with the buttons back.
    pub(crate) fn settle(
        &mut self,
        session: Uuid,
        request_id: Uuid,
        outcome: Result<bool, String>,
    ) -> Settled {
        let Some(cards) = self.by_session.get_mut(&session) else {
            return Settled::Quiet;
        };
        let Some(at) = cards.iter().position(|c| c.request_id == request_id) else {
            return Settled::Quiet;
        };
        let settled = match outcome {
            Ok(true) => {
                cards.remove(at);
                Settled::Quiet
            }
            Ok(false) => {
                let card = cards.remove(at);
                Settled::Stale(format!(
                    "{}: the crab was no longer waiting for this approval",
                    card.tool_name
                ))
            }
            Err(text) => {
                if let Some(card) = cards.get_mut(at) {
                    card.sending = false;
                    card.refusal = Some(text);
                }
                Settled::Refused
            }
        };
        if cards.is_empty() {
            self.by_session.remove(&session);
        }
        settled
    }

    /// The turn of `session` settled: its cards go with it. `true` when one
    /// was showing.
    pub(crate) fn turn_ended(&mut self, session: Uuid) -> bool {
        self.by_session.remove(&session).is_some()
    }

    /// Every card goes: the window now holds another crab.
    pub(crate) fn clear(&mut self) {
        self.by_session.clear();
    }
}

/// The window event a reply to `answer` becomes. A refusal reads in the
/// words every other refused action uses ([`cap::notice`]).
pub(crate) fn settled_event(
    answer: &ApprovalAnswer,
    reply: Result<bool, BackendError>,
) -> TurnEvent {
    TurnEvent::ApprovalSettled {
        session: answer.session_id,
        request_id: answer.request_id,
        outcome: reply.map_err(|e| cap::notice(ApprovalAnswer::NAME, &e)),
    }
}

/// The handlers the card's three buttons call.
pub(crate) struct CardActions {
    pub(crate) approve: transcript::ClickHandler,
    pub(crate) always: transcript::ClickHandler,
    pub(crate) deny: transcript::ClickHandler,
}

/// The card: the tool, its args, the time left, and Approve, Allow for this
/// turn and Deny. While an answer is on its way the buttons give way to a
/// line saying so; a refusal is printed above them.
pub(crate) fn card(
    approval: &PendingApproval,
    more: usize,
    now: Instant,
    actions: CardActions,
) -> Div {
    let p = palette::current();
    let left = approval.deadline.saturating_duration_since(now);
    let title = format!(
        "The crab asks to run {} (denied in {}s)",
        approval.tool_name,
        left.as_secs()
    );
    let mut col = div()
        .flex()
        .flex_col()
        .items_start()
        .gap_1()
        .px_3()
        .py_2()
        .rounded(px(transcript::MESSAGE_RADIUS - 4.0))
        .border_1()
        .border_color(rgba((p.accent << 8) | 0x88))
        .child(
            div()
                .text_size(px(transcript::PLAIN_TEXT_SIZE))
                .text_color(rgb(p.text_normal))
                .child(SharedString::from(title)),
        );
    if !approval.summary.is_empty() {
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(p.text_muted))
                .child(SharedString::from(approval.summary.clone())),
        );
    }
    if more > 0 {
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(p.text_faint))
                .child(SharedString::from(format!("{more} more waiting"))),
        );
    }
    if let Some(refusal) = &approval.refusal {
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(p.red))
                .child(SharedString::from(refusal.clone())),
        );
    }
    if approval.sending {
        return col.child(
            div()
                .text_xs()
                .text_color(rgb(p.text_faint))
                .child(SharedString::from("sending the answer…")),
        );
    }
    let button =
        |id: &'static str, label: &'static str, ink: u32, pick: transcript::ClickHandler| {
            div()
                .id(SharedString::from(id))
                .px_3()
                .py_1()
                .rounded(px(transcript::MESSAGE_RADIUS - 4.0))
                .border_1()
                .border_color(rgba(0xffffff1a))
                .hover(|d| d.bg(rgba(0xffffff14)).border_color(rgba(0xffffff33)))
                .text_color(rgb(ink))
                .cursor_pointer()
                .child(SharedString::from(label))
                .on_click(move |ev, window, app| pick(ev, window, app))
        };
    col.child(
        div()
            .flex()
            .flex_row()
            .gap_2()
            .child(button(
                "approval-approve",
                "Approve",
                p.accent,
                actions.approve,
            ))
            .child(button(
                "approval-always",
                "Allow for this turn",
                p.text_normal,
                actions.always,
            ))
            .child(button("approval-deny", "Deny", p.red, actions.deny)),
    )
}

impl Dashboard {
    /// Send the answer to the card `request_id` of `session`, off the
    /// window's thread. The reply comes back through the sink as
    /// [`TurnEvent::ApprovalSettled`].
    pub(crate) fn answer_approval(
        &mut self,
        session: Uuid,
        request_id: Uuid,
        approved: bool,
        always: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(answer) = self
            .approvals
            .answering(session, request_id, approved, always)
        else {
            return;
        };
        let sink = self.sink.clone();
        let asked = answer.clone();
        let spawned = self.backend.spawn(answer, move |reply| {
            let _ = sink.send(settled_event(&asked, reply));
        });
        if let Err(e) = spawned {
            self.approvals.settle(session, request_id, Err(e));
        }
        cx.notify();
    }

    /// The approval card of the conversation on screen, if one waits.
    pub(crate) fn approval_row(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.chat_session?;
        let now = Instant::now();
        let (approval, more) = self.approvals.shown(session, now)?;
        let approval = approval.clone();
        let request_id = approval.request_id;
        let pick = |approved: bool, always: bool| -> transcript::ClickHandler {
            Rc::new(cx.listener(
                move |this: &mut Self, _ev: &gpui::ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.answer_approval(session, request_id, approved, always, cx);
                },
            ))
        };
        let actions = CardActions {
            approve: pick(true, false),
            always: pick(true, true),
            deny: pick(false, false),
        };
        Some(card(&approval, more, now, actions).into_any_element())
    }
}
