//! The GUI rail moves a conversation to the top when it is used (#2666).
//!
//! Reported: a session used minutes ago sat below one used yesterday, and its
//! date column showed the old time. The ledger was already right
//! (`SELECT * FROM sessions ... ORDER BY updated_at DESC`, with the bump beside
//! the message insert), and the TUI re-queries on open, so the defect was the
//! rail's in-memory snapshot: `sessions.items` was refetched on session
//! create, session delete, model switch, turn resume, topic rename and topic
//! create, and never when a turn settled.
//!
//! GPUI has no headless pointer/render harness here, so these checks pin the
//! wiring that keeps the snapshot fresh. The ordering itself is exercised by
//! the repository tests and by `gui/sessions.rs`.

const GUI_MAIN: &str = include_str!("../gui/main.rs");

fn block<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = source.split_once(start).expect("source block starts");
    let (body, _) = tail.split_once(end).expect("source block ends");
    body
}

#[test]
fn a_settled_turn_refreshes_the_rail() {
    let done = block(
        GUI_MAIN,
        "agent_bridge::TurnEvent::Done {",
        "agent_bridge::TurnEvent::Failed {",
    );
    assert!(
        done.contains("self.refresh_rail(cx)"),
        "a finished turn must refetch the rail: the ledger bumped \
         `updated_at` when the message landed, and the rail paints that \
         order, so without the refetch the row keeps its old slot and date"
    );
}

#[test]
fn a_failed_turn_refreshes_the_rail_too() {
    let failed = block(
        GUI_MAIN,
        "agent_bridge::TurnEvent::Failed {",
        "agent_bridge::TurnEvent::TurnReverted {",
    );
    assert!(
        failed.contains("self.refresh_rail(cx)"),
        "a failed turn is still activity: the question row was written before \
         the failure, so the conversation has to move up like any other"
    );
}

#[test]
fn the_rail_still_paints_the_field_it_is_sorted_by() {
    let sessions = include_str!("../gui/sessions.rs");
    let row = block(sessions, "pub fn row(", "pub fn on_click(");
    // #2815 rolled a card's stamp up over the topics it hides as chips, so the
    // row prints `stamp_at` instead of the parent's own `updated_at`. It is
    // still the field the order is keyed on: printing `created_at`, or the
    // parent row's untouched `updated_at`, would leave a correctly sorted rail
    // looking scrambled.
    assert!(
        row.contains("rail_stamp(stamp_at"),
        "the row prints the stamp the order is keyed on"
    );
    let ordered = block(sessions, "pub fn ordered(", "pub fn is_closed(");
    assert!(
        ordered.contains("std::cmp::Reverse(activity.get(&s.id)"),
        "the order is keyed on the same rolled-up activity the card prints"
    );
}
