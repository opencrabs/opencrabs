//! The follow-up offer's card (#3101): the row above the composer, and the keys
//! that reach it.
//!
//! The card's drawing is a layout fact each frontend owns, so what is asserted
//! here is the decision both surfaces share: that an offer waiting says so on
//! the hint row, that it says it in front of every other hint, and that it
//! survives the filter each surface runs. The last one matters most: the offer
//! names keys rather than a slash command, and the filter used to drop any hint
//! whose command the surface did not answer, which would have made the one hint
//! about something already on screen the one hint that never printed.

use crate::ui_core::commands::{lookup, support_gui};
use crate::ui_core::hints::{
    CONTEXT_ROOMY, CONTEXT_TIGHT, HintState, LONG_SESSION_TURNS, candidates, gui_hint, hint,
};

/// A conversation with one turn behind it and nothing else to say: no update,
/// plenty of context left. The offer is the only thing on it.
fn with_offer(offers: usize) -> HintState {
    HintState {
        context_used: 4_000,
        context_max: 200_000,
        clear_base: 3_000,
        turns: 1,
        offers,
        ..HintState::default()
    }
}

/// The terminal's filter: it answers every name in the shared table.
fn tui(state: &HintState) -> Option<crate::ui_core::hints::Hint> {
    hint(state, |command| lookup(command).is_some())
}

#[test]
fn an_offer_waiting_is_the_first_thing_the_row_says() {
    let hint = tui(&with_offer(1)).expect("an offer on screen earns a line");
    assert!(
        hint.command.is_empty(),
        "the offer names keys, not a command"
    );
    assert!(
        hint.text.contains('↑'),
        "the row has to say the arrow reaches it, or the keys stay a secret: {}",
        hint.text
    );
    assert!(
        hint.text.contains("enter"),
        "and what sends it once it is reached: {}",
        hint.text
    );
}

#[test]
fn one_offer_and_many_offers_do_not_read_the_same() {
    // One option needs no walk: saying "walk them" over a single button would
    // send the reader looking for arrows that do nothing. Both lines still
    // name the way out, because Down past the only row is the exit (#3114).
    let single = tui(&with_offer(1)).expect("one offer");
    let many = tui(&with_offer(3)).expect("three offers");
    assert!(
        !single.text.contains("walk"),
        "nothing to walk down: {}",
        single.text
    );
    assert!(
        single.text.contains("back to the field"),
        "the only row is also the exit, and the line says so: {}",
        single.text
    );
    assert!(
        many.text.contains('↓'),
        "a list is walked both ways: {}",
        many.text
    );
    assert!(
        many.text.contains("past the last") && many.text.contains("back to the field"),
        "the end of a list has to say what the end does: {}",
        many.text
    );
}

#[test]
fn an_offer_outranks_every_other_hint() {
    // It is the only rung about something already on screen and one keystroke
    // away, so a full context and a waiting release both wait their turn.
    let mut state = with_offer(2);
    state.turns = LONG_SESSION_TURNS;
    state.update_version = Some("0.7.0".to_string());
    state.context_used = 190_000;
    let order: Vec<&str> = candidates(&state).iter().map(|hint| hint.command).collect();
    assert_eq!(order, vec!["", "/clear", "/evolve"]);
    assert_eq!(tui(&state).map(|h| h.command), Some(""));
}

#[test]
fn the_offer_survives_a_surface_that_runs_none_of_the_commands() {
    // The pin for the filter itself. Every command is refused and the offer
    // still prints, because it has no command to refuse.
    let state = with_offer(1);
    let printed = hint(&state, |_| false).expect("keys are not a command to filter");
    assert!(printed.command.is_empty());
    assert!(gui_hint(&state).is_some(), "the desktop app has keys too");
}

#[test]
fn no_offer_says_nothing_about_offers() {
    // The common case: the turn offered nothing, and the row must not advertise
    // keys that would land on an empty card.
    let state = with_offer(0);
    // The card may still carry a tip, which is the point of the tips. What it
    // must never carry is a line about offer keys when the turn offered none.
    assert!(
        candidates(&state).iter().all(|hint| hint.id != "offers"),
        "no offer, no line advertising the keys that would land on an empty card"
    );
    assert!(
        gui_hint(&state).is_none_or(|hint| hint.id != "offers"),
        "the desktop app has no offer to point at either"
    );
}

#[test]
fn a_running_turn_keeps_the_row_quiet() {
    // The offer arrives when the turn ends. Mid-answer there is nothing to
    // send, and a row offering to send it would be a lie about the moment.
    let mut state = with_offer(2);
    state.processing = true;
    assert!(tui(&state).is_none());
    assert!(gui_hint(&state).is_none());
}

#[test]
fn the_offer_does_not_crowd_out_a_context_that_is_running_out() {
    // Both are printed: the offer wins the row, but `candidates` still carries
    // the clear line behind it, so a surface with room for two says both.
    let mut state = with_offer(1);
    state.context_used = 170_000; // over the tight line
    assert!(state.context_percent() >= CONTEXT_TIGHT);
    let all = candidates(&state);
    // The tips ride behind both, so the claim is about the two that matter
    // here: the offer keeps the row and the clear line is still carried.
    assert!(
        all.len() >= 2,
        "the offer and the clear line are both carried"
    );
    assert_eq!(all[0].id, "offers", "the offer wins the row");
    assert_eq!(all[1].command, "/clear", "the clear line is still behind it");
    // And with room to spare the offer still wins the row: a tip riding
    // behind it never outranks something the reader can act on right now.
    let mut roomy = with_offer(1);
    roomy.context_used = 1_000;
    assert!(roomy.context_percent() < CONTEXT_ROOMY);
    assert_eq!(
        candidates(&roomy).first().map(|hint| hint.id),
        Some("offers")
    );
}

#[test]
fn the_desktop_app_and_the_terminal_agree_about_the_offer() {
    // Two surfaces, one decision: the same state produces the same line, so a
    // reader who uses both is not told two different things about the same
    // card. The desktop app answers fewer commands than the terminal, which is
    // exactly why the shared answer is asserted rather than each one's own.
    for offers in [1usize, 2, 5] {
        let state = with_offer(offers);
        let terminal = tui(&state).map(|h| h.text);
        let desktop = gui_hint(&state).map(|h| h.text);
        assert_eq!(terminal, desktop, "{offers} offers");
        assert!(terminal.is_some());
    }
    // And the desktop's own support list has not started refusing keys.
    assert_eq!(
        support_gui("/clear"),
        crate::ui_core::commands::Support::Runs
    );
}
