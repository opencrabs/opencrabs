//! Structured completion reports are never discarded (#1506).
//!
//! Every phantom detector matches verbatim strings — intent phrases, quoted
//! commands, sha/tally tokens — and a distilled turn summary paraphrases by
//! construction. On 2026-09-10 four legitimate batch reports (1.8-2.0 KB,
//! markdown tables included, every fact real) were killed by the mid-loop
//! self-heal: `StripStreamedContent` drained the streamed report and the
//! retry produced a shortened rewrite. The owner's standing directive: a
//! report with tables and structured data must NEVER be discarded.
//!
//! The fix is two-part in `tool_loop`'s kill block: (1) the ten detector
//! branches hoist into a named `fired_branches` list so the WARN attributes
//! exactly what fired — previously unattributable, per the read-only audit;
//! (2) `is_structured_report` exempts the genre from the discard wholesale,
//! with the fired branches logged for forensics.
//!
//! Deliberate trade-off, owner-ruled: #1423's fabricated table report would
//! now be delivered instead of discarded, with a WARN trail instead of
//! silence. The zero-tool turn-end verdict (#752) does NOT exempt structured
//! reports (#1541 fix): a structured report with zero tools is fabricated by
//! construction, so the turn-end note fires. The in-loop exemption at #1506
//! is gated on `tool_calls_completed_this_turn > 0`, so it only protects
//! legitimate post-tool summaries. Every prose-shaped detector keeps full
//! authority over non-structured narration.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::brain::agent::service::phantom::is_structured_report;

const TOOL_LOOP_SRC: &str = include_str!("../brain/agent/service/tool_loop.rs");

/// The shape that got killed on 2026-09-10: a batch-completion report with a
/// verdict table, tallies and shas. Every fact in it was real; the detectors
/// fired anyway because distillation paraphrases.
const BATCH_REPORT: &str = "Batch done. 13/13 tasks, all 10 PRs merged locally.\n\
     \n\
     | Merge | PR | Surface |\n\
     |---|---|---|\n\
     | `8b1e1928` | #1503 | +761, 8 files |\n\
     | `147f434b` | #1501 | exclusion list |\n\
     \n\
     Gates: check 0, clippy 0, suite 8228 passed / 0 failed.\n";

// ─── the genre detector ──────────────────────────────────────

#[test]
fn a_batch_report_with_a_verdict_table_is_structured() {
    assert!(is_structured_report(BATCH_REPORT));
}

#[test]
fn a_table_needs_a_data_row_not_just_a_separator() {
    // Header + separator with zero data rows is furniture without content;
    // not enough to exempt a text from every detector.
    let header_only = "Gate | Verdict |\n|---|---|\nprose continues here, no rows follow";
    assert!(!is_structured_report(header_only));
}

#[test]
fn an_alphanumeric_pipe_row_is_data_never_a_separator() {
    // `|---|---|` is a separator; `| a--b |` is a cell containing dashes. A
    // table needs the former.
    let no_separator = "| Gate | Verdict |\n| a--b | c--d |\n| e | f |";
    assert!(!is_structured_report(no_separator));
    let with_separator = "| Gate | Verdict |\n|---|---|\n| a | b |";
    assert!(is_structured_report(with_separator));
}

#[test]
fn two_section_headings_are_a_structured_document() {
    let doc = "## What landed\n\nProse paragraph.\n\n## Gates\n\nMore prose.\n";
    assert!(is_structured_report(doc));
}

#[test]
fn one_heading_is_not_enough() {
    // A single heading is a label, not a document. Short prose under one
    // heading keeps full detector coverage.
    let one = "## Summary\n\nCommitted as deadbeef42 and pushed.\n";
    assert!(!is_structured_report(one));
}

#[test]
fn prose_fabrications_are_never_structured() {
    // The exemption must not become a blanket pass: these are exactly the
    // shapes the detectors exist for, and they keep dying.
    for text in [
        "Done.",
        "Pushed to origin.",
        "Committed as deadbeef42, tests green.",
        "Batch done. 13/13 tasks, all merged locally, CI green.",
        "I'll dig into the delete endpoint, the send path, and the flow.",
    ] {
        assert!(
            !is_structured_report(text),
            "{text:?} wrongly classed as structured"
        );
    }
}

#[test]
fn a_table_buried_in_prose_still_counts() {
    // Real reports lead with prose and put the table mid-body; the scan is
    // line-based, not position-based.
    let buried = "All work is committed and the tree is clean.\n\
          \n\
          | Commit | What |\n\
          |---|---|\n\
          | abc1234 | the fix |\n\
          \n\
          Push next.\n";
    assert!(is_structured_report(buried));
}

#[test]
fn an_indented_or_padded_table_still_counts() {
    // Reports paste tool output with leading whitespace; trim before the
    // pipe check.
    let padded = "Receipts:\n\n  | Gate | Exit |\n  |---|---|\n  | check | 0 |\n";
    assert!(is_structured_report(padded));
}

// ─── wiring sentinels: attribution (#1506 audit gap) ──────────

#[test]
fn the_kill_block_builds_a_named_branch_list() {
    // Without attribution the WARN cannot say which detector fired, which is
    // what made the 2026-09-10 kills undiagnosable from logs alone.
    let list = TOOL_LOOP_SRC
        .split("let fired_branches")
        .nth(1)
        .expect("fired_branches list not found in tool_loop");
    let list = list.split("];").next().expect("unterminated list");
    for label in [
        "intent_no_tools",
        "intent_full_text",
        "tool_name_narrated",
        "shell_fence_narrated",
        "unbacked_side_effects",
        "bare_completion_delivery",
        "media_claim_no_marker",
        "uncalled_commands",
        "unbacked_evidence",
        "unbacked_facts",
    ] {
        assert!(list.contains(label), "branch label `{label}` missing");
    }
}

#[test]
fn the_warn_carries_the_branch_names() {
    let block = TOOL_LOOP_SRC
        .split("Phantom tool call detected")
        .nth(1)
        .expect("phantom WARN not found");
    let block = block.split('\n').take(4).collect::<Vec<_>>().join("\n");
    assert!(
        block.contains("branches={:?}") || block.contains("branches = ?"),
        "the phantom WARN does not name the fired branches: {block}"
    );
}

// ─── wiring sentinels: the structured-report exemption ────────

#[test]
fn the_kill_gate_consults_the_structured_report_check() {
    assert!(
        TOOL_LOOP_SRC.contains("let structured_report ="),
        "the kill block never computes the structured-report classification"
    );
    assert!(
        TOOL_LOOP_SRC.contains("super::phantom::is_structured_report(&iteration_text)"),
        "the kill block does not consult is_structured_report"
    );
}

#[test]
fn the_exempt_path_is_suppression_not_a_branch_of_the_kill() {
    // The suppression branch must fire BEFORE the kill body: a structured
    // report takes the log-and-deliver path, never the discard path.
    let gate = TOOL_LOOP_SRC
        .split("if kill && structured_report")
        .nth(1)
        .expect("suppression branch not found");
    let gate = gate
        .split("} else if kill {")
        .next()
        .expect("kill branch not found");
    assert!(
        gate.contains("phantom suppressed"),
        "the suppression path is not logged"
    );
    assert!(
        !gate.contains("phantom_detections_total += 1"),
        "the suppression path burns the retry budget — it must not"
    );
    assert!(
        !gate.contains("StripStreamedContent"),
        "the suppression path drains the streamed report — the exact #1506 bug"
    );
}

#[test]
fn the_kill_still_requires_a_fired_branch_and_budget() {
    // The exemption narrows the kill; it must not relax its other gates.
    let gate = TOOL_LOOP_SRC
        .split("let kill =")
        .nth(1)
        .expect("kill gate not found");
    let gate = gate.split(';').next().unwrap();
    assert!(gate.contains("phantom_retries_used < MAX_PHANTOM_RETRIES"));
    assert!(gate.contains("phantom_detections_total < MAX_PHANTOM_DETECTIONS_TOTAL"));
    assert!(gate.contains("phantom_eligible"));
    assert!(gate.contains("!fired_branches.is_empty()"));
}

#[test]
fn the_suppressed_report_logs_its_branches_for_forensics() {
    // The #1423 trade-off is visible, not silent: a fabricated table report
    // ships with a WARN naming what would have fired.
    let gate = TOOL_LOOP_SRC
        .split("if kill && structured_report")
        .nth(1)
        .expect("suppression branch not found");
    let gate = gate.split("} else if kill {").next().unwrap();
    assert!(
        gate.contains("branches = ?fired_branches"),
        "the suppression WARN does not name the fired branches"
    );
    assert!(
        gate.contains("text_len"),
        "the suppression WARN lacks a size breadcrumb"
    );
}

// ─── wiring sentinel: the turn-end verdict honors the exemption (#1541) ───

#[test]
fn the_turn_end_verdict_does_not_exempt_structured_reports_with_zero_tools() {
    // #1541 fix: the turn-end verdict is already gated on
    // `tool_calls_completed_this_turn == 0`. A structured completion report
    // with zero tools is fabricated by construction — the #1541 exemption
    // was only valid post-tool, where it belongs (see the in-loop gate at
    // #1506, which now checks `tool_calls_completed_this_turn > 0`).
    // The old test asserted the exemption existed here; that WAS the bug.
    let verdict = TOOL_LOOP_SRC
        .split("Turn-end phantom verdict (#752)")
        .nth(1)
        .expect("#752 turn-end verdict not found in tool_loop");
    let verdict = verdict
        .split("final_text = \"I described actions")
        .next()
        .expect("#752 note assignment not found");
    assert!(
        !verdict.contains("!super::phantom::is_structured_report(&final_text)"),
        "the #752 turn-end verdict must NOT exempt structured reports — zero tools means fabricated"
    );
    // The exemption DOES belong in-loop, gated on tools having run.
    assert!(
        TOOL_LOOP_SRC.contains("kill && structured_report && tool_calls_completed_this_turn > 0"),
        "the #1506 in-loop exemption must be gated on tool_calls_completed_this_turn > 0"
    );
}

#[test]
fn the_turn_end_verdict_still_fires_for_genuine_prose_narration() {
    // #1541 test spec 2: the exemption is scoped to structured reports only.
    // A turn with zero tools that merely NARRATES work (phantom intent,
    // unbacked side-effect/media claims, unrun shell blocks) must still get
    // stripped and replaced with the truthful note, so the note remains the
    // last-resort backstop for prose phantoms and never quietly disappears.
    let verdict = TOOL_LOOP_SRC
        .split("Turn-end phantom verdict (#752)")
        .nth(1)
        .expect("#752 turn-end verdict not found in tool_loop");
    let verdict = verdict
        .split("final_text = \"I described actions")
        .next()
        .expect("#752 note assignment not found");
    for detector in [
        "has_phantom_tool_intent_no_tools(&final_text)",
        "claims_unbacked_side_effects(&final_text)",
        "claims_unbacked_media_result(&final_text)",
        "narrates_unrun_shell_block(&final_text)",
    ] {
        assert!(
            verdict.contains(detector),
            "the #752 verdict lost its prose detector `{detector}` — the note must still fire for narration (#1541 spec 2)"
        );
    }
    assert!(
        verdict.contains("tool_calls_completed_this_turn == 0"),
        "the #752 verdict must keep the zero-tools gate"
    );
    assert!(
        TOOL_LOOP_SRC.contains("final_text = \"I described actions"),
        "the #752 truthful note assignment is gone — prose phantoms would ship unchallenged"
    );
}
