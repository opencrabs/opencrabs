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
//! What it does share with the local panel is the reading experience, because
//! a remote crab that renders as plain text is a different product: the stored
//! row goes through the same two steps the transcript uses. `split_segments`
//! folds the chain of thought away from the answer, and `markdown::blocks_flow`
//! parses what is left. Painting `content` raw was the bug Daniel hit on the
//! Lenovo: `<!-- reasoning -->` markers, `[SYSTEM: ...]` scaffolding and a wall
//! of asterisks in the chat, and a 230 KB row that took seconds to lay out.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, IntoElement, SharedString, Window, div, prelude::*, rems, rgb,
};

use crate::instances::Health;
use crate::markdown::{self, MdBlock, MdFlow};
use crate::palette;
use crate::transcript;
use opencrabs::remote::instance_client::RemoteMessage;
use opencrabs::tui::app::reasoning_split::{Segment, split_segments};
use opencrabs::utils::internal_message::is_internal_system_message;

/// How much of a folded block (thinking, retained narration) reaches the
/// screen. The fold exists to keep the answer readable; a paragraph of it is
/// enough to see the shape of the reasoning, and the whole chain is what made
/// the panel slow in the first place.
const FOLDED_MAX: usize = 480;

/// Ceiling on one answer segment. Answers are meant to be read, so this is
/// generous; it is here so a runaway row cannot cost a second per frame.
const ANSWER_MAX: usize = 12_000;

/// One piece of a remote row, already parsed.
#[derive(Clone)]
pub(crate) enum RemotePart {
    /// Visible words, parsed as markdown by the transcript's own renderer.
    Answer(Rc<Vec<MdBlock>>),
    /// Internal text the local panel folds away: thinking, retained narration,
    /// a tool ledger reduced to a count. Muted, labelled and capped.
    Folded {
        label: &'static str,
        body: SharedString,
    },
}

/// One row as the paint reads it.
#[derive(Clone)]
pub(crate) struct RemoteRow {
    pub(crate) role: SharedString,
    pub(crate) at: SharedString,
    pub(crate) parts: Vec<RemotePart>,
}

impl RemoteRow {
    /// `None` for a row the local panel would not draw either: a compaction
    /// marker, or the harness talking to itself. Both live in the stored
    /// transcript and neither is conversation.
    pub(crate) fn from_message(message: &RemoteMessage) -> Option<Self> {
        let content = message.content.as_str();
        if content.starts_with("[CONTEXT COMPACTION") {
            return None;
        }
        if message.role == "user" && is_internal_system_message(content) {
            return None;
        }
        let parts = parts_for(&message.role, content);
        if parts.is_empty() {
            return None;
        }
        Some(Self {
            role: SharedString::from(if message.role == "user" { "tu" } else { "crab" }),
            // The remote stamps unix seconds, the same clock the rail sorts
            // by. The panel prints the local clock and nothing else: a date
            // under every row is noise on a screen that is read all day.
            at: SharedString::from(
                chrono::DateTime::from_timestamp(message.created_at, 0)
                    .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
                    .unwrap_or_default(),
            ),
            parts,
        })
    }
}

/// Split one stored row into what may be read and what must be folded.
///
/// Mirrors `transcript::row_blocks`: assistant rows go through the reasoning
/// split, every other row is taken at face value. A user row is parsed as
/// verbatim so a soft break stays a line break, which is how the local panel
/// treats typed text.
fn parts_for(role: &str, content: &str) -> Vec<RemotePart> {
    if role != "assistant" {
        let (text, _) = strip_html_comments(content);
        return answer_parts(&text, MdFlow::Verbatim);
    }
    // Split FIRST. The reasoning markers are themselves HTML comments, so
    // stripping comments before the split would delete the block boundaries
    // and dump the chain of thought into the answer as plain text.
    let mut parts: Vec<RemotePart> = Vec::new();
    let mut tools = 0usize;
    for segment in split_segments(content) {
        match segment {
            Segment::Reasoning(body) => push_folded(&mut parts, "pensamento", &body),
            Segment::Blocked(body) => push_folded(&mut parts, "narração retida", &body),
            Segment::Text(body) => {
                let (text, found) = strip_html_comments(&body);
                tools += found;
                parts.extend(answer_parts(&text, MdFlow::Prose));
            }
        }
    }
    // A row whose only content was a tool ledger has no words to show, but
    // dropping it silently would hide that the crab did something. One muted
    // line says it without pasting the ledger's markup into the chat.
    if parts.is_empty() && tools > 0 {
        parts.push(RemotePart::Folded {
            label: "ferramentas",
            body: SharedString::from(format!("{tools} chamada(s) de ferramenta, sem texto")),
        });
    }
    parts
}

/// Parse the visible words of a row, capped so one row cannot own the frame.
fn answer_parts(text: &str, flow: MdFlow) -> Vec<RemotePart> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let (capped, _) = capped(trimmed, ANSWER_MAX);
    let blocks = markdown::blocks_flow(&capped, flow);
    if blocks.is_empty() {
        return Vec::new();
    }
    vec![RemotePart::Answer(Rc::new(blocks))]
}

/// Push one folded piece, dropping it when there is nothing in it.
fn push_folded(parts: &mut Vec<RemotePart>, label: &'static str, body: &str) {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return;
    }
    let (capped, _) = capped(trimmed, FOLDED_MAX);
    parts.push(RemotePart::Folded {
        label,
        body: SharedString::from(capped),
    });
}

/// Cut on a character boundary, with one ellipsis. Written over `chars()`
/// rather than a byte range because the byte-slice ratchet counts every new
/// `[..]` site and this file has no business adding one.
fn capped(text: &str, max: usize) -> (String, bool) {
    let mut out = String::with_capacity(text.len().min(max));
    let mut rest = text.chars();
    for _ in 0..max {
        match rest.next() {
            Some(ch) => out.push(ch),
            None => return (out, false),
        }
    }
    if rest.next().is_some() {
        out.push('…');
        (out, true)
    } else {
        (out, false)
    }
}

/// Remove `<!-- ... -->` spans, counting the tool ledgers among them.
///
/// The stored row keeps its ledgers and its marker comments inline. The local
/// panel turns them into blocks of their own; this one has no ledger UI, so
/// the markup is dropped rather than painted as dim HTML, which is what the
/// user saw as strange characters in the chat.
fn strip_html_comments(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut tools = 0usize;
    let mut rest = text;
    loop {
        let Some((head, tail)) = rest.split_once("<!--") else {
            out.push_str(rest);
            break;
        };
        out.push_str(head);
        let Some((inside, after)) = tail.split_once("-->") else {
            // Unterminated: everything from here is markup that never closed.
            break;
        };
        if inside.trim_start().starts_with("tools") {
            tools += 1;
        }
        rest = after;
    }
    (out, tools)
}

/// What the panel draws for the selected remote.
#[derive(Default)]
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
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
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
        })
        // Closing the view, not forgetting the crab: the pairing stays in the
        // store and the badge stays in the strip, so "desligar" costs nothing
        // and the next look is one click. Forgetting lives on the badge's
        // right-click menu, where the press is deliberate.
        .child(div().flex_1())
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
                .on_click(on_close)
                .child(SharedString::from("desligar")),
        );

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
        let mut content = div().flex().flex_col().gap_2().min_w_0();
        for part in &row.parts {
            content = match part {
                // The transcript's own renderer: same markdown, same tables,
                // same reading size as a local reply.
                RemotePart::Answer(blocks) => content.child(transcript::document_blocks(blocks)),
                RemotePart::Folded { label, body } => content.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .pl_2()
                        .border_l_1()
                        .border_color(rgb(p.track))
                        .child(
                            div()
                                .text_size(rems(0.625))
                                .text_color(rgb(p.text_faint))
                                .child(SharedString::from(*label)),
                        )
                        .child(
                            div()
                                .text_size(rems(0.75))
                                .text_color(rgb(p.text_muted))
                                .child(body.clone()),
                        ),
                ),
            };
        }
        body.push(
            div()
                .flex()
                .flex_col()
                .gap_2()
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
                .child(content)
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
                .id("remote-send")
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
            // A scroll container, not a clip: `overflow_hidden` cut every row
            // past the fold, so a long remote answer was simply gone with no
            // way to reach it. The id is what makes `overflow_y_scroll`
            // resolve at all (it lives on `StatefulInteractiveElement`).
            div()
                .id("remote-body")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(body),
        )
        .child(composer)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(role: &str, content: &str) -> RemoteMessage {
        RemoteMessage {
            id: String::from("m1"),
            role: role.to_string(),
            content: content.to_string(),
            created_at: 1_760_000_000,
        }
    }

    fn visible(row: &RemoteRow) -> String {
        row.parts
            .iter()
            .filter_map(|part| match part {
                RemotePart::Answer(blocks) => Some(
                    blocks
                        .iter()
                        .filter_map(|b| match b {
                            MdBlock::Line(line) => Some(line.text.clone()),
                            MdBlock::Table(_) => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                RemotePart::Folded { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn folded_labels(row: &RemoteRow) -> Vec<&'static str> {
        row.parts
            .iter()
            .filter_map(|part| match part {
                RemotePart::Folded { label, .. } => Some(*label),
                RemotePart::Answer(_) => None,
            })
            .collect()
    }

    #[test]
    fn reasoning_is_folded_out_of_the_answer() {
        let message = row(
            "assistant",
            "<!-- reasoning -->\nI should think about this.\n<!-- /reasoning -->\n\nThe answer.",
        );
        let row = RemoteRow::from_message(&message).expect("a row with an answer is drawn");
        assert_eq!(folded_labels(&row), vec!["pensamento"]);
        let text = visible(&row);
        assert!(text.contains("The answer."), "got {text:?}");
        assert!(
            !text.contains("I should think"),
            "thinking leaked into the answer: {text:?}"
        );
    }

    #[test]
    fn the_harness_talking_to_itself_is_not_a_row() {
        let seed = row(
            "user",
            "[SYSTEM: Setup just finished. This session runs on the opencrabs provider.]",
        );
        assert!(RemoteRow::from_message(&seed).is_none());
        let compaction = row("user", "[CONTEXT COMPACTION] summary of everything");
        assert!(RemoteRow::from_message(&compaction).is_none());
    }

    #[test]
    fn a_tool_ledger_becomes_a_count_not_markup() {
        let message = row(
            "assistant",
            "<!-- tools: read_file, bash -->\n<!-- /tools -->",
        );
        let row = RemoteRow::from_message(&message).expect("the call is still worth a line");
        assert_eq!(folded_labels(&row), vec!["ferramentas"]);
        let text = visible(&row);
        assert!(
            text.is_empty(),
            "ledger markup reached the answer: {text:?}"
        );
    }

    #[test]
    fn markdown_is_parsed_instead_of_painted_raw() {
        let message = row("assistant", "## Título\n\n**forte** e `código`");
        let row = RemoteRow::from_message(&message).expect("drawn");
        let text = visible(&row);
        assert!(text.contains("Título"), "got {text:?}");
        assert!(
            !text.contains("**") && !text.contains("##"),
            "markers survived the parse: {text:?}"
        );
    }

    #[test]
    fn a_huge_reasoning_block_is_capped() {
        let huge = "a".repeat(50_000);
        let message = row(
            "assistant",
            &format!("<!-- reasoning -->\n{huge}\n<!-- /reasoning -->\n\nok"),
        );
        let row = RemoteRow::from_message(&message).expect("drawn");
        let folded: Vec<usize> = row
            .parts
            .iter()
            .filter_map(|part| match part {
                RemotePart::Folded { body, .. } => Some(body.chars().count()),
                RemotePart::Answer(_) => None,
            })
            .collect();
        assert_eq!(folded.len(), 1);
        assert!(
            folded[0] <= FOLDED_MAX + 1,
            "fold kept {} chars, cap is {FOLDED_MAX}",
            folded[0]
        );
    }
}
