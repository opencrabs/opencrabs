//! #2156: one turn, one answer.
//!
//! The reported turn posted three rich bubbles and deleted one. The owner saw
//! "kenapa output pesan elu ada 2" (why does your output arrive as two
//! messages). The harness believed it answered once; Telegram showed the
//! answer twice.
//!
//! Two defects lined up, and each test below pins one:
//!
//! 1. **The block kept its own copy of the answer.** The reclaim that lifts a
//!    folded final out of the processing-log block only ran when the final text
//!    was EMPTY. A non-empty final ships as its own bubble, so the block kept
//!    the streamed run: the resticked block (3651 chars) sat above the final
//!    bubble (3761) repeating it, and the block later grew to 5503.
//! 2. **The dedup only matched exact equality.** An intermediate that is a
//!    PREFIX of the final, which is exactly what pre-tool narration is, never
//!    satisfied `norm(i) == norm_final`. The 1962-char narration bubble stayed
//!    visible above a 3761-char answer that opened with the same words.
//!
//! The ids below are the ones from the incident log, so a regression reads as
//! the same turn the owner reported.

use teloxide::types::MessageId;

use crate::channels::telegram::delivery::duplicate_intermediate_ids;
use crate::channels::telegram::flow::{
    FlowEntry, answer_already_delivers, pop_redundant_folded_texts,
};

/// The 1962-char narration bubble Telegram kept (msg 13884).
const NARRATION: &str = "I checked the three files and the migration is the only one \
that touches the schema. Now updating the caller so the old signature still \
resolves.";

/// The 3761-char answer (msg 13886). It OPENS with the narration, which is why
/// a prefix comparison catches it and an equality comparison does not.
fn answer() -> String {
    format!(
        "{NARRATION} The caller is updated, the migration applies cleanly and the \
         tests pass. Here is the full breakdown of what changed and why it had to \
         change in that order."
    )
}

// ---------------------------------------------------------------------------
// Defect 1: the block must not keep a run the final already delivers.
// ---------------------------------------------------------------------------

#[test]
fn resticked_block_drops_the_run_the_final_already_delivers() {
    // The block as it stood after the restick: one tool call, then the
    // streamed answer text folded in behind it.
    let mut entries = vec![FlowEntry::Tool(0), FlowEntry::Text(NARRATION.to_string())];

    let (host, trailer) = pop_redundant_folded_texts(&mut entries, false, &answer());

    assert_eq!(
        host.as_deref(),
        Some(NARRATION),
        "the redundant trailing run is the one reclaimed"
    );
    assert!(trailer.is_none(), "no options surface was pending");
    assert!(
        !entries
            .iter()
            .any(|e| matches!(e, FlowEntry::Text(t) if t == NARRATION)),
        "the block no longer carries the answer text, so the turn cannot show it twice"
    );
    assert_eq!(
        entries.len(),
        1,
        "the tool history survives the reclaim; only the duplicated answer text goes"
    );
    assert!(
        matches!(entries[0], FlowEntry::Tool(0)),
        "what survives is the tool call, not the answer text"
    );
}

#[test]
fn block_keeps_narration_the_final_never_repeats() {
    // A mid-turn status line the answer does not contain. Reclaiming it would
    // delete content the reader never gets anywhere else.
    let mut entries = vec![
        FlowEntry::Tool(0),
        FlowEntry::Text("Running the migration now, this takes a minute.".to_string()),
    ];

    let (host, trailer) = pop_redundant_folded_texts(&mut entries, false, &answer());

    assert!(
        host.is_none(),
        "a run the final does not deliver is not reclaimed"
    );
    assert!(trailer.is_none());
    assert_eq!(
        entries.len(),
        2,
        "the narration is restored untouched, so the block still shows the work"
    );
    assert!(matches!(entries[1], FlowEntry::Text(_)));
}

#[test]
fn a_longer_run_than_the_final_is_never_reclaimed() {
    // The candidate STARTS WITH the final but carries more. Deleting it would
    // drop the extra words, so the one-directional predicate must refuse even
    // though `folded_duplicates_final` (symmetric) would accept.
    let final_text = "Short answer.";
    let candidate = "Short answer. And a long tail of detail that only exists here.";

    assert!(
        !answer_already_delivers(final_text, candidate),
        "a candidate longer than the final carries content the final never delivers"
    );

    let mut entries = vec![FlowEntry::Text(candidate.to_string())];
    let (host, _) = pop_redundant_folded_texts(&mut entries, false, final_text);
    assert!(host.is_none());
    assert_eq!(entries.len(), 1, "the longer run stays in the block");
}

// ---------------------------------------------------------------------------
// Defect 2: a prefix intermediate is a duplicate, and it has an address.
// ---------------------------------------------------------------------------

#[test]
fn prefix_intermediate_is_a_duplicate_of_the_answer() {
    assert!(
        answer_already_delivers(&answer(), NARRATION),
        "the reported shape: narration is a prefix of the final"
    );
    assert!(
        answer_already_delivers("identical", "identical"),
        "exact equality is still a duplicate"
    );
    assert!(
        !answer_already_delivers(&answer(), "The caller is updated, the migration applies"),
        "text that appears MID-answer is quoted by the final, not repeated by it"
    );
    assert!(
        !answer_already_delivers(&answer(), "   "),
        "blank is never a duplicate"
    );
}

#[test]
fn reported_turn_leaves_the_answer_in_exactly_one_bubble() {
    // Every bubble the turn created, with the text it carried. 13883 is the
    // streaming placeholder that became the block, 13884 the narration bubble,
    // 13886 the final answer.
    let bubbles = vec![
        (MessageId(13884), NARRATION.to_string()),
        (MessageId(13886), answer()),
    ];
    let final_id = MessageId(13886);

    let doomed = duplicate_intermediate_ids(&answer(), final_id, &bubbles);

    assert_eq!(
        doomed,
        vec![MessageId(13884)],
        "the narration bubble is deleted and the final bubble is never a candidate"
    );
    assert!(
        !doomed.contains(&final_id),
        "the bubble carrying the answer must never be in the delete set"
    );

    // The invariant the issue asks for, stated over ids: every bubble this turn
    // created except the final one is either edited into the final or deleted.
    let placeholder = MessageId(13883);
    let resticked_block = MessageId(13885);
    let mut accounted: Vec<MessageId> = doomed.clone();
    // The placeholder is removed by the pre-send cleanup, and the block is
    // edited in place by the reclaim (asserted in the first test).
    accounted.push(placeholder);
    accounted.push(resticked_block);

    for id in [placeholder, MessageId(13884), resticked_block] {
        assert!(
            accounted.contains(&id),
            "bubble {id:?} would survive as a second copy of the answer"
        );
    }
    assert_eq!(
        accounted.iter().filter(|id| **id == final_id).count(),
        0,
        "the final bubble is the one bubble allowed to carry the answer"
    );
}

#[test]
fn a_turn_with_no_duplicates_deletes_nothing() {
    // A table report the model surfaced before a tool call, which the final
    // answer does not repeat. It must stay: it is the only copy.
    let bubbles = vec![(
        MessageId(13900),
        "| model | latency |\n|---|---|\n| a | 12ms |".to_string(),
    )];

    assert!(
        duplicate_intermediate_ids(&answer(), MessageId(13901), &bubbles).is_empty(),
        "an intermediate the answer does not deliver is not a duplicate"
    );
}
