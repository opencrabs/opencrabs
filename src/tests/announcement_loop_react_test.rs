//! #2129: two defects that made a reaction abort the turn and switch model.
//!
//! 1. Channel directives reached the text-similarity guard. `<<react:👀>>` and
//!    `<<react:👍>>` are different actions, but the normalizer kept letters
//!    only, so both reduced to the single word `react`. The equality shortcut
//!    in `near_duplicate` runs ahead of the short-text guard, so three
//!    DIFFERENT reactions read as three near-duplicates, tripped the
//!    announcement ring, and the second trip aborted the turn. The abort is
//!    provider-attributable, so it walked the fallback chain: a session lost
//!    its chosen model for reacting.
//! 2. `OutgoingTextRing::nudged` was a per-session bool that was never cleared,
//!    so once a session had been nudged, every later trip in any later turn
//!    aborted. It is now a counter capped at [`MAX_TEXT_LOOP_NUDGES`], reset by
//!    a turn that did not trip, per the owner's rule: nudge, retry five times,
//!    then fall back.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::brain::agent::service::announcement_loop::{
    MAX_TEXT_LOOP_NUDGES, OutgoingTextRing, TextLoopAction, near_duplicate,
};

const EYES: &str = "<<react:👀>>";
const THUMB: &str = "<<react:👍>>";
const OK: &str = "<<react:👌>>";
const ANNOUNCEMENT: &str = "Sending the zip to this thread now";

// ---- defect 1: directives never reach the similarity guard ----

#[test]
fn different_reactions_are_not_near_duplicates() {
    // The exact pair from the 2026-10-18 log, which decided an abort.
    assert!(!near_duplicate(THUMB, EYES));
    assert!(!near_duplicate(OK, THUMB));
    assert!(!near_duplicate(EYES, EYES));
}

#[test]
fn two_different_attachments_are_not_duplicates() {
    // Same class, different payload: two image markers must not collide just
    // because they share the directive name.
    assert!(!near_duplicate("<<IMG:/tmp/a.png>>", "<<IMG:/tmp/b.png>>"));
}

#[test]
fn a_directive_around_prose_does_not_change_the_comparison() {
    // The prose is what the guard judges; the marker is channel plumbing.
    assert!(near_duplicate(
        "On it, sending the zip now <<react:👀>>",
        "On it, sending the zip now <<react:👍>>"
    ));
    assert!(!near_duplicate(
        "On it, sending the zip now <<react:👀>>",
        "Checking the failing test first <<react:👍>>"
    ));
}

#[test]
fn three_different_reactions_never_trip_the_ring() {
    // The logged sequence that aborted the turn: 👌, 👍, 👀 in one session.
    let mut ring = OutgoingTextRing::default();
    for reaction in [OK, THUMB, EYES] {
        assert_eq!(
            ring.record_and_check(reaction),
            TextLoopAction::Continue,
            "a distinct reaction tripped the guard: {reaction}"
        );
    }
}

#[test]
fn a_directive_only_text_is_recorded_as_a_sentinel_and_never_trips() {
    // It must still occupy a slot: `last_recorded` is the dedupe key for the
    // final-text check, and leaving a stale older entry there would let the
    // same text be counted twice.
    let mut ring = OutgoingTextRing::default();
    for _ in 0..6 {
        assert_eq!(ring.record_and_check(THUMB), TextLoopAction::Continue);
    }
    assert_eq!(ring.last_recorded(), Some(""));
}

#[test]
fn prose_without_directives_is_untouched_by_the_stripper() {
    // The stripper is shared with the phantom gate, so it must only remove a
    // real `<<...>>` marker and leave ordinary text alone.
    use crate::brain::agent::service::phantom::strip_inline_directives;
    assert_eq!(
        strip_inline_directives("On it, sending the zip now"),
        "On it, sending the zip now"
    );
    assert_eq!(
        strip_inline_directives("On it <<react:👀>>").trim(),
        "On it"
    );
}

// ---- defect 2: five nudges, then the fallback ----

/// Feed near-identical announcements until the ring trips, and return the
/// action that fired.
///
/// Stops at the first trip: an `Abort` ends the turn in the real code (the
/// caller returns `Err` and stops recording), so continuing to feed after one
/// would test a sequence production can never produce.
fn trip(ring: &mut OutgoingTextRing) -> TextLoopAction {
    for _ in 0..3 {
        match ring.record_and_check(ANNOUNCEMENT) {
            TextLoopAction::Continue => {}
            tripped => return tripped,
        }
    }
    TextLoopAction::Continue
}

#[test]
fn the_repeat_fixture_still_reaches_nudge_then_abort_on_the_sixth_nudged_turn() {
    // #957 must keep working: a model that ignores every nudge is still cut
    // off. The owner's rule is nudge, retry five times, then fall back.
    let mut ring = OutgoingTextRing::default();
    for turn in 1..=MAX_TEXT_LOOP_NUDGES {
        assert_eq!(
            trip(&mut ring),
            TextLoopAction::Nudge,
            "turn {turn} must still nudge, not abort"
        );
        ring.end_turn(); // the turn tripped, so the count survives
    }
    assert_eq!(
        trip(&mut ring),
        TextLoopAction::Abort,
        "the sixth nudged turn must reach the fallback path"
    );
}

#[test]
fn one_turn_cannot_spend_the_whole_retry_budget() {
    // The nudge is a note for the NEXT turn. Ten repeats inside one turn are
    // still one retry, otherwise a single noisy turn reaches the provider
    // switch before the model has read a single nudge (#2129).
    let mut ring = OutgoingTextRing::default();
    for _ in 0..10 {
        assert_eq!(trip(&mut ring), TextLoopAction::Nudge);
    }
    ring.end_turn();
    // Exactly one retry was spent, so four nudged turns remain.
    for turn in 2..=MAX_TEXT_LOOP_NUDGES {
        assert_eq!(trip(&mut ring), TextLoopAction::Nudge, "turn {turn}");
        ring.end_turn();
    }
    assert_eq!(trip(&mut ring), TextLoopAction::Abort);
}

#[test]
fn the_abort_releases_the_budget_for_the_next_turn() {
    // An abort ends the turn before the turn-closing hook runs, so the abort
    // itself has to re-arm the guard. Without that, every later turn in the
    // session aborted on its first trip: the #2129 defect one step later.
    let mut ring = OutgoingTextRing::default();
    for _ in 0..MAX_TEXT_LOOP_NUDGES {
        assert_eq!(trip(&mut ring), TextLoopAction::Nudge);
        ring.end_turn();
    }
    assert_eq!(trip(&mut ring), TextLoopAction::Abort);
    assert_eq!(
        trip(&mut ring),
        TextLoopAction::Nudge,
        "the turn after an abort must start with a fresh budget"
    );
}

#[test]
fn a_clean_turn_clears_the_nudge_count() {
    // The reported defect: one trip armed Abort for the rest of the process,
    // so an unrelated trip days later killed a turn and switched provider.
    let mut ring = OutgoingTextRing::default();
    assert_eq!(trip(&mut ring), TextLoopAction::Nudge);
    ring.end_turn(); // this turn tripped, so the count survives

    // A turn that does not trip: the loop stopped, so the nudge was honoured.
    assert_eq!(
        ring.record_and_check("Deployed the frontend to staging, all checks green"),
        TextLoopAction::Continue
    );
    ring.end_turn();

    // The next unrelated trip must nudge again, not abort.
    assert_eq!(
        trip(&mut ring),
        TextLoopAction::Nudge,
        "the nudge count never reset, so this trip aborted the turn"
    );
}

#[test]
fn consecutive_tripping_turns_still_escalate_to_abort() {
    // No clean turn between them, so the count accumulates even across turns.
    let mut ring = OutgoingTextRing::default();
    for _ in 0..MAX_TEXT_LOOP_NUDGES {
        assert_eq!(trip(&mut ring), TextLoopAction::Nudge);
        ring.end_turn();
    }
    assert_eq!(trip(&mut ring), TextLoopAction::Abort);
}

#[test]
fn a_reaction_after_a_nudge_does_not_abort() {
    // End-to-end shape of the field report: the session is nudged, then the
    // model replies with a different reaction. Nothing may abort.
    let mut ring = OutgoingTextRing::default();
    assert_eq!(trip(&mut ring), TextLoopAction::Nudge);
    ring.end_turn();

    for reaction in [OK, THUMB, EYES] {
        assert_ne!(
            ring.record_and_check(reaction),
            TextLoopAction::Abort,
            "a distinct reaction aborted the turn after a nudge: {reaction}"
        );
    }
}

// ---- #1397: the same normalizer path must still tell commands apart ----

#[test]
fn counter_incremented_commands_stay_apart() {
    // #1397: the near-match signature erased the digits that distinguished one
    // legitimate command from another. The directive work must not undo that.
    assert!(near_duplicate(
        "Отправляю 1 подтверждение",
        "Отправляю 2 подтверждение"
    ));
    assert!(!near_duplicate("git push origin main", "cargo test --lib"));
}
