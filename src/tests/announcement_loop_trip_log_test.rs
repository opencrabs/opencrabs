//! #2052: the announcement-loop trip logs the pair that matched, and the logged
//! scores are the scores that decided it. The guard's decision is unchanged.

use crate::brain::agent::service::announcement_loop::{
    OutgoingTextRing, TextLoopAction, near_duplicate, similarity_scores,
};

#[test]
fn three_near_duplicates_still_trip_to_a_nudge() {
    let mut ring = OutgoingTextRing::default();
    assert!(matches!(
        ring.record_and_check("Sending the zip to this thread now"),
        TextLoopAction::Continue
    ));
    assert!(matches!(
        ring.record_and_check("Sending the zip to this thread now"),
        TextLoopAction::Continue
    ));
    assert!(matches!(
        ring.record_and_check("Sending the zip to this thread now"),
        TextLoopAction::Nudge
    ));
}

#[test]
fn distinct_progress_updates_do_not_trip() {
    let mut ring = OutgoingTextRing::default();
    for text in [
        "Now the two walk loops in the cooldown module",
        "Now the test file for the registry",
        "The ProviderError type is not Clone, so the mock changes",
        "Running the focused tests before the full suite",
    ] {
        assert!(matches!(
            ring.record_and_check(text),
            TextLoopAction::Continue
        ));
    }
}

#[test]
fn the_logged_scores_are_the_decided_scores() {
    let a = "sending the zip to this thread now";
    let b = "sending the zip to this thread";
    let (jaccard, overlap) = similarity_scores(a, b).expect("comparable");
    assert_eq!(
        near_duplicate(a, b),
        jaccard >= 0.8 || overlap >= 0.8,
        "the scores the log prints must reproduce the decision"
    );
}

#[test]
fn too_short_texts_have_no_scores_and_are_not_duplicates() {
    assert!(similarity_scores("now the", "now the file").is_none());
    assert!(!near_duplicate("now the", "now the file"));
}
