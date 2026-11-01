//! Configuration types, defaults, loading, and validation.

use super::provider_registry::ProviderRegistryConfig;
use super::wire_dialect::WireDialect;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Outcome of the FIRST `Config::load()` in the process, kept so the TUI can
/// tell the user at startup that it is running on recovered values.
///
/// Written once and only ever read, never consumed: the previous consume-once
/// flags were stolen by whichever thread called the accessor first, so a
/// concurrent reader silently cleared the signal out from under the startup
/// notification (#912). Everything that needs the outcome of a SPECIFIC load
/// takes it from `Config::load_with_status()` instead.
pub(crate) static FIRST_LOAD_STATUS: std::sync::OnceLock<ConfigLoadStatus> =
    std::sync::OnceLock::new();

/// Unknown top-level keys found in config.toml (possible typos).
static CONFIG_TYPO_WARNINGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Mutex protecting read-modify-write cycles on config.toml / keys.toml.
/// Without this, concurrent `write_key` calls can race: one reads while
/// another is mid-write, gets a partial/empty file, parses it as empty,
/// and overwrites the real config with an empty table.
pub static CONFIG_FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Main configuration structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// provider registry integration configuration
    #[serde(default)]
    pub provider_registry: ProviderRegistryConfig,

    /// Database configuration
    #[serde(default)]
    pub database: DatabaseConfig,

    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,

    /// Debug options
    #[serde(default)]
    pub debug: DebugConfig,

    /// LLM provider configurations
    #[serde(default)]
    pub providers: ProviderConfigs,

    /// Messaging channel integrations
    #[serde(default)]
    pub channels: ChannelsConfig,

    /// Agent behaviour configuration
    #[serde(default)]
    pub agent: AgentConfig,

    /// Daemon mode configuration (systemd / launchd service)
    #[serde(default)]
    pub daemon: DaemonConfig,

    /// A2A (Agent-to-Agent) protocol gateway configuration
    #[serde(default, alias = "gateway")]
    pub a2a: A2aConfig,

    /// Image generation and vision configuration
    #[serde(default)]
    pub image: ImageConfig,

    /// Cron job defaults
    #[serde(default)]
    pub cron: CronConfig,

    /// Memory / embedding configuration
    #[serde(default)]
    pub memory: MemoryConfig,

    /// Tiered decision cache (#1648): per-tier reuse rings with a
    /// release-day keep-or-cut evaluation. Empty section = feature fully
    /// off, every code path behaves exactly like before this existed.
    #[serde(default)]
    pub decisions: DecisionsConfig,

    /// Brain-file behaviour: read-time empty-section stripping and other
    /// per-file knobs. Optional — defaults preserve historical behaviour
    /// where strip-on-load was off.
    #[serde(default)]
    pub brain: BrainConfig,

    /// Browser configuration for browser_navigate and browser_click tools.
    /// When `cdp_endpoint` is set, connects to an existing Chromium instance
    /// instead of spawning a new one. Useful for sharing a single browser
    /// across multiple profiles to save memory.
    #[serde(default)]
    pub browser: BrowserConfig,

    /// Self-repair configuration for `/doctor --fix` and the startup sweep
    #[serde(default)]
    pub doctor: DoctorConfig,

    /// TUI (terminal UI) configuration: theme, display preferences.
    /// Optional — defaults preserve current (crab-dark) rendering.
    #[serde(default)]
    pub tui: TuiConfig,

    /// Runtime feature flags (#1705): opt-in capabilities that default
    /// OFF so per-use cost and storage only appear when the operator
    /// asks for them. Empty section = everything off.
    #[serde(default)]
    pub features: FeaturesConfig,
}

/// Opt-in runtime feature flags (#1705).
///
/// ```toml
/// [features]
/// audit_recording = true   # write turn_retrievals + turn_outcomes for /audit
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeaturesConfig {
    /// Audit recording (#1705): persist one `turn_retrievals` row per
    /// read-class tool call (read/search/list) and one `turn_outcomes` row
    /// per turn with the mechanically-observed verdict. Default off: with
    /// the flag unset the tables stay empty and `/audit` renders the
    /// pre-existing ACTION data only. Resolved once at agent construction
    /// (same policy as `[agent] lazy_tools`), so a change needs a restart
    /// or config reload.
    #[serde(default)]
    pub audit_recording: bool,
}

/// TUI (terminal UI) configuration.
///
/// ```toml
/// [tui]
/// theme = "dracula"   # any built-in or user preset; omit for crab-dark default
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TuiConfig {
    /// Active theme name. Case-insensitive lookup against built-in presets
    /// (crab-dark, dracula, alucard, monokai, solarized-light, solarized-dark,
    /// catppuccin-mocha, catppuccin-latte) and user presets under
    /// `~/.opencrabs/themes/*.toml`. `None` / empty = crab-dark default.
    #[serde(default)]
    pub theme: Option<String>,
}

/// Custom deserializer for `[brain.caps]` that accepts both:
///
/// - Quoted keys: `"AGENTS.md" = 600` (already a string key → usize)
/// - Unquoted dotted keys: `AGENTS.md = 600` (TOML 1.0 treats as nested table
///   `{AGENTS: {md: 600}}` which this deserializer flattens back)
fn deser_caps_compat<'de, D>(d: D) -> std::result::Result<BTreeMap<String, usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;

    let value: toml::Value = toml::Value::deserialize(d)?;
    let mut result = BTreeMap::new();
    if let Some(table) = value.as_table() {
        flatten_caps_table(table, String::new(), &mut result);
    }
    Ok(result)
}

/// Recursively walk a TOML table, reconstructing dotted keys from nested tables.
fn flatten_caps_table(
    table: &toml::map::Map<String, toml::Value>,
    prefix: String,
    out: &mut BTreeMap<String, usize>,
) {
    for (key, value) in table {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{}.{}", prefix, key)
        };
        if let Some(n) = value.as_integer() {
            out.insert(full_key, n as usize);
        } else if let Some(sub_table) = value.as_table() {
            flatten_caps_table(sub_table, full_key, out);
        }
    }
}

/// Brain-file behaviour configuration. Issue #164 added read-time stripping
/// of empty header stubs (`## Header` with no body) so the LLM never sees
/// dead sections, plus a per-file line cap so `sync_templates` cannot
/// silently grow a file past the user's budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainConfig {
    /// Strip header stubs from brain-file reads. Default true. Writes are
    /// never affected — disk stays authoritative; only the loaded view is
    /// filtered.
    #[serde(default = "default_strip_empty_sections")]
    pub strip_empty_sections: bool,

    /// Per-file line caps for `sync_templates`. When a merged file would
    /// exceed its cap, the sync BAILS instead of writing — the user sees
    /// a warning naming the file, the current and upstream line counts,
    /// and the top-3 largest new sections that would have been added.
    /// Empty map means no cap configured beyond `default_brain_file_cap`.
    /// Issue #164 fix 2.
    #[serde(default, deserialize_with = "deser_caps_compat")]
    pub caps: std::collections::BTreeMap<String, usize>,

    /// Fallback cap applied to any brain file not explicitly listed in
    /// `caps`. Default 500 lines per the issue's recommended budget.
    #[serde(default = "default_brain_file_cap")]
    pub default_cap: usize,
}

/// `[doctor]` — self-repair switches for `/doctor --fix` and the startup
/// sweep (#1114).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct DoctorConfig {
    /// Run the repair sweep automatically at startup (stuck cron rows,
    /// stale pre-init plan markers, loose brain/log permissions). Set false
    /// to restrict repairs to explicit `/doctor --fix` invocations.
    #[serde(default = "default_true")]
    pub auto_fix: bool,
}

impl Default for DoctorConfig {
    fn default() -> Self {
        Self { auto_fix: true }
    }
}

fn default_true() -> bool {
    true
}

fn default_strip_empty_sections() -> bool {
    true
}

fn default_brain_file_cap() -> usize {
    500
}

impl Default for BrainConfig {
    fn default() -> Self {
        Self {
            strip_empty_sections: default_strip_empty_sections(),
            caps: std::collections::BTreeMap::new(),
            default_cap: default_brain_file_cap(),
        }
    }
}

impl BrainConfig {
    /// Resolve the line cap for a specific filename. Looks up `caps` first,
    /// falls back to `default_cap`. Filenames are matched exactly (case
    /// sensitive) so `TOOLS.md` and `tools.md` are distinct entries.
    pub fn cap_for(&self, filename: &str) -> usize {
        self.caps.get(filename).copied().unwrap_or(self.default_cap)
    }
}

/// Browser configuration for browser_navigate and browser_click tools.
///
/// When `cdp_endpoint` is set, the browser manager connects to an existing
/// Chromium instance via Chrome DevTools Protocol instead of spawning a new
/// one. This allows multiple profiles to share a single browser, saving
/// significant memory (each Chromium instance uses ~250-300MB).
///
/// Example in config.toml:
/// ```toml
/// [browser]
/// cdp_endpoint = "http://localhost:9222"
/// ```
///
/// To start a standalone Chromium with CDP enabled:
/// ```bash
/// chromium --remote-debugging-port=9222 --headless --no-sandbox
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BrowserConfig {
    /// CDP endpoint for an existing Chromium instance with remote debugging
    /// enabled. When set, the browser manager connects to this endpoint instead
    /// of spawning a new browser, so multiple profiles can share one Chromium.
    ///
    /// Prefer the `http://host:port` form — the manager queries `/json/version`
    /// to discover the real devtools websocket URL. A bare `ws://host:port` is
    /// also accepted (normalized to `http://` internally); a full
    /// `ws://host:port/devtools/browser/<id>` URL is used as-is.
    ///
    /// Example: "http://localhost:9222"
    #[serde(default)]
    pub cdp_endpoint: Option<String>,
}

/// Daemon mode configuration (systemd / launchd service).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Health check HTTP port. When set, `opencrabs daemon` binds a tiny HTTP
    /// server on `0.0.0.0:<port>` that responds to `GET /health` with 200 OK.
    /// Useful for systemd watchdog, uptime monitors, and external health probes.
    #[serde(default)]
    pub health_port: Option<u16>,
}

/// A2A (Agent-to-Agent) protocol gateway configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2aConfig {
    /// Whether the A2A gateway is enabled (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// Bind address (default: "127.0.0.1")
    #[serde(default = "default_a2a_bind")]
    pub bind: String,

    /// Gateway port (default: 18790)
    #[serde(default = "default_a2a_port")]
    pub port: u16,

    /// Allowed CORS origins — must be set explicitly, no cross-origin requests allowed by default
    #[serde(default)]
    pub allowed_origins: Vec<String>,

    /// Optional externally-reachable URL for this profile's gateway (public
    /// IP, tailscale hostname, relay). The target declares how it is reachable
    /// once; every caller inherits it via `profile_list` / the agent card.
    /// Absent → callers fall back to `http://{bind}:{port}` (same-box use).
    #[serde(default)]
    pub advertise_url: Option<String>,

    /// Optional API key for authenticating incoming A2A requests (Bearer token).
    /// If set, all JSON-RPC requests must include `Authorization: Bearer <key>`.
    /// If unset, no authentication is required (suitable for loopback-only use).
    #[serde(default)]
    pub api_key: Option<String>,
}

fn default_a2a_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_a2a_port() -> u16 {
    18790
}

impl Default for A2aConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_a2a_bind(),
            port: default_a2a_port(),
            allowed_origins: vec![],
            advertise_url: None,
            api_key: None,
        }
    }
}

/// Messaging channel integrations configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub discord: DiscordConfig,
    #[serde(default)]
    pub whatsapp: WhatsAppConfig,
    #[serde(default)]
    pub slack: SlackConfig,
    #[serde(default)]
    pub trello: TrelloConfig,
    #[serde(default)]
    pub signal: SignalConfig,
    #[serde(default)]
    pub google_chat: GoogleChatConfig,
    #[serde(default)]
    pub imessage: IMessageConfig,
}

/// When the bot should respond to messages in group channels.
/// DMs always get a response regardless of this setting.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RespondTo {
    /// Respond to all messages from allowed users
    All,
    /// Only respond to direct messages, ignore group channels entirely
    DmOnly,
    /// Only respond when @mentioned (or replied-to on Telegram)
    #[default]
    Mention,
    /// Auto-switch: respond to all when ≤1 active sender, switch to
    /// mention-only when a second unique sender is detected (#244).
    /// Once switched, stays mention-only until manually reset.
    Auto,
}

/// Deserialize `allowed_users` from either a TOML integer array (legacy) or string array.
fn deser_users_compat<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumOrStr {
        Int(i64),
        Str(String),
    }
    Vec::<NumOrStr>::deserialize(d).map(|v| {
        v.into_iter()
            .map(|x| match x {
                NumOrStr::Int(n) => n.to_string(),
                NumOrStr::Str(s) => s,
            })
            .collect()
    })
}

/// Deserialize an optional free-text value from whatever scalar TOML holds.
///
/// A group title that happens to read as a number or a bool (`2026`, `true`)
/// is a perfectly ordinary group name, and a hand-edited config will carry it
/// unquoted. Without this, one such line fails the whole config load over a
/// field that is pure display metadata.
fn deser_opt_text_compat<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Scalar {
        Str(String),
        Int(i64),
        Float(f64),
        Bool(bool),
    }
    Ok(Option::<Scalar>::deserialize(d)?.map(|s| match s {
        Scalar::Str(s) => s,
        Scalar::Int(n) => n.to_string(),
        Scalar::Float(f) => f.to_string(),
        Scalar::Bool(b) => b.to_string(),
    }))
}

/// Telegram userbot configuration — the receive-only MTProto user session.
///
/// This plane logs in as the user's account and receives updates from chats the
/// Bot API cannot see. Text from `allowed_chats` is passively stored for
/// explicit retrieval; it never invokes the agent or writes to Telegram.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelegramUserbotConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Telegram app api_id (my.telegram.org → API development tools).
    pub api_id: Option<i64>,
    /// Telegram app api_hash — a secret; belongs in keys.toml.
    pub api_hash: Option<String>,
    /// Login phone in international format (e.g. +2547…).
    pub phone: Option<String>,
    /// Local session path. Default: <opencrabs home>/telegram_userbot.session
    pub session_path: Option<String>,
    /// Chat IDs whose inbound text may be passively stored. Empty = dry mode.
    #[serde(default)]
    pub allowed_chats: Vec<String>,
}

/// Telegram channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    #[serde(default)]
    pub userbot: TelegramUserbotConfig,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: Option<String>,
    /// Allowlisted Telegram user IDs (numeric). Accepts int or string arrays.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub allowed_users: Vec<String>,
    /// Restrict bot to specific channel IDs. Empty = all channels. DMs always pass.
    #[serde(default)]
    pub allowed_channels: Vec<String>,
    /// When the bot should respond: "all", "dm_only", or "mention" (default)
    #[serde(default)]
    pub respond_to: RespondTo,
    /// Idle session timeout in hours for non-owner sessions.
    #[serde(default)]
    pub session_idle_hours: Option<f64>,
    /// Answer guest queries: `@botname` tags in chats the bot is NOT a member
    /// of (Bot API 10.0 guest mode, #2087). Off by default: this is the one
    /// inbound surface where someone can reach the bot in a chat it was never
    /// invited to, so it is opt-in. Even when on, the caller must be the
    /// owner, an admin (`allowed_users`), or listed in that chat's
    /// `groups.<chat_id>.allowed_users`. A group's `open` flag grants nothing
    /// here, because it records the owner opening THAT chat for the bot, not
    /// permission to answer wherever that group's members tag it. See
    /// `TelegramConfig::guest_allowed`.
    #[serde(default)]
    pub guest_queries: bool,
    /// Send structured replies and flow blocks as native Telegram rich
    /// messages (Bot API 10.1: tables, headings, lists, math, collapsible
    /// details). On by default (#425). Older clients and Telegram Web show
    /// rich messages as a "not supported" placeholder, so users on outdated
    /// clients can disable it in the onboard dialog or via
    /// `/channels telegram richtext off`; the universal HTML
    /// rendering (which works on every client) is used instead.
    #[serde(default = "default_true")]
    pub rich_messages: bool,
    /// Render ```mermaid code fences as inline diagram images inside rich
    /// messages (#1044). Diagrams are rendered by mermaid.ink over HTTP and
    /// embedded as `<img>` in the Telegram rich-HTML message; a diagram the
    /// renderer rejects degrades to a legible failure block instead of
    /// breaking the message. Note: the diagram source is sent to mermaid.ink
    /// (a third party) for rendering. Requires `rich_messages`. Disable to
    /// keep mermaid fences as plain code blocks.
    #[serde(default = "default_true")]
    pub mermaid_render: bool,
    /// Inline local MARKDOWN documents in rich messages (#1968). Off by
    /// default: a `.md` or `.markdown` file referenced from a reply becomes a
    /// plain marker and the file ships as its own document bubble, which the
    /// client's built-in viewer opens (some clients, notably Telegram
    /// Android, cannot open a document inlined in a rich message). Set true
    /// to rewrite the reference to a rich document reference and inline it AT
    /// the reference instead. Other document kinds are unaffected and always
    /// inline.
    #[serde(default)]
    pub inline_markdown_documents: bool,
    /// Silently ignore /start commands from non-allowed users in group chats.
    /// When true (default), the bot does NOT reply with user ID in groups.
    /// Users who need their ID can DM the bot instead.
    #[serde(default = "default_true")]
    pub silence_group_start: bool,
    /// Bot owner user IDs. Owners can access gated commands, see hidden files
    /// in /cd, and manage profiles. When unset, defaults to the first entry
    /// in `allowed_users`. Accepts int or string arrays.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub bot_owner: Vec<String>,
    /// Proactive per-peer flood governors (#1211), `[channels.telegram.
    /// rate_limiter]`. Three independent token buckets keyed by forum chat id
    /// — typing (~1 call / 3 s, burst 8, concurrent sessions coalesced per
    /// topic), edits (~30/min with a priority drop ladder clock → brain
    /// preview → intermediary → status; settle renders and plan-card refreshes
    /// queue latest-wins and are never dropped) and sends (~1/s under an
    /// ~18/min ceiling) — keep a busy multi-topic deployment under Telegram's
    /// documented per-peer budgets BEFORE the API answers 429, instead of only
    /// sleeping after (`rate_limit::wait_out`). Enforcement is forums-only:
    /// a chat counts as governed once any call is observed carrying a topic
    /// id, and DMs (positive chat ids) are never touched. Defaults mirror the
    /// documented bot regime; tuning down is safe, tuning up invites back the
    /// flood windows this exists to prevent.
    #[serde(default)]
    pub rate_limiter: RateLimiterConfig,
    /// Per-group access control and behavior overrides, keyed by chat id:
    /// `[channels.telegram.groups.<chat_id>]`. A user listed under a group's
    /// `allowed_users` may interact in THAT group only and is still refused in
    /// DMs unless they are also a global admin (`allowed_users`) or the owner.
    /// Set a group's `open = true` to let ANY member of that group interact
    /// there without being individually listed (DMs and other groups stay
    /// locked).
    #[serde(default)]
    pub groups: std::collections::HashMap<String, TelegramGroupConfig>,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            userbot: TelegramUserbotConfig::default(),
            enabled: false,
            token: None,
            allowed_users: Vec::new(),
            allowed_channels: Vec::new(),
            respond_to: RespondTo::default(),
            session_idle_hours: None,
            guest_queries: false,
            rich_messages: true,
            inline_markdown_documents: false,
            mermaid_render: true,
            silence_group_start: true,
            bot_owner: Vec::new(),
            rate_limiter: RateLimiterConfig::default(),
            groups: std::collections::HashMap::new(),
        }
    }
}

/// Per-group access control + behaviour override for one Telegram group.
/// Lives under `[channels.telegram.groups.<chat_id>]`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TelegramGroupConfig {
    /// The group's human-readable title, recorded from Telegram so config is
    /// readable by a person or an agent inspecting it (#984). Sections are
    /// keyed by chat id, which on its own says nothing about which group it is.
    ///
    /// Display metadata ONLY. Access control keys off the chat id and never
    /// reads this: a group title is set by whoever administers the group, so
    /// it is untrusted text. Refreshed when the observed title changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "deser_opt_text_compat")]
    pub name: Option<String>,
    /// Users allowed to interact ONLY within this group. They are NOT granted
    /// DM access (that needs the global `allowed_users` or owner). Accepts int
    /// or string arrays, same as `allowed_users`.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub allowed_users: Vec<String>,
    /// Per-group respond mode. Overrides the channel-level `respond_to` for
    /// this group when set; `None` falls back to the global value.
    #[serde(default)]
    pub respond_to: Option<RespondTo>,
    /// Opt-in open mode for THIS group: when true, any member of this group
    /// passes the ACL without being listed in `allowed_users`. DMs and every
    /// other group stay locked (a member here is still refused in DMs and in
    /// other groups unless separately allowed). Lets a trusted group serve all
    /// its members while keeping secure-by-default everywhere else. Default:
    /// false.
    #[serde(default)]
    pub open: bool,
}

/// Proactive flood-governor knobs (#1211), `[channels.telegram.rate_limiter]`.
///
/// Defaults are the documented Telegram bot regime per peer: ~20 typing
/// actions / 5 s + 40 / 30 s, edits observed safe at 30/min, sends under
/// ~20/min. The governors read these live on every gate evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimiterConfig {
    /// Master switch. Default true — enforcement only ever engages for FORUM
    /// peers (a chat seen carrying a topic id); DMs are untouched either way,
    /// so opting out is only needed to restore fully reactive behavior.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// G1 typing: minimum spacing between `sendChatAction` calls, keyed on the
    /// CHAT — `governor.rs` buckets one peer per chat id and a thread id only
    /// sets `forum_seen` — so the sustained ceiling is exactly `60 / interval`
    /// per chat per minute. 3 s is therefore 20/min: Telegram's documented
    /// per-group ceiling, with no headroom. Default: 3.
    #[serde(default = "default_typing_min_interval_secs")]
    pub typing_min_interval_secs: u64,
    /// G1 typing: burst capacity of the bucket (refreshes allowed before
    /// pacing bites). Default: 8.
    #[serde(default = "default_typing_burst")]
    pub typing_burst: u32,
    /// G1 typing: longest a refresh may be HELD waiting for a token before it
    /// is dropped instead. The indicator expires after ~5 s, so holding past
    /// this serves nobody. Default: 30.
    #[serde(default = "default_typing_max_hold_secs")]
    pub typing_max_hold_secs: u64,
    /// G2 edits: steady-state `editMessageText` budget per forum peer per
    /// minute (sized safely below Telegram's ~20/min group limit). Default: 18.
    #[serde(default = "default_edits_per_minute")]
    pub edits_per_minute: u32,
    /// G2 edits: burst capacity of the edit bucket. Default: 10.
    #[serde(default = "default_edit_burst")]
    pub edit_burst: u32,
    /// G3 sends: minimum spacing between full messages sent to one chat, in
    /// milliseconds. Default: 1000 (~1/s).
    #[serde(default = "default_send_min_interval_millis")]
    pub send_min_interval_millis: u64,
    /// G3 sends: per-group ceiling on full messages per minute, under the
    /// official ~20/min group limit. Default: 18.
    #[serde(default = "default_sends_ceiling_per_minute")]
    pub sends_ceiling_per_minute: u32,
    /// G3 sends: burst capacity of the send pacer. Default: 5.
    #[serde(default = "default_sends_burst")]
    pub sends_burst: u32,
    /// G4 rich: steady-state budget per forum peer per minute for the rich
    /// message endpoint (`sendRichMessage` / its edit sibling), sized safely
    /// below Telegram's ~20/min group limit. Default: 18.
    #[serde(default = "default_rich_per_minute")]
    pub rich_per_minute: u32,
    /// G4 rich: burst capacity of the rich bucket. Default: 10.
    #[serde(default = "default_rich_burst")]
    pub rich_burst: u32,
    /// Cross-surface minimum spacing between ANY two admissions to one forum
    /// peer, in milliseconds (#1927). Telegram's per-chat rule is sub-minute
    /// ("no more than one message per second"), which a per-minute bucket
    /// cannot express: measured refusals land while the refusing surface is
    /// still under its own per-minute ceiling, and counting admissions in the
    /// prior second separates refusal minutes from clean ones (>=1 admission:
    /// 50.1 % vs 23.8 % refusal, 2.11x). `0` disables the floor and restores
    /// the pre-#1927 per-surface pacing exactly. Default: 1000 (~1/s).
    #[serde(default = "default_spacing_floor_ms")]
    pub spacing_floor_ms: u64,
    /// Spacing of the telemetry summary INFO line (one line per active forum:
    /// admissions, ladder drops per class, finals stats, throttled ms).
    /// Default: 300.
    #[serde(default = "default_summary_log_secs")]
    pub summary_log_secs: u64,
}

impl Default for RateLimiterConfig {
    /// Default-ON: the whole point of #1211 is that the safe regime is the
    /// default regime; opting out is the explicit act.
    fn default() -> Self {
        Self {
            enabled: true,
            typing_min_interval_secs: default_typing_min_interval_secs(),
            typing_burst: default_typing_burst(),
            typing_max_hold_secs: default_typing_max_hold_secs(),
            edits_per_minute: default_edits_per_minute(),
            edit_burst: default_edit_burst(),
            send_min_interval_millis: default_send_min_interval_millis(),
            sends_ceiling_per_minute: default_sends_ceiling_per_minute(),
            sends_burst: default_sends_burst(),
            rich_per_minute: default_rich_per_minute(),
            rich_burst: default_rich_burst(),
            spacing_floor_ms: default_spacing_floor_ms(),
            summary_log_secs: default_summary_log_secs(),
        }
    }
}

fn default_typing_min_interval_secs() -> u64 {
    3
}

fn default_typing_burst() -> u32 {
    8
}

fn default_typing_max_hold_secs() -> u64 {
    30
}

fn default_edits_per_minute() -> u32 {
    18
}

fn default_edit_burst() -> u32 {
    10
}

fn default_send_min_interval_millis() -> u64 {
    1000
}

fn default_sends_ceiling_per_minute() -> u32 {
    18
}

fn default_sends_burst() -> u32 {
    5
}

fn default_rich_per_minute() -> u32 {
    18
}

fn default_rich_burst() -> u32 {
    10
}

/// Cross-surface spacing floor (#1927). 1000 ms is Telegram's documented
/// per-chat ceiling ("no more than one message per second") expressed as the
/// interval the floor enforces; `0` disables it.
fn default_spacing_floor_ms() -> u64 {
    1000
}

fn default_summary_log_secs() -> u64 {
    300
}

impl TelegramConfig {
    /// Check if a user ID is a bot owner.
    ///
    /// Uses `bot_owner` if configured, otherwise falls back to the first entry
    /// in `allowed_users`. With BOTH empty the channel is unconfigured and
    /// nobody is an owner, which is what `config::owner::is_owner` implements;
    /// this comment previously claimed the opposite ("everyone is treated as
    /// owner") and that stale claim misled a caller into assuming an open
    /// channel grants ownership.
    pub fn is_owner(&self, user_id: &str) -> bool {
        crate::config::owner::is_owner(&self.allowed_users, &self.bot_owner, user_id)
    }

    /// Whether a message we are NOT going to answer is still worth keeping.
    ///
    /// Passive capture exists so the bot holds context in a chat it belongs
    /// to. It ran for every undirected message regardless of whether the chat
    /// was ever authorised, so a group nobody approved still had its members'
    /// messages and media written to the database and disk (#1043). "Not
    /// addressed to us" and "not ours at all" are different things and only
    /// the first should retain anything.
    ///
    /// A chat qualifies when it has its own `[channels.telegram.groups.<id>]`
    /// entry, which is what the owner adding the bot creates, or when the
    /// sender is someone we already answer anywhere (allowlisted or owner) —
    /// their message is ours to keep even in a chat with no entry yet.
    pub fn retains_history(&self, chat_id: &str, sender_id: &str) -> bool {
        self.groups.contains_key(chat_id)
            || Self::id_in(&self.allowed_users, sender_id)
            || self.is_owner(sender_id)
    }

    /// Whether any list in `list` matches `uid` (ignoring a leading '+').
    /// Is `user_id` already on `chat_id`'s roster?
    ///
    /// Lets the caller skip a registration attempt that would reload config
    /// only to discover the user is already listed (#840).
    pub fn group_has_user(&self, chat_id: &str, user_id: &str) -> bool {
        self.groups
            .get(chat_id)
            .is_some_and(|g| Self::id_in(&g.allowed_users, user_id.trim_start_matches('+')))
    }

    fn id_in(list: &[String], uid: &str) -> bool {
        list.iter().any(|u| u.trim_start_matches('+') == uid)
    }

    /// A global admin (`allowed_users`) or the owner (`bot_owner`). These may
    /// act in any chat, DMs included.
    fn is_admin_or_owner(&self, uid: &str) -> bool {
        Self::id_in(&self.allowed_users, uid) || Self::id_in(&self.bot_owner, uid)
    }

    /// Per-chat access control.
    ///
    /// Tiers:
    /// - `bot_owner` + `allowed_users` (admins): allowed anywhere, DMs included.
    /// - `groups.<chat_id>.open = true`: any member of THAT group is allowed in
    ///   THAT group only; refused in DMs.
    /// - `groups.<chat_id>.allowed_users`: allowed in THAT group only; refused
    ///   in DMs. This closes the "move the bot into a private chat to escape
    ///   group oversight" bypass.
    ///
    /// When neither `allowed_users` nor `bot_owner` is configured the bot is
    /// unconfigured and denies access (secure by default); configuring either
    /// list activates the strict ACL.
    pub fn user_allowed(&self, user_id: &str, chat_id: &str, is_dm: bool) -> bool {
        let uid = user_id.trim_start_matches('+');
        if self.allowed_users.is_empty() && self.bot_owner.is_empty() {
            return false;
        }
        if self.is_admin_or_owner(uid) {
            return true;
        }
        if is_dm {
            return false;
        }
        self.groups
            .get(chat_id)
            .is_some_and(|g| g.open || Self::id_in(&g.allowed_users, uid))
    }

    /// Guest mode access (#2087): may this user get an answer to a tag placed
    /// in a chat the bot is not a member of?
    ///
    /// Deliberately stricter than `user_allowed`. That one answers "may this
    /// person talk to the bot HERE", so a group that is `open` passes. This
    /// one answers "may the bot be pulled into a chat it was never invited
    /// to", and there the `open` flag is no consent at all: it was given for
    /// one chat, not for every chat that chat's members happen to tag the bot
    /// in. Owner and admins act anywhere; everyone else needs an explicit
    /// listing for the chat the tag came from, and a missing chat id means no
    /// listing can match, so the query is refused.
    pub fn guest_allowed(&self, user_id: &str, caller_chat_id: Option<&str>) -> bool {
        if !self.guest_queries {
            return false;
        }
        let uid = user_id.trim_start_matches('+');
        if self.is_admin_or_owner(uid) {
            return true;
        }
        caller_chat_id.is_some_and(|chat| self.group_has_user(chat, uid))
    }

    /// Respond mode for a chat: the group's override if set, else the global.
    pub fn respond_to_for(&self, chat_id: &str) -> RespondTo {
        self.groups
            .get(chat_id)
            .and_then(|g| g.respond_to)
            .unwrap_or(self.respond_to)
    }
}

/// Discord channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: Option<String>,
    /// Allowlisted Discord user IDs (numeric). Accepts int or string arrays.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub allowed_users: Vec<String>,
    /// Restrict bot to specific channel IDs. Empty = all channels.
    #[serde(default)]
    pub allowed_channels: Vec<String>,

    /// Role IDs granted access in guilds, in addition to `allowed_users`.
    /// A member carrying ANY of these roles may use the bot (#387).
    #[serde(default)]
    pub allowed_roles: Vec<String>,

    /// Hours before interactive components (select menus, form buttons)
    /// expire; stale clicks answer "expired" instead of firing (#386).
    /// Default 24.
    #[serde(default = "default_component_ttl_hours")]
    pub component_ttl_hours: f64,
    /// When the bot should respond: "all", "dm_only", or "mention" (default)
    #[serde(default)]
    pub respond_to: RespondTo,
    /// Idle session timeout in hours for non-owner sessions.
    #[serde(default)]
    pub session_idle_hours: Option<f64>,
    /// Bot owner user IDs. When unset, defaults to the first entry in
    /// `allowed_users`. Accepts int or string arrays.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub bot_owner: Vec<String>,
    /// Fold intermediate narration (the text a model writes between tool
    /// calls) into the turn's tool-group bubble as dim subtext lines instead
    /// of posting each as a separate message — one editable "work log" per
    /// turn (agent-disco-style live trace). Default: true (#1871); set
    /// `trace_narration = false` to post each intermediate as its own
    /// message.
    #[serde(default = "default_true")]
    pub trace_narration: bool,
    /// Post answers longer than this many characters as a short teaser in
    /// the channel plus an anchored thread carrying the full body. 0
    /// disables. Discord's own message cap is 2000, so 1800 is a sensible
    /// threshold. Default: 0 (disabled).
    #[serde(default)]
    pub auto_thread_min_chars: usize,
    /// Treat messages starting with `!` as thread requests: strip the `!`,
    /// anchor a thread to the message, and route the whole turn (tool
    /// bubble, trace notes, answer) into it. No-op in DMs (no threads).
    /// Default: false.
    #[serde(default)]
    pub bang_new_thread: bool,
    /// Per-channel overrides, keyed by channel id, under
    /// `[channels.discord.channels.<id>]` (#2014). A thread or forum post
    /// inherits any field it does not set from its parent channel.
    #[serde(default)]
    pub channels: std::collections::HashMap<String, DiscordChannelConfig>,
    /// `[channels.discord.governor]` - the outbound write policy (#1910). Serenity's
    /// per-bucket limiter stays underneath: it is transport hygiene and owns
    /// the actual request queue. This is the PRODUCT layer on top: it knows
    /// the flow ticker and the tool-group edits are the same message being
    /// rewritten every few seconds, so it can drop a re-render instead of
    /// queueing five stale snapshots of it, and it records what the API's
    /// 429s told us. Default-on and deliberately loose.
    #[serde(default)]
    pub governor: DiscordGovernorConfig,
}

/// Discord outbound write policy (#1910).
///
/// Discord publishes no fixed per-route budget and its docs forbid
/// hardcoding one, so nothing here pretends to be the real limit. The
/// buckets are CEILINGS that only bite when the bot outruns its own cadence,
/// and `pause_secs` is only the fallback window for a 429 that did not carry
/// a `retry_after`: when the response names a window, that window is what we
/// honor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DiscordGovernorConfig {
    /// Master switch. False makes every gate a pass-through: the layer stops
    /// deciding, and serenity's limiter plus the 429 log stay as the only
    /// protection, which is the pre-#1910 behavior.
    pub enabled: bool,
    /// Rolling per-channel write ceiling. Any surface counts against it
    /// (send, edit, chrome): it is the one number that says "this channel is
    /// being rewritten too often". `0` = no ceiling.
    pub writes_per_minute: u32,
    /// Burst capacity for that ceiling. A turn opening with a tool group plus
    /// an answer plus a settle edit needs to land at once, so this is the
    /// headroom before the sustained rate starts pacing. Ignored when the
    /// ceiling is off.
    pub burst: u32,
    /// Minimum spacing between two LIVE re-renders of the same message: the
    /// flow ticker's clock edits and the waiting-line refresh. Content edits
    /// and sends never wait on it. `0` = no spacing.
    pub chrome_min_spacing_ms: u64,
    /// Fallback pause window for a 429 that named no `retry_after`. When the
    /// response DOES name one, that window is what we honor, clamped to four
    /// times this number: a response asking for an hour parks the channel for
    /// that ceiling and then resets, because a governor that stalls a channel
    /// forever is a worse outage than the one it prevents.
    pub pause_secs: u64,
}

impl Default for DiscordGovernorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // Generous on purpose: a real Discord write budget is per route
            // and per bucket, and this layer must not be the thing that
            // stalls a turn. It exists to catch a runaway ticker, not to
            // second-guess the API.
            writes_per_minute: 40,
            burst: 12,
            chrome_min_spacing_ms: 1000,
            pause_secs: 5,
        }
    }
}

impl Default for DiscordConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: None,
            allowed_users: Vec::new(),
            allowed_channels: Vec::new(),
            allowed_roles: Vec::new(),
            component_ttl_hours: default_component_ttl_hours(),
            respond_to: RespondTo::default(),
            session_idle_hours: None,
            bot_owner: Vec::new(),
            trace_narration: default_true(),
            auto_thread_min_chars: 0,
            bang_new_thread: false,
            governor: DiscordGovernorConfig::default(),
            channels: std::collections::HashMap::new(),
        }
    }
}

/// One Discord channel's overrides (#2014). Lives under
/// `[channels.discord.channels.<channel_id>]`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiscordChannelConfig {
    /// Channel name, recorded so the config is readable without the id.
    /// Display metadata only; access never reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Respond mode for this channel. `None` inherits the global value.
    #[serde(default)]
    pub respond_to: Option<RespondTo>,
    /// Let any member of this channel pass the ACL here without being
    /// listed in `allowed_users`. DMs and other channels stay locked.
    #[serde(default)]
    pub open: bool,
}

impl DiscordConfig {
    /// Check if a user ID is a bot owner. See [`crate::config::owner::is_owner`].
    pub fn is_owner(&self, user_id: &str) -> bool {
        crate::config::owner::is_owner(&self.allowed_users, &self.bot_owner, user_id)
    }

    /// Respond mode for a message in `channel_id`. A thread's own setting
    /// wins, then its parent channel's, then the global `respond_to` (#2014).
    pub fn respond_to_for(&self, channel_id: &str, parent_id: Option<&str>) -> RespondTo {
        self.channel_field(channel_id, parent_id, |c| c.respond_to)
            .unwrap_or(self.respond_to)
    }

    /// Whether `channel_id` or its parent has its own `[channels.discord.channels.<id>]`
    /// block. Such a block is the owner's scope for that channel, so it admits the
    /// channel without an entry in the global `allowed_channels` list.
    pub fn has_channel_block(&self, channel_id: &str, parent_id: Option<&str>) -> bool {
        self.channels.contains_key(channel_id)
            || parent_id.is_some_and(|p| self.channels.contains_key(p))
    }

    /// Whether `channel_id` (or its parent) is `open` (#2014). Opening a
    /// channel opens its threads; nothing else is affected.
    pub fn channel_open(&self, channel_id: &str, parent_id: Option<&str>) -> bool {
        self.channel_field(channel_id, parent_id, |c| c.open.then_some(true))
            .unwrap_or(false)
    }

    /// First set value of `pick` along thread → parent, field by field.
    fn channel_field<T>(
        &self,
        channel_id: &str,
        parent_id: Option<&str>,
        pick: impl Fn(&DiscordChannelConfig) -> Option<T>,
    ) -> Option<T> {
        let own = self.channels.get(channel_id).and_then(&pick);
        own.or_else(|| {
            parent_id
                .filter(|p| *p != channel_id)
                .and_then(|p| self.channels.get(p))
                .and_then(&pick)
        })
    }
}

/// Slack channel configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SlackConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Bot token (xoxb-...)
    #[serde(default)]
    pub token: Option<String>,
    /// App-level token for Socket Mode (xapp-...)
    #[serde(default)]
    pub app_token: Option<String>,
    /// Allowlisted Slack user IDs (U12345678). Accepts int or string arrays.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub allowed_users: Vec<String>,
    /// Restrict bot to specific channel IDs. Empty = all channels.
    #[serde(default)]
    pub allowed_channels: Vec<String>,
    /// When the bot should respond: "all", "dm_only", or "mention" (default)
    #[serde(default)]
    pub respond_to: RespondTo,
    /// Idle session timeout in hours for non-owner sessions.
    #[serde(default)]
    pub session_idle_hours: Option<f64>,
    /// Bot owner user IDs. When unset, defaults to the first entry in
    /// `allowed_users`. Accepts int or string arrays.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub bot_owner: Vec<String>,
    /// `[channels.slack.governor]` - the outbound write policy (#2012), the
    /// Slack twin of `[channels.discord.governor]` (#1910). slack-morphism's
    /// own throttler stays underneath: it queues the request and retries it,
    /// it does not know that this request is the fourth re-render of a step
    /// group nobody has read yet.
    #[serde(default)]
    pub governor: SlackGovernorConfig,
}

/// Slack outbound write policy (#2012).
///
/// Slack DOES publish per-method tier budgets, and slack-morphism's connector
/// enforces them before the request leaves the process. These numbers are not
/// a mirror of those: they are ceilings that only bite when the bot outruns
/// its own cadence, plus the window we honor when Slack's 429 names a
/// `Retry-After`. When the response names a window, that window governs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SlackGovernorConfig {
    /// Master switch. False makes every gate a pass-through: the layer stops
    /// deciding, and slack-morphism's throttler plus the 429 log are the only
    /// protection, which is the pre-#2012 behavior.
    pub enabled: bool,
    /// Rolling per-conversation write ceiling (all surfaces together). It is
    /// the one number that says "this conversation is being rewritten too
    /// often". `0` = no ceiling.
    pub writes_per_minute: u32,
    /// Burst capacity for that ceiling: a turn opening with a step group plus
    /// an answer plus a settle edit has to land at once. Ignored when the
    /// ceiling is off.
    pub burst: u32,
    /// Minimum spacing between two LIVE re-renders of the same message: the
    /// flow ticker's clock and the tool-group status edits. Content edits and
    /// sends never wait on it. `0` = no spacing.
    pub chrome_min_spacing_ms: u64,
    /// Fallback park window for a 429 that named no `Retry-After`, and the
    /// safety ceiling on a park that did (4x this number): a response asking
    /// for an hour parks the conversation for the ceiling and then resets,
    /// because a governor that stalls a conversation forever is a worse
    /// outage than the one it prevents.
    pub pause_secs: u64,
}

impl Default for SlackGovernorConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            // Generous on purpose: the real budget is Slack's own per-method
            // tier limit, enforced by the connector. This layer exists to
            // catch a runaway ticker and to remember a `Retry-After`, not to
            // second-guess the API.
            writes_per_minute: 50,
            burst: 10,
            chrome_min_spacing_ms: 1000,
            pause_secs: 5,
        }
    }
}

impl SlackConfig {
    /// Check if a user ID is a bot owner. See [`crate::config::owner::is_owner`].
    pub fn is_owner(&self, user_id: &str) -> bool {
        crate::config::owner::is_owner(&self.allowed_users, &self.bot_owner, user_id)
    }
}

/// WhatsApp channel configuration
/// Who the WhatsApp bot answers. The paired account's own self-chat and any
/// `bot_owner` (operator) number are ALWAYS allowed regardless of policy.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaResponsePolicy {
    /// Legacy/auto: open when `allowed_phones` is empty, otherwise owner +
    /// allow-listed contacts. Preserves the historical behaviour.
    #[default]
    Auto,
    /// Only the paired account's self-chat and `bot_owner` operators.
    OwnerOnly,
    /// Owner/operator plus the contacts in `allowed_phones`.
    Allowlist,
    /// Every incoming DM (a business serving any customer).
    Open,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WhatsAppConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Allowlisted phone numbers (E.164 format: "+15551234567").
    /// Empty = accept messages from everyone (not recommended for business numbers).
    #[serde(default)]
    pub allowed_phones: Vec<String>,
    /// Idle session timeout in hours for non-owner sessions.
    #[serde(default)]
    pub session_idle_hours: Option<f64>,
    /// Bot owner phone numbers. When unset, defaults to the first entry in
    /// `allowed_phones`. Accepts int or string arrays.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub bot_owner: Vec<String>,
    /// Who the bot responds to: `auto` (legacy), `owner_only`, `allowlist`, or
    /// `open`. The paired account's self-chat and `bot_owner` are always
    /// allowed. Lets a number paired to serve other people's DMs choose to
    /// answer everyone (`open`) or a fixed contact list (`allowlist`).
    #[serde(default)]
    pub response_policy: WaResponsePolicy,
    /// Outbound send budget (#1407): token-bucket pacing plus a rolling
    /// 24h cap with a FIFO queue. Defaults sit in the sane-automation
    /// band; `0` disables either knob. See [`WaRateLimitConfig`].
    #[serde(default)]
    pub rate_limit: WaRateLimitConfig,
    /// `[channels.whatsapp.broadcast]` - opt-in rails for the newsletter,
    /// status and label surfaces (#1485, #1827). Empty allowlist = nobody, on
    /// purpose: an unconfigured install must not be one tool call away from
    /// messaging arbitrary numbers.
    #[serde(default)]
    pub broadcast: WaBroadcastConfig,
    /// Send tool-approval prompts as native-flow interactive buttons (#1411)
    /// instead of plain text. Default false, and deliberately so: this
    /// channel already shipped buttons once (`ButtonsMessage`) and removed
    /// them in 2f15f1d1 because modern clients silently never rendered them.
    /// `NativeFlowMessage` is a different path that may well work, but an
    /// approval prompt that does not render is a safety-critical message the
    /// owner cannot answer, so it is opt-in until a real phone has shown the
    /// card. Test it first with `whatsapp_send` action `send_buttons`.
    #[serde(default)]
    pub interactive_buttons: bool,
    /// Channel-wide disappearing-message TTL in seconds (#1487). Unset or
    /// `0` means messages do not expire, which is the WhatsApp default.
    /// Common values: 86400 (24h), 604800 (7d), 7776000 (90d, the maximum).
    /// A `whatsapp_send` call may override it per message.
    #[serde(default)]
    pub ephemeral_ttl: Option<u32>,
    /// #1525: chats whose recovered history frames are captured into
    /// `channel_messages` (message_type `imported`) instead of waking the
    /// agent. Exact normalized chat id strings (the `Display` of the source
    /// JID, as the live capture stores them), per-chat opt-in; an empty list
    /// means the feature is entirely off. The request fires on connect and
    /// is bounded (10k messages / 90 days) by `channels::whatsapp::history`.
    #[serde(default)]
    pub history_import_chats: Vec<String>,
    /// #1529: WhatsApp newsletter/channel JIDs to poll every 15 minutes and
    /// surface to the owner as one digest message per tick. Per-channel
    /// opt-in; empty = the poller never starts. Posts are never injected as
    /// inbound agent turns (#1542's anti-pattern) — owner-facing summary
    /// plus a stored `newsletter` row in `channel_messages` only.
    #[serde(default)]
    pub newsletters: Vec<String>,
}

impl WhatsAppConfig {
    /// Check if a phone number is a bot owner. See
    /// [`crate::config::owner::is_owner`]. Owners are resolved against
    /// `allowed_phones` (WhatsApp's allow list).
    pub fn is_owner(&self, user_id: &str) -> bool {
        crate::config::owner::is_owner(&self.allowed_phones, &self.bot_owner, user_id)
    }
}

/// `[channels.whatsapp.broadcast]` - rails for the broadcast surfaces (#1485).
///
/// Newsletter posts and status updates are the highest-signal spam pattern
/// Meta watches, and on this channel the phone number IS the bot: there is no
/// token to rotate if it gets banned. So the rails are config, not a
/// convention the caller is trusted to follow.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WaBroadcastConfig {
    /// Numbers (E.164 or any formatting) that may receive a status update, and
    /// newsletter (channel) JIDs that `post_newsletter` may publish to
    /// (#1827). A channel entry is written as its own JID, e.g.
    /// `120363...@newsletter`; matching keeps the digits only, so the JID form
    /// and the bare digits are the same entry. EMPTY MEANS NOBODY, which is
    /// the opposite of the usual convention and is deliberate: broadcast has
    /// to be switched on by a human.
    pub allowed_targets: Vec<String>,
    /// Minimum seconds between two broadcast sends. `0` or unset falls back
    /// to the 5s default rather than disabling pacing - turning the rail off
    /// is not a knob this offers, because a config typo must not become a
    /// mass-send.
    pub min_delay_seconds: Option<u64>,
}

/// `[channels.whatsapp.rate_limit]` - outbound send budget (#1407).
///
/// Defaults sit in the sane-automation band from the issue (30-60
/// msgs/hr stays under Meta's heuristics; 500+/hr draws blocks):
/// 30/min burst-bucketed pacing plus an 800-per-rolling-24h hard cap.
/// `0` disables either knob. Over-budget sends pace or queue - never
/// drop. Owner-bound messages bypass the budget entirely.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WaRateLimitConfig {
    /// Token-bucket rate: sustained sends per minute (also the burst
    /// capacity). Over-budget sends sleep for refill, never dropped.
    /// `0` = no per-minute pacing.
    pub messages_per_minute: u32,
    /// Rolling 24h hard cap. On saturation further sends queue (FIFO,
    /// flushed by the drainer as the window slides) and the owner is
    /// alerted exactly once per episode. `0` = no daily cap.
    pub daily_cap: u32,
}

impl Default for WaRateLimitConfig {
    fn default() -> Self {
        Self {
            messages_per_minute: 30,
            daily_cap: 800,
        }
    }
}

/// Trello channel configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrelloConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Trello API Token
    #[serde(default)]
    pub token: Option<String>,
    /// Trello API Key (stored as app_token for keys.toml symmetry)
    #[serde(default)]
    pub app_token: Option<String>,
    /// Allowlisted Trello member IDs. Empty = respond to all members.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub allowed_users: Vec<String>,
    /// Board IDs to monitor for @mentions.
    /// Accepts the old `allowed_channels` key as an alias for migration compatibility.
    #[serde(default, alias = "allowed_channels")]
    pub board_ids: Vec<String>,
    /// Optional polling interval in seconds. Absent or 0 = no polling (tool-only mode).
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,
    /// Idle session timeout in hours for non-owner sessions.
    #[serde(default)]
    pub session_idle_hours: Option<f64>,
    /// Bot owner member IDs. When unset, defaults to the first entry in
    /// `allowed_users`. Accepts int or string arrays.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub bot_owner: Vec<String>,
}

impl TrelloConfig {
    /// Check if a member ID is a bot owner. See [`crate::config::owner::is_owner`].
    pub fn is_owner(&self, user_id: &str) -> bool {
        crate::config::owner::is_owner(&self.allowed_users, &self.bot_owner, user_id)
    }
}

/// Signal channel configuration (placeholder — not yet implemented)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SignalConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Allowlisted phone numbers (E.164 format)
    #[serde(default)]
    pub allowed_phones: Vec<String>,
    /// Idle session timeout in hours.
    #[serde(default)]
    pub session_idle_hours: Option<f64>,
}

/// Google Chat channel configuration (placeholder — not yet implemented)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GoogleChatConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: Option<String>,
    /// Allowlisted user IDs. Accepts int or string arrays.
    #[serde(default, deserialize_with = "deser_users_compat")]
    pub allowed_users: Vec<String>,
    /// Idle session timeout in hours.
    #[serde(default)]
    pub session_idle_hours: Option<f64>,
}

/// iMessage channel configuration (placeholder — not yet implemented)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IMessageConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Allowlisted phone numbers (E.164 format)
    #[serde(default)]
    pub allowed_phones: Vec<String>,
    /// Idle session timeout in hours.
    #[serde(default)]
    pub session_idle_hours: Option<f64>,
}

/// STT mode: API (Groq Whisper) or Local (whisper.cpp)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SttMode {
    #[default]
    Api,
    Local,
}

/// TTS mode: API (OpenAI) or Local (Piper)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TtsMode {
    #[default]
    Api,
    Local,
}

/// Runtime voice configuration — assembled from providers.stt / providers.tts.
/// NOT serialized to config file.
#[derive(Debug, Clone)]
pub struct VoiceConfig {
    pub stt_enabled: bool,
    pub stt_mode: SttMode,
    pub local_stt_model: String,
    pub stt_base_url: Option<String>,
    pub stt_model: Option<String>,
    pub stt_api_key: Option<String>,
    pub tts_enabled: bool,
    pub tts_mode: TtsMode,
    pub tts_voice: String,
    pub tts_model: String,
    pub tts_base_url: Option<String>,
    pub tts_api_key: Option<String>,
    pub local_tts_voice: String,
    pub stt_provider: Option<ProviderConfig>,
    pub tts_provider: Option<ProviderConfig>,
    pub voicebox_stt_enabled: bool,
    pub voicebox_stt_base_url: String,
    pub voicebox_tts_enabled: bool,
    pub voicebox_tts_base_url: String,
    pub voicebox_tts_profile_id: String,
    pub voicebox_tts_engine: String,
    /// User-defined STT fallback order. Empty means "use the default
    /// priority: voicebox → openai-compatible → groq → local". When the
    /// active provider fails (5xx, liveness probe error, unreachable),
    /// the dispatcher walks this list in order and tries each one that
    /// has the credentials/config it needs. Mirrors the
    /// completion-side `fallback_providers` chain so the user can
    /// codify "if my local voicebox is down, try Groq, then OpenAI".
    /// Values: `"voicebox"`, `"openai_compatible"`, `"groq"`, `"local"`.
    pub stt_fallback_chain: Vec<String>,
    /// User-defined TTS fallback order. Empty means "use the default
    /// priority: voicebox → openai-compatible → openai → local". Same
    /// semantics as `stt_fallback_chain` but for synthesis.
    /// Values: `"voicebox"`, `"openai_compatible"`, `"openai"`, `"local"`.
    pub tts_fallback_chain: Vec<String>,
}

fn default_local_stt_model() -> String {
    "local-tiny".to_string()
}
fn default_tts_voice() -> String {
    "echo".to_string()
}
fn default_tts_model() -> String {
    "gpt-4o-mini-tts".to_string()
}
fn default_local_tts_voice() -> String {
    "ryan".to_string()
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            stt_enabled: false,
            stt_mode: SttMode::default(),
            local_stt_model: default_local_stt_model(),
            stt_base_url: None,
            stt_model: None,
            stt_api_key: None,
            tts_enabled: false,
            tts_mode: TtsMode::default(),
            tts_voice: default_tts_voice(),
            tts_model: default_tts_model(),
            tts_base_url: None,
            tts_api_key: None,
            local_tts_voice: default_local_tts_voice(),
            stt_provider: None,
            tts_provider: None,
            voicebox_stt_enabled: false,
            voicebox_stt_base_url: default_voicebox_url(),
            voicebox_tts_enabled: false,
            voicebox_tts_base_url: default_voicebox_url(),
            voicebox_tts_profile_id: String::new(),
            voicebox_tts_engine: String::new(),
            stt_fallback_chain: Vec::new(),
            tts_fallback_chain: Vec::new(),
        }
    }
}

/// Image generation and vision configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageConfig {
    #[serde(default)]
    pub generation: ImageGenerationConfig,
    #[serde(default)]
    pub vision: ImageVisionConfig,
}

/// Image generation configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageGenerationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_image_model")]
    pub model: String,
    /// Loaded from keys.toml at runtime, never serialized to config.toml
    #[serde(skip, default)]
    pub api_key: Option<String>,
}

impl Default for ImageGenerationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: default_image_model(),
            api_key: None,
        }
    }
}

/// Image vision configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageVisionConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_image_model")]
    pub model: String,
    /// Loaded from keys.toml at runtime, never serialized to config.toml
    #[serde(skip, default)]
    pub api_key: Option<String>,
}

impl Default for ImageVisionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: default_image_model(),
            api_key: None,
        }
    }
}

pub fn default_image_model() -> String {
    "gemini-3.1-flash-image-preview".to_string()
}

/// Agent behaviour configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Approval policy: "ask", "auto-session", "auto-always"
    #[serde(default = "default_approval_policy")]
    pub approval_policy: String,

    /// Maximum concurrent tool calls
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,

    /// Context window limit in tokens (default: 200000)
    #[serde(default = "default_context_limit")]
    pub context_limit: u32,

    /// Max output tokens for API calls (default: 65536)
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,

    /// Default provider for spawned sub-agents (e.g., "openrouter", "anthropic", "custom:lmstudio").
    /// If unset, sub-agents inherit the parent session's active provider.
    #[serde(default)]
    pub subagent_provider: Option<String>,

    /// Default model for spawned sub-agents (e.g., "claude-sonnet-4-6").
    /// Only used when subagent_provider is set.
    #[serde(default)]
    pub subagent_model: Option<String>,

    /// Provider to use while a plan is being drafted, between `/plan` and
    /// approval (#792). Unset means planning runs on whatever the session is
    /// already using, which is the default and a true no-op.
    #[serde(default)]
    pub plan_provider: Option<String>,

    /// Model to use while a plan is being drafted. With `plan_provider` set,
    /// unset means that provider's default; on its own it swaps the model and
    /// keeps the current provider.
    #[serde(default)]
    pub plan_model: Option<String>,

    /// Provider to use while an approved plan executes, from approval until
    /// the plan is 100% complete (#793). Unset means execution runs on
    /// whatever the session is already using.
    #[serde(default)]
    pub execute_provider: Option<String>,

    /// Model to use while an approved plan executes. With `execute_provider`
    /// set, unset means that provider's default; on its own it swaps the model
    /// and keeps the current provider.
    #[serde(default)]
    pub execute_model: Option<String>,

    /// Route plan-task `start` through isolated execution (#908 option A):
    /// each started task runs in a freshly spawned child session that gets
    /// ONLY the task brief plus the parent's plan file threaded via
    /// `plan_session_override`. Default FALSE since Aug 26: inline execution
    /// is the default; the Aug 24-25 auto-isolation window showed per-item
    /// subagents demand operational maturity (conflict-free scopes, verdict
    /// discipline) before being safe as a silent default. Opt in per call
    /// (`isolated: true`) or here for genuine Ralph fan-out. An explicit
    /// `isolated` on plan start overrides this flag either way.
    #[serde(default = "default_plan_isolated_execution")]
    pub plan_isolated_execution: bool,
    /// Auto-start the next plan task when `complete` succeeds (#1195).
    /// Default FALSE: `complete` is a pure state transition - it marks the
    /// task done and reports the next eligible row as a hint, but never
    /// starts anything. Set true to restore the legacy cascade where
    /// completing a task immediately marks+surfaces the next one. Only
    /// an explicit `plan start` launches an isolated worker either way;
    /// this flag governs whether that launch may happen implicitly.
    #[serde(default = "default_plan_auto_start")]
    pub plan_auto_start: bool,

    /// #1903: how many hours an Active plan may sit untouched (its file's
    /// `updated_at`) before the harness stops injecting the full
    /// "[ACTIVE PLAN REMINDER]" execution nag into every user turn. The
    /// reminder is written for in-flight plans; a dead one is permanent
    /// prompt overhead the model cannot clear by itself, especially under
    /// CLI providers that bring their own toolset and have no native `plan`
    /// tool. Past the cutoff the block shrinks to a one-line stale note
    /// naming the human exits (`plan discard`, `plan start`). Set 0 to
    /// disable the cutoff and remind forever.
    #[serde(default = "default_plan_reminder_max_age_hours")]
    pub plan_reminder_max_age_hours: u64,

    /// Gate plan approval on an explicit human signal (#20). Default TRUE:
    /// `approval_policy = "auto-always"` does NOT satisfy the plan approval
    /// gate — plans stay Editing until the user Approves (plan-card button,
    /// `/execute`, reaction consent) or the agent approves under a
    /// user-granted in-session autonomy (`plan grant_autonomy`). Set FALSE
    /// only if you deliberately want the legacy auto-activation, where
    /// yolo / cron / run / a2a sessions go Active with no human approve.
    #[serde(default = "default_plan_require_approval")]
    pub plan_require_approval: bool,

    /// Whether isolated plan-task workers may themselves spawn sub-agents or
    /// background tasks (#1195). Default false: a plan worker executes one
    /// item solo - nested spawns orphan grandchildren and race the parent's
    /// disk verdict.
    #[serde(default)]
    pub plan_worker_allow_nested: bool,

    /// #1195 drift guard: when false (default), isolated plan workers are
    /// spawned READ-ONLY (#1173 grant) - they verify and report; mutations
    /// stay with the parent session. Set true only for write-isolation runs.
    #[serde(default)]
    pub plan_worker_allow_write: bool,

    /// Play a short notification sound when a plan reaches 100% at turn
    /// settle (#2255). Default FALSE: an agent that chirps unasked is
    /// noise, so this is opt-in. Only the WAV at
    /// `src/assets/sound/notification.wav` is used, only the TUI plays
    /// it, and no desktop notification or toast is ever raised.
    #[serde(default)]
    pub completion_notification_enabled: bool,

    /// Auto-install new releases on startup without prompting (default: true).
    /// When false, the user is shown an update prompt dialog instead.
    #[serde(default = "default_auto_update")]
    pub auto_update: bool,

    /// Allow an auto-update / `/evolve` binary swap while running as root
    /// (default: true). Kept true so an existing root daemon keeps updating and
    /// a forgotten flag never silently breaks updates (OC-03). A
    /// security-conscious operator can set this false to refuse root swaps; the
    /// TUI then explains what to do. The checksum verification (SHA256SUMS) is
    /// the real integrity gate and applies regardless of this flag.
    #[serde(default = "default_evolve_allow_root")]
    pub evolve_allow_root: bool,

    /// Days to keep a spawned sub-agent's session before it is pruned
    /// (default: 7). `0` disables pruning and keeps them forever.
    ///
    /// Every `spawn_agent` creates a session of its own, and nothing ever
    /// revisits them, so they accumulate along with their messages, tool
    /// executions and on-disk plan files (#931). They are hidden from the
    /// session list from the moment they are created; this only decides when
    /// they stop taking up space.
    #[serde(default = "default_subagent_session_ttl_days")]
    pub subagent_session_ttl_days: u32,

    /// Override provider for autonomous RSI self-improvement cycles (e.g. "zhipu", "minimax").
    /// RSI runs on its own provider chain so it never competes with chat or sub-agents for quota.
    /// When set, RSI jobs use this provider instead of the session's active one.
    #[serde(default)]
    pub self_improvement_provider: Option<String>,

    /// Master switch for the RSI autonomous engine (hourly self-improvement
    /// cycles with auto-approved tools). `None` (key absent) resolves by run
    /// mode: ON for the interactive TUI, OFF for headless daemons, because an
    /// unattended daemon burning provider quota and appending to brain files
    /// every hour is a bug, not a feature (#1063). Explicit `true`/`false`
    /// always wins, so a daemon operator who wants RSI opts in knowingly:
    /// ```toml
    /// [agent]
    /// rsi_enabled = true
    /// ```
    /// Re-checked every cycle from the live config mirror, so flipping it
    /// takes effect on the next cycle without a restart.
    #[serde(default)]
    pub rsi_enabled: Option<bool>,

    /// Override model for RSI self-improvement cycles. Only used when self_improvement_provider is set.
    /// Prefer cheap, fast models for autonomous analysis — results are deterministic.
    #[serde(default)]
    pub self_improvement_model: Option<String>,

    /// Ordered chain of provider names used for LIVE evaluations (the offline
    /// eval harness ignores this). Each name must match a configured provider;
    /// each judge uses that provider's own `default_model`. The chain serves as
    /// both a judge panel (independent verdicts, majority vote) and a resilience
    /// chain (a failing provider falls through to the next). Empty = live evals
    /// off. Example: `eval_providers = ["anthropic", "openrouter", "zhipu"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub eval_providers: Vec<String>,

    /// Default provider for main chat sessions. When set, new sessions use this
    /// provider instead of inheriting from the most recent session or falling back
    /// to the config priority list. When changed at runtime, existing sessions
    /// pick up the new default on next resume (via sync_provider_for_session).
    ///
    /// Example in config.toml:
    /// ```toml
    /// [agent]
    /// default_provider = "xiaomi"
    /// default_model = "mimo-v2.5-pro"
    /// ```
    #[serde(default)]
    pub default_provider: Option<String>,

    /// Default model for main chat sessions. Only used when default_provider is set.
    /// Matches the semantics of CronConfig's default_provider/default_model pair.
    #[serde(default)]
    pub default_model: Option<String>,

    /// Suppress the agent's playful post-compaction narration. Default
    /// `false` (= keep the personality moments). When true, the
    /// compaction-recovery prompts switch to a silent-continuation
    /// variant that tells the model to resume without acknowledging
    /// the compaction at all.
    ///
    /// Why default fun: users have specifically called out post-
    /// compaction one-liners as something they enjoy and forward to
    /// friends — emergent character per-language (e.g. Russian мат in
    /// frustration moments) generates the "this thing has personality"
    /// signal that's hard to fake. The flag exists for formal /
    /// corporate / customer-facing deployments where dropping mid-
    /// session profanity would be inappropriate.
    #[serde(default)]
    pub silent_compaction: bool,

    /// Render the `⏳ Compacting` / `✅ Compacted` progress pair in chat.
    /// Default `false` (= the hint only, no pair).
    ///
    /// Distinct from `silent_compaction`, which governs the model's
    /// post-compaction narration and deliberately stays `false` so the
    /// personality survives. This flag governs channel chrome: the two flow
    /// lines carrying percentages and elapsed time.
    ///
    /// Off does not mean blind. The anti-staleness header pin
    /// (`StreamingState::compacting`) is NOT gated here. No streaming chunks
    /// arrive for 10-60s while a background summariser runs, so without the pin
    /// the flow header would hold a stale "Working on…" preview for the whole
    /// silent window. The pin carries a bare "⏳ Compacting context…" with no
    /// numbers, which is exactly the hint.
    ///
    /// Telegram is the only surface that renders the pair: Discord's
    /// `Compacting` arm pings the native typing indicator with no text, and the
    /// TUI drops both events.
    #[serde(default)]
    pub compaction_notice: bool,

    /// Run auto-compaction in the background. **On by default.** The
    /// summariser call is spawned on a snapshot of the conversation and the
    /// turn keeps going; the summary is swapped in on a later budget check.
    /// Compaction becomes something the session reports afterwards instead of
    /// a minute the user spends watching nothing happen.
    ///
    /// The turn that follows a spawn always waits for the summary before it
    /// answers, so a reply is never computed against a context that is about
    /// to be replaced, and a context crossing the 80% ceiling waits too. The
    /// in-flight summary is never cancelled to make room: discarding it is
    /// what left a truncated context with no marker and looped two sessions
    /// on reload (2026-05-05).
    ///
    /// Set `background_compaction = false` to make every compaction block the
    /// turn that triggered it. `/compact` is always synchronous and visible:
    /// the user asked for it.
    #[serde(default = "default_background_compaction")]
    pub background_compaction: bool,

    /// Skill glob gate master switch (issue #150). **On by default.**
    /// When a skill declares `globs:` frontmatter, tool calls touching a
    /// matching path are rejected until the skill body is loaded in the
    /// current session context (fresh sessions AND post-compaction).
    /// Per-skill opt-out: delete the skill's `globs` key.
    #[serde(default = "default_skill_glob_gate")]
    pub skill_glob_gate: bool,

    /// Mechanical plan requirement. **On by default, 12 calls.**
    ///
    /// After this many tool calls in a single turn with no plan for the
    /// session, every further call is refused until the agent calls `plan`
    /// init. Prose in the brain files was the only thing asking for a plan
    /// before, and it was routinely skipped on research, audits and multi-file
    /// fixes; the gate makes the requirement mechanical instead of advisory.
    ///
    /// `plan` and `tool_search` always pass, so the agent can always reach the
    /// tool that unblocks it. Sub-agent sessions and cron runs are exempt: they
    /// execute a task someone else already planned, and a throwaway child plan
    /// nobody sees would be worse than no plan.
    ///
    /// Set 0 to disable the gate entirely.
    #[serde(default = "default_plan_gate_after_tool_calls")]
    pub plan_gate_after_tool_calls: usize,

    /// Lazy tool-schema loading. **On by default.** A request ships only the
    /// CORE tool schemas (~4k tokens) plus `tool_search`, instead of all ~95
    /// (~20k counted in every request's input); the agent calls `tool_search`
    /// to discover and activate extended tools on demand. Set
    /// `lazy_tools = false` to restore the old behaviour (all tool schemas in
    /// every request).
    #[serde(default = "default_lazy_tools")]
    pub lazy_tools: bool,

    /// Redact sensitive data (API keys, tokens, passwords, IPs) from tool
    /// outputs and display. **On by default** for safety. Set to `false`
    /// during sysadmin/devops work where seeing IPs, tokens, and passwords
    /// is necessary. When false, the agent will still warn about secrets
    /// in logs but won't redact them from display.
    #[serde(default = "default_redact_sensitive_data")]
    pub redact_sensitive_data: bool,

    /// Per-scope redaction override for GROUP/channel chats (#677). `None`
    /// follows `redact_sensitive_data` (the global default). Set via
    /// `/redact group on|off`. Redaction matters most here — others can see it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redact_group: Option<bool>,

    /// Per-scope redaction override for DIRECT messages (#677). `None` resolves
    /// to OFF — a DM is owner-private, so secrets are SHOWN by default. Set via
    /// `/redact dm on|off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redact_dm: Option<bool>,

    /// Enable debug file logging from config, on top of the `--debug` CLI flag
    /// (#678). Lets non-technical users (or the agent, on request) turn detailed
    /// file logs on/off by editing config.toml; the change hot-reloads without a
    /// restart. Always serialized as `false` so it is discoverable and flippable.
    /// Effective state is `--debug || debug_logs` — the flag always wins.
    #[serde(default = "default_debug_logs")]
    pub debug_logs: bool,

    /// Thinking-loop guard ceiling in seconds (#890), the global tier. If the
    /// model streams this long without emitting a single tool call, the guard
    /// fires. Catches the failure mode where a reasoning model loops internally
    /// (thinking tokens flowing) but never acts. Default: 600 (10 min). Set to
    /// 0 to disable.
    ///
    /// Two qualifiers as of #1690:
    ///
    /// * A provider may name its own ceiling via
    ///   `[providers.<name>] thinking_loop_timeout_secs`, which wins here. Long
    ///   reasoning runs are a property of the model, not of the agent, so one
    ///   slow provider must not inherit — or impose — this number globally.
    /// * The guard no longer discards a delivering stream. It kills only a
    ///   stream that has produced NO output at all; a stream that is writing
    ///   text but not calling tools stands the clock down and finishes, because
    ///   replaying a turn to lose a complete answer is the worse bug. The
    ///   killed stream is what triggers multi-language phantom enforcement in
    ///   the retry.
    #[serde(default = "default_thinking_loop_timeout_secs")]
    pub thinking_loop_timeout_secs: u64,

    /// Global non-streaming request ceiling, in seconds (#1688).
    ///
    /// The fallback for `[providers.<name>].timeout_secs`: a provider that
    /// names no ceiling of its own inherits this one, and if neither is set the
    /// family's compiled 300s default applies. Resolution lives in
    /// `config::timeout`.
    ///
    /// Like the per-provider key, this has NO effect on streaming. Streams run
    /// on a client built without a total timeout, so a healthy long turn is
    /// never cut by a wall clock (#1687). The only stream timer here is
    /// `stream_idle_timeout_secs`.
    ///
    /// ```toml
    /// [agent]
    /// timeout_secs = 120
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    /// Global inter-chunk streaming inactivity timeout, in seconds (#1688).
    ///
    /// The fallback for `[providers.<name>].stream_idle_timeout_secs`. Silence
    /// on a stream for longer than this is treated as a dead connection.
    ///
    /// There is deliberately no compiled floor for this flag: the default is
    /// picked at stream time (3600s for local and CLI targets, 20s for remote),
    /// so leaving it unset defers to that runtime choice instead of overriding
    /// it.
    ///
    /// ```toml
    /// [agent]
    /// stream_idle_timeout_secs = 45
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_secs: Option<u64>,

    /// Interval in seconds for injecting mid-turn time notices in long-running
    /// tool execution loops (#153). Default: 900 (15 min). Set to 0 to disable.
    #[serde(default = "default_time_marker_interval_secs")]
    pub time_marker_interval_secs: u64,

    /// Maximum turns allowed for a session goal before auto-pausing (#190).
    /// Default None (resolving to DEFAULT_MAX_TURNS=20).
    /// Can be set in config.toml:
    /// ```toml
    /// [agent]
    /// goal_max_turns = 30
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_max_turns: Option<u32>,

    /// Kill switch for the session_notify family (issue #1802, owner order).
    /// Default OFF: an unprompted channel into another session's context
    /// breaks session isolation, so cross-session notification must be an
    /// explicit operator opt-in. When false, the session_notify tool refuses
    /// every action, the A2A `session/notify` method refuses, and
    /// `opencrabs session notify` exits 4. Cross-session "notify" belongs to
    /// the channel send tools (telegram_send / slack_send) under explicit
    /// user request; opt in only for deliberate machine tooling fan-out.
    /// Post-mortem: issue #1203 / PR #1207.
    #[serde(default)]
    pub session_notify_enabled: bool,
}

impl AgentConfig {
    /// Resolve whether to redact for the current scope (#677). DMs are
    /// owner-private and default to NOT redacting; group/channel chats default
    /// to the global `redact_sensitive_data`. An explicit per-scope override
    /// (set via `/redact`) always wins.
    pub fn redact_for(&self, is_dm: bool) -> bool {
        if is_dm {
            self.redact_dm.unwrap_or(false)
        } else {
            self.redact_group.unwrap_or(self.redact_sensitive_data)
        }
    }
}

fn default_background_compaction() -> bool {
    true
}

fn default_skill_glob_gate() -> bool {
    true
}

/// Twelve calls is roughly where a turn stops being one step. Below it a
/// read-edit-verify cycle is still a single action; above it the turn is
/// doing multi-step work that belongs in a checklist.
fn default_plan_gate_after_tool_calls() -> usize {
    12
}

fn default_lazy_tools() -> bool {
    true
}

fn default_redact_sensitive_data() -> bool {
    true
}

fn default_debug_logs() -> bool {
    false
}

fn default_thinking_loop_timeout_secs() -> u64 {
    600
}

fn default_time_marker_interval_secs() -> u64 {
    crate::brain::agent::service::tool_loop::DEFAULT_TIME_MARKER_INTERVAL_SECS
}

fn default_approval_policy() -> String {
    "auto-always".to_string()
}

fn default_component_ttl_hours() -> f64 {
    24.0
}

fn default_max_concurrent() -> u32 {
    4
}

fn default_context_limit() -> u32 {
    200_000
}

fn default_max_tokens() -> u32 {
    65536
}

fn default_evolve_allow_root() -> bool {
    true
}

fn default_auto_update() -> bool {
    true
}

fn default_plan_isolated_execution() -> bool {
    // Default FALSE since #1195 follow-up (Aug 26): the Aug 24-25 incident
    // showed hidden per-item workers carry more operational risk than value
    // by default (drift, duplicate work, poisoned checklists). Inline remains
    // the default; isolation stays one explicit `isolated:true` (or this key)
    // away for genuine Ralph fan-out.
    false
}

fn default_plan_auto_start() -> bool {
    false
}

fn default_plan_reminder_max_age_hours() -> u64 {
    // #1903: one day without movement on the plan file and the full
    // execution nag stops riding every turn.
    24
}

fn default_plan_require_approval() -> bool {
    // #20: the plan approval gate is never satisfied by the tool
    // auto-approve policy unless the operator explicitly opts out.
    true
}

fn default_subagent_session_ttl_days() -> u32 {
    7
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            approval_policy: default_approval_policy(),
            max_concurrent: default_max_concurrent(),
            context_limit: default_context_limit(),
            max_tokens: default_max_tokens(),
            subagent_provider: None,
            subagent_model: None,
            plan_provider: None,
            plan_model: None,
            execute_provider: None,
            execute_model: None,
            plan_isolated_execution: default_plan_isolated_execution(),
            plan_auto_start: default_plan_auto_start(),
            plan_reminder_max_age_hours: default_plan_reminder_max_age_hours(),
            plan_require_approval: default_plan_require_approval(),
            plan_worker_allow_nested: false,
            plan_worker_allow_write: false,
            completion_notification_enabled: false,
            auto_update: default_auto_update(),
            evolve_allow_root: default_evolve_allow_root(),
            session_notify_enabled: false,
            subagent_session_ttl_days: default_subagent_session_ttl_days(),
            self_improvement_provider: None,
            rsi_enabled: None,
            self_improvement_model: None,
            eval_providers: Vec::new(),
            default_provider: None,
            default_model: None,
            silent_compaction: false,
            compaction_notice: false,
            background_compaction: default_background_compaction(),
            skill_glob_gate: default_skill_glob_gate(),
            plan_gate_after_tool_calls: default_plan_gate_after_tool_calls(),
            lazy_tools: default_lazy_tools(),
            redact_sensitive_data: default_redact_sensitive_data(),
            redact_group: None,
            redact_dm: None,
            debug_logs: default_debug_logs(),
            thinking_loop_timeout_secs: default_thinking_loop_timeout_secs(),
            timeout_secs: None,
            stream_idle_timeout_secs: None,
            time_marker_interval_secs: default_time_marker_interval_secs(),
            goal_max_turns: None,
        }
    }
}

/// Cron job default settings.
///
/// When a cron job has no `provider` or `model` set, these defaults are used
/// instead of the system's active provider. Useful for routing cron jobs to
/// cheaper providers while keeping the interactive session on a premium one.
///
/// Example in config.toml:
/// ```toml
/// [cron]
/// default_provider = "minimax"
/// default_model = "MiniMax-M2.7"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CronConfig {
    /// Default provider for cron jobs without an explicit provider
    #[serde(default)]
    pub default_provider: Option<String>,

    /// Default model for cron jobs without an explicit model
    #[serde(default)]
    pub default_model: Option<String>,
}

/// One tier of the decision pyramid (#1648). A tier is a decision-shaped
/// question the operator has declared worth caching: same canonical input
/// + same policy version must give the same answer.
///
/// ```toml
/// [decisions.tiers.triage]
/// policy_version = "p1"        # REQUIRED — bump when the decision semantics change
/// mode = "shadow"              # shadow (default) | live | off
/// ttl_hours = 168              # optional; older rows miss and are swept (PR3)
/// margin_floor = 0.2           # optional, default 0.0; sub-floor margins never cached
/// similarity = false           # L2 reuse, not implemented — true is a load error
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionTierConfig {
    /// Semantic version of THIS tier's decision policy. Missing or empty is
    /// a named load error: without it, a policy change silently reuses
    /// stale decisions, which is the one failure the ring must never allow.
    #[serde(default)]
    pub policy_version: String,

    /// Reuse posture for this tier.
    #[serde(default)]
    pub mode: DecisionMode,

    /// Rows older than this miss (and will be pruned by the PR3 sweeper).
    /// `None` = no expiry on reads; keep one set so pruning has a bound.
    #[serde(default)]
    pub ttl_hours: Option<i64>,

    /// Write gate: a reported margin below this is never cached. Default
    /// 0.0 caches everything measurable; a real deployment should raise it
    /// (e.g. 0.2) so borderline decisions stay live.
    #[serde(default)]
    pub margin_floor: f64,

    /// Similarity (L2) reuse. Unimplemented; `true` is rejected at load so
    /// an operator never believes a fuzzy ring is on.
    #[serde(default)]
    pub similarity: bool,
}

impl Default for DecisionTierConfig {
    fn default() -> Self {
        Self {
            policy_version: String::new(),
            mode: DecisionMode::default(),
            ttl_hours: None,
            margin_floor: 0.0,
            similarity: false,
        }
    }
}

/// How a tier uses its cache. Default is `Shadow`: keys computed, would-hits
/// counted, the model still always asked — the release-day evaluation is
/// built on shadow evidence, and promotion to `Live` is a per-deployment
/// operator decision, never a code default (#1648 kill rule).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DecisionMode {
    #[default]
    Shadow,
    Live,
    Off,
}

/// The `[decisions]` section: a map of named tiers. No global knobs in
/// v1 — a tier with no config entry does not exist, and asking for it is
/// a named error, never an implicit default.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DecisionsConfig {
    /// Named tiers: `[decisions.tiers.<name>]`.
    #[serde(default)]
    pub tiers: std::collections::BTreeMap<String, DecisionTierConfig>,
}

/// OpenAI-compatible embedding provider configuration.
///
/// When set, embeddings are generated via an HTTP API call instead of the
/// local GGUF model (embeddinggemma-300M). This eliminates the ~300MB model
/// download and ~2.9GB RAM overhead of llama.cpp.
///
/// Supports any OpenAI-compatible `/v1/embeddings` endpoint:
/// OpenAI, Ollama, LM Studio, localai, etc.
///
/// Example in config.toml:
/// ```toml
/// [memory.embedding]
/// url = "https://api.openai.com/v1"
/// model = "text-embedding-3-small"
/// # api_key loaded from keys.toml: [providers.memory_embedding] api_key = "sk-..."
/// # dimensions = 1536   # auto-detected from first API response if unset
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EmbeddingConfig {
    /// OpenAI-compatible API base URL (e.g. "https://api.openai.com/v1").
    /// The `/embeddings` path is appended automatically.
    #[serde(default)]
    pub url: Option<String>,

    /// Embedding model name (e.g. "text-embedding-3-small", "nomic-embed-text").
    #[serde(default)]
    pub model: Option<String>,

    /// API key for the embedding endpoint.
    /// Also loaded from keys.toml under `[providers.memory_embedding]`.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Embedding vector dimensions.
    /// Auto-detected from the first API response if unset.
    /// Local GGUF model always produces 768-dim vectors.
    #[serde(default)]
    pub dimensions: Option<usize>,
}

/// Memory / embedding configuration.
///
/// Controls whether vector embeddings are enabled for semantic memory search.
/// When disabled, only FTS5 (keyword) search is used.
///
/// OFF by default (#1798): the native GGUF/llama.cpp engine hard-aborts on
/// VPS and Windows hosts mid-index (exit 0xC0000409), taking the daemon down
/// with it. Local embeddings still run well on Apple Silicon and most Linux
/// desktops, so they are a deliberate opt-in here; startup VPS detection also
/// writes an explicit `vector_enabled = false` on cloud hosts as a safety net.
///
/// When `vector_enabled = true`, embeddings can be generated either:
/// - **Locally**: via embeddinggemma-300M GGUF model (default, no config needed)
/// - **Via API**: by configuring `[memory.embedding]` with an OpenAI-compatible endpoint
///
/// Example in config.toml:
/// ```toml
/// [memory]
/// vector_enabled = true
///
/// [memory.embedding]
/// url = "https://api.openai.com/v1"
/// model = "text-embedding-3-small"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Whether vector embeddings are enabled (default: false; opt in with
    /// `vector_enabled = true`, see #1798)
    #[serde(default = "default_vector_enabled")]
    pub vector_enabled: bool,

    /// OpenAI-compatible embedding provider. When set, embeddings are generated
    /// via API instead of the local GGUF model. Eliminates ~300MB download + ~2.9GB RAM.
    #[serde(default)]
    pub embedding: Option<EmbeddingConfig>,

    /// External filesystem paths indexed into the `external` collection (#1051).
    /// Entries are bare path strings or `{ path, pattern }` tables. Relative
    /// paths resolve against the OpenCrabs home, not the session cwd, so the
    /// index stays stable across `/cd` and profile switches.
    #[serde(default)]
    pub extra_paths: Vec<ExtraPath>,

    /// Glob patterns excluded from external indexing (#1051), global for all
    /// extra paths. Defaults cover VCS/build dirs and common secret files;
    /// the session gate is the real security boundary, this is defense in
    /// depth.
    #[serde(default = "default_external_excludes")]
    pub exclude: Vec<String>,

    /// Allow `scope="external"` results in shared/group sessions (#1051).
    /// Default-deny: external content inherits memory_search's exposure
    /// surface, so it stays main/owner-session-only unless opted in.
    #[serde(default)]
    pub external_allowed_in_shared: bool,

    /// Allow the internal memory surfaces (`scope="brain"`/`scope="memory"`
    /// results, personal-context brain files through `load_brain_file`, and
    /// the per-turn MEMORY.md recall) in shared/group sessions (#1957).
    /// Default-deny, mirroring `external_allowed_in_shared`: the prompt
    /// rule ("MEMORY.md … load/write only in the MAIN session, never in
    /// shared/group chats") was prose-only, and group-readable sessions
    /// could still retrieve the owner's personal context off every surface
    /// except `external`. Opt in per instance when the group IS the owner's.
    #[serde(default)]
    pub internal_allowed_in_shared: bool,

    /// Seconds between external-path freshness sweeps (#1051). The sweep
    /// discovers added/removed files and reconciles config changes; modified
    /// files are caught lazily at search time regardless.
    #[serde(default = "default_sweep_interval_secs")]
    pub sweep_interval_secs: u64,

    /// How often to sweep for documents that still have no embedding, in
    /// seconds. `0` disables the sweep. Default 300.
    ///
    /// Embedding used to happen only at startup and on write. A backfill that
    /// failed (a bad key, an endpoint that was down) left the vector table at
    /// zero until someone restarted the process, and nothing said so: search
    /// silently degraded to keyword-only FTS (#1069). The sweep re-reads
    /// config.toml on every tick, so a key fixed mid-session takes effect
    /// without a restart.
    #[serde(default = "default_backfill_interval_secs")]
    pub backfill_interval_secs: u64,
}

/// One external index path (#1051): a bare string or `{ path, pattern }`.
/// Untagged so both forms parse from the same TOML array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtraPath {
    /// Bare path: indexed with the default pattern `**/*.md`.
    Simple(String),
    /// Path with an explicit glob, matched against root-relative paths.
    WithPattern {
        path: String,
        #[serde(default = "default_extra_pattern")]
        pattern: String,
    },
}

impl ExtraPath {
    /// The configured path, either form.
    pub fn path(&self) -> &str {
        match self {
            ExtraPath::Simple(p) | ExtraPath::WithPattern { path: p, .. } => p,
        }
    }

    /// The glob pattern, defaulting to `**/*.md` for bare entries.
    pub fn pattern(&self) -> &str {
        match self {
            ExtraPath::Simple(_) => "**/*.md",
            ExtraPath::WithPattern { pattern, .. } => pattern,
        }
    }
}

fn default_extra_pattern() -> String {
    "**/*.md".to_string()
}

const fn default_sweep_interval_secs() -> u64 {
    300
}

fn default_external_excludes() -> Vec<String> {
    vec![
        ".git".to_string(),
        "node_modules".to_string(),
        "target".to_string(),
        "dist".to_string(),
        "build".to_string(),
        "vendor".to_string(),
        "__pycache__".to_string(),
        ".env*".to_string(),
        "*.key".to_string(),
        "*.pem".to_string(),
        ".ssh/**".to_string(),
        "*credential*".to_string(),
    ]
}

/// #1798: vector embeddings are OFF by default on every platform.
///
/// The native GGUF/llama.cpp engine aborts (0xC0000409) mid-index on
/// VPS and Windows hosts and used to take the daemon down with it; that
/// crash is deliberate won't-fix territory, so local embeddings are now an
/// explicit opt-in (`vector_enabled = true`), which still runs well on
/// Apple Silicon and capable Linux desktops.
const fn default_vector_enabled() -> bool {
    false
}

const fn default_backfill_interval_secs() -> u64 {
    300
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            vector_enabled: default_vector_enabled(),
            embedding: None,
            extra_paths: Vec::new(),
            exclude: default_external_excludes(),
            external_allowed_in_shared: false,
            internal_allowed_in_shared: false,
            sweep_interval_secs: default_sweep_interval_secs(),
            backfill_interval_secs: default_backfill_interval_secs(),
        }
    }
}

impl MemoryConfig {
    /// Detect whether we're running on a VPS/cloud instance.
    ///
    /// Heuristics:
    /// - `/proc/1/cgroup` contains "container" or cloud provider strings
    /// - `/sys/class/dmi/id/product_name` contains cloud vendor names
    /// - Total system RAM is below 2GB
    /// - No display server detected (no DISPLAY/WAYLAND_DISPLAY env vars)
    fn is_vps() -> bool {
        #[cfg(target_os = "linux")]
        {
            // Check /sys/class/dmi/id/product_name for cloud vendor strings
            if let Ok(product) = std::fs::read_to_string("/sys/class/dmi/id/product_name") {
                let product = product.to_lowercase();
                let cloud_vendors = [
                    "droplet",
                    "digitalocean",
                    "ec2",
                    "amazon",
                    "gce",
                    "google compute",
                    "kvm",
                    "vultr",
                    "linode",
                    "akamai",
                    "azure",
                    "hyper-v",
                    "oracle",
                    "oci",
                ];
                for vendor in &cloud_vendors {
                    if product.contains(vendor) {
                        return true;
                    }
                }
            }
            // Check for container environment
            if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup")
                && (cgroup.contains("docker")
                    || cgroup.contains("containerd")
                    || cgroup.contains("kubepods"))
            {
                return true;
            }

            // Check system RAM — if less than 2GB, likely a small VPS
            if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
                for line in meminfo.lines() {
                    if line.starts_with("MemTotal:") {
                        // MemTotal is in kB
                        if let Some(kb_str) = line.split_whitespace().nth(1)
                            && let Ok(kb) = kb_str.parse::<u64>()
                            && {
                                let gb = kb / 1_048_576; // kB to GB
                                gb < 2
                            }
                        {
                            return true;
                        }
                        break;
                    }
                }
            }

            // No display server — likely headless server
            let has_display =
                std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok();
            if !has_display {
                return true;
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            // Non-Linux (macOS, Windows) — assume desktop, not VPS
        }

        false
    }

    /// Auto-apply VPS defaults if detected and config doesn't already have [memory] section.
    /// Returns true if config was modified.
    pub fn auto_apply_vps_defaults() -> bool {
        if !Self::is_vps() {
            return false;
        }

        // Check if [memory] section already exists in config.toml
        let config_path = opencrabs_home().join("config.toml");
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            // If user already has a [memory] section, don't override
            if content.contains("[memory]") {
                return false;
            }
        }

        // Append [memory] section to config.toml
        tracing::info!(
            "VPS/cloud detected — disabling vector embeddings for memory search (FTS-only mode)"
        );

        let append = "\n# Auto-configured: VPS/cloud detected\n\
                      # Local vector embeddings disabled to save RAM (~2.9GB).\n\
                      # FTS5 keyword search still works. WIP: OpenAI-compatible\n\
                      # embedding through API coming soon.\n\
                      [memory]\n\
                      vector_enabled = false\n";

        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&config_path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, append.as_bytes()));

        true
    }
}

/// Debug configuration options
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DebugConfig {
    /// Enable LSP debug logging
    #[serde(default)]
    pub debug_lsp: bool,

    /// Enable profiling
    #[serde(default)]
    pub profiling: bool,

    /// Write per-keystroke detail to the desktop window's log
    /// (`~/.opencrabs/logs/opencrabs-gui.log`). The `OPENCRABS_GUI_DEBUG`
    /// environment variable forces it on regardless of this value.
    #[serde(default)]
    pub gui_debug_logs: bool,
}

/// LLM Provider configurations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfigs {
    /// Anthropic configuration
    #[serde(default)]
    pub anthropic: Option<ProviderConfig>,

    /// OpenAI configuration (official API)
    #[serde(default)]
    pub openai: Option<ProviderConfig>,

    /// OpenRouter configuration
    #[serde(default)]
    pub openrouter: Option<ProviderConfig>,

    /// Minimax configuration
    #[serde(default)]
    pub minimax: Option<ProviderConfig>,

    /// z.ai (GLM) configuration (supports API and Coding endpoints).
    /// Legacy section name `zhipu` still loads via the serde alias (#1448).
    #[serde(default, alias = "zhipu")]
    pub zai: Option<ProviderConfig>,

    /// Moonshot AI (Kimi) configuration. Supports the API plan and the Coding
    /// (token) plan via `endpoint_type`:
    ///
    /// - `"api"` (default) → `https://api.moonshot.ai/v1` pay-per-token
    /// - `"coding"` → `https://api.kimi.com/coding/v1` (Kimi Code token plan:
    ///   k3, kimi-for-coding, kimi-for-coding-highspeed)
    #[serde(default)]
    pub moonshot: Option<ProviderConfig>,

    /// Xiaomi MiMo configuration. OpenAI-compatible and keyed: the user
    /// supplies an API key from platform.xiaomimimo.com.
    ///
    /// Absent from the TOML means absent, exactly as for every other provider.
    /// It once defaulted to a populated, enabled section, from the window when
    /// MiMo shipped as the keyless default provider. That window closed and the
    /// default outlived it, so every config that did not name xiaomi parsed as
    /// though it had enabled one, and any whole-file rewrite then wrote that
    /// section to disk over providers the user had actually chosen. The picker
    /// never needed it: `load_default_models` reads each provider's catalogue
    /// from the embedded `config.toml.example`, where xiaomi ships disabled.
    #[serde(default)]
    pub xiaomi: Option<ProviderConfig>,

    /// Built-in OpenCrabs provider. The user never types its key: onboarding
    /// asks the inference proxy for one token per install, and that token is
    /// merged in from keys.toml ([providers.opencrabs] api_key). The install ID
    /// stored beside it is not read here.
    #[serde(default)]
    pub opencrabs: Option<ProviderConfig>,

    /// Named custom OpenAI-compatible providers (e.g. [providers.custom.ollama])
    #[serde(default, deserialize_with = "deserialize_custom_providers")]
    pub custom: Option<BTreeMap<String, ProviderConfig>>,

    /// GitHub Copilot configuration (uses OAuth device flow token)
    #[serde(default)]
    pub github: Option<ProviderConfig>,

    /// Google Gemini configuration
    #[serde(default)]
    pub gemini: Option<ProviderConfig>,

    /// Claude CLI (Max subscription) — direct subprocess, no proxy needed
    #[serde(default)]
    pub claude_cli: Option<ProviderConfig>,

    /// OpenCode CLI — direct subprocess, access to opencode's free models
    #[serde(default)]
    pub opencode_cli: Option<ProviderConfig>,

    /// Codex CLI (ChatGPT/Codex subscription) — direct subprocess, no API key needed
    #[serde(default)]
    pub codex_cli: Option<ProviderConfig>,
    /// Command Code CLI (`command-code`) subprocess provider — no API key needed
    #[serde(default)]
    pub command_code_cli: Option<ProviderConfig>,

    /// Codex OAuth — native device-code flow, stores tokens in ~/.opencrabs/auth/codex.json
    #[serde(default)]
    pub codex: Option<ProviderConfig>,

    /// OpenCode API — native provider for Go and Zen plans (opencode.ai)
    #[serde(default)]
    pub opencode: Option<ProviderConfig>,

    /// Qwen (DashScope OpenAI-compatible) — standard API-key provider.
    #[serde(default)]
    pub qwen: Option<ProviderConfig>,

    /// Ollama — local or cloud (api.ollama.com). Auto-detects local models via /api/tags.
    #[serde(default)]
    pub ollama: Option<ProviderConfig>,

    /// AWS Bedrock configuration
    #[serde(default)]
    pub bedrock: Option<ProviderConfig>,

    /// VertexAI configuration
    #[serde(default)]
    pub vertex: Option<ProviderConfig>,

    /// STT (Speech-to-Text) provider configurations
    #[serde(default)]
    pub stt: Option<SttProviders>,

    /// TTS (Text-to-Speech) provider configurations
    #[serde(default)]
    pub tts: Option<TtsProviders>,

    /// Web search provider configurations
    #[serde(default)]
    pub web_search: Option<WebSearchProviders>,

    /// Image provider configurations (e.g. [providers.image.gemini])
    #[serde(default)]
    pub image: Option<ImageProviders>,

    /// Fallback provider configuration (under [providers.fallback] in config)
    #[serde(default)]
    pub fallback: Option<FallbackProviderConfig>,
}

impl ProviderConfigs {
    /// Get the first enabled custom provider that is actually usable.
    ///
    /// `enabled` alone is not enough. Built-in providers are already required
    /// to carry a key before they can be selected, but custom ones were picked
    /// on the flag by itself, so a provider that was merely present outranked
    /// the one the user configured (#917). The map is a `BTreeMap`, so "first"
    /// means alphabetically first, and a shipped example named early in the
    /// alphabet won every time.
    ///
    /// A custom provider is only a candidate once it has a base URL, since
    /// there is nowhere to send a request without one.
    pub fn active_custom(&self) -> Option<(&str, &ProviderConfig)> {
        self.custom
            .as_ref()?
            .iter()
            .find(|(_, cfg)| cfg.enabled && Self::custom_is_configured(cfg))
            .map(|(name, cfg)| (name.as_str(), cfg))
    }

    /// Whether a custom provider carries enough to be used at all.
    ///
    /// Deliberately minimal: a base URL is the one field without which no
    /// request can be made. Demanding more here would silently drop working
    /// local setups, which need no key.
    fn custom_is_configured(cfg: &ProviderConfig) -> bool {
        cfg.base_url.as_ref().is_some_and(|u| !u.trim().is_empty())
    }

    /// Get a specific custom provider by name (case-insensitive, normalized)
    pub fn custom_by_name(&self, name: &str) -> Option<&ProviderConfig> {
        let normalized = normalize_toml_key(name);
        self.custom.as_ref()?.get(&normalized)
    }

    /// Single source of truth for built-in provider iteration. Both
    /// `active_provider_and_model` (factory routing) and
    /// `resolve_provider_from_config` (display) walk this list, so adding a
    /// new provider field above only needs ONE new entry here — no more
    /// hardcoded if-else ladders silently omitting providers (the bug that
    /// hid `opencode`, `ollama`, `bedrock`, `vertex` from the TUI display
    /// for months).
    ///
    /// Tuple shape: `(session_id, display_name, requires_api_key, &Option<ProviderConfig>)`.
    /// `requires_api_key=false` for CLI providers where `enabled=true`
    /// alone activates them (claude-cli, opencode-cli, codex-cli, codex
    /// OAuth — the latter stores tokens in `~/.opencrabs/auth/`).
    ///
    /// Priority order matches what `factory::create_provider` would pick:
    /// CLI providers first (free, no key), then API providers, with custom
    /// providers handled separately by the caller via `active_custom()`.
    pub(crate) fn provider_registry(
        &self,
    ) -> [(&'static str, &'static str, bool, Option<&ProviderConfig>); 20] {
        [
            // Xiaomi MiMo — keyed (requires_api_key = true): the user supplies
            // an API key from platform.xiaomimimo.com. An enabled-but-keyless
            // section is correctly skipped here so it never becomes a broken
            // default.
            ("xiaomi", "Xiaomi", true, self.xiaomi.as_ref()),
            // CLI providers — enabled flag alone is enough
            ("claude-cli", "Claude CLI", false, self.claude_cli.as_ref()),
            (
                "opencode-cli",
                "OpenCode CLI",
                false,
                self.opencode_cli.as_ref(),
            ),
            ("codex-cli", "Codex CLI", false, self.codex_cli.as_ref()),
            (
                "command-code-cli",
                "Command Code CLI",
                false,
                self.command_code_cli.as_ref(),
            ),
            ("codex", "Codex OAuth", false, self.codex.as_ref()),
            // OpenCode API — OAuth-backed but registered as a regular provider
            ("opencode", "OpenCode", false, self.opencode.as_ref()),
            // API providers — require api_key in addition to enabled
            ("qwen", "Qwen", true, self.qwen.as_ref()),
            ("minimax", "Minimax", true, self.minimax.as_ref()),
            ("zai", "z.ai", true, self.zai.as_ref()),
            ("moonshot", "Moonshot AI", true, self.moonshot.as_ref()),
            ("openrouter", "OpenRouter", true, self.openrouter.as_ref()),
            ("anthropic", "Anthropic", true, self.anthropic.as_ref()),
            ("openai", "OpenAI", true, self.openai.as_ref()),
            ("github", "GitHub Copilot", true, self.github.as_ref()),
            ("gemini", "Google Gemini", true, self.gemini.as_ref()),
            ("ollama", "Ollama", false, self.ollama.as_ref()),
            ("bedrock", "AWS Bedrock", true, self.bedrock.as_ref()),
            ("vertex", "Google Vertex", true, self.vertex.as_ref()),
            // Built-in OpenCrabs — keyed by the install token, which is not typed.
            ("opencrabs", "OpenCrabs", true, self.opencrabs.as_ref()),
        ]
    }

    /// Return `(provider_name, default_model)` for the currently active provider,
    /// using the same priority order as `factory::create_provider`.
    ///
    /// Walks `provider_registry()` in priority order and returns the first
    /// entry that is enabled and (if `requires_api_key`) has an API key.
    /// Falls through to the first active custom provider, otherwise
    /// `("none", "none")`.
    /// Has the user actually DECLARED this provider — is there a section for
    /// it in their config?
    ///
    /// Deliberately different from `factory::is_known_provider_name`, which
    /// answers "does this software support such a provider". That is too broad
    /// for deciding whether `<x>/<model>` is a provider prefix: `anthropic` is
    /// both a provider id and an OpenRouter vendor, so the registry answer
    /// routes a valid OpenRouter model id at a provider the user may never
    /// have set up (#939).
    ///
    /// Declared, not enabled: naming a provider explicitly is how a user points
    /// at one that is not currently active, so requiring `enabled` would break
    /// the case the prefix exists for.
    pub fn is_declared(&self, name: &str) -> bool {
        let bare = name.strip_prefix("custom:").unwrap_or(name);
        if self
            .custom
            .as_ref()
            .is_some_and(|m| m.contains_key(bare) || m.contains_key(&normalize_toml_key(bare)))
        {
            return true;
        }
        // Resolve aliases (`claude_cli` -> `claude-cli`) before consulting the
        // registry, which is the single source of truth for what exists.
        let Some(canonical) = crate::utils::providers::find_provider_meta(name).map(|m| m.id)
        else {
            return false;
        };
        self.provider_registry()
            .iter()
            .any(|(id, _, _, cfg)| *id == canonical && cfg.is_some())
    }

    /// Whether the named provider is configured, enabled and has the
    /// credentials to run (#977). Alias resolution mirrors `is_declared`;
    /// health additionally demands `enabled` and a real API key where one
    /// is required. `create_provider_by_name` remains the final arbiter —
    /// this is the cheap pre-filter for resolution ladders.
    pub fn is_healthy(&self, name: &str) -> bool {
        // Custom providers: the factory refuses them without a real key
        // ("requests will fail authentication"), so keyless ≠ healthy.
        let bare = name.strip_prefix("custom:").unwrap_or(name);
        if let Some(customs) = self.custom.as_ref()
            && let Some(cfg) = customs
                .get(bare)
                .or_else(|| customs.get(&normalize_toml_key(bare)))
        {
            return cfg.enabled
                && cfg
                    .api_key
                    .as_deref()
                    .is_some_and(crate::config::stored_key::is_real_key);
        }
        // Known providers: same alias ladder as `is_declared`, plus
        // enabled + key.
        let Some(canonical) = crate::utils::providers::find_provider_meta(name).map(|m| m.id)
        else {
            return false;
        };
        self.provider_registry()
            .iter()
            .any(|(id, _, requires_key, cfg)| {
                *id == canonical
                    && cfg.is_some_and(|c| c.enabled && (!requires_key || c.api_key.is_some()))
            })
    }

    pub fn active_provider_and_model(&self) -> (String, String) {
        for (id, _display, requires_api_key, cfg) in self.provider_registry() {
            if let Some(c) = cfg
                && c.enabled
                && (!requires_api_key || c.api_key.is_some())
            {
                let model = c
                    .default_model
                    .clone()
                    .unwrap_or_else(|| "(default)".to_string());
                return (id.to_string(), model);
            }
        }
        if let Some((name, cfg)) = self.active_custom() {
            let model = cfg
                .default_model
                .clone()
                .unwrap_or_else(|| "(default)".to_string());
            return (format!("custom:{}", name), model);
        }
        ("none".to_string(), "none".to_string())
    }
}

/// Custom deserializer that handles both old flat format `[providers.custom]`
/// and new named map format `[providers.custom.<name>]`.
fn deserialize_custom_providers<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<BTreeMap<String, ProviderConfig>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    let value: Option<toml::Value> = Option::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };

    // Check if there are nested tables (named providers like [providers.custom.nvidia])
    // alongside top-level keys (flat format like [providers.custom] with enabled/api_key).
    // If both exist, extract the flat keys as "default" and parse named tables separately.
    let table = match value.as_table() {
        Some(t) => t,
        None => return Ok(None),
    };

    let flat_keys = ["enabled", "api_key", "base_url", "default_model", "models"];
    let has_flat = flat_keys.iter().any(|k| table.contains_key(*k));
    let has_named = table.values().any(|v| v.is_table());

    if has_flat && has_named {
        // Mixed: flat "default" provider + named providers in same section
        let mut map = BTreeMap::new();
        let mut flat_table = toml::map::Map::new();
        for key in &flat_keys {
            if let Some(v) = table.get(*key) {
                flat_table.insert(key.to_string(), v.clone());
            }
        }
        let default_cfg: ProviderConfig = toml::Value::Table(flat_table)
            .try_into()
            .map_err(de::Error::custom)?;
        map.insert("default".to_string(), default_cfg);
        for (name, val) in table {
            if flat_keys.contains(&name.as_str()) {
                continue;
            }
            if val.is_table() {
                let cfg: ProviderConfig = val.clone().try_into().map_err(de::Error::custom)?;
                map.insert(normalize_toml_key(name), cfg);
            }
        }
        Ok(Some(map))
    } else if has_flat {
        // Pure flat format — wrap as "default"
        let config: ProviderConfig = toml::Value::Table(table.clone())
            .try_into()
            .map_err(de::Error::custom)?;
        let mut map = BTreeMap::new();
        map.insert("default".to_string(), config);
        Ok(Some(map))
    } else {
        // Pure named map format — normalize keys on load
        let raw: BTreeMap<String, ProviderConfig> = toml::Value::Table(table.clone())
            .try_into()
            .map_err(de::Error::custom)?;
        let map: BTreeMap<String, ProviderConfig> = raw
            .into_iter()
            .map(|(k, v)| (normalize_toml_key(&k), v))
            .collect();
        Ok(if map.is_empty() { None } else { Some(map) })
    }
}

/// Fallback provider configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FallbackProviderConfig {
    /// Enable fallback
    #[serde(default)]
    pub enabled: bool,

    /// Legacy: single fallback provider type (backwards compat)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Ordered list of fallback provider names — tried in sequence on failure.
    /// Each name must match a configured provider (e.g. "anthropic", "openrouter").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,

    /// Ordered list of provider names to check for `vision_model` before
    /// falling back to the default REGISTRATIONS scan.  Each name must
    /// match a configured provider (e.g. "minimax", "anthropic").
    /// Empty = no override, scan all providers as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vision: Vec<String>,

    /// Ordered list of provider names to try for `generate_image`,
    /// mirroring the `vision` chain but resolved over each provider's
    /// `generation_model` (#1672). The session's current provider is always
    /// tried first; this chain extends the roll, it does not replace it.
    /// Global `[image.generation]` Gemini stays the last resort.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub generation: Vec<String>,
}

/// STT (Speech-to-Text) provider configurations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SttProviders {
    /// Groq STT configuration ([providers.stt.groq])
    #[serde(default)]
    pub groq: Option<ProviderConfig>,

    /// Local whisper.cpp STT configuration ([providers.stt.local])
    #[serde(default)]
    pub local: Option<LocalSttConfig>,

    /// OpenAI-compatible STT configuration ([providers.stt.openai_compatible])
    #[serde(default)]
    pub openai_compatible: Option<OpenaiCompatibleSttConfig>,

    /// Voicebox STT configuration ([providers.stt.voicebox])
    #[serde(default)]
    pub voicebox: Option<VoiceboxSttConfig>,

    /// User-defined STT fallback order. Empty/None means "use the default
    /// priority". Each value names a provider: `"voicebox"`,
    /// `"openai_compatible"`, `"groq"`, or `"local"`. When the active
    /// provider fails the dispatcher walks this list in order and tries
    /// each entry that has the credentials/config it needs.
    ///
    /// Mirrors the completion-side `fallback_providers` chain — use it
    /// to codify "if my local voicebox is down, try Groq, then OpenAI"
    /// without having to manually swap providers in the TUI on every
    /// outage.
    #[serde(default)]
    pub fallback_chain: Option<Vec<String>>,
}

/// OpenAI-compatible STT configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenaiCompatibleSttConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Base URL (e.g. "http://localhost:11434" or "https://api.groq.com/openai")
    #[serde(default)]
    pub base_url: Option<String>,
    /// Model name (e.g. "whisper-large-v3-turbo")
    #[serde(default)]
    pub model: Option<String>,
    /// API key
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Voicebox STT configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceboxSttConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Base URL (e.g. "http://localhost:8000")
    #[serde(default = "default_voicebox_url")]
    pub base_url: String,
}

impl Default for VoiceboxSttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_voicebox_url(),
        }
    }
}

fn default_voicebox_url() -> String {
    "http://localhost:8000".to_string()
}

/// Local STT (whisper.cpp) configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSttConfig {
    /// Whether local STT is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Model preset (e.g. "local-tiny", "local-base", "local-small", "local-medium")
    #[serde(default = "default_local_stt_model")]
    pub model: String,
}

impl Default for LocalSttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: default_local_stt_model(),
        }
    }
}

/// TTS (Text-to-Speech) provider configurations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TtsProviders {
    /// OpenAI TTS configuration ([providers.tts.openai])
    #[serde(default)]
    pub openai: Option<ProviderConfig>,

    /// Local Piper TTS configuration ([providers.tts.local])
    #[serde(default)]
    pub local: Option<LocalTtsConfig>,

    /// OpenAI-compatible TTS configuration ([providers.tts.openai_compatible])
    #[serde(default)]
    pub openai_compatible: Option<OpenaiCompatibleTtsConfig>,

    /// Voicebox TTS configuration ([providers.tts.voicebox])
    #[serde(default)]
    pub voicebox: Option<VoiceboxTtsConfig>,

    /// User-defined TTS fallback order. Empty/None means "use the default
    /// priority". Each value names a provider: `"voicebox"`,
    /// `"openai_compatible"`, `"openai"`, or `"local"`. When the active
    /// provider fails the dispatcher walks this list in order and tries
    /// each entry that has the credentials/config it needs.
    ///
    /// Mirrors the STT-side `fallback_chain` so the user can codify
    /// "if my local voicebox is down, try OpenAI TTS, then Piper" in
    /// one place.
    #[serde(default)]
    pub fallback_chain: Option<Vec<String>>,
}

/// OpenAI-compatible TTS configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenaiCompatibleTtsConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Base URL (e.g. "http://localhost:11434")
    #[serde(default)]
    pub base_url: Option<String>,
    /// Model name (e.g. "gpt-4o-mini-tts")
    #[serde(default)]
    pub model: Option<String>,
    /// Voice name (e.g. "echo")
    #[serde(default)]
    pub voice: Option<String>,
    /// API key
    #[serde(default)]
    pub api_key: Option<String>,
}

/// Voicebox TTS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceboxTtsConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Base URL (e.g. "http://localhost:8000")
    #[serde(default = "default_voicebox_url")]
    pub base_url: String,
    /// Voice profile ID for synthesis
    #[serde(default)]
    pub profile_id: String,
    /// TTS engine (e.g. "kokoro", "qwen", "qwen_custom_voice")
    #[serde(default)]
    pub engine: String,
}

impl Default for VoiceboxTtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_voicebox_url(),
            profile_id: String::new(),
            engine: String::new(),
        }
    }
}

/// Local TTS (Piper) configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalTtsConfig {
    /// Whether local TTS is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Piper voice name (default: "ryan")
    #[serde(default = "default_local_tts_voice")]
    pub voice: String,
}

impl Default for LocalTtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            voice: default_local_tts_voice(),
        }
    }
}

/// Web Search provider configurations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebSearchProviders {
    /// EXA search configuration
    #[serde(default)]
    pub exa: Option<ProviderConfig>,

    /// Brave search configuration
    #[serde(default)]
    pub brave: Option<ProviderConfig>,

    /// Serper (Google SERP) search configuration (#1731)
    #[serde(default)]
    pub serper: Option<ProviderConfig>,
}

/// Image provider configurations (e.g. Gemini for generation/vision)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageProviders {
    /// Google Gemini image configuration
    #[serde(default)]
    pub gemini: Option<ProviderConfig>,
}

/// Individual provider configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    /// Provider enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// API key (will be loaded from env or secrets)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,

    /// API base URL override
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Default model to use
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,

    /// Available models for this provider (can be updated at runtime)
    #[serde(default)]
    pub models: Vec<String>,
    /// When true on the ACTIVE default provider's section, a config reload
    /// pushes this section's default pair to every non-archived session,
    /// overriding their stored pairs (#466). Absent or false keeps the
    /// post-#379 isolation: defaults apply to new sessions only.
    #[serde(default)]
    pub force_default: bool,

    /// Vision-capable model to use when the default model doesn't support images.
    /// When set and images are present, the provider swaps to this model for that
    /// request only (e.g. `vision_model = "MiniMax-Text-01"` for MiniMax M2.7).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision_model: Option<String>,

    /// Output token cap for `analyze_image` / `analyze_video` calls on this
    /// provider (#2120).
    ///
    /// Unset is the default and the right answer for almost everyone: the
    /// request then carries no `max_tokens`, so the endpoint applies its own
    /// default. A reasoning vision model spends part of that budget on
    /// reasoning tokens, and a hard-coded cap used to truncate the answer or
    /// leave `content` empty, which surfaced as "No text response from vision
    /// model". Set this only to pin a specific figure (an endpoint with a low
    /// default, or a cost ceiling).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision_max_tokens: Option<u32>,

    /// Image-generation model override for this provider.
    ///
    /// Wins over the global `image.generation.model` when the active
    /// session's provider has it set. Lets users point `generate_image`
    /// at an alternative without leaving the TUI — e.g.
    /// `generation_model = "imagen-4.0-generate-001"` on the Gemini
    /// provider, or `generation_model = "black-forest-labs/flux-1.1-pro"`
    /// on an OpenRouter / OpenAI-compatible provider that exposes the
    /// `/v1/images/generations` endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_model: Option<String>,

    /// Speech-to-text model for the active provider. Set it and voice notes
    /// transcribe through that provider, the same way `vision_model` drives
    /// image understanding and `generation_model` drives image creation. The
    /// endpoint is the provider's own (`base_url`, or its known default), so
    /// any [OI]-compatible provider can serve transcription.
    ///
    /// The wire depends on the endpoint. The built-in endpoint takes the audio
    /// as an `input_audio` part on `/v1/chat/completions` and serves no
    /// `/v1/audio/transcriptions` route at all; every other provider gets the
    /// multipart transcription request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_model: Option<String>,

    /// Text-to-speech model for this provider's [OI]-compatible
    /// `/v1/audio/speech` route. See `stt_model`.
    ///
    /// The built-in endpoint serves no speech route in any shape, so setting
    /// this on it has no effect: synthesis falls through to the built-in local
    /// voice, a `[providers.tts.*]` block, or Voicebox.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_model: Option<String>,

    /// Context window size in tokens for this provider's model.
    /// Used by auto-compaction to know when to summarize history.
    /// Essential for custom/local providers whose models aren't recognized by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,

    /// Endpoint type for providers with multiple API modes (e.g. zhipu: "api" or "coding")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_type: Option<String>,

    /// Which route an [OI]-compatible provider speaks: `auto`, `chat`, or
    /// `responses`. Unset means `auto`, which resolves by provider kind (the
    /// official host stays `auto`, every other compatible endpoint is pinned
    /// to `chat`). See `crate::config::wire_dialect` (#2059).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_dialect: Option<WireDialect>,

    /// Kimi Code subscription-plan tier (`moderato`, `allegretto`, `allegro`,
    /// `vivace`). Only meaningful for a provider pointed at the Kimi Coding
    /// endpoint. When set and `context_window` is unset, the tier derives the
    /// auto-compaction context window (256K on moderato, up to 1M on
    /// allegretto and above); an explicit `context_window` always wins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,

    /// Reasoning control, resolved per family: Kimi (`max` on K3, `on`/`off`
    /// on K2.x), qwen rungs, DeepSeek / GLM-5.3+ `low | high | max` (a rung
    /// from another family is mapped and logged, see `glm_reasoning`).
    /// Applied only when the active model accepts it; otherwise a no-op.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,

    /// TTS voice name (e.g. "echo") — only used by TTS providers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,

    /// TTS model override (e.g. "gpt-4o-mini-tts") — only used by TTS providers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Thinking-mode switch for reasoning-capable models.
    ///
    /// Two different pathways honour this flag:
    /// - **DashScope Qwen** (`[providers.qwen]`) — inserted at the top
    ///   level of the request body so the gateway enables Qwen3's hybrid
    ///   reasoning mode. Unset / false keeps the model in fast mode.
    /// - **Local providers** (custom providers whose `base_url` points at
    ///   `localhost`, `*.local`, or an RFC1918 private IP — i.e. a
    ///   self-hosted llama.cpp / MLX / LM Studio / Ollama server) —
    ///   wrapped into `chat_template_kwargs: {"enable_thinking": X}`,
    ///   matching what `llama-server --jinja --chat-template-kwargs`
    ///   does. For local providers the default is `true` (Unsloth's
    ///   default behaviour — letting Qwen/Kimi/DeepSeek templates render
    ///   `<tool_call>` tags correctly); set `enable_thinking = false` in
    ///   the custom provider config to force non-thinking fast mode.
    ///
    /// Cloud providers that aren't Qwen ignore this flag entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_thinking: Option<bool>,

    /// OpenRouter response caching — add `X-OpenRouter-Cache: true` header
    /// to eligible requests. Cached identical requests return in milliseconds
    /// with zero tokens billed. Only effective for OpenRouter endpoints.
    /// Default: false (opt-in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_enabled: Option<bool>,

    /// Cache TTL in seconds for OpenRouter response caching (1-86400).
    /// Default: 300 (5 minutes). Only used when cache_enabled is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ttl: Option<u32>,

    /// Non-streaming request ceiling, in seconds, for this provider. Covers
    /// calls that buffer a whole body: title generation, compaction, `/models`,
    /// the TUI dialogs. Overrides the default 300s.
    ///
    /// It has NO effect on streaming. Streams run on a client built without a
    /// total timeout, so a healthy long turn is never cut by a wall clock
    /// (#1687); the only stream timer is `stream_idle_timeout_secs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    /// Inter-chunk streaming inactivity timeout in seconds.
    /// Overrides the default 20s idle timeout for remote streams (or 3600s for local/CLI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_secs: Option<u64>,

    /// Thinking-loop guard ceiling in seconds, for this provider. Overrides
    /// `[agent] thinking_loop_timeout_secs` (default 600).
    ///
    /// Unlike the two transport clocks above, `0` here is a VALUE, not a typo:
    /// it disables the guard for this provider. The guard is a prompt to the
    /// model ("you narrated work without calling a tool"), not a verdict on the
    /// connection, and a provider that legitimately needs longer to reach its
    /// first tool call is silenced by setting this to `0` rather than by
    /// guessing a bigger number (#1690).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_loop_timeout_secs: Option<u64>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Path to SQLite database file
    #[serde(default = "default_db_path")]
    pub path: PathBuf,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_db_path(),
        }
    }
}

fn default_db_path() -> PathBuf {
    opencrabs_home().join("opencrabs.db")
}

/// Expand leading `~` or `~/` in a path to the actual home directory.
fn expand_tilde(p: &Path) -> PathBuf {
    if let Ok(rest) = p.strip_prefix("~") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest)
    } else {
        p.to_path_buf()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub level: String,

    /// Log to file
    #[serde(default)]
    pub file: Option<PathBuf>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: None,
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider_registry: ProviderRegistryConfig::default(),
            database: DatabaseConfig {
                path: default_db_path(),
            },
            logging: LoggingConfig {
                level: default_log_level(),
                file: None,
            },
            debug: DebugConfig::default(),
            tui: TuiConfig::default(),
            providers: ProviderConfigs::default(),
            channels: ChannelsConfig::default(),
            agent: AgentConfig::default(),
            daemon: DaemonConfig::default(),
            a2a: A2aConfig::default(),
            image: ImageConfig::default(),
            cron: CronConfig::default(),
            memory: MemoryConfig::default(),
            decisions: DecisionsConfig::default(),
            brain: BrainConfig::default(),
            browser: BrowserConfig::default(),
            doctor: DoctorConfig::default(),
            features: FeaturesConfig::default(),
        }
    }
}

pub(crate) mod io;
pub use io::*;
// Private keys helpers used by the loader submodule (sibling of `io`).
pub(crate) use io::{load_keys_from_file, merge_channel_keys};
mod loader;
pub use loader::*;
mod voice_port;
