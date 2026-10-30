//! Usage Ledger Tests
//!
//! Tests for usage tracking, model name normalization, and stats aggregation.

use crate::db::Database;
use crate::db::repository::usage_ledger::{UsageLedgerRepository, normalize_model_name};

#[tokio::test]
async fn test_record_and_totals() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = UsageLedgerRepository::new(db.pool().clone());

    repo.record("s1", "prov", "sonnet-4-5", 100, 0.05, None, None)
        .await
        .unwrap();
    repo.record("s1", "prov", "sonnet-4-5", 200, 0.10, None, None)
        .await
        .unwrap();
    repo.record("s2", "prov", "opus-4-6", 500, 0.50, None, None)
        .await
        .unwrap();

    let (tokens, cost) = repo.totals().await.unwrap();
    assert_eq!(tokens, 800);
    assert!((cost - 0.65).abs() < 0.001);
}

#[tokio::test]
async fn test_stats_by_model() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = UsageLedgerRepository::new(db.pool().clone());

    repo.record("s1", "prov", "sonnet", 100, 0.05, None, None)
        .await
        .unwrap();
    repo.record("s2", "prov", "opus", 500, 0.50, None, None)
        .await
        .unwrap();
    repo.record("s3", "prov", "sonnet", 200, 0.10, None, None)
        .await
        .unwrap();

    let stats = repo.stats_by_model().await.unwrap();
    assert_eq!(stats.len(), 2);
    // normalize_model_name maps bare "opus" → "opus-4-6" at write time
    assert_eq!(stats[0].model, "opus-4-6");
    assert_eq!(stats[0].total_tokens, 500);
    assert_eq!(stats[1].model, "sonnet-4-6");
    assert_eq!(stats[1].total_tokens, 300);
}

#[tokio::test]
async fn test_stats_by_model_merges_claude_prefix() {
    let db = Database::connect_in_memory()
        .await
        .expect("Failed to create database");
    db.run_migrations().await.expect("Failed to run migrations");
    let repo = UsageLedgerRepository::new(db.pool().clone());

    repo.record("s1", "prov", "claude-opus-4-6", 1000, 1.0, None, None)
        .await
        .unwrap();
    repo.record("s2", "prov", "opus-4-6", 500, 0.50, None, None)
        .await
        .unwrap();
    repo.record("s3", "prov", "claude-sonnet-4-6", 200, 0.10, None, None)
        .await
        .unwrap();

    let stats = repo.stats_by_model().await.unwrap();
    assert_eq!(stats.len(), 2);
    assert_eq!(stats[0].model, "opus-4-6");
    assert_eq!(stats[0].total_tokens, 1500);
    assert_eq!(stats[1].model, "sonnet-4-6");
    assert_eq!(stats[1].total_tokens, 200);
}

#[test]
fn test_normalize_model_name() {
    // #697: a leaked "-@botname" attribution suffix is stripped so the same
    // model does not split into duplicate /usage rows.
    assert_eq!(
        normalize_model_name("qwen3.8-max-preview-@adicrabs_bot"),
        "qwen3.8-max-preview"
    );
    assert_eq!(
        normalize_model_name("qwen3.8-max-preview"),
        "qwen3.8-max-preview"
    );
    assert_eq!(normalize_model_name("opus@somebot"), "opus-4-6");

    // Claude normalization
    assert_eq!(normalize_model_name("claude-opus-4-6"), "opus-4-6");
    assert_eq!(normalize_model_name("claude-sonnet-4-6"), "sonnet-4-6");
    assert_eq!(normalize_model_name("opus"), "opus-4-6");
    assert_eq!(normalize_model_name("sonnet"), "sonnet-4-6");
    assert_eq!(normalize_model_name("haiku"), "haiku-4-5");
    assert_eq!(normalize_model_name("opus-4-6"), "opus-4-6");

    // Qwen 3.6 Plus normalization — all variants → canonical name
    assert_eq!(normalize_model_name("coder-model"), "qwen3.6-plus");
    assert_eq!(normalize_model_name("qwen-3.6-plus"), "qwen3.6-plus");
    assert_eq!(normalize_model_name("qwen3.6-plus"), "qwen3.6-plus");
    assert_eq!(normalize_model_name("qwen/qwen3.6-plus"), "qwen3.6-plus");
    assert_eq!(normalize_model_name("qwen3.6-plus:free"), "qwen3.6-plus");
    assert_eq!(normalize_model_name("qwen3.6-plus-free"), "qwen3.6-plus");
    assert_eq!(
        normalize_model_name("qwen-3.6-plus-thinking"),
        "qwen3.6-plus"
    );
    assert_eq!(
        normalize_model_name("qwen3.6-plus-thinking"),
        "qwen3.6-plus"
    );
    assert_eq!(
        normalize_model_name("qwen/qwen3.6-plus:free"),
        "qwen3.6-plus"
    );
    assert_eq!(
        normalize_model_name("opencode/qwen3.6-plus-free"),
        "qwen3.6-plus"
    );
    assert_eq!(normalize_model_name("qwen3.5-plus"), "qwen3.5-plus");
    assert_eq!(normalize_model_name("qwen-3.5-plus"), "qwen3.5-plus");

    // Provider prefix stripping
    assert_eq!(
        normalize_model_name("openrouter/elephant-model"),
        "elephant-model"
    );
    assert_eq!(
        normalize_model_name("opencode/qwen3.6-plus"),
        "qwen3.6-plus"
    );

    // Lowercase + normalization
    assert_eq!(normalize_model_name("MiniMax-M2.5"), "minimax-m2.5");
    assert_eq!(normalize_model_name("MiniMax-M2.7"), "minimax-m2.7");
    assert_eq!(normalize_model_name("minimax-m2.7"), "minimax-m2.7");
    assert_eq!(normalize_model_name("MiniMax-M3"), "minimax-m3");
    assert_eq!(normalize_model_name("minimax-m3"), "minimax-m3");
    assert_eq!(normalize_model_name("glm-5.1"), "glm-5.1");
    assert_eq!(normalize_model_name("glm-5-turbo"), "glm-5-turbo");
    assert_eq!(normalize_model_name("zhipu"), "glm-5-turbo");
    assert_eq!(normalize_model_name("zai"), "glm-5-turbo");
    assert_eq!(normalize_model_name("mimo-v2-omni-free"), "mimo-v2-omni");
    assert_eq!(normalize_model_name("mimo-v2-pro-free"), "mimo-v2-pro");
    assert_eq!(normalize_model_name("kimi-k2.5"), "kimi-k2.5");
    // Kimi K3 live model id "k3" folds to the canonical "kimi-k3" pricing key
    assert_eq!(normalize_model_name("k3"), "kimi-k3");
    assert_eq!(normalize_model_name("kimi-k3"), "kimi-k3");
    // Everything lowercased
    assert_eq!(normalize_model_name("GPT-5-mini"), "gpt-5-mini");
}

#[tokio::test]
async fn provider_breakdowns_group_filter_and_handle_unknown() {
    let db = crate::db::Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = crate::db::repository::UsageLedgerRepository::new(db.pool().clone());

    repo.record(
        "s1",
        "custom.opencode-go",
        "deepseek-v4-flash",
        100,
        1.0,
        None,
        None,
    )
    .await
    .unwrap();
    repo.record(
        "s2",
        "custom.nvidia-ds4flash",
        "deepseek-v4-flash",
        200,
        2.0,
        None,
        None,
    )
    .await
    .unwrap();
    repo.record("s3", "custom.opencode-go", "mimo-v2.5", 50, 0.5, None, None)
        .await
        .unwrap();
    repo.record("s4", "", "mystery-model", 10, 0.1, None, None)
        .await
        .unwrap();

    // Per-provider totals, ordered by cost; empty provider -> "unknown".
    let rows = repo.by_provider(None, None).await.unwrap();
    assert_eq!(rows.len(), 3, "rows: {rows:?}");
    assert_eq!(rows[0].0, "custom.nvidia-ds4flash");
    assert_eq!(rows[0].1, 200);
    assert!(rows.iter().any(|r| r.0 == "unknown" && r.1 == 10));

    // Prefix filter is case-insensitive.
    let nvidia = repo.by_provider(None, Some("CUSTOM.NVIDIA")).await.unwrap();
    assert_eq!(nvidia.len(), 1);
    assert_eq!(nvidia[0].2, 2.0);

    // Same model across providers splits correctly.
    let per = repo
        .by_provider_model(None, None, Some("deepseek-v4-flash"))
        .await
        .unwrap();
    assert_eq!(per.len(), 2, "per: {per:?}");
    assert!(per.iter().all(|r| r.1 == "deepseek-v4-flash"));

    // Period filter: nothing recorded before epoch 0 boundary in future.
    let future = repo
        .by_provider(Some(chrono::Utc::now().timestamp() + 3600), None)
        .await
        .unwrap();
    assert!(future.is_empty());
}
