//! Behaviour tests for the desktop settings model (#2878).

use opencrabs::config::{Config, ProviderConfig};

use crate::settings_view::{
    RowValue, SPECS, SettingKind, SettingSpec, SettingsSection, WriteOp, advance, credential_rows,
    current_values, pane_index, section_rows, step_number, value_label, write_op,
};

fn spec_of(key: &str) -> &'static SettingSpec {
    SPECS.iter().find(|s| s.key == key).expect("spec exists")
}

fn spec_index(key: &str) -> usize {
    SPECS
        .iter()
        .position(|s| s.key == key)
        .expect("spec exists")
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
        // Every row explains itself: the master switch that used to explain
        // the notification children is gone (#3118), so nothing inherits a
        // blurb from above any more.
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
fn every_row_declares_a_paintable_section() {
    let mut painted = 0;
    for section in SettingsSection::ALL {
        if section == SettingsSection::Credentials {
            continue;
        }
        let rows = section_rows(section, SPECS.len(), 0);
        assert!(
            !rows.is_empty(),
            "{} paints no rows, so the rail would open an empty pane",
            section.label()
        );
        painted += rows.len();
    }
    // No row is orphaned and none is painted twice.
    assert_eq!(painted, SPECS.len());
}

#[test]
fn sections_paint_their_own_rows_only() {
    let notifications = section_rows(SettingsSection::Notifications, SPECS.len(), 0);
    assert_eq!(notifications.len(), 2);
    for index in &notifications {
        assert_eq!(SPECS[*index].group, SettingsSection::Notifications);
    }
    // The approval policy is General, never swept into a neighbouring pane.
    let general = section_rows(SettingsSection::General, SPECS.len(), 0);
    assert_eq!(general, vec![spec_index("approval_policy")]);
}

#[test]
fn credentials_are_the_last_section_only() {
    let creds = section_rows(SettingsSection::Credentials, SPECS.len(), 3);
    assert_eq!(creds, vec![SPECS.len(), SPECS.len() + 1, SPECS.len() + 2]);
    for section in SettingsSection::ALL {
        if section == SettingsSection::Credentials {
            continue;
        }
        let rows = section_rows(section, SPECS.len(), 3);
        assert!(
            rows.iter().all(|row| *row < SPECS.len()),
            "{} must not paint a credential row",
            section.label()
        );
    }
}

#[test]
fn pane_index_tracks_the_cursor_inside_its_section() {
    let memory = section_rows(SettingsSection::Memory, SPECS.len(), 2);
    for (position, row) in memory.iter().enumerate() {
        assert_eq!(
            pane_index(SettingsSection::Memory, *row, SPECS.len(), 2),
            position
        );
    }
    // A credential row scrolls to its own place in the credentials pane.
    assert_eq!(
        pane_index(
            SettingsSection::Credentials,
            SPECS.len() + 1,
            SPECS.len(),
            2
        ),
        1
    );
}

#[test]
fn tab_walks_the_rail_both_ways_and_wraps() {
    assert_eq!(
        SettingsSection::General.step(1),
        SettingsSection::Notifications
    );
    assert_eq!(
        SettingsSection::Credentials.step(1),
        SettingsSection::General
    );
    assert_eq!(
        SettingsSection::General.step(-1),
        SettingsSection::Credentials
    );
    for (position, section) in SettingsSection::ALL.iter().enumerate() {
        assert_eq!(section.position(), position);
    }
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
fn every_keyed_chat_provider_is_listed_without_keeping_its_secret() {
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
    assert_eq!(
        step_number(guard, 0, -1),
        0,
        "zero is the floor, not a wrap"
    );
    assert_eq!(step_number(guard, 7200, 1), 7200);
    assert_eq!(
        step_number(guard, 99999, -1),
        7140,
        "out of range snaps first"
    );
    let sweep = spec_of("sweep_interval_secs");
    assert_eq!(
        step_number(sweep, 10, -1),
        10,
        "the engine minimum is respected"
    );
}

#[test]
fn labels_say_what_the_value_is() {
    assert_eq!(value_label(&RowValue::Bool(true)), "on");
    assert_eq!(value_label(&RowValue::OptBool(None)), "default");
    assert_eq!(value_label(&RowValue::OptBool(Some(false))), "off");
    assert_eq!(value_label(&RowValue::Number(900)), "900");
}

/// The modal against the crab's settings ops (`backend::settings`): the
/// crab's table and the window's rows agree, and a window pointed at
/// another crab reads and writes that crab's files, never this machine's.
mod crab_door {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use axum::Router;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Json, Response};
    use axum::routing::post;
    use opencrabs::backend::settings::{Kind, SETTABLE, SettingsLoad, settable_values};
    use opencrabs::backend::{BackendError, Ctx, Op, Scope, cap};
    use opencrabs::config::Config;
    use opencrabs::config::profile::{with_home_override, with_home_override_async};
    use serde_json::Value;

    use crate::backend_glue::Backend;
    use crate::backend_remote::RemoteCrab;
    use crate::settings_view::{
        RowValue, SPECS, SettingKind, WriteOp, current_values, failure_line, values_from,
        wire_write,
    };

    fn kind_of(kind: SettingKind) -> Kind {
        match kind {
            SettingKind::Toggle => Kind::Bool,
            SettingKind::OptBool => Kind::OptBool,
            SettingKind::Number { .. } => Kind::Number,
            SettingKind::Choice(options) => Kind::Choice(options),
        }
    }

    #[test]
    fn every_row_is_a_key_the_crab_lets_be_set_with_the_same_kind() {
        for spec in SPECS {
            // The window's own preference (#3178) is the one row that never
            // travels: the crab must not list it, or a remote modal would
            // write the rail switch into another machine's config.
            if crate::settings_view::is_window_pref(spec) {
                assert!(
                    !SETTABLE
                        .iter()
                        .any(|(section, key, _)| *section == spec.section && *key == spec.key),
                    "{}.{} is this window's, the crab must not set it",
                    spec.section,
                    spec.key
                );
                continue;
            }
            let found = SETTABLE
                .iter()
                .find(|(section, key, _)| *section == spec.section && *key == spec.key);
            let Some((_, _, kind)) = found else {
                panic!("{}.{} is not in SETTABLE", spec.section, spec.key);
            };
            assert_eq!(*kind, kind_of(spec.kind), "{}.{}", spec.section, spec.key);
        }
        for (section, key, _) in SETTABLE {
            assert!(
                SPECS
                    .iter()
                    .any(|spec| spec.section == *section && spec.key == *key),
                "{section}.{key} is settable but no row paints it"
            );
        }
    }

    #[test]
    fn the_crabs_values_land_on_the_rows_the_window_reads() {
        let mut config = Config::default();
        config.agent.approval_policy = "ask".to_string();
        config.agent.redact_dm = Some(true);
        config.agent.lazy_tools = false;
        config.agent.time_marker_interval_secs = 1200;
        config.memory.sweep_interval_secs = 90;
        config.memory.internal_allowed_in_shared = true;
        assert_eq!(
            values_from(&settable_values(&config)),
            current_values(&config)
        );
    }

    #[test]
    fn a_row_the_crab_did_not_answer_cannot_write() {
        let values = values_from(&[]);
        assert_eq!(values.len(), SPECS.len());
        let guard = SPECS
            .iter()
            .position(|spec| spec.key == "thinking_loop_timeout_secs")
            .expect("spec exists");
        assert_eq!(
            crate::settings_view::write_op(&SPECS[guard], &values[guard]),
            None
        );
    }

    #[test]
    fn every_file_operation_travels_as_its_write() {
        use opencrabs::backend::settings::Write;
        assert_eq!(
            wire_write(WriteOp::SetScalar("1".into())),
            Write::Set("1".into())
        );
        assert_eq!(
            wire_write(WriteOp::SetString("ask".into())),
            Write::SetString("ask".into())
        );
        assert_eq!(wire_write(WriteOp::Remove), Write::Remove);
    }

    #[test]
    fn a_refusal_reads_as_access_and_a_failure_keeps_its_line() {
        let refused = BackendError::Forbidden {
            scope: Scope::Admin,
        };
        assert_eq!(
            failure_line(SettingsLoad::NAME, "config load failed", refused.clone()),
            cap::notice(SettingsLoad::NAME, &refused)
        );
        assert_eq!(
            failure_line(
                SettingsLoad::NAME,
                "config load failed",
                BackendError::Failed("bad toml".into())
            ),
            "config load failed: bad toml"
        );
    }

    /// A stand-in for another crab's gateway: `gui/call` runs the op under
    /// that crab's own home, as the real gateway runs it under its own.
    struct Crab {
        ctx: Ctx,
        home: PathBuf,
    }

    async fn rpc(State(crab): State<Arc<Crab>>, Json(req): Json<Value>) -> Response {
        let id = req["id"].clone();
        let name = req["params"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let params = req["params"]["params"].clone();
        let answer = with_home_override_async(
            crab.home.clone(),
            opencrabs::backend::dispatch_as(&crab.ctx, Scope::ALL, &name, params),
        )
        .await;
        match answer {
            Ok(result) => Json(serde_json::json!({ "jsonrpc": "2.0", "result": result, "id": id }))
                .into_response(),
            Err(err) => {
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": { "code": -32603, "message": err.to_string(), "data": err },
                    "id": id,
                });
                (StatusCode::OK, Json(body)).into_response()
            }
        }
    }

    fn serve(home: &Path) -> (tokio::runtime::Runtime, Arc<RemoteCrab>) {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let crab = Arc::new(Crab {
            ctx: Ctx::new(home.join("opencrabs.db")).with_home(home.to_path_buf()),
            home: home.to_path_buf(),
        });
        let app = Router::new().route("/a2a/v1", post(rpc)).with_state(crab);
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        runtime.spawn(async move { axum::serve(listener, app).await });
        let remote = Arc::new(RemoteCrab::new(
            "other",
            format!("http://{addr}/"),
            Some("t0ken".into()),
            home.join("window.db"),
        ));
        (runtime, remote)
    }

    #[test]
    fn a_remote_crabs_modal_never_touches_this_machines_config() {
        let here = tempfile::tempdir().expect("this machine's home");
        let there = tempfile::tempdir().expect("the other crab's home");
        std::fs::write(
            there.path().join("config.toml"),
            "[agent]\nlazy_tools = true\n\n[memory]\nvector_enabled = false\n",
        )
        .expect("seed the crab's config");
        std::fs::write(there.path().join("keys.toml"), "").expect("seed the crab's keys");
        let (_runtime, crab) = serve(there.path());
        let backend = Backend::Remote(crab);
        let lazy = SPECS
            .iter()
            .position(|spec| spec.key == "lazy_tools")
            .expect("spec exists");

        // Everything the modal does runs with this thread's home pinned to
        // `here`: a read or write that slipped past the backend would land
        // in it.
        with_home_override(here.path().to_path_buf(), || {
            let (values, creds) =
                futures::executor::block_on(crate::settings_view::load(&backend)).expect("open");
            assert_eq!(values[lazy], RowValue::Bool(true), "the crab's value");
            assert!(creds.iter().any(|c| c.section == "providers.anthropic"));
            let written = futures::executor::block_on(crate::settings_view::write(
                &backend,
                &SPECS[lazy],
                &RowValue::Bool(false),
            ))
            .expect("write");
            assert_eq!(written.as_deref(), Some("false"));
            futures::executor::block_on(crate::settings_view::write_credential(
                &backend,
                "providers.anthropic",
                "sk-teste",
            ))
            .expect("write the key");
        });

        for file in ["config.toml", "keys.toml"] {
            assert!(
                !here.path().join(file).exists(),
                "{file} was written on this machine"
            );
        }
        let config = std::fs::read_to_string(there.path().join("config.toml")).expect("config");
        assert!(config.contains("lazy_tools = false"), "{config}");
        let keys = std::fs::read_to_string(there.path().join("keys.toml")).expect("keys");
        assert!(keys.contains("sk-teste"), "the key reached the crab");
    }
}
