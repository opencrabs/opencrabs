//! The Ralph loop verification gate (#852).
//!
//! The gate exists because a model can report a task as `success` without the
//! work having passed, and from inside the turn the claim and the truth look
//! identical. It replaces self-report with a shell exit code.
//!
//! None of it was testable: every path ran `sh -c`. `verify_with` now takes the
//! runner as a seam, so the verdict logic can be exercised directly, and
//! `commands_for_type` is pure over the config section.
//!
//! Fixtures are synthetic and carry no user identifiers.

use crate::brain::tools::plan_tool::{
    CriteriaPolicy, CriteriaVerdict, RalphVerification, TaskTypeCommands, VerificationOutcome,
    commands_for_type, criteria_verdict, run_verification_command, truncate_output, verify_with,
};

fn cmds(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A runner that answers from a table and records what it was asked to run.
fn fake<'a>(
    table: Vec<(&'static str, i32, &'static str)>,
    log: &'a mut Vec<String>,
) -> impl FnMut(&str) -> (i32, String) + 'a {
    move |cmd: &str| {
        log.push(cmd.to_string());
        table
            .iter()
            .find(|(c, _, _)| *c == cmd)
            .map(|(_, code, out)| (*code, out.to_string()))
            .unwrap_or((0, String::new()))
    }
}

// ── The verdict ─────────────────────────────────────────────────────────────

#[test]
fn all_commands_passing_accepts_the_task() {
    let mut log = Vec::new();
    let mut run = fake(vec![("cargo test", 0, "ok")], &mut log);
    assert!(verify_with("build", 1, &cmds(&["cargo test"]), true, &mut run).is_ok());
}

#[test]
fn a_nonzero_exit_rejects_the_task() {
    // The whole point: the model cannot self-report success past this.
    let mut log = Vec::new();
    let mut run = fake(
        vec![("cargo clippy", 101, "error: mismatched types")],
        &mut log,
    );
    let err = verify_with("build", 3, &cmds(&["cargo clippy"]), true, &mut run)
        .expect_err("a failing command must reject");
    assert!(err.contains("REJECTED"), "{err}");
}

#[test]
fn the_rejection_names_the_command_the_code_and_the_task() {
    // A bare "rejected" leaves the model guessing, which is how a gate turns
    // into a retry loop.
    let mut log = Vec::new();
    let mut run = fake(
        vec![("cargo clippy", 101, "error: mismatched types")],
        &mut log,
    );
    let err = verify_with("build", 7, &cmds(&["cargo clippy"]), true, &mut run).unwrap_err();
    assert!(err.contains("cargo clippy"), "missing command: {err}");
    assert!(err.contains("101"), "missing exit code: {err}");
    assert!(err.contains("#7"), "missing task order: {err}");
    assert!(err.contains("mismatched types"), "missing output: {err}");
}

#[test]
fn no_commands_means_nothing_to_verify() {
    let mut log = Vec::new();
    let mut run = fake(vec![], &mut log);
    assert!(verify_with("docs", 1, &[], true, &mut run).is_ok());
}

// ── require_all_pass ────────────────────────────────────────────────────────

#[test]
fn require_all_runs_every_command_and_reports_all_failures() {
    let mut log = Vec::new();
    {
        let mut run = fake(
            vec![("a", 1, "a failed"), ("b", 0, ""), ("c", 2, "c failed")],
            &mut log,
        );
        let err = verify_with("build", 1, &cmds(&["a", "b", "c"]), true, &mut run).unwrap_err();
        assert!(err.contains("a failed"), "first failure missing: {err}");
        assert!(err.contains("c failed"), "later failure missing: {err}");
    }
    assert_eq!(
        log,
        vec!["a", "b", "c"],
        "require_all must not short-circuit"
    );
}

#[test]
fn without_require_all_the_first_failure_short_circuits() {
    // Contract, not optimisation: a cheap check placed first is meant to spare
    // an expensive one after it.
    let mut log = Vec::new();
    {
        let mut run = fake(vec![("fast", 1, "fast failed"), ("slow", 0, "")], &mut log);
        assert!(verify_with("build", 1, &cmds(&["fast", "slow"]), false, &mut run).is_err());
    }
    assert_eq!(log, vec!["fast"], "the remaining commands must not run");
}

#[test]
fn without_require_all_a_pass_still_continues_to_the_next() {
    // Short-circuit is on failure only. Stopping on the first success would
    // let a later failing command through.
    let mut log = Vec::new();
    {
        let mut run = fake(vec![("first", 0, ""), ("second", 1, "boom")], &mut log);
        assert!(verify_with("build", 1, &cmds(&["first", "second"]), false, &mut run).is_err());
    }
    assert_eq!(log, vec!["first", "second"]);
}

#[test]
fn a_runner_error_is_a_failure_not_a_pass() {
    // run_verification_command yields -1 when the process cannot start. That
    // must reject, never sail through as if verified.
    let mut log = Vec::new();
    let mut run = fake(vec![("missing-binary", -1, "Failed to execute")], &mut log);
    assert!(verify_with("build", 1, &cmds(&["missing-binary"]), true, &mut run).is_err());
}

// ── Task-type mapping ───────────────────────────────────────────────────────

fn verification(enabled: bool, pairs: &[(&str, &[&str])]) -> RalphVerification {
    RalphVerification {
        enabled,
        require_all_pass: true,
        // Added by #870 after these tests were written. Default keeps them
        // testing the command-mapping behaviour they were written for.
        criteria_policy: Default::default(),
        // Added by #1904: the per-task spec check policy. These tests pin the
        // per-type command mapping, which the spec-check path does not touch.
        spec_checks: Default::default(),
        // Added by #2063: model-written checks. Off here, so these tests keep
        // pinning the command mapping and never ask a model for anything.
        spec_check_generation: false,
        task_type_commands: pairs
            .iter()
            .map(|(t, c)| TaskTypeCommands {
                task_type: t.to_string(),
                commands: cmds(c),
            })
            .collect(),
    }
}

#[test]
fn the_matching_task_type_selects_its_commands() {
    let v = verification(
        true,
        &[("build", &["cargo clippy"]), ("test", &["cargo test"])],
    );
    assert_eq!(
        commands_for_type(&v, "build"),
        Some(cmds(&["cargo clippy"]))
    );
    assert_eq!(commands_for_type(&v, "test"), Some(cmds(&["cargo test"])));
}

#[test]
fn task_type_matching_is_case_insensitive() {
    let v = verification(true, &[("build", &["cargo clippy"])]);
    assert_eq!(
        commands_for_type(&v, "BUILD"),
        Some(cmds(&["cargo clippy"]))
    );
}

#[test]
fn an_unmapped_task_type_verifies_nothing() {
    let v = verification(true, &[("build", &["cargo clippy"])]);
    assert_eq!(commands_for_type(&v, "docs"), None);
}

#[test]
fn an_empty_command_list_verifies_nothing() {
    // Otherwise an empty list would report a vacuous pass, which reads as
    // verified when nothing ran.
    let v = verification(true, &[("build", &[])]);
    assert_eq!(commands_for_type(&v, "build"), None);
}

#[test]
fn disabling_verification_turns_the_gate_off_entirely() {
    // Documents the failure mode rather than endorsing it: with the gate off
    // every task passes unverified, and a TOML typo produces this same state
    // silently.
    let v = verification(false, &[("build", &["cargo clippy"])]);
    assert_eq!(commands_for_type(&v, "build"), None);
}

// ── Output truncation ───────────────────────────────────────────────────────

#[test]
fn short_output_is_untouched() {
    assert_eq!(truncate_output("brief", 500), "brief");
}

#[test]
fn long_output_is_truncated_and_marked() {
    let long = "x".repeat(600);
    let out = truncate_output(&long, 500);
    assert!(out.ends_with("...[truncated]"));
    assert!(out.len() < long.len() + 20);
}

#[test]
fn truncation_does_not_split_a_multibyte_character() {
    // The original sliced `&output[..500]`, which panics when byte 500 lands
    // inside a character. Compiler output makes that likely: rustc emits `─`
    // and smart quotes freely, so the gate would panic on the very failure it
    // exists to report.
    for filler in ["─", "é", "🦀", "…"] {
        let noisy = filler.repeat(400);
        let out = truncate_output(&noisy, 500);
        assert!(
            out.ends_with("...[truncated]"),
            "truncation failed for {filler:?}"
        );
    }
}

#[test]
fn truncating_at_an_exact_boundary_is_stable() {
    let exact = "y".repeat(500);
    assert_eq!(
        truncate_output(&exact, 500),
        exact,
        "no marker when it fits"
    );
}

// ── Criteria-aware policy (#870) ────────────────────────────────────────────
//
// Acceptance criteria used to be decorative: the gate keyed only on task_type,
// so a task could declare criteria and complete "success" with nothing checking
// them. `criteria_verdict` is the pure decision the Complete handler applies.

#[test]
fn strict_rejects_an_unverified_claim_against_criteria() {
    // The teeth: criteria declared, no commands ran for the type → refuse.
    assert_eq!(
        criteria_verdict(CriteriaPolicy::Strict, VerificationOutcome::NotConfigured),
        CriteriaVerdict::Reject
    );
}

#[test]
fn downgrade_accepts_but_flags_an_unverified_claim() {
    // The default: honest, not blocking — the belief lands as Uncertain.
    assert_eq!(
        criteria_verdict(
            CriteriaPolicy::Downgrade,
            VerificationOutcome::NotConfigured
        ),
        CriteriaVerdict::Downgrade
    );
}

#[test]
fn off_keeps_the_pre_870_behaviour() {
    assert_eq!(
        criteria_verdict(CriteriaPolicy::Off, VerificationOutcome::NotConfigured),
        CriteriaVerdict::Accept
    );
}

#[test]
fn a_proven_completion_accepts_under_every_policy() {
    // Commands ran and passed — the claim is Verified regardless of policy.
    for policy in [
        CriteriaPolicy::Downgrade,
        CriteriaPolicy::Strict,
        CriteriaPolicy::Off,
    ] {
        assert_eq!(
            criteria_verdict(policy, VerificationOutcome::Verified),
            CriteriaVerdict::Accept,
            "policy {policy:?} must accept a proven completion"
        );
    }
}

#[test]
fn a_disabled_gate_accepts_even_under_strict() {
    // Global gate-off is an explicit user choice; strict must not override it.
    assert_eq!(
        criteria_verdict(CriteriaPolicy::Strict, VerificationOutcome::Disabled),
        CriteriaVerdict::Accept
    );
}

#[test]
fn no_criteria_is_not_proof_either() {
    // Silence is not proof (maintainer ruling, 2026-08-17): a criteria-less
    // completion with nothing verified inherits the same verdict as a
    // declared one. Declaring criteria buys nothing; omitting them dodges
    // nothing.
    assert_eq!(
        criteria_verdict(
            CriteriaPolicy::Downgrade,
            VerificationOutcome::NotConfigured
        ),
        CriteriaVerdict::Downgrade
    );
    assert_eq!(
        criteria_verdict(CriteriaPolicy::Strict, VerificationOutcome::NotConfigured),
        CriteriaVerdict::Reject
    );
}

#[test]
fn a_criteria_less_completion_still_accepts_when_proven_or_disabled() {
    // The escape hatches stay open: proof (commands ran and passed) or an
    // explicit gate-off accepts regardless of criteria.
    for outcome in [VerificationOutcome::Verified, VerificationOutcome::Disabled] {
        assert_eq!(
            criteria_verdict(CriteriaPolicy::Strict, outcome),
            CriteriaVerdict::Accept
        );
    }
}

// ── criteria_policy deserialization ─────────────────────────────────────────

#[test]
fn criteria_policy_defaults_to_downgrade_when_absent() {
    // Non-destructive default: an existing config with no criteria_policy must
    // not suddenly start rejecting completions.
    let v: RalphVerification = toml::from_str("enabled = true").expect("valid TOML");
    assert_eq!(v.criteria_policy, CriteriaPolicy::Downgrade);
}

#[test]
fn criteria_policy_parses_each_variant_lowercase() {
    let strict: RalphVerification =
        toml::from_str("enabled = true\ncriteria_policy = \"strict\"").expect("valid TOML");
    assert_eq!(strict.criteria_policy, CriteriaPolicy::Strict);

    let off: RalphVerification =
        toml::from_str("enabled = true\ncriteria_policy = \"off\"").expect("valid TOML");
    assert_eq!(off.criteria_policy, CriteriaPolicy::Off);

    let downgrade: RalphVerification =
        toml::from_str("enabled = true\ncriteria_policy = \"downgrade\"").expect("valid TOML");
    assert_eq!(downgrade.criteria_policy, CriteriaPolicy::Downgrade);
}

// ── Working directory (#921) ──────────────────────────────────────────

/// The gate shelled out with no `current_dir`, so it inherited the OpenCrabs
/// process cwd and verified whichever repo the binary happened to be launched
/// from. A plan in one repo was gated on another repo's build.
#[test]
fn verification_runs_in_the_directory_it_is_given() {
    // Unique per process. A fixed name under the shared temp dir is one
    // mutable global: two concurrent `cargo test` runs execute this test
    // against the SAME directory, and whichever finishes first deletes it out
    // from under the other. That made this fail intermittently in a full
    // suite while passing in isolation.
    let dir = std::env::temp_dir().join(format!("opencrabs_ralph_cwd_test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    // macOS reports /var/... as /private/var/..., so compare resolved paths.
    let expected = dir.canonicalize().expect("canonicalize temp dir");

    let (code, output) = run_verification_command("pwd", &expected);

    assert_eq!(code, 0, "pwd should succeed: {output}");
    assert_eq!(
        output.trim(),
        expected.to_string_lossy(),
        "command must run in the directory passed in, not the process cwd"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The directory is threaded through, not silently replaced by the process cwd
/// when the two differ.
#[test]
fn verification_directory_is_not_the_process_cwd() {
    let dir = std::env::temp_dir().join(format!(
        "opencrabs_ralph_cwd_differs-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let expected = dir.canonicalize().expect("canonicalize temp dir");
    let process_cwd = std::env::current_dir().expect("process cwd");

    assert_ne!(
        expected, process_cwd,
        "fixture is meaningless if the temp dir IS the process cwd"
    );

    let (_, output) = run_verification_command("pwd", &expected);
    assert_ne!(
        output.trim(),
        process_cwd.to_string_lossy(),
        "the gate must not fall back to the launch directory"
    );

    std::fs::remove_dir_all(&dir).ok();
}
