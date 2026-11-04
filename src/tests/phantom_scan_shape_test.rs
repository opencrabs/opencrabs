//! Work-shape guard for the phantom language scan (#2245).
//!
//! Every phantom detector reads its tables across all language packs, so one
//! scan of a narration costs a traversal of the pack list per pack. That cost
//! is meant to scale with the NUMBER OF PACKS and never with the LENGTH of the
//! narration: detection must stay linear in the input, since the input is the
//! model's own narration and can run to thousands of bytes.
//!
//! These tests assert that shape by counting pack traversals rather than
//! timing the scan. A wall-clock budget cannot hold on this box: CI runs on
//! the same Mac that hosts interactive sessions, so any millisecond threshold
//! flakes under contention and gets muted. A count is deterministic and fails
//! exactly when someone makes the scan input-proportional, which is the change
//! worth catching.
//!
//! Added ahead of the Japanese, Korean and German packs (#2245) so the curve
//! is pinned before the corpus grows by roughly a third.

use crate::brain::agent::service::phantom::{
    has_phantom_tool_intent, has_phantom_tool_intent_no_tools,
};
use crate::brain::agent::service::phantom_lang::config::scan_counters;
use crate::brain::agent::service::phantom_lang::{Lang, all_langs};

/// Neutral filler with no words in any pack: no vowels, no spaces, so no
/// intent phrase, action verb or line-start pattern can match it. The tests
/// assert non-matching explicitly, so a filler that starts matching fails
/// loudly instead of quietly shrinking the traversal count.
fn filler(bytes: usize) -> String {
    "zqzqzqzq".repeat(bytes.div_ceil(8))
}

/// Input lengths spanning two orders of magnitude. All exceed the detectors'
/// floors (20 and 40 bytes) so each one reaches the pack scan.
const LENGTHS: [usize; 4] = [100, 400, 1600, 6400];

/// Neutral multi-line filler: the same non-matching chunk, one per line, so the
/// input grows in LINE count as well as in bytes. Real narration is multi-line,
/// and a scan that pays a pack traversal per line is one of the likeliest ways
/// this work shape bends. A byte-growth ladder alone cannot see it: one long
/// line has the same line count at every length, so the mutation that adds a
/// per-line scan passes it silently. That blind spot was measured, not guessed.
fn multi_line_filler(lines: usize) -> String {
    "zqzqzqzq\n".repeat(lines)
}

/// Line counts spanning two orders of magnitude. All clear the detectors' byte
/// floors: 8 lines is 72 bytes.
const LINE_COUNTS: [usize; 4] = [8, 32, 128, 512];

#[test]
fn pack_traversals_do_not_grow_with_input_length() {
    let mut no_tools_counts = Vec::new();
    let mut tool_intent_counts = Vec::new();

    for len in LENGTHS {
        let text = filler(len);

        assert!(
            !has_phantom_tool_intent_no_tools(&text),
            "filler of {len} bytes started matching the no-tools detector; \
             pick a neutral filler, do not relax the assertion"
        );
        scan_counters::reset();
        assert!(!has_phantom_tool_intent_no_tools(&text));
        no_tools_counts.push(scan_counters::traversals());

        assert!(
            !has_phantom_tool_intent(&text),
            "filler of {len} bytes started matching the tool-intent detector; \
             pick a neutral filler, do not relax the assertion"
        );
        scan_counters::reset();
        assert!(!has_phantom_tool_intent(&text));
        tool_intent_counts.push(scan_counters::traversals());
    }

    assert!(
        no_tools_counts.windows(2).all(|pair| pair[0] == pair[1]),
        "has_phantom_tool_intent_no_tools traversals scale with input length: \
         lengths {LENGTHS:?} produced {no_tools_counts:?}; the scan became \
         input-proportional"
    );
    assert!(
        tool_intent_counts.windows(2).all(|pair| pair[0] == pair[1]),
        "has_phantom_tool_intent traversals scale with input length: \
         lengths {LENGTHS:?} produced {tool_intent_counts:?}; the scan became \
         input-proportional"
    );
    assert!(
        no_tools_counts[0] > 0 && tool_intent_counts[0] > 0,
        "a detector completed without traversing the pack list, so the \
         assertion above is vacuous; the counter is no longer wired"
    );
}

#[test]
fn pack_traversals_stay_under_a_pinned_ceiling() {
    let text = filler(1600);

    scan_counters::reset();
    assert!(!has_phantom_tool_intent_no_tools(&text));
    let no_tools = scan_counters::traversals();

    scan_counters::reset();
    assert!(!has_phantom_tool_intent(&text));
    let tool_intent = scan_counters::traversals();

    // Measured at 8 packs. These are ceilings, not targets: they exist so a
    // change that scans the packs once per byte or per line fails here even
    // when the length-independence test above is satisfied by coincidence.
    assert!(
        (1..=64).contains(&no_tools),
        "no-tools detector traversed the pack list {no_tools} times for a \
         1600-byte input; expected a small multiple of one pass"
    );
    assert!(
        (1..=64).contains(&tool_intent),
        "tool-intent detector traversed the pack list {tool_intent} times for \
         a 1600-byte input; expected a small multiple of one pass"
    );
    assert!(
        no_tools + tool_intent <= 96,
        "the two detectors together traversed the pack list {} times for a \
         1600-byte input; the per-pass work grew",
        no_tools + tool_intent
    );
}

/// Sum of every list field across every shipped pack.
fn corpus_entries() -> usize {
    all_langs()
        .iter()
        .map(|pack| {
            pack.intent_phrases.len()
                + pack.action_verbs.len()
                + pack.completion_claims.len()
                + pack.executed_framings.len()
                + pack.intent_verb_exclusions.len()
                + pack.announcement_tail_words.len()
                + pack.tool_use_markers.len()
                + pack.tool_nouns.len()
                + pack.tool_name_determiners.len()
                + pack.media_delivery_phrases.len()
                + pack.media_context_words.len()
                + pack.file_delivery_phrases.len()
                + pack.file_context_words.len()
        })
        .sum()
}

#[test]
fn pack_corpus_stays_inside_its_budget() {
    let entries = corpus_entries();

    // 8 shipped packs hold 7,073 entries (verified with a real TOML parse, not
    // a line grep, which miscounts inline comments). The planned Japanese,
    // Korean and German packs add roughly 880 entries each at the current
    // average, landing near 9,700. The budget is set above that so the three
    // can land without touching this number, and a fourth pack or a runaway
    // phrase list has to raise it deliberately, with the reason written down.
    const BUDGET: usize = 12_000;

    assert!(
        entries <= BUDGET,
        "the phantom pack corpus grew to {entries} entries, over the {BUDGET} \
         budget. If a new language pack is the cause, raise BUDGET here and \
         record which packs landed; if not, a phrase list ballooned"
    );
    assert!(
        entries >= 5_000,
        "the pack corpus shrank to {entries} entries, far under the shipped \
         7,073. A pack likely failed to load or a section was dropped, which \
         would silently weaken detection"
    );
    assert_eq!(
        Lang::ALL.len(),
        all_langs().len(),
        "the registry and the pack list disagree on how many languages ship"
    );
}

#[test]
fn pack_traversals_do_not_grow_with_line_count() {
    let mut no_tools_counts = Vec::new();
    let mut tool_intent_counts = Vec::new();

    for lines in LINE_COUNTS {
        let text = multi_line_filler(lines);

        assert!(
            !has_phantom_tool_intent_no_tools(&text),
            "multi-line filler of {lines} lines started matching the no-tools \
             detector; pick a neutral filler, do not relax the assertion"
        );
        scan_counters::reset();
        assert!(!has_phantom_tool_intent_no_tools(&text));
        no_tools_counts.push(scan_counters::traversals());

        assert!(
            !has_phantom_tool_intent(&text),
            "multi-line filler of {lines} lines started matching the \
             tool-intent detector; pick a neutral filler"
        );
        scan_counters::reset();
        assert!(!has_phantom_tool_intent(&text));
        tool_intent_counts.push(scan_counters::traversals());
    }

    assert!(
        no_tools_counts.windows(2).all(|pair| pair[0] == pair[1]),
        "has_phantom_tool_intent_no_tools traversals scale with LINE count: \
         line counts {LINE_COUNTS:?} produced {no_tools_counts:?}; the scan \
         became line-proportional"
    );
    assert!(
        tool_intent_counts.windows(2).all(|pair| pair[0] == pair[1]),
        "has_phantom_tool_intent traversals scale with LINE count: line \
         counts {LINE_COUNTS:?} produced {tool_intent_counts:?}; the scan \
         became line-proportional"
    );
    assert!(
        no_tools_counts[0] > 0 && tool_intent_counts[0] > 0,
        "a detector completed without traversing the pack list, so the \
         assertion above is vacuous; the counter is no longer wired"
    );
}
