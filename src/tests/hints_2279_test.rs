//! The hint row (#2279): one line under the composer, on both surfaces.
//!
//! The row itself is a layout fact each frontend owns, so what is asserted here
//! is the shared decision it prints: which suggestion wins, what it says, and
//! whether the surface asking can actually run the command it names. A hint
//! naming a command that surface does not answer is worse than no hint, because
//! it teaches a habit that bounces.

use crate::ui_core::commands::{Support, lookup, support_gui};
use crate::ui_core::hints::{
    CONTEXT_ROOMY, CONTEXT_TIGHT, HintState, LONG_SESSION_TURNS, candidates, compact_tokens,
    gui_hint, hint,
};

/// A fresh conversation: a little context used, no update waiting.
fn fresh() -> HintState {
    HintState {
        context_used: 4_000,
        context_max: 200_000,
        clear_base: 3_000,
        ..HintState::default()
    }
}

/// The same filter each surface passes, so a test asserts the real answer and
/// not a stub that would accept anything.
fn tui(state: &HintState) -> Option<crate::ui_core::hints::Hint> {
    // The terminal dispatches the whole shared table, so a name the table
    // knows is a name it answers.
    hint(state, |command| lookup(command).is_some())
}

#[test]
fn a_fresh_conversation_says_nothing() {
    assert!(tui(&fresh()).is_none());
    assert!(gui_hint(&fresh()).is_none());
    assert!(candidates(&fresh()).is_empty());
}

#[test]
fn nothing_is_suggested_while_the_answer_is_being_written() {
    // The reader is waiting, not choosing: a suggestion mid-turn competes with
    // the thing they asked for.
    let mut state = fresh();
    state.processing = true;
    state.context_used = 199_000; // over the tight line, and still silent
    state.update_version = Some("0.7.0".to_string());
    assert!(tui(&state).is_none());
    assert!(gui_hint(&state).is_none());
}

#[test]
fn a_full_context_suggests_clearing_and_names_what_it_frees() {
    let mut state = fresh();
    state.context_used = 170_000; // 85% of 200k
    let hint = tui(&state).expect("a context this full earns a hint");
    assert_eq!(hint.command, "/clear");
    // 170k used minus the 3k base is what the reader gets back.
    assert!(
        hint.text.contains("167.0k"),
        "the freed amount should be the difference, got {:?}",
        hint.text
    );
    assert!(
        state.context_percent() >= CONTEXT_TIGHT,
        "the fixture must actually be over the line"
    );
}

#[test]
fn a_waiting_update_outranks_a_long_conversation() {
    let mut state = fresh();
    state.turns = LONG_SESSION_TURNS + 4;
    state.update_version = Some("0.6.1".to_string());
    let hint = tui(&state).expect("an update is worth a row");
    assert_eq!(hint.command, "/evolve");
    assert!(hint.text.contains("0.6.1"), "got {:?}", hint.text);
}

#[test]
fn a_long_but_roomy_conversation_names_both_ways_out() {
    let mut state = fresh();
    state.turns = LONG_SESSION_TURNS;
    let hint = tui(&state).expect("a dozen turns is worth mentioning");
    assert_eq!(hint.command, "/new");
    assert!(
        state.context_percent() < CONTEXT_ROOMY,
        "this hint is only for a conversation with room left"
    );
    // Both names, because they cost different things: a fresh session takes the
    // conversation with it, a clear only frees the window and leaves the history
    // and the title where they are (#3113).
    assert!(hint.text.contains("/new"), "got {:?}", hint.text);
    assert!(hint.text.contains("/clear"), "got {:?}", hint.text);
    assert!(
        hint.text.contains("history"),
        "the line has to say what /clear keeps: {:?}",
        hint.text
    );
    // One line, one decision: the window prints what the terminal prints.
    assert_eq!(gui_hint(&state).map(|hint| hint.text), Some(hint.text.clone()));
}

#[test]
fn a_long_conversation_with_little_room_left_gets_no_clean_slate_line() {
    // Past `CONTEXT_ROOMY` the context hint owns the row: telling someone to
    // start fresh while quietly running out of window would be the wrong half
    // of the advice.
    let mut state = fresh();
    state.turns = LONG_SESSION_TURNS + 6;
    state.context_used = 90_000; // 45%, under the tight line, over the roomy one
    assert!(tui(&state).is_none());
}

#[test]
fn the_ladder_is_ordered_by_how_much_it_matters() {
    // A full context outranks everything: the reader is about to lose the
    // conversation whether they chose to or not.
    let mut state = fresh();
    state.turns = LONG_SESSION_TURNS;
    state.update_version = Some("0.6.1".to_string());
    state.context_used = 190_000;
    let order: Vec<&str> = candidates(&state).iter().map(|hint| hint.command).collect();
    assert_eq!(order, vec!["/clear", "/evolve"]);

    // With room left the clean-slate line comes back, still under the update:
    // a release waiting is news, a long session is only an option.
    state.context_used = 4_000;
    let order: Vec<&str> = candidates(&state).iter().map(|hint| hint.command).collect();
    assert_eq!(order, vec!["/evolve", "/new"]);
}

#[test]
fn a_surface_drops_a_hint_it_cannot_run_instead_of_printing_it() {
    // The whole reason the filter exists: the row falls through to the next
    // suggestion rather than naming a command that would answer "Unknown".
    let mut state = fresh();
    state.update_version = Some("0.6.1".to_string());
    state.turns = LONG_SESSION_TURNS;
    let only_new = hint(&state, |command| command == "/new").expect("the last rung still runs");
    assert_eq!(only_new.command, "/new");
    let nothing = hint(&state, |_| false);
    assert!(
        nothing.is_none(),
        "a surface that runs none of them is silent"
    );
}

#[test]
fn every_command_the_ladder_names_runs_on_both_surfaces() {
    // A regression guard, not a restatement: drop one of these from the
    // frontend's support list and its hint vanishes with no error anywhere.
    for command in ["/clear", "/evolve", "/new"] {
        assert!(
            lookup(command).is_some(),
            "{command} is in the hint ladder, so the terminal must know it"
        );
        assert_eq!(
            support_gui(command),
            Support::Runs,
            "{command} is in the hint ladder, so the desktop app must run it"
        );
    }
}

#[test]
fn the_desktop_app_answers_the_ladder_through_its_own_table() {
    // `gui_hint` is the same decision as `hint` filtered by the window's own
    // support list, including the screens only it owns.
    let mut state = fresh();
    state.context_used = 180_000;
    let from_shared = hint(&state, |command| support_gui(command) == Support::Runs);
    let from_window = gui_hint(&state);
    assert_eq!(
        from_window.map(|hint| hint.command),
        from_shared.map(|hint| hint.command)
    );
}

#[test]
fn tokens_are_printed_in_the_units_a_person_reads() {
    assert_eq!(compact_tokens(840), "840");
    assert_eq!(compact_tokens(1_000), "1.0k");
    assert_eq!(compact_tokens(104_400), "104.4k");
    assert_eq!(compact_tokens(1_200_000), "1.2M");
}

#[test]
fn clearing_never_promises_a_negative_saving() {
    // A base larger than what is in use means the two numbers came from
    // different moments. "frees -3k tokens" is worse than saying nothing.
    let state = HintState {
        context_used: 1_000,
        context_max: 200_000,
        clear_base: 4_000,
        ..HintState::default()
    };
    assert_eq!(state.clearable(), 0);
}

#[test]
fn an_unknown_window_reads_as_nothing_to_warn_about() {
    // `context_max` is zero before the session's model is known, and a
    // division by it would print "0% of nothing" as if it were a measurement.
    let state = HintState::default();
    assert_eq!(state.context_percent(), 0.0);
}
