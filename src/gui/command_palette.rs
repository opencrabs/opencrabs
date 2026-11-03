//! The Seeker card: an always-dark glass pill floating over a dimmed window
//! (#2491), Spotlight-shaped: rounded, shadowed, top-center, a wordmark on
//! the field, kind chips under it, and a curated menu that waits three
//! seconds before it shows.
//!
//! The ranking, the keyword table and the state live in `ui_core::palette`, so
//! the terminal and the window print the same rows in the same order (#2458).
//! This file is the window's half: the scrim, the field, the rows and the
//! footer. The content is built by [`rows`], which is pure, so what the card
//! says can be tested without a window.

use gpui::{
    App, ClickEvent, SharedString, Window, div, linear_color_stop, linear_gradient, prelude::*, px,
    rems, rgb, rgba,
};
use std::rc::Rc;

use crate::palette;
use opencrabs::ui_core::commands::{Support, support};
use opencrabs::ui_core::palette::{PaletteKind, PaletteState, roast};

/// One printed row, decided without gpui. The renderer paints what this says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RowView {
    /// The focus marker: the row the arrows stand on, or the best match while
    /// nothing is armed.
    pub marker: &'static str,
    pub title: String,
    pub subtitle: String,
    pub kind: &'static str,
    pub armed: bool,
    /// A command the terminal runs and this window does not. It is printed
    /// anyway, dimmed, because hiding it would leave a user who saw it in the
    /// terminal wondering why it vanished here (#2458).
    pub missing: bool,
}

/// The rows the card prints, in the order the engine ranked them.
///
/// With nothing typed the cursor is unarmed and the first row is still the one
/// Enter acts on, so the top row carries the marker before the user touches an
/// arrow. That is the whole "no dead keyboard" rule: the field keeps accepting
/// characters while a row is armed.
pub fn rows(state: &PaletteState) -> Vec<RowView> {
    let visible = state.visible();
    let armed = state.cursor().unwrap_or(0);
    visible
        .into_iter()
        .enumerate()
        .map(|(ix, row)| RowView {
            marker: if ix == armed { MARKER } else { " " },
            // Read before the title is moved into the row: same value, and the
            // borrow checker has one order that works.
            missing: row.item.kind == PaletteKind::Command
                && support(&row.item.title) == Support::Missing,
            title: row.item.title,
            subtitle: row.item.subtitle,
            kind: kind_label(row.item.kind),
            armed: ix == armed,
        })
        .collect()
}

/// The line under the field when nothing matched, or a count of what did.
pub fn status(state: &PaletteState) -> String {
    match state.query() {
        "" => String::new(),
        query => {
            let rows = state.visible();
            if rows.is_empty() {
                // No rows at all: blank words. The roast still says it.
                roast(query).to_string()
            } else if rows.len() == 1 && rows[0].item.kind == PaletteKind::Ask {
                "no match. enter asks a fresh session".to_string()
            } else if rows.len() == 1 {
                "1 match".to_string()
            } else {
                format!("{} matches", rows.len())
            }
        }
    }
}

/// One click handler per printed row, built by the caller because only it holds
/// the context the action needs. Named so the card's signature, and every call
/// site that fills it, says what the box holds.
pub type PickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// One kind chip's press: `None` is the All chip. The caller builds it,
/// because only it can mutate the state behind the card (#2491).
pub type ChipHandler = Box<dyn Fn(Option<PaletteKind>, &mut App) + 'static>;

/// The chips the card prints, in paint order. Four kinds cover what the card
/// actually lists; anything rarer stays reachable by typing its word.
fn chip_defs() -> Vec<(&'static str, Option<PaletteKind>)> {
    vec![
        ("all", None),
        ("screens", Some(PaletteKind::Surface)),
        ("commands", Some(PaletteKind::Command)),
        ("docs", Some(PaletteKind::Docs)),
        ("sessions", Some(PaletteKind::Session)),
    ]
}

/// The card. `pick` holds one handler per printed row, in the same order
/// [`rows`] prints them, so a click acts on the row it landed on and nothing
/// else. `chip` is the press handler for the kind chips. `setup` is the line
/// for a channel the query names but the config does not carry yet (#2462).
/// `idle_revealed` is the caller's [`PaletteState::idle_ready`] verdict: a
/// fresh card paints the field alone until it says otherwise (#2491).
pub fn overlay(
    state: &PaletteState,
    pick: Vec<PickHandler>,
    chip: ChipHandler,
    setup: Option<&'static str>,
    idle_revealed: bool,
) -> impl IntoElement {
    let theme = palette::current();
    let printed = rows(state);
    let idle = state.query().trim().is_empty();
    let pick_armed = state.pick().is_some();
    // The reveal gate: picker rows and typed results print at once, the
    // idle menu holds for IDLE_REVEAL_DELAY so the first paint is the field
    // and the invitation alone (#2491).
    let reveal = pick_armed || !idle || idle_revealed;

    // The glass is always dark, Spotlight-style, light theme included: the
    // theme keeps its say through the accent on the armed row, the active
    // chip and the wordmark crab (#2491).
    let glass = if idle && idle_revealed && !pick_armed {
        // The idle menu sits under the solid field: more transparency at
        // the bottom than the top, so the list reads as a hint, not a wall.
        linear_gradient(
            180.0,
            linear_color_stop(rgba(0x14141cee), 0.0),
            linear_color_stop(rgba(0x14141c59), 1.0),
        )
    } else {
        linear_gradient(
            180.0,
            linear_color_stop(rgba(0x14141cf2), 0.0),
            linear_color_stop(rgba(0x14141cd9), 1.0),
        )
    };
    let armed_bg = rgba((theme.accent << 8) | 0x2e);
    let chip = Rc::new(chip);

    let mut card = div()
        .id("command-palette")
        .flex()
        .flex_col()
        .w(rems(34.0))
        .max_h(px(480.0))
        .rounded(px(16.0))
        .border_1()
        .border_color(rgba(0xffffff1f))
        .bg(glass)
        .shadow_md()
        .overflow_hidden()
        // Inert: a click on the card stops here instead of reaching
        // the scrim, so reading the list never closes it.
        .on_click(|_ev, _window, cx| cx.stop_propagation());

    // The field: the wordmark on the left, because our name is not Spotlight's
    // and the card should say whose it is; the query or the invitation on the
    // right of it (#2491).
    card = card.child(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .child(div().text_xs().child("🦀"))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme.text_muted))
                    .child("OpenCrabs"),
            )
            .child(
                div()
                    .flex_1()
                    .text_base()
                    .text_color(rgb(theme.text))
                    .truncate()
                    .child(SharedString::from(if state.query().is_empty() {
                        "Sessions, feats, skills, docs, troubleshooting and more".to_string()
                    } else {
                        state.query().to_string()
                    })),
            ),
    );

    if reveal {
        // The kind chips. Hidden while a session pick is armed: a narrowed
        // conversation list has no kinds to filter (#2491).
        if !pick_armed {
            let wanted = state.kind_filter();
            let mut row = div().flex().flex_row().gap_1().px_3().pb_1();
            for (ix, (label, kind)) in chip_defs().into_iter().enumerate() {
                let active = wanted == kind;
                let press = chip.clone();
                row = row.child(
                    div()
                        .id(("palette-chip", ix))
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .border_1()
                        .text_xs()
                        .cursor_pointer()
                        .when(active, |d| {
                            d.border_color(rgb(theme.accent))
                                .text_color(rgb(theme.accent))
                        })
                        .when(!active, |d| {
                            d.border_color(rgba(0xffffff14))
                                .text_color(rgb(theme.text_muted))
                        })
                        .hover(|d| d.border_color(rgb(theme.accent)))
                        .on_click(move |_ev, _window, cx| {
                            cx.stop_propagation();
                            press(kind, cx);
                        })
                        .child(SharedString::from(label)),
                );
            }
            card = card.child(row);
        }

        // The rows: title only, subtitle and enter hint on the armed row,
        // and the one right-edge word that does work, terminal only (#2491).
        let mut list = div().flex().flex_col().gap_0p5().px_1();
        let mut handlers = pick.into_iter();
        for (ix, rowv) in printed.iter().enumerate() {
            let armed = rowv.armed;
            let mut cell = div()
                .id(("palette-row", ix))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded(px(6.0))
                .cursor_pointer()
                .when(armed, |d| d.bg(armed_bg));
            if armed && !rowv.missing {
                cell = cell.child(
                    div()
                        .w(px(10.0))
                        .text_xs()
                        .text_color(rgb(theme.accent))
                        .child(SharedString::from(MARKER)),
                );
            } else {
                cell = cell.child(div().w(px(10.0)));
            }
            cell = cell.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(if rowv.missing {
                                theme.text_muted
                            } else {
                                theme.text
                            }))
                            .truncate()
                            .child(SharedString::from(rowv.title.clone())),
                    )
                    .when(armed && !rowv.subtitle.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(rgb(theme.text_muted))
                                .truncate()
                                .child(SharedString::from(rowv.subtitle.clone())),
                        )
                    }),
            );
            if rowv.missing {
                // A command the terminal runs and this window does not keeps
                // its tag, dimmed; hiding it would leave a user who saw it
                // in the terminal wondering why it vanished (#2458).
                cell = cell.child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.text_muted))
                        .child("terminal only"),
                );
            } else if armed {
                cell = cell.child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.text_muted))
                        .child("↵ enter"),
                );
            }
            if let Some(handler) = handlers.next() {
                cell = cell.on_click(handler);
            }
            list = list.child(cell);
        }
        card = card.child(div().flex().flex_col().overflow_hidden().child(list));

        // Status only when it does work: the roast on a miss, the ask row's
        // naming. The count is chrome; the rows are the answer (#2491).
        let miss = printed.is_empty() && !state.is_empty();
        let vis = state.visible();
        let asks = vis.len() == 1 && vis[0].item.kind == PaletteKind::Ask;
        if miss || asks {
            card = card.child(
                div()
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .text_color(rgb(theme.text_muted))
                    .child(SharedString::from(status(state))),
            );
        }

        if let Some(text) = setup {
            card = card
                .child(
                    div()
                        .px_3()
                        .pb_1()
                        .text_xs()
                        .text_color(rgb(theme.warn))
                        .child(SharedString::from(text)),
                )
                .child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_xs()
                        .text_color(rgb(theme.text_muted))
                        .child("enter starts the setup · esc closes"),
                );
        } else if !idle && !miss && !asks {
            // The legend prints only under a typed list; an idle card ends
            // at its rows so the glass stays a hint (#2491).
            card = card.child(
                div()
                    .px_3()
                    .pb_2()
                    .text_xs()
                    .text_color(rgb(theme.text_muted))
                    .child("enter run · up/down move · esc close"),
            );
        }
    }

    div()
        .id("command-palette-scrim")
        // Modal: the scrim owns the pointer, so a click meant for the window
        // behind it cannot act on anything but the card.
        .occlude()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        // Spotlight floats the card high, not centered: the eye reads it as
        // a command surface, not a dialog (#2491).
        .justify_start()
        .pt(rems(6.5))
        .bg(rgba(0x0a0a0a99))
        .child(card)
}

/// The kind label for a row, exposed so a test can check the column without
/// reaching into the engine.
pub fn kind_label(kind: PaletteKind) -> &'static str {
    kind.label()
}

/// The marker the card prints for a row, kept here so the test and the paint
/// read the same string.
pub const MARKER: &str = ">";

#[cfg(test)]
mod tests {
    use super::*;
    use opencrabs::ui_core::palette::{PaletteAction, PaletteItem, Surface};

    fn state_with(items: Vec<PaletteItem>) -> PaletteState {
        PaletteState::new(items)
    }

    fn item(title: &str, kind: PaletteKind) -> PaletteItem {
        PaletteItem::new(
            title,
            "detail",
            kind,
            PaletteAction::OpenSurface(Surface::Help),
        )
    }

    /// The top row is armed before any arrow press, because Enter acts on it:
    /// a card whose marker only appears after a key press reads as broken.
    #[test]
    fn the_top_row_is_armed_until_an_arrow_moves_it() {
        let mut state = state_with(vec![
            item("alpha", PaletteKind::Command),
            item("alpine", PaletteKind::Command),
        ]);
        let printed = rows(&state);
        assert_eq!(printed.len(), 2);
        assert_eq!(printed[0].marker, MARKER);
        assert!(printed[0].armed);
        assert_eq!(printed[1].marker, " ");
        assert!(!printed[1].armed);

        // The engine's rule: the first press arms the top row (so one arrow
        // never skips the best match), and the next press steps down from it.
        state.move_by(1, 10);
        assert_eq!(state.cursor(), Some(0));
        state.move_by(1, 10);
        let printed = rows(&state);
        assert!(!printed[0].armed);
        assert!(printed[1].armed);
        assert_eq!(printed[1].marker, MARKER);
    }

    /// The kind column is the engine's own word, so the two frontends cannot
    /// label the same row differently.
    #[test]
    fn the_kind_column_uses_the_engines_label() {
        let state = state_with(vec![item("a", PaletteKind::Session)]);
        let printed = rows(&state);
        assert_eq!(printed[0].kind, kind_label(PaletteKind::Session));
        assert_eq!(printed[0].kind, "session");
    }

    /// A miss offers the ask row and names it in the status line, never an
    /// empty panel with a silent footer (#2482).
    #[test]
    fn a_miss_offers_the_ask_row_and_the_status_names_it() {
        let mut state = state_with(vec![item("alpha", PaletteKind::Command)]);
        for c in "zzz".chars() {
            state.type_char(c);
        }
        let printed = rows(&state);
        assert_eq!(printed.len(), 1, "a miss must offer exactly the ask row");
        assert_eq!(printed[0].kind, kind_label(PaletteKind::Ask));
        assert_eq!(status(&state), "no match. enter asks a fresh session");
    }

    /// A single hit says "1 match", not "1 matches".
    #[test]
    fn the_count_reads_like_english() {
        let mut state = state_with(vec![
            item("alpha", PaletteKind::Command),
            item("beta", PaletteKind::Command),
        ]);
        assert_eq!(status(&state), "", "an empty query prints no count");
        state.type_char('a');
        state.type_char('l');
        assert_eq!(status(&state), "1 match");
        state.backspace();
        state.clear();
        state.type_char('a');
        assert_eq!(status(&state), "2 matches");
    }

    /// Every command the terminal runs prints its row here, and since the
    /// window answers all of them none is marked "terminal only" any more
    /// (#2469, #2470). The mark itself is kept: the day a command lands in
    /// the shared table before its window wiring does, this same loop marks
    /// it rather than the row vanishing.
    #[test]
    fn every_command_row_is_runnable_on_the_window() {
        use opencrabs::ui_core::commands::{SLASH_COMMANDS, Support, support};

        let items: Vec<PaletteItem> = SLASH_COMMANDS
            .iter()
            .map(|cmd| item(cmd.name, PaletteKind::Command))
            .collect();

        // The idle menu is curated since #2491: it leads with surfaces and
        // drops /brain and /compact, whose screens already carry them. The
        // contract is reachability, not the dump: typing a command's own
        // name finds its row, and no found command is terminal only.
        for cmd in SLASH_COMMANDS {
            let mut probe = state_with(items.clone());
            for ch in cmd.name.chars() {
                probe.type_char(ch);
            }
            let printed = rows(&probe);
            let row = printed
                .iter()
                .find(|row| row.title == cmd.name)
                .unwrap_or_else(|| panic!("{} not found by its own name", cmd.name));
            assert!(
                !row.missing,
                "{} is marked terminal only but the window runs it",
                row.title
            );
        }

        // The gap that would mark a row is gone, so the badge's own trigger is
        // asserted directly: a name the shared table does not know is not a
        // gap, it is a typo.
        let gap = SLASH_COMMANDS
            .iter()
            .filter(|cmd| support(cmd.name) == Support::Missing)
            .count();
        assert_eq!(gap, 0, "the window answers every command in the table");

        // Only command rows can carry the mark; a session or model row that
        // happens to be named like one never does.
        let sessions = rows(&state_with(vec![item("/usage", PaletteKind::Session)]));
        assert!(!sessions[0].missing, "a session row is never terminal only");
    }
}
