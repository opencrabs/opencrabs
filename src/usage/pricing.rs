//! Model pricing configuration
//!
//! Loaded from `~/.opencrabs/usage_pricing.toml` at runtime.
//! No compiled-in fallback — if the file is missing or broken, an error is returned.
//! Users can edit the file live — changes take effect on next `/usage` open.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

/// Minimum matched-substring length for a fuzzy family match, so a tiny
/// coincidental overlap can't price a model against an unrelated family.
const MIN_MATCH_OVERLAP: usize = 3;

/// Conservative default input rate (USD per 1M tokens) billed when no pricing
/// entry matches a model (#1717). Sonnet-class mid-tier: an unpriced model's
/// estimate errs visibly high rather than silently to $0, while a mistaken
/// default stays within the order of magnitude of a real invoice.
pub const DEFAULT_UNKNOWN_INPUT_PER_M: f64 = 3.0;

/// Conservative default output rate (USD per 1M tokens) for unpriced models.
/// See [`DEFAULT_UNKNOWN_INPUT_PER_M`].
pub const DEFAULT_UNKNOWN_OUTPUT_PER_M: f64 = 15.0;

/// Models already warned about this process lifetime (warn once per model,
/// not per turn, #1717).
static WARNED_UNKNOWN_MODELS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn warn_unknown_model_once(model: &str) {
    let mut warned = WARNED_UNKNOWN_MODELS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if warned.insert(model.to_lowercase()) {
        tracing::warn!(
            target: "pricing",
            model,
            input_per_m = DEFAULT_UNKNOWN_INPUT_PER_M,
            output_per_m = DEFAULT_UNKNOWN_OUTPUT_PER_M,
            "no pricing entry matched: billing a conservative default rate \
             (marked ~ in /usage). Add the model to usage_pricing.toml for exact pricing"
        );
    }
}

/// How a cost number was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostOrigin {
    /// Resolved from a `usage_pricing.toml` entry.
    Priced,
    /// No entry matched: [`DEFAULT_UNKNOWN_INPUT_PER_M`] /
    /// [`DEFAULT_UNKNOWN_OUTPUT_PER_M`] applied (#1717).
    Estimated,
}

/// A computed cost plus how it was produced, so callers can tell a table
/// price from the conservative default (#1717).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostBreakdown {
    pub total: f64,
    pub origin: CostOrigin,
}

/// Per-model rates with the documented cache defaults already applied, plus how
/// they were resolved. Unlike a raw [`PricingEntry`], every field is concrete:
/// `cache_write_per_m` and `cache_read_per_m` never surface as `None`, because
/// the dashboard needs a number to multiply, not an option to branch on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRates {
    pub input_per_m: f64,
    pub output_per_m: f64,
    pub cache_write_per_m: f64,
    pub cache_read_per_m: f64,
    pub origin: CostOrigin,
}

/// Cache money for one model's cached-token volume: what the cache reads
/// actually cost, what writing the cache cost, what those same read tokens
/// would have cost at the plain input rate, and the difference.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CacheCost {
    /// Cache-READ tokens at `cache_read_per_m`.
    pub read_cost: f64,
    /// Cache-WRITE tokens at `cache_write_per_m`.
    pub write_cost: f64,
    /// What the read tokens would have cost at the plain input rate.
    pub full_rate_cost: f64,
    /// `full_rate_cost - read_cost`: what caching saved on the read side.
    pub saved: f64,
}

impl CacheCost {
    /// Money for `cached_tokens` reads plus `cache_creation_tokens` writes under
    /// `rates`. `full_rate_cost` prices the READ tokens at the plain input rate,
    /// so `saved` answers "what did the cache save me versus sending these
    /// tokens as ordinary input".
    pub fn compute(cached_tokens: i64, cache_creation_tokens: i64, rates: &ModelRates) -> Self {
        let per_m = 1_000_000.0;
        let read_cost = (cached_tokens as f64 / per_m) * rates.cache_read_per_m;
        let write_cost = (cache_creation_tokens as f64 / per_m) * rates.cache_write_per_m;
        let full_rate_cost = (cached_tokens as f64 / per_m) * rates.input_per_m;
        CacheCost {
            read_cost,
            write_cost,
            full_rate_cost,
            saved: full_rate_cost - read_cost,
        }
    }
}

/// Remove version tokens (digit runs with `.`/`_` separators) from a model or
/// prefix and collapse leftover separators, so a new release inherits the
/// last-known price of its family/tier (#655): `qwen3.8-max-preview` ->
/// `qwen-max-preview`, `claude-opus-4-9` -> `claude-opus`, `gpt-5.3` -> `gpt`.
fn strip_version_tokens(s: &str) -> String {
    static VER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+(?:[._]\d+)*").unwrap());
    static SEP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[-._]{2,}").unwrap());
    let no_ver = VER.replace_all(s, "");
    let collapsed = SEP.replace_all(&no_ver, "-");
    collapsed
        .trim_matches(|c| c == '-' || c == '.' || c == '_')
        .to_string()
}

/// A single model pricing entry.
/// `prefix` is matched as a substring of the model name (case-insensitive).
/// First match wins, so put more specific prefixes before general ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingEntry {
    pub prefix: String,
    pub input_per_m: f64,
    pub output_per_m: f64,
    /// Cache write cost per million tokens (defaults to 1.25x input_per_m if absent)
    #[serde(default)]
    pub cache_write_per_m: Option<f64>,
    /// Cache read cost per million tokens (defaults to 0.1x input_per_m if absent)
    #[serde(default)]
    pub cache_read_per_m: Option<f64>,
}

/// Per-provider block in the TOML file.
/// TOML format: `[providers.anthropic]\nentries = [...]`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderBlock {
    #[serde(default)]
    pub entries: Vec<PricingEntry>,
}

/// The full pricing table, keyed by provider name (for display only).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PricingConfig {
    #[serde(default)]
    pub providers: HashMap<String, ProviderBlock>,
}

impl PricingConfig {
    /// Calculate cost for a model + token counts (no cache breakdown).
    /// Treats all input tokens at the regular input rate.
    pub fn calculate_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        self.calculate_cost_with_cache(model, input_tokens, output_tokens, 0, 0)
    }

    /// Calculate cost with full cache breakdown.
    /// `input_tokens` = non-cached input only.
    /// Cache write defaults to 1.25x input rate, cache read to 0.1x.
    pub fn calculate_cost_with_cache(
        &self,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cache_creation_tokens: u32,
        cache_read_tokens: u32,
    ) -> f64 {
        self.calculate_cost_with_cache_detailed(
            model,
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
        )
        .total
    }

    /// [`Self::calculate_cost_with_cache`] plus the [`CostOrigin`], so callers
    /// can flag a turn as estimated instead of table-priced (#1717).
    /// Unrecognized families bill the conservative default rates (never $0)
    /// and warn once per model.
    pub fn calculate_cost_with_cache_detailed(
        &self,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cache_creation_tokens: u32,
        cache_read_tokens: u32,
    ) -> CostBreakdown {
        match self.resolve_entry(model) {
            Some(entry) => {
                let input = (input_tokens as f64 / 1_000_000.0) * entry.input_per_m;
                let output = (output_tokens as f64 / 1_000_000.0) * entry.output_per_m;
                let cache_write_rate = entry.cache_write_per_m.unwrap_or(entry.input_per_m * 1.25);
                let cache_read_rate = entry.cache_read_per_m.unwrap_or(entry.input_per_m * 0.1);
                let cache_write = (cache_creation_tokens as f64 / 1_000_000.0) * cache_write_rate;
                let cache_read = (cache_read_tokens as f64 / 1_000_000.0) * cache_read_rate;
                CostBreakdown {
                    total: input + output + cache_write + cache_read,
                    origin: CostOrigin::Priced,
                }
            }
            None => {
                warn_unknown_model_once(model);
                let input = (input_tokens as f64 / 1_000_000.0) * DEFAULT_UNKNOWN_INPUT_PER_M;
                let output = (output_tokens as f64 / 1_000_000.0) * DEFAULT_UNKNOWN_OUTPUT_PER_M;
                let cache_write = (cache_creation_tokens as f64 / 1_000_000.0)
                    * (DEFAULT_UNKNOWN_INPUT_PER_M * 1.25);
                let cache_read =
                    (cache_read_tokens as f64 / 1_000_000.0) * (DEFAULT_UNKNOWN_INPUT_PER_M * 0.1);
                CostBreakdown {
                    total: input + output + cache_write + cache_read,
                    origin: CostOrigin::Estimated,
                }
            }
        }
    }

    /// Estimate cost from a combined token count using an 80/20 input/output
    /// split. Unrecognizable families bill the conservative default rates
    /// (#1717) instead of returning nothing; check [`Self::cost_origin`] to
    /// distinguish estimated from table-priced.
    pub fn estimate_cost(&self, model: &str, token_count: i64) -> f64 {
        match self.resolve_entry(model) {
            Some(entry) => {
                let input = (token_count as f64 * 0.80 / 1_000_000.0) * entry.input_per_m;
                let output = (token_count as f64 * 0.20 / 1_000_000.0) * entry.output_per_m;
                input + output
            }
            None => {
                warn_unknown_model_once(model);
                (token_count as f64 * 0.80 / 1_000_000.0) * DEFAULT_UNKNOWN_INPUT_PER_M
                    + (token_count as f64 * 0.20 / 1_000_000.0) * DEFAULT_UNKNOWN_OUTPUT_PER_M
            }
        }
    }

    /// Which origin [`Self::estimate_cost`] /
    /// [`Self::calculate_cost_with_cache`] would use for `model` right now:
    /// the cheap check the `/usage` dashboard uses to mark estimated rows
    /// (#1717).
    pub fn cost_origin(&self, model: &str) -> CostOrigin {
        if self.resolve_entry(model).is_some() {
            CostOrigin::Priced
        } else {
            CostOrigin::Estimated
        }
    }

    /// Resolve `model`'s rates with the documented cache defaults applied
    /// (write 1.25x input, read 0.1x input), so a caller never has to know the
    /// fallback multipliers. Unknown families bill the conservative
    /// [`DEFAULT_UNKNOWN_INPUT_PER_M`] / [`DEFAULT_UNKNOWN_OUTPUT_PER_M`] pair
    /// and are flagged [`CostOrigin::Estimated`] (#1717), same as
    /// [`Self::calculate_cost_with_cache_detailed`].
    pub fn rates_for(&self, model: &str) -> ModelRates {
        match self.resolve_entry(model) {
            Some(entry) => ModelRates {
                input_per_m: entry.input_per_m,
                output_per_m: entry.output_per_m,
                cache_write_per_m: entry.cache_write_per_m.unwrap_or(entry.input_per_m * 1.25),
                cache_read_per_m: entry.cache_read_per_m.unwrap_or(entry.input_per_m * 0.1),
                origin: CostOrigin::Priced,
            },
            None => {
                warn_unknown_model_once(model);
                ModelRates {
                    input_per_m: DEFAULT_UNKNOWN_INPUT_PER_M,
                    output_per_m: DEFAULT_UNKNOWN_OUTPUT_PER_M,
                    cache_write_per_m: DEFAULT_UNKNOWN_INPUT_PER_M * 1.25,
                    cache_read_per_m: DEFAULT_UNKNOWN_INPUT_PER_M * 0.1,
                    origin: CostOrigin::Estimated,
                }
            }
        }
    }

    /// Resolve the best pricing entry for a model, never dropping a recognizable
    /// family to no-match (#655). Three tiers, most specific first: an exact
    /// substring match; then a version-agnostic tier match, so a new release
    /// inherits the last-known price of its tier (`qwen3.8-max-preview` takes the
    /// qwen-max-preview rate, `claude-opus-4-9` the opus rate) while keeping
    /// max-vs-plus apart; then a family-root fallback to that family's flagship
    /// (most expensive) entry. Returns `None` only when no known family shares
    /// the model's root.
    fn resolve_entry(&self, model: &str) -> Option<&PricingEntry> {
        let m = model.to_lowercase();
        if let Some(e) = self.best_substring_match(&m, false) {
            return Some(e);
        }
        if let Some(e) = self.best_substring_match(&m, true) {
            return Some(e);
        }
        self.family_flagship(&strip_version_tokens(&m))
    }

    /// Best entry matching `model`, both sides version-stripped when
    /// `versionless`. Matches in either direction: the model carrying the prefix
    /// (`claude-opus-4-9` contains `claude-opus-4`), or the prefix carrying a
    /// vendor-stripped short model name (`claude-opus-4` contains `opus-4`).
    /// Longest overlap wins (most specific); ties break to the higher input rate
    /// (family flagship). Overlaps under [`MIN_MATCH_OVERLAP`] are ignored.
    fn best_substring_match(&self, model: &str, versionless: bool) -> Option<&PricingEntry> {
        let m = if versionless {
            strip_version_tokens(model)
        } else {
            model.to_string()
        };
        if m.is_empty() {
            return None;
        }
        self.providers
            .values()
            .flat_map(|b| b.entries.iter())
            .filter_map(|e| {
                let prefix = e.prefix.to_lowercase();
                let key = if versionless {
                    strip_version_tokens(&prefix)
                } else {
                    prefix
                };
                if key.is_empty() {
                    return None;
                }
                let overlap = if m.contains(&key) {
                    key.len()
                } else if key.contains(&m) {
                    m.len()
                } else {
                    return None;
                };
                (overlap >= MIN_MATCH_OVERLAP).then_some((overlap, e))
            })
            .max_by(|(la, ea), (lb, eb)| {
                la.cmp(lb).then(
                    ea.input_per_m
                        .partial_cmp(&eb.input_per_m)
                        .unwrap_or(Ordering::Equal),
                )
            })
            .map(|(_, e)| e)
    }

    /// Flagship (most expensive) entry whose version-stripped prefix shares its
    /// first token with `model_base`. The last-resort "never $0" price for a new
    /// tier in a known family (e.g. an unforeseen `qwen-*` variant).
    fn family_flagship(&self, model_base: &str) -> Option<&PricingEntry> {
        let root = model_base.split(['-', '.']).next().unwrap_or("");
        if root.len() < 3 {
            return None;
        }
        self.providers
            .values()
            .flat_map(|b| b.entries.iter())
            .filter(|e| {
                strip_version_tokens(&e.prefix.to_lowercase())
                    .split(['-', '.'])
                    .next()
                    .is_some_and(|t| t == root)
            })
            .max_by(|a, b| {
                a.input_per_m
                    .partial_cmp(&b.input_per_m)
                    .unwrap_or(Ordering::Equal)
            })
    }

    /// Load from ~/.opencrabs/usage_pricing.toml.
    /// Supports both the current schema (`[providers.X] entries = [...]`) and the
    /// legacy on-disk schema (`[[usage.pricing.X]]` array-of-tables).
    /// Falls back to the embedded example file if the user file doesn't exist
    /// (useful for tests and CI environments).
    /// Returns an error only if both the user file and embedded example fail to parse.
    ///
    /// **Additive baseline merge:** after parsing the user file, the
    /// embedded example is parsed too and any `(provider, prefix)` pair
    /// present in the baseline but missing from the user file is
    /// appended. The merged file is written back to disk so subsequent
    /// loads see the new entries without re-running the merge. This is
    /// how MiniMax-M3 / future model releases reach users who already
    /// have a `usage_pricing.toml` from an earlier install — without
    /// this, the seed-on-missing pattern leaves their pricing table
    /// frozen at whatever shipped on their first install.
    ///
    /// The merge is strictly additive: user customisations (renamed
    /// prefixes, hand-tweaked rates) are NEVER overwritten. Only
    /// prefixes the user doesn't have at all get appended.
    pub fn load() -> Result<Self, String> {
        let path = crate::config::opencrabs_home().join("usage_pricing.toml");
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                // Fall back to embedded example (tests, CI, fresh installs)
                let example = include_str!("../../usage_pricing.toml.example");
                return Self::parse_content(example, "embedded example");
            }
        };
        let mut user_cfg = Self::parse_content(&content, &format!("{:?}", path))?;
        // Additive merge with embedded baseline. Skip on parse failure
        // of the baseline — we never want a bad bundled example to
        // mask a working user file.
        let baseline_example = include_str!("../../usage_pricing.toml.example");
        if let Ok(baseline_cfg) = Self::parse_content(baseline_example, "embedded example baseline")
        {
            let added = user_cfg.merge_missing_from(&baseline_cfg);
            if added > 0 {
                tracing::info!(
                    target: "pricing",
                    added,
                    path = ?path,
                    "merged new baseline pricing entries into user pricing file (additive only)"
                );
                let new_content = Self::serialize_to_toml(&user_cfg);
                if let Err(e) = std::fs::write(&path, &new_content) {
                    tracing::warn!(
                        target: "pricing",
                        error = %e,
                        path = ?path,
                        "failed to persist merged pricing back to disk — entries are in-memory only \
                         this session, will re-merge on next load"
                    );
                }
            }
        }
        Ok(user_cfg)
    }

    /// Append any `(provider, prefix)` from `other` that does not exist
    /// in `self`. Returns the number of entries appended. Idempotent:
    /// running twice with the same baseline adds zero on the second
    /// call. Matching is case-insensitive on the `prefix` field —
    /// `minimax-m3` and `MiniMax-M3` count as the same entry.
    pub fn merge_missing_from(&mut self, other: &PricingConfig) -> usize {
        let mut added = 0;
        for (prov_name, baseline_block) in &other.providers {
            let user_block = self.providers.entry(prov_name.clone()).or_default();
            for baseline_entry in &baseline_block.entries {
                let needle = baseline_entry.prefix.to_lowercase();
                if !user_block
                    .entries
                    .iter()
                    .any(|e| e.prefix.to_lowercase() == needle)
                {
                    user_block.entries.push(baseline_entry.clone());
                    added += 1;
                }
            }
        }
        added
    }

    /// Parse TOML content with both schema variants.
    fn parse_content(content: &str, source: &str) -> Result<Self, String> {
        // Try current schema first.
        if let Ok(cfg) = toml::from_str::<PricingConfig>(content)
            && !cfg.providers.is_empty()
        {
            return Ok(cfg);
        }

        // Try legacy schema: [[usage.pricing.<provider>]] entries
        if let Ok(cfg) = Self::load_legacy(content)
            && !cfg.providers.is_empty()
        {
            tracing::warn!(
                "usage_pricing.toml uses old format — please update it to the new schema. \
                 See usage_pricing.toml.example in the repo"
            );
            let new_content = Self::serialize_to_toml(&cfg);
            // Only write back if source is a real file path (not embedded example)
            if source != "embedded example" {
                let path = crate::config::opencrabs_home().join("usage_pricing.toml");
                let _ = std::fs::write(&path, new_content);
            }
            return Ok(cfg);
        }

        Err(format!(
            "usage_pricing.toml from {} failed to parse with both schemas.\n\
             Check the file syntax or re-copy from usage_pricing.toml.example",
            source
        ))
    }

    /// Parse the legacy `[[usage.pricing.<provider>]]` format.
    fn load_legacy(content: &str) -> Result<Self, toml::de::Error> {
        #[derive(serde::Deserialize)]
        struct LegacyRoot {
            usage: Option<LegacyUsage>,
        }
        #[derive(serde::Deserialize)]
        struct LegacyUsage {
            pricing: Option<toml::Value>,
        }

        let root: LegacyRoot = toml::from_str(content)?;
        let pricing_val = root
            .usage
            .and_then(|u| u.pricing)
            .unwrap_or(toml::Value::Table(toml::map::Map::new()));

        let mut providers: HashMap<String, ProviderBlock> = HashMap::new();
        if let toml::Value::Table(table) = pricing_val {
            for (provider_name, entries_val) in table {
                if let toml::Value::Array(arr) = entries_val {
                    let entries: Vec<PricingEntry> =
                        arr.into_iter().filter_map(|v| v.try_into().ok()).collect();
                    if !entries.is_empty() {
                        providers.insert(provider_name, ProviderBlock { entries });
                    }
                }
            }
        }

        Ok(PricingConfig { providers })
    }

    /// Serialize a PricingConfig back to the canonical TOML schema.
    fn serialize_to_toml(cfg: &PricingConfig) -> String {
        let mut out = String::from(
            "# OpenCrabs Usage Pricing — auto-migrated to current schema.\n\
             # Edit freely. Changes take effect immediately on next /usage open.\n\
             # prefix is matched case-insensitively as a substring of the model name.\n\
             # Costs are per 1 million tokens (USD).\n\n",
        );
        let mut providers: Vec<(&String, &ProviderBlock)> = cfg.providers.iter().collect();
        providers.sort_by_key(|(k, _)| k.as_str());
        for (name, block) in providers {
            out.push_str(&format!("[providers.{}]\nentries = [\n", name));
            for e in &block.entries {
                let mut row = format!(
                    "  {{ prefix = {:?}, input_per_m = {}, output_per_m = {}",
                    e.prefix, e.input_per_m, e.output_per_m
                );
                // Preserve optional cache rates — without this the
                // additive merge would silently drop MiniMax-M3's
                // cache_read_per_m on the first write-back, leaving
                // the user paying the default 0.1x input fallback
                // instead of the real $0.12/M list rate.
                if let Some(cw) = e.cache_write_per_m {
                    row.push_str(&format!(", cache_write_per_m = {cw}"));
                }
                if let Some(cr) = e.cache_read_per_m {
                    row.push_str(&format!(", cache_read_per_m = {cr}"));
                }
                row.push_str(" },\n");
                out.push_str(&row);
            }
            out.push_str("]\n\n");
        }
        out
    }

    /// Copy `usage_pricing.toml.example` to brain directory on first run only.
    /// Existing users: see release notes for instructions to diff and update their file.
    pub fn seed_from_example() {
        let path = crate::config::opencrabs_home().join("usage_pricing.toml");

        if path.exists() {
            return; // User owns this file. Never overwrite.
        }

        let example_content = include_str!("../../usage_pricing.toml.example");
        if let Err(e) = std::fs::write(&path, example_content) {
            tracing::error!("Failed to seed usage_pricing.toml from example: {}", e);
        } else {
            tracing::info!("Seeded usage_pricing.toml from example");
        }
    }
}
