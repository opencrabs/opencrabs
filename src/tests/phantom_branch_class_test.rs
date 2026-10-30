//! Branch-class gate for structured reports (#2039).
//!
//! A structured report may exempt the phrase-matching detectors, which
//! false-positive on distilled report prose. It may never exempt a fact check:
//! a fired fact check means the report states something the conversation
//! cannot back, and a table does not make an invented sha true.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::brain::agent::service::phantom::{
    asserted_facts, is_fact_check_branch, is_structured_report, option_halt_exempts,
    structured_report_exempts, unbacked_facts,
};

const PHRASE_BRANCHES: &[&str] = &[
    "intent_no_tools",
    "intent_full_text",
    "tool_name_narrated",
    "shell_fence_narrated",
    "unbacked_side_effects",
    "bare_completion_delivery",
    "media_claim_no_marker",
];

const FACT_BRANCHES: &[&str] = &["unbacked_facts", "uncalled_commands", "unbacked_evidence"];

#[test]
fn the_three_evidence_checks_are_fact_check_branches() {
    for branch in FACT_BRANCHES {
        assert!(
            is_fact_check_branch(branch),
            "{branch} must be a fact check"
        );
    }
}

#[test]
fn the_phrase_matchers_are_not_fact_check_branches() {
    for branch in PHRASE_BRANCHES {
        assert!(
            !is_fact_check_branch(branch),
            "{branch} must stay exemptable"
        );
    }
}

#[test]
fn a_structured_report_exempts_phrase_matchers_alone() {
    for branch in PHRASE_BRANCHES {
        assert!(
            structured_report_exempts(true, &[*branch]),
            "a structured report must exempt {branch}"
        );
    }
    assert!(structured_report_exempts(
        true,
        &["intent_no_tools", "shell_fence_narrated"]
    ));
}

#[test]
fn a_structured_report_never_exempts_a_fired_fact_check() {
    for branch in FACT_BRANCHES {
        assert!(
            !structured_report_exempts(true, &[*branch]),
            "a fired {branch} must kill a structured report"
        );
    }
    assert!(!structured_report_exempts(
        true,
        &["intent_no_tools", "uncalled_commands"]
    ));
}

#[test]
fn prose_is_never_exempted_by_the_structured_rule() {
    assert!(!structured_report_exempts(
        false,
        &["unbacked_side_effects"]
    ));
    assert!(!structured_report_exempts(false, &["unbacked_facts"]));
}

#[test]
fn a_table_report_with_invented_shas_fails_the_fact_check() {
    // The #1423 shape: a structured report whose shas exist nowhere in the
    // conversation. The fact check must fire, and the table must not exempt it.
    let report =
        "Done.\n\n| Commit | Result |\n|---|---|\n| 9f3c2a1e | merged |\n| 41b29f23 | merged |\n";
    assert!(is_structured_report(report));

    let facts = asserted_facts(report);
    let unbacked = unbacked_facts(&facts, "user: please check the build");
    assert!(!unbacked.is_empty(), "invented shas must be unbacked");
    assert!(!structured_report_exempts(true, &["unbacked_facts"]));
}

#[test]
fn a_table_report_with_backed_facts_keeps_the_exemption() {
    // Same shape, but the shas appear in the conversation, so no fact check
    // fires. The phrase matcher that tripped is exempt, as #1506 intends.
    let report = "Done.\n\n| Commit | Result |\n|---|---|\n| 9f3c2a1e | merged |\n";
    assert!(is_structured_report(report));

    let facts = asserted_facts(report);
    let unbacked = unbacked_facts(&facts, "git log showed 9f3c2a1e merged");
    assert!(unbacked.is_empty(), "backed shas must not be unbacked");
    assert!(structured_report_exempts(true, &["unbacked_side_effects"]));
}

/// #31 sign-off gate. A text-only iteration that follows an option-surface
/// halt is the model closing a turn it already handed back with options
/// rendered. The phrase matchers read that prose as a promise and the retry
/// they bought posted a second answer to a channel whose first answer had
/// already landed (2026-10-17 Discord, `intent_no_tools` on a delivered
/// research report). Fact checks still kill it, so a sign-off cannot smuggle
/// an invented sha past the gate.
#[test]
fn an_option_halt_signoff_exempts_phrase_matchers_alone() {
    for branch in PHRASE_BRANCHES {
        assert!(
            option_halt_exempts(&[*branch]),
            "a sign-off after an option halt must exempt {branch}"
        );
    }
    for branch in FACT_BRANCHES {
        assert!(
            !option_halt_exempts(&[*branch]),
            "a fired fact check must still kill the sign-off ({branch})"
        );
    }
    assert!(option_halt_exempts(&[
        "intent_no_tools",
        "intent_full_text"
    ]));
    assert!(!option_halt_exempts(&["intent_no_tools", "unbacked_facts"]));
}

/// The gate has to be wired where the kill is decided, or the exemption is a
/// function nobody calls.
#[test]
fn the_kill_site_consults_the_option_halt_gate() {
    let source = include_str!("../brain/agent/service/tool_loop.rs");
    let gate = source
        .find("option_halt_exempts(&fired_branches)")
        .expect("the phantom kill site must consult the option-halt gate");
    let kill = source
        .find("Phantom tool call detected")
        .expect("the kill WARN is present");
    assert!(
        gate < kill,
        "the option-halt gate must be evaluated before the retry is injected"
    );
}
