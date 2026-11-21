//! The hint card's dismissal rules, and the friendly rungs that ride it.
//!
//! Two classes share the card and only one of them is advice. These tests pin
//! the split: a tip can be hidden for a week and the next one takes its place,
//! while a line that names what the keys are doing RIGHT NOW cannot be hidden
//! at all. They also pin where the compaction explanation lands, because a line
//! that explains the machine after it already acted is worse than no line.

use crate::ui_core::hints::{
    COMPACTION_TIP_AT, CONTEXT_TIGHT, HintState, candidates, dismissible, task_line,
};

/// The terminal's filter: it dispatches the whole shared table.
fn any_command(_: &str) -> bool {
    true
}

#[test]
fn an_empty_session_says_nothing() {
    // Nothing has happened yet, so there is nothing to explain and nothing to
    // search: a card over an empty transcript is chrome.
    assert!(candidates(&HintState::default()).is_empty());
}

#[test]
fn dismissing_a_tip_reveals_the_next_one() {
    let mut state = HintState {
        turns: 20,
        ..HintState::default()
    };
    let first = candidates(&state);
    assert_eq!(
        first.first().map(|hint| hint.id),
        Some("fresh-start"),
        "the earliest rung the reader has earned comes first"
    );

    // The dismissal is what lets the next one through: without it the card
    // would be stuck on a line the reader has already hidden. Each push is a
    // week of silence for exactly one line, and the row hands over to the rung
    // behind it rather than going blank.
    state.dismissed.push("fresh-start".to_string());
    let after = candidates(&state);
    assert_eq!(
        after.first().map(|hint| hint.id),
        Some("isolated-sessions"),
        "the next rung takes the row"
    );

    state.dismissed.push("isolated-sessions".to_string());
    let later = candidates(&state);
    assert_eq!(
        later.first().map(|hint| hint.id),
        Some("usage"),
        "and the one behind that"
    );
    assert!(
        later
            .iter()
            .all(|hint| hint.id != "fresh-start" && hint.id != "isolated-sessions"),
        "a dismissed line does not come back inside its window"
    );
}

#[test]
fn a_functional_line_cannot_be_dismissed() {
    // These name what the keys do at this moment, or a fact about the machine.
    // Hiding any of them would leave the reader guessing at a screen that is
    // behaving for a reason they cannot see.
    for id in ["offers", "queue", "rail", "rail-split", "context", "update"] {
        assert!(!dismissible(id), "{id} is not advice, so it stays");
    }
    for id in ["seeker", "usage", "compaction", "fresh-start", "whisper"] {
        assert!(dismissible(id), "{id} is advice, so it can be hidden");
    }
}

#[test]
fn a_stale_row_in_the_store_cannot_hide_what_the_keys_are_doing() {
    // The store is keyed by id and written by two frontends. If a functional id
    // ever landed in it, the filter must still refuse to silence the line.
    let state = HintState {
        offers: 2,
        dismissed: vec!["offers".to_string()],
        ..HintState::default()
    };
    let line = task_line(&state, any_command).expect("the offer line still speaks");
    assert!(
        line.contains("options"),
        "the offer keys are what the card is for: {line}"
    );
}

#[test]
fn the_compaction_tip_arrives_before_the_clear_warning() {
    // The explanation belongs in the window where it can still arrive before
    // the event. Auto-compaction starts well below the tight line, so a tip
    // printed only after the warning is a report, not a tip.
    let mut state = HintState {
        context_max: 200_000,
        ..HintState::default()
    };
    state.context_used = 120_000; // 60%: past the tip, short of the warning
    assert!((COMPACTION_TIP_AT..CONTEXT_TIGHT).contains(&state.context_percent()));
    assert_eq!(
        candidates(&state).first().map(|hint| hint.id),
        Some("compaction"),
        "the tip owns the row before the warning does"
    );

    state.context_used = 180_000; // 90%: the warning outranks it
    assert_eq!(
        candidates(&state).first().map(|hint| hint.id),
        Some("context"),
        "a full context outranks a friendly tip"
    );
}

#[test]
fn the_seeker_tip_waits_for_the_first_exchange() {
    // An empty session has nothing to search, so the introduction would be a
    // greeting card. One exchange is enough for the offer to mean something.
    let empty = HintState::default();
    assert!(
        candidates(&empty).iter().all(|hint| hint.id != "seeker"),
        "no messages, no introduction"
    );

    let started = HintState {
        turns: 1,
        ..HintState::default()
    };
    assert!(
        candidates(&started).iter().any(|hint| hint.id == "seeker"),
        "one exchange in, the search is worth naming"
    );

    let settled = HintState {
        turns: 8,
        ..HintState::default()
    };
    assert!(
        candidates(&settled).iter().all(|hint| hint.id != "seeker"),
        "later it is nagging, not an introduction"
    );
}

#[test]
fn the_voice_tip_waits_for_an_engine() {
    // A tip pointing at `/whisper` with no engine behind it bounces, which is
    // the same failure as naming a command the surface does not answer.
    let silent = HintState {
        turns: 2,
        voice: false,
        ..HintState::default()
    };
    assert!(
        candidates(&silent).iter().all(|hint| hint.id != "whisper"),
        "no engine, no line about talking"
    );

    let speaking = HintState {
        turns: 2,
        voice: true,
        ..HintState::default()
    };
    assert!(
        candidates(&speaking)
            .iter()
            .any(|hint| hint.id == "whisper"),
        "an engine turns the tip on"
    );
}
