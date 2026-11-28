//! Reading and writing settings; secrets are write-only.
//!
//! Operations land here as their GUI doors move behind the contract.

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::db::repository::HintDismissalRepository;
use crate::ui_core::hints::DISMISS_DAYS;

use super::door::{self, Door};
use super::{BackendError, Ctx, Op, Scope};

/// The hint ids hidden right now (#3120, #3121).
///
/// The hint engine never reads a database, so the window asks for the set
/// and hands it over as plain state. A failure is carried as its `{e:#}`
/// text; the window reads any failure as "nothing hidden", so a broken read
/// leaves the hints showing rather than blanking them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HintDismissals {}

impl Op for HintDismissals {
    const NAME: &'static str = "settings.hint_dismissals";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<String>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = HintDismissalRepository::new(db.pool().clone());
        repo.active()
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

/// Hide one hint for [`DISMISS_DAYS`].
///
/// The reply is the write's failure, worded for the log, or `None` when it
/// landed. A ledger that cannot be opened is the `Err`: the window has always
/// let that one pass in silence, since the line coming back on the next
/// launch is the safe direction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DismissHint {
    /// The hint's id, as the engine names it.
    pub hint_id: String,
}

impl Op for DismissHint {
    const NAME: &'static str = "settings.dismiss_hint";
    const SCOPE: Scope = Scope::Send;
    type Reply = Option<String>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = HintDismissalRepository::new(db.pool().clone());
        Ok(repo
            .dismiss(&self.hint_id, DISMISS_DAYS)
            .await
            .err()
            .map(|e| format!("hint dismissal write: {e}")))
    }
}

/// What a settable key holds: the shape a write must have before the crab
/// lets it reach the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `true` or `false`, written as a scalar.
    Bool,
    /// `true`, `false`, or the key's absence (the default).
    OptBool,
    /// A non-negative integer, written as a scalar.
    Number,
    /// One of a fixed list, always written as a TOML string.
    Choice(&'static [&'static str]),
}

/// Every config key a surface may read and write through
/// [`SettingsLoad`] and [`SettingWrite`]: `(section, key, kind)`.
///
/// The crab's own list, so a caller cannot name a key it does not hold. No
/// secret lives here: credentials are write-only, through
/// [`CredentialWrite`].
pub const SETTABLE: &[(&str, &str, Kind)] = &[
    (
        "agent",
        "approval_policy",
        Kind::Choice(&["ask", "auto-session", "auto-always"]),
    ),
    ("agent", "notify_on_plan_completed", Kind::Bool),
    ("agent", "notify_on_turn_failed", Kind::Bool),
    ("agent", "redact_sensitive_data", Kind::Bool),
    ("agent", "redact_group", Kind::OptBool),
    ("agent", "redact_dm", Kind::OptBool),
    ("agent", "lazy_tools", Kind::Bool),
    ("agent", "thinking_loop_timeout_secs", Kind::Number),
    ("agent", "time_marker_interval_secs", Kind::Number),
    ("agent", "debug_logs", Kind::Bool),
    ("memory", "vector_enabled", Kind::Bool),
    ("memory", "sweep_interval_secs", Kind::Number),
    ("memory", "backfill_interval_secs", Kind::Number),
    ("memory", "external_allowed_in_shared", Kind::Bool),
    ("memory", "internal_allowed_in_shared", Kind::Bool),
];

/// The kind of `section.key`, or `None` when it is not in [`SETTABLE`].
pub fn settable_kind(section: &str, key: &str) -> Option<Kind> {
    SETTABLE
        .iter()
        .find(|(s, k, _)| *s == section && *k == key)
        .map(|(_, _, kind)| *kind)
}

/// A setting's current value, as the crab's config holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingValue {
    Bool(bool),
    Choice(String),
    OptBool(Option<bool>),
    Number(u64),
}

/// One settable key and its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingEntry {
    pub section: String,
    pub key: String,
    pub value: SettingValue,
}

/// One keyed provider: where its key writes and whether one is configured.
/// The key itself is never read into this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub section: String,
    pub label: String,
    pub configured: bool,
}

/// What [`SettingsLoad`] answers: every settable value, in [`SETTABLE`]
/// order, and every credential slot. Never a secret's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsLoaded {
    pub values: Vec<SettingEntry>,
    pub credentials: Vec<Credential>,
}

/// The value of `section.key` in `config`, or `None` when the key is not
/// one this crab reads.
pub fn setting_value(config: &Config, section: &str, key: &str) -> Option<SettingValue> {
    let a = &config.agent;
    let m = &config.memory;
    Some(match (section, key) {
        ("agent", "approval_policy") => SettingValue::Choice(a.approval_policy.clone()),
        ("agent", "notify_on_plan_completed") => SettingValue::Bool(a.notify_on_plan_completed),
        ("agent", "notify_on_turn_failed") => SettingValue::Bool(a.notify_on_turn_failed),
        ("agent", "redact_sensitive_data") => SettingValue::Bool(a.redact_sensitive_data),
        ("agent", "redact_group") => SettingValue::OptBool(a.redact_group),
        ("agent", "redact_dm") => SettingValue::OptBool(a.redact_dm),
        ("agent", "lazy_tools") => SettingValue::Bool(a.lazy_tools),
        ("agent", "thinking_loop_timeout_secs") => {
            SettingValue::Number(a.thinking_loop_timeout_secs)
        }
        ("agent", "time_marker_interval_secs") => SettingValue::Number(a.time_marker_interval_secs),
        ("agent", "debug_logs") => SettingValue::Bool(a.debug_logs),
        ("memory", "vector_enabled") => SettingValue::Bool(m.vector_enabled),
        ("memory", "sweep_interval_secs") => SettingValue::Number(m.sweep_interval_secs),
        ("memory", "backfill_interval_secs") => SettingValue::Number(m.backfill_interval_secs),
        ("memory", "external_allowed_in_shared") => {
            SettingValue::Bool(m.external_allowed_in_shared)
        }
        ("memory", "internal_allowed_in_shared") => {
            SettingValue::Bool(m.internal_allowed_in_shared)
        }
        _ => return None,
    })
}

/// Every [`SETTABLE`] key's value in `config`, in table order.
pub fn settable_values(config: &Config) -> Vec<SettingEntry> {
    SETTABLE
        .iter()
        .filter_map(|(section, key, _)| {
            setting_value(config, section, key).map(|value| SettingEntry {
                section: (*section).to_string(),
                key: (*key).to_string(),
                value,
            })
        })
        .collect()
}

/// The chat providers that take an API key, in display order, and every
/// custom provider the config declares. CLI subprocess providers carry no
/// key and are not listed.
pub fn credentials(config: &Config) -> Vec<Credential> {
    let p = &config.providers;
    let named: &[(&str, &str, Option<&crate::config::ProviderConfig>)] = &[
        ("providers.anthropic", "Anthropic", p.anthropic.as_ref()),
        ("providers.openai", "OpenAI", p.openai.as_ref()),
        ("providers.openrouter", "OpenRouter", p.openrouter.as_ref()),
        ("providers.minimax", "MiniMax", p.minimax.as_ref()),
        ("providers.zai", "z.ai (GLM)", p.zai.as_ref()),
        ("providers.moonshot", "Moonshot AI", p.moonshot.as_ref()),
        ("providers.xiaomi", "Xiaomi MiMo", p.xiaomi.as_ref()),
        ("providers.gemini", "Google Gemini", p.gemini.as_ref()),
        ("providers.github", "GitHub Copilot", p.github.as_ref()),
        ("providers.bedrock", "AWS Bedrock", p.bedrock.as_ref()),
        ("providers.vertex", "Google Vertex", p.vertex.as_ref()),
        (
            "providers.opencrabs",
            "OpenCrabs (built-in)",
            p.opencrabs.as_ref(),
        ),
        ("providers.qwen", "Qwen", p.qwen.as_ref()),
        ("providers.ollama", "Ollama", p.ollama.as_ref()),
        ("providers.opencode", "OpenCode API", p.opencode.as_ref()),
    ];
    let mut rows: Vec<Credential> = named
        .iter()
        .map(|(section, label, cfg)| Credential {
            section: (*section).to_string(),
            label: (*label).to_string(),
            configured: cfg
                .and_then(|c| c.api_key.as_ref())
                .is_some_and(|k| !k.is_empty()),
        })
        .collect();
    if let Some(customs) = &p.custom {
        for (name, cfg) in customs {
            rows.push(Credential {
                section: format!("providers.custom.{name}"),
                label: format!("Custom: {name}"),
                configured: cfg.api_key.as_ref().is_some_and(|k| !k.is_empty()),
            });
        }
    }
    rows
}

/// Run `work` against the crab's config files on a thread of its own with a
/// main-thread sized stack, under the profile home the op runs in.
///
/// `Config::load` builds the whole config by value and overflows a small
/// stack (see `status::configured_channel_names`); the writes re-validate
/// the same way. The home is task-local, so it is resolved here and pinned
/// again inside the thread.
fn on_config_thread<T: Send>(
    name: &'static str,
    work: impl FnOnce() -> T + Send,
) -> Result<T, BackendError> {
    let home = crate::config::profile::resolve_profile_home();
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("settings-config".into())
            .stack_size(8 * 1024 * 1024)
            .spawn_scoped(scope, move || {
                crate::config::profile::with_home_override(home, work)
            })
            .map_err(|e| BackendError::Failed(format!("{name}: thread: {e}")))?
            .join()
            .map_err(|_| BackendError::Failed(format!("{name}: the config thread panicked")))
    })
}

/// The text a failed config read or write answers with, by the door it
/// came in at.
///
/// The local door keeps the error's own text, as the window has always shown
/// it. A TOML parse error quotes the offending line, and in `keys.toml` that
/// line can be a key, so the remote door answers only that the config could
/// not be `verb` (read, written) and logs where it broke: the file and the
/// line and column, never the text.
pub(super) fn config_failure(name: &str, door: Door, verb: &str, e: &anyhow::Error) -> String {
    match door {
        Door::Local => format!("{e}"),
        Door::Remote => {
            tracing::warn!(
                "{name}: the config could not be {verb} for a remote caller ({})",
                where_it_broke(e)
            );
            format!("the crab's config could not be {verb}; see the crab's log")
        }
    }
}

/// The config files `e` names and the line and column of its parse error,
/// when it has them: what a log may say about a config failure. Nothing
/// else of the error's text is kept, since its excerpt can hold a key.
fn where_it_broke(e: &anyhow::Error) -> String {
    let text = format!("{e:#}");
    let mut parts: Vec<String> = ["keys.toml", "config.toml", "opencrabs.toml"]
        .into_iter()
        .filter(|file| text.contains(file))
        .map(str::to_string)
        .collect();
    let position = text
        .split_once("TOML parse error at line ")
        .and_then(|(_, rest)| rest.lines().next())
        .and_then(|first| first.split_once(", column "))
        .filter(|(line, column)| {
            [line, column]
                .iter()
                .all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        });
    if let Some((line, column)) = position {
        parts.push(format!("line {line}, column {column}"));
    }
    if parts.is_empty() {
        "no file or position in the error".to_string()
    } else {
        parts.join(", ")
    }
}

/// The crab's settings: every [`SETTABLE`] value and every credential slot,
/// read fresh from its config so a hand edit shows up. Never a secret's
/// value: a credential says only whether a key is configured. A config that
/// does not load is the `Err`, carrying the load's own text at the local
/// door and [`config_failure`]'s at the remote one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsLoad {}

impl Op for SettingsLoad {
    const NAME: &'static str = "settings.load";
    const SCOPE: Scope = Scope::Admin;
    type Reply = SettingsLoaded;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let loaded = on_config_thread(Self::NAME, || {
            Config::load().map(|config| SettingsLoaded {
                values: settable_values(&config),
                credentials: credentials(&config),
            })
        })?;
        let door = door::current();
        loaded.map_err(|e| BackendError::Failed(config_failure(Self::NAME, door, "read", &e)))
    }
}

/// What a [`SettingWrite`] does to its key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Write {
    /// `Config::write_key`: the value parses as bool or integer.
    Set(String),
    /// `Config::write_key_string`: always a TOML string.
    SetString(String),
    /// `Config::remove_key`: the absent-key state of an optional bool.
    Remove,
}

/// Whether `write` fits a key of `kind`, or why not.
fn check_write(kind: Kind, write: &Write) -> Result<(), String> {
    let fits = match (kind, write) {
        (Kind::Bool | Kind::OptBool, Write::Set(v)) => v.trim().parse::<bool>().is_ok(),
        (Kind::OptBool, Write::Remove) => true,
        (Kind::Number, Write::Set(v)) => v.trim().parse::<u64>().is_ok(),
        (Kind::Choice(options), Write::SetString(v)) => options.contains(&v.trim()),
        _ => false,
    };
    if fits {
        Ok(())
    } else {
        Err(format!("{write:?} does not fit a {kind:?} setting"))
    }
}

/// Write one [`SETTABLE`] key into the crab's config, through the file's own
/// locked, revalidating path. The crab refuses any key outside the table and
/// any value that does not fit its kind. The reply is the value as written
/// (what `Config::write_key` answers; empty for a string write or a removal),
/// so the surface repaints from the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingWrite {
    pub section: String,
    pub key: String,
    pub write: Write,
}

impl Op for SettingWrite {
    const NAME: &'static str = "settings.write";
    const SCOPE: Scope = Scope::Admin;
    type Reply = String;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let Some(kind) = settable_kind(&self.section, &self.key) else {
            return Err(BackendError::BadParams(format!(
                "{}: {}.{} is not a setting",
                Self::NAME,
                self.section,
                self.key
            )));
        };
        check_write(kind, &self.write)
            .map_err(|why| BackendError::BadParams(format!("{}: {why}", Self::NAME)))?;
        let SettingWrite {
            section,
            key,
            write,
        } = self;
        let door = door::current();
        let written = on_config_thread(Self::NAME, move || match write {
            Write::Set(v) => Config::write_key(&section, &key, &v),
            Write::SetString(v) => {
                Config::write_key_string(&section, &key, &v).map(|()| String::new())
            }
            Write::Remove => Config::remove_key(&section, &key).map(|()| String::new()),
        })?;
        written.map_err(|e| BackendError::Failed(config_failure(Self::NAME, door, "written", &e)))
    }
}

/// Write one provider's API key into the crab's `keys.toml`. Write-only: the
/// reply is `()` and never carries the key. `section` must be one of the
/// credential slots the crab lists (a named provider or an existing custom
/// one), and an empty key is refused: an empty `api_key` is how a provider
/// looks configured but is not.
#[derive(Clone, Serialize, Deserialize)]
pub struct CredentialWrite {
    pub section: String,
    pub api_key: String,
}

/// Written by hand: a derived `Debug` would print the key into any log that
/// prints the op.
impl std::fmt::Debug for CredentialWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialWrite")
            .field("section", &self.section)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl Op for CredentialWrite {
    const NAME: &'static str = "settings.credential_write";
    const SCOPE: Scope = Scope::Admin;
    type Reply = ();

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        if self.api_key.trim().is_empty() {
            return Err(BackendError::BadParams(format!(
                "{}: empty key not written",
                Self::NAME
            )));
        }
        let CredentialWrite { section, api_key } = self;
        let door = door::current();
        on_config_thread(Self::NAME, move || {
            let config = Config::load()
                .map_err(|e| BackendError::Failed(config_failure(Self::NAME, door, "read", &e)))?;
            if !credentials(&config).iter().any(|c| c.section == section) {
                return Err(BackendError::BadParams(format!(
                    "{}: {section} is not a credential slot",
                    Self::NAME
                )));
            }
            Config::write_keys_key(&section, "api_key", &api_key)
                .map_err(|e| BackendError::Failed(config_failure(Self::NAME, door, "written", &e)))
        })?
    }
}

ops! { HintDismissals, DismissHint, SettingsLoad, SettingWrite, CredentialWrite }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};

    #[tokio::test]
    async fn a_dismissed_hint_reads_back_as_hidden() {
        let (_dir, ctx) = crate::backend::ctx::temp_ctx().await;
        assert_eq!(
            HintDismissals {}.run(&ctx).await.expect("read"),
            Vec::<String>::new()
        );
        let written = DismissHint {
            hint_id: "clear".into(),
        }
        .run(&ctx)
        .await
        .expect("the ledger opens");
        assert_eq!(written, None, "the write landed");
        assert_eq!(
            HintDismissals {}.run(&ctx).await.expect("read"),
            vec!["clear".to_string()]
        );
    }

    #[tokio::test]
    async fn hint_dismissals_has_parity() {
        let gateway = Gateway::start().await;
        let answer = parity!(
            gateway,
            DismissHint {
                hint_id: "clear".into(),
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": null }));
        let answer = parity!(gateway, HintDismissals {});
        assert_eq!(answer, serde_json::json!({ "Ok": ["clear"] }));

        forbidden!(gateway, HintDismissals {});
        forbidden!(
            gateway,
            DismissHint {
                hint_id: "refused".into(),
            }
        );
        let answer = parity!(gateway, HintDismissals {});
        assert_eq!(
            answer,
            serde_json::json!({ "Ok": ["clear"] }),
            "a refused dismissal wrote nothing"
        );
    }

    /// A gateway whose home holds a config with the two settable sections
    /// and a keys file with `keys` in it.
    async fn seeded_gateway(keys: &str) -> Gateway {
        let gateway = Gateway::start().await;
        std::fs::write(
            gateway.home().join("config.toml"),
            "[agent]\nlazy_tools = true\n\n[memory]\nvector_enabled = false\n",
        )
        .expect("seed config.toml");
        std::fs::write(gateway.home().join("keys.toml"), keys).expect("seed keys.toml");
        gateway
    }

    fn config_text(gateway: &Gateway) -> String {
        std::fs::read_to_string(gateway.home().join("config.toml")).expect("config.toml")
    }

    fn keys_text(gateway: &Gateway) -> String {
        std::fs::read_to_string(gateway.home().join("keys.toml")).expect("keys.toml")
    }

    fn value_in(answer: &serde_json::Value, key: &str) -> serde_json::Value {
        answer["Ok"]["values"]
            .as_array()
            .expect("a list of values")
            .iter()
            .find(|entry| entry["key"] == key)
            .map(|entry| entry["value"].clone())
            .unwrap_or_else(|| panic!("{key} is listed: {answer}"))
    }

    fn credential_in(answer: &serde_json::Value, section: &str) -> serde_json::Value {
        answer["Ok"]["credentials"]
            .as_array()
            .expect("a list of credentials")
            .iter()
            .find(|row| row["section"] == section)
            .cloned()
            .unwrap_or_else(|| panic!("{section} is listed: {answer}"))
    }

    fn is_bad_params(answer: &serde_json::Value) -> bool {
        answer["Err"]["kind"] == "bad_params"
    }

    #[test]
    fn no_settable_key_is_a_secret() {
        for (section, key, _) in SETTABLE {
            for word in ["api_key", "token", "secret", "password"] {
                assert!(
                    !key.contains(word) && !section.contains(word),
                    "{section}.{key} looks like a secret"
                );
            }
        }
    }

    #[test]
    fn every_settable_key_reads_a_value_of_its_kind() {
        let config = Config::default();
        for (section, key, kind) in SETTABLE {
            let value = setting_value(&config, section, key)
                .unwrap_or_else(|| panic!("{section}.{key} has no reader"));
            let fits = matches!(
                (kind, &value),
                (Kind::Bool, SettingValue::Bool(_))
                    | (Kind::OptBool, SettingValue::OptBool(_))
                    | (Kind::Number, SettingValue::Number(_))
                    | (Kind::Choice(_), SettingValue::Choice(_))
            );
            assert!(fits, "{section}.{key} reads {value:?} for a {kind:?}");
        }
        assert_eq!(settable_values(&config).len(), SETTABLE.len());
    }

    #[test]
    fn a_credential_write_never_prints_its_key() {
        let op = CredentialWrite {
            section: "providers.anthropic".into(),
            api_key: "sk-teste".into(),
        };
        let shown = format!("{op:?}");
        assert!(!shown.contains("sk-teste"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }

    #[tokio::test]
    async fn settings_load_has_parity_and_never_carries_a_secret() {
        let gateway = seeded_gateway("[providers.anthropic]\napi_key = \"sk-teste\"\n").await;
        let answer = parity!(gateway, SettingsLoad {});
        assert!(!answer.to_string().contains("sk-teste"), "{answer}");
        assert_eq!(
            credential_in(&answer, "providers.anthropic")["configured"],
            true,
            "{answer}"
        );
        assert_eq!(
            credential_in(&answer, "providers.openai")["configured"],
            false
        );
        assert_eq!(
            value_in(&answer, "lazy_tools"),
            serde_json::json!({ "Bool": true })
        );
        assert_eq!(
            answer["Ok"]["values"].as_array().map(Vec::len),
            Some(SETTABLE.len())
        );
        forbidden!(gateway, SettingsLoad {});
    }

    #[tokio::test]
    async fn setting_write_has_parity_and_lands_in_the_crabs_config() {
        let gateway = seeded_gateway("").await;
        let answer = parity!(
            gateway,
            SettingWrite {
                section: "agent".into(),
                key: "lazy_tools".into(),
                write: Write::Set("false".into()),
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": "false" }));
        assert!(config_text(&gateway).contains("lazy_tools = false"));

        let answer = parity!(
            gateway,
            SettingWrite {
                section: "agent".into(),
                key: "approval_policy".into(),
                write: Write::SetString("ask".into()),
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": "" }));
        let answer = parity!(
            gateway,
            SettingWrite {
                section: "agent".into(),
                key: "redact_group".into(),
                write: Write::Remove,
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": "" }));

        let loaded = parity!(gateway, SettingsLoad {});
        assert_eq!(
            value_in(&loaded, "lazy_tools"),
            serde_json::json!({ "Bool": false })
        );
        assert_eq!(
            value_in(&loaded, "approval_policy"),
            serde_json::json!({ "Choice": "ask" })
        );

        forbidden!(
            gateway,
            SettingWrite {
                section: "agent".into(),
                key: "lazy_tools".into(),
                write: Write::Set("true".into()),
            }
        );
        assert!(
            config_text(&gateway).contains("lazy_tools = false"),
            "a refused write changed nothing"
        );
    }

    #[tokio::test]
    async fn setting_write_refuses_keys_and_values_outside_the_table() {
        let gateway = seeded_gateway("").await;
        let before = config_text(&gateway);
        for op in [
            SettingWrite {
                section: "providers.anthropic".into(),
                key: "api_key".into(),
                write: Write::SetString("sk-teste".into()),
            },
            SettingWrite {
                section: "agent".into(),
                key: "lazy_tools".into(),
                write: Write::Set("maybe".into()),
            },
            SettingWrite {
                section: "agent".into(),
                key: "lazy_tools".into(),
                write: Write::Remove,
            },
            SettingWrite {
                section: "agent".into(),
                key: "thinking_loop_timeout_secs".into(),
                write: Write::Set("-5".into()),
            },
            SettingWrite {
                section: "agent".into(),
                key: "approval_policy".into(),
                write: Write::SetString("yolo".into()),
            },
            SettingWrite {
                section: "agent".into(),
                key: "approval_policy".into(),
                write: Write::Set("ask".into()),
            },
        ] {
            let shown = format!("{op:?}");
            let answer = parity!(gateway, op);
            assert!(is_bad_params(&answer), "{shown}: {answer}");
        }
        assert_eq!(
            config_text(&gateway),
            before,
            "a refused write wrote nothing"
        );
    }

    #[tokio::test]
    async fn credential_write_has_parity_and_never_echoes_the_key() {
        let gateway = seeded_gateway("").await;
        let answer = parity!(
            gateway,
            CredentialWrite {
                section: "providers.anthropic".into(),
                api_key: "sk-teste".into(),
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": null }));
        assert!(keys_text(&gateway).contains("sk-teste"), "the key landed");

        let loaded = parity!(gateway, SettingsLoad {});
        assert_eq!(
            credential_in(&loaded, "providers.anthropic")["configured"],
            true
        );
        assert!(!loaded.to_string().contains("sk-teste"), "{loaded}");

        forbidden!(
            gateway,
            CredentialWrite {
                section: "providers.openai".into(),
                api_key: "sk-refused".into(),
            }
        );
        assert!(!keys_text(&gateway).contains("sk-refused"));
    }

    /// A keys file a hand edit broke: the unterminated line carries the key.
    const BROKEN_KEYS: &str = "[providers.anthropic]\napi_key = \"sk-partido\n";

    fn broken_keys_error() -> anyhow::Error {
        let parsed = BROKEN_KEYS.parse::<toml_edit::DocumentMut>();
        anyhow::Error::from(parsed.expect_err("the keys file is broken"))
            // `/fixtures/`, not `/tmp/`: the scan reads a POSIX temp literal as live.
            .context("Failed to parse config file: \"/fixtures/crab/keys.toml\"")
    }

    #[test]
    fn a_config_failure_reads_its_own_text_locally_and_nothing_of_it_remotely() {
        let e = broken_keys_error();
        assert_eq!(
            config_failure("settings.load", Door::Local, "read", &e),
            format!("{e}"),
            "the local door keeps the text it always had"
        );
        let remote = config_failure("settings.load", Door::Remote, "read", &e);
        assert_eq!(
            remote,
            "the crab's config could not be read; see the crab's log"
        );
        assert!(!remote.contains("sk-partido"), "{remote}");
    }

    #[test]
    fn the_log_line_of_a_config_failure_names_the_file_and_position_only() {
        let logged = where_it_broke(&broken_keys_error());
        assert!(!logged.contains("sk-partido"), "{logged}");
        assert!(!logged.contains("api_key"), "{logged}");
        assert!(logged.starts_with("keys.toml, line "), "{logged}");
        assert!(logged.contains(", column "), "{logged}");
        assert_eq!(
            where_it_broke(&anyhow::anyhow!("sk-partido")),
            "no file or position in the error"
        );
    }

    #[tokio::test]
    async fn a_broken_keys_file_never_reaches_a_remote_caller() {
        let gateway = seeded_gateway(BROKEN_KEYS).await;
        let op = CredentialWrite {
            section: "providers.anthropic".into(),
            api_key: "sk-novo".into(),
        };

        let remote = door::remote(gateway.local(op.clone())).await;
        assert_eq!(
            remote,
            Err(BackendError::Failed(
                "the crab's config could not be written; see the crab's log".into()
            ))
        );

        let Err(BackendError::Failed(local)) = gateway.local(op).await else {
            panic!("a broken keys file fails the write at the local door too");
        };
        assert!(local.contains("TOML parse error"), "today's text: {local}");
        assert_eq!(keys_text(&gateway), BROKEN_KEYS, "neither door wrote");
    }

    #[tokio::test]
    async fn credential_write_refuses_an_unknown_slot_and_an_empty_key() {
        let gateway = seeded_gateway("").await;
        for op in [
            CredentialWrite {
                section: "providers.custom.nobody".into(),
                api_key: "sk-teste".into(),
            },
            CredentialWrite {
                section: "agent".into(),
                api_key: "sk-teste".into(),
            },
            CredentialWrite {
                section: "providers.anthropic".into(),
                api_key: "   ".into(),
            },
        ] {
            let shown = format!("{op:?}");
            let answer = parity!(gateway, op);
            assert!(is_bad_params(&answer), "{shown}: {answer}");
            assert!(!answer.to_string().contains("sk-teste"), "{answer}");
        }
        assert!(!keys_text(&gateway).contains("sk-teste"));
    }
}
