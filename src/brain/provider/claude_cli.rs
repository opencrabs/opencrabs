//! Claude CLI Provider — direct subprocess integration
//!
//! Spawns the `claude` CLI binary as a text completion backend and reads
//! its NDJSON stream output, converting it to standard `StreamEvent`s.
//! OpenCrabs handles all tools, memory, and context locally.

use super::error::{ProviderError, Result};
use super::r#trait::{Provider, ProviderStream};
use super::types::*;
use async_trait::async_trait;
use futures::stream::StreamExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{LazyLock, RwLock};
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;

/// On-disk path for the learned alias → resolved-version map.
#[cfg(not(test))]
fn learned_models_path() -> std::path::PathBuf {
    crate::config::profile::base_opencrabs_dir().join("claude_cli_models.json")
}

/// Load the persisted alias map from disk (production), or start empty
/// (tests, so they never depend on or mutate the developer's real
/// ~/.opencrabs/claude_cli_models.json).
#[cfg(not(test))]
fn load_learned_models() -> HashMap<String, String> {
    std::fs::read_to_string(learned_models_path())
        .ok()
        .and_then(|s| serde_json::from_str::<HashMap<String, String>>(&s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
fn load_learned_models() -> HashMap<String, String> {
    HashMap::new()
}

/// Process-wide cache of the version the `claude` CLI actually resolves
/// each bare alias to (e.g. `opus` → `opus-4-8`). Loaded from disk on
/// first access and refreshed whenever the CLI reports a newer release.
///
/// This is how the provider tracks Anthropic's current Claude release
/// automatically instead of relying on a hardcoded table that goes stale
/// every time a new model ships. The CLI is the source of truth; we just
/// learn from it and persist so footers/pricing stay correct across
/// restarts with zero hand-editing.
static LEARNED_MODELS: LazyLock<RwLock<HashMap<String, String>>> =
    LazyLock::new(|| RwLock::new(load_learned_models()));

/// The version this process has learned the CLI resolves `alias` to, if any.
pub(crate) fn learned_alias(alias: &str) -> Option<String> {
    LEARNED_MODELS.read().ok()?.get(alias).cloned()
}

/// Record the version the CLI actually resolved for a bare alias. Persists
/// to disk only when the value changed, so the steady state is zero IO.
pub(crate) fn record_alias(alias: &str, version: &str) {
    // Fast path: already current — no lock upgrade, no write.
    if let Ok(read) = LEARNED_MODELS.read()
        && read.get(alias).map(String::as_str) == Some(version)
    {
        return;
    }
    let snapshot = {
        let Ok(mut write) = LEARNED_MODELS.write() else {
            return;
        };
        // Re-check under the write lock in case another turn raced us.
        if write.get(alias).map(String::as_str) == Some(version) {
            return;
        }
        write.insert(alias.to_string(), version.to_string());
        write.clone()
    };
    tracing::info!(
        "claude-cli: learned {alias} → {version} from the CLI; updating the alias map so footers and pricing track the current release"
    );
    #[cfg(not(test))]
    {
        let path = learned_models_path();
        if let Ok(json) = serde_json::to_string_pretty(&snapshot) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, json);
        }
    }
    let _ = snapshot;
}

/// Clear the learned cache. Test-only seam for deterministic assertions.
#[cfg(test)]
pub(crate) fn clear_learned_models() {
    if let Ok(mut map) = LEARNED_MODELS.write() {
        map.clear();
    }
}

/// Canonical model list for Claude CLI. Single source of truth read by
/// `Provider::supported_models` AND by the channel `/models` menu helper
/// in `utils::providers::cli_supported_models` — adding or removing a
/// Claude variant only requires editing this const, not chasing
/// duplicated lists across modules.
pub(crate) const SUPPORTED_MODELS: &[&str] = &[
    "fable",
    "sonnet",
    "opus",
    "haiku",
    "opus-4-8",
    "opus-4-7",
    "sonnet-4-6",
    "opus-4-6",
    "haiku-4-5",
];

/// Default model when no per-session override is set. Mirrors the
/// `default_for_alias("sonnet")` resolution used by `ClaudeCliProvider::new`.
pub(crate) const DEFAULT_MODEL: &str = "opus-4-8";

/// Words that can appear quoted inside the `--model` help text but are not
/// model names. Keeps the permissive shape filter from picking up prose.
const HELP_PARSE_STOPWORDS: &[&str] = &["model", "alias", "name", "latest", "full", "session"];

/// Whether `s` is shaped like a model name/alias (`opus`, `opus-4-8`,
/// `claude-fable-5`) rather than prose or a sentinel.
///
/// Guards BOTH discovery sources: quoted prose from the `--help` text, and
/// non-model sentinels that can land in the learned-alias cache (a real
/// `~/.opencrabs/claude_cli_models.json` was found holding
/// `"sonnet": "<synthetic>"`, which would otherwise be offered as a model in
/// the `/models` menu).
fn looks_like_model_name(s: &str) -> bool {
    s.len() >= 4
        && s.len() <= 40
        && s.starts_with(|c: char| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
        && !HELP_PARSE_STOPWORDS.contains(&s)
}

/// Extract the model names the `claude` CLI documents in its `--help` output
/// (#753). The CLI describes `--model` as "an alias for the latest model (e.g.
/// 'fable', 'opus', or 'sonnet') or a model's full name (e.g.
/// 'claude-fable-5')", so the quoted tokens in that option's block are the
/// live, self-updating source — no hardcoded table to go stale when a new
/// model ships.
///
/// Pure so the parsing is unit tested without spawning the binary. Scans the
/// `--model` block only, and filters quoted segments by model-name shape, so
/// the stray apostrophe in "model's" (which shifts naive quote pairing) cannot
/// leak prose into the list.
pub(crate) fn parse_models_from_help(help: &str) -> Vec<String> {
    // Collect the --model option block: its line plus the wrapped continuation
    // lines, stopping at the next option.
    let mut block = String::new();
    let mut in_block = false;
    for line in help.lines() {
        let trimmed = line.trim_start();
        if in_block {
            // A new option (or a new section header) ends the block.
            if trimmed.starts_with('-') || (!line.starts_with(' ') && !trimmed.is_empty()) {
                break;
            }
            block.push(' ');
            block.push_str(trimmed);
        } else if trimmed.starts_with("--model") {
            in_block = true;
            block.push_str(trimmed);
        }
    }
    if block.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<String> = Vec::new();
    for seg in block.split('\'') {
        let t = seg.trim();
        if looks_like_model_name(t) && !out.iter().any(|m| m == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// Normalize a discovered id to OpenCrabs' naming: strip the `claude-` prefix
/// and any bracketed variant suffix (`claude-fable-5[1m]` → `fable-5`).
fn normalize_discovered(id: &str) -> String {
    let s = id.strip_prefix("claude-").unwrap_or(id);
    let s = s.split('[').next().unwrap_or(s);
    s.trim().to_ascii_lowercase()
}

/// Model ids the `claude` CLI itself has recorded on this machine (#753).
///
/// Reads Claude Code's own state (`~/.claude.json`) for two shapes:
///   * `additionalModelOptionsCache[].value` — its model-option catalogue,
///   * `clientDataCacheSlots.*.model` — models it has actually run.
///
/// This is how a brand-new release (e.g. `claude-opus-5`) becomes visible in
/// the picker without waiting for OpenCrabs to observe a resolution itself.
/// Strictly best-effort and fail-safe: a missing file, changed schema, or parse
/// error yields an empty list and the built-in floor still applies. Nothing is
/// written; the file is only read.
fn claude_code_state_models() -> Vec<String> {
    let Some(path) = dirs::home_dir().map(|h| h.join(".claude.json")) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let m = normalize_discovered(raw);
        if looks_like_model_name(&m) && !out.iter().any(|x| x == &m) {
            out.push(m);
        }
    };
    if let Some(arr) = json
        .get("additionalModelOptionsCache")
        .and_then(|v| v.as_array())
    {
        for entry in arr {
            if let Some(v) = entry.get("value").and_then(|v| v.as_str()) {
                push(v);
            }
        }
    }
    if let Some(slots) = json.get("clientDataCacheSlots").and_then(|v| v.as_object()) {
        for slot in slots.values() {
            if let Some(v) = slot.get("model").and_then(|v| v.as_str()) {
                push(v);
            }
        }
    }
    out.sort();
    out
}

/// Split a model id into `(family, version_rest)`; `version_rest` is empty for
/// a bare alias (`opus` → `("opus", "")`, `opus-4-8` → `("opus", "4-8")`).
fn split_family(id: &str) -> (&str, &str) {
    match id.split_once('-') {
        Some((family, rest)) => (family, rest),
        None => (id, ""),
    }
}

/// Parse a version tail into `(major, minor)`: `"5"` → `(5, 0)`, `"4-8"` →
/// `(4, 8)`. Non-numeric tails sort last within their family.
fn parse_version(rest: &str) -> (u32, u32) {
    if rest.is_empty() {
        return (0, 0);
    }
    match rest.split_once('-') {
        Some((maj, min)) => (
            maj.parse().unwrap_or(0),
            min.split('-').next().unwrap_or("0").parse().unwrap_or(0),
        ),
        None => (rest.parse().unwrap_or(0), 0),
    }
}

/// Ordering key for the model picker (#755).
///
/// Sorts: bare aliases first (they are the moving "latest" pointer and the
/// safest pick), then versioned models grouped by family with the NEWEST
/// version first. Without this the list came out in merge order, so `opus-5`
/// rendered below `opus-4-8` and above `opus-4-7` — the newest release looking
/// older than its neighbour. Major-only versions compare correctly against
/// major-minor ones because both parse to a `(major, minor)` pair, so
/// `5 → (5, 0)` outranks `4.8 → (4, 8)`.
fn model_sort_key(id: &str) -> (u8, u8, std::cmp::Reverse<(u32, u32)>) {
    let (family, rest) = split_family(id);
    let family_rank = match family {
        "fable" => 0,
        "opus" => 1,
        "sonnet" => 2,
        "haiku" => 3,
        _ => 4,
    };
    let is_versioned = u8::from(!rest.is_empty());
    (
        is_versioned,
        family_rank,
        std::cmp::Reverse(parse_version(rest)),
    )
}

/// Cached result of parsing `claude --help` (the subprocess runs at most once
/// per process). `None` until first attempted; an empty vec records a failed
/// or unavailable lookup so we don't re-spawn on every menu open.
static HELP_MODELS: LazyLock<RwLock<Option<Vec<String>>>> = LazyLock::new(|| RwLock::new(None));

/// Ask the installed `claude` binary which models it documents, cached.
fn help_models() -> Vec<String> {
    if let Ok(cached) = HELP_MODELS.read()
        && let Some(ref v) = *cached
    {
        return v.clone();
    }
    let discovered = resolve_claude_path()
        .ok()
        .and_then(|path| std::process::Command::new(path).arg("--help").output().ok())
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            parse_models_from_help(&text)
        })
        .unwrap_or_default();
    if let Ok(mut w) = HELP_MODELS.write() {
        *w = Some(discovered.clone());
    }
    discovered
}

/// The model list to offer for claude-cli, discovered rather than hardcoded
/// (#753).
///
/// Merges three sources, in order, de-duplicated:
///   1. names the installed CLI documents in `--help` (self-updating aliases),
///   2. the built-in [`SUPPORTED_MODELS`] floor, so nothing disappears when the
///      CLI is missing or its help text changes shape,
///   3. versions [`learned_alias`] observed the CLI actually resolve (e.g.
///      `opus` → `opus-5`), which is how a brand-new release shows up by name
///      without any code change.
///
/// Only the `--help` parse is cached; the learned versions are re-read each
/// call so the menu reflects what the CLI has resolved most recently.
pub(crate) fn available_models() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Normalize on the way in so the same model discovered under two spellings
    // (the help example `claude-fable-5` vs the recorded `fable-5`) collapses to
    // one entry instead of showing twice.
    let mut push = |m: String| {
        let m = normalize_discovered(&m);
        if looks_like_model_name(&m) && !out.iter().any(|x| x == &m) {
            out.push(m);
        }
    };
    for m in help_models() {
        push(m);
    }
    // Models the CLI itself records on this machine — this is what surfaces a
    // brand-new release (opus-5) by name in the picker.
    for m in claude_code_state_models() {
        push(m);
    }
    for m in SUPPORTED_MODELS {
        push((*m).to_string());
    }
    if let Ok(learned) = LEARNED_MODELS.read() {
        // Shape-filtered: the cache can hold non-model sentinels (`<synthetic>`)
        // that must never be offered as a selectable model.
        let mut versions: Vec<String> = learned
            .values()
            .filter(|v| looks_like_model_name(v))
            .cloned()
            .collect();
        versions.sort();
        for v in versions {
            push(v);
        }
    }
    // Merge order is arbitrary (help, then state file, then the const floor),
    // so order the picker semantically instead: aliases first, then newest
    // version per family (#755).
    out.sort_by_key(|m| model_sort_key(m));
    out
}

/// Claude CLI provider — talks directly to the `claude` binary.
#[derive(Clone)]
pub struct ClaudeCliProvider {
    claude_path: String,
    default_model: String,
    /// User override from `providers.claude_cli.context_window` in config.toml.
    configured_context_window: Option<u32>,
}

impl ClaudeCliProvider {
    /// Create a new provider, auto-detecting the claude binary.
    pub fn new() -> Result<Self> {
        let path = resolve_claude_path()?;
        Ok(Self {
            claude_path: path,
            default_model: Self::default_for_alias("sonnet"),
            configured_context_window: None,
        })
    }

    /// Override the context-window budget from `providers.claude_cli.context_window`.
    pub fn with_context_window(mut self, context_window: u32) -> Self {
        self.configured_context_window = Some(context_window);
        self
    }

    /// Override the default model (e.g. "opus", "haiku", "sonnet").
    /// Normalizes to display form (e.g. "opus" → "opus-4-7").
    pub fn with_default_model(mut self, model: String) -> Self {
        self.default_model = Self::normalize_model(&model);
        self
    }

    /// Map full Anthropic model name to CLI shorthand.
    fn map_model(model: &str) -> &str {
        if model.contains("fable") {
            "fable"
        } else if model.contains("opus") {
            "opus"
        } else if model.contains("haiku") {
            "haiku"
        } else {
            "sonnet"
        }
    }

    /// Normalize a model name for pricing/usage tracking.
    /// CLI shorthands like "opus" → "opus-4-7". Strips "claude-" prefix
    /// and any trailing date suffix (e.g. "claude-opus-4-7-20260115"
    /// → "opus-4-7"). Unknown variants are returned as-is so a future
    /// shorthand like "opus-4-8" flows through unchanged.
    pub(crate) fn normalize_model(model: &str) -> String {
        let stripped = model.strip_prefix("claude-").unwrap_or(model);
        let without_date = strip_claude_date_suffix(stripped);
        match without_date {
            "opus" | "sonnet" | "haiku" => Self::default_for_alias(without_date),
            other => other.to_string(),
        }
    }

    /// Resolve a bare CLI shorthand (`opus`/`sonnet`/`haiku`) to its
    /// concrete version. Prefers the version the CLI was last seen to
    /// resolve to (learned at runtime, persisted to disk), falling back
    /// to the built-in seed only on a brand-new install before any turn
    /// has run. The CLI does the real resolution; we just remember it.
    pub(crate) fn default_for_alias(alias: &str) -> String {
        if let Some(learned) = learned_alias(alias) {
            return learned;
        }
        match Self::seed_for_alias(alias) {
            Some(seed) => seed.to_string(),
            None => alias.to_string(),
        }
    }

    /// Built-in fallback versions, used only until the CLI reports the
    /// live release on the first response (then the learned cache takes
    /// over). These are the last-known-good releases at build time; they
    /// are intentionally NOT the place to chase new releases — the
    /// runtime learning does that automatically. `None` for anything that
    /// is not a bare alias (callers pass such ids through unchanged).
    pub(crate) fn seed_for_alias(alias: &str) -> Option<&'static str> {
        match alias {
            "opus" => Some("opus-4-7"),
            "sonnet" => Some("sonnet-4-6"),
            "haiku" => Some("haiku-4-5"),
            _ => None,
        }
    }

    /// Learn the version the CLI actually resolved an alias to. Called on
    /// the first assistant message of every turn with the model the CLI
    /// reported (e.g. `claude-opus-4-8`). Maps it back to its bare alias
    /// and records the normalized version so the next pick, the default
    /// model, pricing, and the footer all track the current release.
    pub(crate) fn record_observed_model(cli_full_model: &str) {
        let alias = Self::map_model(cli_full_model);
        let version = Self::normalize_model(cli_full_model);
        record_alias(alias, &version);
    }

    /// Build a plain-text prompt from LLMRequest messages.
    fn build_prompt(request: &LLMRequest) -> String {
        let mut parts = Vec::new();

        if let Some(system) = request.system_full()
            && !system.is_empty()
        {
            parts.push(system);
        }

        for msg in &request.messages {
            let role = match msg.role {
                Role::User => "Human",
                Role::Assistant => "Assistant",
                Role::System => "System",
            };
            let content: String = msg
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => Some(format!("[tool_result for {}]: {}", tool_use_id, content)),
                    ContentBlock::ToolUse { id, name, input } => {
                        Some(format!("[tool_use {} ({}): {}]", name, id, input))
                    }
                    ContentBlock::Thinking { thinking, .. } => {
                        if thinking.is_empty() {
                            None
                        } else {
                            Some(format!("<thinking>{}</thinking>", thinking))
                        }
                    }
                    ContentBlock::Image { source } => {
                        // CLI -p mode cannot process images inline.
                        // Keep the file path reference so the agent can use
                        // the analyze_image tool to describe it.
                        Some(match source {
                            ImageSource::Base64 { media_type, data } => {
                                // Save to temp file so analyze_image can read it
                                let ext = match media_type.as_str() {
                                    "image/png" => "png",
                                    "image/jpeg" => "jpeg",
                                    "image/gif" => "gif",
                                    "image/webp" => "webp",
                                    _ => "png",
                                };
                                let tmp = std::env::temp_dir().join(format!(
                                    "opencrabs_cli_img_{}.{}",
                                    uuid::Uuid::new_v4(),
                                    ext
                                ));
                                use base64::Engine;
                                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data)
                                    && std::fs::write(&tmp, &bytes).is_ok()
                                {
                                    format!(
                                        "[User attached an image at {}. Use the analyze_image tool to view it.]",
                                        tmp.display()
                                    )
                                } else {
                                    "[User attached an image but it could not be decoded.]".to_string()
                                }
                            }
                            ImageSource::Url { url } => {
                                format!(
                                    "[User attached an image: {}. Use the analyze_image tool to view it.]",
                                    url
                                )
                            }
                        })
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");

            if content.trim().is_empty() {
                continue;
            }
            parts.push(format!("{}: {}", role, content));
        }

        parts.join("\n\n")
    }
}

/// Resolve the claude CLI binary path.
fn resolve_claude_path() -> Result<String> {
    if let Ok(path) = std::env::var("CLAUDE_PATH") {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
        return Err(ProviderError::Internal(format!(
            "CLAUDE_PATH set but not found: {}",
            path
        )));
    }

    for candidate in &["/opt/homebrew/bin/claude", "/usr/local/bin/claude"] {
        if std::path::Path::new(candidate).exists() {
            return Ok(candidate.to_string());
        }
    }

    // Try PATH lookup (cross-platform: `which` on Unix, `where.exe` on Windows)
    if let Some(path) = super::which_binary("claude") {
        return Ok(path);
    }

    Err(ProviderError::Internal(
        "claude CLI not found — install it or set CLAUDE_PATH".to_string(),
    ))
}

// ── CLI NDJSON types ──

/// A parsed NDJSON message from claude CLI stdout.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum CliMessage {
    /// The CLI's system channel. Carries several disjoint payload shapes by
    /// `subtype` (`init`, `task_started`, `task_notification`,
    /// `background_tasks_changed`, ...); every field is optional so all of
    /// them parse into the same variant (#1776).
    System {
        #[serde(default)]
        subtype: Option<String>,
        #[serde(default)]
        task_id: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        description: Option<String>,
        /// `task_notification` carries the human text as `summary` (not `description`).
        #[serde(default)]
        summary: Option<String>,
        #[serde(default)]
        output_file: Option<String>,
    },
    Assistant {
        message: CliAssistantMessage,
    },
    /// Real-time SSE event — contains a complete Anthropic SSE payload.
    StreamEvent {
        event: serde_json::Value,
    },
    /// User turn during multi-turn tool loops (tool_result).
    User {},
    RateLimitEvent {},
    Result {
        stop_reason: Option<String>,
        usage: Option<CliUsage>,
        #[serde(default)]
        is_error: bool,
        result: Option<String>,
    },
    /// Any `type` this build does not know yet (`tool_progress` measured
    /// 2026-11-11, `command_lifecycle` reported in #3088). The CLI adds event
    /// types over time; an unknown one is forward-compatible noise, not a
    /// parse failure. The raw line is already logged at debug before the
    /// parse, so nothing is lost by dropping it.
    #[serde(other)]
    Unknown,
}

/// Classify a CLI `system` message's task-lifecycle payload (#1776).
///
/// The CLI reports its own background tasks through the system channel as
/// `task_started`, `task_notification` and `background_tasks_changed`. These
/// were silently discarded for months (the whole payload reduced to a
/// `CLI → system` debug line), so a backgrounded task's completion never
/// surfaced anywhere after the turn's real events. Returns the
/// [`StreamEvent::BackgroundTask`] to forward, or `None` for every other
/// system subtype (`init`, ...), which keeps the debug-only path.
pub(crate) fn system_task_event(
    subtype: Option<&str>,
    task_id: Option<String>,
    status: Option<String>,
    description: Option<String>,
    summary: Option<String>,
    output_file: Option<String>,
) -> Option<StreamEvent> {
    // `task_started` carries the human text as `description`;
    // `task_notification` carries it as `summary`. One field downstream.
    let description = description.or(summary);
    const TASK_SUBTYPES: [&str; 3] = [
        "task_started",
        "task_notification",
        "background_tasks_changed",
    ];
    let subtype = subtype?;
    if !TASK_SUBTYPES.contains(&subtype) {
        return None;
    }
    Some(StreamEvent::BackgroundTask {
        subtype: subtype.to_string(),
        task_id,
        status,
        description,
        output_file,
    })
}

#[derive(Debug, Deserialize)]
pub(crate) struct CliAssistantMessage {
    pub id: Option<String>,
    pub model: Option<String>,
    pub usage: Option<CliUsage>,
    #[serde(default)]
    pub content: Vec<CliContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum CliContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    Thinking {
        thinking: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CliUsage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    /// Cache tokens are the CLI's system prompt — tracked for billing but not context.
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
}

impl CliUsage {
    /// Non-cached input tokens only. Cache tokens flow separately via
    /// TokenUsage.cache_creation_tokens / cache_read_tokens fields.
    /// tool_loop.rs combines them: billable = input + cache_create + cache_read.
    fn total_input(&self) -> u32 {
        self.input_tokens
    }
}

#[async_trait]
impl Provider for ClaudeCliProvider {
    async fn complete(&self, request: LLMRequest) -> Result<LLMResponse> {
        // Collect streaming response into a single response
        let mut stream = self.stream(request).await?;

        let mut id = String::new();
        let mut model = String::new();
        let mut content = Vec::new();
        let mut stop_reason = None;
        let mut usage = TokenUsage::default();

        // Simple accumulator — text blocks only
        let mut text_buf = String::new();

        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::MessageStart { message } => {
                    id = message.id;
                    model = message.model;
                    usage = message.usage;
                }
                StreamEvent::ContentBlockDelta {
                    delta: ContentDelta::TextDelta { text },
                    ..
                } => {
                    text_buf.push_str(&text);
                }
                StreamEvent::MessageDelta { delta: d, usage: u } => {
                    stop_reason = d.stop_reason;
                    usage.output_tokens = u.output_tokens;
                    if u.cache_creation_tokens > 0 {
                        usage.cache_creation_tokens = u.cache_creation_tokens;
                    }
                    if u.cache_read_tokens > 0 {
                        usage.cache_read_tokens = u.cache_read_tokens;
                    }
                }
                StreamEvent::MessageStop => break,
                _ => {}
            }
        }

        if !text_buf.is_empty() {
            content.push(ContentBlock::Text { text: text_buf });
        }

        Ok(LLMResponse {
            id,
            model,
            content,
            stop_reason,
            usage,
            // CLI subprocess output is parsed in one shot, not streamed.
            streaming_active_secs: None,
            tool_text_leak: false,
        })
    }

    async fn stream(&self, request: LLMRequest) -> Result<ProviderStream> {
        let prompt = Self::build_prompt(&request);
        let original_model = Self::normalize_model(&request.model);
        let model = Self::map_model(&request.model).to_string();

        let cwd = request
            .working_directory
            .as_deref()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/")));

        tracing::info!(
            "Spawning claude CLI: model={}, prompt_len={}, cwd={}",
            model,
            prompt.len(),
            cwd.display()
        );

        // Each CLI spawn gets a fresh session ID. We manage conversation context
        // ourselves (context.rs), so we don't need the CLI to maintain session state.
        // Reusing OpenCrabs session IDs caused "Session ID already in use" errors
        // when concurrent requests (TUI + Telegram/Slack) shared the same session.
        let session_id_str = uuid::Uuid::new_v4().to_string();

        let mut command = tokio::process::Command::new(&self.claude_path);
        command
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_ENTRYPOINT")
            .arg("-p")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--include-partial-messages")
            .arg("--session-id")
            .arg(&session_id_str)
            .arg("--dangerously-skip-permissions")
            // Disable Claude's default `Co-Authored-By: Claude` trailer on git
            // commits. Uses the newer `attribution` setting (empty strings =
            // hide attribution) which takes precedence over the deprecated
            // `includeCoAuthoredBy` flag. Passed as an inline JSON override so
            // we don't touch the user's ~/.claude/settings.json.
            .arg("--settings")
            .arg(r#"{"attribution":{"commit":"","pr":""},"includeCoAuthoredBy":false}"#)
            .arg("--model")
            .arg(&model)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // #1776: put the CLI in its own process group (pgid == its pid) so a
        // cancelled stream can kill shell grandchildren (bash -c tools) that
        // a direct child.kill() would orphan.
        #[cfg(unix)]
        command.process_group(0);
        // #3219: tag the CLI's whole process tree with the session, so work a
        // tool detaches (`nohup`, `setsid`) can be traced back to it after
        // the turn ends. Descendants inherit the variable; see brain::detached.
        let note_session = request.session_id;
        if let Some(sid) = note_session {
            command.env(crate::brain::detached::SESSION_ENV, sid.to_string());
        }
        let mut child = command
            .spawn()
            .map_err(|e| ProviderError::Internal(format!("failed to spawn claude CLI: {}", e)))?;

        // Write prompt via stdin to avoid leaking in `ps aux`
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ProviderError::Internal("failed to capture stdin".to_string()))?;
        let prompt_bytes = prompt.into_bytes();
        tokio::spawn(async move {
            if let Err(e) = stdin.write_all(&prompt_bytes).await {
                tracing::warn!(error = %e, "failed to write prompt to Claude CLI stdin");
            }
            if let Err(e) = stdin.shutdown().await {
                tracing::warn!(error = %e, "failed to shutdown Claude CLI stdin");
            }
        });

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ProviderError::Internal("failed to capture stdout".to_string()))?;

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ProviderError::Internal("failed to capture stderr".to_string()))?;

        // Spawn stderr reader — log everything the CLI writes to stderr
        tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                if !line.is_empty() {
                    tracing::warn!("claude CLI stderr: {}", line);
                }
            }
        });

        // Channel-based stream: parse NDJSON lines → StreamEvent
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<StreamEvent>>(64);

        tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stdout);
            let mut lines = reader.lines();

            // Translation state — mirrors the proxy's TranslateState
            let mut started = false;
            let mut streaming_via_events = false;
            let mut completed_blocks: usize = 0;
            let mut current_block_started = false;
            let mut current_block_chars: usize = 0;
            let mut input_tokens: u32 = 0;
            let mut output_tokens: u32 = 0;
            // Per-round values (replaced each round) — actual context window
            let mut cache_creation_tokens_last: u32 = 0;
            let mut cache_read_tokens_last: u32 = 0;
            // Cumulative billing totals (summed across all rounds)
            let mut cache_creation_tokens_billing: u32 = 0;
            let mut cache_read_tokens_billing: u32 = 0;
            let mut result_received = false;
            // Set when the CLI reports a usage/rate limit. The stream is kept
            // alive through the pause, but if the process then dies without an
            // answer the turn must fail as RateLimitExceeded so the fallback
            // chain runs instead of the turn ending in silence (#1441).
            let mut rate_limited = false;
            let mut line_count = 0u32;
            // Track block index offset across tool rounds — each CLI turn
            // restarts content block indices at 0, so we offset them to prevent
            // collision in stream_complete()'s block_states array.
            let mut block_index_offset: usize = 0;
            let mut max_block_index_this_round: usize = 0;

            loop {
                let line_result = tokio::select! {
                    biased;
                    _ = tx.closed() => {
                        tracing::info!("CLI stream cancelled — killing subprocess");
                        // #1776: the CLI was spawned with process_group(0), so
                        // its pgid equals its pid. Signal the negative pgid to
                        // take down shell grandchildren (bash -c tools) a
                        // direct child.kill() would leave behind.
                        #[cfg(unix)]
                        if let Some(pid) = child.id() {
                            // SAFETY: kill(2) on a process we spawned; ESRCH
                            // after an already-exited group is harmless.
                            unsafe {
                                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
                            }
                        }
                        if let Err(e) = child.kill().await {
                        tracing::warn!(error = %e, "failed to kill Claude CLI child process");
                    }
                        break;
                    }
                    result = lines.next_line() => result,
                };
                let line = match line_result {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        tracing::info!(
                            "CLI stdout EOF after {} lines (started={})",
                            line_count,
                            started
                        );
                        break;
                    }
                    Err(e) => {
                        tracing::error!("CLI stdout read error after {} lines: {}", line_count, e);
                        break;
                    }
                };
                line_count += 1;
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                tracing::debug!("CLI stdout raw: {}", &line[..line.floor_char_boundary(300)]);

                let msg: CliMessage = match serde_json::from_str(&line) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(
                            "Skipping unparseable CLI line: {} — {}",
                            e,
                            &line[..line.floor_char_boundary(200)]
                        );
                        continue;
                    }
                };

                match msg {
                    CliMessage::System {
                        subtype,
                        task_id,
                        status,
                        description,
                        summary,
                        output_file,
                    } => match system_task_event(
                        subtype.as_deref(),
                        task_id,
                        status,
                        description,
                        summary,
                        output_file,
                    ) {
                        Some(ev) => {
                            tracing::info!(
                                event = ?ev,
                                "CLI background-task event surfaced (#1776)"
                            );
                            if tx.send(Ok(ev)).await.is_err() {
                                break;
                            }
                        }
                        None => tracing::debug!("CLI → system"),
                    },

                    CliMessage::StreamEvent { event } => {
                        // Real-time SSE events from CLI — forward directly.
                        // Suppress per-turn lifecycle (message_start after first, message_stop, message_delta).
                        let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        streaming_via_events = true;

                        match event_type {
                            "message_start" => {
                                if !started {
                                    started = true;
                                    // Capture cache tokens from message_start usage
                                    if let Some(msg) = event.get("message")
                                        && let Some(u) = msg.get("usage")
                                        && let Ok(cli_u) =
                                            serde_json::from_value::<CliUsage>(u.clone())
                                    {
                                        input_tokens = cli_u.total_input();
                                    }
                                    // Learn the version the CLI actually
                                    // resolved this alias to, so the alias map
                                    // tracks Anthropic's current release.
                                    if let Some(observed) = event
                                        .get("message")
                                        .and_then(|m| m.get("model"))
                                        .and_then(|m| m.as_str())
                                    {
                                        Self::record_observed_model(observed);
                                    }
                                    match serde_json::from_value::<StreamEvent>(event) {
                                        Ok(mut se) => {
                                            // Patch input_tokens to include cache tokens
                                            // and normalize the model id to the short
                                            // form (e.g. claude-opus-4-8 → opus-4-8) so
                                            // the display, writeback, and pricing all
                                            // see one consistent, current value.
                                            if let StreamEvent::MessageStart { ref mut message } =
                                                se
                                            {
                                                message.usage.input_tokens = input_tokens;
                                                message.model =
                                                    Self::normalize_model(&message.model);
                                            }
                                            if tx.send(Ok(se)).await.is_err() {
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!("Failed to parse message_start: {}", e);
                                        }
                                    }
                                }
                            }
                            "message_delta" => {
                                // Suppress event — Result handles final close.
                                // But capture usage (cache tokens) and track tool rounds.
                                let is_tool_round = event
                                    .get("delta")
                                    .and_then(|d| d.get("stop_reason"))
                                    .and_then(|r| r.as_str())
                                    == Some("tool_use");
                                if is_tool_round {
                                    block_index_offset += max_block_index_this_round;
                                    max_block_index_this_round = 0;
                                    tracing::debug!(
                                        "CLI tool round ended, block_index_offset={}",
                                        block_index_offset
                                    );
                                }
                                // Accumulate token usage from each round's message_delta.
                                // CLI reports cache_read/creation tokens here, not in Result.
                                if let Some(u) = event.get("usage") {
                                    let round_output = u
                                        .get("output_tokens")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(0)
                                        as u32;
                                    output_tokens += round_output;
                                    // Take the max input (includes cache) — each round reports
                                    // the running total of cache_read_input_tokens.
                                    if let Ok(round_usage) =
                                        serde_json::from_value::<CliUsage>(u.clone())
                                    {
                                        let round_input = round_usage.total_input();
                                        if round_input > input_tokens {
                                            input_tokens = round_input;
                                        }
                                        // Per-round: replace (this IS the context window)
                                        cache_creation_tokens_last =
                                            round_usage.cache_creation_input_tokens;
                                        cache_read_tokens_last =
                                            round_usage.cache_read_input_tokens;
                                        // Billing: accumulate across all rounds
                                        cache_creation_tokens_billing +=
                                            round_usage.cache_creation_input_tokens;
                                        cache_read_tokens_billing +=
                                            round_usage.cache_read_input_tokens;
                                    }
                                }
                                // Send Ping to prevent idle timeout during tool execution
                                if tx.send(Ok(StreamEvent::Ping)).await.is_err() {
                                    break;
                                }
                            }
                            "message_stop" => {
                                // Suppress — Result handles final close.
                                // Send Ping to prevent idle timeout.
                                if tx.send(Ok(StreamEvent::Ping)).await.is_err() {
                                    break;
                                }
                            }
                            // Forward tool_use blocks as real stream events — the tool_loop
                            // will see cli_handles_tools() and emit ProgressEvents for TUI
                            // display without re-executing them.
                            "content_block_start"
                                if event
                                    .get("content_block")
                                    .and_then(|b| b.get("type"))
                                    .and_then(|t| t.as_str())
                                    == Some("tool_use") =>
                            {
                                match serde_json::from_value::<StreamEvent>(event.clone()) {
                                    Ok(se) => {
                                        // Track max block index for offset calculation
                                        if let StreamEvent::ContentBlockStart { index, .. } = &se {
                                            max_block_index_this_round =
                                                max_block_index_this_round.max(index + 1);
                                        }
                                        let se = offset_block_index(se, block_index_offset);
                                        if tx.send(Ok(se)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        tracing::debug!(
                                            "Skipping tool_use content_block_start: {}",
                                            e
                                        );
                                    }
                                }
                            }
                            // Forward input_json_delta for tool_use blocks
                            "content_block_delta"
                                if event
                                    .get("delta")
                                    .and_then(|d| d.get("type"))
                                    .and_then(|t| t.as_str())
                                    == Some("input_json_delta") =>
                            {
                                match serde_json::from_value::<StreamEvent>(event.clone()) {
                                    Ok(se) => {
                                        let se = offset_block_index(se, block_index_offset);
                                        if tx.send(Ok(se)).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        tracing::debug!("Skipping input_json_delta: {}", e);
                                    }
                                }
                            }
                            _ => match serde_json::from_value::<StreamEvent>(event.clone()) {
                                Ok(se) => {
                                    // Track max block index for offset calculation
                                    match &se {
                                        StreamEvent::ContentBlockStart { index, .. }
                                        | StreamEvent::ContentBlockDelta { index, .. }
                                        | StreamEvent::ContentBlockStop { index } => {
                                            max_block_index_this_round =
                                                max_block_index_this_round.max(index + 1);
                                        }
                                        _ => {}
                                    }
                                    let se = offset_block_index(se, block_index_offset);
                                    let se = normalize_stream_event(se);
                                    if tx.send(Ok(se)).await.is_err() {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "Skipping stream event '{}': {}",
                                        event_type,
                                        e
                                    );
                                }
                            },
                        }
                    }

                    CliMessage::Assistant { message } => {
                        // With --include-partial-messages, each assistant line has the FULL
                        // accumulated content so far. If streaming_via_events, these are
                        // redundant — just grab usage.
                        if streaming_via_events {
                            if let Some(u) = &message.usage {
                                output_tokens = u.output_tokens;
                            }
                            continue;
                        }

                        // Fallback path: no stream_events, use assistant messages for content.
                        // Emit MessageStart once.
                        if !started {
                            started = true;
                            let msg_id = message.id.unwrap_or_else(|| {
                                format!("msg_{}", uuid::Uuid::new_v4().simple())
                            });
                            if let Some(ref raw) = message.model {
                                Self::record_observed_model(raw);
                            }
                            let msg_model = message
                                .model
                                .map(|m| Self::normalize_model(&m))
                                .unwrap_or_else(|| original_model.clone());
                            let (input_tokens, cc, cr) = message
                                .usage
                                .as_ref()
                                .map(|u| {
                                    (
                                        u.total_input(),
                                        u.cache_creation_input_tokens,
                                        u.cache_read_input_tokens,
                                    )
                                })
                                .unwrap_or((0, 0, 0));

                            let _ = tx
                                .send(Ok(StreamEvent::MessageStart {
                                    message: StreamMessage {
                                        id: msg_id,
                                        model: msg_model,
                                        role: Role::Assistant,
                                        usage: TokenUsage {
                                            input_tokens,
                                            output_tokens: 0,
                                            cache_creation_tokens: cc,
                                            cache_read_tokens: cr,
                                            ..Default::default()
                                        },
                                    },
                                }))
                                .await;
                        }

                        // Diff content blocks against what we already emitted.
                        let num_blocks = message.content.len();
                        for (i, block) in message.content.iter().enumerate() {
                            let is_last = i == num_blocks - 1;

                            // Already fully emitted — skip
                            if i < completed_blocks {
                                continue;
                            }

                            // New block appeared after current — close the previous one
                            if i > completed_blocks {
                                if current_block_started {
                                    let _ = tx
                                        .send(Ok(StreamEvent::ContentBlockStop {
                                            index: completed_blocks,
                                        }))
                                        .await;
                                    completed_blocks += 1;
                                    current_block_chars = 0;
                                    current_block_started = false;
                                }

                                // Emit any intermediate blocks fully
                                while completed_blocks < i {
                                    emit_full_block(
                                        &tx,
                                        &message.content[completed_blocks],
                                        completed_blocks,
                                    )
                                    .await;
                                    completed_blocks += 1;
                                }
                            }

                            let full_text = cli_block_text(block);

                            // Start block if needed
                            if !current_block_started {
                                let empty = cli_empty_block(block);
                                let _ = tx
                                    .send(Ok(StreamEvent::ContentBlockStart {
                                        index: i,
                                        content_block: empty,
                                    }))
                                    .await;
                                current_block_started = true;
                                current_block_chars = 0;
                            }

                            // Emit delta for NEW characters only
                            if full_text.len() > current_block_chars {
                                let new_text = &full_text[current_block_chars..];
                                if !new_text.is_empty() {
                                    let delta = cli_block_delta(block, new_text);
                                    let _ = tx
                                        .send(Ok(StreamEvent::ContentBlockDelta {
                                            index: i,
                                            delta,
                                        }))
                                        .await;
                                    current_block_chars = full_text.len();
                                }
                            }

                            // If not the last block, it's complete — close it
                            if !is_last {
                                let _ = tx
                                    .send(Ok(StreamEvent::ContentBlockStop { index: i }))
                                    .await;
                                completed_blocks += 1;
                                current_block_chars = 0;
                                current_block_started = false;
                            }
                        }

                        if let Some(u) = &message.usage {
                            output_tokens = u.output_tokens;
                        }
                    }

                    CliMessage::Result {
                        stop_reason,
                        usage,
                        is_error,
                        result,
                    } => {
                        // CLI returned an error (API failure, image processing, etc.)
                        // Surface it as a text block so the user sees what happened
                        // instead of silently dropping the response.
                        if is_error {
                            let error_text =
                                result.unwrap_or_else(|| "CLI returned an error".to_string());
                            tracing::error!("CLI result is_error=true: {}", error_text);

                            let error_lower = error_text.to_lowercase();

                            // "Prompt is too long" must be surfaced as ContextLengthExceeded
                            // so the tool loop can run emergency compaction and retry.
                            if error_lower.contains("prompt is too long")
                                || error_lower.contains("too many tokens")
                                || error_lower.contains("context length")
                            {
                                if let Err(e) =
                                    tx.send(Err(ProviderError::ContextLengthExceeded(0))).await
                                {
                                    tracing::warn!(error = %e, "failed to send context length error");
                                }
                                break;
                            }

                            // Rate/account limits must be surfaced as RateLimitExceeded
                            // so the FallbackProvider can trigger the next provider in the chain.
                            if error_lower.contains("rate limit")
                                || error_lower.contains("hit your limit")
                                || error_lower.contains("session limit")
                                || error_lower.contains("overloaded")
                                || error_lower.contains("too many requests")
                                || error_lower.contains("capacity")
                                || error_lower.contains("429")
                            {
                                tracing::warn!(
                                    "CLI rate/account limit detected — returning RateLimitExceeded for fallback"
                                );
                                let _ = tx
                                    .send(Err(ProviderError::RateLimitExceeded(error_text)))
                                    .await;
                                break;
                            }

                            // Ensure message_start was sent
                            if !started {
                                started = true;
                                let _ = tx
                                    .send(Ok(StreamEvent::MessageStart {
                                        message: StreamMessage {
                                            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
                                            model: original_model.clone(),
                                            role: Role::Assistant,
                                            usage: TokenUsage {
                                                input_tokens: 0,
                                                output_tokens: 0,
                                                ..Default::default()
                                            },
                                        },
                                    }))
                                    .await;
                            }

                            // Close any open content block from partial streaming
                            if current_block_started {
                                let _ = tx
                                    .send(Ok(StreamEvent::ContentBlockStop {
                                        index: completed_blocks,
                                    }))
                                    .await;
                                completed_blocks += 1;
                            }

                            // Emit the error as a visible text block
                            let error_idx = completed_blocks + block_index_offset;
                            let _ = tx
                                .send(Ok(StreamEvent::ContentBlockStart {
                                    index: error_idx,
                                    content_block: ContentBlock::Text {
                                        text: String::new(),
                                    },
                                }))
                                .await;
                            let _ = tx
                                .send(Ok(StreamEvent::ContentBlockDelta {
                                    index: error_idx,
                                    delta: ContentDelta::TextDelta {
                                        text: format!("\n\n⚠️ CLI error: {}", error_text),
                                    },
                                }))
                                .await;
                            let _ = tx
                                .send(Ok(StreamEvent::ContentBlockStop { index: error_idx }))
                                .await;
                        } else {
                            // Close any open content block
                            if current_block_started {
                                let _ = tx
                                    .send(Ok(StreamEvent::ContentBlockStop {
                                        index: completed_blocks,
                                    }))
                                    .await;
                            }
                        }

                        let reason = stop_reason.map(|r| match r.as_str() {
                            "end_turn" => StopReason::EndTurn,
                            "tool_use" => StopReason::ToolUse,
                            "max_tokens" => StopReason::MaxTokens,
                            _ => StopReason::EndTurn,
                        });

                        let final_output = usage
                            .as_ref()
                            .map(|u| u.output_tokens)
                            .unwrap_or(output_tokens);
                        let final_input = usage
                            .as_ref()
                            .map(|u| u.total_input())
                            .unwrap_or(input_tokens);
                        if let Some(ref u) = usage {
                            tracing::debug!(
                                "CLI session complete: input={}, cache_create={}, cache_read={}, total={}",
                                u.input_tokens,
                                u.cache_creation_input_tokens,
                                u.cache_read_input_tokens,
                                u.total_input()
                            );
                        }

                        // Context window = last round's per-call cache tokens.
                        // Billing = cumulative across all rounds (for cost).
                        // The Result message reports cumulative totals — use it for
                        // billing if SSE didn't capture any rounds.
                        let (result_cc, result_cr) = usage
                            .as_ref()
                            .map(|u| (u.cache_creation_input_tokens, u.cache_read_input_tokens))
                            .unwrap_or((0, 0));

                        // For context: use last SSE round (per-call), fallback to Result
                        let ctx_cache_creation = if cache_creation_tokens_last > 0 {
                            cache_creation_tokens_last
                        } else {
                            result_cc
                        };
                        let ctx_cache_read = if cache_read_tokens_last > 0 {
                            cache_read_tokens_last
                        } else {
                            result_cr
                        };

                        // For billing: use cumulative SSE totals, fallback to Result
                        let billing_cache_creation = if cache_creation_tokens_billing > 0 {
                            cache_creation_tokens_billing
                        } else {
                            result_cc
                        };
                        let billing_cache_read = if cache_read_tokens_billing > 0 {
                            cache_read_tokens_billing
                        } else {
                            result_cr
                        };

                        tracing::info!(
                            "CLI token split: context={}+{}+{}={}, billing={}+{}+{}={}",
                            final_input,
                            ctx_cache_creation,
                            ctx_cache_read,
                            final_input + ctx_cache_creation + ctx_cache_read,
                            final_input,
                            billing_cache_creation,
                            billing_cache_read,
                            final_input + billing_cache_creation + billing_cache_read,
                        );

                        let _ = tx
                            .send(Ok(StreamEvent::MessageDelta {
                                delta: MessageDelta {
                                    stop_reason: reason,
                                    stop_sequence: None,
                                },
                                usage: TokenUsage {
                                    input_tokens: final_input,
                                    output_tokens: final_output,
                                    // Context window tokens (per-call)
                                    cache_creation_tokens: ctx_cache_creation,
                                    cache_read_tokens: ctx_cache_read,
                                    // Billing tokens (cumulative across rounds)
                                    billing_cache_creation,
                                    billing_cache_read,
                                    ..Default::default()
                                },
                            }))
                            .await;

                        if let Err(e) = tx.send(Ok(StreamEvent::MessageStop)).await {
                            tracing::warn!(error = %e, "failed to send MessageStop event");
                        }
                        result_received = true;
                        break;
                    }

                    CliMessage::User { .. } => {
                        tracing::debug!("CLI → user turn (tool_result)");
                        // #3219: a `systemd-run` unit does not inherit the
                        // session tag, so it is found by the line systemd-run
                        // prints into the tool result.
                        if let Some(sid) = note_session {
                            crate::brain::detached::note_units(sid, &line);
                        }
                        // Keep the stream alive — tool results arrive during internal
                        // tool execution which can take >60s for complex tasks.
                        if tx.send(Ok(StreamEvent::Ping)).await.is_err() {
                            break;
                        }
                    }

                    CliMessage::RateLimitEvent {} => {
                        tracing::warn!("CLI → rate_limit_event");
                        rate_limited = true;
                        // Keep the stream alive during rate-limit pauses: the
                        // CLI resumes on its own when the window rolls over.
                        // The flag only matters if it never comes back.
                        if tx.send(Ok(StreamEvent::Ping)).await.is_err() {
                            break;
                        }
                    }

                    CliMessage::Unknown => {
                        tracing::debug!("CLI → unknown event type, ignored (#3088)");
                    }
                }
            }

            // A turn that ends with no Result AND no content delivered nothing.
            // Synthesising EndTurn here (below) marked it a success, and CLI
            // providers are deliberately exempt from the empty-answer guard in
            // helpers.rs because they run their own inner loop — so nothing
            // downstream caught it and the channel got silence (#1441). Failing
            // the turn is what runs the retry/fallback ladder and puts a line
            // in front of the user.
            let produced_content = completed_blocks > 0 || current_block_chars > 0;
            let mut failure_reported = false;
            if !result_received && !produced_content {
                let detail = if rate_limited {
                    "claude CLI hit its usage limit and exited without producing an answer"
                } else {
                    "claude CLI stream ended without a result and without any content"
                };
                tracing::warn!("{detail} — surfacing as a provider error (#1441)");
                let err = if rate_limited {
                    ProviderError::RateLimitExceeded(detail.to_string())
                } else {
                    ProviderError::Internal(detail.to_string())
                };
                let _ = tx.send(Err(err)).await;
                failure_reported = true;
            }

            // If the loop ended without a Result message (EOF/error) but we
            // accumulated token usage from message_delta events, send a final
            // MessageDelta + MessageStop so helpers.rs captures the real counts.
            if started && !result_received && (input_tokens > 0 || output_tokens > 0) {
                tracing::info!(
                    "CLI EOF without Result — flushing accumulated usage: input={}, output={}",
                    input_tokens,
                    output_tokens
                );
                let _ = tx
                    .send(Ok(StreamEvent::MessageDelta {
                        delta: MessageDelta {
                            stop_reason: Some(StopReason::EndTurn),
                            stop_sequence: None,
                        },
                        usage: TokenUsage {
                            input_tokens,
                            output_tokens,
                            cache_creation_tokens: cache_creation_tokens_last,
                            cache_read_tokens: cache_read_tokens_last,
                            ..Default::default()
                        },
                    }))
                    .await;
                if let Err(e) = tx.send(Ok(StreamEvent::MessageStop)).await {
                    tracing::warn!(error = %e, "failed to send MessageStop event");
                }
            }

            // Wait for process exit
            let exit_status = child.wait().await;
            match &exit_status {
                Ok(status) if !status.success() => {
                    tracing::warn!("claude CLI exited with status: {}", status);
                    if !started && !failure_reported {
                        let _ = tx
                            .send(Err(ProviderError::Internal(format!(
                                "claude CLI exited with {} before producing any output",
                                status
                            ))))
                            .await;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to wait on claude CLI: {}", e);
                }
                Ok(_) => {
                    if !started && !failure_reported {
                        // Clean exit, zero events: previously logged and
                        // nothing else, so the turn ended silently (#1441).
                        tracing::warn!(
                            "claude CLI exited successfully but produced no stream events"
                        );
                        let _ = tx
                            .send(Err(ProviderError::Internal(
                                "claude CLI exited without producing any output".to_string(),
                            )))
                            .await;
                    }
                }
            }
        });

        // Convert mpsc receiver to a Stream via unfold
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "claude-cli"
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn supported_models(&self) -> Vec<String> {
        // Discovered, not hardcoded (#753) — keeps this in sync with the
        // /models menu so a newly released model the CLI accepts is never
        // rejected here as "not in the catalogue".
        available_models()
    }

    fn configured_context_window(&self) -> Option<u32> {
        self.configured_context_window
    }

    fn context_window(&self, _model: &str) -> Option<u32> {
        // The user's `providers.<name>.context_window` wins (#973). The
        // README promises this override works on every provider including
        // the CLI ones, and the field was already stored and exposed here;
        // only this method ignored it.
        if let Some(cw) = self.configured_context_window {
            return Some(cw);
        }
        Some(200_000)
    }

    fn calculate_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        crate::usage::pricing::PricingConfig::load()
            .map(|cfg| cfg.calculate_cost(model, input_tokens, output_tokens))
            .unwrap_or(0.0)
    }

    fn supports_tools(&self) -> bool {
        true // CLI handles tools internally
    }

    fn supports_vision(&self) -> bool {
        false // CLI -p mode cannot process images — use analyze_image fallback
    }

    fn cli_handles_tools(&self) -> bool {
        true // CLI executes tools internally — display only, don't re-execute
    }

    fn cli_manages_context(&self) -> bool {
        // We send Claude CLI the full conversation each turn via stdin.
        // Claude CLI's internal session state in ~/.claude/ is irrelevant —
        // what matters is what we feed it. So OpenCrabs owns the context
        // and MUST run its own compaction; otherwise local tiktoken
        // estimates climb without bound (484k/200k = 242% reported by
        // user 2026-05-04) because tool-result messages pile up across
        // iterations and we never trim. Claude CLI handles its own prompt
        // caching transparently — that's a separate concern from context
        // sizing.
        false
    }
}

// ── Helper functions for assistant message → StreamEvent translation ──

/// Extract text content from a CLI content block.
fn cli_block_text(block: &CliContentBlock) -> &str {
    match block {
        CliContentBlock::Text { text } => text.as_str(),
        CliContentBlock::Thinking { thinking } => thinking.as_str(),
        _ => "",
    }
}

/// Create an empty ContentBlock for content_block_start.
fn cli_empty_block(block: &CliContentBlock) -> ContentBlock {
    match block {
        CliContentBlock::Text { .. } => ContentBlock::Text {
            text: String::new(),
        },
        CliContentBlock::Thinking { .. } => ContentBlock::Thinking {
            thinking: String::new(),
            signature: None,
        },
        CliContentBlock::ToolUse { id, name, .. } => ContentBlock::ToolUse {
            id: id.clone(),
            name: normalize_cli_tool_name(name),
            input: serde_json::json!({}),
        },
        CliContentBlock::Unknown => ContentBlock::Text {
            text: String::new(),
        },
    }
}

/// Create the appropriate ContentDelta for a block type.
fn cli_block_delta(block: &CliContentBlock, new_text: &str) -> ContentDelta {
    match block {
        CliContentBlock::Thinking { .. } => ContentDelta::ThinkingDelta {
            thinking: new_text.to_string(),
        },
        _ => ContentDelta::TextDelta {
            text: new_text.to_string(),
        },
    }
}

/// Emit a complete block (start + delta + stop) in one shot.
async fn emit_full_block(
    tx: &tokio::sync::mpsc::Sender<super::error::Result<StreamEvent>>,
    block: &CliContentBlock,
    index: usize,
) {
    let empty = cli_empty_block(block);
    let _ = tx
        .send(Ok(StreamEvent::ContentBlockStart {
            index,
            content_block: empty,
        }))
        .await;

    match block {
        CliContentBlock::Text { text } => {
            let _ = tx
                .send(Ok(StreamEvent::ContentBlockDelta {
                    index,
                    delta: ContentDelta::TextDelta { text: text.clone() },
                }))
                .await;
        }
        CliContentBlock::Thinking { thinking } => {
            let _ = tx
                .send(Ok(StreamEvent::ContentBlockDelta {
                    index,
                    delta: ContentDelta::ThinkingDelta {
                        thinking: thinking.clone(),
                    },
                }))
                .await;
        }
        CliContentBlock::ToolUse { input, .. } => {
            let input_str = serde_json::to_string(input).unwrap_or_default();
            let _ = tx
                .send(Ok(StreamEvent::ContentBlockDelta {
                    index,
                    delta: ContentDelta::InputJsonDelta {
                        partial_json: input_str,
                    },
                }))
                .await;
        }
        CliContentBlock::Unknown => {}
    }

    if let Err(e) = tx.send(Ok(StreamEvent::ContentBlockStop { index })).await {
        tracing::warn!(error = %e, "failed to send ContentBlockStop event");
    }
}

/// Normalize Claude Code CLI tool names to OpenCrabs format.
fn normalize_cli_tool_name(name: &str) -> String {
    match name {
        "Bash" => "bash".to_string(),
        "Read" => "read_file".to_string(),
        "Write" => "write_file".to_string(),
        "Edit" => "edit_file".to_string(),
        "Grep" => "grep".to_string(),
        "Glob" => "glob".to_string(),
        "LSP" => "lsp".to_string(),
        "WebSearch" => "web_search".to_string(),
        "WebFetch" => "http_request".to_string(),
        "Agent" => "agent".to_string(),
        "NotebookEdit" => "notebook_edit".to_string(),
        other => other.to_string(),
    }
}

/// Offset content block indices in a StreamEvent by the given amount.
/// This prevents collisions when multiple CLI tool rounds produce blocks
/// with indices restarting at 0.
fn offset_block_index(event: StreamEvent, offset: usize) -> StreamEvent {
    if offset == 0 {
        return event;
    }
    match event {
        StreamEvent::ContentBlockStart {
            index,
            content_block,
        } => StreamEvent::ContentBlockStart {
            index: index + offset,
            content_block,
        },
        StreamEvent::ContentBlockDelta { index, delta } => StreamEvent::ContentBlockDelta {
            index: index + offset,
            delta,
        },
        StreamEvent::ContentBlockStop { index } => StreamEvent::ContentBlockStop {
            index: index + offset,
        },
        other => other,
    }
}

/// Normalize tool names in stream events from CLI to OpenCrabs format.
fn normalize_stream_event(event: StreamEvent) -> StreamEvent {
    match event {
        StreamEvent::ContentBlockStart {
            index,
            content_block: ContentBlock::ToolUse { id, name, input },
        } => StreamEvent::ContentBlockStart {
            index,
            content_block: ContentBlock::ToolUse {
                id,
                name: normalize_cli_tool_name(&name),
                input,
            },
        },
        other => other,
    }
}

/// Strip the trailing release-date suffix from a Claude model string.
/// Anthropic stamps model ids as `claude-opus-4-7-20260115`; for display
/// and pricing we want `opus-4-7`. Only strips an 8-digit date at the
/// very end preceded by a hyphen.
pub(crate) fn strip_claude_date_suffix(s: &str) -> &str {
    if let Some(idx) = s.rfind('-') {
        let suffix = &s[idx + 1..];
        if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
            return &s[..idx];
        }
    }
    s
}
