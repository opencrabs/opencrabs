//! Tests for tool-failure classification (#236, #2251).
//!
//! Recoverable/environmental/user-driven failures must be classified so the
//! ledger keeps them out of the success-rate denominator; genuine defects must
//! still count. Calls the HARNESS refused before execution (plan gate, skill
//! gate, guest scope, a declined approval prompt) get their own class and their
//! own `gate_denied` event: the tool never ran, so it cannot be at fault.

use crate::brain::feedback_policy::{
    FailureClass, classify_tool_failure, is_recoverable_tool_failure, tool_event_type,
};

#[test]
fn hashline_stale_hash_is_recoverable() {
    assert!(is_recoverable_tool_failure(
        "hashline_edit",
        Some("Hash #EJ not found in file. The file may have changed since your last read.")
    ));
    assert!(is_recoverable_tool_failure(
        "hashline_edit",
        Some("Hash collision on #NH across 9 lines")
    ));
}

#[test]
fn channel_send_not_connected_is_recoverable() {
    assert!(is_recoverable_tool_failure(
        "telegram_send",
        Some("Telegram is not connected. Ask the user to connect Telegram first.")
    ));
    assert!(is_recoverable_tool_failure(
        "whatsapp_send",
        Some("not connected")
    ));
    assert!(is_recoverable_tool_failure(
        "discord_connect",
        Some("failed to connect to gateway")
    ));
}

#[test]
fn genuine_defects_are_not_recoverable() {
    // A real bug in a tool's own logic must still count as a failure.
    assert!(!is_recoverable_tool_failure(
        "hashline_edit",
        Some("internal panic: index out of bounds")
    ));
    // bash failing a command is a real outcome, not an exempt class.
    assert!(!is_recoverable_tool_failure(
        "bash",
        Some("command not found: frobnicate")
    ));
    // telegram_send with an unknown-action misuse is a real (learnable) error,
    // not the environmental "not connected" class.
    assert!(!is_recoverable_tool_failure(
        "telegram_send",
        Some("unknown action 'list_channels'")
    ));
}

#[test]
fn environmental_bash_failures_are_recoverable() {
    // #1068: bash had no arm at all, so a well-formed command that hit a
    // missing file or a dead service counted against the tool exactly like a
    // command the model got wrong. That made `tool_failure|bash` the ledger's
    // loudest signal and pointed RSI at "bash is broken".
    assert!(is_recoverable_tool_failure(
        "bash",
        Some(
            "Command exited with code 1\n\n-- output captured before error --\n\
             STDERR:\ncat: /etc/nope.conf: No such file or directory\n"
        )
    ));
    assert!(is_recoverable_tool_failure(
        "bash",
        Some(
            "Command exited with code 7\n\n-- output captured before error --\n\
             STDERR:\ncurl: (7) Failed to connect to localhost port 8931: Connection refused\n"
        )
    ));
}

#[test]
fn bash_commands_the_model_got_wrong_stay_failures() {
    // The other half of the split, and the reason the classifier checks model
    // markers first: these are agent behaviour RSI is supposed to act on.
    for stderr in [
        "bash: line 3: syntax error near unexpected token `fi'",
        "bash: line 1: MY_VAR: unbound variable",
        "ls: illegal option -- Z",
    ] {
        assert!(
            !is_recoverable_tool_failure(
                "bash",
                Some(&format!(
                    "Command exited with code 2\n\n-- output captured before error --\n\
                     STDERR:\n{stderr}\n"
                ))
            ),
            "must stay a failure: {stderr}"
        );
    }
}

#[test]
fn missing_error_text_is_not_recoverable() {
    // No error string → cannot be classified benign; must count normally.
    assert!(!is_recoverable_tool_failure("hashline_edit", None));
    assert_eq!(
        classify_tool_failure("hashline_edit", None),
        FailureClass::Defect
    );
}

#[test]
fn unrelated_tools_are_never_auto_recoverable() {
    // A tool not in the policy list must never be silently exempted, even if
    // its error happens to contain a keyword.
    assert!(!is_recoverable_tool_failure(
        "read_file",
        Some("file not found")
    ));
}

// ── Harness refusals (#2251) ────────────────────────────────────────────────

/// The real gate, not a hand-copied string: 9 calls into a turn whose threshold
/// is 3 and whose session has no plan. This is the exact denial that put five
/// `tool_failure|telegram_send` rows in the ledger.
#[tokio::test]
async fn plan_requirement_denial_is_a_gate_refusal() {
    use crate::brain::tools::plan_gate::{GateDecision, check_plan_requirement};
    use crate::config::profile::with_home_override_async;
    use uuid::Uuid;

    // Plan state is read from the OpenCrabs home, so keep the live one out of
    // this test (#1399).
    let dir = tempfile::TempDir::new().unwrap();
    let reason = with_home_override_async(dir.path().to_path_buf(), async {
        match check_plan_requirement(Uuid::new_v4(), "telegram_send", 9, 3, false).await {
            GateDecision::Deny(reason) => reason,
            other => panic!("expected a denial, got {other:?}"),
        }
    })
    .await;

    assert!(
        reason.starts_with("Plan gate:"),
        "the classifier keys on this prefix: {reason}"
    );
    assert_eq!(
        classify_tool_failure("telegram_send", Some(&reason)),
        FailureClass::GateRefusal
    );
    assert_eq!(
        tool_event_type(false, "telegram_send", Some(&reason)),
        "gate_denied"
    );
    // A refusal is not "recoverable": it is its own thing, so the two never get
    // conflated in the ledger breakdown.
    assert!(!is_recoverable_tool_failure("telegram_send", Some(&reason)));
}

#[tokio::test]
async fn guest_scope_refusal_is_a_gate_refusal() {
    let reason = crate::brain::tools::scope::guest_refusal("bash");
    assert_eq!(
        classify_tool_failure("bash", Some(&reason)),
        FailureClass::GateRefusal
    );
    assert_eq!(tool_event_type(false, "bash", Some(&reason)), "gate_denied");
}

#[test]
fn declined_approval_and_skill_gate_are_gate_refusals() {
    // Sentinels authored by the harness, not by the tool:
    //   tool_loop.rs records `user_denied_approval` when the prompt is declined
    //   registry.rs returns the `[SKILL GATE]` block instead of running the call
    for (tool, reason) in [
        ("bash", "user_denied_approval"),
        (
            "write_file",
            "[SKILL GATE] This call touches '/srv/x/y.md' which matches skill 'y'",
        ),
        (
            "edit_file",
            "Plan gate: 'edit_file' is blocked until the approved plan's ...",
        ),
    ] {
        assert_eq!(
            classify_tool_failure(tool, Some(reason)),
            FailureClass::GateRefusal,
            "must read as a gate refusal: {reason}"
        );
        assert_eq!(tool_event_type(false, tool, Some(reason)), "gate_denied");
    }
}

#[test]
fn event_type_names_the_outcome() {
    assert_eq!(tool_event_type(true, "bash", None), "tool_success");
    assert_eq!(
        tool_event_type(
            false,
            "telegram_send",
            Some("unknown action 'list_channels'")
        ),
        "tool_failure"
    );
    assert_eq!(
        tool_event_type(false, "telegram_send", Some("Telegram is not connected.")),
        "recoverable_failure"
    );
    // A network error is not a refusal: Telegram answered neither way, the tool
    // really did try, and the model cannot fix that by planning. Stays a defect.
    assert_eq!(
        tool_event_type(
            false,
            "telegram_send",
            Some("Failed to send photo: A network error: error sending request")
        ),
        "tool_failure"
    );
}

/// The point of the new event: `stats_by_dimension("tool_")` aggregates on the
/// `tool_` prefix, so a `gate_denied` row must not appear in the denominator.
#[tokio::test]
async fn gate_denials_stay_out_of_the_tool_denominator() {
    use crate::db::Database;
    use crate::db::repository::FeedbackLedgerRepository;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = FeedbackLedgerRepository::new(db.pool().clone());

    // A turn that was blocked once and then sent successfully would read 50%
    // under the old recording, which is exactly the health warning that started
    // #2251. Now the refusal is invisible to the rate.
    let denial = "Plan gate: 9 tool calls in this turn and no plan.";
    repo.record(
        "s1",
        tool_event_type(false, "telegram_send", Some(denial)),
        "telegram_send",
        0.0,
        Some(denial),
    )
    .await
    .unwrap();
    repo.record("s1", "tool_success", "telegram_send", 1.0, None)
        .await
        .unwrap();

    let stats = repo.stats_by_dimension("tool_").await.unwrap();
    assert_eq!(stats.len(), 1, "a refusal must not form a second tool row");
    let s = &stats[0];
    assert_eq!(s.dimension, "telegram_send");
    assert_eq!(s.total_events, 1, "only the send that ran counts");
    assert_eq!(s.successes, 1);
    assert_eq!(s.failures, 0);
    assert!((s.success_rate - 1.0).abs() < 0.01);

    // The refusal is still queryable in the event breakdown, just not charged
    // to the tool.
    let recent = repo.recent(10).await.unwrap();
    assert!(
        recent.iter().any(|e| e.event_type == "gate_denied"),
        "the denial must still be recorded"
    );
}
