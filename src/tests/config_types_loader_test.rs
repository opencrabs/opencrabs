use crate::config::Config;
use crate::config::types::*;
use std::fs;
use std::path::PathBuf;
use tempfile::NamedTempFile;

#[test]
fn test_default_config() {
    let config = Config::default();
    // Opt-in (#972): a default config must not present an enabled integration
    // the user never asked for.
    assert!(!config.provider_registry.enabled);
    assert_eq!(config.logging.level, "info");
    assert!(!config.debug.debug_lsp);
    assert!(!config.debug.profiling);
}

#[test]
fn session_notify_key_absent_defaults_to_disabled() {
    // #1802 kill switch: an [agent] section without the key deserializes the
    // flag as false. Default OFF is the whole point: cross-session
    // notification is an explicit operator opt-in, never a silent default.
    let cfg: AgentConfig = serde_json::from_str("{}").expect("empty agent section deserializes");
    assert!(
        !cfg.session_notify_enabled,
        "absent session_notify_enabled must default to disabled"
    );
    assert!(!Config::default().agent.session_notify_enabled);
}

#[test]
fn test_config_validation() {
    let config = Config::default();
    assert!(config.validate().is_ok());
}

#[test]
fn test_config_validation_invalid_log_level() {
    let mut config = Config::default();
    config.logging.level = "invalid".to_string();
    assert!(config.validate().is_err());
}

#[test]
fn test_config_validation_empty_provider_registry_url() {
    let mut config = Config::default();
    // The URL only has to be present when the registry is actually on, and it
    // is off by default now (#972), so enable it to test the invariant.
    config.provider_registry.enabled = true;
    config.provider_registry.base_url = String::new();
    assert!(config.validate().is_err());

    // Disabled, an empty URL is simply unused and must not fail validation.
    config.provider_registry.enabled = false;
    assert!(config.validate().is_ok());
}

#[test]
fn test_config_from_toml() {
    let toml_content = r#"
[database]
path = "/custom/path/db.sqlite"

[logging]
level = "debug"

[debug]
debug_lsp = true
profiling = true

[provider_registry]
enabled = false
        "#;

    let config: Config = toml::from_str(toml_content).unwrap();
    assert_eq!(
        config.database.path,
        PathBuf::from("/custom/path/db.sqlite")
    );
    assert_eq!(config.logging.level, "debug");
    assert!(config.debug.debug_lsp);
    assert!(config.debug.profiling);
    assert!(!config.provider_registry.enabled);
}

#[test]
fn test_config_save_and_load() {
    let temp_file = NamedTempFile::new().unwrap();
    let config = Config::default();

    // Save config
    config.save(temp_file.path()).unwrap();

    // Load config back
    let contents = std::fs::read_to_string(temp_file.path()).unwrap();
    let loaded_config: Config = toml::from_str(&contents).unwrap();

    assert_eq!(loaded_config.logging.level, config.logging.level);
    assert_eq!(
        loaded_config.provider_registry.enabled,
        config.provider_registry.enabled
    );
}

#[test]
fn test_config_from_toml_overrides() {
    let toml_content = r#"
[logging]
level = "trace"

[debug]
debug_lsp = true
profiling = true

[database]
path = "/tmp/test.db"
        "#;

    let config: Config = toml::from_str(toml_content).unwrap();
    assert_eq!(config.logging.level, "trace");
    assert!(config.debug.debug_lsp);
    assert!(config.debug.profiling);
    assert_eq!(config.database.path, PathBuf::from("/tmp/test.db"));
}

#[test]
fn test_provider_config_from_toml() {
    let toml_content = r#"
[providers.anthropic]
enabled = true
api_key = "test-anthropic-key"
default_model = "claude-opus-4-6"

[providers.openai]
enabled = true
api_key = "test-openai-key"
        "#;

    let config: Config = toml::from_str(toml_content).unwrap();

    assert!(config.providers.anthropic.is_some());
    let anthropic = config.providers.anthropic.as_ref().unwrap();
    assert_eq!(anthropic.api_key, Some("test-anthropic-key".to_string()));
    assert_eq!(anthropic.default_model, Some("claude-opus-4-6".to_string()));

    assert!(config.providers.openai.is_some());
    assert_eq!(
        config.providers.openai.as_ref().unwrap().api_key,
        Some("test-openai-key".to_string())
    );
}

#[test]
fn test_system_config_path() {
    let path = Config::system_config_path();
    assert!(path.is_some());
    let path = path.unwrap();
    assert!(path.to_string_lossy().contains("opencrabs"));
    assert!(path.to_string_lossy().ends_with("config.toml"));
}

#[test]
fn test_local_config_path() {
    let path = Config::local_config_path();
    assert_eq!(path, PathBuf::from("./opencrabs.toml"));
}

#[test]
fn test_debug_config_default() {
    let debug = DebugConfig::default();
    assert!(!debug.debug_lsp);
    assert!(!debug.profiling);
}

#[test]
fn test_provider_configs_default() {
    let providers = ProviderConfigs::default();
    assert!(providers.anthropic.is_none());
    assert!(providers.openai.is_none());
    assert!(providers.gemini.is_none());
    assert!(providers.bedrock.is_none());
    assert!(providers.vertex.is_none());
}

#[test]
fn test_database_config_default() {
    let db_config = DatabaseConfig::default();
    assert!(!db_config.path.as_os_str().is_empty());
}

#[test]
fn test_logging_config_default() {
    let logging = LoggingConfig::default();
    assert_eq!(logging.level, "info");
    assert!(logging.file.is_none());
}

#[test]
fn test_agent_config_default() {
    let agent = AgentConfig::default();
    assert_eq!(agent.approval_policy, "auto-always");
    assert_eq!(agent.max_concurrent, 4);
    // #678: debug_logs defaults OFF; enabling is opt-in (flag or config).
    assert!(!agent.debug_logs);
    // #1195 follow-up: isolation defaults OFF after the Aug 24-25 incident;
    // inline is the default, spawn is opt-in.
    assert!(!agent.plan_isolated_execution);
}

#[test]
fn test_agent_config_debug_logs_toml() {
    // #678: a non-technical user (or the agent) flips this in config.toml.
    let on: Config = toml::from_str("[agent]\ndebug_logs = true\n").unwrap();
    assert!(on.agent.debug_logs);
    let off: Config = toml::from_str("[agent]\ndebug_logs = false\n").unwrap();
    assert!(!off.agent.debug_logs);
    // Absent → default OFF (old configs stay valid).
    let absent: Config = toml::from_str("[agent]\nmax_concurrent = 2\n").unwrap();
    assert!(!absent.agent.debug_logs);
}

#[test]
fn test_agent_config_plan_isolated_execution_toml() {
    // #1195 follow-up: isolation defaults OFF; explicit opt-in stays available.
    let on: Config = toml::from_str("[agent]\nplan_isolated_execution = true\n").unwrap();
    assert!(on.agent.plan_isolated_execution);
    // Absent → default OFF (inline by default since the Aug 24-25 incident).
    let absent: Config = toml::from_str("[agent]\nmax_concurrent = 2\n").unwrap();
    assert!(!absent.agent.plan_isolated_execution);
}

#[test]
fn test_agent_config_from_toml() {
    let toml_content = r#"
[agent]
approval_policy = "auto-always"
max_concurrent = 8
        "#;

    let config: Config = toml::from_str(toml_content).unwrap();
    assert_eq!(config.agent.approval_policy, "auto-always");
    assert_eq!(config.agent.max_concurrent, 8);
}

#[test]
fn test_agent_config_defaults_when_absent() {
    // Config without [agent] section should use defaults
    let toml_content = r#"
[logging]
level = "info"
        "#;

    let config: Config = toml::from_str(toml_content).unwrap();
    assert_eq!(config.agent.approval_policy, "auto-always");
    assert_eq!(config.agent.max_concurrent, 4);
}

#[test]
fn test_write_key_creates_and_updates() {
    let dir = tempfile::TempDir::new().unwrap();
    let config_path = dir.path().join("config.toml");

    // Write initial content
    fs::write(&config_path, "[logging]\nlevel = \"info\"\n").unwrap();

    // Use write_key-style logic (can't call write_key directly since it
    // uses system_config_path, but we test the merge logic)
    let content = fs::read_to_string(&config_path).unwrap();
    let mut doc: toml::Value = toml::from_str(&content).unwrap();
    let table = doc.as_table_mut().unwrap();

    // Add a new section
    table.insert(
        "agent".to_string(),
        toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert(
                "approval_policy".to_string(),
                toml::Value::String("auto-session".to_string()),
            );
            m
        }),
    );

    let output = toml::to_string_pretty(&doc).unwrap();
    fs::write(&config_path, &output).unwrap();

    // Verify it round-trips
    let content = fs::read_to_string(&config_path).unwrap();
    let loaded: Config = toml::from_str(&content).unwrap();
    assert_eq!(loaded.agent.approval_policy, "auto-session");
    assert_eq!(loaded.logging.level, "info");
}

#[test]
fn test_config_save_with_agent_section() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut config = Config::default();
    config.agent.approval_policy = "auto-always".to_string();
    config.agent.max_concurrent = 2;

    config.save(temp_file.path()).unwrap();

    let contents = fs::read_to_string(temp_file.path()).unwrap();
    let loaded: Config = toml::from_str(&contents).unwrap();
    assert_eq!(loaded.agent.approval_policy, "auto-always");
    assert_eq!(loaded.agent.max_concurrent, 2);
}

#[test]
fn test_agent_config_default_provider_and_model() {
    let agent = AgentConfig::default();
    assert_eq!(agent.default_provider, None);
    assert_eq!(agent.default_model, None);
}

#[test]
fn test_agent_config_default_provider_and_model_from_toml() {
    let toml_content = r#"
[agent]
default_provider = "xiaomi"
default_model = "mimo-v2.5-pro"
        "#;

    let config: Config = toml::from_str(toml_content).unwrap();
    assert_eq!(config.agent.default_provider, Some("xiaomi".to_string()));
    assert_eq!(
        config.agent.default_model,
        Some("mimo-v2.5-pro".to_string())
    );
}

#[test]
fn test_agent_config_default_provider_only() {
    let toml_content = r#"
[agent]
default_provider = "anthropic"
        "#;

    let config: Config = toml::from_str(toml_content).unwrap();
    assert_eq!(config.agent.default_provider, Some("anthropic".to_string()));
    assert_eq!(config.agent.default_model, None);
}

#[test]
fn test_agent_config_time_marker_interval_secs() {
    let agent = AgentConfig::default();
    assert_eq!(agent.time_marker_interval_secs, 900);

    let toml_content = r#"
[agent]
time_marker_interval_secs = 1200
    "#;
    let config: Config = toml::from_str(toml_content).unwrap();
    assert_eq!(config.agent.time_marker_interval_secs, 1200);

    let toml_disabled = r#"
[agent]
time_marker_interval_secs = 0
    "#;
    let config_disabled: Config = toml::from_str(toml_disabled).unwrap();
    assert_eq!(config_disabled.agent.time_marker_interval_secs, 0);
}

#[test]
fn test_agent_config_save_with_default_provider() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut config = Config::default();
    config.agent.default_provider = Some("xiaomi".to_string());
    config.agent.default_model = Some("mimo-v2.5-pro".to_string());

    config.save(temp_file.path()).unwrap();

    let contents = fs::read_to_string(temp_file.path()).unwrap();
    let loaded: Config = toml::from_str(&contents).unwrap();
    assert_eq!(loaded.agent.default_provider, Some("xiaomi".to_string()));
    assert_eq!(
        loaded.agent.default_model,
        Some("mimo-v2.5-pro".to_string())
    );
}

// ── compaction_notice: the numbered pair is opt-in chrome (#1686) ──

#[test]
fn compaction_notice_defaults_off() {
    assert!(
        !AgentConfig::default().compaction_notice,
        "the numbered pair must not render for a user who never asked for it"
    );
}

#[test]
fn absent_compaction_notice_key_is_off() {
    // The contract that actually ships: a config file with no such key at all
    // has to land off, not merely a Default impl that says so.
    let config: Config = toml::from_str("[agent]\ndefault_model = \"mimo-v2.5-pro\"\n").unwrap();
    assert!(!config.agent.compaction_notice);
}

#[test]
fn compaction_notice_opt_in_is_honoured() {
    let config: Config = toml::from_str("[agent]\ncompaction_notice = true\n").unwrap();
    assert!(config.agent.compaction_notice, "true restores the pair");
}

#[test]
fn compaction_notice_leaves_the_roast_alone() {
    // Two different surfaces. Muting the chrome by default must not silence the
    // model's narration, which is what users forward to friends and the reason
    // silent_compaction defaults false.
    let agent = AgentConfig::default();
    assert!(!agent.compaction_notice, "chrome off");
    assert!(!agent.silent_compaction, "voice still on");
}

#[test]
fn every_compaction_surface_reads_the_flag() {
    // The whole compaction chrome is opt-in (#1686): the two numbered lines
    // AND the header pin. Structural pin only. These arms are closures built
    // inside handle_message, and no test in this repo constructs a Telegram
    // StreamingState, so this proves the source shape rather than behaviour.
    // The containment window is measured from the enclosing `st.lock()` block
    // rather than by bare index order, so a guard that opens elsewhere in the
    // file cannot satisfy it.
    const GUARD: &str = "if Config::current().agent.compaction_notice {";
    const LOCK: &str = "if let Ok(mut s) = st.lock() {";
    const SET: &str = "s.compacting = true;";
    const LIFT: &str = "s.compacting = false;";
    const PROGRESS: &str = include_str!("../channels/telegram/progress.rs");
    const RESUME: &str = include_str!("../channels/telegram/resume.rs");
    for (name, src) in [("progress.rs", PROGRESS), ("resume.rs", RESUME)] {
        assert_eq!(
            src.matches("compacting_flow_line(").count()
                + src.matches("compacted_flow_line(").count(),
            2,
            "{name} renders exactly two numbered flow lines, both gated"
        );
        assert_eq!(
            src.matches(GUARD).count(),
            2,
            "{name} has exactly two gated surfaces"
        );
        assert_eq!(src.matches(SET).count(), 1, "{name} sets the pin once");
        // Two lifts (#3173): the success lift AND the self-heal-alert lift,
        // both unconditional so a failed compaction can never strand a pin.
        assert_eq!(src.matches(LIFT).count(), 2, "{name} lifts the pin twice");

        let set = src.find(SET).expect("pin set");
        let set_block = src[..set].rfind(LOCK).expect("enclosing lock");
        assert!(
            src[set_block..set].contains(GUARD),
            "{name}: the pin set must sit inside the compaction_notice guard"
        );

        let lift = src.find(LIFT).expect("pin lift");
        let lift_block = src[..lift].rfind(LOCK).expect("enclosing lock");
        assert!(
            !src[lift_block..lift].contains(GUARD),
            "{name}: the pin lift must stay unconditional, or a flag flip \
             mid-window strands the pin"
        );
    }
}
