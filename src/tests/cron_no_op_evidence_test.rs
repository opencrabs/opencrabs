//! #1703 hole 1: a cron run reported success with zero tool executions.
//!
//! The evidence gate: `success` requires a tool to have actually started
//! inside the run. A turn with zero starts is recorded `no_op` and every
//! destination receives a timestamped one-line notice instead of the report.

use crate::brain::agent::ProgressEvent;
use crate::cron::scheduler::{
    TurnOutcome, classify_turn_outcome, not_executed_notice, tool_start_counter,
};
use crate::db::CronJobRunRepository;
use rusqlite::params;
use serde_json::json;
use uuid::Uuid;

fn started(tool: &str) -> ProgressEvent {
    ProgressEvent::ToolStarted {
        tool_name: tool.to_string(),
        tool_input: json!({}),
        tool_call_id: String::new(),
    }
}

fn completed(tool: &str) -> ProgressEvent {
    ProgressEvent::ToolCompleted {
        tool_name: tool.to_string(),
        tool_input: json!({}),
        success: true,
        summary: String::new(),
        tool_call_id: String::new(),
    }
}

#[test]
fn zero_tool_starts_is_no_op_not_success() {
    let outcome = classify_turn_outcome(0, "umami-report", "run-42", "2026-10-03T22:00:00Z");
    let TurnOutcome::NoOp(notice) = outcome else {
        panic!("zero-evidence turn must not be a success verdict: {outcome:?}");
    };
    // The notice replaces the report: it names the job, is timestamped and
    // run-stamped so a reader can anchor it.
    assert!(notice.contains("umami-report"), "{notice}");
    assert!(notice.contains("not executed"), "{notice}");
    assert!(notice.contains("2026-10-03T22:00:00Z"), "{notice}");
    assert!(notice.contains("run-42"), "{notice}");
}

#[test]
fn one_tool_start_is_success() {
    // The gate is existence of evidence, not volume: a single start earns
    // the normal success path unchanged.
    assert_eq!(
        classify_turn_outcome(1, "job", "run-1", "2026-10-03T22:00:00Z"),
        TurnOutcome::Success
    );
    assert_eq!(
        classify_turn_outcome(7, "job", "run-1", "2026-10-03T22:00:00Z"),
        TurnOutcome::Success
    );
}

#[test]
fn notice_is_ascii_one_liner() {
    let notice = not_executed_notice("morning recap", "abc-123", "2026-10-03T07:00:00Z");
    assert!(
        notice.is_ascii(),
        "delivered lines must stay ASCII: {notice}"
    );
    assert!(!notice.contains('\n'), "notice must be one line");
}

#[test]
fn counter_counts_only_tool_starts() {
    let (starts, cb) = tool_start_counter();
    let sid = Uuid::new_v4();
    cb(sid, started("bash"));
    cb(sid, ProgressEvent::Thinking);
    cb(sid, completed("bash"));
    cb(
        sid,
        ProgressEvent::StreamingChunk {
            text: "done!".into(),
        },
    );
    cb(sid, started("read_file"));
    assert_eq!(
        starts.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "only ToolStarted may inflate execution evidence"
    );

    // Zero evidence stays zero: text alone is exactly the replay shape the
    // gate exists for.
    let (quiet_starts, quiet_cb) = tool_start_counter();
    quiet_cb(
        Uuid::new_v4(),
        ProgressEvent::StreamingChunk { text: "ok".into() },
    );
    assert_eq!(quiet_starts.load(std::sync::atomic::Ordering::Relaxed), 0);
}

async fn test_db() -> crate::db::Pool {
    let db = crate::db::Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    db.pool().clone()
}

/// Seed a parent job + a 'running' run row, mirroring the scheduler insert.
async fn seed_running(pool: &crate::db::Pool, run_id: &str) {
    let job_id = Uuid::new_v4().to_string();
    let rid = run_id.to_string();
    let ts = chrono::Utc::now().to_rfc3339();
    pool.get()
        .await
        .unwrap()
        .interact(move |conn| {
            conn.execute(
                "INSERT INTO cron_jobs (id, name, cron_expr, timezone, prompt, thinking, \
                 auto_approve, enabled, created_at, updated_at) \
                 VALUES (?1, 'evidence-test', '0 0 * * *', 'UTC', 'x', 'off', 1, 1, ?2, ?2)",
                params![job_id, ts],
            )?;
            conn.execute(
                "INSERT INTO cron_job_runs (id, job_id, job_name, status, content, error, \
                 input_tokens, output_tokens, cost, provider, model, started_at, completed_at, created_at) \
                 VALUES (?1, ?2, 'evidence-test', 'running', '', '', 0, 0, 0, '', '', ?3, NULL, ?3)",
                params![rid, job_id, ts],
            )
        })
        .await
        .unwrap()
        .unwrap();
}

async fn read_row(pool: &crate::db::Pool, run_id: &str) -> (String, String, Option<String>) {
    let rid = run_id.to_string();
    pool.get()
        .await
        .unwrap()
        .interact(move |conn| {
            conn.query_row(
                "SELECT status, content, completed_at FROM cron_job_runs WHERE id = ?1",
                params![rid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
        })
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn complete_no_op_writes_no_op_and_keeps_report_for_forensics() {
    let pool = test_db().await;
    let run_id = Uuid::new_v4().to_string();
    seed_running(&pool, &run_id).await;

    let repo = CronJobRunRepository::new(pool.clone());
    repo.complete_no_op(&run_id, "fabricated report text", 120, 34, 0.0042)
        .await
        .unwrap();

    let (status, content, completed_at) = read_row(&pool, &run_id).await;
    assert_eq!(status, "no_op", "ledger must refuse a success verdict");
    assert_eq!(
        content, "fabricated report text",
        "report stays for forensics"
    );
    assert!(completed_at.is_some(), "no_op is a terminal status");
}

#[tokio::test]
async fn complete_success_still_writes_success() {
    // The gate must not move the goalposts for evidenced runs: the normal
    // writer keeps its exact contract.
    let pool = test_db().await;
    let run_id = Uuid::new_v4().to_string();
    seed_running(&pool, &run_id).await;

    let repo = CronJobRunRepository::new(pool.clone());
    repo.complete_success(&run_id, "real report", 100, 50, 0.001)
        .await
        .unwrap();

    let (status, content, _) = read_row(&pool, &run_id).await;
    assert_eq!(status, "success");
    assert_eq!(content, "real report");
}

#[test]
fn scheduler_wires_the_evidence_probe_into_the_agent_path() {
    // Structural sentinel: the agent call site must pass the counting
    // callback (not None) and the completion path must branch on the gate.
    let src = include_str!("../cron/scheduler.rs");
    assert!(
        src.contains("let (tool_starts, evidence_cb) = tool_start_counter();"),
        "evidence probe must be created per run"
    );
    assert!(
        src.contains("Some(evidence_cb),"),
        "the agent turn must receive the evidence callback"
    );
    assert!(
        src.contains("classify_turn_outcome(starts, &job.name, &run_id, &at)"),
        "the completion path must consult the gate"
    );
}
