//! A fabrication that rode a real tool call was never checked (#1693).
//!
//! Observed 2026-09-22 19:14:10, session `a58b8714`, one response, two content
//! blocks: text asserting `Gate died on 2× E0603 "this item is private"`, and a
//! `bash` call whose command was `arch -arm64 cargo clippy -p opencrabs
//! --all-features --tests` — the gate it had just claimed was dead. The log
//! records `Found 1 tool uses to execute`, so `tool_uses.len() == 1` and the
//! entire 1,341-line phantom block behind `if tool_uses.is_empty()` was skipped.
//! The clippy receipt it referred to reads `clippy exit=0`.
//!
//! Two independent gaps had to line up for that sentence to reach the user, and
//! the issue as first filed named only one of them:
//!
//! 1. **Reach.** Every fact check lived inside the zero-tool branch.
//! 2. **Extraction.** No existing extractor recognises a rustc diagnostic code.
//!    `asserted_shas` needs a run of at least seven hex characters and `E0603`
//!    is five; `asserted_tallies` knows only `passed` / `failed` / `ignored`;
//!    `claims_unbacked_evidence` needs three evidence-shaped lines; and
//!    `claims_uncalled_commands` needs a backticked command with an
//!    already-executed framing. Opening the gate alone would therefore have
//!    evaluated the incident text and found nothing to check.
//!
//! Fixtures are synthetic and carry no user identifiers. The incident fixture
//! uses only the clause the log preview attests verbatim — the full iteration
//! was 118 characters and the tail is not recoverable from the log.

use crate::brain::agent::service::phantom::{asserted_facts, mixed_iteration_facts};

/// The verbatim-attested clause of the incident text. `E0603` is the invented
/// diagnostic; the gate it names was being launched in the same response.
const INCIDENT_TEXT: &str = r#"Gate died on 2× E0603 "this item is private"."#;

/// What the turn had actually produced before that sentence.
const UNRELATED_OUTPUT: &str = "\
src/brain/agent/service/mod: 41 lines
   Compiling opencrabs v0.5.3
Finished `dev` profile [unoptimized + debuginfo] target(s) in 3m 44s
";

/// The same gate, genuinely failing. A claim against this is not a fabrication.
const REAL_E0603_OUTPUT: &str = "\
error[E0603]: module `service` is private
  --> src/brain/agent/mod.rs:1:1
   |
1  | mod service;
   | ^^^^^^^^^^^^ private module
";

// ─── extraction (#1693 gap 2) ────────────────────────────────

#[test]
fn a_rustc_diagnostic_code_is_extracted_as_an_asserted_fact() {
    let facts = asserted_facts(INCIDENT_TEXT);
    assert!(
        facts.iter().any(|f| f == "E0603"),
        "the diagnostic code was not extracted: {facts:?}"
    );
}

#[test]
fn a_diagnostic_touching_a_longer_token_is_not_extracted() {
    // Boundary guards, each a false positive that would re-open the immunity
    // the exemption exists to close: a hex run in a sha, a path segment, and a
    // five-digit code that is not a rustc shape.
    for text in [
        "parent 1a2b3E0603ff is on main",
        "see src/E0603.rs for the fixture",
        "code E06031 is not a rustc id",
        "offset 0xE0603 in the dump",
    ] {
        let facts = asserted_facts(text);
        assert!(
            !facts.iter().any(|f| f == "E0603"),
            "{text:?} extracted a diagnostic from a non-diagnostic token: {facts:?}"
        );
    }
}

#[test]
fn a_bare_and_a_bracketed_code_are_both_extracted() {
    for text in [
        "Gate died on E0603 again.",
        "cargo clippy reported error[E0425] on the call.",
    ] {
        let facts = asserted_facts(text);
        assert!(
            facts.iter().any(|f| f.starts_with('E') && f.len() == 5),
            "{text:?} yielded no diagnostic: {facts:?}"
        );
    }
}

// ─── reach (#1693 gap 1) ─────────────────────────────────────

#[test]
fn the_incident_fires_on_a_mixed_iteration() {
    let executed: Vec<String> = vec![
        r#"{"command":"cargo fmt --all"}"#.to_string(),
        r#"{"command":"arch -arm64 cargo clippy -p opencrabs --all-features"}"#.to_string(),
    ];
    let outputs: Vec<String> = vec![UNRELATED_OUTPUT.to_string()];
    let violation = mixed_iteration_facts(INCIDENT_TEXT, &executed, &outputs, UNRELATED_OUTPUT)
        .expect("the incident fabrication was not caught on a mixed iteration");
    assert!(
        violation.branches.contains(&"mixed_unbacked_facts"),
        "wrong branch fired: {:?}",
        violation.branches
    );
    assert!(
        violation.unbacked_facts.iter().any(|f| f == "E0603"),
        "the invented code is not named for the nudge: {:?}",
        violation.unbacked_facts
    );
}

#[test]
fn the_same_claim_backed_by_real_output_does_not_fire() {
    let executed: Vec<String> = vec![r#"{"command":"cargo clippy"}"#.to_string()];
    let outputs: Vec<String> = vec![REAL_E0603_OUTPUT.to_string()];
    let evidence = format!("{UNRELATED_OUTPUT}{REAL_E0603_OUTPUT}");
    assert!(
        mixed_iteration_facts(INCIDENT_TEXT, &executed, &outputs, &evidence).is_none(),
        "a diagnostic that really was printed must never be called a fabrication"
    );
}

#[test]
fn a_recap_of_the_work_its_own_call_did_is_not_flagged() {
    // The union requirement: this iteration's own call counts as executed. Scored
    // against prior inputs alone, the command check would flag the very work the
    // response is about to do.
    let in_flight = r#"{"command":"gh pr list -R opencrabs/opencrabs --state open"}"#;
    let text = "Ran `gh pr list -R opencrabs/opencrabs --state open` for the current set.";
    assert!(
        mixed_iteration_facts(text, &[in_flight.to_string()], &[], "").is_none(),
        "the in-flight call must vouch for its own command"
    );
}

#[test]
fn the_same_recap_without_the_call_in_input_is_flagged() {
    // Negative control for the test above: without the union the command really
    // is unaccounted for, so the check must fire.
    let text = "Ran `gh pr list -R opencrabs/opencrabs --state open` for the current set.";
    let prior = r#"{"command":"git status"}"#;
    let violation = mixed_iteration_facts(text, &[prior.to_string()], &[], "")
        .expect("uncalled command missed");
    assert!(
        violation.branches.contains(&"mixed_uncalled_commands"),
        "wrong branch fired: {:?}",
        violation.branches
    );
}

#[test]
fn empty_text_yields_no_violation() {
    assert!(mixed_iteration_facts("   ", &[], &[], "").is_none());
}

// ─── wiring (#1693 gap 1, structural) ────────────────────────

/// The `(start, end)` byte span of the zero-tool phantom block, brace-matched
/// from its own opening brace.
fn zero_tool_block(src: &str) -> (usize, usize) {
    let start = src
        .find("if tool_uses.is_empty() {")
        .expect("the zero-tool gate is gone — this test needs re-pointing");
    let open = start + src[start..].find('{').expect("no opening brace");
    let mut depth = 0usize;
    for (i, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return (start, open + i);
                }
            }
            _ => {}
        }
    }
    panic!("unterminated zero-tool block");
}

#[test]
fn the_mixed_check_runs_outside_the_zero_tool_block() {
    let src = include_str!("../brain/agent/service/tool_loop.rs");
    let (start, end) = zero_tool_block(src);
    // The call site is the `_with_history` entry point (#2122), which wraps the
    // plain one so a denial can also see earlier turns.
    let call = src
        .find("super::phantom::mixed_iteration_facts_with_history(")
        .expect("the mixed-iteration check is not wired into the loop");
    assert!(
        call > end,
        "the mixed check sits inside the zero-tool block ({start}..{end}), byte {call}"
    );
    // And it is guarded against the empty case, so it does not double-count
    // with the block above on a zero-tool iteration. Anchor on the binding that
    // produces the call rather than a fixed byte window: the history read
    // (#2122) sits between the guard and the call, and a magic window turns
    // every added line above the call into a fake failure.
    let binding = src[..call]
        .rfind("let mixed_fact_verdict")
        .expect("the mixed check is not bound to a verdict");
    let guard_window = &src[binding..call];
    assert!(
        guard_window.contains("tool_uses.is_empty()"),
        "the mixed check does not exclude zero-tool iterations"
    );
}

#[test]
fn the_shape_tells_stay_inside_the_zero_tool_block() {
    // After a real call, "Running fmt, then clippy" is a legitimate recap
    // (#1506, #1172). Widening these is the regression this guards.
    let src = include_str!("../brain/agent/service/tool_loop.rs");
    let (start, end) = zero_tool_block(src);
    for tell in [
        "matches_work_announcement(",
        "matches_now_gerund(",
        "matches_plan_announcement(",
    ] {
        if let Some(at) = src.find(tell) {
            assert!(
                at > start && at < end,
                "{tell} was moved outside the zero-tool block (span {start}..{end})"
            );
        }
    }
}

#[test]
fn the_correction_lands_after_the_tool_results() {
    // The nudge must follow the results, or the model is corrected before it
    // can see what it actually got — the ordering the repeat verdict relies on
    // (#1030).
    let src = include_str!("../brain/agent/service/tool_loop.rs");
    let results = src
        .find("context.add_message(tool_result_msg);")
        .expect("tool result persistence not found");
    let correction = src
        .find("if let Some(violation) = mixed_fact_verdict")
        .expect("the mixed-iteration correction is not injected");
    assert!(
        correction > results,
        "the correction is injected before the tool results reach the context"
    );
}

#[test]
fn the_correction_honours_the_structured_report_carve_out() {
    // #1506: a report with tables ships untouched. The mixed check must not
    // become a way of nagging one into a retry.
    let src = include_str!("../brain/agent/service/tool_loop.rs");
    let (_, rest) = src
        .split_once("if let Some(violation) = mixed_fact_verdict")
        .expect("correction block not found");
    let (block, _) = rest
        .split_once("context.add_message(Message::user(nudge));")
        .expect("correction block never injects its nudge");
    assert!(
        block.contains("violation.structured_report"),
        "the correction ignores the #1506 structured-report carve-out"
    );
    assert!(
        block.contains("is_structured_report") || block.contains("structured_report"),
        "no structured-report guard in the correction block"
    );
}
