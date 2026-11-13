//! Behaviour tests for the desktop settings model (#2878).

use opencrabs::config::{Config, ProviderConfig};

use crate::settings_view::{
    RowValue, SPECS, SettingKind, SettingSpec, WriteOp, advance, body_index, credential_rows,
    current_values, step_number, value_label, write_op,
};

fn spec_of(key: &str) -> &'static SettingSpec {
    SPECS.iter().find(|s| s.key == key).expect("spec exists")
}

fn value_of(config: &Config, key: &str) -> RowValue {
    let index = SPECS
        .iter()
        .position(|spec| spec.key == key)
        .expect("setting exists");
    current_values(config)
        .into_iter()
        .nth(index)
        .expect("one value per setting")
}

#[test]
fn every_spec_has_a_reader_and_a_real_control() {
    let src = include_str!("settings_view.rs");
    let readers = src
        .split("pub fn current_values")
        .nth(1)
        .and_then(|tail| tail.split("pub fn credential_rows").next())
        .expect("current_values has a body before credential_rows");
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for spec in SPECS {
        assert!(!spec.label.is_empty(), "{} has no label", spec.key);
        assert!(!spec.blurb.is_empty(), "{} has no blurb", spec.key);
        assert!(
            !seen.contains(&(spec.section, spec.key)),
            "{}.{} is listed twice",
            spec.section,
            spec.key
        );
        seen.push((spec.section, spec.key));
        let pair = format!("(\"{}\", \"{}\")", spec.section, spec.key);
        assert!(readers.contains(&pair), "current_values misses {pair}");
        match spec.kind {
            SettingKind::Choice(options) => {
                assert!(!options.is_empty(), "{} cycles nothing", spec.key)
            }
            SettingKind::Number { min, step, max } => {
                assert!(step > 0, "{} cannot step", spec.key);
                assert!(min <= max, "{} has an inverted range", spec.key);
            }
            _ => {}
        }
    }
}

#[test]
fn body_indices_include_section_headers() {
    let memory = SPECS
        .iter()
        .position(|spec| spec.section == "memory")
        .expect("memory section");
    assert_eq!(body_index(0, SPECS.len()), 1);
    assert_eq!(body_index(memory, SPECS.len()), memory + 2);
    assert_eq!(body_index(SPECS.len(), SPECS.len()), SPECS.len() + 3);
    // A failed config read has no bogus setting rows, but a credential row
    // would still follow the credentials header.
    assert_eq!(body_index(0, 0), 1);
}

#[test]
fn values_come_from_the_real_config() {
    let mut config = Config::default();
    config.agent.approval_policy = "auto-always".to_string();
    config.agent.redact_group = Some(false);
    config.agent.thinking_loop_timeout_secs = 900;
    config.memory.vector_enabled = true;
    config.memory.backfill_interval_secs = 120;
    assert_eq!(
        value_of(&config, "approval_policy"),
        RowValue::Choice("auto-always".to_string())
    );
    assert_eq!(
        value_of(&config, "redact_group"),
        RowValue::OptBool(Some(false))
    );
    assert_eq!(
        value_of(&config, "thinking_loop_timeout_secs"),
        RowValue::Number(900)
    );
    assert_eq!(value_of(&config, "vector_enabled"), RowValue::Bool(true));
    assert_eq!(
        value_of(&config, "backfill_interval_secs"),
        RowValue::Number(120)
    );
}

#[test]
fn every_keyed_provider_is_listed_without_keeping_its_secret() {
    let mut config = Config::default();
    config.providers.qwen = Some(ProviderConfig {
        api_key: Some("test-qwen-secret".to_string()),
        ..ProviderConfig::default()
    });
    config.providers.ollama = Some(ProviderConfig {
        api_key: Some("test-ollama-secret".to_string()),
        ..ProviderConfig::default()
    });
    config.providers.opencode = Some(ProviderConfig::default());
    let rows = credential_rows(&config);
    for label in [
        "Anthropic",
        "OpenAI",
        "OpenRouter",
        "MiniMax",
        "z.ai (GLM)",
        "Moonshot AI",
        "Xiaomi MiMo",
        "Google Gemini",
        "GitHub Copilot",
        "AWS Bedrock",
        "Google Vertex",
        "OpenCrabs (built-in)",
        "Qwen",
        "Ollama",
        "OpenCode API",
    ] {
        assert!(rows.iter().any(|row| row.label == label), "missing {label}");
    }
    let qwen = rows.iter().find(|row| row.label == "Qwen").expect("Qwen");
    assert!(qwen.configured);
    assert_eq!(qwen.section, "providers.qwen");
    assert!(!rows.iter().any(|row| row.label.contains("secret")));
}

#[test]
fn writes_map_to_the_right_file_operation() {
    let toggle = spec_of("lazy_tools");
    assert_eq!(
        write_op(toggle, &RowValue::Bool(false)),
        Some(WriteOp::SetScalar("false".to_string()))
    );
    let policy = spec_of("approval_policy");
    assert_eq!(
        write_op(policy, &RowValue::Choice("ask".to_string())),
        Some(WriteOp::SetString("ask".to_string())),
        "a choice always writes as a string, never a parsed scalar"
    );
    let group = spec_of("redact_group");
    assert_eq!(
        write_op(group, &RowValue::OptBool(None)),
        Some(WriteOp::Remove),
        "the default state is the key's absence, not a written value"
    );
    assert_eq!(
        write_op(group, &RowValue::OptBool(Some(true))),
        Some(WriteOp::SetScalar("true".to_string()))
    );
    let guard = spec_of("thinking_loop_timeout_secs");
    assert_eq!(
        write_op(guard, &RowValue::Number(600)),
        Some(WriteOp::SetScalar("600".to_string()))
    );
    assert_eq!(write_op(toggle, &RowValue::Number(1)), None);
}

#[test]
fn enter_advances_every_kind() {
    let toggle = spec_of("lazy_tools");
    assert_eq!(
        advance(toggle, &RowValue::Bool(false)),
        Some(RowValue::Bool(true))
    );
    let policy = spec_of("approval_policy");
    assert_eq!(
        advance(policy, &RowValue::Choice("ask".to_string())),
        Some(RowValue::Choice("auto-session".to_string()))
    );
    assert_eq!(
        advance(policy, &RowValue::Choice("auto-always".to_string())),
        Some(RowValue::Choice("ask".to_string())),
        "the last option wraps to the first"
    );
    assert_eq!(
        advance(policy, &RowValue::Choice("hand-edited".to_string())),
        Some(RowValue::Choice("ask".to_string())),
        "an unknown hand-edited value restarts the cycle"
    );
    let dm = spec_of("redact_dm");
    assert_eq!(
        advance(dm, &RowValue::OptBool(None)),
        Some(RowValue::OptBool(Some(true)))
    );
    assert_eq!(
        advance(dm, &RowValue::OptBool(Some(true))),
        Some(RowValue::OptBool(Some(false)))
    );
    assert_eq!(
        advance(dm, &RowValue::OptBool(Some(false))),
        Some(RowValue::OptBool(None))
    );
}

#[test]
fn number_steps_clamp_to_the_range() {
    let guard = spec_of("thinking_loop_timeout_secs");
    assert_eq!(step_number(guard, 600, 1), 660);
    assert_eq!(step_number(guard, 600, -1), 540);
    assert_eq!(step_number(guard, 0, -1), 0, "zero is the floor, not a wrap");
    assert_eq!(step_number(guard, 7200, 1), 7200);
    assert_eq!(step_number(guard, 99999, -1), 7140, "out of range snaps first");
    let sweep = spec_of("sweep_interval_secs");
    assert_eq!(step_number(sweep, 30, -1), 30, "the minimum is respected");
}

#[test]
fn labels_say_what_the_value_is() {
    assert_eq!(value_label(&RowValue::Bool(true)), "on");
    assert_eq!(value_label(&RowValue::OptBool(None)), "default");
    assert_eq!(value_label(&RowValue::OptBool(Some(false))), "off");
    assert_eq!(value_label(&RowValue::Number(900)), "900");
}
