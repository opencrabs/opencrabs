//! Tests for the #1226 K fix and the #31 trailer reclaim.
//!
//! #1226 incident (smoke v3, 2026-08-28): the model delivered its final
//! answer mid-turn, then called suggest_options; the turn halted with
//! `options_pending`, the promote branch asked for the folded final, got
//! None silently, and the answer stayed imprisoned (badly formatted) in
//! the flow block while the buttons landed standalone.
//!
//! The fix gives `pop_trailing_folded_texts` an options-aware gate: when
//! options are pending it lifts the trailing Tool run aside, pops the Text
//! run immediately before it, and RESTORES the Tool run on top — only the
//! answer text leaves the block, the tool history stays. The gate never
//! fires on turns that did not halt on an option surface.
//!
//! #31 extension (smoke v4 abandonment): after the halt, the model's
//! text-only sign-off iteration folds ANOTHER Text run into the flow —
//! AFTER the suggest_options Tool entry. The gate now returns BOTH runs:
//! `(host, trailer)` — the answer before the Tool, the sign-off after it.
//! Nothing is discarded; `settle_options_reclaim` arbitrates against
//! `response.content` (host wins the content slot, duplicates die,
//! an unfolded content leftover is promoted to the trailer).

use crate::channels::telegram::flow::{
    FlowEntry, MAX_TRAILER_CHARS, normalized_body, pop_trailing_folded_texts,
    settle_options_reclaim, trailer_promotes_to_answer,
};

#[test]
fn options_pending_reclaims_answer_before_trailing_tool() {
    // The K shape: answer folded, then suggest_options appends a Tool
    // entry LAST. The gate must reach past the tool and pop the answer.
    let mut entries = vec![
        FlowEntry::Text("the substantive answer".into()),
        FlowEntry::Tool(4),
    ];
    let (host, trailer) = pop_trailing_folded_texts(&mut entries, true);
    assert_eq!(host.as_deref(), Some("the substantive answer"));
    assert_eq!(trailer, None, "no post-halt run folded");
    // The Tool entry is RESTORED — the flow block keeps its tool history.
    assert_eq!(entries.len(), 1, "tool entry restored after reclaim");
    assert!(matches!(entries[0], FlowEntry::Tool(4)));
}

#[test]
fn options_pending_pops_whole_run_before_tool() {
    // Multi-part answer (#478 run semantics carry across the gate): both
    // parts come out, joined in order; earlier narration behind an older
    // tool stays in the block.
    let mut entries = vec![
        FlowEntry::Text("mid-turn narration".into()),
        FlowEntry::Tool(2),
        FlowEntry::Text("answer part one".into()),
        FlowEntry::Text("answer part two".into()),
        FlowEntry::Tool(5),
    ];
    let (host, trailer) = pop_trailing_folded_texts(&mut entries, true);
    assert_eq!(host.as_deref(), Some("answer part one\n\nanswer part two"));
    assert_eq!(trailer, None);
    assert_eq!(entries.len(), 3, "narration + old tool + suggest tool stay");
    assert!(matches!(entries[0], FlowEntry::Text(_)));
    assert!(matches!(entries[1], FlowEntry::Tool(2)));
    assert!(matches!(entries[2], FlowEntry::Tool(5)));
}

#[test]
fn options_pending_restores_multiple_trailing_tools_in_order() {
    let mut entries = vec![
        FlowEntry::Text("answer".into()),
        FlowEntry::Tool(1),
        FlowEntry::Tool(2),
    ];
    let (host, trailer) = pop_trailing_folded_texts(&mut entries, true);
    assert_eq!(host.as_deref(), Some("answer"));
    assert_eq!(trailer, None);
    assert_eq!(entries.len(), 2, "both tools restored");
    assert!(matches!(entries[0], FlowEntry::Tool(1)));
    assert!(matches!(entries[1], FlowEntry::Tool(2)));
}

#[test]
fn options_pending_without_preceding_text_changes_nothing() {
    // No Text run before the trailing tools: nothing to reclaim, and the
    // entries must come back untouched (no mutation on None).
    let mut entries = vec![FlowEntry::Tool(3), FlowEntry::Tool(4)];
    let (host, trailer) = pop_trailing_folded_texts(&mut entries, true);
    assert_eq!(host, None);
    assert_eq!(trailer, None);
    assert_eq!(entries.len(), 2);
    assert!(matches!(entries[0], FlowEntry::Tool(3)));
    assert!(matches!(entries[1], FlowEntry::Tool(4)));
}

#[test]
fn options_pending_pops_host_and_trailer_around_tool() {
    // #31 both-runs shape: the sign-off folds AFTER the suggest_options
    // Tool entry. The gate returns host AND trailer; the Tool entry is
    // restored so the flow block keeps its tool history.
    let mut entries = vec![
        FlowEntry::Text("mid-turn narration".into()),
        FlowEntry::Tool(2),
        FlowEntry::Text("the substantive answer".into()),
        FlowEntry::Tool(4),
        FlowEntry::Text("trailer line one".into()),
        FlowEntry::Text("trailer line two".into()),
    ];
    let (host, trailer) = pop_trailing_folded_texts(&mut entries, true);
    assert_eq!(host.as_deref(), Some("the substantive answer"));
    assert_eq!(
        trailer.as_deref(),
        Some("trailer line one\n\ntrailer line two"),
        "post-halt run joins in order"
    );
    assert_eq!(entries.len(), 3, "narration + old tool + suggest tool stay");
    assert!(matches!(entries[0], FlowEntry::Text(_)));
    assert!(matches!(entries[1], FlowEntry::Tool(2)));
    assert!(matches!(entries[2], FlowEntry::Tool(4)));
}

#[test]
fn options_pending_trailer_only_when_no_host_text() {
    // The answer arrived in response.content (never folded), but the
    // sign-off still folded after the Tool entry: trailer comes back
    // alone, host None, tool history intact.
    let mut entries = vec![
        FlowEntry::Tool(4),
        FlowEntry::Text("all set — sign-off".into()),
    ];
    let (host, trailer) = pop_trailing_folded_texts(&mut entries, true);
    assert_eq!(host, None);
    assert_eq!(trailer.as_deref(), Some("all set — sign-off"));
    assert_eq!(entries.len(), 1);
    assert!(matches!(entries[0], FlowEntry::Tool(4)));
}

#[test]
fn options_pending_trailing_text_without_tool_is_host() {
    // Tolerance: no Tool entry trailed the run — the gate's precondition
    // (flow ends on the suggest Tool) does not hold, so the popped run is
    // the stock trailing answer, NEVER a trailer.
    let mut entries = vec![FlowEntry::Text("plain trailing answer".into())];
    let (host, trailer) = pop_trailing_folded_texts(&mut entries, true);
    assert_eq!(host.as_deref(), Some("plain trailing answer"));
    assert_eq!(trailer, None);
    assert!(entries.is_empty());
}

#[test]
fn gate_closed_keeps_tool_last_invariant() {
    // options_pending = false: a tool-ended flow still reclaims nothing —
    // the stock invariant (answer is in response.content) is untouched
    // for every turn that did not halt on an option surface.
    let mut entries = vec![
        FlowEntry::Text("the substantive answer".into()),
        FlowEntry::Tool(4),
    ];
    let (host, trailer) = pop_trailing_folded_texts(&mut entries, false);
    assert_eq!(host, None);
    assert_eq!(trailer, None);
    assert_eq!(entries.len(), 2, "nothing consumed");
}

#[test]
fn gate_closed_on_trailing_text_unchanged() {
    // Regression guard: the gate's plumbing must not disturb the stock
    // trailing-run pop when the flow ends on Text.
    let mut entries = vec![
        FlowEntry::Tool(0),
        FlowEntry::Text("plain final answer".into()),
    ];
    let (host, trailer) = pop_trailing_folded_texts(&mut entries, false);
    assert_eq!(host.as_deref(), Some("plain final answer"));
    assert_eq!(trailer, None);
    assert_eq!(entries.len(), 1);
}

#[test]
fn settle_host_wins_content_slot() {
    // The old prepend shipped the ack inside the answer bubble (#31
    // abandonment): host takes the content slot, the ack rides AFTER the
    // buttons as the trailer.
    let (text, trailer) = settle_options_reclaim(
        "all set, pushed".into(),
        Some("the substantive answer".into()),
        Some("sign-off paragraph".into()),
    );
    assert_eq!(text, "the substantive answer");
    assert_eq!(trailer.as_deref(), Some("sign-off paragraph"));
}

#[test]
fn settle_drops_trailer_duplicate_of_host() {
    // Provider double-copy (inferhub, smoke v4/S4): the sign-off run IS
    // the final text — no double-send.
    let (text, trailer) = settle_options_reclaim(
        "ack".into(),
        Some("the substantive answer".into()),
        Some("the substantive answer".into()),
    );
    assert_eq!(text, "the substantive answer");
    assert_eq!(trailer, None);
}

#[test]
fn settle_drops_trailer_that_repeats_the_answer_as_markdown_links() {
    // Observed in production on 2026-11-04: `content` had already been through
    // the file-marker pass (`📎 label`, U+200B hidden inside the label), while
    // the folded trailer still carried the model's raw
    // `[label](</abs path>)`. Same answer in two renderings: the old
    // whitespace-only normalization called them different, so the trailer rode
    // after the buttons and the whole answer shipped twice in one 6051-char
    // bubble.
    let marker_form = "Laporan ada di sini:\n\
         📎 LAPORAN-021126-1437.\u{200b}md · 📎 BUKTI-021126-1437.\u{200b}md";
    let link_form = "Laporan ada di sini:\n\
         [LAPORAN-021126-1437.md](</home/adijr/reports/LAPORAN-021126-1437.md>) · \
         [BUKTI-021126-1437.md](</home/adijr/reports/BUKTI-021126-1437.md>)";
    let (text, trailer) =
        settle_options_reclaim(marker_form.to_string(), None, Some(link_form.to_string()));
    assert_eq!(text, marker_form);
    assert_eq!(trailer, None, "the same answer must not ride twice");

    // Same comparison with the host carrying the raw form instead.
    let (text, trailer) = settle_options_reclaim(
        "unused".into(),
        Some(link_form.to_string()),
        Some(marker_form.to_string()),
    );
    assert_eq!(text, link_form);
    assert_eq!(trailer, None);
}

#[test]
fn settle_content_becomes_trailer_when_no_run_folded() {
    // Keep-never-discard: the popper found no trailer run (the ack never
    // folded into the flow) but content carries a non-duplicate leftover —
    // it BECOMES the trailer instead of vanishing.
    let (text, trailer) = settle_options_reclaim(
        "unfolded sign-off ack".into(),
        Some("the substantive answer".into()),
        None,
    );
    assert_eq!(text, "the substantive answer");
    assert_eq!(trailer.as_deref(), Some("unfolded sign-off ack"));
}

#[test]
fn settle_content_duplicate_of_host_not_promoted() {
    // The content leftover is a duplicate of the host (prefix overlap) —
    // promoting it would double-send the answer.
    let (text, trailer) = settle_options_reclaim(
        "the substantive answer".into(),
        Some("the substantive answer delivered in full".into()),
        None,
    );
    assert_eq!(text, "the substantive answer delivered in full");
    assert_eq!(trailer, None);
}

#[test]
fn settle_no_host_uses_content() {
    // Nothing folded before the Tool: content IS the final text, trailer
    // rides unchanged.
    let (text, trailer) =
        settle_options_reclaim("the whole answer".into(), None, Some("sign-off".into()));
    assert_eq!(text, "the whole answer");
    assert_eq!(trailer.as_deref(), Some("sign-off"));
}

#[test]
fn trailer_promotes_to_answer_matrix() {
    // Pure threshold matrix (#58): a fence promotes at any size; no fence
    // promotes only past MAX_TRAILER_CHARS; the cap itself does NOT
    // promote (strictly greater — a run AT the cap is still a trailer).
    assert!(trailer_promotes_to_answer(true, 10));
    assert!(!trailer_promotes_to_answer(false, MAX_TRAILER_CHARS));
    assert!(trailer_promotes_to_answer(false, MAX_TRAILER_CHARS + 1));
}

#[test]
fn settle_no_host_no_trailer_is_stock() {
    // Plain shape: content only, no runs — the stock reclaim result.
    let (text, trailer) = settle_options_reclaim("the whole answer".into(), None, None);
    assert_eq!(text, "the whole answer");
    assert_eq!(trailer, None);
}

#[test]
fn settle_oversized_trailer_promoted_to_answer() {
    // The #58 incident shape, cap leg (config-independent — no fence): the
    // post-halt run carries the whole substantive answer while the pre-Tool
    // host is just the ack. Position said "trailer"; size says "answer".
    // The big text takes the answer slot, the ack demotes to the trailer.
    let big_answer = "word ".repeat(121);
    assert!(big_answer.chars().count() > MAX_TRAILER_CHARS);
    let (text, trailer) = settle_options_reclaim(
        "unused content".into(),
        Some("Got it — one moment.".into()),
        Some(big_answer.clone()),
    );
    assert_eq!(text, big_answer, "oversized trailer promoted to the answer");
    assert_eq!(
        trailer.as_deref(),
        Some("Got it — one moment."),
        "displaced host demotes to trailer"
    );
}

#[test]
fn settle_small_trailer_stays_trailer() {
    // #58 regression guard: a normal-sized fence-less sign-off must NOT be
    // promoted — the rule fires on fences or oversize, nothing else.
    let (text, trailer) = settle_options_reclaim(
        "content".into(),
        Some("the substantive answer".into()),
        Some("all set — sign-off".into()),
    );
    assert_eq!(text, "the substantive answer");
    assert_eq!(trailer.as_deref(), Some("all set — sign-off"));
}

#[test]
fn settle_oversized_trailer_empty_host_drops_demote() {
    // CLI-provider shape (content empty, nothing folded): promoting the
    // oversized trailer leaves NOTHING to demote — the trailer slot dies
    // rather than shipping an empty ack bubble after the buttons.
    let big_answer = "word ".repeat(121);
    let (text, trailer) = settle_options_reclaim(String::new(), None, Some(big_answer.clone()));
    assert_eq!(text, big_answer);
    assert_eq!(
        trailer, None,
        "empty host demotes to None, not an empty trailer"
    );
}

#[test]
fn settle_demoted_host_trips_guard_concatenates_into_answer() {
    // #92 demoted-host leg (the #58 matrix covered the trailer leg only):
    // the displaced host itself trips the promotion guard — an oversized
    // run, the config-independent leg (`should_render_mermaid` reads
    // `Config::current()`; the fence leg shares the same guard call pinned
    // by `trailer_promotes_to_answer_matrix`). Before the fix the
    // fence-bearing host rode the trailer slot and shipped raw; now it is
    // concatenated after the promoted trailer in the answer slot so BOTH
    // texts ride the final-answer render path.
    let big_host = "host ".repeat(121);
    let big_trailer = "trailer ".repeat(76);
    assert!(big_host.chars().count() > MAX_TRAILER_CHARS);
    assert!(big_trailer.chars().count() > MAX_TRAILER_CHARS);
    let (text, trailer) = settle_options_reclaim(
        "unused content".into(),
        Some(big_host.clone()),
        Some(big_trailer.clone()),
    );
    assert_eq!(
        text,
        format!("{big_trailer}\n\n{big_host}"),
        "promoted trailer keeps the answer head; the tripping demoted host \
         concatenates after it"
    );
    assert_eq!(
        trailer, None,
        "trailer slot dies — both texts ride the answer render path"
    );
}

// ---------------------------------------------------------------------------
// normalized_body: file-reference canonicalization (2026-11-04)
//
// One answer arrived twice inside a single bubble: the delivered half carried
// the visible `📎 label` markers the file pass renders, the folded half still
// carried the model's raw `[label](</abs path>)`. Whitespace-only normalization
// called those different, so the dedupe checks let both ride.
// ---------------------------------------------------------------------------

#[test]
fn normalized_body_unifies_marker_and_local_link_forms() {
    let marker = "Laporan:\n📎 LAPORAN-021126-1437.\u{200b}md · 📎 BUKTI-021126-1437.\u{200b}md";
    let link = "Laporan:\n[LAPORAN-021126-1437.md](</home/adijr/reports/LAPORAN-021126-1437.md>) · \
                [BUKTI-021126-1437.md](</home/adijr/reports/BUKTI-021126-1437.md>)";
    assert_eq!(
        normalized_body(marker),
        normalized_body(link),
        "the same answer rendered as markers and as markdown links must compare equal"
    );
}

#[test]
fn normalized_body_ignores_zero_width_space_only() {
    let plain = "LAPORAN-021126-1437.md sudah siap";
    let zwsp = "LAPORAN-021126-1437.\u{200b}md sudah siap";
    assert_eq!(
        normalized_body(plain),
        normalized_body(zwsp),
        "an invisible space must not make two renderings differ"
    );
}

#[test]
fn normalized_body_keeps_different_answers_distinct() {
    let a = "Laporan:\n📎 LAPORAN-021126-1437.md";
    let b = "Laporan:\n📎 BUKTI-021126-1437.md";
    assert_ne!(
        normalized_body(a),
        normalized_body(b),
        "different files are different answers; canonicalization must not merge them"
    );
}

#[test]
fn normalized_body_keeps_remote_link_targets_distinct() {
    let a = "lihat [issue](https://example.com/issues/1)";
    let b = "lihat [issue](https://example.com/issues/2)";
    assert_ne!(
        normalized_body(a),
        normalized_body(b),
        "only local file targets are unwrapped; remote destinations still matter"
    );
}
