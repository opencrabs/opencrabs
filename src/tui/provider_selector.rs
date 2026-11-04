//! Shared provider + model selection state and logic.
//!
//! Used by both the `/models` dialog and the `/onboard` wizard to avoid
//! duplicate code that falls out of sync.

use crate::config::ProviderConfig;

/// Sentinel value stored in api_key_input when a key was loaded from config.
/// The actual key is never held in memory, this just signals "key exists".
///
/// Re-exported from the config layer, which owns the marker and the only
/// sanctioned way to read past it. It used to be spelled out here and at seven
/// other call sites, each with its own equality test, and equality is wrong
/// once a seeded field has been typed into (#1075).
pub use crate::config::stored_key::EXISTING_KEY_SENTINEL;

/// Provider definitions (index → info).
/// Last entry is always "Custom OpenAI-Compatible".
pub use crate::tui::onboarding::{PROVIDERS, ProviderInfo};

/// Index of the "Custom OpenAI-Compatible" sentinel (always last in PROVIDERS).
pub const CUSTOM_PROVIDER_IDX: usize = PROVIDERS.len() - 1;

/// First index used for existing custom provider instances (stored in config).
pub const CUSTOM_INSTANCES_START: usize = PROVIDERS.len();

/// Shared state for provider + model selection.
/// Both `/models` dialog and `/onboard` wizard embed this struct.
#[derive(Default)]
pub struct ProviderSelectorState {
    /// Currently selected provider index (0..CUSTOM_PROVIDER_IDX = static,
    /// CUSTOM_PROVIDER_IDX = new custom, CUSTOM_INSTANCES_START+ = existing customs)
    pub selected_provider: usize,
    /// Cached list of existing custom provider names
    pub custom_names: Vec<String>,
    /// Whether a key exists in config (boolean flag only — never load actual key into UI)
    pub has_existing_key: bool,
    /// User-typed API key, or EXISTING_KEY_SENTINEL when loaded from config
    pub api_key_input: String,
    /// Cursor position in api_key_input
    pub api_key_cursor: usize,
    /// Models fetched live from provider API
    pub models: Vec<String>,
    /// Models loaded from config.toml (fallback when API fetch not available)
    pub config_models: Vec<String>,
    /// Currently selected model index in filtered list
    pub selected_model: usize,
    /// Live search filter for models (case-insensitive substring match)
    pub model_filter: String,
    /// Whether an async model fetch is in progress
    pub models_fetching: bool,
    /// z.ai GLM endpoint type: 0=API, 1=Coding
    pub zhipu_endpoint_type: usize,
    /// Moonshot AI endpoint type: 0=API plan, 1=Coding (token) plan
    pub moonshot_endpoint_type: usize,
    /// OpenCode plan: 0 = Go (default), 1 = Zen (#2040).
    pub opencode_endpoint_type: usize,
    /// Kimi Coding-plan tier index into `kimi_plan::PLAN_TIERS`
    /// (0=moderato, 1=allegretto, 2=allegro, 3=vivace). Only used when
    /// `moonshot_endpoint_type == 1` (coding plan).
    pub moonshot_plan: usize,
    /// Xiaomi MiMo endpoint type: 0=API, 1=Token Plan
    pub xiaomi_endpoint_type: usize,
    /// Base URL for custom providers
    pub base_url: String,
    /// Model name for custom providers (free-text)
    pub custom_model: String,
    /// Identifier name for custom provider (e.g. "nvidia", "ollama")
    pub custom_name: String,
    /// Original name of the custom provider entry being edited, captured
    /// when the dialog opened. `Some(name)` = editing an existing entry;
    /// `None` = adding a new one. Save writes back to `editing_custom_key`
    /// (renaming the TOML table key if `custom_name` differs) instead of
    /// naively inserting at `custom_name` — prevents duplicate entries and
    /// api_key loss on rename.
    pub editing_custom_key: Option<String>,
    /// Context window size for custom providers (digits only)
    pub context_window: String,
    /// Which field is currently focused (numbering varies by provider type)
    pub focused_field: usize,
    /// Whether the provider list is expanded/visible
    pub showing_providers: bool,
    /// Codex OAuth device flow: user code to display
    pub codex_user_code: Option<String>,
    /// Codex OAuth device flow: current status
    pub codex_device_flow_status: crate::tui::onboarding::CodexDeviceFlowStatus,
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
    /// API-key field" — use this instead of re-deriving
    /// `is_cli() || key_label.is_empty()` at each site.
    pub fn is_keyless(&self) -> bool {
        self.is_cli() || (!self.is_custom() && self.current_provider().key_label.is_empty())
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

    /// Check if a provider at the given index has credentials configured.
    /// Used by renderers to show a green indicator in the provider list.
    /// Does NOT mutate state — pure read from config.
    pub fn provider_has_credentials(&self, idx: usize) -> bool {
        let config = match crate::config::Config::load() {
            Ok(c) => c,
            Err(_) => return false,
        };

        if idx < CUSTOM_PROVIDER_IDX {
            let id = PROVIDERS[idx].id;
            match id {
                // CLI providers — always "configured" if binary exists
                "claude-cli" | "opencode-cli" | "codex-cli" | "command-code-cli" => {
                    let bin = match id {
                        "claude-cli" => "claude",
                        "opencode-cli" => "opencode",
                        "command-code-cli" => "command-code",
                        _ => "codex",
                    };
                    which::which(bin).is_ok()
                }
                // OAuth providers — check for token/accounts
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
            false // "+ New Custom" — never configured
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
                    false // CLI providers — no API key
                } else if self.is_oauth() {
                    // OAuth providers — check for token file, not API key
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
                // New custom — start with blank fields
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
    /// Never returns the [`EXISTING_KEY_SENTINEL`] placeholder — sending that as
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
                return; // CLI — static or fetched, no config models
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
        self.config_models = load_default_models(self.provider_id());
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
        crate::tui::model_order::sort_newest_first(&mut out);
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
            // to "first item in the list", losing the user's input —
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

/// Map an API model id to a human-readable display label.
/// Returns the id itself when no special label is defined.
/// Used by /models, onboarding, and the footer to show friendly names
/// for models whose API id is an opaque alias (e.g. qwen-oauth's `coder-model`).
pub fn model_display_label(model_id: &str) -> &str {
    match model_id {
        "qwen-3.7-max" | "qwen3.7-max" | "qwen-latest-series" | "qwen-latest-series-invite" => {
            "Qwen 3.7 Max"
        }
        "qwen-3.7-plus" | "qwen3.7-plus" | "qwen-3.7-plus-preview" => "Qwen 3.7 Plus",
        "qwen-3.6-max-preview" | "qwen3.6-max-preview" => "Qwen 3.6 Max Preview",
        "coder-model" | "qwen-3.6-plus" | "qwen3.6-plus" => "Qwen 3.6 Plus",
        "qwen-3.5-plus" | "qwen3.5-plus" => "Qwen 3.5 Plus",
        "minimax-m2.5" => "Minimax M2.5",
        "minimax-m2.7" => "Minimax M2.7",
        "minimax-m3" => "Minimax M3",
        "mimo-v2-omni" | "mimo-v2-omni-free" => "Mimo V2 Omni",
        "mimo-v2-pro" | "mimo-v2-pro-free" => "Mimo V2 Pro",
        "kimi-k2.6" => "Kimi K2.6",
        "kimi-k2.5" | "kimi-k2-5" => "Kimi K2.5",
        "glm-5.1" => "GLM 5.1",
        "glm-5-turbo" => "GLM 5 Turbo",
        // Bare Claude CLI aliases are a MOVING pointer to the newest release of
        // that family (the CLI documents them as "an alias for the latest
        // model"), so they must not be labelled with a fixed version — `fable`
        // and `fable-5` both rendering "Fable 5" put the same label on two rows
        // of the picker (#754). Label them as the pointer they are.
        "fable" => "Fable (latest)",
        "opus" => "Opus (latest)",
        "sonnet" => "Sonnet (latest)",
        "haiku" => "Haiku (latest)",
        "fable-5" => "Fable 5",
        "opus-4-8" => "Opus 4.8",
        "opus-4-7" => "Opus 4.7",
        "opus-4-6" => "Opus 4.6",
        "sonnet-4-6" => "Sonnet 4.6",
        "haiku-4-5" => "Haiku 4.5",
        other => prettify_claude_cli_model(other).unwrap_or(other),
    }
}

/// Fallback prettifier for Claude CLI shorthand models we haven't
/// hardcoded yet. Matches `opus-X-Y` / `sonnet-X-Y` / `haiku-X-Y` and
/// returns "Opus X.Y", "Sonnet X.Y", "Haiku X.Y". The `&'static str`
/// return matches the main match arm; per-id strings are leaked into
/// a process-wide cache (one slot per distinct model id ever observed)
/// so the same id never re-leaks.
fn prettify_claude_cli_model(model: &str) -> Option<&'static str> {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    static PRETTIFIED: LazyLock<Mutex<HashMap<String, &'static str>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    let (family, rest) = if let Some(r) = model.strip_prefix("opus-") {
        ("Opus", r)
    } else if let Some(r) = model.strip_prefix("sonnet-") {
        ("Sonnet", r)
    } else if let Some(r) = model.strip_prefix("fable-") {
        ("Fable", r)
    } else {
        let r = model.strip_prefix("haiku-")?;
        ("Haiku", r)
    };
    // Versions come in two shapes: `4-8` (major-minor) and `5` (major only).
    // Requiring the `-` meant a major-only release rendered as the raw id —
    // "opus-5" sat un-formatted right below "Opus 4.8" (#754).
    let version = match rest.split_once('-') {
        Some((major, minor)) => {
            if major.is_empty()
                || minor.is_empty()
                || !major.chars().all(|c| c.is_ascii_digit())
                || !minor.chars().all(|c| c.is_ascii_digit())
            {
                return None;
            }
            format!("{major}.{minor}")
        }
        None => {
            if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            rest.to_string()
        }
    };

    let mut cache = PRETTIFIED.lock().ok()?;
    if let Some(existing) = cache.get(model) {
        return Some(existing);
    }
    let pretty: &'static str = Box::leak(format!("{family} {version}").into_boxed_str());
    cache.insert(model.to_string(), pretty);
    Some(pretty)
}

/// Load default models from embedded config.toml.example for a provider.
pub fn load_default_models(provider_id: &str) -> Vec<String> {
    let config_content = include_str!("../../config.toml.example");
    let mut models = Vec::new();

    if let Ok(config) = config_content.parse::<toml::Value>()
        && let Some(providers) = config.get("providers")
    {
        // Map provider id to config.toml.example section key
        // (config uses underscore: "claude_cli", but provider id uses hyphen: "claude-cli")
        let section_key = match provider_id {
            "claude-cli" => "claude_cli",
            "opencode-cli" => "opencode_cli",
            "codex-cli" => "codex_cli",
            "command-code-cli" => "command_code_cli",
            "codex" => "codex", // Codex OAuth
            "" => "custom",     // empty id = custom providers
            other => other,
        };

        if section_key == "custom" {
            // Custom providers: merge models from all custom sections
            if let Some(custom) = providers.get("custom")
                && let Some(custom_table) = custom.as_table()
            {
                for (_name, entry) in custom_table {
                    if let Some(models_arr) = entry.get("models").and_then(|m| m.as_array()) {
                        for model in models_arr {
                            if let Some(model_str) = model.as_str()
                                && !models.contains(&model_str.to_string())
                            {
                                models.push(model_str.to_string());
                            }
                        }
                    }
                }
            }
        } else if let Some(section) = providers.get(section_key)
            && let Some(models_arr) = section.get("models").and_then(|m| m.as_array())
        {
            for model in models_arr {
                if let Some(model_str) = model.as_str() {
                    models.push(model_str.to_string());
                }
            }
        }
    }

    tracing::debug!(
        "Loaded {} default models from config.toml.example for provider '{}'",
        models.len(),
        provider_id
    );
    models
}
