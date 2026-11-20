//! The panel for a remote crab: its session's transcript, read-only, with the
//! banner that says which crab you are reading.
//!
//! Deliberately not `transcript::view`. That function takes a
//! `TranscriptState` built from ledger rows, and a remote row is not a ledger
//! row: it has no local session, no cost, no reactions and no message id this
//! window can act on. Forcing it through would mean inventing twenty fields to
//! make a read-only view compile, and the first thing a wrong field would break
//! is the copy and the click handlers the local transcript owns.
//!
//! So this draws the three things a remote transcript actually has: who spoke,
//! what they said, and when. Nothing here is clickable except the composer's
//! send, which is a different action (`message/send`) from anything the local
//! panel does.

use gpui::{
    AnyElement, App, ClickEvent, Context, IntoElement, SharedString, Window, div, prelude::*, rems,
    rgb,
};

use crate::instances::Health;
use crate::palette;
use opencrabs::remote::instance_client::RemoteMessage;

/// One row as the paint reads it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoteRow {
    pub(crate) role: SharedString,
    pub(crate) body: SharedString,
    pub(crate) at: SharedString,
}

impl RemoteRow {
    pub(crate) fn from_message(message: &RemoteMessage) -> Self {
        Self {
            role: SharedString::from(if message.role == "user" { "tu" } else { "crab" }),
            body: SharedString::from(message.content.clone()),
            // The remote stamps unix seconds, the same clock the rail sorts
            // by. The panel prints the local clock and nothing else: a date
            // under every row is noise on a screen that is read all day.
            at: SharedString::from(
                chrono::DateTime::from_timestamp(message.created_at, 0)
                    .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
                    .unwrap_or_default(),
            ),
        }
    }
}

/// What the panel draws for the selected remote.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RemotePanelFrame {
    /// The crab's label, as the owner typed it.
    pub(crate) label: SharedString,
    pub(crate) url: SharedString,
    pub(crate) health: Option<Health>,
    /// The session being read, when one was opened.
    pub(crate) session_title: Option<SharedString>,
    pub(crate) rows: Vec<RemoteRow>,
    /// Why there is nothing to read, when there is nothing to read.
    pub(crate) notice: Option<SharedString>,
    /// A send is in flight; the composer says so and refuses a second.
    pub(crate) sending: bool,
}

/// Draw the banner plus the transcript body.
///
/// `on_send` is minted by the window, because the field's text and the network
/// call both belong to it. Passing it in keeps this module a paint function.
pub(crate) fn view(
    frame: &RemotePanelFrame,
    draft: &SharedString,
    on_send: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let p = palette::current();
    let dot = match frame.health {
        Some(Health::Up { .. }) => p.green,
        Some(Health::Down { .. }) => p.red,
        _ => p.text_faint,
    };

    let banner = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .px_4()
        .py_2()
        .bg(rgb(p.panel))
        .border_b_1()
        .border_color(rgb(p.track))
        .child(
            div()
                .w(rems(0.375))
                .h(rems(0.375))
                .rounded_full()
                .bg(rgb(dot)),
        )
        .child(
            div()
                .text_size(rems(0.75))
                .text_color(rgb(p.text_normal))
                .child(frame.label.clone()),
        )
        .child(
            div()
                .text_size(rems(0.6875))
                .text_color(rgb(p.text_faint))
                .child(frame.url.clone()),
        )
        .when_some(frame.session_title.clone(), |d, title| {
            d.child(
                div()
                    .text_size(rems(0.6875))
                    .text_color(rgb(p.text_muted))
                    .child(SharedString::from(format!("· {title}"))),
            )
        })
        .when(frame.rows.is_empty() && frame.notice.is_none(), |d| {
            d.child(
                div()
                    .text_size(rems(0.6875))
                    .text_color(rgb(p.text_faint))
                    .child(SharedString::from("· escolhe uma sessão na rail")),
            )
        });

    let mut body: Vec<AnyElement> = Vec::with_capacity(frame.rows.len() + 1);
    if let Some(notice) = &frame.notice {
        body.push(
            div()
                .px_4()
                .py_2()
                .text_size(rems(0.75))
                .text_color(rgb(p.warn))
                .child(notice.clone())
                .into_any_element(),
        );
    }
    for row in &frame.rows {
        body.push(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(rgb(p.track))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(rems(0.625))
                                .text_color(rgb(if row.role == "tu" { p.teal } else { p.amber }))
                                .child(row.role.clone()),
                        )
                        .child(
                            div()
                                .text_size(rems(0.625))
                                .text_color(rgb(p.text_faint))
                                .child(row.at.clone()),
                        ),
                )
                .child(
                    div()
                        .text_size(rems(0.8125))
                        .text_color(rgb(p.text))
                        .child(row.body.clone()),
                )
                .into_any_element(),
        );
    }

    // The composer is one line and one button. It says what it will do
    // (`message/send` to this crab) so nobody mistakes it for the local one.
    let composer = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .px_4()
        .py_2()
        .border_t_1()
        .border_color(rgb(p.track))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(p.panel))
                .border_1()
                .border_color(rgb(p.track))
                .text_size(rems(0.75))
                .text_color(if draft.is_empty() {
                    rgb(p.text_faint)
                } else {
                    rgb(p.text)
                })
                .child(if draft.is_empty() {
                    SharedString::from("mensagem para este crab (Enter envia)")
                } else {
                    draft.clone()
                }),
        )
        .child(
            div()
                .px_3()
                .py_1()
                .rounded_md()
                .when(frame.sending, |d| d.opacity(0.5))
                .when(!frame.sending, |d| d.cursor_pointer())
                .bg(rgb(p.accent))
                .text_size(rems(0.75))
                .text_color(rgb(p.bg))
                .child(SharedString::from(if frame.sending {
                    "a enviar…"
                } else {
                    "enviar"
                }))
                .on_click(move |ev, window, app| {
                    on_send(ev, window, app);
                }),
        );

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .h_full()
        .child(banner)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .children(body),
        )
        .child(composer)
        .into_any_element()
}
