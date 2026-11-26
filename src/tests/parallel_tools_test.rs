//! Tests for concurrent tool-batch execution (#361).
//!
//! Proves the three contracts of the parallel path: eligibility gating
//! (approval-required tools and small batches stay sequential), result
//! ordering (outcomes match the original tool_use order even when earlier
//! tools finish later), and real concurrency (wall clock far below the sum
//! of tool durations), plus cancellation.

use crate::brain::agent::AgentService;
use crate::brain::tools::{Tool, ToolExecutionContext, ToolRegistry};
use crate::db::Database;
use crate::services::ServiceContext;
use crate::tests::agent_service_mocks::{MockProvider, MockTool, MockToolRequiresApproval};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

/// Sleeps for `ms` then echoes `tag` — used to prove ordering + overlap.
struct SleepTool;

#[async_trait]
impl Tool for SleepTool {
    fn name(&self) -> &str {
        "sleep_tool"
    }
    fn description(&self) -> &str {
        "sleeps then echoes"
    }
    fn input_schema(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {"ms": {"type": "integer"}, "tag": {"type": "string"}}})
    }
    fn capabilities(&self) -> Vec<crate::brain::tools::ToolCapability> {
        vec![]
    }
    fn requires_approval(&self) -> bool {
        false
    }
    async fn execute(
        &self,
        input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> crate::brain::tools::Result<crate::brain::tools::ToolResult> {
        let ms = input.get("ms").and_then(|v| v.as_u64()).unwrap_or(0);
        let tag = input
            .get("tag")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        Ok(crate::brain::tools::ToolResult::success(format!(
            "done:{tag}"
        )))
    }
}

/// A writer that names its target through the shared `path` key and reports
/// it via [`Tool::write_target`] — the shape `edit_file` / `write_file` have.
/// Two of these on one path are what the batch gate must keep apart (#593):
/// run concurrently they each read the original and the later write replaces
/// the file outright, dropping the earlier edit while both report success.
struct PathWriterTool;

#[async_trait]
impl Tool for PathWriterTool {
    fn name(&self) -> &str {
        "path_writer"
    }
    fn description(&self) -> &str {
        "writes one path"
    }
    fn input_schema(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {"path": {"type": "string"}}})
    }
    fn capabilities(&self) -> Vec<crate::brain::tools::ToolCapability> {
        vec![]
    }
    fn requires_approval(&self) -> bool {
        false
    }
    fn write_target(
        &self,
        input: &serde_json::Value,
        working_directory: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        crate::brain::tools::r#trait::write_target_from_path_arg(input, working_directory)
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
        _context: &ToolExecutionContext,
    ) -> crate::brain::tools::Result<crate::brain::tools::ToolResult> {
        Ok(crate::brain::tools::ToolResult::success(
            "wrote".to_string(),
        ))
    }
}

async fn service_with_tools() -> AgentService {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let context = ServiceContext::new(db.pool().clone());
    let provider = Arc::new(MockProvider);
    let registry = ToolRegistry::new();
    registry.register(Arc::new(MockTool));
    registry.register(Arc::new(MockToolRequiresApproval));
    registry.register(Arc::new(SleepTool));
    registry.register(Arc::new(PathWriterTool));
    // Config::default() derives zero for max_concurrent (serde defaults
    // only apply when parsing config.toml), so set the real default here.
    let mut config = crate::config::Config::default();
    config.agent.max_concurrent = 4;
    // A gating policy, because the default is `auto-always` and the policy now
    // resolves into the parallel-eligibility gate (#769). Under an auto policy
    // nothing needs approval, so an approval-gated tool would correctly stay
    // parallel-eligible and the "approval poisons the batch" case below would
    // not be exercised at all.
    config.agent.approval_policy = "ask".to_string();
    AgentService::new(provider, context, &config)
        .await
        .with_tool_registry(Arc::new(registry))
}

fn batch(entries: &[(&str, serde_json::Value)]) -> Vec<(String, String, serde_json::Value)> {
    entries
        .iter()
        .enumerate()
        .map(|(i, (name, input))| (format!("id-{i}"), name.to_string(), input.clone()))
        .collect()
}

#[tokio::test]
async fn approval_tools_and_small_batches_stay_sequential() {
    let service = service_with_tools().await;
    let ctx = ToolExecutionContext::new(Uuid::new_v4());

    // Single tool: never parallel.
    let single = batch(&[("test_tool", json!({}))]);
    assert!(!service.batch_is_parallel_eligible(&single, &ctx, false));

    // Two auto-approved tools: parallel.
    let two = batch(&[("test_tool", json!({})), ("sleep_tool", json!({"ms": 1}))]);
    assert!(service.batch_is_parallel_eligible(&two, &ctx, false));

    // Any approval-gated tool poisons the batch back to sequential.
    let with_approval = batch(&[
        ("test_tool", json!({})),
        ("approval_tool", json!({"action": "x"})),
    ]);
    assert!(!service.batch_is_parallel_eligible(&with_approval, &ctx, false));

    // Unknown tool name: sequential path owns the error handling.
    let unknown = batch(&[("test_tool", json!({})), ("nope", json!({}))]);
    assert!(!service.batch_is_parallel_eligible(&unknown, &ctx, false));

    // auto_approve on the context neutralizes the approval gate.
    let mut auto_ctx = ToolExecutionContext::new(Uuid::new_v4());
    auto_ctx.auto_approve = true;
    assert!(service.batch_is_parallel_eligible(&with_approval, &auto_ctx, false));
}

/// #593: two calls that write one path must not share a parallel batch. Each
/// writer is a whole-file read-modify-write, so overlapping them makes both
/// read the same original and the later write replace the file outright —
/// the earlier edit vanishes while both calls report success. Refusing
/// parallelism sends the batch down the sequential path instead, where each
/// call reads its predecessor's output and both edits land.
#[tokio::test]
async fn same_path_writers_stay_sequential() {
    let service = service_with_tools().await;
    let ctx = ToolExecutionContext::new(Uuid::new_v4());

    // Two writers, one path: sequential, so each edit builds on the last.
    let same_path = batch(&[
        ("path_writer", json!({"path": "shared.txt"})),
        ("path_writer", json!({"path": "shared.txt"})),
    ]);
    assert!(!service.batch_is_parallel_eligible(&same_path, &ctx, false));

    // The gate compares resolved targets, not raw strings: a differently
    // spelled route to the same file still collides.
    let respelled = batch(&[
        ("path_writer", json!({"path": "shared.txt"})),
        ("path_writer", json!({"path": "./shared.txt"})),
    ]);
    assert!(!service.batch_is_parallel_eligible(&respelled, &ctx, false));

    // Two writers, two paths: nothing to lose, so it stays parallel.
    let distinct_paths = batch(&[
        ("path_writer", json!({"path": "a.txt"})),
        ("path_writer", json!({"path": "b.txt"})),
    ]);
    assert!(service.batch_is_parallel_eligible(&distinct_paths, &ctx, false));

    // A reader beside a writer on one path reports no write target, so it
    // never collides — the batch keeps its parallelism.
    let reader_and_writer = batch(&[
        ("test_tool", json!({})),
        ("path_writer", json!({"path": "shared.txt"})),
    ]);
    assert!(service.batch_is_parallel_eligible(&reader_and_writer, &ctx, false));
}

#[tokio::test]
async fn parallel_batch_preserves_order_and_overlaps() {
    let service = service_with_tools().await;
    let ctx = ToolExecutionContext::new(Uuid::new_v4());
    // Slowest FIRST: with sequential execution this takes ~300+200+100=600ms;
    // ordered results despite the first finishing last proves buffered
    // in-order yielding, and the wall clock proves real overlap.
    let uses = batch(&[
        ("sleep_tool", json!({"ms": 300, "tag": "a"})),
        ("sleep_tool", json!({"ms": 200, "tag": "b"})),
        ("sleep_tool", json!({"ms": 100, "tag": "c"})),
    ]);
    let started = std::time::Instant::now();
    let out = service
        .execute_tools_parallel(Uuid::new_v4(), uses, &ctx, None, None, Uuid::new_v4())
        .await;
    let elapsed = started.elapsed();

    assert!(!out.cancelled);
    assert_eq!(out.successes, 3);
    assert_eq!(out.outputs.len(), 3);
    // Results in ORIGINAL order regardless of completion order.
    let texts: Vec<&str> = out
        .results
        .iter()
        .filter_map(|b| match b {
            crate::brain::provider::ContentBlock::ToolResult { content, .. } => {
                Some(content.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["done:a", "done:b", "done:c"]);
    // Overlap: sequential execution costs 600ms. Three concurrent sleeps cost
    // the longest one plus scheduling slack, so the ceiling sits well above
    // the 300ms floor instead of 40ms under it, which flaked on a loaded box.
    // The floor also proves the longest tool really ran (#2287).
    assert!(
        elapsed >= std::time::Duration::from_millis(300),
        "the longest tool returned early, took {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(480),
        "expected concurrent execution, took {elapsed:?}"
    );
}

#[tokio::test]
async fn pre_cancelled_token_aborts_the_batch() {
    let service = service_with_tools().await;
    let ctx = ToolExecutionContext::new(Uuid::new_v4());
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let uses = batch(&[
        ("sleep_tool", json!({"ms": 200, "tag": "a"})),
        ("sleep_tool", json!({"ms": 200, "tag": "b"})),
    ]);
    let out = service
        .execute_tools_parallel(
            Uuid::new_v4(),
            uses,
            &ctx,
            Some(&token),
            None,
            Uuid::new_v4(),
        )
        .await;
    assert!(out.cancelled);
    assert!(out.results.is_empty(), "no results after pre-cancel");
}
