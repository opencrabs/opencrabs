//! The panel for a remote crab: its session's transcript, read-only, plus the
//! chrome a remote needs and a local chat does not (a banner naming the crab,
//! and a composer that sends to it).
//!
//! The body is `transcript::view`, the same function the window's own chat
//! draws with. That is the whole point. Daniel read a remote session and saw a
//! different product, because the first version of this module painted
//! `content` raw, and the second hand-rolled its own markdown on top. A remote
//! answer has to read exactly like a local one: same turn cards, same fold
//! header, same thinking block, same table layout, same code fences. Two
//! painters of the same data drift the moment either one changes, and the drift
//! is invisible until someone puts both side by side.
//!
//! What this module owns, then, is the conversion (`RemoteMessage` is not a
//! `Message`) and the chrome. Everything else is the transcript's: what counts
//! as a row, how reasoning folds, how a 40 KB answer is capped, which anchors
//! exist. Re-deciding any of that here is how the two views started
//! disagreeing.

use std::collections::HashMap;

use gpui::{
    AnyElement, App, ClickEvent, IntoElement, ScrollHandle, SharedString, Window, div, prelude::*,
    px, rems, rgb,
};

use crate::instances::Health;
use crate::palette;
use crate::transcript;
use opencrabs::db::models::Message;
use opencrabs::remote::instance_client::RemoteMessage;

/// Build the transcript state a remote session paints with.
///
/// The rows are converted, never filtered. `turns_with_origins` and
/// `row_blocks` already decide what is conversation: a `[SYSTEM: ...]` seed is
/// not, a `[CONTEXT COMPACTION` marker is not, and reasoning folds into a
/// `Thinking` block instead of leaking into the answer. Letting the transcript
/// apply those rules is the only way a remote panel stays identical to the
/// local one when those rules change.
///
/// Called once per fetch, not per paint: the turn split and the row walk cost
/// real work on a 216 KB session, and a paint must not redo it.
pub(crate) fn state_for(
    session_id: &str,
    messages: &[RemoteMessage],
) -> transcript::TranscriptState {
    let session = uuid::Uuid::parse_str(session_id).ok();
    // A remote id that is not a UUID still needs a stable row id: the transcript
    // keys its handlers and reaction chips by it, and two rows sharing one id
    // would make one of them unclickable. Falling back to the row's position
    // keeps it unique inside the session and stable across a re-read of it.
    let rows: Vec<Message> = messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let id = uuid::Uuid::parse_str(&m.id)
                .unwrap_or_else(|_| uuid::Uuid::from_u128((i as u128) + 1));
            let mut row = Message::new(
                session.unwrap_or_else(uuid::Uuid::nil),
                m.role.clone(),
                m.content.clone(),
                i as i32,
            );
            row.id = id;
            row.created_at =
                chrono::DateTime::from_timestamp(m.created_at, 0).unwrap_or_else(chrono::Utc::now);
            // Everything the painter reads from a row travels with the row, and
            // these are the columns that make a remote answer look like a local
            // one: the reasoning it folds, the calls the card counts, the clock,
            // the ctx budget, the rate, and whether the row continues the turn
            // above it. Copying them here, instead of re-deciding any of it in
            // this module, is what keeps the two views from drifting.
            row.thinking = m.thinking.clone();
            row.tool_calls = m.tool_calls;
            row.duration_secs = m.duration_secs;
            row.ctx_window = m.ctx_window;
            row.tokens_per_sec = m.tokens_per_sec;
            row.input_tokens = m.input_tokens;
            row.continuation = m.continuation;
            row
        })
        .collect();
    let total = rows.len() as i64;
    // The card's footer comes from the rows themselves: `TurnMetrics::from_row`
    // reads `input_tokens`, `ctx_window`, `tokens_per_sec` and `tool_calls` off
    // the turn's settled row, which is the same road a local card takes. This
    // map is only the fallback for a turn whose row lost those columns, and a
    // remote window has nothing to put there that the host did not already put
    // on the row.
    //
    // Origins are different: they are stamped onto rows by a turn that settled
    // in *this* database, and this window has never seen these turns settle. An
    // empty map is the honest answer; the transcript already draws a turn
    // without an origin chip.
    let metrics: HashMap<String, transcript::TurnMetrics> = HashMap::new();
    let origins: HashMap<uuid::Uuid, String> = HashMap::new();
    let (hidden, turns) = transcript::turns_with_origins(&rows, total, &metrics, &origins);
    transcript::TranscriptState {
        session,
        hidden,
        turns,
        rows,
        ..Default::default()
    }
}

/// The chrome around a remote transcript.
///
/// Deliberately holds no transcript state: `TranscriptState` is not `Clone`, and
/// cloning 216 KB of rows on every paint to hand the painter its own copy would
/// be a worse bug than the one being fixed. The state travels beside the frame.
#[derive(Default)]
pub(crate) struct RemotePanelFrame {
    /// The crab's label, as the owner typed it.
    pub(crate) label: SharedString,
    pub(crate) url: SharedString,
    pub(crate) health: Option<Health>,
    /// The session being read, when one was opened.
    pub(crate) session_title: Option<SharedString>,
    /// Whether the state holds anything to draw. `turns` alone is not enough: a
    /// session whose rows are all seeds has turns and nothing to read.
    pub(crate) has_rows: bool,
    /// Why there is nothing to read, when there is nothing to read.
    pub(crate) notice: Option<SharedString>,
    /// A send is in flight; the composer says so and refuses a second.
    pub(crate) sending: bool,
    /// The row whose copy just landed, so its icon shows the check and then
    /// loses it. This is the window's own mark, which is what makes a remote
    /// answer acknowledge a copy exactly as a local one does.
    pub(crate) copied: Option<uuid::Uuid>,
}

/// The hooks a read-only transcript needs.
///
/// Every handler is empty on purpose except copy. A remote row has no local id
/// to react to, no pencil this window can open, no composer to quote it into,
/// and nothing on another machine's disk that this one may edit. Copy is the
/// exception because it needs nothing from the row but its text, which the
/// window already holds: `msg_copy_markdown` copies the string it is handed and
/// never reads SQLite, so it is safe on a message this database has never seen.
/// Without it a remote answer loses the affordance a reader uses most, and that
/// alone makes the panel feel like a different product.
fn read_only_hooks(
    copy: HashMap<uuid::Uuid, transcript::ClickHandler>,
    copied: Option<uuid::Uuid>,
) -> transcript::ViewHooks {
    transcript::ViewHooks {
        attachments: HashMap::new(),
        drag: None,
        wheel: None,
        middle_click: None,
        message_click: HashMap::new(),
        message_press: HashMap::new(),
        selection: None,
        menu_dismiss: None,
        ctx_used: None,
        ctx_max: 0,
        live_click: None,
        live_press: HashMap::new(),
        live_edit: HashMap::new(),
        older: None,
        reactions: HashMap::new(),
        reaction_click: HashMap::new(),
        edit: HashMap::new(),
        copy,
        copied,
        edit_row: None,
        voice_pending: false,
        // A remote transcript follows nothing: no live turn lands under it, so
        // there is no bottom to chase.
        following: false,
    }
}

/// Draw the banner, the transcript body and the remote composer.
///
/// `body` is the session's state and its scroll, or `None` when no session is
/// open. It is an `Option` rather than an empty state because `TranscriptState`
/// is not `Clone`: handing the painter a borrowed pair keeps a 200 KB session
/// out of the per-paint copy path.
///
/// `handlers` are the fold clicks, minted by the window because only its own
/// `cx` can build one. Without them the turn cards draw folded and refuse to
/// open, which is the one interaction a remote reader actually needs on a
/// 40-row session.
#[allow(clippy::too_many_arguments)]
pub(crate) fn view(
    frame: &RemotePanelFrame,
    body: Option<(&transcript::TranscriptState, &ScrollHandle)>,
    measure: f32,
    rem_size: f32,
    handlers: HashMap<String, transcript::ClickHandler>,
    // One copy handler per answer row, minted by the window because the
    // clipboard and the "copied" tick live there. Empty would mean the reader
    // can read a remote answer but not take it, which is half the interaction
    // a local answer offers (#2750).
    copy: HashMap<uuid::Uuid, transcript::ClickHandler>,
    // The field itself, not its text: the composer draws a caret from the
    // spans, and a `SharedString` cannot say where the next character lands.
    draft: &opencrabs::ui_core::text_field::TextField,
    on_send: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
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
        .border_b_1()
        .border_color(rgb(p.track))
        .child(
            div()
                .flex_none()
                .w(rems(0.375))
                .h(rems(0.375))
                .rounded_full()
                .bg(rgb(dot)),
        )
        .child(
            div()
                .flex_none()
                .text_size(rems(0.75))
                .text_color(rgb(p.text_normal))
                .child(frame.label.clone()),
        )
        .child(
            div()
                .flex_none()
                .text_size(rems(0.6875))
                .text_color(rgb(p.text_faint))
                .child(frame.url.clone()),
        )
        .when_some(frame.session_title.clone(), |d, title| {
            d.child(
                div()
                    .min_w_0()
                    .text_size(rems(0.6875))
                    .text_color(rgb(p.text_muted))
                    .child(SharedString::from(format!("· {title}"))),
            )
        })
        .when(!frame.has_rows && frame.notice.is_none(), |d| {
            d.child(
                div()
                    .text_size(rems(0.6875))
                    .text_color(rgb(p.text_faint))
                    .child(SharedString::from("· escolhe uma sessão na rail")),
            )
        })
        .child(div().flex_1())
        // "desligar" closes the view; it does not touch the store. Forgetting a
        // crab is its badge's right-click menu, because that costs the pairing
        // and a fresh code is a round trip to the other machine.
        .child(
            div()
                .id("remote-disconnect")
                .flex_none()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .text_size(rems(0.6875))
                .text_color(rgb(p.text_faint))
                .hover(|d| d.text_color(rgb(p.red)))
                .on_click(on_remove)
                .child(SharedString::from("desligar")),
        );

    // The body is the window's own transcript painter. Read-only falls out of
    // the empty hooks, not out of a second renderer.
    let body: AnyElement = match body.filter(|_| frame.has_rows) {
        Some((state, scroll)) => transcript::view(
            state,
            scroll,
            &transcript::LiveTurn::default(),
            false,
            measure,
            rem_size,
            handlers,
            read_only_hooks(copy, frame.copied),
        )
        .into_any_element(),
        None => {
            let mut col = div().flex().flex_col().gap_2().px_4().py_3();
            if let Some(notice) = &frame.notice {
                col = col.child(
                    div()
                        .text_size(rems(0.75))
                        .text_color(rgb(p.warn))
                        .child(notice.clone()),
                );
            }
            col.into_any_element()
        }
    };

    // The composer closes the panel the way the local one closes the chat:
    // the same shell (glass fill, hairline border, the messages' radius, a
    // one pixel divider above the hints) and a field that shows a caret. A
    // remote that drew a plain box with a green button read as another
    // product, which is what the owner called out (#3133).
    let (before, selected, after) = draft.render_spans();
    let empty = draft.is_empty();
    // A caret of its own: the field is not a real text input, so without this
    // bar there is no way to see where the next character lands.
    // 2 px, the local caret's width. The blink is the editor's own clock and
    // this field has none: a bar that stays lit is the honest version of a
    // caret nobody is animating.
    let caret = div().flex_none().w(px(2.)).h(rems(0.95)).bg(rgb(p.accent));
    let mut line = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .w_full()
        .min_w_0();
    // The bar belongs where the next character will land, not at the
    // selection's far edge: after a backwards selection (shift+Left) the
    // caret sits at the selection's start, and painting it after the
    // highlighted run put it a word away from the text it would replace.
    let caret_at_start = draft
        .selection()
        .is_some_and(|(start, _)| draft.caret() == start);
    if empty {
        // The placeholder is text, not a position: the bar goes before it, so
        // an empty composer reads as "typing starts here" instead of putting
        // the caret past the hint.
        line = line.child(caret).child(
            div()
                .text_color(rgb(p.text_faint))
                // The local composer's own string, from the constant it
                // paints with: same field, same words (#3133).
                .child(SharedString::from(crate::editor::PLACEHOLDER)),
        );
    } else if caret_at_start {
        line = line.child(div().child(SharedString::from(before.to_string())));
        line = line.child(caret);
        if !selected.is_empty() {
            line = line.child(
                div()
                    .bg(rgb(p.selection))
                    .child(SharedString::from(selected.to_string())),
            );
        }
        line = line.child(div().child(SharedString::from(after.to_string())));
    } else {
        line = line.child(div().child(SharedString::from(before.to_string())));
        if !selected.is_empty() {
            line = line.child(
                div()
                    .bg(rgb(p.selection))
                    .child(SharedString::from(selected.to_string())),
            );
        }
        line = line
            .child(caret)
            .child(div().child(SharedString::from(after.to_string())));
    }

    // The field's own box, to the local composer's measurements: `card_inset`
    // for the side step so the words land on the messages' column, `py_3` and a
    // 44 px floor so the empty box is the same slab as the one in the chat, and
    // 14 px on a 22 px line so a line of text is the same height in both
    // (#3114, #3133). A remote field that was 13 px in a 32 px box read as a
    // different control even with the same shell around it.
    let field = div()
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .px(transcript::card_inset())
        .py_3()
        .line_height(px(22.))
        .text_size(px(14.))
        .min_h(px(44.))
        .text_color(rgb(p.text))
        .child(line);

    // The footer carries the two chords instead of a second button: Enter is
    // the send, exactly as it is in the local composer, and Esc is the way
    // back to this window's own crab.
    let footer = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .px_3()
        .py_1()
        .child(
            div()
                .flex_none()
                .text_size(rems(0.625))
                .text_color(rgb(p.text_faint))
                .child(SharedString::from("enter envia · esc volta ao meu crab")),
        )
        .child(div().flex_1())
        .child(
            div()
                .id("remote-send")
                .flex_none()
                .px_2()
                .rounded_md()
                .when(frame.sending, |d| d.opacity(0.5))
                .when(!frame.sending, |d| d.cursor_pointer())
                .text_size(rems(0.625))
                .text_color(rgb(p.accent))
                .hover(|d| d.bg(rgb(p.panel_hi)))
                .child(SharedString::from(if frame.sending {
                    "a enviar…"
                } else {
                    "enviar"
                }))
                .on_click(on_send),
        );

    // No `w_full` here: the panel is a column, so the card stretches to it and
    // the side margins inset it instead of pushing it past the edge.
    let composer = div()
        .flex()
        .flex_col()
        .flex_none()
        .min_w_0()
        .mx_4()
        .mt_1()
        .mb_2()
        .bg(opencrabs::ui_core::card::glass_fill(p.panel))
        .border_1()
        .border_color(rgb(p.panel_hi))
        .rounded(px(transcript::MESSAGE_RADIUS))
        .overflow_hidden()
        .child(field)
        .child(div().w_full().h(px(1.)).bg(rgb(p.panel_hi)))
        .child(footer);

    // A send's answer has to be readable whatever the body holds. It used to
    // live only in the empty-body branch, so a reply that landed in a session
    // with messages was written nowhere the owner could see it, which is the
    // other half of "não consigo enviar mensagem" (#3133).
    let notice = frame
        .notice
        .clone()
        .filter(|_| frame.has_rows)
        .map(|notice| {
            div()
                .mx_4()
                .mt_1()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(p.panel_hi))
                .text_size(rems(0.6875))
                .text_color(rgb(p.warn))
                .child(notice)
        });

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .h_full()
        .child(banner)
        .child(div().flex().flex_col().flex_1().min_h_0().child(body))
        .children(notice)
        .child(composer)
        .into_any_element()
}
