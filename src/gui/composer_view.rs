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

use gpui::{AnyElement, Div, IntoElement, SharedString, div, prelude::*, px, rgb};

use opencrabs::brain::tools::suggest_options::{SuggestionItem, SuggestionStyle};
use opencrabs::ui_core::card::{CARD_RADIUS, glass_fill};
use opencrabs::ui_core::commands as slash;

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
/// the queued list sits. One numbered row per label, styled the way the
/// channel buttons are: primary in the accent, danger in the red, default
/// quiet. A click sends that exact label as the next turn (#2708).
///
/// The rows arrive with their handlers already minted by the window, because
/// only it knows which session the offer belongs to. Nothing here reaches the
/// database: see `agent_bridge::SUGGESTIONS` for why there is nothing to
/// persist.
pub(crate) fn options_strip(rows: &[(SuggestionItem, transcript::ClickHandler)]) -> Div {
    let mut col = div()
        .flex()
        .flex_col()
        .gap_1()
        .p_3()
        .rounded(px(CARD_RADIUS))
        .bg(glass_fill(palette::current().panel))
        .child(
            div()
                .text_xs()
                .text_color(rgb(palette::current().text_muted))
                .child(SharedString::from("suggested")),
        );
    for (i, (item, pick)) in rows.iter().enumerate() {
        let color = match item.style {
            SuggestionStyle::Primary => palette::current().accent,
            SuggestionStyle::Danger => palette::current().red,
            SuggestionStyle::Default => palette::current().text_muted,
        };
        let pick = pick.clone();
        col = col.child(
            div()
                .id(SharedString::from(format!("option-row-{i}")))
                .text_xs()
                .text_color(rgb(color))
                .cursor_pointer()
                .hover(|d| d.bg(rgb(palette::current().panel_hi)))
                .child(SharedString::from(format!("{}. {}", i + 1, item.label)))
                .on_click(move |ev, window, app| pick(ev, window, app)),
        );
    }
    col
}

/// The composer's hint line for a state. Grey and quiet, it names the keys
/// that work right now: stopping the turn, recalling a queued message, and
/// queueing another (#2630).
pub(crate) fn composer_hint_for(busy: bool, queued: usize) -> SharedString {
    match (busy, queued) {
        (true, 0) => SharedString::from("esc stops · typing queues"),
        (true, n) => SharedString::from(format!("queued {n} · ↑ recalls · esc stops")),
        (false, 0) => SharedString::from("Talk to the crab…"),
        (false, n) => SharedString::from(format!("{n} queued · sending now")),
    }
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
