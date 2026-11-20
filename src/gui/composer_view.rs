//! The composer's strips and popups: the queued list, the follow-up offers,
//! and the two lists that hang over the input.
//!
//! Everything here draws from state the window already holds. The rows arrive
//! with their click handlers minted, because only the window knows which
//! session a queue or an offer belongs to, so nothing here reaches the
//! database.
//!
//! Moved out of the crate root (#2817 follow-up) as pure code motion: the
//! functions and their comments are unchanged, only the module boundary is
//! new. The slash list's doc comment sat above the emoji list's and now sits
//! on the function it describes.

use gpui::{AnyElement, Div, IntoElement, SharedString, div, prelude::*, px, rgb, rgba};

use opencrabs::brain::tools::suggest_options::{SuggestionItem, SuggestionStyle};
use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill, session_edge};
use opencrabs::ui_core::commands as slash;
use opencrabs::ui_core::hints::{HintState, task_line};

use std::ops::Range;

use crate::agent_bridge;
use crate::editor;
use crate::palette;
use crate::topic_icons;
use crate::transcript;

/// The queued messages, above the input where the user can see what will go
/// out next. Same shape as the TUI's pending list: one line per message, the
/// count in the header, and a word on when they send.
///
/// Past two messages the list folds to its head and the rest is a count, so a
/// busy session cannot push the conversation off the screen; `expanded` is the
/// click that opens it (#2629).
pub(crate) fn queue_strip(queued: &[agent_bridge::PendingMessage], expanded: bool) -> Div {
    let folded = !expanded && queued.len() > 2;
    let shown = queue_rows(queued.len(), expanded);
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().panel))
        .border_1()
        .border_color(session_edge(palette::current().panel_hi))
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().amber))
                .child(SharedString::from(format!("queued {}", queued.len()))),
        );
    for (i, m) in queued.iter().take(shown).enumerate() {
        let text = m.display_text.lines().next().unwrap_or_default();
        let mut chars = text.chars();
        let short: String = chars.by_ref().take(96).collect();
        let suffix = if chars.next().is_some() { "…" } else { "" };
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(format!("{}. {short}{suffix}", i + 1))),
        );
    }
    if queued.len() > shown {
        let rest = queued.len() - shown;
        col = col.child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from(if folded {
                    format!("… {rest} more · click to expand")
                } else {
                    format!("… {rest} more")
                })),
        );
    }
    col
}

/// The follow-up options `suggest_options` offered, above the composer where
/// the queued list sits. One button per label, styled the way the channel
/// buttons are: primary in the accent, danger in the red, default quiet. A
/// click sends that exact label as the next turn (#2708).
///
/// Drawn as a card from the Seeker's family (#3101): its radius, its glass
/// fill, and its shadow, so an offer reads as the same kind of floating
/// surface as the command card rather than a bare list of words. The rows
/// stack one above the other instead of wrapping side by side (#3114): a card
/// wider than the composer pushes the conversation around, and a label sharing
/// its line with a neighbour is truncated while there is room to read it on a
/// row of its own.
///
/// `focused` is the row the keyboard stands on, `None` while the field has the
/// caret. Up from the composer hands the keyboard here instead of walking
/// history (#3101), which is what the focus ring exists to show.
///
/// The rows arrive with their handlers already minted by the window, because
/// only it knows which session the offer belongs to. Nothing here reaches the
/// database: see `agent_bridge::SUGGESTIONS` for why there is nothing to
/// persist.
pub(crate) fn options_strip(
    rows: &[(SuggestionItem, transcript::ClickHandler)],
    focused: Option<usize>,
) -> Div {
    let p = palette::current();
    // The card is the buttons alone, with no title row (#3103). A heading
    // repeating "suggested" said what the buttons already say, it cost a line
    // of height directly above the composer, and its text sat tight against
    // the card's own curve. The keys went with it and lost nothing:
    // `ui_core::hints` already prints the arrows, enter and the way back out on
    // the hint row above the composer whenever an offer is waiting, so the
    // legend was being drawn twice.
    let col = div()
        .flex()
        .flex_col()
        // The buttons hug their labels (#3114): a column stretches its children
        // to the card's width by default, so every row was a full-width bar with
        // its words bunched at the left and an empty half that read as part of
        // the button. `items_start` makes the button column take its own width,
        // and the widest label in the card is what every button then shares: one
        // size, set by the text instead of by the window.
        .items_start()
        .gap_2()
        .p_3()
        // The card wears the message bubble's curve and fill (#3114), not the
        // Seeker's 32 and the raised panel: it sits in the same column as the
        // messages above it, so a rounder, lighter card read as a panel from a
        // different window dropped into the conversation. The shadow stays,
        // because this one floats over the session ground rather than lying in
        // it, and the fill alone would not lift it off that ground.
        .rounded(px(transcript::MESSAGE_RADIUS))
        .bg(glass_fill(transcript::message_card_surface()))
        .border_1()
        .border_color(session_edge(p.panel_hi))
        .shadow_2xl();
    // One offer per row (#3114). They used to wrap side by side, so two
    // labels split the card's width and a long one was cut while a row of its
    // own had room for all of it. The column also makes the keyboard's walk
    // read the way it behaves: the arrows move down a list, not across a
    // paragraph.
    let mut buttons = div().flex().flex_col().gap_1();
    for (i, (item, pick)) in rows.iter().enumerate() {
        let (ink, wash) = match item.style {
            SuggestionStyle::Primary => (p.accent, rgba((p.accent << 8) | 0x1c)),
            SuggestionStyle::Danger => (p.red, rgba((p.red << 8) | 0x1c)),
            SuggestionStyle::Default => (p.text_normal, rgba(0xffffff0d)),
        };
        let on = focused == Some(i);
        let pick = pick.clone();
        // The keyboard's row is the one Enter would send, so it carries the
        // accent edge and the return mark. A pointer row lifts on hover
        // instead: the same wash, one step quieter, so two rows can never look
        // equally armed.
        buttons = buttons.child(
            div()
                .id(SharedString::from(format!("option-row-{i}")))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                // One step inside the card's own 12, so a row's corner never
                // crosses the curve it sits in (#3114).
                .rounded(px(transcript::MESSAGE_RADIUS - 4.0))
                .border_1()
                .border_color(if on {
                    rgba((ink << 8) | 0x88)
                } else {
                    rgba(0xffffff1a)
                })
                .when(on, |d| d.bg(wash))
                .when(!on, |d| {
                    d.hover(|d| d.bg(rgba(0xffffff14)).border_color(rgba(0xffffff33)))
                })
                .text_xs()
                .text_color(rgb(ink))
                .cursor_pointer()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(rgb(p.text_faint))
                        .child(SharedString::from(format!("{}", i + 1))),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(item.label.clone())),
                )
                .when(on, |d| {
                    d.child(
                        div()
                            .flex_shrink_0()
                            .text_color(rgb(p.text_faint))
                            .child(SharedString::from("↵")),
                    )
                })
                .on_click(move |ev, window, app| pick(ev, window, app)),
        );
    }
    col.child(buttons)
}

/// Whether a speech-to-text engine is configured, which decides what the
/// composer may promise (#3044).
///
/// `Config::current()` is the in-memory mirror behind an `Arc`, so this is a
/// pointer clone plus the small derivation `voice_config` builds, never a disk
/// read. It is asked on the UI thread only, the same way the recorder asks
/// before it sends a take: a window with no engine cannot record, so the hint
/// names `/voice` instead of a chord that would only print an error.
pub(crate) fn voice_ready() -> bool {
    opencrabs::config::Config::current()
        .voice_config()
        .stt_enabled
}

/// What the empty field says: the product's prompt plus the keys that act on
/// the field itself (#3102).
///
/// These stay in the field rather than moving to the card above the composer,
/// because they are about the field: Esc stops the turn being written into it,
/// the chord records a message into it. The card speaks about the work.
///
/// `/voice` is the STT/TTS settings screen, not a switch that turns a mic on,
/// so the cold line names it as setup rather than as the way to record
/// (#3102). Recording itself has two doors, the chord and the mic button.
pub(crate) fn composer_hint_for(busy: bool, queued: usize, voice: bool) -> SharedString {
    match (busy, queued) {
        (true, 0) => SharedString::from("esc stops · typing queues"),
        (true, n) => SharedString::from(format!("queued {n} · ↑ recalls · esc stops")),
        (false, 0) if voice => {
            SharedString::from("Talk to the crab… · ctrl+alt+r to record, same key to send")
        }
        (false, 0) => SharedString::from("Talk to the crab… · /voice opens the voice settings"),
        (false, n) => SharedString::from(format!("{n} queued · sending now")),
    }
}

/// The task line the card above the composer paints right now, if any (#3102).
///
/// The card is about the work, so it is filtered through the commands the
/// desktop app actually answers: a line naming a command this surface does not
/// run is worse than no line at all.
pub(crate) fn task_line_for(state: &HintState) -> Option<SharedString> {
    task_line(state, |command| {
        slash::support_gui(command) == slash::Support::Runs
    })
    .map(SharedString::from)
}

/// How many queued messages stay on screen. Two, until the user opens the
/// fold, so a burst of background reports never owns the window (#2629).
pub(crate) fn queue_rows(len: usize, expanded: bool) -> usize {
    if expanded || len <= 2 { len } else { 2 }
}

/// Which slice of the list the popup paints.
///
/// The window never moves until the selection walks past its last row, so the
/// eye keeps a stable block of names instead of a list that scrolls on every
/// arrow. A selection outside the range is clamped, because the list shrinks
/// as the draft grows.
pub(crate) fn popup_window(total: usize, selected: usize, visible: usize) -> Range<usize> {
    let visible = visible.max(1);
    let shown = total.min(visible);
    let selected = selected.min(total.saturating_sub(1));
    let start = selected.saturating_sub(visible - 1).min(total - shown);
    start..start + shown
}

/// The emoji list under a `:query` (#2413): the glyph and its shortcode per row,
/// the highlighted row marked as the command list marks its own.
pub(crate) fn emoji_popup_view(items: &[editor::EmojiRow], selected: usize) -> Div {
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(glass_fill(palette::current().panel));
    for (index, row) in items.iter().enumerate() {
        let on = index == selected;
        // A crab row is one of our pictures (#2769): it has no character to
        // draw, so the row shows the picture the chat will receive.
        let face: AnyElement = match row.crab {
            Some(bytes) => topic_icons::picture(bytes, 1.6).into_any_element(),
            None => SharedString::from(row.glyph.to_string()).into_any_element(),
        };
        col = col.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .px_2()
                .py_0p5()
                .rounded(px(4.0))
                .text_color(rgb(palette::current().text))
                // A subtle fill with an accent edge: the focus colour under
                // muted text was unreadable in the themes (#2421).
                .when(on, |d| {
                    d.bg(rgb(palette::current().panel_hi))
                        .border_l_2()
                        .border_color(rgb(palette::current().accent))
                })
                .child(face)
                .child(SharedString::from(format!(":{}:", row.shortcode))),
        );
    }
    col
}

/// The slash-command list above the input: the one table the terminal reads
/// too, narrowed to what the draft is spelling.
///
/// Only the visible window is built, eight rows at most, and it slides once
/// the selection walks past the last of them. Descriptions are clipped here
/// rather than in the table, because the same strings are printed in full by
/// the terminal's `/help`.
pub(crate) fn command_popup_view(names: &[&'static str], selected: usize) -> Div {
    const VISIBLE: usize = 8;
    let window = popup_window(names.len(), selected, VISIBLE);
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_2()
        .rounded(px(4.0))
        .border_1()
        .border_color(rgb(palette::current().panel_hi))
        .bg(rgb(palette::current().panel));
    for (offset, name) in names[window.clone()].iter().enumerate() {
        let picked = window.start + offset == selected;
        let description = match slash::lookup_gui(name) {
            Some(cmd) => slash::clipped(cmd.description, 52).to_string(),
            None => String::new(),
        };
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_1()
            .rounded(px(4.0));
        if picked {
            row = row.bg(rgb(palette::current().panel_hi));
        }
        let name_color = if picked {
            palette::current().amber
        } else {
            palette::current().text
        };
        col = col.child(
            row.child(
                div()
                    .w(px(112.))
                    .text_xs()
                    .text_color(rgb(name_color))
                    .child(SharedString::from(*name)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette::current().text_muted))
                    .child(SharedString::from(description)),
            ),
        );
    }
    col.child(
        div()
            .text_xs()
            .text_color(rgb(palette::current().text_muted))
            .child(SharedString::from(format!(
                "{}/{} · ↑↓ select · tab complete · enter run",
                selected + 1,
                names.len()
            ))),
    )
}
