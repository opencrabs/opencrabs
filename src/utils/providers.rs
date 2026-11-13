//! Shared provider registry — single source of truth for all LLM provider metadata.
//!
//! Used by TUI `/models`, `/onboard`, channel `/models` commands, and config layer.
//! Add new providers HERE — everything else derives from this list.

use crate::config::{ProviderConfig, ProviderConfigs};

/// Static metadata for a known (non-custom) LLM provider.
pub struct ProviderMeta {
    /// Canonical identifier used in config keys and protocol (e.g. "anthropic", "claude-cli")
    pub id: &'static str,
    /// Human-readable display name (e.g. "Anthropic", "Claude CLI")
    pub display_name: &'static str,
    /// Config section path (e.g. "providers.anthropic")
    pub config_section: &'static str,
    /// Whether this provider needs an API key (false for CLI providers)
    pub needs_api_key: bool,
}

/// All known providers in alphabetical order. Custom providers are dynamic and not listed here.
pub const KNOWN_PROVIDERS: &[ProviderMeta] = &[
    ProviderMeta {
        id: "anthropic",
        display_name: "Anthropic",
        config_section: "providers.anthropic",
        needs_api_key: true,
    },
    ProviderMeta {
        id: "claude-cli",
        display_name: "Claude CLI",
        config_section: "providers.claude_cli",
        needs_api_key: false,
    },
    ProviderMeta {
        id: "codex-cli",
        display_name: "Codex CLI",
        config_section: "providers.codex_cli",
        needs_api_key: false,
    },
    ProviderMeta {
        id: "command-code-cli",
        display_name: "Command Code CLI",
        config_section: "providers.command_code_cli",
        needs_api_key: false,
    },
    ProviderMeta {
        id: "codex",
        display_name: "Codex",
        config_section: "providers.codex",
        needs_api_key: false,
    },
    ProviderMeta {
        id: "gemini",
        display_name: "Gemini",
        config_section: "providers.gemini",
        needs_api_key: true,
    },
    ProviderMeta {
        id: "github",
        display_name: "GitHub Copilot",
        config_section: "providers.github",
        needs_api_key: true,
    },
    ProviderMeta {
        id: "minimax",
        display_name: "MiniMax",
        config_section: "providers.minimax",
        needs_api_key: true,
    },
    ProviderMeta {
        id: "xiaomi",
        display_name: "Xiaomi",
        config_section: "providers.xiaomi",
        // Keyed: the user supplies an API key from platform.xiaomimimo.com.
        needs_api_key: true,
    },
    ProviderMeta {
        id: "openai",
        display_name: "OpenAI",
        config_section: "providers.openai",
        needs_api_key: true,
    },
    ProviderMeta {
        id: "ollama",
        display_name: "Ollama",
        config_section: "providers.ollama",
        needs_api_key: false,
    },
    ProviderMeta {
        id: "opencode",
        display_name: "OpenCode",
        config_section: "providers.opencode",
        needs_api_key: true,
    },
    ProviderMeta {
        id: "opencode-cli",
        display_name: "OpenCode CLI",
        config_section: "providers.opencode_cli",
        needs_api_key: false,
    },
    ProviderMeta {
        id: "opencrabs",
        display_name: "OpenCrabs",
        config_section: "providers.opencrabs",
        // Keyed by the install token the proxy issues, which the user never types.
        needs_api_key: true,
    },
    ProviderMeta {
        id: "openrouter",
        display_name: "OpenRouter",
        config_section: "providers.openrouter",
        needs_api_key: true,
    },
    ProviderMeta {
        id: "qwen",
        display_name: "Qwen",
        config_section: "providers.qwen",
        needs_api_key: true,
    },
    ProviderMeta {
        id: "zai",
        display_name: "z.ai",
        config_section: "providers.zai",
        needs_api_key: true,
    },
    ProviderMeta {
        id: "moonshot",
        display_name: "Moonshot AI",
        config_section: "providers.moonshot",
        needs_api_key: true,
    },
];

/// Look up a known provider by any of its aliases (e.g. "claude_cli", "claude-cli").
pub fn find_provider_meta(name: &str) -> Option<&'static ProviderMeta> {
    let n = name.trim().to_lowercase();
    KNOWN_PROVIDERS.iter().find(|p| {
        p.id == n
            || p.display_name.to_lowercase() == n
            || match p.id {
                "github" => n == "github copilot",
                "gemini" => n == "google" || n == "google gemini",
                "zai" => n == "zhipu" || n == "z.ai glm",
                "moonshot" => n == "kimi" || n == "moonshotai" || n == "moonshot ai",
                "claude-cli" => n == "claude_cli",
                "codex-cli" => n == "codex_cli",
                "command-code-cli" => n == "command_code_cli" || n == "cmd-cli",
                "codex" => n == "codex_oauth",
                // Only `opencode-cli` / `opencode_cli` — never bare "opencode".
                // A custom provider named "opencode" (which users do create,
                // pointing at opencode.ai HTTP) must never collide with this
                // built-in CLI alias. Sessions that reference "opencode" are
                // the custom entry, not the CLI.
                "opencode-cli" => n == "opencode_cli",
                "opencode" => n == "opencode_api" || n == "opencode-ai",
                "ollama" => n == "ollama_local" || n == "ollama-cloud",
                _ => false,
            }
    })
}

/// Canonical provider id for any alias. Returns the alias itself if unknown.
pub fn normalize_provider_name(name: &str) -> String {
    if let Some(meta) = find_provider_meta(name) {
        return meta.id.to_string();
    }
    let lowered = name.trim().to_lowercase();
    if lowered.starts_with("custom:") {
        return lowered;
    }
    if let Some(inner) = lowered
        .strip_prefix("custom(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return format!("custom:{}", inner);
    }
    lowered
}

/// Display name for any provider id (known or custom).
pub fn display_name(name: &str) -> &str {
    if let Some(meta) = find_provider_meta(name) {
        return meta.display_name;
    }
    if let Some(custom_name) = name.strip_prefix("custom:") {
        return custom_name;
    }
    name
}

/// Config section path for any provider id (known or custom).
pub fn config_section(name: &str) -> Option<String> {
    if let Some(meta) = find_provider_meta(name) {
        return Some(meta.config_section.to_string());
    }
    let custom_name = name.strip_prefix("custom:").or_else(|| {
        name.strip_prefix("custom(")
            .and_then(|s| s.strip_suffix(')'))
    })?;
    Some(format!("providers.custom.{}", custom_name))
}

/// Get the ProviderConfig for a given provider id from ProviderConfigs.
pub fn config_for<'a>(providers: &'a ProviderConfigs, name: &str) -> Option<&'a ProviderConfig> {
    let meta = find_provider_meta(name);
    match meta.map(|m| m.id) {
        Some("anthropic") => providers.anthropic.as_ref(),
        Some("openai") => providers.openai.as_ref(),
        Some("github") => providers.github.as_ref(),
        Some("gemini") => providers.gemini.as_ref(),
        Some("openrouter") => providers.openrouter.as_ref(),
        Some("minimax") => providers.minimax.as_ref(),
        Some("moonshot") => providers.moonshot.as_ref(),
        Some("zai") => providers.zai.as_ref(),
        Some("xiaomi") => providers.xiaomi.as_ref(),
        Some("claude-cli") => providers.claude_cli.as_ref(),
        Some("codex-cli") => providers.codex_cli.as_ref(),
        Some("command-code-cli") => providers.command_code_cli.as_ref(),
        Some("opencode-cli") => providers.opencode_cli.as_ref(),
        Some("opencode") => providers.opencode.as_ref(),
        Some("ollama") => providers.ollama.as_ref(),
        Some("opencrabs") => providers.opencrabs.as_ref(),
        Some("qwen") => providers.qwen.as_ref(),
        _ => {
            let custom_name = name.strip_prefix("custom:")?;
            providers.custom.as_ref()?.get(custom_name)
        }
    }
}

/// List all configured providers (have API key or are enabled CLI providers).
/// Returns `(provider_id, display_name)` pairs.
pub fn configured_providers(providers: &ProviderConfigs) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for meta in KNOWN_PROVIDERS {
        let cfg = config_for(providers, meta.id);
        let is_configured = if meta.needs_api_key {
            cfg.is_some_and(|c| c.api_key.is_some())
        } else {
            // CLI providers (no API key needed) — always show them.
            // If the binary isn't installed, the error surfaces when the user selects it.
            // This matches TUI behaviour where CLI providers are always listed.
            true
        };
        if is_configured {
            result.push((meta.id.to_string(), meta.display_name.to_string()));
        }
    }
    if let Some(ref customs) = providers.custom {
        for (name, cfg) in customs {
            if cfg.api_key.is_some() {
                result.push((format!("custom:{}", name), format!("Custom ({})", name)));
            }
        }
    }
    result
}

/// List ALL known providers, marking each as configured or not. Returns
/// `(provider_id, display_name, configured)` triples. Unlike
/// `configured_providers`, this surfaces providers the user hasn't set up
/// yet so a channel picker can show `🔒 OpenCode (needs API key)` next to
/// the active ones (issue #126: there was no way to see that OpenCode-API
/// existed as an option until the user already had the key in keys.toml).
pub fn all_known_providers_with_status(providers: &ProviderConfigs) -> Vec<(String, String, bool)> {
    // Built-in providers, sorted alphabetically by display name — matches the
    // TUI's `provider_display_order`, so a channel `/models` picker lists them
    // in the same order as the desktop UI instead of raw KNOWN_PROVIDERS
    // declaration order (which put e.g. Xiaomi between Minimax and OpenAI).
    let mut result: Vec<(String, String, bool)> = KNOWN_PROVIDERS
        .iter()
        .map(|meta| {
            let configured = if meta.needs_api_key {
                config_for(providers, meta.id).is_some_and(|c| c.api_key.is_some())
            } else {
                true
            };
            (
                meta.id.to_string(),
                meta.display_name.to_string(),
                configured,
            )
        })
        .collect();
    result.sort_by_key(|p| p.1.to_ascii_lowercase());

    // Custom providers follow as their own alphabetical block (the config's
    // custom map is a BTreeMap, so iteration is already key-sorted).
    if let Some(ref customs) = providers.custom {
        for (name, cfg) in customs {
            let configured = cfg.api_key.is_some();
            result.push((
                format!("custom:{}", name),
                format!("Custom ({})", name),
                configured,
            ));
        }
    }
    result
}

/// Path hint shown to users when they tap an unconfigured provider in a
/// channel `/models` picker — tells them where to add the API key.
pub fn keys_toml_path_hint() -> String {
    let dir = crate::config::profile::base_opencrabs_dir();
    let collapsed = crate::utils::string::tilde_home(&dir.display().to_string());
    format!("{collapsed}/keys.toml")
}

/// Canonical model list + default for a CLI provider, read straight from
/// the provider module's const tables. Returns `None` for non-CLI
/// providers. The channel `/models` menu uses this so the listed models
/// can never drift from what the provider actually serves — the
/// 2026-05-27 bug where OpenCode CLI users saw Claude model names came
/// from a hardcoded fallback that bypassed this lookup.
///
/// Two accepted name forms per provider (hyphen and underscore variants)
/// because both surface from different config-loading paths.
pub fn cli_supported_models(name: &str) -> Option<(Vec<String>, &'static str)> {
    use crate::brain::provider::{claude_cli, command_code_cli, opencode_cli};
    match name {
        // Discovered from the installed CLI (its --help aliases + the versions
        // it was seen to resolve), with the built-in const as a floor — so a
        // model released after build time (Opus 5) shows up without a code
        // change (#753).
        "claude-cli" | "claude_cli" => {
            Some((claude_cli::available_models(), claude_cli::DEFAULT_MODEL))
        }
        "opencode-cli" | "opencode_cli" => Some((
            opencode_cli::SUPPORTED_MODELS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            opencode_cli::DEFAULT_MODEL,
        )),
        "command-code-cli" | "command_code_cli" | "cmd-cli" => Some((
            command_code_cli::SUPPORTED_MODELS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            command_code_cli::DEFAULT_MODEL,
        )),
        _ => None,
    }
}

/// Find the TUI PROVIDERS index for a provider name/alias.
/// Returns None for custom providers (those map to 9+ dynamically).
pub fn tui_index_for_id(name: &str) -> Option<usize> {
    use crate::tui::onboarding::PROVIDERS;
    let normalized = normalize_provider_name(name);
    PROVIDERS
        .iter()
        .position(|p| !p.id.is_empty() && p.id == normalized)
}

/// All config sections, for the onboarding exclusive-enable sweep only.
///
/// A model switch does NOT touch enabled flags: direct_model_switch picks
/// the session pair and leaves provider enablement alone (stale comment
/// corrected, #2265).
pub fn all_config_sections(providers: &ProviderConfigs) -> Vec<String> {
    let mut sections: Vec<String> = KNOWN_PROVIDERS
        .iter()
        .map(|p| p.config_section.to_string())
        .collect();
    if let Some(ref customs) = providers.custom {
        for name in customs.keys() {
            sections.push(format!("providers.custom.{}", name));
        }
    }
    sections
}
