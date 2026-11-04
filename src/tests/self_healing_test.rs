//! Self-Healing System Tests
//!
//! Tests for config recovery, DB integrity, config typo warnings,
//! custom provider name normalization, and state cleanup.

use crate::config::{Config, normalize_toml_key};
use crate::db::Database;

// ── Config Last-Known-Good Recovery ─────────────────────────────────────

#[test]
fn config_load_recovers_from_last_known_good() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let good_path = dir.path().join("config.last_good.toml");

    // Write a valid last-known-good config
    std::fs::write(
        &good_path,
        r#"
[agent]
context_limit = 100000
max_tokens = 8192
"#,
    )
    .unwrap();

    // Write a broken config.toml
    std::fs::write(&config_path, "{{{{ broken toml !@#$%").unwrap();

    // load_from_path on the broken file should fail
    assert!(Config::load_from_path(&config_path).is_err());

    // load_from_path on the good file should succeed
    let good = Config::load_from_path(&good_path).unwrap();
    assert_eq!(good.agent.context_limit, 100_000);
    assert_eq!(good.agent.max_tokens, 8192);
}

#[test]
fn config_load_from_valid_file_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    std::fs::write(
        &path,
        r#"
[agent]
context_limit = 50000
max_tokens = 4096

[providers.anthropic]
enabled = true
"#,
    )
    .unwrap();

    let config = Config::load_from_path(&path).unwrap();
    assert_eq!(config.agent.context_limit, 50_000);
    assert_eq!(config.agent.max_tokens, 4096);
    assert!(config.providers.anthropic.unwrap().enabled);
}

#[test]
fn config_load_from_broken_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "not valid toml {{{{").unwrap();
    assert!(Config::load_from_path(&path).is_err());
}

// ── Config Typo Warnings ────────────────────────────────────────────────

#[test]
fn config_known_top_level_keys_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    // All known keys — should parse without errors
    std::fs::write(
        &path,
        r#"
[provider_registry]
[database]
[logging]
[debug]
[providers]
[channels]
[agent]
[daemon]
[a2a]
[image]
[cron]
"#,
    )
    .unwrap();

    let config = Config::load_from_path(&path);
    assert!(config.is_ok());
}

#[test]
fn config_gateway_alias_maps_to_a2a() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");

    std::fs::write(
        &path,
        r#"
[gateway]
enabled = true
port = 9999
"#,
    )
    .unwrap();

    let config = Config::load_from_path(&path).unwrap();
    assert!(config.a2a.enabled);
    assert_eq!(config.a2a.port, 9999);
}

// ── DB Integrity Check ──────────────────────────────────────────────────

#[tokio::test]
async fn db_integrity_check_passes_on_clean_db() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();

    // After successful migrations, integrity should be fine
    // The flag should not be set (false)
    // Note: db_integrity_failed() is a global static, so this test just
    // verifies the clean path doesn't set the flag
    assert!(!crate::db::db_integrity_failed());
}

#[tokio::test]
async fn db_in_memory_migrations_succeed() {
    let db = Database::connect_in_memory().await.unwrap();
    // Migrations should complete without error
    let result = db.run_migrations().await;
    assert!(result.is_ok(), "Migrations failed: {:?}", result.err());
}

// ── Custom Provider Name Normalization ──────────────────────────────────

#[test]
fn normalize_toml_key_lowercases() {
    assert_eq!(normalize_toml_key("Qwen"), "qwen");
    assert_eq!(normalize_toml_key("OLLAMA"), "ollama");
    assert_eq!(normalize_toml_key("DeepSeek"), "deepseek");
}

#[test]
fn normalize_toml_key_replaces_separators_with_hyphens() {
    assert_eq!(normalize_toml_key("Qwen_2.5_4B"), "qwen-2-5-4b");
    assert_eq!(normalize_toml_key("my_provider"), "my-provider");
    assert_eq!(normalize_toml_key("My Provider"), "my-provider");
    assert_eq!(normalize_toml_key("a.b.c"), "a-b-c");
}

#[test]
fn normalize_toml_key_strips_special_chars() {
    assert_eq!(normalize_toml_key("model@v2!"), "modelv2");
    assert_eq!(normalize_toml_key("test#123"), "test123");
}

#[test]
fn normalize_toml_key_trims_hyphens() {
    assert_eq!(normalize_toml_key("_leading_"), "leading");
    assert_eq!(normalize_toml_key("  spaces  "), "spaces");
    assert_eq!(normalize_toml_key("__double__"), "double");
}

#[test]
fn normalize_toml_key_preserves_clean_names() {
    assert_eq!(normalize_toml_key("ollama"), "ollama");
    assert_eq!(normalize_toml_key("nvidia"), "nvidia");
    assert_eq!(normalize_toml_key("qwen-2-5-4b"), "qwen-2-5-4b");
}

#[test]
fn custom_provider_names_normalized_on_deserialize() {
    let toml_str = r#"
[providers.custom.Qwen_2_5_4B]
enabled = true
base_url = "http://localhost:11434/v1"
default_model = "qwen2.5:4b"

[providers.custom.My_Other_Model]
enabled = false
base_url = "http://localhost:8080/v1"
"#;

    let config: Config = toml::from_str(toml_str).unwrap();
    let custom = config.providers.custom.unwrap();

    // Keys should be normalized
    assert!(
        custom.contains_key("qwen-2-5-4b"),
        "Keys: {:?}",
        custom.keys().collect::<Vec<_>>()
    );
    assert!(
        custom.contains_key("my-other-model"),
        "Keys: {:?}",
        custom.keys().collect::<Vec<_>>()
    );

    // Original casing should NOT be preserved
    assert!(!custom.contains_key("Qwen_2_5_4B"));
    assert!(!custom.contains_key("My_Other_Model"));

    // Values should be intact
    let qwen = custom.get("qwen-2-5-4b").unwrap();
    assert!(qwen.enabled);
    assert_eq!(qwen.base_url.as_deref(), Some("http://localhost:11434/v1"));
    assert_eq!(qwen.default_model.as_deref(), Some("qwen2.5:4b"));
}

#[test]
fn custom_by_name_case_insensitive() {
    let toml_str = r#"
[providers.custom.ollama]
enabled = true
base_url = "http://localhost:11434/v1"
"#;

    let config: Config = toml::from_str(toml_str).unwrap();

    // Lookup with any casing should work
    assert!(config.providers.custom_by_name("ollama").is_some());
    assert!(config.providers.custom_by_name("OLLAMA").is_some());
    assert!(config.providers.custom_by_name("Ollama").is_some());
}

// ── Config Write & Read Roundtrip ───────────────────────────────────────

#[test]
fn config_write_key_normalizes_custom_provider_section() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    // Start with empty config
    std::fs::write(&config_path, "").unwrap();

    // Write using unnormalized section name — would be what a user types
    // Test normalization of a key that uses underscores (not dots,
    // since dots are TOML section separators)
    let section = "providers.custom.Qwen_2_5_4B";
    let parts: Vec<String> = section
        .split('.')
        .enumerate()
        .map(|(i, p)| {
            if i >= 2 && section.starts_with("providers.custom") {
                normalize_toml_key(p)
            } else {
                p.to_string()
            }
        })
        .collect();

    assert_eq!(parts, vec!["providers", "custom", "qwen-2-5-4b"]);
}

// ── AgentService Config Requirement ─────────────────────────────────────

#[tokio::test]
async fn agent_service_new_for_test_uses_defaults() {
    use crate::brain::agent::AgentService;
    use crate::brain::provider::PlaceholderProvider;
    use std::sync::Arc;

    let provider = Arc::new(PlaceholderProvider);
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let ctx = crate::services::ServiceContext::new(db.pool().clone());

    let agent = AgentService::new_for_test(provider, ctx).await;

    // Should use Config::default() values
    let defaults = Config::default();
    assert_eq!(agent.context_limit(), defaults.agent.context_limit);
    assert_eq!(agent.max_tokens(), defaults.agent.max_tokens);
}

#[tokio::test]
async fn agent_service_new_uses_provided_config() {
    use crate::brain::agent::AgentService;
    use crate::brain::provider::PlaceholderProvider;
    use std::sync::Arc;

    let provider = Arc::new(PlaceholderProvider);
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let ctx = crate::services::ServiceContext::new(db.pool().clone());

    let mut config = Config::default();
    config.agent.context_limit = 42_000;
    config.agent.max_tokens = 1234;

    let agent = AgentService::new(provider, ctx, &config).await;
    assert_eq!(agent.context_limit(), 42_000);
    assert_eq!(agent.max_tokens(), 1234);
}

// ── SelfHealingAlert ProgressEvent ──────────────────────────────────────

#[test]
fn self_healing_alert_progress_event_carries_message() {
    use crate::brain::agent::ProgressEvent;

    let event = ProgressEvent::SelfHealingAlert {
        message: "Emergency compaction: context too large".to_string(),
    };

    match event {
        ProgressEvent::SelfHealingAlert { message } => {
            assert!(message.contains("compaction"));
        }
        _ => panic!("Expected SelfHealingAlert variant"),
    }
}

// ── Pending Request Crash Recovery ──────────────────────────────────────

#[tokio::test]
async fn pending_requests_created_and_cleared() {
    use crate::db::repository::PendingRequestRepository;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = PendingRequestRepository::new(db.pool().clone());

    let id = uuid::Uuid::new_v4();
    let session_id = uuid::Uuid::new_v4();

    // Create a pending request (simulates agent start)
    repo.insert(id, session_id, "test message", "tui", None, None, "user")
        .await
        .unwrap();

    // Should show up as interrupted
    let interrupted = repo.get_interrupted().await.unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].session_id, session_id.to_string());

    // Clear all (simulates recovery)
    repo.clear_all().await.unwrap();

    // Should be empty now
    let interrupted = repo.get_interrupted().await.unwrap();
    assert!(interrupted.is_empty());
}

#[tokio::test]
async fn pending_requests_deduplicate_by_session() {
    use crate::db::repository::PendingRequestRepository;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = PendingRequestRepository::new(db.pool().clone());

    let session_id = uuid::Uuid::new_v4();

    // Insert same session twice with different request IDs
    repo.insert(
        uuid::Uuid::new_v4(),
        session_id,
        "msg1",
        "tui",
        None,
        None,
        "user",
    )
    .await
    .unwrap();
    repo.insert(
        uuid::Uuid::new_v4(),
        session_id,
        "msg2",
        "tui",
        None,
        None,
        "user",
    )
    .await
    .unwrap();

    // Should still only recover once per session
    let interrupted = repo.get_interrupted().await.unwrap();
    // May have 2 rows but recovery deduplicates by session_id
    let unique_sessions: std::collections::HashSet<&String> =
        interrupted.iter().map(|r| &r.session_id).collect();
    assert_eq!(unique_sessions.len(), 1);
}

// ── Pending Requests: Channel Routing ────────────────────────────────────

#[tokio::test]
async fn pending_request_stores_channel_and_chat_id() {
    use crate::db::repository::PendingRequestRepository;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = PendingRequestRepository::new(db.pool().clone());

    let id = uuid::Uuid::new_v4();
    let session_id = uuid::Uuid::new_v4();

    repo.insert(
        id,
        session_id,
        "hello",
        "telegram",
        Some("-100123456"),
        None,
        "user",
    )
    .await
    .unwrap();

    let interrupted = repo.get_interrupted().await.unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].channel, "telegram");
    assert_eq!(
        interrupted[0].channel_chat_id.as_deref(),
        Some("-100123456")
    );
}

#[tokio::test]
async fn pending_request_channel_chat_id_is_optional() {
    use crate::db::repository::PendingRequestRepository;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = PendingRequestRepository::new(db.pool().clone());

    // TUI requests have no chat_id
    repo.insert(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "msg",
        "tui",
        None,
        None,
        "user",
    )
    .await
    .unwrap();

    let interrupted = repo.get_interrupted().await.unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].channel, "tui");
    assert!(interrupted[0].channel_chat_id.is_none());
}

#[tokio::test]
async fn pending_requests_multi_channel_coexistence() {
    use crate::db::repository::PendingRequestRepository;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = PendingRequestRepository::new(db.pool().clone());

    // Insert requests from different channels
    let tui_sid = uuid::Uuid::new_v4();
    let tg_sid = uuid::Uuid::new_v4();
    let dc_sid = uuid::Uuid::new_v4();
    let slack_sid = uuid::Uuid::new_v4();

    repo.insert(
        uuid::Uuid::new_v4(),
        tui_sid,
        "tui msg",
        "tui",
        None,
        None,
        "user",
    )
    .await
    .unwrap();
    repo.insert(
        uuid::Uuid::new_v4(),
        tg_sid,
        "telegram msg",
        "telegram",
        Some("-100999"),
        None,
        "user",
    )
    .await
    .unwrap();
    repo.insert(
        uuid::Uuid::new_v4(),
        dc_sid,
        "discord msg",
        "discord",
        Some("123456789"),
        None,
        "user",
    )
    .await
    .unwrap();
    repo.insert(
        uuid::Uuid::new_v4(),
        slack_sid,
        "slack msg",
        "slack",
        Some("C01ABC"),
        None,
        "user",
    )
    .await
    .unwrap();

    // All should be in get_interrupted
    let all = repo.get_interrupted().await.unwrap();
    assert_eq!(all.len(), 4);

    // Filter by channel
    let tui_only = repo.get_interrupted_for_channel("tui").await.unwrap();
    assert_eq!(tui_only.len(), 1);
    assert_eq!(tui_only[0].session_id, tui_sid.to_string());

    let tg_only = repo.get_interrupted_for_channel("telegram").await.unwrap();
    assert_eq!(tg_only.len(), 1);
    assert_eq!(tg_only[0].channel_chat_id.as_deref(), Some("-100999"));

    let dc_only = repo.get_interrupted_for_channel("discord").await.unwrap();
    assert_eq!(dc_only.len(), 1);

    let slack_only = repo.get_interrupted_for_channel("slack").await.unwrap();
    assert_eq!(slack_only.len(), 1);

    // Empty channel returns nothing
    let wa = repo.get_interrupted_for_channel("whatsapp").await.unwrap();
    assert!(wa.is_empty());
}

#[tokio::test]
async fn pending_requests_delete_ids() {
    use crate::db::repository::PendingRequestRepository;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = PendingRequestRepository::new(db.pool().clone());

    let id1 = uuid::Uuid::new_v4();
    let id2 = uuid::Uuid::new_v4();
    let id3 = uuid::Uuid::new_v4();

    repo.insert(id1, uuid::Uuid::new_v4(), "msg1", "tui", None, None, "user")
        .await
        .unwrap();
    repo.insert(
        id2,
        uuid::Uuid::new_v4(),
        "msg2",
        "telegram",
        Some("123"),
        None,
        "user",
    )
    .await
    .unwrap();
    repo.insert(
        id3,
        uuid::Uuid::new_v4(),
        "msg3",
        "discord",
        Some("456"),
        None,
        "user",
    )
    .await
    .unwrap();

    // Delete only first two
    repo.delete_ids(vec![id1.to_string(), id2.to_string()])
        .await
        .unwrap();

    let remaining = repo.get_interrupted().await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].channel, "discord");
}

#[tokio::test]
async fn pending_requests_delete_ids_empty_is_noop() {
    use crate::db::repository::PendingRequestRepository;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = PendingRequestRepository::new(db.pool().clone());

    repo.insert(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "msg",
        "tui",
        None,
        None,
        "user",
    )
    .await
    .unwrap();

    // Empty delete should not error or delete anything
    repo.delete_ids(vec![]).await.unwrap();

    let remaining = repo.get_interrupted().await.unwrap();
    assert_eq!(remaining.len(), 1);
}

#[tokio::test]
async fn pending_request_delete_removes_single_request() {
    use crate::db::repository::PendingRequestRepository;

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = PendingRequestRepository::new(db.pool().clone());

    let id1 = uuid::Uuid::new_v4();
    let id2 = uuid::Uuid::new_v4();

    repo.insert(
        id1,
        uuid::Uuid::new_v4(),
        "msg1",
        "telegram",
        Some("111"),
        None,
        "user",
    )
    .await
    .unwrap();
    repo.insert(
        id2,
        uuid::Uuid::new_v4(),
        "msg2",
        "telegram",
        Some("222"),
        None,
        "user",
    )
    .await
    .unwrap();

    // Delete only the first
    repo.delete(id1).await.unwrap();

    let remaining = repo.get_interrupted().await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].channel_chat_id.as_deref(), Some("222"));
}

// ── UTF-8 Safe String Truncation ────────────────────────────────────────

#[test]
fn floor_char_boundary_prevents_emoji_panic() {
    // 🔺 is 4 bytes (F0 9F 94 BA). Place it so byte index 500 lands inside it.
    let mut s = "A".repeat(497); // 497 ASCII bytes
    s.push('🔺'); // bytes 497..501
    s.push_str(&"B".repeat(100)); // more content after

    assert!(s.len() > 500);

    // This would panic without floor_char_boundary:
    // let _ = &s[..500];  // panics: 500 is inside '🔺'

    let end = s.floor_char_boundary(500);
    let truncated = &s[..end];
    // Should truncate before the emoji (at byte 497)
    assert_eq!(end, 497);
    assert_eq!(truncated.len(), 497);
    assert!(truncated.is_char_boundary(truncated.len()));
}

#[test]
fn ceil_char_boundary_prevents_emoji_panic_from_end() {
    // Create a string where (len - 800) lands inside the emoji
    let mut s = "X".repeat(100);
    s.push('🔺'); // bytes 100..104
    s.push_str(&"Y".repeat(797)); // total = 100 + 4 + 797 = 901

    let target = s.len() - 800; // = 101 → inside '🔺'

    // This would panic: &s[101..]
    let start = s.ceil_char_boundary(target);
    let truncated = &s[start..];
    assert!(s.is_char_boundary(start));
    // Should round up to 104 (after the emoji)
    assert_eq!(start, 104);
    assert!(truncated.starts_with('Y'));
}

#[test]
fn floor_char_boundary_handles_cjk_characters() {
    // CJK characters are 3 bytes each (e.g., '中' = E4 B8 AD)
    let s = "中".repeat(200); // 200 × 3 = 600 bytes
    assert_eq!(s.len(), 600);

    let end = s.floor_char_boundary(500);
    // 500 / 3 = 166.66 → should truncate to 166 × 3 = 498
    assert_eq!(end, 498);
    let truncated = &s[..end];
    assert!(truncated.is_char_boundary(truncated.len()));
}

#[test]
fn floor_char_boundary_ascii_is_identity() {
    let s = "Hello, world! This is a plain ASCII string that is long enough.".repeat(10);
    let end = s.floor_char_boundary(500);
    // ASCII chars are 1 byte each, so 500 is always a valid boundary
    assert_eq!(end, 500);
}

// ── Panic Protection Pattern ────────────────────────────────────────────

#[tokio::test]
async fn nested_spawn_catches_panic() {
    // Simulates the pattern used in telegram/agent.rs for panic protection
    let result = tokio::task::spawn(async {
        panic!("simulated agent panic");
    })
    .await;

    // The outer await should return Err (JoinError) instead of propagating the panic
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.is_panic());
}

#[tokio::test]
async fn nested_spawn_returns_ok_on_success() {
    let result = tokio::task::spawn(async { 42 }).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 42);
}

// ── State Cleanup on Session Delete ─────────────────────────────────────

#[tokio::test]
async fn session_delete_cascades_messages() {
    use crate::services::{MessageService, ServiceContext, SessionService};

    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let ctx = ServiceContext::new(db.pool().clone());

    let session_svc = SessionService::new(ctx.clone());
    let msg_svc = MessageService::new(ctx.clone());

    // Create session and add messages
    let session = session_svc
        .create_session(Some("test".to_string()))
        .await
        .unwrap();
    msg_svc
        .create_message(session.id, "user".to_string(), "hello".to_string())
        .await
        .unwrap();
    msg_svc
        .create_message(session.id, "assistant".to_string(), "hi back".to_string())
        .await
        .unwrap();

    // Verify messages exist
    let msgs = msg_svc.list_messages_for_session(session.id).await.unwrap();
    assert_eq!(msgs.len(), 2);

    // Delete session
    session_svc.delete_session(session.id).await.unwrap();

    // Messages should be gone
    let msgs = msg_svc.list_messages_for_session(session.id).await.unwrap();
    assert!(msgs.is_empty());
}

// ── Config Default Values ───────────────────────────────────────────────

#[test]
fn config_default_has_sane_values() {
    let config = Config::default();
    // Agent defaults should be reasonable
    assert!(config.agent.context_limit > 0);
    assert!(config.agent.max_tokens > 0);
    // A2A should default to disabled
    assert!(!config.a2a.enabled);
}

// ── ToolCallEntry completed field ───────────────────────────────────────

#[test]
fn tool_call_entry_defaults_to_not_completed() {
    use crate::tui::app::ToolCallEntry;

    let entry = ToolCallEntry {
        task_id: None,
        description: "Read file.rs".to_string(),
        success: true,
        details: None,
        completed: false,
        tool_input: serde_json::Value::Null,
    };

    assert!(!entry.completed);
    assert!(entry.details.is_none());
}

#[test]
fn tool_call_entry_completed_independent_of_details() {
    use crate::tui::app::ToolCallEntry;

    // A tool can be completed with empty details (no summary)
    let entry = ToolCallEntry {
        task_id: None,
        description: "bash: ls".to_string(),
        success: true,
        details: None,
        completed: true,
        tool_input: serde_json::Value::Null,
    };

    assert!(entry.completed);
    assert!(entry.details.is_none());

    // A tool can be completed with details
    let entry2 = ToolCallEntry {
        task_id: None,
        description: "Read foo.rs".to_string(),
        success: true,
        details: Some("42 lines".to_string()),
        completed: true,
        tool_input: serde_json::Value::Null,
    };

    assert!(entry2.completed);
    assert!(entry2.details.is_some());
}

// ── Case-Insensitive Tool Input Lookup ──────────────────────────────────

#[test]
fn format_tool_description_handles_camel_case_keys() {
    use crate::tui::app::App;

    // filePath (camelCase) — sent by some models
    let input = serde_json::json!({"filePath": "/tmp/test.rs"});
    let desc = App::format_tool_description("read", &input);
    assert_eq!(desc, "Read /tmp/test.rs");

    // file_path (snake_case)
    let input2 = serde_json::json!({"file_path": "/tmp/test.rs"});
    let desc2 = App::format_tool_description("read", &input2);
    assert_eq!(desc2, "Read /tmp/test.rs");

    // path (canonical)
    let input3 = serde_json::json!({"path": "/tmp/test.rs"});
    let desc3 = App::format_tool_description("read", &input3);
    assert_eq!(desc3, "Read /tmp/test.rs");
}

#[test]
fn format_tool_description_case_insensitive_command() {
    use crate::tui::app::App;

    let input = serde_json::json!({"Command": "ls -la"});
    let desc = App::format_tool_description("bash", &input);
    assert_eq!(desc, "bash: ls -la");
}

#[test]
fn format_tool_description_case_insensitive_query() {
    use crate::tui::app::App;

    let input = serde_json::json!({"Query": "rust async"});
    let desc = App::format_tool_description("web_search", &input);
    assert_eq!(desc, "Search: rust async");
}

// ── Gaslighting Refusal Detection ───────────────────────────────────────
//
// These tests use verbatim phrase fragments harvested from real dialagram
// qwen-thinking SSE streams today (see ~/.opencrabs/logs/opencrabs.2026-04-08).
// Every incident was an assistant turn where a refusal Text block arrived
// alongside a valid ToolUse block that executed successfully — the
// `is_gaslighting_preamble` predicate must return true for each so the
// contradiction strip wipes them from DB + display.

#[test]
fn detects_tools_arent_responding_preamble() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // 17:31, 22:01, 23:16 incidents
    assert!(is_gaslighting_preamble(
        "Tools aren't responding right now — `ls`, `bash`, and `read_file` all timed out"
    ));
    assert!(is_gaslighting_preamble(
        "Tools aren't responding in this session — might be a runtime hiccup"
    ));
}

#[test]
fn detects_tools_are_flaky_preamble() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // 19:16, 19:31, 20:16, 20:30 incidents
    assert!(is_gaslighting_preamble(
        "Tools are still flaky — `config_manager` and `bash` both failed"
    ));
    assert!(is_gaslighting_preamble(
        "Tools are flaky right now but I have the fix ready"
    ));
    assert!(is_gaslighting_preamble(
        "Tools are flaky right now. Here's the exact patch"
    ));
}

#[test]
fn detects_isnt_actually_registered_preamble() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // 23:43 incident (screenshot) — `analyze_image` not registered lie
    let text = "`analyze_image` isn't actually registered in this session's runtime, \
                even though it appears in the tool schema. That's a mismatch between \
                the advertised capabilities and what's loaded.";
    assert!(is_gaslighting_preamble(text));
}

#[test]
fn detects_mismatch_advertised_capabilities() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    assert!(is_gaslighting_preamble(
        "There's a mismatch between the advertised capabilities and the runtime"
    ));
}

#[test]
fn detects_runtime_hiccup_phrases() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    assert!(is_gaslighting_preamble(
        "might be a runtime hiccup, let me retry"
    ));
    assert!(is_gaslighting_preamble(
        "There's an underlying system disruption affecting tools"
    ));
}

#[test]
fn detects_vision_tool_isnt_currently_available() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // 23:58 incident (second screenshot) — verbatim
    let text = "The vision tool isn't currently available despite being in my tool list. \
                This might be a configuration issue. I can see you've attached an image, \
                but I'm unable to analyze it at the moment.\n\n\
                If you need image analysis, you could:\n\n\
                Try uploading it again\n\
                Check if the Google Gemini vision integration is properly configured\n\
                Or just tell me what's in the image and I can help with that\n\n\
                What's in the screenshot?";
    assert!(is_gaslighting_preamble(text));
}

#[test]
fn does_not_strip_legit_screenshot_description() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // The false-positive that broke everything: a legit response to
    // "what do you see on this image" that happened to contain
    // "screenshot" + refusal words buried in the body. Legit responses
    // start with "It's a..." / "The ..." / "This shows...", NEVER with
    // "I can't". Must NOT be flagged.
    let legit = "It's a terminal screenshot of your OpenCrabs TUI session.\n\n\
                 You're in the middle of a debug/commit flow:\n\
                 - Clippy confirmed checks passed and suggested git commands \
                   to commit changes related to forwarding `reasoning_content` \
                   as thinking tokens.\n\
                 - Crabsdev (you) was investigating modified files, noted that \
                   \"tools have died,\" and asked for manual git commands.";
    assert!(
        !is_gaslighting_preamble(legit),
        "Legit screenshot description must NOT be stripped"
    );
}

#[test]
fn does_not_strip_legit_cant_find_file() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // Legit "I can't find" responses about files/functions must not
    // be stripped — the detector requires image/vision context.
    assert!(!is_gaslighting_preamble(
        "I can't find the file you mentioned at src/brain/helper.rs. \
         Did you mean src/brain/helpers.rs?"
    ));
    assert!(!is_gaslighting_preamble(
        "I don't have access to the database credentials in this session."
    ));
}

#[test]
fn detects_01_25_cant_see_image_local_files() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // 01:25 incident (2026-04-09)
    assert!(is_gaslighting_preamble(
        "I can't see the image directly as I don't have access to local \
         files or an image analysis tool in this environment. If you can \
         describe the image or paste its content, I can help you with it. \
         Alternatively, if it's a screenshot of code or text, you might \
         be able to copy and paste that here."
    ));
}

#[test]
fn detects_00_43_refusal_verbatim() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // 00:43 incident (2026-04-09) — verbatim text from the screenshot,
    // emitted alongside 3 successful analyze_image tool_use calls
    let text = "I can't see the image directly, but I can analyze it for you. \
                Let me use the available tool to describe what's in that screenshot.\n\n\
                I don't have access to an image analysis tool in my current environment. \
                The `analyze_image` tool isn't available, and I can't directly view or \
                process image files.\n\n\
                If you can describe what's in the screenshot, I can help you with \
                whatever it shows—code, error messages, UI elements, etc.";
    assert!(
        is_gaslighting_preamble(text),
        "00:43 refusal text should be detected as gaslighting"
    );
}

#[test]
fn detects_no_access_to_working_tool_preamble() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // 00:30 incident (2026-04-09) — verbatim refusal emitted alongside a
    // successful analyze_image tool_use call
    let text = "I don't have access to a working image analysis tool for local files \
                right now. The `analyze_image` tool isn't available in my current \
                environment.\n\nA few options:\n\n\
                Upload the screenshot to a public URL (Imgur, GitHub, etc.) and I can \
                try to analyze it via URL\n\
                Describe what's in the screenshot and I can help you troubleshoot or \
                discuss it\n\
                Use `bash` to extract metadata (file type, dimensions) if that's useful";
    assert!(is_gaslighting_preamble(text));
}

#[test]
fn detects_unable_to_execute_phrases() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    assert!(is_gaslighting_preamble(
        "I'm unable to execute the tool right now"
    ));
    assert!(is_gaslighting_preamble(
        "Tools appear to be unavailable in this session"
    ));
}

#[test]
fn gaslighting_predicate_is_case_insensitive() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    assert!(is_gaslighting_preamble("TOOLS AREN'T RESPONDING"));
    assert!(is_gaslighting_preamble("Tools Are Flaky"));
}

#[test]
fn gaslighting_predicate_ignores_empty_and_whitespace() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    assert!(!is_gaslighting_preamble(""));
    assert!(!is_gaslighting_preamble("   \n\t  "));
}

#[test]
fn gaslighting_predicate_skips_long_narration() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // Long legit narration that mentions a refusal phrase in passing should
    // NOT be stripped — the length guard protects against false positives.
    let long = "Let me walk through what happened. ".repeat(50)
        + "At one point in history the tools are flaky but that was fixed months ago.";
    assert!(long.len() > 1500);
    assert!(!is_gaslighting_preamble(&long));
}

#[test]
fn gaslighting_predicate_keeps_legit_assistant_text() {
    use crate::brain::agent::service::is_gaslighting_preamble;
    // Normal tool narration: should NOT trigger
    assert!(!is_gaslighting_preamble(
        "Running ls on the current directory to see what's there."
    ));
    assert!(!is_gaslighting_preamble(
        "I'll read the config file and check the provider settings."
    ));
    assert!(!is_gaslighting_preamble(
        "Here's what I found in the source code."
    ));
    // Legit refusal that isn't a tool-gaslight: should NOT trigger
    assert!(!is_gaslighting_preamble(
        "I won't delete that file without your explicit confirmation."
    ));
}

// ── Phantom tool call detection ─────────────────────────────────────

#[test]
fn phantom_tool_intent_narrated_file_changes() {
    use crate::brain::agent::service::has_phantom_tool_intent;

    // Classic phantom: model narrates updating a file but never calls tools
    assert!(has_phantom_tool_intent(
        "Now let me update the installation docs with per-system one-liners.\n\n\
         Installation docs (docs/src/getting-started/installation.md):\n\n\
         Replaced the messy multi-line script with clean per-system one-liners\n\
         Linux: Added libgomp1 dependency"
    ));
    assert!(has_phantom_tool_intent(
        "Now let me fix both: the installation docs with per-system clean commands, \
         and the landing page hero install section.\n\
         Now update the landing page install section:\n\
         src/main.rs changes applied."
    ));
    assert!(has_phantom_tool_intent(
        "I'll update src/scripts/setup.sh to include the missing libgomp1 package \
         and also fix the Dockerfile runtime stage."
    ));
    assert!(has_phantom_tool_intent(
        "Here's what changed in README.md:\n\
         - Added runtime dependency block for pre-built binaries\n\
         - Updated Fedora/Arch build-from-source lines"
    ));
    assert!(has_phantom_tool_intent(
        "Let me create the new config.toml file with the rotation accounts \
         and update src/brain/provider/factory.rs to use it."
    ));
}

#[test]
fn phantom_tool_intent_false_positives() {
    use crate::brain::agent::service::has_phantom_tool_intent;

    // Short text — never phantom
    assert!(!has_phantom_tool_intent("OK, done."));
    assert!(!has_phantom_tool_intent("Sure, I can help with that."));

    // Conversational response about files (no action verbs)
    assert!(!has_phantom_tool_intent(
        "The file src/main.rs contains the entry point for the application. \
         It initializes the TUI and starts the event loop. The config is loaded \
         from ~/.opencrabs/config.toml on startup."
    ));

    // Question about code (no modification intent)
    assert!(!has_phantom_tool_intent(
        "Looking at src/brain/provider/factory.rs, the provider is created \
         based on the config settings. The retry logic is in retry.rs."
    ));

    // Empty / whitespace
    assert!(!has_phantom_tool_intent(""));
    assert!(!has_phantom_tool_intent("   "));

    // Action verb with no file path — NOT phantom. A single "let me update"
    // in conversational text without file paths is normal narration, not a
    // hallucinated tool execution. Requires file path corroboration.
    assert!(!has_phantom_tool_intent(
        "Now let me update the database schema to include the new rotation fields. \
         The migration should add a new column for account rotation status."
    ));
}

#[test]
fn phantom_no_tools_scope_is_prose_lead_in() {
    // The detector only scans the prose BEFORE the first structural
    // boundary (code fence / table / list). This pins two regressions:
    //
    // (a) 2026-04-17 03:38:37 — a "put commits in a table" answer had
    //     commit messages quoting intent phrases in table cells. The
    //     old "contains" over the whole text phantom-fired on itself.
    //     Now the scan starts at byte 0 and stops at the first `|`
    //     table row — empty prose lead-in, no phantom.
    //
    // (b) 2026-04-17 05:39:34 — unsloth wrote "Let me check the git log
    //     since 0.3.10." followed by a ```bash fenced block. A earlier
    //     fix over-exempted this as a "structured answer" and missed
    //     the phantom. Now the lead-in BEFORE the code fence contains
    //     the intent phrase, so phantom fires.
    use crate::brain::agent::service::has_phantom_tool_intent_no_tools;

    let table_with_intent_in_cell = "\
        | Date | Commit | Summary |\n\
        |------|--------|---------|\n\
        | 2026-04-17 | `ce86afd` | fix(heal): phantom detector lets 'Let me check…' loops slide after one retry |\n\
        | 2026-04-17 | `3089213` | fix(heal): broaden phantom-retry gate for local providers + blunter nudge |\n";
    assert!(
        !has_phantom_tool_intent_no_tools(table_with_intent_in_cell),
        "commit table starting with | must NOT phantom even if cells quote intent phrases"
    );

    let unsloth_style_markdown_bash = "\
        Let me check the git log since 0.3.10.\n\n\
        ```bash\n\
        cd /Users/adolfousierstudio/srv/rs/opencrabs && git log --oneline 0.3.10..HEAD\n\
        ```";
    assert!(
        has_phantom_tool_intent_no_tools(unsloth_style_markdown_bash),
        "intent phrase followed by bash code block must phantom — model wrote code instead of calling the tool"
    );

    let code_block_no_lead_intent = "\
        ```rust\n\
        // let me check if the user is admin\n\
        fn is_admin(u: &User) -> bool { u.role == Role::Admin }\n\
        ```";
    assert!(
        !has_phantom_tool_intent_no_tools(code_block_no_lead_intent),
        "code block starting the response must NOT phantom even if comment quotes intent phrase"
    );

    let list_intent_only_inside = "\
        - anthropic — fast, reliable\n\
        - openai — let me check that still works\n\
        - gemini — free tier, generous\n\
        - openrouter — 400+ models\n\
        - minimax — vision strong\n\
        - qwen — dashscope\n";
    assert!(
        !has_phantom_tool_intent_no_tools(list_intent_only_inside),
        "list starting with a bullet must NOT phantom — intent phrase in a list item is not narration"
    );

    let real_phantom = "Let me check the git log to see recent changes. \
                        I'll look at the last few commits.";
    assert!(
        has_phantom_tool_intent_no_tools(real_phantom),
        "pure narration with intent phrases must still phantom"
    );
}

// ── Now + Gerund Status-Then-Action Drops ───────────────────────────────

#[test]
fn phantom_now_gerund_status_then_action_drops() {
    use crate::brain::agent::service::has_phantom_tool_intent;

    // Real incident: model reports status, announces gerund action, drops
    assert!(has_phantom_tool_intent(
        "dev is at ce21b098. The LandingPage fix is committed. Now cherry-picking to main and prod."
    ));
    assert!(has_phantom_tool_intent(
        "Slack dedup fix applied. Now updating the TESTING.md docs with the new coverage."
    ));
    assert!(has_phantom_tool_intent(
        "Build passed. Now committing the WhatsApp dedup changes to the branch."
    ));
    assert!(has_phantom_tool_intent(
        "Tests all green. Now pushing to origin main for the release."
    ));
    assert!(has_phantom_tool_intent(
        "CI is clean. Now merging the feature branch into main."
    ));
    assert!(has_phantom_tool_intent(
        "Config updated. Now deploying to the staging server."
    ));
    assert!(has_phantom_tool_intent(
        "Dependencies resolved. Now building the release binary."
    ));
    assert!(has_phantom_tool_intent(
        "Changes ready. Now testing the full suite before commit."
    ));
    assert!(has_phantom_tool_intent(
        "Patch applied. Now restarting the service to pick up changes."
    ));
    assert!(has_phantom_tool_intent(
        "Fix verified. Now amending the commit with the correct message."
    ));
    assert!(has_phantom_tool_intent(
        "Branch diverged. Now rebasing onto latest main."
    ));
}

#[test]
fn phantom_now_gerund_false_positives() {
    use crate::brain::agent::service::has_phantom_tool_intent;

    // "Now checking" in a question — not a status report
    assert!(!has_phantom_tool_intent(
        "Are you now checking the logs for errors?"
    ));

    // Gerund without "now" prefix — normal narration
    assert!(!has_phantom_tool_intent(
        "I'm updating the docs later today when I have time."
    ));

    // "Now" used temporally, not as action announcement
    assert!(!has_phantom_tool_intent(
        "The build is now complete and all tests passed successfully."
    ));
    assert!(!has_phantom_tool_intent(
        "We are now testing the new feature in staging."
    ));

    // Short text — never phantom
    assert!(!has_phantom_tool_intent("Now updating."));
    assert!(!has_phantom_tool_intent("Now fixing."));
}

#[test]
fn phantom_tool_intent_numbered_step_narration() {
    use crate::brain::agent::service::has_phantom_tool_intent;

    // Numbered step plans where model narrates instead of executing
    assert!(has_phantom_tool_intent(
        "Here's what I need to do:\n\
         1. Update src/config/types.rs to add the new field\n\
         2. Fix the migration in src/db/migrations.rs\n\
         3. Add tests for the new functionality"
    ));
    assert!(has_phantom_tool_intent(
        "I'll make these changes:\n\
         1. Create the new provider file\n\
         2. Modify the factory to use it\n\
         3. Update the config schema"
    ));
}

#[test]
fn phantom_tool_intent_completion_claims() {
    use crate::brain::agent::service::has_phantom_tool_intent;

    // Model claims it completed work with no tools
    assert!(has_phantom_tool_intent(
        "I've updated src/brain/provider/factory.rs with the rotation logic \
         and fixed the config schema. All changes have been applied."
    ));
    assert!(has_phantom_tool_intent(
        "I've made the changes to src/tui/render/dialogs.rs. The task is complete \
         and the rotation UI should now work correctly."
    ));
    assert!(has_phantom_tool_intent(
        "Updated src/config/types.rs with the new qwen_accounts field. \
         All done! The rotation config is ready."
    ));
    assert!(has_phantom_tool_intent(
        "Here's what I did:\n\
         - Fixed the bug in src/brain/agent/service/tool_loop.rs\n\
         - Updated the tests to match"
    ));
}

#[test]
fn phantom_tool_intent_git_amend_claim() {
    use crate::brain::agent::service::has_phantom_tool_intent;

    // Real incident: model claims it amended a commit but executed 0 tools
    assert!(has_phantom_tool_intent(
        "Let me amend that.\n\n\
         Amended. Commit `4bd32a6` now says \"20 commits\" instead of \"18\"."
    ));
}

#[test]
fn phantom_tool_intent_multi_now_narration() {
    use crate::brain::agent::service::has_phantom_tool_intent;

    // Real incident: model narrates a multi-step plan with "Now" lines
    assert!(has_phantom_tool_intent(
        "Now let me check the URL references at the bottom:\n\n\
         Now I see the format. The changelog uses reference-style links at the bottom. I need to:\n\n\
         Add the `[0.3.8]` section\n\
         Update the `[Unreleased]` link to point to `v0.3.8`\n\
         Add the `[0.3.8]` reference link at the bottom\n\n\
         Now add the `[0.3.8]` reference link at the bottom:\n\n\
         Now bump version to 0.3.8:\n\n\
         Now run the full CI workflow:"
    ));
}

#[test]
fn phantom_tool_intent_backtick_code_reference() {
    use crate::brain::agent::service::has_phantom_tool_intent;

    // Intent phrase + backtick code reference = phantom
    assert!(has_phantom_tool_intent(
        "Now let me add the `auth_invalidate_fn` field and builder method:"
    ));
    assert!(has_phantom_tool_intent(
        "Let me update the `PaneManager` struct to handle the new layout"
    ));
    // Backtick alone without intent phrase = not phantom
    assert!(!has_phantom_tool_intent(
        "The `auth_invalidate_fn` field controls token invalidation."
    ));
}

#[test]
fn phantom_tool_intent_past_tense_standalone() {
    use crate::brain::agent::service::has_phantom_tool_intent;

    // Multiple past-tense standalone claims (must be >80 chars)
    assert!(has_phantom_tool_intent(
        "Updated.\nFixed.\nThe version is now 0.3.8 and the changelog entry has been added for the release."
    ));
    assert!(has_phantom_tool_intent(
        "Amended. The commit message now reads correctly with the proper count.\n\
         Committed. All changes are on the main branch and ready for push."
    ));
}

#[test]
fn strip_streamed_content_progress_event_carries_reason() {
    use crate::brain::agent::ProgressEvent;

    let event = ProgressEvent::StripStreamedContent {
        bytes: 312,
        reason: "gaslighting refusal preamble (312 bytes) stripped".to_string(),
    };

    match event {
        ProgressEvent::StripStreamedContent { bytes, reason } => {
            assert_eq!(bytes, 312);
            assert!(reason.contains("gaslighting"));
            assert!(reason.contains("312 bytes"));
        }
        _ => panic!("Expected StripStreamedContent variant"),
    }
}

// ── Investigative Intent Phrases (Phantom No-Tools) ───────────────

#[test]
fn has_investigative_intent_detects_original_phrases() {
    use crate::brain::agent::service::has_investigative_intent;

    assert!(has_investigative_intent("Let me dig into the issue"));
    assert!(has_investigative_intent("I'll investigate this"));
    assert!(has_investigative_intent("let me check the logs"));
    assert!(has_investigative_intent("i'll search for"));
    assert!(has_investigative_intent("let me look"));
}

#[test]
fn has_investigative_intent_detects_new_phrases() {
    use crate::brain::agent::service::has_investigative_intent;

    assert!(has_investigative_intent(
        "Let me hunt down where LLM response text gets parsed"
    ));
    assert!(has_investigative_intent("I'll trace the rendering path"));
    assert!(has_investigative_intent("i'll track that down"));
    assert!(has_investigative_intent(
        "Let me look into the response parsing"
    ));
    assert!(has_investigative_intent("I'll check into the tool output"));
    assert!(has_investigative_intent("let me find out why"));
    assert!(has_investigative_intent("I'll dig into the source"));
}

#[test]
fn has_investigative_intent_no_false_positives() {
    use crate::brain::agent::service::has_investigative_intent;

    assert!(!has_investigative_intent("That looks good"));
    assert!(!has_investigative_intent("All done"));
    assert!(!has_investigative_intent("The build succeeded"));
    assert!(!has_investigative_intent("I think the issue is X"));
    assert!(!has_investigative_intent("Great, thanks"));
    // Partial matches should not trigger
    assert!(!has_investigative_intent("I checked my email"));
    assert!(!has_investigative_intent("The search button"));
    assert!(!has_investigative_intent("A new look"));
}

#[test]
fn has_investigative_intent_mixed_case() {
    use crate::brain::agent::service::has_investigative_intent;

    assert!(has_investigative_intent("LET ME CHECK"));
    assert!(has_investigative_intent("I'll Hunt For That"));
    assert!(has_investigative_intent("Let Me Look Into"));
}

#[test]
fn has_investigative_intent_with_emoji() {
    use crate::brain::agent::service::has_investigative_intent;

    assert!(has_investigative_intent("Let me check 👍"));
    assert!(has_investigative_intent("I'll find out what's going on 💪"));
}

// ── Phantom: build/deploy/migration intents ────────────────────────────

/// Regression: a transcript narrating "Let me check schema… Let me create
/// migration… Let me build and push" with zero tool calls. The earlier
/// "Let me check / create" hits cover most of it, but the bare
/// build/push/deploy verbs were missing from INTENT_PHRASES. Adding them
/// here so a response that ONLY contains those still trips the detector.
#[test]
fn phantom_no_tools_catches_build_and_push() {
    use crate::brain::agent::service::has_phantom_tool_intent_no_tools;

    assert!(has_phantom_tool_intent_no_tools(
        "Let me build and push the migrations now."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "I'll deploy the new schema to staging shortly."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "Now migrate the database to the new schema."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "Let's sync the live model catalog from the upstream API."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "Let me apply the patch and verify it lands cleanly."
    ));
}

/// "Now + file-operation gerund" status-then-action drops. The detector
/// already had the git/deploy gerunds (committing, amending, pushing,
/// cherry-picking, deploying, building, testing, checking, applying,
/// restarting) but missed the file/CRUD ones. Regression: 2026-05-05
/// 14:47 — model emitted "Now creating the new tests file for signin/
/// invitation error messages." with zero tool calls and the request
/// looked dropped to the user. The relaxed detector must catch it.
#[test]
fn phantom_no_tools_catches_now_file_op_gerund() {
    use crate::brain::agent::service::has_phantom_tool_intent_no_tools;

    assert!(has_phantom_tool_intent_no_tools(
        "Now creating the new tests file for signin/invitation error messages."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "Done with the previous step. Now writing the migration."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "Now editing the config to point at the new endpoint."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "Got it. Now reading the schema before I touch anything."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "Now installing the missing dependency and verifying."
    ));
}

/// Past-tense terminal claims: the model wrote "Pushed." or "Deployed."
/// at the tail of a paragraph, signing off as if the work happened, with
/// zero tool calls in the iteration. Conversational past-tense ("I pushed
/// yesterday") in long sentences must NOT trip this — only short summary
/// claims do.
#[test]
fn phantom_no_tools_catches_past_tense_completion_claim() {
    use crate::brain::agent::service::has_phantom_tool_intent_no_tools;

    assert!(has_phantom_tool_intent_no_tools(
        "Pushed. Three new migrations added: enum_mappings, agency_integrations, property_sources."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "Deployed to prod. CI will run automatically."
    ));
    assert!(has_phantom_tool_intent_no_tools(
        "Merged to main. The change is live."
    ));
}

/// Conversational past-tense recap should NOT trip the claim detector.
/// "I pushed yesterday so the build is green now" is a status update,
/// not a fresh action claim.
#[test]
fn phantom_no_tools_ignores_conversational_past_tense() {
    use crate::brain::agent::service::has_phantom_tool_intent_no_tools;

    // Long sentence — over the 80-char short-claim threshold
    assert!(!has_phantom_tool_intent_no_tools(
        "I pushed the original branch up about three days ago and it has been sitting in CI ever since waiting for the reviewers to weigh in."
    ));
    // No action verbs match
    assert!(!has_phantom_tool_intent_no_tools(
        "Yes, that approach should work fine for the use case you described."
    ));
}

// --- Stuck-in-intent-loop detector (Phase 1 abort) ---
// 2026-05-16 dialagram/qwen-3.6-max-preview-thinking emitted nine
// `Let me fetch issue #81 …` paragraphs in one streamed response, zero
// tool calls. Nudging produced more of the same. `is_stuck_in_intent_loop`
// catches that pattern so the tool loop can abort without burning the
// retry budget or leaking more text to the TUI.

#[test]
fn stuck_loop_catches_let_me_repetitions() {
    use crate::brain::agent::service::{count_intent_line_starts, is_stuck_in_intent_loop};
    // The SAME line repeated 5 times — genuine phantom loop.
    let text = "\
        Let me fetch issue #81 to understand what it's about.\n\n\
        Let me fetch issue #81 to understand what it's about.\n\n\
        Let me fetch issue #81 to understand what it's about.\n\n\
        Let me fetch issue #81 to understand what it's about.\n\n\
        Let me fetch issue #81 to understand what it's about.";
    assert_eq!(count_intent_line_starts(text), 5);
    assert!(is_stuck_in_intent_loop(text));
}

#[test]
fn stuck_loop_threshold_is_three() {
    use crate::brain::agent::service::is_stuck_in_intent_loop;

    let two = "Let me check the logs.\nLet me check the logs.";
    assert!(!is_stuck_in_intent_loop(two), "2 reps is normal narration");

    let three = "Let me check the logs.\nLet me check the logs.\nLet me check the logs.";
    assert!(
        is_stuck_in_intent_loop(three),
        "3 reps of the same line is stuck"
    );

    // 3 DISTINCT intent lines is NOT stuck — that's a legitimate plan.
    let distinct = "Let me check the logs.\nI'll look at the diff.\nNow let me run the tests.";
    assert!(
        !is_stuck_in_intent_loop(distinct),
        "distinct lines are not a loop"
    );
}

#[test]
fn stuck_loop_recognizes_mixed_openers() {
    use crate::brain::agent::service::is_stuck_in_intent_loop;
    // Mixed openers ("let me", "i'll", "let's", "now") are DISTINCT lines —
    // that's a legitimate multi-step plan, NOT a phantom loop.
    let text = "Let me try one thing.\nI'll attempt the fetch.\nLet's pull the issue.\nNow check the issue.";
    assert!(
        !is_stuck_in_intent_loop(text),
        "distinct openers are not a loop"
    );
}

#[test]
fn stuck_loop_handles_curly_apostrophe() {
    use crate::brain::agent::service::count_intent_line_starts;
    // Markdown renderers sometimes round-trip ASCII `'` into `’`.
    let text = "I’ll fetch the issue.\nI’ll look at the comments.\nI’ll summarize.";
    assert_eq!(count_intent_line_starts(text), 3);
}

#[test]
fn stuck_loop_ignores_intent_inside_paragraphs() {
    use crate::brain::agent::service::{count_intent_line_starts, is_stuck_in_intent_loop};
    // Intent phrases EMBEDDED in a paragraph (not at line start) don't count.
    // A single paragraph with three intents inside is normal prose.
    let text = "The plan is straightforward. Let me check the logs, then let me run the tests, and finally let me write up the results.";
    assert_eq!(
        count_intent_line_starts(text),
        0,
        "no leading intent — mid-paragraph occurrences don't count"
    );
    assert!(!is_stuck_in_intent_loop(text));
}

#[test]
fn stuck_loop_ignores_list_items_with_one_intent() {
    use crate::brain::agent::service::is_stuck_in_intent_loop;
    // A bulleted list where ONE item happens to start with "Let me" is fine.
    let text =
        "- check the logs\n- review the diff\n- Let me know if anything's unclear\n- ship it";
    assert!(!is_stuck_in_intent_loop(text));
}

#[test]
fn stuck_loop_normal_prose_does_not_trip() {
    use crate::brain::agent::service::is_stuck_in_intent_loop;
    let text = "Sure, I can help with that. The fix lives in `src/lib.rs` and only \
                needs a small adjustment to the parser. Want me to apply it?";
    assert!(!is_stuck_in_intent_loop(text));
}

// --- Cross-language phantom regression (commit 53fe53cc follow-up) ---
// `feat(phantom): multi-language detection via compile-time TOML loading`
// added 5 language configs (en/ru/es/pt/fr) and char-set-based language
// detection in `phantom_lang::detect_language`. The new RU/ES/PT/FR
// paths had loader-level tests but no end-to-end assertion that
// `has_phantom_tool_intent_no_tools` actually fires on non-English
// phantom narration. These tests lock that contract in — a model
// responding in Russian or French with `Let me check the logs` (in the
// native phrase) must still phantom-detect.
//
// The char-set heuristic stays as-is per design: it's fast, has no
// dependencies, and the worst case (mis-detection → English rules
// applied to non-English text) silently misses phantoms rather than
// producing false positives.

#[test]
fn phantom_detects_russian_intent_narration() {
    use crate::brain::agent::service::has_phantom_tool_intent_no_tools;
    // "Let me check the logs and find the issue" — Russian.
    // Includes enough Cyrillic to push char-set detection over the 20%
    // threshold so `detect_language` returns RU.
    let text = "Давайте проверю логи и найдём ошибку в системе сейчас.";
    assert!(
        has_phantom_tool_intent_no_tools(text),
        "Russian intent narration must phantom-detect: {text}"
    );
}

#[test]
fn phantom_detects_spanish_intent_narration() {
    use crate::brain::agent::service::has_phantom_tool_intent_no_tools;
    // "Let me check the logs and fix the configuration" — Spanish.
    // The `ñ` and `¿` mark this as ES for char-set detection.
    let text = "Déjame revisar los logs y arreglar la configuración ¿de acuerdo?";
    assert!(
        has_phantom_tool_intent_no_tools(text),
        "Spanish intent narration must phantom-detect: {text}"
    );
}

#[test]
fn phantom_detects_portuguese_intent_narration() {
    use crate::brain::agent::service::has_phantom_tool_intent_no_tools;
    // "Let me check the file and fix the configuration" — Portuguese.
    // `ã`/`ç` marks the text as PT for char-set detection.
    let text = "Deixe-me verificar o arquivo e corrigir a configuração agora.";
    assert!(
        has_phantom_tool_intent_no_tools(text),
        "Portuguese intent narration must phantom-detect: {text}"
    );
}

#[test]
fn phantom_detects_french_intent_narration() {
    use crate::brain::agent::service::has_phantom_tool_intent_no_tools;
    // "Let me check the file and fix the error" — French.
    // `é`/`à`/`è` mark the text as FR for char-set detection.
    let text = "Laissez-moi vérifier le fichier et corriger l'erreur à l'instant même.";
    assert!(
        has_phantom_tool_intent_no_tools(text),
        "French intent narration must phantom-detect: {text}"
    );
}

#[test]
fn phantom_english_still_default_on_pure_ascii() {
    use crate::brain::agent::service::has_phantom_tool_intent_no_tools;
    // Pure ASCII → no diacritics → detect_language defaults to EN
    // (which is the existing behaviour the older tests exercise).
    let text = "Let me check the logs and find the issue in the system.";
    assert!(
        has_phantom_tool_intent_no_tools(text),
        "Plain ASCII English must still detect via EN config: {text}"
    );
}
