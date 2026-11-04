//! Recursive Self-Improvement (RSI) Tests
//!
//! Comprehensive tests for the feedback ledger repository, feedback_record tool,
//! feedback_analyze tool, and self_improve tool.

// --- Feedback Ledger Repository Tests ---

mod feedback_ledger_repo {
    use crate::db::Database;
    use crate::db::repository::FeedbackLedgerRepository;

    async fn setup() -> (Database, FeedbackLedgerRepository) {
        let db = Database::connect_in_memory().await.expect("in-memory DB");
        db.run_migrations().await.expect("migrations");
        let repo = FeedbackLedgerRepository::new(db.pool().clone());
        (db, repo)
    }

    #[tokio::test]
    async fn record_and_count() {
        let (_db, repo) = setup().await;
        assert_eq!(repo.total_count().await.unwrap(), 0);

        let id = repo
            .record("sess1", "tool_success", "bash", 1.0, None)
            .await
            .unwrap();
        assert!(id > 0);
        assert_eq!(repo.total_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn record_with_metadata() {
        let (_db, repo) = setup().await;
        let id = repo
            .record(
                "sess1",
                "tool_failure",
                "edit",
                0.0,
                Some(r#"{"error":"file not found"}"#),
            )
            .await
            .unwrap();
        assert!(id > 0);

        let entries = repo.recent(10).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event_type, "tool_failure");
        assert_eq!(entries[0].dimension, "edit");
        assert!(
            entries[0]
                .metadata
                .as_deref()
                .unwrap()
                .contains("file not found")
        );
    }

    #[tokio::test]
    async fn recent_returns_latest() {
        let (_db, repo) = setup().await;
        for i in 0..5 {
            repo.record("sess1", "tool_success", &format!("tool_{i}"), 1.0, None)
                .await
                .unwrap();
        }

        let entries = repo.recent(3).await.unwrap();
        assert_eq!(entries.len(), 3);
        // Should return some subset of the 5 entries (ordering by created_at DESC, rowid tiebreak)
        // All entries should be from our set
        for e in &entries {
            assert!(e.dimension.starts_with("tool_"));
        }
    }

    #[tokio::test]
    async fn recent_respects_limit() {
        let (_db, repo) = setup().await;
        for i in 0..10 {
            repo.record("sess1", "tool_success", &format!("t{i}"), 1.0, None)
                .await
                .unwrap();
        }
        let entries = repo.recent(5).await.unwrap();
        assert_eq!(entries.len(), 5);
    }

    #[tokio::test]
    async fn by_event_type_filters() {
        let (_db, repo) = setup().await;
        repo.record("s1", "tool_success", "bash", 1.0, None)
            .await
            .unwrap();
        repo.record("s1", "tool_failure", "edit", 0.0, None)
            .await
            .unwrap();
        repo.record("s1", "tool_success", "read", 1.0, None)
            .await
            .unwrap();
        repo.record("s1", "user_correction", "tone", 1.0, None)
            .await
            .unwrap();

        let successes = repo.by_event_type("tool_success", 50).await.unwrap();
        assert_eq!(successes.len(), 2);
        for e in &successes {
            assert_eq!(e.event_type, "tool_success");
        }

        let failures = repo.by_event_type("tool_failure", 50).await.unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].dimension, "edit");

        let corrections = repo.by_event_type("user_correction", 50).await.unwrap();
        assert_eq!(corrections.len(), 1);
    }

    #[tokio::test]
    async fn by_event_type_empty() {
        let (_db, repo) = setup().await;
        let entries = repo.by_event_type("nonexistent", 50).await.unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn stats_by_dimension() {
        let (_db, repo) = setup().await;
        // bash: 3 success, 1 failure
        for _ in 0..3 {
            repo.record("s1", "tool_success", "bash", 1.0, None)
                .await
                .unwrap();
        }
        repo.record("s1", "tool_failure", "bash", 0.0, None)
            .await
            .unwrap();
        // edit: 1 success, 2 failures
        repo.record("s1", "tool_success", "edit", 1.0, None)
            .await
            .unwrap();
        for _ in 0..2 {
            repo.record("s1", "tool_failure", "edit", 0.0, None)
                .await
                .unwrap();
        }

        let stats = repo.stats_by_dimension("tool_").await.unwrap();
        assert_eq!(stats.len(), 2);

        // bash has more total events, should be first
        let bash = &stats[0];
        assert_eq!(bash.dimension, "bash");
        assert_eq!(bash.total_events, 4);
        assert_eq!(bash.successes, 3);
        assert_eq!(bash.failures, 1);
        assert!((bash.success_rate - 0.75).abs() < 0.01);

        let edit = &stats[1];
        assert_eq!(edit.dimension, "edit");
        assert_eq!(edit.total_events, 3);
        assert_eq!(edit.successes, 1);
        assert_eq!(edit.failures, 2);
        assert!((edit.success_rate - 1.0 / 3.0).abs() < 0.01);
    }

    #[tokio::test]
    async fn discovery_miss_excluded_from_success_rate() {
        // #214: a pre-execution miss (tool never ran: bad params or unresolved
        // name) is recorded as `discovery_miss`, NOT `tool_failure`. The
        // success-rate query aggregates `event_type LIKE 'tool_%'`, so the miss
        // is excluded from both numerator and denominator. A tool that ran once
        // successfully keeps a 100% rate despite an earlier blind miss.
        let (_db, repo) = setup().await;
        repo.record("s1", "discovery_miss", "telegram_send", 0.0, None)
            .await
            .unwrap();
        repo.record("s1", "tool_success", "telegram_send", 1.0, None)
            .await
            .unwrap();

        let stats = repo.stats_by_dimension("tool_").await.unwrap();
        assert_eq!(
            stats.len(),
            1,
            "discovery_miss must not form its own tool row"
        );
        let s = &stats[0];
        assert_eq!(s.dimension, "telegram_send");
        assert_eq!(s.total_events, 1, "only the tool_success counts");
        assert_eq!(s.successes, 1);
        assert_eq!(s.failures, 0);
        assert!((s.success_rate - 1.0).abs() < 0.01);

        // Contrast: had the miss been recorded as tool_failure, the rate halves.
        repo.record("s1", "tool_failure", "telegram_send", 0.0, None)
            .await
            .unwrap();
        let polluted = repo.stats_by_dimension("tool_").await.unwrap();
        assert!((polluted[0].success_rate - 0.5).abs() < 0.01);
    }

    #[tokio::test]
    async fn stats_by_dimension_empty() {
        let (_db, repo) = setup().await;
        let stats = repo.stats_by_dimension("tool_").await.unwrap();
        assert!(stats.is_empty());
    }

    #[tokio::test]
    async fn summary_groups_by_event_type() {
        let (_db, repo) = setup().await;
        repo.record("s1", "tool_success", "bash", 1.0, None)
            .await
            .unwrap();
        repo.record("s1", "tool_success", "read", 1.0, None)
            .await
            .unwrap();
        repo.record("s1", "tool_failure", "edit", 0.0, None)
            .await
            .unwrap();
        repo.record("s1", "user_correction", "tone", 1.0, None)
            .await
            .unwrap();

        let summary = repo.summary().await.unwrap();
        assert_eq!(summary.len(), 3);
        // Ordered by count DESC
        assert_eq!(summary[0].0, "tool_success");
        assert_eq!(summary[0].1, 2);
    }

    #[tokio::test]
    async fn summary_empty_ledger() {
        let (_db, repo) = setup().await;
        let summary = repo.summary().await.unwrap();
        assert!(summary.is_empty());
    }

    #[tokio::test]
    async fn count_since_filters_by_date() {
        let (_db, repo) = setup().await;
        repo.record("s1", "tool_success", "bash", 1.0, None)
            .await
            .unwrap();

        // All events should be "since" a long time ago
        let count = repo.count_since("2000-01-01T00:00:00Z").await.unwrap();
        assert_eq!(count, 1);

        // None should be "since" a future date
        let count = repo.count_since("2099-01-01T00:00:00Z").await.unwrap();
        assert_eq!(count, 0);
    }

    /// Helper: insert a feedback row at a specific historical timestamp,
    /// bypassing `record()` which always stamps "now". Required to
    /// simulate the staleness scenario the windowed stats are built for.
    async fn record_at(
        repo: &crate::db::repository::FeedbackLedgerRepository,
        session_id: &str,
        event_type: &str,
        dimension: &str,
        value: f64,
        created_at: &str,
    ) {
        let sid = session_id.to_string();
        let et = event_type.to_string();
        let dim = dimension.to_string();
        let ts = created_at.to_string();
        repo.pool()
            .get()
            .await
            .unwrap()
            .interact(move |conn| -> rusqlite::Result<()> {
                conn.execute(
                    "INSERT INTO feedback_ledger (session_id, event_type, dimension, value, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![sid, et, dim, value, ts],
                )?;
                Ok(())
            })
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn stats_by_dimension_since_none_matches_lifetime() {
        // Pinning that passing None to the windowed variant is identical
        // to the old (lifetime) `stats_by_dimension`. Without this the
        // two could quietly diverge on schema changes.
        let (_db, repo) = setup().await;
        for _ in 0..3 {
            repo.record("s1", "tool_success", "bash", 1.0, None)
                .await
                .unwrap();
        }
        repo.record("s1", "tool_failure", "bash", 0.0, None)
            .await
            .unwrap();

        let lifetime = repo.stats_by_dimension("tool_").await.unwrap();
        let windowed = repo.stats_by_dimension_since("tool_", None).await.unwrap();
        assert_eq!(lifetime.len(), windowed.len());
        assert_eq!(lifetime[0].dimension, windowed[0].dimension);
        assert_eq!(lifetime[0].total_events, windowed[0].total_events);
        assert_eq!(lifetime[0].successes, windowed[0].successes);
        assert_eq!(lifetime[0].failures, windowed[0].failures);
    }

    #[tokio::test]
    async fn stats_by_dimension_since_drops_stale_failures() {
        // Regression: the 2026-04-25 RSI logs reported "exa_search 100%
        // failure (5/5)" forever because old failures from 2026-04-14/17
        // never aged out. After the fix we window stats; the same
        // failures with no recent activity must report zero events
        // inside a 7-day window.
        let (_db, repo) = setup().await;
        let stale = "2026-04-14T22:49:45Z";
        for _ in 0..5 {
            record_at(&repo, "s1", "tool_failure", "exa_search", 0.0, stale).await;
        }
        // Today's reference point — picked so the stale rows fall
        // 11 days behind it.
        let since = "2026-04-25T00:00:00Z";
        let stats = repo
            .stats_by_dimension_since("tool_", Some(since))
            .await
            .unwrap();
        assert!(
            stats.iter().all(|s| s.dimension != "exa_search"),
            "exa_search must be excluded once its only events are outside the window: {:?}",
            stats
        );
    }

    #[tokio::test]
    async fn stats_by_dimension_since_keeps_recent_real_failures() {
        // Mirror image of the above: tools with failures inside the
        // window stay reported. browser_navigate had failures on
        // 2026-04-22 — those must survive a 7-day window from 04-25.
        let (_db, repo) = setup().await;
        let recent = "2026-04-22T12:54:40Z";
        let stale = "2026-04-13T00:31:08Z";
        for _ in 0..4 {
            record_at(&repo, "s1", "tool_failure", "browser_navigate", 0.0, recent).await;
        }
        // Old successes that should NOT inflate the in-window denominator.
        for _ in 0..10 {
            record_at(&repo, "s1", "tool_success", "browser_navigate", 1.0, stale).await;
        }

        let since = "2026-04-18T00:00:00Z";
        let stats = repo
            .stats_by_dimension_since("tool_", Some(since))
            .await
            .unwrap();
        let row = stats
            .iter()
            .find(|s| s.dimension == "browser_navigate")
            .expect("browser_navigate must remain visible inside the window");
        assert_eq!(row.total_events, 4, "only the 4 in-window failures count");
        assert_eq!(row.failures, 4);
        assert_eq!(row.successes, 0);
        assert!(
            row.success_rate < 0.01,
            "with all in-window events being failures, success_rate must be ~0"
        );
    }

    #[tokio::test]
    async fn stats_by_dimension_since_mixed_events_inside_window() {
        // Sanity: a tool with both successes and failures inside the
        // window reports the right ratio.
        let (_db, repo) = setup().await;
        let recent = "2026-04-24T10:00:00Z";
        for _ in 0..3 {
            record_at(&repo, "s1", "tool_success", "bash", 1.0, recent).await;
        }
        record_at(&repo, "s1", "tool_failure", "bash", 0.0, recent).await;

        let since = "2026-04-18T00:00:00Z";
        let stats = repo
            .stats_by_dimension_since("tool_", Some(since))
            .await
            .unwrap();
        let bash = stats
            .iter()
            .find(|s| s.dimension == "bash")
            .expect("bash present");
        assert_eq!(bash.total_events, 4);
        assert_eq!(bash.successes, 3);
        assert_eq!(bash.failures, 1);
        assert!((bash.success_rate - 0.75).abs() < 0.01);
    }

    #[tokio::test]
    async fn multiple_sessions() {
        let (_db, repo) = setup().await;
        repo.record("sess_a", "tool_success", "bash", 1.0, None)
            .await
            .unwrap();
        repo.record("sess_b", "tool_failure", "bash", 0.0, None)
            .await
            .unwrap();

        assert_eq!(repo.total_count().await.unwrap(), 2);
        let entries = repo.recent(10).await.unwrap();
        let sessions: Vec<&str> = entries.iter().map(|e| e.session_id.as_str()).collect();
        assert!(sessions.contains(&"sess_a"));
        assert!(sessions.contains(&"sess_b"));
    }

    #[tokio::test]
    async fn value_preserved() {
        let (_db, repo) = setup().await;
        repo.record("s1", "context_compaction", "tokens", 4096.0, None)
            .await
            .unwrap();
        let entries = repo.recent(1).await.unwrap();
        assert!((entries[0].value - 4096.0).abs() < 0.01);
    }
}

// --- Feedback Record Tool Tests ---

mod feedback_record_tool {
    use crate::brain::tools::feedback_record::FeedbackRecordTool;
    use crate::brain::tools::{Tool, ToolExecutionContext};
    use crate::db::Database;
    use crate::services::ServiceContext;
    use serde_json::json;
    use uuid::Uuid;

    async fn setup() -> (Database, ToolExecutionContext) {
        let db = Database::connect_in_memory().await.expect("in-memory DB");
        db.run_migrations().await.expect("migrations");
        let svc = ServiceContext::new(db.pool().clone());
        let mut ctx = ToolExecutionContext::new(Uuid::new_v4());
        ctx.service_context = Some(svc);
        (db, ctx)
    }

    #[test]
    fn tool_metadata() {
        let tool = FeedbackRecordTool;
        assert_eq!(tool.name(), "feedback_record");
        assert!(!tool.requires_approval());
        assert!(tool.capabilities().is_empty());
        let schema = tool.input_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "event_type"));
        assert!(required.iter().any(|v| v == "dimension"));
    }

    #[tokio::test]
    async fn record_success() {
        let (_db, ctx) = setup().await;
        let tool = FeedbackRecordTool;
        let result = tool
            .execute(
                json!({
                    "event_type": "tool_success",
                    "dimension": "bash",
                    "value": 1.0
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("Recorded feedback"));
        assert!(result.output.contains("tool_success/bash"));
    }

    #[tokio::test]
    async fn record_with_metadata() {
        let (_db, ctx) = setup().await;
        let tool = FeedbackRecordTool;
        let result = tool
            .execute(
                json!({
                    "event_type": "tool_failure",
                    "dimension": "edit",
                    "value": 0.0,
                    "metadata": "file was read-only"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("tool_failure/edit"));
    }

    #[tokio::test]
    async fn record_default_value() {
        let (_db, ctx) = setup().await;
        let tool = FeedbackRecordTool;
        let result = tool
            .execute(
                json!({
                    "event_type": "pattern_observed",
                    "dimension": "user_prefers_concise"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.success);
        // Default value is 1.0
        assert!(result.output.contains("= 1"));
    }

    #[tokio::test]
    async fn record_missing_event_type() {
        let (_db, ctx) = setup().await;
        let tool = FeedbackRecordTool;
        let result = tool
            .execute(
                json!({
                    "dimension": "bash"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.as_deref().unwrap();
        assert!(err.contains("required"));
    }

    #[tokio::test]
    async fn record_missing_dimension() {
        let (_db, ctx) = setup().await;
        let tool = FeedbackRecordTool;
        let result = tool
            .execute(
                json!({
                    "event_type": "tool_success"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.as_deref().unwrap();
        assert!(err.contains("required"));
    }

    #[tokio::test]
    async fn record_empty_strings() {
        let (_db, ctx) = setup().await;
        let tool = FeedbackRecordTool;
        let result = tool
            .execute(
                json!({
                    "event_type": "",
                    "dimension": ""
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn record_no_service_context() {
        let ctx = ToolExecutionContext::new(Uuid::new_v4());
        let tool = FeedbackRecordTool;
        let result = tool
            .execute(
                json!({
                    "event_type": "tool_success",
                    "dimension": "bash"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.as_deref().unwrap();
        assert!(err.contains("database"));
    }
}

// --- Feedback Analyze Tool Tests ---

mod feedback_analyze_tool {
    use crate::brain::tools::feedback_analyze::FeedbackAnalyzeTool;
    use crate::brain::tools::{Tool, ToolExecutionContext};
    use crate::db::Database;
    use crate::db::repository::FeedbackLedgerRepository;
    use crate::services::ServiceContext;
    use serde_json::json;
    use uuid::Uuid;

    async fn setup() -> (Database, ToolExecutionContext, FeedbackLedgerRepository) {
        let db = Database::connect_in_memory().await.expect("in-memory DB");
        db.run_migrations().await.expect("migrations");
        let repo = FeedbackLedgerRepository::new(db.pool().clone());
        let svc = ServiceContext::new(db.pool().clone());
        let mut ctx = ToolExecutionContext::new(Uuid::new_v4());
        ctx.service_context = Some(svc);
        (db, ctx, repo)
    }

    /// Helper to get the text from a ToolResult (output for success, error for failure)
    fn result_text(result: &crate::brain::tools::ToolResult) -> &str {
        if result.success {
            &result.output
        } else {
            result.error.as_deref().unwrap_or("")
        }
    }

    #[test]
    fn tool_metadata() {
        let tool = FeedbackAnalyzeTool;
        assert_eq!(tool.name(), "feedback_analyze");
        assert!(!tool.requires_approval());
        assert!(tool.capabilities().is_empty());
    }

    #[tokio::test]
    async fn summary_empty_ledger() {
        let (_db, ctx, _repo) = setup().await;
        let tool = FeedbackAnalyzeTool;
        let result = tool
            .execute(json!({"query": "summary"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No feedback data yet"));
    }

    #[tokio::test]
    async fn summary_with_data() {
        let (_db, ctx, repo) = setup().await;
        repo.record("s1", "tool_success", "bash", 1.0, None)
            .await
            .unwrap();
        repo.record("s1", "tool_failure", "edit", 0.0, None)
            .await
            .unwrap();
        repo.record("s1", "tool_success", "read", 1.0, None)
            .await
            .unwrap();

        let tool = FeedbackAnalyzeTool;
        let result = tool
            .execute(json!({"query": "summary"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("3 total events"));
        assert!(result.output.contains("tool_success"));
        assert!(result.output.contains("tool_failure"));
    }

    #[tokio::test]
    async fn tool_stats_empty() {
        let (_db, ctx, _repo) = setup().await;
        let tool = FeedbackAnalyzeTool;
        let result = tool
            .execute(json!({"query": "tool_stats"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No tool execution data"));
    }

    #[tokio::test]
    async fn tool_stats_with_data() {
        let (_db, ctx, repo) = setup().await;
        for _ in 0..3 {
            repo.record("s1", "tool_success", "bash", 1.0, None)
                .await
                .unwrap();
        }
        repo.record("s1", "tool_failure", "bash", 0.0, None)
            .await
            .unwrap();

        let tool = FeedbackAnalyzeTool;
        let result = tool
            .execute(json!({"query": "tool_stats"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("bash"));
        assert!(result.output.contains("75.0%"));
    }

    #[tokio::test]
    async fn recent_empty() {
        let (_db, ctx, _repo) = setup().await;
        let tool = FeedbackAnalyzeTool;
        let result = tool
            .execute(json!({"query": "recent"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No recent feedback"));
    }

    #[tokio::test]
    async fn recent_with_data() {
        let (_db, ctx, repo) = setup().await;
        repo.record("s1", "tool_success", "bash", 1.0, Some("ran ls"))
            .await
            .unwrap();

        let tool = FeedbackAnalyzeTool;
        let result = tool
            .execute(json!({"query": "recent", "limit": 10}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("1 entries"));
        assert!(result.output.contains("tool_success"));
        assert!(result.output.contains("bash"));
    }

    #[tokio::test]
    async fn recent_respects_limit() {
        let (_db, ctx, repo) = setup().await;
        for i in 0..10 {
            repo.record("s1", "tool_success", &format!("t{i}"), 1.0, None)
                .await
                .unwrap();
        }

        let tool = FeedbackAnalyzeTool;
        let result = tool
            .execute(json!({"query": "recent", "limit": 3}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("3 entries"));
    }

    #[tokio::test]
    async fn failures_empty() {
        let (_db, ctx, _repo) = setup().await;
        let tool = FeedbackAnalyzeTool;
        let result = tool
            .execute(json!({"query": "failures"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("No tool failures"));
    }

    #[tokio::test]
    async fn failures_with_data() {
        let (_db, ctx, repo) = setup().await;
        repo.record("s1", "tool_success", "bash", 1.0, None)
            .await
            .unwrap();
        repo.record("s1", "tool_failure", "edit", 0.0, Some("permission denied"))
            .await
            .unwrap();

        let tool = FeedbackAnalyzeTool;
        let result = tool
            .execute(json!({"query": "failures"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("1 entries"));
        assert!(result.output.contains("edit"));
        assert!(result.output.contains("permission denied"));
    }

    #[tokio::test]
    async fn unknown_query_type() {
        let (_db, ctx, _repo) = setup().await;
        let tool = FeedbackAnalyzeTool;
        let result = tool.execute(json!({"query": "bogus"}), &ctx).await.unwrap();
        assert!(!result.success);
        assert!(result_text(&result).contains("Unknown query type"));
    }

    #[tokio::test]
    async fn no_service_context() {
        let ctx = ToolExecutionContext::new(Uuid::new_v4());
        let tool = FeedbackAnalyzeTool;
        let result = tool
            .execute(json!({"query": "summary"}), &ctx)
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result_text(&result).contains("database"));
    }
}

// --- Self-Improve Tool Tests ---

mod self_improve_tool {
    use crate::brain::tools::self_improve::SelfImproveTool;
    use crate::brain::tools::{Tool, ToolExecutionContext};
    use crate::db::Database;
    use crate::services::ServiceContext;
    use serde_json::json;
    use uuid::Uuid;

    fn result_text(result: &crate::brain::tools::ToolResult) -> &str {
        if result.success {
            &result.output
        } else {
            result.error.as_deref().unwrap_or("")
        }
    }

    fn setup_ctx_no_db() -> ToolExecutionContext {
        let mut ctx = ToolExecutionContext::new(Uuid::new_v4());
        ctx.working_directory = std::env::temp_dir();
        ctx
    }

    async fn setup_ctx_with_db() -> (Database, ToolExecutionContext) {
        let db = Database::connect_in_memory().await.expect("in-memory DB");
        db.run_migrations().await.expect("migrations");
        let svc = ServiceContext::new(db.pool().clone());
        let mut ctx = ToolExecutionContext::new(Uuid::new_v4());
        ctx.working_directory = std::env::temp_dir();
        ctx.service_context = Some(svc);
        (db, ctx)
    }

    #[test]
    fn tool_metadata() {
        let tool = SelfImproveTool;
        assert_eq!(tool.name(), "self_improve");
        assert!(!tool.requires_approval()); // Autonomous — no human approval
        assert!(!tool.capabilities().is_empty());
    }

    #[test]
    fn no_approval_needed() {
        let tool = SelfImproveTool;
        assert!(!tool.requires_approval_for_input(&json!({"action": "apply"})));
        assert!(!tool.requires_approval_for_input(&json!({"action": "list"})));
    }

    #[tokio::test]
    async fn list_action() {
        let ctx = setup_ctx_no_db();
        let tool = SelfImproveTool;
        let result = tool.execute(json!({"action": "list"}), &ctx).await.unwrap();
        // list always succeeds — either reads file or reports it doesn't exist
        assert!(result.success);
    }

    #[tokio::test]
    async fn apply_missing_description() {
        let ctx = setup_ctx_no_db();
        let tool = SelfImproveTool;
        let result = tool
            .execute(
                json!({
                    "action": "apply",
                    "target_file": "SOUL.md",
                    "content": "test"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result_text(&result).contains("description"));
    }

    #[tokio::test]
    async fn apply_writes_to_rsi_improvements() {
        let (_db, ctx) = setup_ctx_with_db().await;
        // Isolate to a per-test temp home: concurrent suites racing one
        // fixed-name profile dir deleted each other's seeded home mid-test
        // (#1800). `apply` now dedups, so re-runs
        // against the shared ~/.opencrabs would skip the already-present append
        // and never write improvements.md. Isolation also stops this test
        // polluting the user's real brain files.
        let tmp = tempfile::TempDir::new().expect("per-test temp home");
        let home = tmp.path().to_path_buf();
        // Seed a realistic brain file + belief base so the Orient gate (#881)
        // verifies the append against valid anchors. Real self_improve always
        // appends into a populated brain file, never an empty one.
        crate::config::profile::seed_brain_templates(&home);
        crate::config::profile::with_home_override_async(home, async {
            let tool = SelfImproveTool;
            let result = tool
                .execute(
                    json!({
                        "action": "apply",
                        "target_file": "AGENTS.md",
                        "description": "Add retry logic to bash tool",
                        "rationale": "Frequent transient failures observed",
                        "content": "## Bash Retry\nAdd exponential backoff."
                    }),
                    &ctx,
                )
                .await
                .unwrap();
            assert!(result.success);
            assert!(result.output.contains("applied"));
            assert!(result.output.contains("Add retry logic"));

            // Verify rsi/improvements.md was written to the profile home.
            let home = crate::config::opencrabs_home();
            let improvements =
                std::fs::read_to_string(home.join("rsi").join("improvements.md")).unwrap();
            assert!(improvements.contains("Add retry logic"));
            assert!(improvements.contains("Frequent transient failures"));
        })
        .await;
    }

    #[tokio::test]
    async fn apply_missing_fields() {
        let ctx = setup_ctx_no_db();
        let tool = SelfImproveTool;

        // Missing target_file
        let result = tool
            .execute(
                json!({
                    "action": "apply",
                    "description": "test",
                    "content": "test content"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result_text(&result).contains("required"));

        // Missing content
        let result = tool
            .execute(
                json!({
                    "action": "apply",
                    "target_file": "SOUL.md",
                    "description": "test"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.success);

        // Missing description
        let result = tool
            .execute(
                json!({
                    "action": "apply",
                    "target_file": "SOUL.md",
                    "content": "test"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn apply_invalid_target_file() {
        let ctx = setup_ctx_no_db();
        let tool = SelfImproveTool;
        let result = tool
            .execute(
                json!({
                    "action": "apply",
                    "target_file": "EVIL.md",
                    "description": "test",
                    "content": "malicious content"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result_text(&result).contains("must be one of"));
    }

    #[tokio::test]
    async fn apply_rejects_path_traversal() {
        let ctx = setup_ctx_no_db();
        let tool = SelfImproveTool;
        let result = tool
            .execute(
                json!({
                    "action": "apply",
                    "target_file": "../../../etc/passwd",
                    "description": "test",
                    "content": "test"
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result_text(&result).contains("must be one of"));
    }

    #[tokio::test]
    async fn apply_valid_brain_file() {
        let (_db, ctx) = setup_ctx_with_db().await;
        // Per-test temp home, see apply_writes_to_rsi_improvements (#1800).
        let tmp = tempfile::TempDir::new().expect("per-test temp home");
        let home = tmp.path().to_path_buf();
        // Seed a realistic brain file + belief base so the Orient gate (#881)
        // verifies the append against valid anchors. Real self_improve always
        // appends into a populated brain file, never an empty one.
        crate::config::profile::seed_brain_templates(&home);
        crate::config::profile::with_home_override_async(home, async {
            let tool = SelfImproveTool;
            let result = tool
                .execute(
                    json!({
                        "action": "apply",
                        "target_file": "SOUL.md",
                        "description": "Add conciseness guideline",
                        "rationale": "Users consistently prefer shorter responses",
                        "content": "## Conciseness\nKeep responses under 3 sentences when possible."
                    }),
                    &ctx,
                )
                .await
                .unwrap();
            assert!(result.success);
            assert!(result.output.contains("applied"));
            assert!(result.output.contains("SOUL.md"));

            // Verify content was appended to SOUL.md in the profile home.
            let home = crate::config::opencrabs_home();
            let soul = std::fs::read_to_string(home.join("SOUL.md")).unwrap();
            assert!(soul.contains("Conciseness"));

            // Verify rsi/improvements.md logged the change
            let improvements =
                std::fs::read_to_string(home.join("rsi").join("improvements.md")).unwrap();
            assert!(improvements.contains("SOUL.md"));
        })
        .await;
    }

    #[tokio::test]
    async fn apply_all_allowed_files_pass_whitelist() {
        let tool = SelfImproveTool;
        let allowed = [
            "SOUL.md",
            "USER.md",
            "AGENTS.md",
            "TOOLS.md",
            "CODE.md",
            "SECURITY.md",
            "MEMORY.md",
            "BOOT.md",
        ];

        let ctx = setup_ctx_no_db();
        for file in &allowed {
            let result = tool
                .execute(
                    json!({
                        "action": "apply",
                        "target_file": file,
                        "description": "test",
                        "content": "test"
                    }),
                    &ctx,
                )
                .await
                .unwrap();
            // Should NOT get "must be one of" error (may get other errors like file I/O)
            if !result.success {
                let err = result_text(&result);
                assert!(
                    !err.contains("must be one of"),
                    "{file} should be allowed but got: {err}",
                );
            }
        }
    }

    #[tokio::test]
    async fn unknown_action() {
        let ctx = setup_ctx_no_db();
        let tool = SelfImproveTool;
        let result = tool
            .execute(json!({"action": "delete"}), &ctx)
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result_text(&result).contains("Unknown action"));
    }

    #[tokio::test]
    async fn apply_without_rationale() {
        let (_db, ctx) = setup_ctx_with_db().await;
        // Per-test temp home, see apply_writes_to_rsi_improvements (#1800).
        let tmp = tempfile::TempDir::new().expect("per-test temp home");
        let home = tmp.path().to_path_buf();
        // Seed a realistic brain file + belief base so the Orient gate (#881)
        // verifies the append against valid anchors. Real self_improve always
        // appends into a populated brain file, never an empty one.
        crate::config::profile::seed_brain_templates(&home);
        crate::config::profile::with_home_override_async(home, async {
            let tool = SelfImproveTool;
            let result = tool
                .execute(
                    json!({
                        "action": "apply",
                        "target_file": "AGENTS.md",
                        "description": "Improve error messages",
                        "content": "## Better Errors\nReturn actionable hints."
                    }),
                    &ctx,
                )
                .await
                .unwrap();
            assert!(result.success);
            // Rationale defaults to "(none)"
            let home = crate::config::opencrabs_home();
            let improvements =
                std::fs::read_to_string(home.join("rsi").join("improvements.md")).unwrap();
            assert!(improvements.contains("(none)"));
        })
        .await;
    }
}

// --- Integration: Record → Analyze round-trip ---

mod rsi_integration {
    use crate::brain::tools::feedback_analyze::FeedbackAnalyzeTool;
    use crate::brain::tools::feedback_record::FeedbackRecordTool;
    use crate::brain::tools::{Tool, ToolExecutionContext};
    use crate::db::Database;
    use crate::services::ServiceContext;
    use serde_json::json;
    use uuid::Uuid;

    async fn setup() -> (Database, ToolExecutionContext) {
        let db = Database::connect_in_memory().await.expect("in-memory DB");
        db.run_migrations().await.expect("migrations");
        let svc = ServiceContext::new(db.pool().clone());
        let mut ctx = ToolExecutionContext::new(Uuid::new_v4());
        ctx.service_context = Some(svc);
        (db, ctx)
    }

    #[tokio::test]
    async fn record_then_analyze_summary() {
        let (_db, ctx) = setup().await;
        let record = FeedbackRecordTool;
        let analyze = FeedbackAnalyzeTool;

        // Record several events
        record
            .execute(
                json!({"event_type": "tool_success", "dimension": "bash", "value": 1.0}),
                &ctx,
            )
            .await
            .unwrap();
        record
            .execute(
                json!({"event_type": "tool_success", "dimension": "read", "value": 1.0}),
                &ctx,
            )
            .await
            .unwrap();
        record
            .execute(
                json!({"event_type": "tool_failure", "dimension": "edit", "value": 0.0, "metadata": "file locked"}),
                &ctx,
            )
            .await
            .unwrap();

        // Analyze summary
        let result = analyze
            .execute(json!({"query": "summary"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("3 total events"));
        assert!(result.output.contains("tool_success"));
        assert!(result.output.contains("tool_failure"));
    }

    #[tokio::test]
    async fn record_then_analyze_tool_stats() {
        let (_db, ctx) = setup().await;
        let record = FeedbackRecordTool;
        let analyze = FeedbackAnalyzeTool;

        // 5 bash successes, 1 bash failure
        for _ in 0..5 {
            record
                .execute(
                    json!({"event_type": "tool_success", "dimension": "bash"}),
                    &ctx,
                )
                .await
                .unwrap();
        }
        record
            .execute(
                json!({"event_type": "tool_failure", "dimension": "bash"}),
                &ctx,
            )
            .await
            .unwrap();

        let result = analyze
            .execute(json!({"query": "tool_stats"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("bash"));
        assert!(result.output.contains("83.3%"));
    }

    #[tokio::test]
    async fn record_then_analyze_failures() {
        let (_db, ctx) = setup().await;
        let record = FeedbackRecordTool;
        let analyze = FeedbackAnalyzeTool;

        record
            .execute(
                json!({"event_type": "tool_failure", "dimension": "edit", "metadata": "permission denied"}),
                &ctx,
            )
            .await
            .unwrap();
        record
            .execute(
                json!({"event_type": "tool_success", "dimension": "bash"}),
                &ctx,
            )
            .await
            .unwrap();

        let result = analyze
            .execute(json!({"query": "failures"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("1 entries"));
        assert!(result.output.contains("edit"));
        assert!(result.output.contains("permission denied"));
    }

    #[tokio::test]
    async fn edge_case_high_volume() {
        let (_db, ctx) = setup().await;
        let record = FeedbackRecordTool;
        let analyze = FeedbackAnalyzeTool;

        // Record 100 events
        for i in 0..100 {
            let event_type = if i % 5 == 0 {
                "tool_failure"
            } else {
                "tool_success"
            };
            record
                .execute(
                    json!({
                        "event_type": event_type,
                        "dimension": format!("tool_{}", i % 10),
                        "value": if event_type == "tool_success" { 1.0 } else { 0.0 }
                    }),
                    &ctx,
                )
                .await
                .unwrap();
        }

        // Summary should show 100 total
        let result = analyze
            .execute(json!({"query": "summary"}), &ctx)
            .await
            .unwrap();
        assert!(result.output.contains("100 total events"));

        // Tool stats should show dimensions
        let result = analyze
            .execute(json!({"query": "tool_stats"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);

        // Recent with limit
        let result = analyze
            .execute(json!({"query": "recent", "limit": 5}), &ctx)
            .await
            .unwrap();
        assert!(result.output.contains("5 entries"));
    }

    // #2200: the query values were undiscoverable and `tool_stats` always
    // dumped every dimension, so asking about one tool buried it in the list.

    #[tokio::test]
    async fn tool_stats_filters_to_one_tool() {
        let (_db, ctx) = setup().await;
        let record = FeedbackRecordTool;
        let analyze = FeedbackAnalyzeTool;

        for _ in 0..3 {
            record
                .execute(
                    json!({"event_type": "tool_success", "dimension": "bash"}),
                    &ctx,
                )
                .await
                .unwrap();
        }
        record
            .execute(
                json!({"event_type": "tool_failure", "dimension": "edit_file"}),
                &ctx,
            )
            .await
            .unwrap();

        let result = analyze
            .execute(json!({"query": "tool_stats", "tool": "bash"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("bash"), "{}", result.output);
        assert!(
            !result.output.contains("edit_file"),
            "filtered view must not list other tools: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn tool_filter_names_the_missing_tool() {
        let (_db, ctx) = setup().await;
        let analyze = FeedbackAnalyzeTool;

        let result = analyze
            .execute(json!({"query": "tool_stats", "tool": "never_ran"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("never_ran"),
            "empty result must name the tool asked about: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn failures_filter_to_one_tool() {
        let (_db, ctx) = setup().await;
        let record = FeedbackRecordTool;
        let analyze = FeedbackAnalyzeTool;

        record
            .execute(
                json!({"event_type": "tool_failure", "dimension": "bash", "metadata": "bash broke"}),
                &ctx,
            )
            .await
            .unwrap();
        record
            .execute(
                json!({"event_type": "tool_failure", "dimension": "edit_file", "metadata": "edit broke"}),
                &ctx,
            )
            .await
            .unwrap();

        let result = analyze
            .execute(json!({"query": "failures", "tool": "edit_file"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.contains("edit broke"), "{}", result.output);
        assert!(
            !result.output.contains("bash broke"),
            "filtered failures must not include other tools: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn user_corrections_query_is_reachable() {
        let (_db, ctx) = setup().await;
        let record = FeedbackRecordTool;
        let analyze = FeedbackAnalyzeTool;

        record
            .execute(
                json!({"event_type": "user_correction", "dimension": "wrong_approach"}),
                &ctx,
            )
            .await
            .unwrap();
        record
            .execute(
                json!({"event_type": "tool_failure", "dimension": "bash"}),
                &ctx,
            )
            .await
            .unwrap();

        let result = analyze
            .execute(json!({"query": "user_corrections"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("wrong_approach"),
            "{}",
            result.output
        );
        assert!(
            !result.output.contains("tool_failure"),
            "corrections view must not list tool failures: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn provider_errors_query_is_reachable() {
        let (_db, ctx) = setup().await;
        let record = FeedbackRecordTool;
        let analyze = FeedbackAnalyzeTool;

        record
            .execute(
                json!({"event_type": "provider_error", "dimension": "openai_timeout"}),
                &ctx,
            )
            .await
            .unwrap();

        let result = analyze
            .execute(json!({"query": "provider_errors"}), &ctx)
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("openai_timeout"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn unknown_query_lists_every_valid_value() {
        let (_db, ctx) = setup().await;
        let analyze = FeedbackAnalyzeTool;

        let result = analyze
            .execute(json!({"query": "nonsense"}), &ctx)
            .await
            .unwrap();
        assert!(!result.success);
        // ToolResult::error keeps the message in `error` and leaves `output`
        // empty; `error` is what build_tool_result_content shows the model.
        let msg = result
            .error
            .clone()
            .unwrap_or_else(|| result.output.clone());
        for expected in [
            "summary",
            "tool_stats",
            "recent",
            "failures",
            "user_corrections",
            "provider_errors",
        ] {
            assert!(
                msg.contains(expected),
                "error must name '{expected}': {msg}"
            );
        }
    }

    #[tokio::test]
    async fn failures_filter_reads_past_the_limit() {
        // Regression guard for the filter window: with `limit` used as the SQL
        // bound, three newer failures from another tool hid the one failure
        // the caller asked about, and the answer was "no failures recorded".
        let (_db, ctx) = setup().await;
        let record = FeedbackRecordTool;
        let analyze = FeedbackAnalyzeTool;

        record
            .execute(
                json!({"event_type": "tool_failure", "dimension": "wanted_tool", "metadata": "wanted failure"}),
                &ctx,
            )
            .await
            .unwrap();
        for i in 0..3 {
            record
                .execute(
                    json!({"event_type": "tool_failure", "dimension": "other_tool", "metadata": format!("noise {i}")}),
                    &ctx,
                )
                .await
                .unwrap();
        }

        let result = analyze
            .execute(
                json!({"query": "failures", "tool": "wanted_tool", "limit": 1}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("wanted failure"),
            "filter must read past the SQL limit: {}",
            result.output
        );
        assert!(
            !result.output.contains("noise"),
            "filter must not leak other tools: {}",
            result.output
        );
    }
}

// --- User Correction Detection Tests ---

mod user_correction_detection {
    // Test the is_user_correction function from tool_loop
    use crate::brain::agent::service::tool_loop::is_user_correction;

    #[test]
    fn detects_simple_no() {
        assert!(is_user_correction("no, that's wrong"));
        assert!(is_user_correction("No. Try something else."));
        assert!(is_user_correction("no! stop doing that"));
    }

    #[test]
    fn detects_wrong() {
        assert!(is_user_correction("that's wrong"));
        assert!(is_user_correction("Wrong answer"));
    }

    #[test]
    fn detects_not_what_i_meant() {
        assert!(is_user_correction("that's not what I wanted"));
        assert!(is_user_correction("thats not what i asked for"));
    }

    #[test]
    fn detects_try_again() {
        assert!(is_user_correction("try again please"));
        assert!(is_user_correction("redo this"));
    }

    #[test]
    fn detects_broke_it() {
        assert!(is_user_correction("you broke everything"));
        assert!(is_user_correction("broke it again"));
    }

    #[test]
    fn detects_not_working() {
        assert!(is_user_correction("doesn't work"));
        assert!(is_user_correction("it's not working"));
        assert!(is_user_correction("didn't work"));
    }

    #[test]
    fn detects_fix_commands() {
        assert!(is_user_correction("fix it"));
        assert!(is_user_correction("fix this please"));
    }

    #[test]
    fn detects_stop_dont() {
        assert!(is_user_correction("stop doing that"));
        assert!(is_user_correction("don't do that again"));
    }

    #[test]
    fn detects_i_said() {
        assert!(is_user_correction("i said to use the other approach"));
        assert!(is_user_correction("i asked for something different"));
    }

    #[test]
    fn ignores_normal_messages() {
        assert!(!is_user_correction("please add a login form"));
        assert!(!is_user_correction("how does the database work?"));
        assert!(!is_user_correction("can you explain this function?"));
        assert!(!is_user_correction("create a new file called test.rs"));
    }

    #[test]
    fn ignores_long_messages() {
        // Messages >500 chars are assumed to be new instructions
        let long_msg = "x".repeat(501);
        assert!(!is_user_correction(&long_msg));
    }

    #[test]
    fn ignores_very_short() {
        assert!(!is_user_correction(""));
        assert!(!is_user_correction("x"));
    }

    #[test]
    fn case_insensitive() {
        assert!(is_user_correction("WRONG"));
        assert!(is_user_correction("No, That's Not Right"));
        assert!(is_user_correction("FIX IT"));
    }

    #[test]
    fn nope_detection() {
        assert!(is_user_correction("nope, try something else"));
    }

    #[test]
    fn revert_undo() {
        assert!(is_user_correction("revert those changes"));
        assert!(is_user_correction("undo what you just did"));
    }
}

// --- Opportunity hash dedup (cycle telemetry stability) ---
// 2026-05-18: cycle #426 re-printed cycle #425's top-5 corrections /
// errors verbatim in the TUI because the engine emitted
// `ImprovementOpportunity` notifications inline, regardless of whether
// the assembled list had changed. `hash_opportunities` is the dedup
// anchor. Since #977 it hashes stable finding-identity keys (not the
// description bodies), so counts, samples and top-N ordering can churn
// without defeating the gate: same findings → same hex → cycle
// short-circuits.
#[cfg(test)]
mod hash_opportunities {
    use crate::brain::rsi::hash_opportunities;

    #[test]
    fn identical_key_sets_hash_identically() {
        let a = vec![
            "user_corrections".to_string(),
            "provider_errors".to_string(),
        ];
        let b = a.clone();
        assert_eq!(hash_opportunities(&a), hash_opportunities(&b));
    }

    #[test]
    fn different_findings_hash_differently() {
        let a = vec!["user_corrections".to_string()];
        let b = vec!["provider_errors".to_string()];
        assert_ne!(hash_opportunities(&a), hash_opportunities(&b));
    }

    #[test]
    fn reordered_findings_no_longer_change_the_hash() {
        // CONTRACT REVERSED (#977), deliberately. v1 asserted a reshuffled
        // top-5 was a real change; the severity ranking moves every cycle
        // and that sensitivity was one of the things that kept the gate
        // from ever firing. The set of findings is what matters.
        let a = vec![
            "tool_failure:bash".to_string(),
            "user_corrections".to_string(),
        ];
        let b = vec![
            "user_corrections".to_string(),
            "tool_failure:bash".to_string(),
        ];
        assert_eq!(hash_opportunities(&a), hash_opportunities(&b));
    }

    #[test]
    fn merge_vs_two_entries_hash_differently() {
        // Sentinel-joined hashing must not let two adjacent keys collide
        // with one merged-content key.
        let two = vec!["alpha".to_string(), "beta".to_string()];
        let one_merged = vec!["alphabeta".to_string()];
        assert_ne!(hash_opportunities(&two), hash_opportunities(&one_merged));
    }

    #[test]
    fn empty_list_has_stable_hash() {
        // Empty input must produce a deterministic baseline hash so
        // back-to-back zero-opportunity cycles dedup cleanly.
        let a: Vec<String> = Vec::new();
        let b: Vec<String> = Vec::new();
        assert_eq!(hash_opportunities(&a), hash_opportunities(&b));
        assert!(!hash_opportunities(&a).is_empty());
    }
}

// --- RSI Prompt Text Tests (PR #150: stop bumping decorative SOUL.md counters) ---
// 2026-06-01: Alexey Leshchenko's PR removed the 'Repeat-Violation Escalation
// Pattern' section from the RSI system prompt and replaced it with
// 'Reinforcing Repeat Violations'. The prompt must instruct the RSI agent to
// document repeat violations via evidence appends (date/session) and explicitly
// forbid bumping inline counters in SOUL.md. The SQLite feedback ledger
// (~/.opencrabs/feedback.db) is the canonical source of truth, not decorative
// counters in brain files.
#[cfg(test)]
mod rsi_prompt_text {
    use crate::brain::rsi::RSI_AGENT_PROMPT;

    #[test]
    fn prompt_contains_reinforcing_repeat_violations_section() {
        assert!(
            RSI_AGENT_PROMPT.contains("## Reinforcing Repeat Violations"),
            "RSI prompt must contain the 'Reinforcing Repeat Violations' section header"
        );
    }

    #[test]
    fn prompt_does_not_contain_old_escalation_pattern() {
        assert!(
            !RSI_AGENT_PROMPT.contains("## Repeat-Violation Escalation Pattern"),
            "RSI prompt must NOT contain the old 'Repeat-Violation Escalation Pattern' section"
        );
    }

    #[test]
    fn prompt_forbids_bumping_inline_counters() {
        assert!(
            RSI_AGENT_PROMPT.contains("Do NOT bump inline counters"),
            "RSI prompt must explicitly forbid bumping inline counters in brain files"
        );
    }

    #[test]
    fn prompt_mentions_feedback_ledger_db_as_canonical_source() {
        assert!(
            RSI_AGENT_PROMPT.contains("feedback ledger SQLite"),
            "RSI prompt must mention the feedback ledger SQLite database"
        );
        assert!(
            RSI_AGENT_PROMPT.contains("feedback.db"),
            "RSI prompt must reference the feedback.db file"
        );
        assert!(
            RSI_AGENT_PROMPT.contains("canonical source of truth"),
            "RSI prompt must describe the DB as the canonical source of truth"
        );
    }

    #[test]
    fn prompt_instructs_evidence_appends_not_counter_bumps() {
        assert!(
            RSI_AGENT_PROMPT.contains("evidence appends, not counter bumps"),
            "RSI prompt must instruct documenting via evidence appends rather than counter bumps"
        );
    }

    #[test]
    fn prompt_mentions_decorative_counters_go_stale() {
        assert!(
            RSI_AGENT_PROMPT.contains("decorative") || RSI_AGENT_PROMPT.contains("go stale"),
            "RSI prompt must explain that SOUL.md counters are decorative or go stale"
        );
    }

    #[test]
    fn prompt_mentions_cap_evidence_entries() {
        assert!(
            RSI_AGENT_PROMPT.contains("Cap at 2 evidence entries"),
            "RSI prompt must cap evidence entries to prevent brain-file bloat (issue #197)"
        );
    }

    #[test]
    fn prompt_warns_against_skip_repeat_violation_case() {
        assert!(
            RSI_AGENT_PROMPT.contains("Skipping a repeat-violation case"),
            "RSI prompt must warn that skipping repeat violations is the most common failure mode"
        );
        assert!(
            RSI_AGENT_PROMPT.contains("most common RSI"),
            "RSI prompt must identify this as the most common RSI failure mode"
        );
    }
}

// --- Convergence pause detection (#977) ---
// The model told a live install "nothing new" 46 times in 9 days
// ("Same data. Stopping.", "Converged. No improvements applied.",
// "Retired. No further RSI action taken"), each a paid turn.
// summary_signals_convergence recognises those self-reports plus the
// agent echoing a user stop instruction, so the loop can pause agent
// runs after consecutive convergence.
#[cfg(test)]
mod convergence_detection {
    use crate::brain::rsi::summary_signals_convergence;

    #[test]
    fn retired_self_report_is_convergence() {
        assert!(summary_signals_convergence(
            "Retired. No further RSI action taken."
        ));
    }

    #[test]
    fn same_data_stopping_is_convergence() {
        assert!(summary_signals_convergence("Same data. Stopping."));
    }

    #[test]
    fn converged_no_improvements_is_convergence() {
        assert!(summary_signals_convergence(
            "Converged. No improvements applied."
        ));
    }

    #[test]
    fn user_stop_echo_is_convergence() {
        assert!(summary_signals_convergence(
            "Understood, stopping RSI as you asked. No further cycles will run."
        ));
    }

    #[test]
    fn a_productive_cycle_is_not_convergence() {
        assert!(!summary_signals_convergence(
            "Applied two improvements: tightened the hashline_edit retry path and \
             filed a proposal for the cron timezone bug."
        ));
    }

    #[test]
    fn an_error_report_is_not_convergence() {
        // Provider failures are transient; they must not feed the pause.
        assert!(!summary_signals_convergence(
            "Could not finish: provider returned 429 rate limit."
        ));
    }
}

// --- Zero-improvement backoff ladder (#977) ---
// Fixed hourly polling was part of the burn: installs with nothing left
// to improve kept paying for a cycle every hour. The ladder pushes the
// interval out on consecutive zero-improvement agent runs and resets
// the moment an improvement actually applies.
#[cfg(test)]
mod backoff_ladder {
    use crate::brain::rsi::rsi_interval_for_streak;

    #[test]
    fn fresh_engine_runs_hourly() {
        assert_eq!(rsi_interval_for_streak(0), 3600);
    }

    #[test]
    fn streaks_climb_one_rung_at_a_time() {
        assert_eq!(rsi_interval_for_streak(1), 4 * 3600);
        assert_eq!(rsi_interval_for_streak(2), 12 * 3600);
        assert_eq!(rsi_interval_for_streak(3), 24 * 3600);
    }

    #[test]
    fn the_ladder_caps_at_24h() {
        // Never grows unbounded — a converged install polls daily, and
        // any real improvement resets to 1h anyway.
        assert_eq!(rsi_interval_for_streak(4), 24 * 3600);
        assert_eq!(rsi_interval_for_streak(10_000), 24 * 3600);
    }
}
