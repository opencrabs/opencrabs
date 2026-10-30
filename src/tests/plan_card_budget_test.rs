//! Plan card height budget (#1750, #2165): on short terminals the card yields
//! rows before the chat viewport starves. Tall terminals keep the pre-#1750
//! behaviour byte-identical.
//!
//! The floor the card must leave behind is `CHAT_FLOOR`, the same number the
//! layout asks for with `Constraint::Min`. It used to be 6 here against 10
//! there, so the two budgets disagreed and the solver starved the transcript to
//! satisfy the card.

use crate::tui::render::{CHAT_FLOOR, plan_card_height};

/// Rows left for the transcript once the card, queue, input and status bar are
/// paid for.
fn chat_rows(card: u16, area: u16, input: u16, queue: u16) -> u16 {
    area.saturating_sub(card)
        .saturating_sub(queue)
        .saturating_sub(input)
        .saturating_sub(1)
}

#[test]
fn tall_terminal_keeps_full_card() {
    // 40 rows: status 1 + input 10 + queue 2 + chat floor = 13 + floor,
    // so a 12-row card fits whole.
    assert_eq!(plan_card_height(12, 40, 10, 2), 12);
}

#[test]
fn short_pane_shrinks_card_to_budget() {
    // 30 rows, input 3, no queue: budget is 30 - 1 - 3 - CHAT_FLOOR = 16,
    // so a 12-row card is not what gets cut here; a taller desired is.
    assert_eq!(plan_card_height(20, 30, 3, 0), 16);
}

#[test]
fn chat_keeps_its_floor_whenever_the_card_is_drawn() {
    // The invariant that was broken: whatever the card takes, the transcript
    // still gets its floor. This is the bug where the live "thinking" row and
    // the turn's answer slid under the checklist widget.
    for area in 14u16..60 {
        for input in [3u16, 4, 10] {
            for queue in [0u16, 2] {
                let card = plan_card_height(12, area, input, queue);
                if card == 0 {
                    // Card withheld: the transcript owns the rows.
                    continue;
                }
                assert!(
                    chat_rows(card, area, input, queue) >= CHAT_FLOOR,
                    "area={area} input={input} queue={queue} card={card} left chat {} rows",
                    chat_rows(card, area, input, queue)
                );
            }
        }
    }
}

#[test]
fn degenerate_pane_drops_the_card_instead_of_crushing_chat() {
    // Nothing left once the floor is paid: the card goes away rather than
    // forcing three rows out of the transcript.
    assert_eq!(plan_card_height(12, 8, 10, 2), 0);
    assert_eq!(plan_card_height(5, 8, 10, 2), 0);
    assert_eq!(plan_card_height(12, 12, 3, 0), 0);
}

#[test]
fn small_plan_untouched_when_budget_exceeds_desired() {
    // 5-task plan (desired 7) on a 24-row pane with a small input and
    // no queue: budget 10, desired wins.
    assert_eq!(plan_card_height(7, 24, 3, 0), 7);
}

#[test]
fn seed_strip_survives_normal_panes() {
    // The 5-line seed strip (#1945, text row padded by a blank above
    // and below) never grows and never shrinks on a sane pane.
    assert_eq!(plan_card_height(5, 24, 4, 2), 5);
}

#[test]
fn seed_strip_degrades_to_card_min_on_a_tight_pane() {
    // 17 rows, input 3: budget 17 - 1 - 3 - CHAT_FLOOR = 3, the card min,
    // so the strip renders tight instead of vanishing.
    assert_eq!(plan_card_height(5, 17, 3, 0), 3);
}
