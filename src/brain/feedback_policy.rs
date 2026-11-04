//! Tool-failure classification policy for the RSI feedback ledger.
//!
//! Not every tool call that returns `success = false` is a tool DEFECT worth
//! learning from. Many are expected, environmental, or user-driven outcomes:
//! a stale-hash mismatch the agent simply retries after re-reading; a
//! channel-send tool with no live bot connected; a user declining an
//! interactive prompt. Counting these as failures dragged tool success-rates
//! down and led the RSI to ban working built-in tools — e.g. escalating
//! `hashline_edit` to a "blanket DO NOT USE" over recoverable stale-hash
//! errors (#236).
//!
//! A third class exists on top of those two: calls the HARNESS refused before
//! the tool ever ran (plan gate, skill gate, guest scope, a declined approval
//! prompt). The tool was never executed, so the tool cannot be at fault, yet
//! the denial used to land in its denominator like a real failure. That is how
//! `telegram_send` reached a 50% health warning with five plan-gate denials
//! and zero Telegram errors (#2251).
//!
//! This module is the single place that decides which of the three a failed
//! call belongs to.

/// How a failed tool call should be classified for the feedback ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// A real defect in the tool or in the way the model called it. Counted in
    /// the tool's success rate.
    Defect,
    /// An expected, environmental or user-driven outcome. Recorded, but kept
    /// out of the success-rate denominator.
    Recoverable,
    /// The harness refused the call before execution. Recorded, but kept out
    /// of the success-rate denominator: the tool never ran.
    GateRefusal,
}

/// Reasons emitted by a gate that refuses a call BEFORE the tool executes.
/// Each is produced by a gate in `src/brain/tools/`, so the tool is never
/// entered and has no part in the outcome.
///
/// Matched lowercase against the failure text, which is why they are written
/// in lowercase here.
const GATE_REFUSAL_MARKERS: &[&str] = &[
    // plan_gate::check_plan_gate (write freeze while Editing) and
    // plan_gate::check_plan_requirement (no plan after N calls in a turn).
    "plan gate:",
    // skill_gate::check block: the full skill body is returned instead of
    // running the call, and the identical retry is armed.
    "[skill gate]",
    // scope::guest_refusal: a non-owner turn reaching for an owner-only tool.
    "is not available to a guest turn",
    // tool_loop approval callback: the user said no to the prompt.
    "user_denied_approval",
];

/// The ledger event type for a tool call outcome.
///
/// Gate refusals get their own event (`gate_denied`) rather than riding on
/// `recoverable_failure`, so "the harness said no" stays distinguishable from
/// "the environment was not ready". Neither is a `tool_` prefixed event, so
/// neither reaches `stats_by_dimension("tool_")`.
pub fn tool_event_type(success: bool, tool_name: &str, error: Option<&str>) -> &'static str {
    if success {
        return "tool_success";
    }
    match classify_tool_failure(tool_name, error) {
        FailureClass::GateRefusal => "gate_denied",
        FailureClass::Recoverable => "recoverable_failure",
        FailureClass::Defect => "tool_failure",
    }
}

/// Classify a failed call into defect, recoverable, or harness refusal.
///
/// The gate check runs first: a refusal reason is authored by the harness and
/// must never be re-read as an environmental arm just because the tool it
/// blocked happens to be `bash` or a channel sender.
pub fn classify_tool_failure(tool_name: &str, error: Option<&str>) -> FailureClass {
    let Some(error) = error else {
        return FailureClass::Defect;
    };
    let e = error.to_ascii_lowercase();
    if GATE_REFUSAL_MARKERS.iter().any(|marker| e.contains(marker)) {
        return FailureClass::GateRefusal;
    }
    if is_recoverable_tool_failure(tool_name, Some(error)) {
        FailureClass::Recoverable
    } else {
        FailureClass::Defect
    }
}

/// Whether a failed tool call is a recoverable / environmental / user-driven
/// outcome rather than a tool defect.
///
/// Recoverable failures are still recorded in the ledger (under a non-`tool_`
/// event type) so they stay queryable, but they never count against the tool's
/// success rate and therefore never drive the RSI to disable a working tool.
pub fn is_recoverable_tool_failure(tool_name: &str, error: Option<&str>) -> bool {
    let Some(error) = error else {
        return false;
    };
    let e = error.to_ascii_lowercase();
    match tool_name {
        // Stale-hash mismatch: the file changed since the agent's last read, so
        // the line hash no longer resolves. The fix is to re-read and retry — a
        // normal step in the edit workflow, not a defect in the tool.
        "hashline_edit" => {
            e.contains("not found")
                || e.contains("may have changed")
                || e.contains("hash")
                || e.contains("collision")
        }
        // Channel send/connect tools need a live, connected bot. "Not
        // connected" (and connection errors generally) is an environment state,
        // not a tool defect — the tool works once the channel is up.
        n if n.ends_with("_send") || n.ends_with("_connect") => {
            e.contains("not connected") || e.contains("connect")
        }
        // bash carries two failure populations under one tool name (#1068). A
        // correctly written command that hit a missing file, a closed port or a
        // dead service says nothing about the tool, and counting it as a defect
        // made `tool_failure|bash` the ledger's loudest signal, steering RSI at
        // "bash is broken" when the real finding was a bad command or an absent
        // resource.
        //
        // Model errors stay hard failures on purpose: wrong syntax, an invented
        // binary, a bare REPL. Those are agent behaviour RSI should act on.
        // So does an unclassifiable failure, because guessing "environmental"
        // would quietly drop real defects out of the denominator.
        "bash" => {
            crate::brain::bash_failure::classify(error)
                == crate::brain::bash_failure::BashFailureKind::Environmental
        }
        _ => false,
    }
}
