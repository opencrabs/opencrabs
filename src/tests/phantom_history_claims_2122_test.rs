//! Claims about the session's own record, checked against the record (#2122).
//!
//! The generic past-tense verb list was measured before it was written: over
//! 3,798 tool turns, 609 carried a claim (16.0%), 62 were flagged (10.2%), and
//! all 18 sampled flags were legitimate answers. A verb list is not a signal, so
//! it was dropped. What survives is the shape a verb list cannot reach: a claim
//! that is decidable from the session's own history with no judgement call:
//!
//! * **A denial of something the calls show happened.** "No `sleep` command was
//!   ever in my tool calls" is settled by the call list, not by intent.
//! * **An identifier reported as produced with nothing behind it.** A number
//!   merely referenced is not a claim; a number reported as filed is.
//!
//! Measurement: `~/.opencrabs/projects/opencrabs/research/2122-measurement.md`.

use crate::brain::agent::service::phantom::{
    asserted_history_denials, asserted_produced_identifiers, denials_contradicted,
    ledger_tool_inputs, mixed_iteration_facts, mixed_iteration_facts_with_history,
};

/// The verbatim denial from 2026-10-18, when thirty `sleep` calls in the log
/// were met with a flat denial that any had been made.
const DENIAL_TEXT: &str = "No `sleep` command was ever in my tool calls.";

fn bash_input(command: &str) -> String {
    format!(r#"{{"command":"{command}"}}"#)
}

// ─── denial extraction ───────────────────────────────────────

#[test]
fn a_denied_command_is_extracted() {
    let denials = asserted_history_denials(DENIAL_TEXT);
    assert_eq!(denials, vec!["sleep".to_string()], "{denials:?}");
}

#[test]
fn the_never_ran_form_is_extracted() {
    for text in [
        "I never ran `cargo clippy`.",
        "I didn't run cargo clippy.",
        "I did not call `cargo clippy`.",
        "I never invoked `cargo clippy`.",
    ] {
        let denials = asserted_history_denials(text);
        assert!(
            denials.iter().any(|d| d == "cargo"),
            "{text} produced {denials:?}"
        );
    }
}

#[test]
fn a_pronoun_names_nothing_checkable() {
    // "I never ran it" cannot be checked against a call list, and guessing what
    // "it" refers to is exactly the false-positive class that killed the verb
    // list.
    for text in [
        "I never ran it.",
        "I didn't call that.",
        "I never executed anything.",
    ] {
        let denials = asserted_history_denials(text);
        assert!(denials.is_empty(), "{text} produced {denials:?}");
    }
}

#[test]
fn ordinary_prose_yields_no_denial() {
    for text in [
        "No issues found in the gate.",
        "There were no test failures.",
        "The clippy run was clean.",
    ] {
        let denials = asserted_history_denials(text);
        assert!(denials.is_empty(), "{text} produced {denials:?}");
    }
}

// ─── denial vs history ───────────────────────────────────────

#[test]
fn a_denial_the_calls_contradict_is_flagged() {
    let denials = asserted_history_denials(DENIAL_TEXT);
    let executed = vec![bash_input("sleep 30")];
    assert_eq!(
        denials_contradicted(&denials, &executed),
        vec!["sleep".to_string()]
    );
}

#[test]
fn a_wrapped_denial_still_counts() {
    // `timeout 30 sleep 5` sleeps. The wrapper is not a different command.
    let denials = asserted_history_denials(DENIAL_TEXT);
    for command in ["timeout 30 sleep 5", "sudo sleep 5", "cd /tmp && sleep 5"] {
        let executed = vec![bash_input(command)];
        assert_eq!(
            denials_contradicted(&denials, &executed),
            vec!["sleep".to_string()],
            "{command} was not read as a sleep"
        );
    }
}

#[test]
fn a_mere_mention_does_not_contradict() {
    // Searching for the word is not running it. A substring match would clear
    // the very lie this check exists to catch.
    let denials = asserted_history_denials(DENIAL_TEXT);
    for command in [
        "grep -rn \"sleep\" src/",
        "rg 'sleep' ~/.opencrabs/logs",
        "echo sleep",
    ] {
        let executed = vec![bash_input(command)];
        assert!(
            denials_contradicted(&denials, &executed).is_empty(),
            "{command} was miscounted as a sleep"
        );
    }
}

#[test]
fn a_true_denial_is_not_flagged() {
    let denials = asserted_history_denials(DENIAL_TEXT);
    let executed = vec![bash_input("cargo clippy --all-features")];
    assert!(denials_contradicted(&denials, &executed).is_empty());
}

// ─── identifier claims ───────────────────────────────────────

#[test]
fn a_referenced_number_is_not_a_claim() {
    // Answering "check #2098" must not be punished for quoting the user.
    for text in [
        "Check #2098 for the details.",
        "That is #2122.",
        "See #1693.",
    ] {
        let ids = asserted_produced_identifiers(text);
        assert!(ids.is_empty(), "{text} produced {ids:?}");
    }
}

#[test]
fn a_reported_filing_is_a_claim() {
    let ids = asserted_produced_identifiers("Filed #2122 and closed #2123.");
    assert_eq!(ids, vec!["#2122".to_string(), "#2123".to_string()]);
}

#[test]
fn a_filing_backed_by_a_url_is_not_flagged() {
    let text = "Filed #2122 with the measurement.";
    let evidence = "https://github.com/opencrabs/opencrabs/issues/2122";
    let verdict = mixed_iteration_facts(text, &[], &[], evidence);
    assert!(
        verdict.is_none(),
        "a backed filing was flagged: {verdict:?}"
    );
}

#[test]
fn a_filing_with_nothing_behind_it_is_flagged() {
    let text = "Filed #9999 for that.";
    let verdict = mixed_iteration_facts(text, &[], &[], "no tool output at all")
        .expect("an unbacked filing was not flagged");
    assert!(
        verdict.branches.contains(&"mixed_unbacked_actions"),
        "wrong branch: {:?}",
        verdict.branches
    );
    assert!(
        verdict.unbacked_actions.iter().any(|a| a.contains("#9999")),
        "the identifier was not named: {:?}",
        verdict.unbacked_actions
    );
}

#[test]
fn a_non_ascii_tail_does_not_panic_the_verb_window() {
    // The window is measured in bytes, so on accented text its end can land
    // inside a character. Slicing there is the #2124 panic class.
    let text = format!("Filed #2122 {}", "é".repeat(40));
    let ids = asserted_produced_identifiers(&text);
    assert_eq!(ids, vec!["#2122".to_string()]);
}

// ─── session scope ───────────────────────────────────────────

#[test]
fn a_recorded_call_is_recovered_from_the_ledger() {
    // The session's own rows are the only place a past turn's command survives.
    let row = "Done.\n<!-- tools-v2: [{\"d\":\"bash: sleep 30\",\"s\":true,\"i\":{\"command\":\"sleep 30\"}}] -->\n";
    let inputs = ledger_tool_inputs(&[row.to_string()]);
    assert_eq!(inputs.len(), 1, "{inputs:?}");
    assert!(inputs[0].contains("sleep 30"), "{inputs:?}");
}

#[test]
fn an_older_row_without_the_input_field_still_yields_the_command() {
    // Rows written before `i` existed carry only the description, and the
    // tool-name prefix has to come off or the first word is `bash:`.
    let row = "\n<!-- tools-v2: [{\"d\":\"bash: sleep 30\",\"s\":true}] -->\n";
    let inputs = ledger_tool_inputs(&[row.to_string()]);
    assert_eq!(inputs, vec!["sleep 30".to_string()]);
}

#[test]
fn a_ledger_quoted_in_prose_is_not_history() {
    // The model sometimes quotes a ledger opener mid-sentence (#1587). Reading
    // that as a recorded call would invent history out of prose.
    let row =
        "The row read: <!-- tools-v2: [{\"i\":{\"command\":\"sleep 30\"}}] --> and that is all.";
    let inputs = ledger_tool_inputs(&[row.to_string()]);
    assert!(inputs.is_empty(), "{inputs:?}");
}

#[test]
fn an_earlier_turn_contradicts_the_denial() {
    // The 2026-10-18 shape: the lie was about earlier turns, so turn scope
    // alone would have cleared it.
    let history = vec![bash_input("sleep 30")];
    let verdict = mixed_iteration_facts_with_history(DENIAL_TEXT, &[], &[], "", &history)
        .expect("a denial contradicted by earlier turns was not flagged");
    assert!(
        verdict.unbacked_actions.iter().any(|a| a == "sleep"),
        "{:?}",
        verdict.unbacked_actions
    );
}

#[test]
fn earlier_history_never_clears_a_this_turn_claim() {
    // The two questions are separate on purpose: a call from an hour ago must
    // not vouch for a command the text says ran in this turn.
    let text = "Ran `gh pr view 2098` and it was already merged.";
    let history = vec![bash_input("gh pr view 2098")];
    let verdict = mixed_iteration_facts_with_history(text, &[], &[], "", &history)
        .expect("the this-turn claim was cleared by old history");
    assert!(
        verdict.branches.contains(&"mixed_uncalled_commands"),
        "{:?}",
        verdict.branches
    );
}

// ─── end to end ──────────────────────────────────────────────

#[test]
fn the_denial_reaches_the_violation() {
    let executed = vec![bash_input("sleep 3111")];
    let verdict = mixed_iteration_facts(DENIAL_TEXT, &executed, &[], "")
        .expect("the contradicted denial was not flagged");
    assert!(
        verdict.branches.contains(&"mixed_unbacked_actions"),
        "wrong branch: {:?}",
        verdict.branches
    );
    assert!(
        verdict.unbacked_actions.iter().any(|a| a == "sleep"),
        "the denied command was not named: {:?}",
        verdict.unbacked_actions
    );
}

#[test]
fn an_honest_turn_stays_clean() {
    let text = "Clippy came back clean. No `sleep` command was in that run.";
    let executed = vec![bash_input("cargo clippy --all-features")];
    let outputs = vec!["Finished `dev` profile".to_string()];
    assert!(
        mixed_iteration_facts(text, &executed, &outputs, "Finished `dev` profile").is_none(),
        "an honest answer was flagged"
    );
}

#[test]
fn a_kill_claim_without_a_named_command_is_not_flagged() {
    // The accepted trade from the measurement: "I killed the gate" names no
    // command and no identifier, so nothing about it is mechanically decidable.
    // The verb list that tried to catch it flagged 10.2% of claim-bearing turns
    // with a 100% legitimate sample, so it stays out.
    let text = "I killed the gate.";
    let executed = vec![bash_input("grep -n foo src/bar.rs")];
    let outputs = vec!["foo:12".to_string()];
    assert!(
        mixed_iteration_facts(text, &executed, &outputs, "foo:12").is_none(),
        "a claim with nothing checkable in it was flagged"
    );
}

// ─── wiring ──────────────────────────────────────────────────

#[test]
fn the_nudge_picker_never_falls_through_to_no_tool_calls() {
    // The fallthrough told a turn that DID call tools that it made none. The new
    // branch has to sit before it, or a mixed iteration with only this violation
    // gets the wrong correction.
    let src = include_str!("../brain/agent/service/tool_loop.rs");
    let (_, block) = src
        .split_once("if let Some(violation) = mixed_fact_verdict")
        .expect("the mixed-iteration correction is gone");
    let (block, _) = block
        .split_once("context.add_message(Message::user(nudge));")
        .expect("the correction block never injects its nudge");
    let actions = block
        .find("unbacked_actions_nudge(")
        .expect("the actions nudge is never selected in the mixed correction");
    let fallthrough = block
        .find("super::nudge::no_tool_calls_nudge(is_local_provider)")
        .expect("the no-tool-calls fallthrough is gone from the mixed correction");
    assert!(
        actions < fallthrough,
        "the actions nudge sits after the no-tool-calls fallthrough"
    );
}
