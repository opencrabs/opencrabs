//! Tests for trigger-gated cron pipeline with mechanical pre-flight and goal dispatch (issue #233).

use crate::cron::pipeline::{PipelineExecutor, TriggerOutcome, interpolate_template};
use crate::cron::trigger::{TriggerCondition, TriggerResult, TriggerRunner};
use crate::db::models::CronJob;

#[test]
fn test_trigger_condition_parse() {
    assert_eq!(TriggerCondition::parse(None), TriggerCondition::NonEmpty);
    assert_eq!(
        TriggerCondition::parse(Some("non_empty")),
        TriggerCondition::NonEmpty
    );
    assert_eq!(
        TriggerCondition::parse(Some("exit_non_zero")),
        TriggerCondition::ExitNonZero
    );
    assert_eq!(
        TriggerCondition::parse(Some("exit_zero")),
        TriggerCondition::ExitZero
    );
    assert_eq!(
        TriggerCondition::parse(Some("exitzero")),
        TriggerCondition::ExitZero
    );
    assert_eq!(
        TriggerCondition::parse(Some("regex:disk [0-9]+%")),
        TriggerCondition::Regex("disk [0-9]+%".into())
    );
    assert_eq!(
        TriggerCondition::parse(Some("re:ERROR.*")),
        TriggerCondition::Regex("ERROR.*".into())
    );
    assert_eq!(
        TriggerCondition::parse(Some("always")),
        TriggerCondition::Always
    );
    assert_eq!(
        TriggerCondition::parse(Some("unknown_custom")),
        TriggerCondition::NonEmpty
    );
}

#[test]
fn test_trigger_condition_exit_zero() {
    let cond = TriggerCondition::ExitZero;

    let res_success = TriggerResult {
        stdout: "ok".into(),
        stderr: String::new(),
        exit_code: 0,
    };
    assert!(cond.should_fire(&res_success));

    let res_failure = TriggerResult {
        stdout: String::new(),
        stderr: "err".into(),
        exit_code: 1,
    };
    assert!(!cond.should_fire(&res_failure));
}

#[test]
fn test_trigger_condition_regex() {
    let cond = TriggerCondition::Regex("alert:\\s*([0-9]+)".into());

    let res_match = TriggerResult {
        stdout: "alert: 42 anomalies detected".into(),
        stderr: String::new(),
        exit_code: 0,
    };
    assert!(cond.should_fire(&res_match));

    let res_no_match = TriggerResult {
        stdout: "all systems normal".into(),
        stderr: String::new(),
        exit_code: 0,
    };
    assert!(!cond.should_fire(&res_no_match));
}

#[test]
fn test_trigger_condition_non_empty() {
    let cond = TriggerCondition::NonEmpty;

    let res_empty = TriggerResult {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
    };
    assert!(!cond.should_fire(&res_empty));

    let res_stdout = TriggerResult {
        stdout: "found 1 item".into(),
        stderr: String::new(),
        exit_code: 0,
    };
    assert!(cond.should_fire(&res_stdout));

    let res_stderr = TriggerResult {
        stdout: String::new(),
        stderr: "warning: disk usage high".into(),
        exit_code: 0,
    };
    assert!(cond.should_fire(&res_stderr));

    let res_whitespace_only = TriggerResult {
        stdout: "   \n\t  ".into(),
        stderr: String::new(),
        exit_code: 0,
    };
    assert!(!cond.should_fire(&res_whitespace_only));
}

#[test]
fn test_trigger_condition_exit_non_zero() {
    let cond = TriggerCondition::ExitNonZero;

    let res_zero = TriggerResult {
        stdout: "some output".into(),
        stderr: String::new(),
        exit_code: 0,
    };
    assert!(!cond.should_fire(&res_zero));

    let res_err = TriggerResult {
        stdout: String::new(),
        stderr: "failure".into(),
        exit_code: 1,
    };
    assert!(cond.should_fire(&res_err));

    let res_exit_2 = TriggerResult {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 2,
    };
    assert!(cond.should_fire(&res_exit_2));
}

#[test]
fn test_trigger_condition_always() {
    let cond = TriggerCondition::Always;

    let res_empty = TriggerResult {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
    };
    assert!(cond.should_fire(&res_empty));

    let res_err = TriggerResult {
        stdout: "err".into(),
        stderr: String::new(),
        exit_code: 127,
    };
    assert!(cond.should_fire(&res_err));
}

#[test]
fn test_interpolate_template() {
    let result = TriggerResult {
        stdout: "line 1\nline 2".into(),
        stderr: "warn".into(),
        exit_code: 0,
    };

    let tmpl = "Output:\n{output}\nStdout: {stdout}\nStderr: {stderr}\nCode: {exit_code}";
    let interpolated = interpolate_template(tmpl, &result);

    assert!(interpolated.contains("Output:\nline 1\nline 2\nwarn"));
    assert!(interpolated.contains("Stdout: line 1\nline 2"));
    assert!(interpolated.contains("Stderr: warn"));
    assert!(interpolated.contains("Code: 0"));
}

#[tokio::test]
async fn test_trigger_runner_echo() {
    let runner = TriggerRunner::default();
    let res = runner.run("echo 'trigger test'").await.expect("run echo");
    assert_eq!(res.exit_code, 0);
    assert_eq!(res.stdout.trim(), "trigger test");
    assert!(res.stderr.is_empty());
}

#[tokio::test]
async fn test_trigger_runner_exit_code() {
    let runner = TriggerRunner::default();
    let res = runner.run("sh -c 'exit 42'").await.expect("run exit 42");
    assert_eq!(res.exit_code, 42);
}

#[tokio::test]
async fn test_pipeline_evaluate_no_trigger() {
    let job = CronJob::new(
        "no-trigger-job".into(),
        "0 0 * * *".into(),
        "UTC".into(),
        "do something".into(),
        None,
        None,
        "off".into(),
        true,
        None,
        None,
    );

    match PipelineExecutor::evaluate_trigger(&job).await {
        TriggerOutcome::NoTrigger => {}
        other => panic!("expected NoTrigger, got {other:?}"),
    }
}

#[tokio::test]
async fn test_pipeline_evaluate_fired() {
    let job = CronJob::new_with_trigger(
        "fired-job".into(),
        "0 0 * * *".into(),
        "UTC".into(),
        "do something".into(),
        None,
        None,
        "off".into(),
        true,
        None,
        None,
        Some("echo 'work required'".into()),
        Some("non_empty".into()),
        false,
        None,
    );

    match PipelineExecutor::evaluate_trigger(&job).await {
        TriggerOutcome::Fired(res) => {
            assert_eq!(res.stdout.trim(), "work required");
        }
        other => panic!("expected Fired, got {other:?}"),
    }
}

#[tokio::test]
async fn test_pipeline_evaluate_skipped() {
    let job = CronJob::new_with_trigger(
        "skipped-job".into(),
        "0 0 * * *".into(),
        "UTC".into(),
        "do something".into(),
        None,
        None,
        "off".into(),
        true,
        None,
        None,
        Some("true".into()),
        Some("non_empty".into()),
        false,
        None,
    );

    match PipelineExecutor::evaluate_trigger(&job).await {
        TriggerOutcome::Skipped(res) => {
            assert!(res.stdout.is_empty());
        }
        other => panic!("expected Skipped, got {other:?}"),
    }
}

#[tokio::test]
async fn test_cron_set_goal_requires_session() {
    use crate::brain::tools::cron_manage::CronManageTool;
    use crate::brain::tools::r#trait::{Tool, ToolExecutionContext};
    use serde_json::json;

    let db = crate::db::Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let pool = db.pool().clone();
    let repo = crate::db::repository::CronJobRepository::new(pool.clone());
    let tool = CronManageTool::new(repo);
    let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());

    // 1. Create with set_goal = true but no deliver_to -> REJECTED
    let input_no_deliver = json!({
        "action": "create",
        "name": "goal-job-1",
        "cron": "0 0 * * *",
        "tz": "UTC",
        "prompt": "do something",
        "set_goal": true
    });
    let res = tool.execute(input_no_deliver, &ctx).await.unwrap();
    assert!(!res.success, "set_goal without deliver_to must be rejected");
    assert!(
        res.error
            .unwrap_or_default()
            .contains("set_goal requires oc://session"),
        "error message must explain set_goal requires session target"
    );

    // 2. Create with set_goal = true and channel delivery -> REJECTED
    let input_channel_deliver = json!({
        "action": "create",
        "name": "goal-job-2",
        "cron": "0 0 * * *",
        "tz": "UTC",
        "prompt": "do something",
        "deliver_to": "https://example.com/webhook",
        "set_goal": true
    });
    let res2 = tool.execute(input_channel_deliver, &ctx).await.unwrap();
    assert!(
        !res2.success,
        "set_goal with channel delivery must be rejected"
    );
    assert!(
        res2.error
            .unwrap_or_default()
            .contains("set_goal requires oc://session"),
        "error message must explain channel delivery is passive"
    );

    // 3. Create with set_goal = true and session delivery -> SUCCESS
    let input_session_deliver = json!({
        "action": "create",
        "name": "goal-job-3",
        "cron": "0 0 * * *",
        "tz": "UTC",
        "prompt": "do something",
        "deliver_to": "oc://session/12345678-1234-1234-1234-123456789abc",
        "set_goal": true
    });
    let res3 = tool.execute(input_session_deliver, &ctx).await.unwrap();
    assert!(
        res3.success,
        "set_goal with session delivery must succeed: {:?}",
        res3.error
    );
}

// ── trigger process lifecycle ────────────────────────────────────────────────

/// stderr is carried back alongside stdout and the exit code.
///
/// Came in with the inline block that used to live in `cron/trigger.rs`; the
/// rest of that block duplicated assertions already above, this one did not.
#[tokio::test]
async fn test_trigger_runner_captures_stderr() {
    let runner = TriggerRunner::default();
    let res = runner
        .run("echo 'err' >&2; exit 2")
        .await
        .expect("run stderr probe");
    assert_eq!(res.exit_code, 2);
    assert_eq!(res.stderr.trim(), "err");
    assert!(res.stdout.trim().is_empty());
    assert!(TriggerCondition::ExitNonZero.should_fire(&res));
}

/// A trigger that outruns its timeout is an error, and its shell is killed.
///
/// The second half is the part worth testing: on timeout the `wait_with_output`
/// future is dropped, and a tokio `Child` does not kill on drop by default, so
/// without `kill_on_drop(true)` the shell keeps running unsupervised — once per
/// schedule tick, for the life of the daemon. Asserted by looking for the
/// process, with a unique marker in the command line, not by reading the
/// builder.
///
/// The `; :` matters: with a single command `sh` execs it directly and the
/// marker vanishes from the surviving process, which made the first version of
/// this test pass with `kill_on_drop` removed. The trailing no-op keeps `sh`
/// itself alive and carrying the marker.
///
/// Scope is the direct child. `kill_on_drop` kills the shell, not its
/// descendants, so the inner `sleep` outlives it either way — kept short for
/// that reason. Reaping the whole tree would need a process group.
#[cfg(unix)]
#[tokio::test]
async fn test_timed_out_trigger_kills_its_shell() {
    use std::time::Duration;

    let marker = format!("opencrabs_trigger_orphan_probe_{}", std::process::id());
    let runner = TriggerRunner::new(Duration::from_millis(200));

    let err = runner
        .run(&format!("sleep 5; : # {marker}"))
        .await
        .expect_err("a 5s sleep must outrun a 200ms timeout");
    assert!(err.contains("timed out"), "unexpected error: {err}");

    // The kill is delivered as the dropped child is reaped; give it a moment.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let survivors = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(&marker)
        .output()
        .expect("pgrep");
    let found = String::from_utf8_lossy(&survivors.stdout);
    assert!(
        found.trim().is_empty(),
        "timed-out trigger left its shell running (pids: {})",
        found.trim()
    );
}

// ===========================================================================
// #526 — a fired trigger's payload must reach the AGENT-path turn message.
// ===========================================================================
//
// **What these tests can and cannot prove.** The turn-message build lives inside
// `CronScheduler::tick` -> `execute_job`, which needs a `ChannelFactory`, a
// `ServiceContext` and a live agent, so it is not unit-reachable — the same
// limitation the #457 source-shape test above documents. These tests therefore
// prove two things, and say plainly that they are two:
//
//   1. the TRANSFORM — `interpolate_template` substitutes the placeholders and is
//      the identity on a placeholder-free prompt (cases a, b, f, g, plus the
//      empty-payload and join-rule edges);
//   2. the WIRING SHAPE — that ONLY the `Fired` arm builds the substituted
//      prompt, that the payload is carried to the turn, and that the raw
//      `job.prompt.clone()` read is gone.
//
// Cases c, d and e (NoTrigger / Skipped / Error) are covered by the wiring-shape
// half rather than by constructing the variants: the contract for a non-firing
// outcome is "the prompt is unchanged", the mechanism is `fired_prompt == None`,
// and `None.unwrap_or(prompt)` is asserted directly. Constructing the variants
// here would prove only that Rust can build an enum.
//
// The wiring's BEHAVIORAL proof is the live smoke leg (a real job whose trigger
// stdout provably reaches the turn), not this file.

/// (a) A fired trigger substitutes every placeholder it finds.
#[test]
fn test_fired_prompt_substitutes_all_placeholders() {
    let res = TriggerResult {
        stdout: "line 1\nline 2".into(),
        stderr: "warn".into(),
        exit_code: 0,
    };

    assert_eq!(
        interpolate_template("out={stdout}|err={stderr}|code={exit_code}", &res),
        "out=line 1\nline 2|err=warn|code=0"
    );

    // A signal-killed trigger reports exit_code -1 (`trigger.rs` uses
    // `status.code().unwrap_or(-1)`), and -1 is what must be substituted.
    let killed = TriggerResult {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: -1,
    };
    assert_eq!(interpolate_template("c={exit_code}", &killed), "c=-1");
}

/// (b) A fired trigger with a placeholder-free prompt changes nothing — this is
/// the whole contract of the fix: it is opt-in, and its blast radius is nil.
#[test]
fn test_fired_prompt_without_placeholder_is_byte_identical() {
    let res = TriggerResult {
        stdout: "line 1\nline 2".into(),
        stderr: "warn".into(),
        exit_code: 0,
    };
    let prompt = "plain prompt with no placeholders at all";

    let out = interpolate_template(prompt, &res);

    assert_eq!(out, prompt);
    assert_eq!(out.len(), prompt.len(), "length must be unchanged");
    assert_eq!(out.as_bytes(), prompt.as_bytes(), "bytes must be identical");
}

/// (f) `interpolate_template` is a SEQUENTIAL replace chain, not a single pass.
/// `{output}` is expanded first, so a payload that itself contains the literal
/// `{stdout}` is expanded again by the later step. Pinned as an exact string so
/// a future move to single-pass semantics is a deliberate edit rather than a
/// silent behaviour change. This documents pre-existing helper behaviour and
/// adds no escaping.
#[test]
fn test_fired_prompt_output_then_stdout_is_a_sequential_chain() {
    let res = TriggerResult {
        stdout: "S{stdout}T".into(),
        stderr: String::new(),
        exit_code: 0,
    };

    assert_eq!(interpolate_template("P{output}Q", &res), "PSS{stdout}TTQ");
}

/// (g) The identity holds at the top of the size range too, so the fix's blast
/// radius is pinned at both ends.
#[test]
fn test_fired_prompt_large_payload_without_placeholder_is_byte_identical() {
    let big = "x".repeat(5000);
    let res = TriggerResult {
        stdout: big.clone(),
        stderr: String::new(),
        exit_code: 0,
    };
    let prompt = "no placeholder here";

    assert_eq!(big.len(), 5000);
    assert_eq!(interpolate_template(prompt, &res), prompt);
}

/// A fire that produced nothing still substitutes the empty string, so
/// "fired and found nothing" stays distinguishable from "no trigger at all".
#[test]
fn test_fired_prompt_empty_payload_substitutes_the_empty_string() {
    let res = TriggerResult {
        stdout: String::new(),
        stderr: String::new(),
        exit_code: 0,
    };

    assert_eq!(interpolate_template("a{stdout}b", &res), "ab");
}

/// `{output}` follows `combined_output()`'s join rule, not a naive
/// concatenation: stdout alone, stderr alone, or `stdout\nstderr`.
#[test]
fn test_fired_prompt_output_join_rule() {
    let both = TriggerResult {
        stdout: "L1".into(),
        stderr: "L2".into(),
        exit_code: 0,
    };
    let stdout_only = TriggerResult {
        stdout: "L1".into(),
        stderr: String::new(),
        exit_code: 0,
    };
    let stderr_only = TriggerResult {
        stdout: String::new(),
        stderr: "L2".into(),
        exit_code: 0,
    };

    assert_eq!(interpolate_template("o={output}", &both), "o=L1\nL2");
    assert_eq!(interpolate_template("o={output}", &stdout_only), "o=L1");
    assert_eq!(interpolate_template("o={output}", &stderr_only), "o=L2");
}

/// (c) (d) (e) — the substitution is `Fired`-only, and the `None` path is the
/// identity. A non-firing outcome that started building a prompt would leak a
/// substitution onto a turn where nothing fired: the likeliest way to get this
/// fix wrong, and the reason this is asserted against the arms themselves.
#[test]
fn test_non_fired_outcomes_never_build_a_prompt() {
    const SRC: &str = include_str!("../cron/scheduler.rs");

    const SKIPPED: &str = "TriggerOutcome::Skipped(ref trig_res) =>";
    const ERROR: &str = "TriggerOutcome::Error(err) =>";
    const FIRED: &str = "TriggerOutcome::Fired(ref trig_res) =>";
    const NO_TRIGGER: &str = "TriggerOutcome::NoTrigger =>";

    fn arm<'a>(src: &'a str, from: &str, to: &str) -> &'a str {
        let start = src
            .find(from)
            .unwrap_or_else(|| panic!("arm `{from}` not found — did the match move?"));
        let rest = &src[start..];
        let end = rest
            .find(to)
            .unwrap_or_else(|| panic!("arm `{to}` not found after `{from}` — did the match move?"));
        &rest[..end]
    }

    // Only the Fired arm may build the substituted prompt.
    assert!(
        !arm(SRC, SKIPPED, ERROR).contains("fired_prompt"),
        "the Skipped arm must not build a substituted prompt — nothing fired"
    );
    assert!(
        !arm(SRC, ERROR, FIRED).contains("fired_prompt"),
        "the Error arm must not build a substituted prompt — nothing fired"
    );
    assert!(
        !arm(SRC, NO_TRIGGER, "resolve_or_create_cron_session").contains("fired_prompt"),
        "the NoTrigger arm must not build a substituted prompt — nothing fired"
    );
    assert!(
        arm(SRC, FIRED, NO_TRIGGER).contains("fired_prompt"),
        "the Fired arm must build the substituted prompt — that is the fix"
    );

    // The None path is the identity in PRODUCTION, not in a local literal: the
    // turn message falls back to the job's own prompt, byte for byte. Asserted
    // against the real call site. A local `None.unwrap_or(x)` would be a
    // tautology that can never fail (clippy::unnecessary_literal_unwrap) — the
    // behavioural identity itself is covered by the placeholder tests above.
    assert!(
        SRC.contains("fired_prompt.unwrap_or(job.prompt.as_str())"),
        "the None path must fall back to the job's own prompt — the turn message \
         is byte-identical when nothing fired"
    );
}

/// The wiring invariant, in the repo's established `include_str!` idiom: the
/// Fired arm builds the message through the existing helper, the payload is
/// carried to the turn, and the defect's own signature is gone.
#[test]
fn test_agent_turn_message_is_built_through_the_trigger_helper() {
    const SCHEDULER_SRC: &str = include_str!("../cron/scheduler.rs");

    // The Fired arm builds the message through the existing helper.
    assert!(SCHEDULER_SRC.contains("interpolate_template("));
    // The payload is carried to the turn, not dropped at the match.
    assert!(SCHEDULER_SRC.contains("fired_prompt"));
    assert!(SCHEDULER_SRC.contains("fired_prompt.as_deref()"));
    assert!(SCHEDULER_SRC.contains("turn_prompt"));
    // The defect's own signature: the raw prompt reaching the turn verbatim.
    assert!(
        !SCHEDULER_SRC.contains("job.prompt.clone()"),
        "#526 REGRESSION: the turn message is built from the raw job prompt again — \
         the fired trigger's payload is being dropped"
    );
    // The gate arms must NOT be restructured (edition-2024 match ergonomics).
    assert!(!SCHEDULER_SRC.contains("match &outcome"));
}

// ===========================================================================
// #2065: the trigger gate must run on the UNSCOPED path too.
// ===========================================================================
//
// The gate used to live INSIDE the `needs_scope` arm of the scheduler's
// profile split. A job stamped with the ACTIVE profile takes the other arm, so
// in a single-profile install every job carrying a `trigger_cmd` skipped the
// gate entirely: full agent turn, real tokens, real cost, on every tick, and
// the 0-token skip path never executed once.
//
// These tests drive `run_cron_job_body`, the function the unscoped arm now
// calls, because `CronScheduler::tick` needs a live daemon. The structural
// sentinel at the end pins the other half: that the split calls it on BOTH
// arms and owns no trigger arm of its own.

const GATE_SRC: &str = include_str!("../cron/scheduler.rs");

async fn gate_pool() -> crate::db::Pool {
    let db = crate::db::Database::connect_in_memory()
        .await
        .expect("in-memory db");
    db.run_migrations().await.expect("migrations");
    db.pool().clone()
}

fn gate_factory(ctx: crate::services::ServiceContext) -> crate::channels::ChannelFactory {
    let (_, config_rx) = tokio::sync::watch::channel(crate::config::Config::default());
    crate::channels::ChannelFactory::new(
        std::sync::Arc::new(crate::tests::agent_service_mocks::MockProvider)
            as std::sync::Arc<dyn crate::brain::provider::Provider>,
        ctx,
        "test brain".to_string(),
        std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from("/tmp/oc_test_brain"),
        std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        config_rx,
    )
}

/// One `cron_job_runs` row: (status, content, input_tokens, output_tokens, cost).
type RunRow = (String, String, i64, i64, f64);

/// Every run row for a job, oldest first.
async fn gate_rows(pool: &crate::db::Pool, job_id: uuid::Uuid) -> Vec<RunRow> {
    let jid = job_id.to_string();
    pool.get()
        .await
        .expect("pool")
        .interact(move |conn| -> rusqlite::Result<Vec<RunRow>> {
            let mut stmt = conn.prepare(
                "SELECT status, COALESCE(content,''), input_tokens, output_tokens, cost \
                     FROM cron_job_runs WHERE job_id = ?1 ORDER BY started_at",
            )?;
            let rows = stmt
                .query_map(rusqlite::params![jid], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .expect("interact")
        .expect("query")
}

async fn gate_session_count(pool: &crate::db::Pool) -> i64 {
    pool.get()
        .await
        .expect("pool")
        .interact(move |conn| conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0)))
        .await
        .expect("interact")
        .expect("count")
}

/// The reported case: a job stamped with the ACTIVE profile. `needs_scope` is
/// false for it, which is exactly why the gate was dead. Its trigger prints
/// nothing, so the fire must be skipped at 0 tokens with no agent turn.
#[tokio::test]
async fn unmet_trigger_on_the_active_profile_records_a_zero_token_skip() {
    let pool = gate_pool().await;
    let repo = crate::db::CronJobRepository::new(pool.clone());

    let mut job = CronJob::new_with_trigger(
        "gate-unscoped".into(),
        "0 0 * * *".into(),
        "UTC".into(),
        "do something".into(),
        None,
        None,
        "off".into(),
        true,
        None,
        None,
        Some("true".into()), // prints nothing, exits 0
        Some("non_empty".into()),
        false,
        None,
    );
    job.profile_name = Some(crate::config::profile::current_profile_name());

    // Pin the test to the bug's own condition: if this job ever stops taking
    // the unscoped arm, the test stops covering the reported case.
    let active = crate::config::profile::active_profile().unwrap_or("default");
    let needs_scope =
        job.profile_name.as_deref().is_some() && job.profile_name.as_deref() != Some(active);
    assert!(
        !needs_scope,
        "this test only means something while the job takes the unscoped arm"
    );

    repo.insert(&job).await.expect("insert job");

    let ctx = crate::services::ServiceContext::new(pool.clone());
    let factory = gate_factory(ctx.clone());
    let run_repo = crate::db::CronJobRunRepository::new(pool.clone());

    crate::cron::scheduler::run_cron_job_body(&job, &factory, &ctx, &run_repo)
        .await
        .expect("gate body");

    let rows = gate_rows(&pool, job.id).await;
    assert_eq!(rows.len(), 1, "exactly one ledger row per fire: {rows:?}");
    let (status, content, input, output, cost) = &rows[0];
    assert_eq!(status, "success");
    assert!(content.contains("Trigger condition not met"), "{content}");
    assert_eq!((*input, *output), (0, 0), "the skip must be a 0-token row");
    assert_eq!(*cost, 0.0);
    assert_eq!(
        gate_session_count(&pool).await,
        0,
        "the agent turn must not run: no cron session may be created"
    );
}

/// The same job on the OTHER path. `needs_scope` is true here, so the caller
/// wraps the body in a profile home scope. The outcome must be identical, so
/// the two paths cannot drift apart again.
#[tokio::test]
async fn unmet_trigger_on_a_foreign_profile_produces_the_same_skip() {
    let pool = gate_pool().await;
    let repo = crate::db::CronJobRepository::new(pool.clone());

    let mut job = CronJob::new_with_trigger(
        "gate-scoped".into(),
        "0 0 * * *".into(),
        "UTC".into(),
        "do something".into(),
        None,
        None,
        "off".into(),
        true,
        None,
        None,
        Some("true".into()),
        Some("non_empty".into()),
        false,
        None,
    );
    job.profile_name = Some("ops".to_string());
    repo.insert(&job).await.expect("insert job");

    let ctx = crate::services::ServiceContext::new(pool.clone());
    let factory = gate_factory(ctx.clone());
    let run_repo = crate::db::CronJobRunRepository::new(pool.clone());

    crate::config::profile::with_profile_home_async(Some("ops"), async {
        crate::cron::scheduler::run_cron_job_body(&job, &factory, &ctx, &run_repo)
            .await
            .expect("gate body");
    })
    .await;

    let rows = gate_rows(&pool, job.id).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (status, content, input, output, cost) = &rows[0];
    assert_eq!(status, "success");
    assert!(content.contains("Trigger condition not met"), "{content}");
    assert_eq!((*input, *output), (0, 0));
    assert_eq!(*cost, 0.0);
    assert_eq!(gate_session_count(&pool).await, 0);
}

/// A job with an empty prompt and a FIRING trigger is the documented 0-token
/// delivery pattern. It used to live inside the scoped arm too, so on the
/// active profile it fell through to a full agent turn with an empty prompt.
#[tokio::test]
async fn firing_trigger_with_empty_prompt_delivers_directly_on_the_unscoped_path() {
    let pool = gate_pool().await;
    let repo = crate::db::CronJobRepository::new(pool.clone());

    let mut job = CronJob::new_with_trigger(
        "gate-direct".into(),
        "0 0 * * *".into(),
        "UTC".into(),
        String::new(), // empty prompt = 0-token pass-through
        None,
        None,
        "off".into(),
        true,
        None,
        None,
        Some("echo alert-body".into()),
        Some("non_empty".into()),
        false,
        None,
    );
    job.profile_name = Some(crate::config::profile::current_profile_name());
    repo.insert(&job).await.expect("insert job");

    let ctx = crate::services::ServiceContext::new(pool.clone());
    let factory = gate_factory(ctx.clone());
    let run_repo = crate::db::CronJobRunRepository::new(pool.clone());

    crate::cron::scheduler::run_cron_job_body(&job, &factory, &ctx, &run_repo)
        .await
        .expect("gate body");

    let rows = gate_rows(&pool, job.id).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (status, content, input, output, cost) = &rows[0];
    assert_eq!(status, "success");
    assert!(
        content.contains("alert-body"),
        "the trigger payload is the report: {content}"
    );
    assert_eq!((*input, *output), (0, 0));
    assert_eq!(*cost, 0.0);
    assert_eq!(
        gate_session_count(&pool).await,
        0,
        "a direct delivery runs no agent turn"
    );
}

/// The half a behavioural test cannot reach: `CronScheduler::tick` needs a live
/// daemon, so the SHAPE of the split is pinned here. It must call the gate body
/// on both arms and must own no trigger arm of its own, which is the defect.
#[test]
fn the_needs_scope_split_does_not_own_the_trigger_gate() {
    let start = GATE_SRC
        .find("let result = if needs_scope {")
        .expect("the profile split must exist");
    let rest = &GATE_SRC[start..];
    let end = rest
        .find("if let Err(e) = result {")
        .expect("the split is followed by the error report");
    let split = &rest[..end];

    assert!(
        !split.contains("TriggerOutcome::"),
        "#2065 REGRESSION: the trigger gate is back inside the needs_scope split, \
         a job stamped with the active profile takes the unscoped arm and its \
         trigger_cmd is silently dead again"
    );
    assert_eq!(
        split.matches("run_cron_job_body(").count(),
        2,
        "both arms of the split must call the same gate body"
    );

    // And the gate body must actually evaluate, not merely exist.
    let body_at = GATE_SRC
        .find("pub(crate) async fn run_cron_job_body(")
        .expect("the gate body must exist");
    let body = &GATE_SRC[body_at..];
    assert!(body.contains("evaluate_trigger("));
    assert!(body.contains("record_skipped_run("));
    assert!(body.contains("execute_direct_trigger_job("));
}
