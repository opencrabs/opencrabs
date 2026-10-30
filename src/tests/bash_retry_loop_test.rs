//! Tests for Layer 3 of the bash retry-loop guard
//! (`check_recent_failure` / `record_bash_outcome`).
//!
//! Background: Layers 1 (interactive command rejection) and 2 (tool
//! description hints) catch most loops on attempt 1. Layer 3 is the
//! catch-all for any failure mode — if the agent runs the exact same
//! command twice in a row and the first attempt failed, the second
//! attempt is short-circuited with a "stop retrying" message that
//! quotes the prior error.
//!
//! State is process-wide (a `OnceLock<RwLock<HashMap<...>>>`), so each
//! test uses a fresh `Uuid::new_v4()` for isolation — cargo test runs
//! tests in parallel, and an explicit "clear all state" reset would
//! wipe sibling tests' buffers.

// Per-session buffers are keyed on `Uuid`, so each test that uses a
// fresh `Uuid::new_v4()` is naturally isolated from siblings even
// though the underlying state map is process-wide. No explicit reset
// needed — and trying to reset wipes parallel tests' buffers.
use crate::brain::tools::bash::{
    RECENT_BASH_MAX_BLOCKS, RECENT_BASH_TTL, RECENT_BASH_WINDOW, check_recent_failure,
    record_bash_block, record_bash_outcome, record_bash_outcome_at, recorded_at_of,
};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[test]
fn fresh_session_has_no_history() {
    let sid = Uuid::new_v4();
    assert!(check_recent_failure(sid, "ls -la").is_none());
}

#[test]
fn successful_run_does_not_block_retry() {
    // Successful commands aren't held against a re-run — the agent is
    // free to run `git status` 50 times.
    let sid = Uuid::new_v4();
    record_bash_outcome(sid, "git status".to_string(), false, None);
    assert!(check_recent_failure(sid, "git status").is_none());
}

#[test]
fn failed_run_blocks_exact_retry() {
    // The core case: agent ran a command, it failed, agent tries the
    // exact same string. The second attempt is intercepted with a
    // message that quotes the prior error.
    let sid = Uuid::new_v4();
    record_bash_outcome(
        sid,
        "git push origin main".to_string(),
        true,
        Some("rejected: non-fast-forward".to_string()),
    );
    let msg = check_recent_failure(sid, "git push origin main").expect("should block");
    assert!(msg.contains("already ran this exact command"));
    assert!(msg.contains("Previous error:"));
    assert!(msg.contains("non-fast-forward"));
}

#[test]
fn whitespace_normalization_catches_cosmetic_retries() {
    // The agent sometimes adds trailing whitespace or pads with spaces
    // when "retrying". Trimmed comparison still catches it.
    let sid = Uuid::new_v4();
    record_bash_outcome(
        sid,
        "git pull".to_string(),
        true,
        Some("conflict".to_string()),
    );
    assert!(check_recent_failure(sid, "  git pull  ").is_some());
}

#[test]
fn different_command_is_not_blocked() {
    // A different command (even if related) is a different attempt and
    // should be allowed through.
    let sid = Uuid::new_v4();
    record_bash_outcome(sid, "git push origin main".to_string(), true, None);
    assert!(check_recent_failure(sid, "git push origin main --force-with-lease").is_none());
    assert!(check_recent_failure(sid, "git push origin foo").is_none());
}

#[test]
fn move_to_front_keeps_one_entry_per_unique_command() {
    // Re-recording the same command moves it to the front (latest
    // outcome wins) instead of stacking duplicate entries that would
    // crowd out other history.
    let sid = Uuid::new_v4();
    record_bash_outcome(sid, "cmd_a".to_string(), true, Some("err1".to_string()));
    record_bash_outcome(sid, "cmd_b".to_string(), false, None);
    record_bash_outcome(sid, "cmd_a".to_string(), true, Some("err2".to_string()));

    // The latest outcome's snippet wins.
    let msg = check_recent_failure(sid, "cmd_a").expect("should block");
    assert!(msg.contains("err2"));
    assert!(!msg.contains("err1"));
}

#[test]
fn outcome_can_flip_from_fail_to_success() {
    // If a previously-failed command later succeeds, the entry flips
    // and a subsequent retry is no longer blocked.
    let sid = Uuid::new_v4();
    record_bash_outcome(sid, "make build".to_string(), true, Some("err".to_string()));
    assert!(check_recent_failure(sid, "make build").is_some());

    record_bash_outcome(sid, "make build".to_string(), false, None);
    assert!(check_recent_failure(sid, "make build").is_none());
}

#[test]
fn old_failure_falls_off_window() {
    // After RECENT_BASH_WINDOW newer commands, an old failure should
    // no longer block — the agent has clearly moved on and a deliberate
    // retry-much-later is fine.
    let sid = Uuid::new_v4();
    record_bash_outcome(
        sid,
        "old_failing_cmd".to_string(),
        true,
        Some("err".to_string()),
    );
    for i in 0..RECENT_BASH_WINDOW {
        record_bash_outcome(sid, format!("filler_cmd_{}", i), false, None);
    }
    assert!(check_recent_failure(sid, "old_failing_cmd").is_none());
}

#[test]
fn sessions_are_isolated() {
    // Two sessions running the same failing command must not see each
    // other's history.
    let sid_a = Uuid::new_v4();
    let sid_b = Uuid::new_v4();
    record_bash_outcome(
        sid_a,
        "broken_cmd".to_string(),
        true,
        Some("err".to_string()),
    );
    assert!(check_recent_failure(sid_a, "broken_cmd").is_some());
    assert!(check_recent_failure(sid_b, "broken_cmd").is_none());
}

#[test]
fn rejection_message_names_the_window_size() {
    // The agent should see the size of the look-back window so it
    // knows roughly how to "escape" (different command, or wait it
    // out with other work).
    let sid = Uuid::new_v4();
    record_bash_outcome(sid, "x".to_string(), true, None);
    let msg = check_recent_failure(sid, "x").expect("should block");
    assert!(msg.contains(&RECENT_BASH_WINDOW.to_string()));
}

#[test]
fn rejection_message_carries_the_shared_variation_directive() {
    // #32: the Layer-3 rejection must teach the same lesson every
    // loop-breaker teaches — read the result in hand, verify state,
    // vary or report completion. The 2026-08-29 incident was exactly
    // this fixture: a ship call retried after a sibling lane had
    // already shipped, the reason sitting in the first result.
    let sid = Uuid::new_v4();
    record_bash_outcome(
        sid,
        "oc-deploy ship --sha abc123".to_string(),
        true,
        Some("already shipped by sibling lane".to_string()),
    );
    let msg = check_recent_failure(sid, "oc-deploy ship --sha abc123").expect("should block");
    assert!(msg.contains("Do not re-issue the same call"), "{msg}");
    assert!(msg.contains("result you already have"), "{msg}");
    assert!(msg.contains("report completion"), "{msg}");
    // The frame and the quoted error survive the rewording.
    assert!(msg.contains("already ran this exact command"), "{msg}");
    assert!(msg.contains("Previous error:"), "{msg}");
}

#[test]
fn stale_failure_stops_blocking() {
    // #195: A failure older than RECENT_BASH_TTL should age out and no longer block.
    let sid = Uuid::new_v4();
    let past = Instant::now()
        .checked_sub(RECENT_BASH_TTL + Duration::from_secs(1))
        .expect("monotonic clock underflow");
    record_bash_outcome_at(
        sid,
        "failing-cmd".to_string(),
        true,
        Some("transient error".to_string()),
        past,
    );
    assert!(
        check_recent_failure(sid, "failing-cmd").is_none(),
        "stale failure older than TTL should yield execution"
    );
}

#[test]
fn block_does_not_refresh_the_clock() {
    // #195: record_bash_block must NOT update recorded_at. Updating recorded_at
    // on each block would permanently refresh the TTL and resurrect the latch.
    let sid = Uuid::new_v4();
    let initial_t = Instant::now()
        .checked_sub(Duration::from_secs(60))
        .expect("monotonic clock underflow");
    record_bash_outcome_at(
        sid,
        "failing-cmd".to_string(),
        true,
        Some("transient error".to_string()),
        initial_t,
    );

    let t_before = recorded_at_of(sid, "failing-cmd").expect("entry must exist");
    assert_eq!(t_before, initial_t);

    record_bash_block(sid, "failing-cmd");

    let t_after = recorded_at_of(sid, "failing-cmd").expect("entry must exist");
    assert_eq!(
        t_after, initial_t,
        "record_bash_block must NOT refresh recorded_at"
    );
}

#[test]
fn repeated_blocks_eventually_yield() {
    // #195: Consecutive short-circuits up to RECENT_BASH_MAX_BLOCKS must block,
    // and the attempt after RECENT_BASH_MAX_BLOCKS blocks must yield one real execution.
    let sid = Uuid::new_v4();
    record_bash_outcome(
        sid,
        "failing-cmd".to_string(),
        true,
        Some("transient error".to_string()),
    );

    for i in 0..RECENT_BASH_MAX_BLOCKS {
        assert!(
            check_recent_failure(sid, "failing-cmd").is_some(),
            "attempt {i} should be blocked within budget"
        );
        record_bash_block(sid, "failing-cmd");
    }

    assert!(
        check_recent_failure(sid, "failing-cmd").is_none(),
        "after budget exhausted, check_recent_failure must yield one real execution"
    );
}

#[test]
fn real_outcome_resets_the_block_budget() {
    // #195: Once budget is exhausted and execution yields, if that run fails again,
    // record_bash_outcome resets blocks to 0 and arms a fresh budget.
    let sid = Uuid::new_v4();
    record_bash_outcome(
        sid,
        "failing-cmd".to_string(),
        true,
        Some("transient error".to_string()),
    );

    for _ in 0..RECENT_BASH_MAX_BLOCKS {
        record_bash_block(sid, "failing-cmd");
    }
    assert!(check_recent_failure(sid, "failing-cmd").is_none());

    // Next real execution fails again
    record_bash_outcome(
        sid,
        "failing-cmd".to_string(),
        true,
        Some("persisting error".to_string()),
    );

    assert!(
        check_recent_failure(sid, "failing-cmd").is_some(),
        "new real failure outcome must re-arm block budget"
    );
}

#[test]
fn stale_entry_does_not_block_a_different_command() {
    // #195: Isolation check — TTL and blocks check apply specifically to the matching entry.
    let sid = Uuid::new_v4();
    record_bash_outcome(
        sid,
        "first-cmd".to_string(),
        true,
        Some("error 1".to_string()),
    );
    assert!(check_recent_failure(sid, "different-cmd").is_none());
}

// ── #2211: the guard's premise is "same command, unchanged world" ─────────
//
// A recorded failure stops being true when something the agent did changed
// the environment, or when the user asks again in a new turn. The reported
// case: `cat release.cfg` fails because the file is missing, `write_file`
// creates it, the user asks for the identical command, and the guard refuses
// to run it.

#[test]
fn a_successful_non_bash_call_clears_the_recorded_failure() {
    let sid = Uuid::new_v4();
    record_bash_outcome(
        sid,
        "cat release.cfg".to_string(),
        true,
        Some("cat: release.cfg: No such file or directory".to_string()),
    );
    assert!(check_recent_failure(sid, "cat release.cfg").is_some());

    // What the registry does after `write_file` succeeds.
    crate::brain::tools::bash::note_environment_changed(sid);

    assert!(
        check_recent_failure(sid, "cat release.cfg").is_none(),
        "the missing file now exists, so the recorded error is obsolete"
    );
}

#[test]
fn a_new_turn_clears_the_recorded_failure() {
    let sid = Uuid::new_v4();
    record_bash_outcome(
        sid,
        "cat release.cfg".to_string(),
        true,
        Some("cat: release.cfg: No such file or directory".to_string()),
    );

    // What run_tool_loop does at the start of every user turn.
    crate::brain::tools::bash::note_environment_changed(sid);

    assert!(check_recent_failure(sid, "cat release.cfg").is_none());
}

#[test]
fn clearing_one_session_leaves_another_sessions_history_intact() {
    let cleared = Uuid::new_v4();
    let other = Uuid::new_v4();
    record_bash_outcome(
        cleared,
        "make build".to_string(),
        true,
        Some("error".to_string()),
    );
    record_bash_outcome(
        other,
        "make build".to_string(),
        true,
        Some("error".to_string()),
    );

    crate::brain::tools::bash::note_environment_changed(cleared);

    assert!(check_recent_failure(cleared, "make build").is_none());
    assert!(
        check_recent_failure(other, "make build").is_some(),
        "another session's failure history is not this session's business"
    );
}

#[test]
fn a_loop_still_blocks_after_a_clear_because_bash_never_clears_itself() {
    // The clear only fires for non-bash tools and new turns. Within a turn,
    // bash retrying itself is exactly the loop the guard exists to stop.
    let sid = Uuid::new_v4();
    record_bash_outcome(
        sid,
        "cargo build".to_string(),
        true,
        Some("error[E0308]".to_string()),
    );
    assert!(check_recent_failure(sid, "cargo build").is_some());
    record_bash_block(sid, "cargo build");
    assert!(
        check_recent_failure(sid, "cargo build").is_some(),
        "back-to-back bash retries must stay blocked"
    );
}

// ── #2211 wiring: which tool calls are allowed to clear the window ────────
//
// The clear only fires from the registry dispatch choke point, and only for a
// call that could have changed what the blocked command sees. A read cannot,
// so fail -> read -> retry the identical command stays blocked.

mod registry_wiring {
    use super::*;
    use crate::brain::tools::{
        Tool, ToolCapability, ToolExecutionContext, ToolRegistry, ToolResult,
    };
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::Arc;

    struct Stub {
        name: &'static str,
        capability: ToolCapability,
    }

    #[async_trait]
    impl Tool for Stub {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn input_schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![self.capability]
        }
        fn requires_approval(&self) -> bool {
            false
        }
        async fn execute(
            &self,
            _input: Value,
            _context: &ToolExecutionContext,
        ) -> crate::brain::tools::Result<ToolResult> {
            Ok(ToolResult::success("ok".to_string()))
        }
    }

    fn registry_with(name: &'static str, capability: ToolCapability) -> ToolRegistry {
        let registry = ToolRegistry::new();
        registry.register(Arc::new(Stub { name, capability }));
        registry
    }

    async fn dispatch(registry: &ToolRegistry, name: &str, session_id: Uuid) {
        let context = ToolExecutionContext::new(session_id).with_auto_approve(true);
        registry
            .execute(name, serde_json::json!({}), &context)
            .await
            .expect("stub dispatch");
    }

    #[tokio::test]
    async fn a_successful_write_clears_the_window() {
        let sid = Uuid::new_v4();
        record_bash_outcome(
            sid,
            "cat release.cfg".to_string(),
            true,
            Some("cat: release.cfg: No such file or directory".to_string()),
        );
        let registry = registry_with("write_file", ToolCapability::WriteFiles);

        dispatch(&registry, "write_file", sid).await;

        assert!(
            check_recent_failure(sid, "cat release.cfg").is_none(),
            "the file now exists, so the retry must be allowed"
        );
    }

    #[tokio::test]
    async fn a_read_does_not_clear_the_window() {
        let sid = Uuid::new_v4();
        record_bash_outcome(
            sid,
            "cat release.cfg".to_string(),
            true,
            Some("cat: release.cfg: No such file or directory".to_string()),
        );
        let registry = registry_with("read_file", ToolCapability::ReadFiles);

        dispatch(&registry, "read_file", sid).await;

        assert!(
            check_recent_failure(sid, "cat release.cfg").is_some(),
            "nothing changed on disk, so this is still the loop the guard stops"
        );
    }
}
