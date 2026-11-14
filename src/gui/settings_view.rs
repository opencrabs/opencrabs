//! The settings modal's rows (#2878).
//!
//! One static table describes every setting the window exposes: the config
//! section and key it writes, the control it paints, and whether the change
//! only takes effect after a restart. The pure decisions (what a click does
//! to a value, what a value does to the file) live here so ordinary unit
//! tests can exercise them without a window. GPUI shells live in
//! `settings_modal`; their listeners stay with the dashboard in `main.rs`.
//!
//! Credentials never show a value: a provider row says only whether a key is
//! configured. Key material never reaches the source, status line, or a log.

use opencrabs::config::Config;

/// What a row's control does to its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingKind {
    /// A boolean: enter or click flips it.
    Toggle,
    /// One of a fixed list of strings: enter or click cycles forward.
    Choice(&'static [&'static str]),
    /// An `Option<bool>`: `default` (key absent) -> `on` -> `off` -> `default`.
    OptBool,
    /// Seconds or counts: left/right or the -/+ buttons step it, clamped.
    Number { min: u64, step: u64, max: u64 },
}

/// The rail categories (#2890). Every row declares its own group, so a new
/// setting has to pick one at compile time instead of landing in whichever
/// bucket a catch-all arm happened to name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSection {
    General,
    Notifications,
    Privacy,
    Advanced,
    Memory,
    Credentials,
}

impl SettingsSection {
    /// Rail order, top to bottom.
    pub const ALL: [SettingsSection; 6] = [
        SettingsSection::General,
        SettingsSection::Notifications,
        SettingsSection::Privacy,
        SettingsSection::Advanced,
        SettingsSection::Memory,
        SettingsSection::Credentials,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::General => "General",
            SettingsSection::Notifications => "Notifications",
            SettingsSection::Privacy => "Privacy",
            SettingsSection::Advanced => "Advanced",
            SettingsSection::Memory => "Memory",
            SettingsSection::Credentials => "Credentials",
        }
    }

    /// The one-line hint under the rail label.
    pub fn hint(self) -> &'static str {
        match self {
            SettingsSection::General => "approval and behaviour",
            SettingsSection::Notifications => "sound and OS alerts",
            SettingsSection::Privacy => "what output may show",
            SettingsSection::Advanced => "tools, timeouts, logging",
            SettingsSection::Memory => "indexing and shared sessions",
            SettingsSection::Credentials => "provider api keys",
        }
    }

    pub fn position(self) -> usize {
        Self::ALL
            .iter()
            .position(|s| *s == self)
            .unwrap_or_default()
    }

    /// tab / shift+tab on the rail, wrapping at both ends.
    pub fn step(self, dir: i32) -> SettingsSection {
        let count = Self::ALL.len();
        let at = self.position();
        let next = if dir < 0 {
            (at + count - 1) % count
        } else {
            (at + 1) % count
        };
        Self::ALL[next]
    }
}

/// One row of the modal, bound to one config key.
pub struct SettingSpec {
    pub section: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub kind: SettingKind,
    /// Which rail category paints this row.
    pub group: SettingsSection,
    /// True when the running process cannot pick the change up: the row
    /// carries a `restart` chip instead of pretending to apply live.
    pub restart: bool,
    pub blurb: &'static str,
}

/// A row's current value, read out of the loaded config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowValue {
    Bool(bool),
    Choice(String),
    OptBool(Option<bool>),
    Number(u64),
}

/// Every row, in index order. This array is the storage order: a row's index
/// here is the index into the live values, so the rail filters it and never
/// reorders it.
pub const SPECS: &[SettingSpec] = &[
    SettingSpec {
        section: "agent",
        key: "approval_policy",
        label: "Approval policy",
        kind: SettingKind::Choice(&["ask", "auto-session", "auto-always"]),
        restart: false,
        blurb: "how tool calls are approved",
        group: SettingsSection::General,
    },
    SettingSpec {
        section: "agent",
        key: "completion_notification_enabled",
        label: "Completion feedback",
        kind: SettingKind::Toggle,
        restart: false,
        blurb: "master switch for sound and OS notifications; the four below need it on",
        group: SettingsSection::Notifications,
    },
    SettingSpec {
        section: "agent",
        key: "notify_on_turn_completed",
        label: "Notify when a turn finishes",
        kind: SettingKind::Toggle,
        restart: false,
        blurb: "",
        group: SettingsSection::Notifications,
    },
    SettingSpec {
        section: "agent",
        key: "notify_on_turn_failed",
        label: "Notify when a turn fails",
        kind: SettingKind::Toggle,
        restart: false,
        blurb: "",
        group: SettingsSection::Notifications,
    },
    SettingSpec {
        section: "agent",
        key: "notify_on_task_completed",
        label: "Notify when a plan task settles",
        kind: SettingKind::Toggle,
        restart: false,
        blurb: "",
        group: SettingsSection::Notifications,
    },
    SettingSpec {
        section: "agent",
        key: "notify_on_plan_completed",
        label: "Notify when a plan finishes",
        kind: SettingKind::Toggle,
        restart: false,
        blurb: "",
        group: SettingsSection::Notifications,
    },
    SettingSpec {
        section: "agent",
        key: "redact_sensitive_data",
        label: "Redact sensitive data",
        kind: SettingKind::Toggle,
        restart: false,
        blurb: "hide keys, tokens, passwords and IPs in output",
        group: SettingsSection::Privacy,
    },
    SettingSpec {
        section: "agent",
        key: "redact_group",
        label: "Redact in group chats",
        kind: SettingKind::OptBool,
        restart: false,
        blurb: "default follows the global switch",
        group: SettingsSection::Privacy,
    },
    SettingSpec {
        section: "agent",
        key: "redact_dm",
        label: "Redact in direct messages",
        kind: SettingKind::OptBool,
        restart: false,
        blurb: "default shows secrets in owner-private DMs",
        group: SettingsSection::Privacy,
    },
    SettingSpec {
        section: "agent",
        key: "lazy_tools",
        label: "Lazy tool schemas",
        kind: SettingKind::Toggle,
        restart: true,
        blurb: "ship core tools only, discover the rest on demand",
        group: SettingsSection::Advanced,
    },
    SettingSpec {
        section: "agent",
        key: "thinking_loop_timeout_secs",
        label: "Thinking loop guard (s)",
        kind: SettingKind::Number {
            min: 0,
            step: 60,
            max: 7200,
        },
        restart: false,
        blurb: "kill a stream that produces no output; 0 disables",
        group: SettingsSection::Advanced,
    },
    SettingSpec {
        section: "agent",
        key: "time_marker_interval_secs",
        label: "Time marker interval (s)",
        kind: SettingKind::Number {
            min: 0,
            step: 60,
            max: 7200,
        },
        restart: false,
        blurb: "mid-turn time notices in long tool loops; 0 disables",
        group: SettingsSection::Advanced,
    },
    SettingSpec {
        section: "agent",
        key: "debug_logs",
        label: "Debug file logging",
        kind: SettingKind::Toggle,
        restart: false,
        blurb: "hot-reloads; the --debug flag always wins",
        group: SettingsSection::Advanced,
    },
    SettingSpec {
        section: "memory",
        key: "vector_enabled",
        label: "Vector embeddings",
        kind: SettingKind::Toggle,
        restart: true,
        blurb: "semantic memory search",
        group: SettingsSection::Memory,
    },
    SettingSpec {
        section: "memory",
        key: "sweep_interval_secs",
        label: "External sweep interval (s)",
        kind: SettingKind::Number {
            min: 10,
            step: 30,
            max: 86400,
        },
        restart: false,
        blurb: "freshness sweeps of the external index paths",
        group: SettingsSection::Memory,
    },
    SettingSpec {
        section: "memory",
        key: "backfill_interval_secs",
        label: "Embedding backfill sweep (s)",
        kind: SettingKind::Number {
            min: 0,
            step: 60,
            max: 86400,
        },
        restart: false,
        blurb: "re-reads config each tick; 0 disables",
        group: SettingsSection::Memory,
    },
    SettingSpec {
        section: "memory",
        key: "external_allowed_in_shared",
        label: "External memory in shared sessions",
        kind: SettingKind::Toggle,
        restart: false,
        blurb: "default deny outside the owner session",
        group: SettingsSection::Memory,
    },
    SettingSpec {
        section: "memory",
        key: "internal_allowed_in_shared",
        label: "Brain files in shared sessions",
        kind: SettingKind::Toggle,
        restart: false,
        blurb: "default deny outside the owner session",
        group: SettingsSection::Memory,
    },
];

/// What a row's new value asks the config file to do. Pure, so the decision
/// is testable without touching any file; `main.rs` executes the op.
#[derive(Debug, PartialEq, Eq)]
pub enum WriteOp {
    /// `Config::write_key`: the value parses as bool/integer on write.
    SetScalar(String),
    /// `Config::write_key_string`: always a TOML string, even for "2026".
    SetString(String),
    /// `Config::remove_key`: the absent-key third state of an OptBool.
    Remove,
}

/// The write a value maps to, or None when the shape does not fit the row
/// (a bug in the caller, not a user state, so it must not write anything).
pub fn write_op(spec: &SettingSpec, value: &RowValue) -> Option<WriteOp> {
    match (spec.kind, value) {
        (SettingKind::Toggle, RowValue::Bool(b)) => Some(WriteOp::SetScalar(b.to_string())),
        (SettingKind::Choice(_), RowValue::Choice(s)) => Some(WriteOp::SetString(s.clone())),
        (SettingKind::OptBool, RowValue::OptBool(None)) => Some(WriteOp::Remove),
        (SettingKind::OptBool, RowValue::OptBool(Some(b))) => {
            Some(WriteOp::SetScalar(b.to_string()))
        }
        (SettingKind::Number { .. }, RowValue::Number(n)) => {
            Some(WriteOp::SetScalar(n.to_string()))
        }
        _ => None,
    }
}

/// Enter/click on a row: the value it lands on.
pub fn advance(spec: &SettingSpec, value: &RowValue) -> Option<RowValue> {
    match (spec.kind, value) {
        (SettingKind::Toggle, RowValue::Bool(b)) => Some(RowValue::Bool(!b)),
        (SettingKind::Choice(options), RowValue::Choice(current)) => {
            let at = options.iter().position(|o| o == &current.as_str());
            // A value the list does not know (hand-edited config) restarts
            // the cycle at the first option rather than vanishing.
            let next = at.map_or(0, |i| (i + 1) % options.len());
            options
                .get(next)
                .map(|s| RowValue::Choice((*s).to_string()))
        }
        (SettingKind::OptBool, RowValue::OptBool(current)) => {
            Some(RowValue::OptBool(match current {
                None => Some(true),
                Some(true) => Some(false),
                Some(false) => None,
            }))
        }
        (SettingKind::Number { .. }, RowValue::Number(n)) => {
            Some(RowValue::Number(step_number(spec, *n, 1)))
        }
        _ => None,
    }
}

/// Left/right on a number row: one step, clamped to the row's range. A
/// hand-edited value outside the range snaps to the nearest bound first.
pub fn step_number(spec: &SettingSpec, current: u64, dir: i32) -> u64 {
    let SettingKind::Number { min, step, max } = spec.kind else {
        return current;
    };
    let clamped = current.clamp(min, max);
    if dir < 0 {
        clamped.saturating_sub(step).max(min)
    } else {
        clamped.saturating_add(step).min(max)
    }
}

/// Read every row's current value out of the loaded config, in SPECS order.
pub fn current_values(config: &Config) -> Vec<RowValue> {
    let a = &config.agent;
    let m = &config.memory;
    SPECS
        .iter()
        .map(|spec| match (spec.section, spec.key) {
            ("agent", "approval_policy") => RowValue::Choice(a.approval_policy.clone()),
            ("agent", "completion_notification_enabled") => {
                RowValue::Bool(a.completion_notification_enabled)
            }
            ("agent", "notify_on_turn_completed") => RowValue::Bool(a.notify_on_turn_completed),
            ("agent", "notify_on_turn_failed") => RowValue::Bool(a.notify_on_turn_failed),
            ("agent", "notify_on_task_completed") => RowValue::Bool(a.notify_on_task_completed),
            ("agent", "notify_on_plan_completed") => RowValue::Bool(a.notify_on_plan_completed),
            ("agent", "redact_sensitive_data") => RowValue::Bool(a.redact_sensitive_data),
            ("agent", "redact_group") => RowValue::OptBool(a.redact_group),
            ("agent", "redact_dm") => RowValue::OptBool(a.redact_dm),
            ("agent", "lazy_tools") => RowValue::Bool(a.lazy_tools),
            ("agent", "thinking_loop_timeout_secs") => {
                RowValue::Number(a.thinking_loop_timeout_secs)
            }
            ("agent", "time_marker_interval_secs") => RowValue::Number(a.time_marker_interval_secs),
            ("agent", "debug_logs") => RowValue::Bool(a.debug_logs),
            ("memory", "vector_enabled") => RowValue::Bool(m.vector_enabled),
            ("memory", "sweep_interval_secs") => RowValue::Number(m.sweep_interval_secs),
            ("memory", "backfill_interval_secs") => RowValue::Number(m.backfill_interval_secs),
            ("memory", "external_allowed_in_shared") => {
                RowValue::Bool(m.external_allowed_in_shared)
            }
            ("memory", "internal_allowed_in_shared") => {
                RowValue::Bool(m.internal_allowed_in_shared)
            }
            // A spec without a reader is a table bug; the Bool is a paintable
            // placeholder, and the dedicated descriptor test is what fails.
            _ => RowValue::Bool(false),
        })
        .collect()
}

/// One keyed provider in the credentials section: where its key writes and
/// whether one is configured. The key itself is never read into a row.
pub struct CredSpec {
    pub section: String,
    pub label: String,
    pub configured: bool,
}

/// The chat providers that take an API key, in display order. CLI subprocess
/// providers carry no key and are not listed.
pub fn credential_rows(config: &Config) -> Vec<CredSpec> {
    let p = &config.providers;
    let named: &[(&str, &str, Option<&opencrabs::config::ProviderConfig>)] = &[
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
    let mut rows: Vec<CredSpec> = named
        .iter()
        .map(|(section, label, cfg)| CredSpec {
            section: (*section).to_string(),
            label: (*label).to_string(),
            configured: cfg
                .and_then(|c| c.api_key.as_ref())
                .is_some_and(|k| !k.is_empty()),
        })
        .collect();
    if let Some(customs) = &p.custom {
        for (name, cfg) in customs {
            rows.push(CredSpec {
                section: format!("providers.custom.{name}"),
                label: format!("Custom: {name}"),
                configured: cfg.api_key.as_ref().is_some_and(|k| !k.is_empty()),
            });
        }
    }
    rows
}

/// The value a control paints, in one place so the row and its tests agree.
pub fn value_label(value: &RowValue) -> String {
    match value {
        RowValue::Bool(b) => if *b { "on" } else { "off" }.to_string(),
        RowValue::Choice(s) => s.clone(),
        RowValue::OptBool(None) => "default".to_string(),
        RowValue::OptBool(Some(b)) => if *b { "on" } else { "off" }.to_string(),
        RowValue::Number(n) => n.to_string(),
    }
}

/// The global row indices a rail section paints, in paint order: its settings
/// first, then the credential rows when the section is Credentials.
pub fn section_rows(group: SettingsSection, setting_count: usize, cred_count: usize) -> Vec<usize> {
    let settings = setting_count.min(SPECS.len());
    let mut rows: Vec<usize> = SPECS
        .iter()
        .take(settings)
        .enumerate()
        .filter(|(_, spec)| spec.group == group)
        .map(|(index, _)| index)
        .collect();
    if group == SettingsSection::Credentials {
        rows.extend(settings..settings + cred_count);
    }
    rows
}

/// A row's child index inside its section pane, for scrolling to the cursor.
pub fn pane_index(
    group: SettingsSection,
    row: usize,
    setting_count: usize,
    cred_count: usize,
) -> usize {
    section_rows(group, setting_count, cred_count)
        .iter()
        .position(|candidate| *candidate == row)
        .unwrap_or_default()
}
