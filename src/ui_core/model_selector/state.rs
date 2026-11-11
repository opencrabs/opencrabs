//! Frontend-neutral provider and model selection state.
//!
//! This module owns request construction and result identity. It deliberately
//! contains no renderer, keyboard type, or terminal runtime dependency.

use crate::config::ProviderConfig;

pub use crate::config::stored_key::EXISTING_KEY_SENTINEL;
/// Provider definitions
pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        id: "anthropic",
        name: "Anthropic",
        models: &[], // Fetched from API
        key_label: "Setup Token",
        help_lines: &[
            "Claude Max / Code: run 'claude setup-token'",
            "Or paste API key from console.anthropic.com",
        ],
    },
    ProviderInfo {
        id: "openai",
        name: "OpenAI",
        models: &[],
        key_label: "API Key",
        help_lines: &["Get key from platform.openai.com"],
    },
    ProviderInfo {
        id: "github",
        name: "GitHub Copilot",
        models: &[],
        key_label: "OAuth",
        help_lines: &["Sign in with GitHub to use your Copilot subscription"],
    },
    ProviderInfo {
        id: "gemini",
        name: "Google Gemini",
        models: &[],
        key_label: "API Key",
        help_lines: &["Get key from aistudio.google.com"],
    },
    ProviderInfo {
        id: "openrouter",
        name: "OpenRouter",
        models: &[],
        key_label: "API Key",
        help_lines: &["Get key from openrouter.ai/keys"],
    },
    ProviderInfo {
        id: "minimax",
        name: "Minimax",
        models: &[], // Loaded from config.toml at runtime
        key_label: "API Key",
        help_lines: &["Get key from platform.minimax.io"],
    },
    ProviderInfo {
        id: "zai",
        name: "z.ai",
        models: &[], // Fetched from API
        key_label: "API Key",
        help_lines: &["Get key from open.bigmodel.cn"],
    },
    ProviderInfo {
        id: "moonshot",
        name: "Moonshot AI",
        models: &[], // Fetched from API
        key_label: "API Key",
        help_lines: &[
            "API plan: key from platform.moonshot.ai",
            "Coding plan: key from kimi.com (token subscription)",
        ],
    },
    ProviderInfo {
        id: "claude-cli",
        name: "Claude CLI",
        models: &["sonnet", "opus", "haiku"],
        key_label: "",
        help_lines: &[
            "Uses local 'claude' CLI subprocess - no API key needed",
            "Requires: npm install -g @anthropic-ai/claude-code",
        ],
    },
    ProviderInfo {
        id: "opencode-cli",
        name: "OpenCode CLI",
        models: &[],
        key_label: "",
        help_lines: &[
            "Uses local 'opencode' CLI subprocess - free models, no API key needed",
            "Requires: curl -fsSL https://opencode.ai/install | bash",
        ],
    },
    ProviderInfo {
        id: "codex-cli",
        name: "Codex CLI",
        models: &[
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
            "gpt-5.2",
        ],
        key_label: "",
        help_lines: &[
            "Uses local 'codex' CLI subprocess - ChatGPT/Codex auth, no API key needed",
            "Requires: npm install -g @openai/codex (then run 'codex' once to sign in)",
        ],
    },
    ProviderInfo {
        id: "command-code-cli",
        name: "Command Code CLI",
        models: &[
            "taste-1",
            "deepseek/deepseek-v4-flash",
            "moonshotai/Kimi-K2.7-Code",
            "zai-org/GLM-5.2",
            "xiaomi/mimo-v2.5",
            "MiniMaxAI/MiniMax-M2.7",
            "Qwen/Qwen3.6-Max-Preview",
        ],
        key_label: "",
        help_lines: &[
            "Uses local 'cmd' CLI subprocess - Command Code account, no API key needed",
            "Requires: npm install -g command-code (then run 'cmd login')",
        ],
    },
    ProviderInfo {
        id: "codex",
        name: "Codex",
        models: &[
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
            "gpt-5.2",
            "gpt-4o",
            "o3",
            "o4-mini",
        ],
        key_label: "OAuth",
        help_lines: &[
            "Authenticate with OpenAI Codex subscription via device-code flow",
            "No CLI needed - sign in through OpenCrabs onboarding",
        ],
    },
    ProviderInfo {
        id: "opencode",
        name: "OpenCode",
        models: &[], // Fetched from opencode.ai API
        key_label: "API Key",
        help_lines: &[
            "Go and Zen plans - opencode.ai",
            "Get key from opencode.ai/settings",
        ],
    },
    ProviderInfo {
        id: "qwen",
        name: "Qwen",
        models: &[
            "qwen3.6-plus",
            "qwen3-max",
            "qwen3-coder-plus",
            "qwen3.5-plus",
            "qwen-max",
            "qwen-plus",
            "qwen-flash",
        ],
        key_label: "API Key",
        help_lines: &[
            "DashScope OpenAI-compatible API (Alibaba Cloud Model Studio)",
            "Get key from bailian.console.aliyun.com or qwen.ai/apiplatform",
        ],
    },
    ProviderInfo {
        id: "ollama",
        name: "Ollama",
        models: &[], // Fetched from /api/tags
        key_label: "API Key",
        help_lines: &[
            "Local: runs on localhost:11434 - no key needed",
            "Cloud: set base_url + optional API key",
            "Requires: ollama serve (local) or cloud endpoint",
        ],
    },
    // Xiaomi MiMo - OpenAI-compatible, keyed. Get an API key at
    // platform.xiaomimimo.com.
    ProviderInfo {
        id: "xiaomi",
        name: "Xiaomi",
        models: &[], // Fetched live from /v1/models (chat models only)
        key_label: "API Key",
        help_lines: &[
            "Get an API key at platform.xiaomimimo.com",
            "OpenAI-compatible - MiMo v2.5 models",
        ],
    },
    // Built-in OpenCrabs - no key is typed. Onboarding requests the install token.
    ProviderInfo {
        id: "opencrabs",
        name: "OpenCrabs",
        models: &["deepseek-v4.1-flash"],
        key_label: "",
        help_lines: &[
            "Built in: no account or key needed",
            "Runs on the OpenCrabs inference service",
        ],
    },
    ProviderInfo {
        id: "", // dynamic - custom providers use runtime names
        name: "Custom OpenAI-Compatible",
        models: &[],
        key_label: "API Key",
        help_lines: &["Enter your own API endpoint"],
    },
];

pub struct ProviderInfo {
    /// Canonical provider id matching `KNOWN_PROVIDERS` (e.g. "anthropic", "claude-cli")
    /// Empty string for "Custom OpenAI-Compatible" (last entry) which is dynamic.
    pub id: &'static str,
    pub name: &'static str,
    pub models: &'static [&'static str],
    pub key_label: &'static str,
    pub help_lines: &'static [&'static str],
}

/// Codex OAuth device flow status
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CodexDeviceFlowStatus {
    /// Not started
    #[default]
    Idle,
    /// Waiting for user to enter code at verification URL
    WaitingForUser,
    /// User authorized, tokens saved
    Complete,
    /// Flow failed
    Failed(String),
}

pub const CUSTOM_PROVIDER_IDX: usize = PROVIDERS.len() - 1;
pub const CUSTOM_INSTANCES_START: usize = PROVIDERS.len();

/// A complete, immutable snapshot for one provider model request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FetchRequest {
    pub provider_index: usize,
    pub provider_id: String,
    pub provider_name: String,
    pub api_key: Option<String>,
    pub zhipu_endpoint: Option<String>,
    pub xiaomi_endpoint: Option<String>,
    pub moonshot_endpoint: Option<String>,
    pub base_url: Option<String>,
    pub opencode_endpoint: Option<String>,
}

impl FetchRequest {
    /// Capture exactly what the fetcher needs, including unsaved wizard edits.
    pub fn from_selector(selector: &ProviderSelectorState) -> Option<Self> {
        Self::from_selector_and_api_key(selector, selector.effective_api_key())
    }

    /// Bind an explicit credential to its provider. A late OAuth completion
    /// must not send its token to whichever provider is selected now.
    pub fn from_selector_and_api_key_for_provider(
        selector: &ProviderSelectorState,
        provider_id: &str,
        api_key: Option<String>,
    ) -> Option<Self> {
        if selector.provider_id() != provider_id {
            return None;
        }
        Self::from_selector_and_api_key(selector, api_key)
    }

    /// Capture a request with an explicit credential, such as a fresh OAuth
    /// token whose config write may not have landed yet.
    pub fn from_selector_and_api_key(
        selector: &ProviderSelectorState,
        api_key: Option<String>,
    ) -> Option<Self> {
        if !selector.supports_model_fetch() {
            return None;
        }
        if !selector.is_custom() && !selector.is_keyless() && api_key.is_none() {
            return None;
        }
        let info = selector.current_provider();
        let (provider_id, provider_name) = if selector.is_custom() {
            (selector.custom_name.clone(), selector.custom_name.clone())
        } else {
            (info.id.to_string(), info.name.to_string())
        };
        Some(Self {
            provider_index: selector.selected_provider,
            provider_id,
            provider_name,
            api_key,
            zhipu_endpoint: selector.zhipu_endpoint_str(),
            xiaomi_endpoint: selector.xiaomi_endpoint_str(),
            moonshot_endpoint: selector.is_moonshot().then(|| {
                if selector.moonshot_endpoint_type == 1 {
                    "coding".to_string()
                } else {
                    "api".to_string()
                }
            }),
            base_url: (!selector.base_url.trim().is_empty()).then(|| selector.base_url.clone()),
            opencode_endpoint: (selector.provider_id() == "opencode").then(|| {
                if selector.opencode_endpoint_type == 1 {
                    "zen".to_string()
                } else {
                    "go".to_string()
                }
            }),
        })
    }

    pub fn for_provider(index: usize) -> Option<Self> {
        let mut selector = ProviderSelectorState {
            selected_provider: index,
            ..Default::default()
        };
        selector.load_custom_names();
        selector.detect_existing_key();
        selector.reload_config_models();
        selector.load_custom_fields();
        Self::from_selector(&selector)
    }

    pub fn provider_index(provider: &str) -> Option<usize> {
        if let Some(index) = PROVIDERS.iter().position(|info| info.id == provider) {
            return Some(index);
        }
        let mut selector = ProviderSelectorState::default();
        selector.load_custom_names();
        selector
            .custom_names
            .iter()
            .position(|name| name == provider)
            .map(|index| CUSTOM_INSTANCES_START + index)
    }
}

/// One asynchronous response, carrying the request's provider identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FetchOutcome {
    pub provider_index: usize,
    pub provider_id: String,
    pub provider_name: String,
    pub models: Vec<String>,
    pub error: Option<String>,
}

/// Monotonic request identity shared by both frontends.
#[derive(Clone, Debug, Default)]
pub struct FetchLifecycle {
    generation: u64,
    active: Option<(u64, usize)>,
    pub error: Option<String>,
}

impl FetchLifecycle {
    pub fn begin(&mut self, request: &FetchRequest) -> u64 {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.active = Some((self.generation, request.provider_index));
        self.error = None;
        self.generation
    }

    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1).max(1);
        self.active = None;
        self.error = None;
    }

    pub fn is_fetching(&self) -> bool {
        self.active.is_some()
    }

    /// Consume a result only if it belongs to the current request and provider.
    pub fn finish(
        &mut self,
        generation: u64,
        provider_index: usize,
        outcome: FetchOutcome,
    ) -> Option<FetchOutcome> {
        if self.active != Some((generation, provider_index))
            || outcome.provider_index != provider_index
        {
            return None;
        }
        self.active = None;
        self.error = outcome.error.clone();
        Some(outcome)
    }
}

#[derive(Clone, Default)]
pub struct ProviderSelectorState {
    pub selected_provider: usize,
    pub custom_names: Vec<String>,
    pub has_existing_key: bool,
    pub api_key_input: String,
    pub api_key_cursor: usize,
    pub models: Vec<String>,
    pub config_models: Vec<String>,
    pub selected_model: usize,
    pub model_filter: String,
    pub models_fetching: bool,
    pub zhipu_endpoint_type: usize,
    pub moonshot_endpoint_type: usize,
    pub opencode_endpoint_type: usize,
    pub moonshot_plan: usize,
    pub xiaomi_endpoint_type: usize,
    pub base_url: String,
    pub custom_model: String,
    pub custom_name: String,
    pub editing_custom_key: Option<String>,
    pub context_window: String,
    pub focused_field: usize,
    pub showing_providers: bool,
    pub codex_user_code: Option<String>,
    pub codex_device_flow_status: CodexDeviceFlowStatus,
}

impl ProviderSelectorState {
    /// Get provider info for the currently selected provider.
    pub fn current_provider(&self) -> &ProviderInfo {
        let idx = if self.selected_provider >= CUSTOM_PROVIDER_IDX {
            CUSTOM_PROVIDER_IDX
        } else {
            self.selected_provider
        };
        &PROVIDERS[idx]
    }

    pub fn is_custom(&self) -> bool {
        self.selected_provider >= CUSTOM_PROVIDER_IDX
    }

    pub fn is_cli(&self) -> bool {
        let id = self.provider_id();
        id == "claude-cli" || id == "opencode-cli" || id == "codex-cli" || id == "command-code-cli"
    }

    pub fn is_oauth(&self) -> bool {
        let id = self.provider_id();
        id == "github" || id == "codex"
    }

    /// Whether the current provider needs NO API key from the user: CLI
    /// subprocesses (claude-cli, …) AND any API provider whose onboarding entry
    /// has an empty `key_label`. The single source of truth for "skip the
    /// API-key field" - use this instead of re-deriving
    /// `is_cli() || key_label.is_empty()` at each site.
    pub fn is_keyless(&self) -> bool {
        self.is_cli() || (!self.is_custom() && self.current_provider().key_label.is_empty())
    }

    /// Whether the current provider still owes the user an API key.
    ///
    /// A picker that advances past this state would commit a provider with no
    /// credentials and no way back to the key field, so every automatic
    /// advance checks it first (#2782). Custom providers answer `false`: their
    /// key lives in `CustomApiKey`, and a local endpoint legitimately has none.
    pub fn needs_api_key(&self) -> bool {
        !self.is_custom() && !self.is_keyless() && self.api_key_input.is_empty()
    }

    /// z.ai provider (canonical id `zai`, legacy `zhipu`).
    pub fn is_zhipu(&self) -> bool {
        self.provider_id() == "zai"
    }

    pub fn is_moonshot(&self) -> bool {
        self.provider_id() == "moonshot"
    }

    pub fn is_xiaomi(&self) -> bool {
        self.provider_id() == "xiaomi"
    }

    /// Get the canonical provider id for the current selection.
    pub fn provider_id(&self) -> &'static str {
        if self.selected_provider < CUSTOM_PROVIDER_IDX {
            PROVIDERS[self.selected_provider].id
        } else {
            "" // custom
        }
    }
}

/// Look up the index of a provider by its canonical id. Returns `None`
/// if the id isn't in `PROVIDERS`. Lets call-sites avoid hardcoding
/// positions so reordering the array doesn't cascade into the TUI.
pub fn index_of_provider(id: &str) -> Option<usize> {
    PROVIDERS.iter().position(|p| p.id == id)
}

/// The model a provider tile may switch to without a second screen.
///
/// A picker listing exactly one model has nothing to choose: the tile the
/// user just tapped IS the choice. v0.5.15 answered that tap with a second
/// screen holding the one model already marked ✓ (it is the active one), so
/// the tap read as dead. The built-in `opencrabs` provider has exactly one
/// model, which is how it surfaced. Returns `None` for zero or several
/// models, where the list is a real choice and must still be shown.
///
/// The single decision point for "one model means no choice" (#2782): the
/// channel pickers, the GUI provider row and the TUI wizard all resolve
/// through it, so no two surfaces can disagree about what a one-model
/// provider is. `channels::commands` re-exports it for the channel callers.
pub fn single_model_choice(models: &[String]) -> Option<&str> {
    match models {
        [only] => Some(only.as_str()),
        _ => None,
    }
}

impl ProviderSelectorState {
    /// The OpenCode plan's (`name`, chat `base_url`) pair, for config writes.
    /// Zen and Go serve different catalogs (#2040).
    pub fn opencode_endpoint(plan: usize) -> (&'static str, &'static str) {
        if plan == 1 {
            ("zen", "https://opencode.ai/zen/v1/chat/completions")
        } else {
            ("go", "https://opencode.ai/zen/go/v1/chat/completions")
        }
    }

    /// Whether the current provider supports live model fetching from API.
    pub fn supports_model_fetch(&self) -> bool {
        // Custom providers: always try /v1/models if base_url is set
        if self.is_custom() {
            return !self.base_url.trim().is_empty();
        }
        matches!(
            self.provider_id(),
            "anthropic"
                | "openai"
                | "github"
                | "gemini"
                | "openrouter"
                | "xiaomi"
                | "zai"
                | "moonshot"
                | "minimax"
                | "opencode-cli"
                | "codex-cli"
                | "command-code-cli"
                | "codex"
                | "opencode"
                | "claude-cli"
                | "ollama"
        )
    }

    /// Maximum number of fields for the current provider type.
    pub fn max_field(&self) -> usize {
        if self.is_custom() {
            6 // provider(0), base_url(1), api_key(2), model(3), name(4), context_window(5)
        } else if self.is_zhipu() || self.is_xiaomi() || self.is_moonshot() {
            4 // provider(0), endpoint_type(1), api_key(2), model(3)
        } else {
            3 // provider(0), api_key(1), model(2)
        }
    }

    /// Whether the current api_key_input holds a pre-existing key sentinel.
    pub fn has_existing_key_sentinel(&self) -> bool {
        crate::config::stored_key::is_stored_marker(&self.api_key_input)
    }

    /// Visual display order: named providers sorted alphabetically,
    /// then existing custom instances, then "+ New Custom" last.
    pub fn provider_display_order(&self) -> Vec<usize> {
        let num_customs = self.custom_names.len();
        // Named providers: everything except the last "Custom" sentinel
        let mut static_indices: Vec<usize> = (0..CUSTOM_PROVIDER_IDX).collect();
        static_indices.sort_by_key(|&i| PROVIDERS[i].name.to_ascii_lowercase());
        static_indices
            .into_iter()
            .chain(CUSTOM_INSTANCES_START..CUSTOM_INSTANCES_START + num_customs)
            .chain(std::iter::once(CUSTOM_PROVIDER_IDX))
            .collect()
    }

    /// Provider refresh order for a catalogue covering every provider.
    /// The active provider goes first; saved custom instances stay included.
    pub fn refresh_order(&self, active_provider: &str) -> Vec<usize> {
        let active = PROVIDERS
            .iter()
            .position(|provider| provider.id == active_provider)
            .or_else(|| {
                self.custom_names
                    .iter()
                    .position(|name| name == active_provider)
                    .map(|index| CUSTOM_INSTANCES_START + index)
            });
        let mut order = self.provider_display_order();
        if let Some(position) =
            active.and_then(|index| order.iter().position(|candidate| *candidate == index))
        {
            order.swap(0, position);
        }
        order
    }

    /// Check if a provider at the given index has credentials configured.
    /// Used by renderers to show a green indicator in the provider list.
    /// Does NOT mutate state - pure read from config.
    pub fn provider_has_credentials(&self, idx: usize) -> bool {
        let config = match crate::config::Config::load() {
            Ok(c) => c,
            Err(_) => return false,
        };

        if idx < CUSTOM_PROVIDER_IDX {
            let id = PROVIDERS[idx].id;
            match id {
                // CLI providers - always "configured" if binary exists
                "claude-cli" | "opencode-cli" | "codex-cli" | "command-code-cli" => {
                    let bin = match id {
                        "claude-cli" => "claude",
                        "opencode-cli" => "opencode",
                        "command-code-cli" => "command-code",
                        _ => "codex",
                    };
                    which::which(bin).is_ok()
                }
                // OAuth providers - check for token/accounts
                "github" => config
                    .providers
                    .github
                    .as_ref()
                    .and_then(|p| p.api_key.as_ref())
                    .is_some_and(|k| !k.is_empty()),
                "codex" => {
                    // Check for OAuth tokens at ~/.opencrabs/auth/codex.json
                    let token_path = crate::config::opencrabs_home()
                        .join("auth")
                        .join("codex.json");
                    token_path.exists()
                }
                "qwen" => config
                    .providers
                    .qwen
                    .as_ref()
                    .and_then(|p| p.api_key.as_ref())
                    .is_some_and(|k| !k.is_empty()),
                // Keyless API providers (empty key_label, e.g. Xiaomi): the
                // proxy supplies the key, so they're always configured/ready.
                _ if PROVIDERS[idx].key_label.is_empty() => true,
                // Standard API key providers
                _ => crate::utils::providers::config_for(&config.providers, id)
                    .and_then(|p| p.api_key.as_ref())
                    .is_some_and(|k| !k.is_empty()),
            }
        } else if idx == CUSTOM_PROVIDER_IDX {
            false // "+ New Custom" - never configured
        } else {
            // Existing custom provider
            let custom_idx = idx - CUSTOM_INSTANCES_START;
            self.custom_names
                .get(custom_idx)
                .and_then(|name| config.providers.custom_by_name(name))
                .and_then(|p| p.api_key.as_ref())
                .is_some_and(|k| !k.is_empty())
        }
    }

    /// Detect if an API key exists in config for the current provider.
    /// Sets `has_existing_key` flag and `api_key_input` sentinel. Never loads actual key.
    pub fn detect_existing_key(&mut self) {
        match crate::config::Config::load() {
            Ok(config) => self.detect_existing_key_from(&config),
            Err(_) => {
                self.api_key_input.clear();
                self.has_existing_key = false;
                self.selected_model = 0;
                self.model_filter.clear();
            }
        }
    }

    /// Same as [`Self::detect_existing_key`], but answers from the config it is
    /// handed instead of the live `config.toml`. A caller that was given a
    /// specific config must use this one: reading disk made the answer depend
    /// on the machine running it (#2077).
    pub fn detect_existing_key_from(&mut self, config: &crate::config::Config) {
        fn has_nonempty_key(p: Option<&ProviderConfig>) -> bool {
            p.and_then(|p| p.api_key.as_ref())
                .is_some_and(|k| !k.is_empty())
        }

        self.api_key_input.clear();
        self.has_existing_key = false;

        {
            let has_key = if self.selected_provider < CUSTOM_PROVIDER_IDX {
                let id = PROVIDERS[self.selected_provider].id;
                if self.is_cli() {
                    false // CLI providers - no API key
                } else if self.is_oauth() {
                    // OAuth providers - check for token file, not API key
                    let id = PROVIDERS[self.selected_provider].id;
                    if id == "codex" {
                        let token_path = crate::config::opencrabs_home()
                            .join("auth")
                            .join("codex.json");
                        token_path.exists()
                    } else if id == "github" {
                        config
                            .providers
                            .github
                            .as_ref()
                            .and_then(|p| p.api_key.as_ref())
                            .is_some_and(|k| !k.is_empty())
                    } else {
                        false
                    }
                } else {
                    has_nonempty_key(crate::utils::providers::config_for(&config.providers, id))
                }
            } else if self.selected_provider == CUSTOM_PROVIDER_IDX {
                // New custom - start with blank fields
                self.custom_name.clear();
                self.base_url.clear();
                self.custom_model.clear();
                self.context_window.clear();
                self.editing_custom_key = None;
                false
            } else {
                // Existing custom provider
                let custom_idx = self.selected_provider - CUSTOM_INSTANCES_START;
                if let Some(cname) = self.custom_names.get(custom_idx).cloned() {
                    if let Some(c) = config.providers.custom_by_name(&cname) {
                        self.editing_custom_key = Some(cname.clone());
                        self.custom_name = cname;
                        self.base_url = c.base_url.clone().unwrap_or_default();
                        self.custom_model = c.default_model.clone().unwrap_or_default();
                        self.context_window = c
                            .context_window
                            .map(|cw| cw.to_string())
                            .unwrap_or_default();
                        c.api_key.as_ref().is_some_and(|k| !k.is_empty())
                    } else {
                        false
                    }
                } else {
                    false
                }
            };

            self.has_existing_key = has_key;
            if has_key {
                self.api_key_input = EXISTING_KEY_SENTINEL.to_string();
                self.api_key_cursor = 0;
            }
        }

        // Clear model selection when provider changes
        self.selected_model = 0;
        self.model_filter.clear();
    }

    /// Load custom provider fields when navigating to an existing custom (10+),
    /// clear fields for new custom (9), load zhipu endpoint type for index 6,
    /// load xiaomi endpoint type.
    pub fn load_custom_fields(&mut self) {
        if self.is_zhipu()
            && let Ok(config) = crate::config::Config::load()
            && let Some(zai_cfg) = &config.providers.zai
        {
            self.zhipu_endpoint_type = match zai_cfg.endpoint_type.as_deref() {
                Some("coding") => 1,
                _ => 0,
            };
        }
        if self.is_xiaomi()
            && let Ok(config) = crate::config::Config::load()
            && let Some(xiaomi) = &config.providers.xiaomi
        {
            self.xiaomi_endpoint_type = match xiaomi.endpoint_type.as_deref() {
                Some("token-plan") => 1,
                _ => 0,
            };
        }
        if self.provider_id() == "opencode"
            && let Ok(config) = crate::config::Config::load()
            && let Some(oc) = &config.providers.opencode
        {
            self.opencode_endpoint_type = if oc.endpoint_type.as_deref() == Some("zen") {
                1
            } else {
                0
            };
        }
        if self.is_moonshot()
            && let Ok(config) = crate::config::Config::load()
            && let Some(moonshot) = &config.providers.moonshot
        {
            self.moonshot_endpoint_type = match moonshot.endpoint_type.as_deref() {
                Some("coding") => 1,
                _ => 0,
            };
            self.moonshot_plan = moonshot
                .plan
                .as_deref()
                .and_then(|p| {
                    crate::brain::provider::kimi_plan::PLAN_TIERS
                        .iter()
                        .position(|t| t.eq_ignore_ascii_case(p.trim()))
                })
                .unwrap_or(0);
        }
        if self.selected_provider == CUSTOM_PROVIDER_IDX {
            self.custom_name.clear();
            self.base_url.clear();
            self.custom_model.clear();
            self.context_window.clear();
            self.editing_custom_key = None;
        } else if self.selected_provider >= CUSTOM_INSTANCES_START {
            let custom_idx = self.selected_provider - CUSTOM_INSTANCES_START;
            if let Some(cname) = self.custom_names.get(custom_idx).cloned()
                && let Ok(config) = crate::config::Config::load()
                && let Some(c) = config.providers.custom_by_name(&cname)
            {
                self.editing_custom_key = Some(cname.clone());
                self.custom_name = cname;
                self.base_url = c.base_url.clone().unwrap_or_default();
                self.custom_model = c.default_model.clone().unwrap_or_default();
                self.context_window = c
                    .context_window
                    .map(|cw| cw.to_string())
                    .unwrap_or_default();
                if c.api_key.as_ref().is_some_and(|k| !k.is_empty()) {
                    self.api_key_input = EXISTING_KEY_SENTINEL.to_string();
                }
            }
        }
    }

    /// Load the actual API key value from config for the current provider.
    /// Used when making API calls (fetch models, save config). Returns None if no key.
    pub fn load_api_key_from_config(&self) -> Option<String> {
        let config = crate::config::Config::load().ok()?;
        if self.selected_provider < CUSTOM_PROVIDER_IDX {
            crate::utils::providers::config_for(
                &config.providers,
                PROVIDERS[self.selected_provider].id,
            )
            .and_then(|p| p.api_key.clone())
        } else if self.selected_provider >= CUSTOM_INSTANCES_START {
            let custom_idx = self.selected_provider - CUSTOM_INSTANCES_START;
            self.custom_names.get(custom_idx).and_then(|name| {
                config
                    .providers
                    .custom_by_name(name)
                    .and_then(|p| p.api_key.clone())
            })
        } else {
            None
        }
        .filter(|k| !k.is_empty())
    }

    /// The API key to use for a live call (model fetch, save, etc.): a freshly
    /// typed key when the user entered one, otherwise the saved key from config.
    /// Never returns the [`EXISTING_KEY_SENTINEL`] placeholder - sending that as
    /// a bearer token is what 401'd custom providers whose real key lives in
    /// keys.toml (e.g. Model Studio). `None` means no key is available (a keyless
    /// local endpoint), so the caller sends no auth.
    pub fn effective_api_key(&self) -> Option<String> {
        if let Some(typed) = crate::config::stored_key::real_key(&self.api_key_input) {
            return Some(typed.to_string());
        }
        self.load_api_key_from_config()
    }

    /// Resolve the effective API key: user-typed key if present, else config key.
    pub fn resolve_api_key(&self) -> Option<String> {
        match crate::config::stored_key::real_key(&self.api_key_input) {
            Some(typed) => Some(typed.to_string()),
            None => self.load_api_key_from_config(),
        }
    }

    /// Zhipu endpoint type as string for API calls.
    pub fn zhipu_endpoint_str(&self) -> Option<String> {
        if self.is_zhipu() {
            Some(
                if self.zhipu_endpoint_type == 1 {
                    "coding"
                } else {
                    "api"
                }
                .to_string(),
            )
        } else {
            None
        }
    }

    /// Xiaomi MiMo endpoint type as string for API calls.
    pub fn xiaomi_endpoint_str(&self) -> Option<String> {
        if self.is_xiaomi() {
            Some(
                if self.xiaomi_endpoint_type == 1 {
                    "token-plan"
                } else {
                    "api"
                }
                .to_string(),
            )
        } else {
            None
        }
    }

    // ── Model list management ───────────────────────────────────────

    /// Merge any config-persisted models into the live-fetched list
    /// so user-pasted models that the provider's `/v1/models` doesn't
    /// list survive the fetch. Reloads `config_models` first so the
    /// merge sees the latest disk state. Fetched names keep their
    /// order at the top; config-only names get appended at the end.
    pub fn merge_config_models_into_fetched(&mut self) {
        self.reload_config_models();
        let extras: Vec<String> = self
            .config_models
            .iter()
            .filter(|m| !self.models.iter().any(|x| x == *m))
            .cloned()
            .collect();
        self.models.extend(extras);
    }

    /// Reload config_models for the currently selected provider.
    pub fn reload_config_models(&mut self) {
        self.config_models.clear();
        if let Ok(config) = crate::config::Config::load() {
            if self.is_cli() {
                return; // CLI - static or fetched, no config models
            }
            if self.selected_provider < CUSTOM_PROVIDER_IDX {
                let id = PROVIDERS[self.selected_provider].id;
                if let Some(p) = crate::utils::providers::config_for(&config.providers, id)
                    && !p.models.is_empty()
                {
                    self.config_models = p.models.clone();
                    return;
                }
            } else if self.selected_provider >= CUSTOM_PROVIDER_IDX {
                // The provider being EDITED, resolved by name, not whichever
                // custom provider happens to be active. Reading `active_custom`
                // here handed a new provider someone else's catalogue: the
                // persist step merges `config_models` into what it writes, so
                // a freshly created provider was saved listing models it does
                // not serve, and when its own `/v1/models` fetch had not landed
                // yet that foreign list was all it got. The same eight names
                // ended up on several unrelated providers this way.
                //
                // A provider with no entry yet contributes nothing. Falling
                // through to the static catalogue below would be the same
                // mistake in a different costume: inventing names for an
                // endpoint nobody has queried.
                let custom_idx = self.selected_provider.checked_sub(CUSTOM_INSTANCES_START);
                let by_name = custom_idx
                    .and_then(|i| self.custom_names.get(i))
                    .or(Some(&self.custom_name))
                    .filter(|name| !name.is_empty())
                    .and_then(|name| config.providers.custom_by_name(name));
                if let Some(p) = by_name
                    && !p.models.is_empty()
                {
                    self.config_models = p.models.clone();
                }
                return;
            }
        }
        self.config_models =
            crate::ui_core::model_selector::load_default_models(self.provider_id());
    }

    /// All model names for the current provider, with the live fetch
    /// merged on top of the config-persisted list. Any model the user
    /// has previously pasted in (and saved) survives even when the
    /// provider's `/v1/models` endpoint omits it on the next call.
    /// Fetched names win for ordering; config-only names are appended
    /// at the end in their original order. Falls back to the static
    /// provider catalogue when nothing's been fetched or saved yet.
    pub fn all_model_names(&self) -> Vec<&str> {
        if self.models.is_empty() && self.config_models.is_empty() {
            return self.current_provider().models.to_vec();
        }
        let mut out: Vec<&str> = Vec::with_capacity(self.models.len() + self.config_models.len());
        for m in &self.models {
            out.push(m.as_str());
        }
        for m in &self.config_models {
            if !out.contains(&m.as_str()) {
                out.push(m.as_str());
            }
        }
        out
    }

    /// The single model this provider offers, when the picker can be skipped.
    ///
    /// `None` for zero or several models, for a typed filter (the user is
    /// searching, so the list is theirs to narrow), and for a provider that
    /// still owes an API key. One model and nothing else means the only
    /// possible choice is already made, so asking again is a screen with no
    /// decision on it (#2782).
    pub fn sole_model_choice(&self) -> Option<String> {
        if self.is_custom() || self.needs_api_key() || !self.model_filter.trim().is_empty() {
            return None;
        }
        let offered: Vec<String> = self
            .all_model_names()
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        single_model_choice(&offered).map(str::to_string)
    }

    /// Model names filtered by `model_filter` (case-insensitive substring match).
    pub fn filtered_model_names(&self) -> Vec<&str> {
        let all = self.all_model_names();
        let mut out = if self.model_filter.is_empty() {
            all
        } else {
            let q = self.model_filter.to_lowercase();
            all.into_iter()
                .filter(|m| m.to_lowercase().contains(&q))
                .collect()
        };
        // Newest first, here rather than in all_model_names: that function's
        // fetched-then-config insertion order is a contract other callers
        // rely on. Ordering is purely what the picker shows, so it belongs on
        // the display path and never touches config.models on disk (#1057).
        crate::ui_core::model_selector::sort_newest_first(&mut out);
        out
    }

    /// Number of models available after applying the current filter.
    pub fn model_count(&self) -> usize {
        self.filtered_model_names().len()
    }

    /// Get the selected model name (resolves through filter).
    ///
    /// Three branches, in order:
    ///   1. Filter matches something → pick `filtered[selected_model]`.
    ///   2. Filter matches nothing AND filter is non-empty → use the
    ///      typed text itself as the model name. This is the escape
    ///      hatch for new models that aren't in the hardcoded list yet
    ///      (e.g. user types `MiniMax-M3` on a build where the suggestion
    ///      list still only shows M2.7 / M2.5 / M2.1). The wizard render
    ///      should surface this so the user can see what will commit.
    ///   3. Filter is empty AND nothing matches → fall back to the first
    ///      entry in the full list (default behaviour for a fresh
    ///      provider with no typed input).
    pub fn selected_model_name(&self) -> &str {
        let filtered = self.filtered_model_names();
        if let Some(name) = filtered.get(self.selected_model) {
            name
        } else if !self.model_filter.trim().is_empty() {
            // Typed text becomes the model name when there's no list
            // match. Without this branch the wizard silently fell back
            // to "first item in the list", losing the user's input -
            // a user typing `MiniMax-M3` on a build before this fix
            // would end up configured for `MiniMax-M2.7`.
            self.model_filter.trim()
        } else {
            self.all_model_names().first().copied().unwrap_or("")
        }
    }

    /// Resolve `selected_model` index from `custom_model` string.
    pub fn resolve_selected_model_index(&mut self) {
        if self.custom_model.is_empty() {
            return;
        }
        let all = self.all_model_names();
        if let Some(idx) = all.iter().position(|m| *m == self.custom_model) {
            self.selected_model = idx;
        }
    }

    /// Cache existing custom provider names from config.
    pub fn load_custom_names(&mut self) {
        self.custom_names = crate::config::Config::load()
            .ok()
            .and_then(|c| c.providers.custom.map(|m| m.keys().cloned().collect()))
            .unwrap_or_default();
    }
}
