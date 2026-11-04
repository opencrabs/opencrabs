//! Usage dashboard data layer
//!
//! All data structs, DB aggregation queries, activity classifier, and period enum.
//! Queries hit the DB pool directly — no dependency on repository structs.

use crate::db::{Pool, interact_err};
use crate::usage::pricing::{CacheCost, CostOrigin, PricingConfig};
use anyhow::{Context, Result};
use rusqlite::params;

// ── Period filter ────────────────────────────────────────────────────────────

/// Time period filter for dashboard cards
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Today,
    Week,
    Month,
    AllTime,
}

impl Period {
    /// Cycle to the next period (wraps around)
    pub fn next(self) -> Self {
        match self {
            Self::Today => Self::Week,
            Self::Week => Self::Month,
            Self::Month => Self::AllTime,
            Self::AllTime => Self::Today,
        }
    }

    /// Returns the epoch timestamp for the start of this period, or None for AllTime
    pub fn since_epoch(self) -> Option<i64> {
        let now = chrono::Utc::now().timestamp();
        match self {
            Self::Today => Some(now - 86_400),
            Self::Week => Some(now - 7 * 86_400),
            Self::Month => Some(now - 30 * 86_400),
            Self::AllTime => None,
        }
    }

    /// Short label for the status bar
    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Week => "Week",
            Self::Month => "Month",
            Self::AllTime => "All Time",
        }
    }
}

// ── Data structs ─────────────────────────────────────────────────────────────

/// Summary bar: total tokens, total cost, session count
#[derive(Debug, Clone, Default)]
pub struct SummaryStats {
    pub total_tokens: i64,
    pub total_cost: f64,
    pub session_count: i64,
    pub call_count: i64,
}

/// Daily usage for the sparkline / bar chart
#[derive(Debug, Clone)]
pub struct DailyStats {
    pub date: String,
    pub tokens: i64,
    pub cost: f64,
    pub calls: i64,
}

/// Per-project (working_directory) usage
#[derive(Debug, Clone)]
pub struct ProjectStats {
    pub project: String,
    pub cost: f64,
    pub tokens: i64,
    pub sessions: i64,
}

/// Per-model usage (flat, kept for backward compat)
#[derive(Debug, Clone)]
pub struct ModelStats {
    pub model: String,
    pub tokens: i64,
    pub cost: f64,
    pub calls: i64,
    pub estimated: bool,
}

/// Grouped model usage with quantization variants
#[derive(Debug, Clone, Default)]
pub struct ModelEntry {
    pub model: String, // base model name (e.g. "qwen3.6-35b-a3b")
    pub tokens: i64,
    pub cost: f64,
    pub calls: i64,
    pub estimated: bool,
    /// Cache-read tokens the ledger recorded for this model, or `None` when
    /// none of its rows carried a split (#2027).
    pub cache_read_tokens: Option<i64>,
    /// Token total of the ledger rows that DID record a split. The cache share
    /// divides by this, so rows written before the cache-read column existed
    /// cannot drag the percentage toward zero.
    pub cache_split_tokens: i64,
    pub variants: Vec<ModelVariant>,
}

impl ModelEntry {
    /// Share (0-100) of the recorded tokens that were served from cache, or
    /// `None` when the ledger holds no split for this model. `None` renders
    /// blank, which is a different statement from a measured 0% (#2027).
    pub fn cache_read_pct(&self) -> Option<f64> {
        let cached = self.cache_read_tokens?;
        if self.cache_split_tokens <= 0 {
            return None;
        }
        Some(cached as f64 / self.cache_split_tokens as f64 * 100.0)
    }
}

/// A single quantization variant under a grouped model
#[derive(Debug, Clone)]
pub struct ModelVariant {
    pub name: String, // full name (e.g. "qwen3.6-35b-a3b-gguf")
    pub tokens: i64,
    pub cost: f64,
    pub calls: i64,
}

/// Tool usage count
#[derive(Debug, Clone)]
pub struct ToolStats {
    pub tool_name: String,
    pub call_count: i64,
}

/// Activity category usage
#[derive(Debug, Clone)]
pub struct ActivityStats {
    pub category: String,
    pub cost: f64,
    pub turns: i64,
    pub one_shot_pct: f64,
}

/// Cache efficiency statistics
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Percentage of input tokens served from cache (0-100)
    pub cache_hit_pct: f64,
    /// Total tokens served from cache
    pub cached_tokens: i64,
    /// Total input tokens (cached + non-cached)
    pub total_input_tokens: i64,
    /// Per-model cache efficiency and cache money, highest hit-rate first.
    pub per_model: Vec<ModelCacheStat>,
}

impl CacheStats {
    /// Sum the per-model money for the TOTAL line of the cache breakdown
    /// (#2027). Every row prices the same quantity (that model's cached read
    /// and write tokens), so the components add up field by field.
    pub fn cost_totals(&self) -> CacheCost {
        self.per_model
            .iter()
            .fold(CacheCost::default(), |acc, m| CacheCost {
                read_cost: acc.read_cost + m.cost.read_cost,
                write_cost: acc.write_cost + m.cost.write_cost,
                full_rate_cost: acc.full_rate_cost + m.cost.full_rate_cost,
                saved: acc.saved + m.cost.saved,
            })
    }
}

/// Cache efficiency and cache money for a single model (caching-capable
/// requests only).
#[derive(Debug, Clone)]
pub struct ModelCacheStat {
    pub model: String,
    pub cache_hit_pct: f64,
    pub cached_tokens: i64,
    /// Cache-WRITE tokens the provider reported for this model.
    pub cache_creation_tokens: i64,
    /// What the cached reads cost, what the writes cost, what the reads would
    /// have cost at the plain input rate, and the difference (#2027).
    pub cost: CacheCost,
    /// The model's plain input rate, so the overlay can price the counterfactual.
    pub input_per_m: f64,
    /// The model's cache-read rate, shown next to the read tokens.
    pub cache_read_per_m: f64,
    /// True when no pricing entry matched, so the money is the conservative
    /// default estimate rather than a table price (#1717).
    pub estimated: bool,
}

/// All dashboard data, fetched once per period change
#[derive(Debug, Clone, Default)]
pub struct DashboardData {
    pub summary: SummaryStats,
    pub daily: Vec<DailyStats>,
    pub projects: Vec<ProjectStats>,
    pub models: Vec<ModelEntry>,
    pub tools: Vec<ToolStats>,
    pub activities: Vec<ActivityStats>,
    pub cache: Option<CacheStats>,
}

/// Normalize a model name for grouping by stripping quantization suffixes.
///
/// Handles GGUF-style quant tags like `-gguf`, `-oq2`, `-oq4`, `-q4_k_m`,
/// `-iq4_xs`, `-ud-iq4_xs`, `-ud-oq2`, etc.
///
/// Also strips a trailing `.gguf` file extension first, so names that
/// arrive as raw llama.cpp filenames (e.g. `qwen3.6-35b-a3b-ud-iq4_xs.gguf`)
/// still match the quant patterns and group under the same parent as their
/// non-`.gguf` siblings. Without this, the `.gguf`-suffixed variant
/// rendered as its own top-level row in the /usage dashboard instead of
/// folding under the base model.
///
/// Examples:
/// - `qwen3.6-35b-a3b-gguf` → `qwen3.6-35b-a3b`
/// - `qwen3.6-35b-a3b-ud-iq4_xs.gguf` → `qwen3.6-35b-a3b`
/// - `qwen3.6-35b-a3b-ud-oq2` → `qwen3.6-35b-a3b`
/// - `qwen3.6-35b-a3b-q4_k_m` → `qwen3.6-35b-a3b`
/// - `qwen3.6-35b-a3b` → `qwen3.6-35b-a3b` (no-op)
pub fn normalize_model_for_grouping(name: &str) -> String {
    let mut n = name.to_string();
    // Strip trailing `.gguf` file extension before pattern matching. Local
    // llama.cpp setups frequently report the on-disk filename verbatim.
    if let Some(stripped) = n.strip_suffix(".gguf") {
        n = stripped.to_string();
    }
    // Strip common GGUF quantization suffixes (order matters: longer first)
    let quant_patterns = [
        "-ud-iq4_xs",
        "-ud-oq2",
        "-ud-oq4",
        "-oq2",
        "-oq4",
        "-oq8",
        "-iq4_xs",
        "-q2_k",
        "-q3_k_s",
        "-q3_k_m",
        "-q3_k_l",
        "-q4_0",
        "-q4_1",
        "-q4_k_m",
        "-q4_k_s",
        "-q4_k",
        "-q5_0",
        "-q5_1",
        "-q5_k_m",
        "-q5_k_s",
        "-q6_k",
        "-q8_0",
        "-q8_1",
        "-gguf",
    ];
    for pattern in &quant_patterns {
        if n.ends_with(pattern) {
            n.truncate(n.len() - pattern.len());
            break; // only strip once
        }
    }
    n
}

/// True when a raw model name is a trivial cosmetic alias of its
/// parent group — same model written with different separator
/// punctuation, case, etc. Examples that return true: `qwen3.7-max`
/// vs `qwen-3.7-max`; `Qwen 3.7 Max` vs `qwen-3.7-max`.
///
/// Used by the usage breakdown to suppress noise: when a normalized
/// group like `qwen-3.7-max` contains a child whose name is just a
/// cosmetic alias of the group, there's no information value in
/// showing it — same model reported twice with different
/// punctuation. Meaningful variants (`qwen-3.7-max-preview`, dated
/// snapshots, `qwen-latest-series-invite-beta-v34`) have different
/// canonical forms and survive this check.
///
/// Canonical form: alphanumeric ASCII only, lowercased.
pub fn is_cosmetic_alias_of_parent(variant: &str, parent: &str) -> bool {
    canonical_model_key(variant) == canonical_model_key(parent)
}

/// Punctuation- and case-insensitive identity for a model name: ASCII
/// alphanumerics only, lowercased.
///
/// Grouping keyed on the raw name split one model across two rows whenever it
/// was reported with different punctuation (`qwen3.8-max-preview` alongside
/// `qwen-3.8-max-preview`), so its spend was reported as two smaller models
/// (#929). Neither was ever the other's child, so `is_cosmetic_alias_of_parent`
/// never ran on them.
///
/// Only punctuation and case collapse. A preview suffix, a dated snapshot or a
/// different version number all differ in alphanumerics, so genuinely distinct
/// models stay distinct.
pub fn canonical_model_key(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

// ── Activity classifier ──────────────────────────────────────────────────────

/// Classify a session title into an activity category using keyword heuristics.
pub fn classify_activity(title: &str) -> &'static str {
    let t = title.to_lowercase();

    if t.contains("ci") || t.contains("deploy") || t.contains("release") || t.contains("workflow") {
        return "CI/Deploy";
    }
    if t.contains("bug") || t.contains("fix") || t.contains("error") || t.contains("crash") {
        return "Bug Fixes";
    }
    if t.contains("refactor") || t.contains("cleanup") || t.contains("clean up") {
        return "Refactoring";
    }
    if t.contains("test") || t.contains("spec") || t.contains("coverage") {
        return "Testing";
    }
    if t.contains("doc") || t.contains("readme") || t.contains("changelog") {
        return "Documentation";
    }
    if t.contains("feat") || t.contains("add") || t.contains("new") || t.contains("implement") {
        return "Features";
    }
    if t.contains("config") || t.contains("setup") || t.contains("setting") {
        return "Config";
    }
    "Development"
}

// ── Data fetching ────────────────────────────────────────────────────────────

impl DashboardData {
    /// Fetch all dashboard data for a given period.
    pub async fn fetch(pool: &Pool, period: Period) -> Result<Self> {
        let since = period.since_epoch();

        // Run all queries concurrently
        let (mut summary, daily, projects, models, tools, activities, cache) = tokio::try_join!(
            fetch_summary(pool, since),
            fetch_daily(pool, since),
            fetch_projects(pool, since),
            fetch_model_entries(pool, since),
            fetch_tools(pool, since),
            fetch_activities(pool, since),
            fetch_cache_stats(pool, since),
        )?;

        // Header total is the sum of the model rows, so it agrees with the
        // By Model card. Those rows sum the ledger's per-call cost, so this is
        // the real figure, not a second cache-blind estimate.
        summary.total_cost = models.iter().map(|m| m.cost).sum();

        Ok(Self {
            summary,
            daily,
            projects,
            models,
            tools,
            activities,
            cache,
        })
    }
}

async fn fetch_summary(pool: &Pool, since: Option<i64>) -> Result<SummaryStats> {
    let conn = pool.get().await.context("pool")?;
    conn.interact(move |conn| {
        let (query, param): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(s) = since {
            (
                "SELECT COALESCE(SUM(token_count), 0), COALESCE(SUM(cost), 0.0), \
                 COUNT(DISTINCT session_id), COUNT(*) \
                 FROM usage_ledger WHERE created_at >= ?1",
                vec![Box::new(s)],
            )
        } else {
            (
                "SELECT COALESCE(SUM(token_count), 0), COALESCE(SUM(cost), 0.0), \
                 COUNT(DISTINCT session_id), COUNT(*) \
                 FROM usage_ledger",
                vec![],
            )
        };
        let refs: Vec<&dyn rusqlite::types::ToSql> = param.iter().map(|p| p.as_ref()).collect();
        conn.query_row(query, refs.as_slice(), |row| {
            Ok(SummaryStats {
                total_tokens: row.get(0)?,
                total_cost: row.get(1)?,
                session_count: row.get(2)?,
                call_count: row.get(3)?,
            })
        })
    })
    .await
    .map_err(interact_err)?
    .context("Failed to fetch summary stats")
}

async fn fetch_daily(pool: &Pool, since: Option<i64>) -> Result<Vec<DailyStats>> {
    let conn = pool.get().await.context("pool")?;
    conn.interact(move |conn| {
        let (query, param): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(s) = since {
            (
                "SELECT date(created_at, 'unixepoch') AS day, \
                 COALESCE(SUM(token_count), 0), COALESCE(SUM(cost), 0.0), COUNT(*) \
                 FROM usage_ledger WHERE created_at >= ?1 \
                 GROUP BY day ORDER BY day ASC",
                vec![Box::new(s)],
            )
        } else {
            (
                "SELECT date(created_at, 'unixepoch') AS day, \
                 COALESCE(SUM(token_count), 0), COALESCE(SUM(cost), 0.0), COUNT(*) \
                 FROM usage_ledger GROUP BY day ORDER BY day ASC",
                vec![],
            )
        };
        let refs: Vec<&dyn rusqlite::types::ToSql> = param.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(query)?;
        let rows = stmt.query_map(refs.as_slice(), |row| {
            Ok(DailyStats {
                date: row.get(0)?,
                tokens: row.get(1)?,
                cost: row.get(2)?,
                calls: row.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
    })
    .await
    .map_err(interact_err)?
    .context("Failed to fetch daily stats")
}

/// Map raw working_directory to a display-friendly project name.
/// User home directory (no project) typically means brain-file editing.
fn normalize_project_name(raw: &str) -> String {
    if raw == "unknown" || raw.is_empty() {
        return "unknown".to_string();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() && raw.trim_end_matches('/') == home.trim_end_matches('/') {
        return "brain-files".to_string();
    }
    raw.rsplit('/').next().unwrap_or(raw).to_string()
}

/// Merge rows that map to the same display name by summing stats.
fn merge_project_stats(stats: &mut Vec<ProjectStats>) {
    let mut map = std::collections::HashMap::<String, ProjectStats>::new();
    for s in stats.drain(..) {
        map.entry(s.project.clone())
            .and_modify(|e| {
                e.cost += s.cost;
                e.tokens += s.tokens;
                e.sessions += s.sessions;
            })
            .or_insert(s);
    }
    *stats = map.into_values().collect();
    stats.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

async fn fetch_projects(pool: &Pool, since: Option<i64>) -> Result<Vec<ProjectStats>> {
    let conn = pool.get().await.context("pool")?;
    conn.interact(move |conn| -> rusqlite::Result<Vec<ProjectStats>> {
        let (query, param): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(s) = since {
            (
                "SELECT COALESCE(s.working_directory, 'unknown'), \
                 COALESCE(SUM(u.cost), 0.0), COALESCE(SUM(u.token_count), 0), \
                 COUNT(DISTINCT u.session_id) \
                 FROM usage_ledger u \
                 LEFT JOIN sessions s ON u.session_id = s.id \
                 WHERE u.created_at >= ?1 \
                 GROUP BY s.working_directory \
                 ORDER BY SUM(u.cost) DESC",
                vec![Box::new(s)],
            )
        } else {
            (
                "SELECT COALESCE(s.working_directory, 'unknown'), \
                 COALESCE(SUM(u.cost), 0.0), COALESCE(SUM(u.token_count), 0), \
                 COUNT(DISTINCT u.session_id) \
                 FROM usage_ledger u \
                 LEFT JOIN sessions s ON u.session_id = s.id \
                 GROUP BY s.working_directory \
                 ORDER BY SUM(u.cost) DESC",
                vec![],
            )
        };
        let refs: Vec<&dyn rusqlite::types::ToSql> = param.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(query)?;
        let rows = stmt.query_map(refs.as_slice(), |row| {
            let raw: String = row.get(0)?;
            let project = normalize_project_name(&raw);
            Ok(ProjectStats {
                project,
                cost: row.get(1)?,
                tokens: row.get(2)?,
                sessions: row.get(3)?,
            })
        })?;
        let mut stats: Vec<ProjectStats> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        merge_project_stats(&mut stats);
        Ok(stats)
    })
    .await
    .map_err(interact_err)?
    .context("Failed to fetch project stats")
}

/// Normalize model name the same way the SQL query does, so we can
/// apply the grouping step in Rust after the SQL normalization.
///
/// MUST stay byte-for-byte equivalent to the SQL CTE chain inside
/// `fetch_model_entries` — anything different and grouped rows split.
#[cfg(test)]
pub(crate) fn sql_normalize_model(raw: &str) -> String {
    let m1 = if raw.contains('/') {
        raw.rsplit('/').next().unwrap_or(raw).to_lowercase()
    } else {
        raw.to_lowercase()
    };
    let m2 = if m1.ends_with(":free") || m1.ends_with("-free") {
        &m1[..m1.len() - 5]
    } else if m1.ends_with("-thinking") {
        &m1[..m1.len() - 9]
    } else {
        m1.as_str()
    };
    // Strip provider-namespace prefix separated by `:`. Examples:
    //   `dialagram:qwen-3.6-max-preview-thinking` → `qwen-3.6-max-preview`
    //     (after the `-thinking` strip above runs first)
    //   `customprovider-qwen:qwen3.6-plus` → `qwen3.6-plus`
    // Splits on the FIRST colon so a hypothetical `provider:family:variant`
    // still keeps `family:variant` intact. `:free` suffix was already
    // stripped above before reaching this step.
    let m2 = m2.split_once(':').map(|(_, after)| after).unwrap_or(m2);
    let m3 = m2.strip_prefix("claude-").unwrap_or(m2);
    // Normalize display-name variants with spaces to kebab-case so that
    // "Qwen 3.7 Max", "Qwen 3.6 Max Preview", "Qwen 3.5 Plus" etc. fold
    // into the same stream as their CLI cousins. Lowercasing already
    // happened above; this just swaps spaces for dashes.
    let m3_owned = m3.replace(' ', "-");
    let m3 = m3_owned.as_str();
    match m3 {
        "opus" | "opus-4-6" => "opus-4-6".to_string(),
        "sonnet" | "sonnet-4-6" => "sonnet-4-6".to_string(),
        "haiku" | "haiku-4-5" | "haiku-4-5-20251001" => "haiku-4-5".to_string(),
        // Qwen 3.7 Max family: consolidate every surface form (dash /
        // no-dash after `qwen`, preview channel, dated snapshot,
        // `latest-series` aliases, display-name with spaces) into one
        // canonical kebab-case bucket.
        "qwen-3.7-max"
        | "qwen3.7-max"
        | "qwen-3-7-max"
        | "qwen3-7-max"
        | "qwen-3.7-max-preview"
        | "qwen3.7-max-preview"
        | "qwen-3.7-max-20260520"
        | "qwen3.7-max-20260520"
        | "qwen-latest-series"
        | "qwen-latest-series-invite" => "qwen-3.7-max".to_string(),
        // Qwen 3.7 Plus family.
        "qwen-3.7-plus" | "qwen3.7-plus" | "qwen-3.7-plus-preview" | "qwen3.7-plus-preview" => {
            "qwen-3.7-plus".to_string()
        }
        "qwen-3.6-max-preview"
        | "qwen3.6-max-preview"
        | "qwen-3-6-max-preview"
        | "qwen3-6-max-preview"
        | "qwen-max-preview" => "qwen-3.6-max-preview".to_string(),
        "coder-model" | "qwen3.6-plus" | "qwen-3.6-plus" => "qwen-3.6-plus".to_string(),
        "qwen3.5-plus" | "qwen-3.5-plus" => "qwen-3.5-plus".to_string(),
        "minimax-m2.5" => "minimax-m2.5".to_string(),
        "minimax-m2.7" => "minimax-m2.7".to_string(),
        "mimo-v2-omni" | "mimo-v2-omni-free" => "mimo-v2-omni".to_string(),
        "mimo-v2-pro" | "mimo-v2-pro-free" => "mimo-v2-pro".to_string(),
        "kimi-k2.5" | "kimi-k2-5" | "kimi-k2.6" | "kimi-k2-6" | "kimik2.6" => {
            "kimi-k2.6".to_string()
        }
        "glm-5.1" | "glm-5-1" | "glm-5" => "glm-5.1".to_string(),
        "glm-5-turbo" | "zhipu" | "zai" => "glm-5-turbo".to_string(),
        _ => m3.to_string(),
    }
}

/// One ledger row as the By Model query returns it, before grouping.
struct RawModelRow {
    /// The SQL-normalized model name, the grouping key's input.
    normalized: String,
    /// The spelling the row was recorded under, kept for display.
    raw: String,
    tokens: i64,
    calls: i64,
    cost: f64,
    /// Cache-read tokens this row recorded. `None` when the row predates the
    /// ledger's cache-read column, i.e. it carries no split at all (#2027).
    cache_read: Option<i64>,
    /// This row's tokens, counted only when the row recorded a split. The
    /// cache share divides by the sum of these, so rows with no split cannot
    /// drag the percentage toward zero.
    split_tokens: i64,
}

async fn fetch_model_entries(pool: &Pool, since: Option<i64>) -> Result<Vec<ModelEntry>> {
    let pricing = crate::usage::pricing::PricingConfig::load().ok();

    let conn = pool.get().await.context("pool")?;
    let raw = conn.interact(move |conn| {
        let base_where = if since.is_some() {
            "WHERE model != '' AND created_at >= ?1"
        } else {
            "WHERE model != ''"
        };
        // Fetch raw model stats (grouped by SQL-normalized model, not quant-normalized)
        let query = format!(
            "WITH stripped AS ( \
               SELECT *, \
                 LOWER(CASE WHEN model LIKE '%/%' \
                   THEN SUBSTR(model, INSTR(model, '/') + 1) \
                   ELSE model \
                 END) AS m1 \
               FROM usage_ledger {base_where} \
             ), \
             cleaned AS ( \
               SELECT *, \
                 CASE \
                   WHEN m1 LIKE '%:free' THEN SUBSTR(m1, 1, LENGTH(m1) - 5) \
                   WHEN m1 LIKE '%-free' THEN SUBSTR(m1, 1, LENGTH(m1) - 5) \
                   WHEN m1 LIKE '%-thinking' THEN SUBSTR(m1, 1, LENGTH(m1) - 9) \
                   ELSE m1 \
                 END AS m2_pre \
               FROM stripped \
             ), \
             namespaced AS ( \
               SELECT *, \
                 CASE WHEN INSTR(m2_pre, ':') > 0 \
                   THEN SUBSTR(m2_pre, INSTR(m2_pre, ':') + 1) \
                   ELSE m2_pre \
                 END AS m2 \
               FROM cleaned \
             ), \
             prefixed AS ( \
               SELECT *, \
                 CASE WHEN m2 LIKE 'claude-%' THEN SUBSTR(m2, 8) ELSE m2 END AS m3_pre \
               FROM namespaced \
             ), \
             spaced AS ( \
               SELECT *, REPLACE(m3_pre, ' ', '-') AS m3 FROM prefixed \
             ) \
             SELECT \
               CASE \
                 WHEN m3 IN ('opus', 'opus-4-6') THEN 'opus-4-6' \
                 WHEN m3 IN ('sonnet', 'sonnet-4-6') THEN 'sonnet-4-6' \
                 WHEN m3 IN ('haiku', 'haiku-4-5', 'haiku-4-5-20251001') THEN 'haiku-4-5' \
                 WHEN m3 IN ('qwen-3.7-max', 'qwen3.7-max', 'qwen-3-7-max', 'qwen3-7-max', 'qwen-3.7-max-preview', 'qwen3.7-max-preview', 'qwen-3.7-max-20260520', 'qwen3.7-max-20260520', 'qwen-latest-series', 'qwen-latest-series-invite', 'qwen-latest-series-invite-beta-v34') THEN 'qwen-3.7-max' \
                 WHEN m3 IN ('qwen-3.7-plus', 'qwen3.7-plus', 'qwen-3.7-plus-preview', 'qwen3.7-plus-preview') THEN 'qwen-3.7-plus' \
                 WHEN m3 IN ('qwen-3.6-max-preview', 'qwen3.6-max-preview', 'qwen-3-6-max-preview', 'qwen3-6-max-preview', 'qwen-max-preview') THEN 'qwen-3.6-max-preview' \
                 WHEN m3 IN ('coder-model', 'qwen3.6-plus', 'qwen-3.6-plus') THEN 'qwen-3.6-plus' \
                 WHEN m3 IN ('qwen3.5-plus', 'qwen-3.5-plus') THEN 'qwen-3.5-plus' \
                 WHEN m3 IN ('minimax-m2.5') THEN 'minimax-m2.5' \
                 WHEN m3 IN ('minimax-m2.7') THEN 'minimax-m2.7' \
                 WHEN m3 IN ('mimo-v2-omni', 'mimo-v2-omni-free') THEN 'mimo-v2-omni' \
                 WHEN m3 IN ('mimo-v2-pro', 'mimo-v2-pro-free') THEN 'mimo-v2-pro' \
                 WHEN m3 IN ('kimi-k2.5', 'kimi-k2-5', 'kimi-k2.6', 'kimi-k2-6', 'kimik2.6') THEN 'kimi-k2.6' \
                 WHEN m3 IN ('glm-5.1', 'glm-5-1', 'glm-5') THEN 'glm-5.1' \
                 WHEN m3 IN ('glm-5-turbo', 'zhipu') THEN 'glm-5-turbo' \
                 ELSE m3 \
               END AS normalized_model, \
               m3 AS raw_model, \
               COALESCE(SUM(token_count), 0), \
               COUNT(*), \
               COALESCE(SUM(cost), 0.0), \
               SUM(CASE WHEN prompt_tokens IS NOT NULL THEN cache_read_tokens END), \
               COALESCE(SUM(CASE WHEN prompt_tokens IS NOT NULL THEN prompt_tokens ELSE 0 END), 0) \
             FROM spaced \
             GROUP BY normalized_model, m3 \
             ORDER BY SUM(token_count) DESC"
        );
        let mut stmt = conn.prepare(&query)?;
        // `SUM(cache_read_tokens)` is NULL for a model whose rows all predate
        // the cache-read column, which is the honest answer: no split was
        // recorded. It is 0 only when a row really recorded zero cache reads.
        // The last column re-counts the tokens of the rows that DID record a
        // split, so the share is measured against what was actually reported.
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<RawModelRow> {
            Ok(RawModelRow {
                normalized: row.get(0)?,
                raw: row.get(1)?,
                tokens: row.get(2)?,
                calls: row.get(3)?,
                cost: row.get(4)?,
                cache_read: row.get(5)?,
                split_tokens: row.get(6)?,
            })
        };
        let rows: Vec<RawModelRow> = if let Some(s) = since {
            stmt.query_map(params![s], map_row)?.collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], map_row)?.collect::<std::result::Result<Vec<_>, _>>()?
        };
        Ok::<_, rusqlite::Error>(rows)
    })
    .await
    .map_err(interact_err)?
    .context("Failed to fetch model stats")?;

    // Group by base model (normalize away quantization suffixes), keyed on the
    // punctuation-insensitive identity so one model reported with two spellings
    // is one row rather than two smaller ones (#929).
    let mut groups: std::collections::HashMap<String, Vec<RawModelRow>> =
        std::collections::HashMap::new();
    // The key is unreadable by design, so each group also remembers the spelling
    // to display: whichever variant carries the most tokens, i.e. the form this
    // model is actually known by rather than the typo that split it.
    let mut display_names: std::collections::HashMap<String, (String, i64)> =
        std::collections::HashMap::new();
    for row in raw {
        let base = normalize_model_for_grouping(&row.normalized);
        let key = canonical_model_key(&base);
        let tokens = row.tokens;
        display_names
            .entry(key.clone())
            .and_modify(|best| {
                if tokens > best.1 {
                    *best = (base.clone(), tokens);
                }
            })
            .or_insert_with(|| (base.clone(), tokens));
        groups.entry(key).or_default().push(row);
    }

    // Build ModelEntry tree, sorted by total tokens descending
    let mut entries: Vec<ModelEntry> = groups
        .into_iter()
        .map(|(key, mut variants)| {
            // Fall back to the key only if a group somehow has no recorded
            // spelling, which cannot happen for a group built above.
            let base = display_names
                .get(&key)
                .map(|(name, _)| name.clone())
                .unwrap_or(key);
            let mut total_tokens: i64 = 0;
            let mut total_calls: i64 = 0;
            let mut total_cost: f64 = 0.0;
            let mut has_estimate = false;
            // Cache reads across the group, and the tokens of the rows that
            // actually recorded a split. `None` until a row records one, so a
            // model whose rows all predate the column reports no split (#2027).
            let mut cache_read_tokens: Option<i64> = None;
            let mut cache_split_tokens: i64 = 0;
            let mut child_rows: Vec<ModelVariant> = Vec::new();

            variants.sort_by_key(|b| std::cmp::Reverse(b.tokens)); // sort by tokens desc
            for v in variants {
                let RawModelRow {
                    raw: full_name,
                    tokens,
                    calls,
                    cost: ledger_cost,
                    cache_read,
                    split_tokens,
                    ..
                } = v;
                if let Some(cached) = cache_read {
                    cache_read_tokens = Some(cache_read_tokens.unwrap_or(0) + cached);
                }
                cache_split_tokens += split_tokens;
                // The ledger holds what each call really cost, cache reads
                // included (#1707), so it is the source of truth. The table
                // estimate is 80/20 input/output and ignores cache, so it only
                // fills rows the ledger recorded at zero.
                let (cost, estimated) = if ledger_cost > 0.0 {
                    (ledger_cost, false)
                } else {
                    let estimate = pricing
                        .as_ref()
                        .map(|p| p.estimate_cost(&full_name, tokens))
                        .unwrap_or(0.0);
                    // #1717: "~" means the conservative-default estimate from
                    // cost_origin, not the old cost == 0.0 sinkhole: a genuinely
                    // free model is Priced and renders $0.00 unmarked.
                    let origin_estimated = pricing.as_ref().is_some_and(|p| {
                        p.cost_origin(&full_name) == crate::usage::pricing::CostOrigin::Estimated
                    });
                    (estimate, origin_estimated)
                };
                if estimated {
                    has_estimate = true;
                }
                child_rows.push(ModelVariant {
                    name: full_name,
                    tokens,
                    cost,
                    calls,
                });
                total_tokens += tokens;
                total_calls += calls;
                total_cost += cost;
            }

            ModelEntry {
                model: base,
                tokens: total_tokens,
                cost: total_cost,
                calls: total_calls,
                estimated: has_estimate,
                cache_read_tokens,
                cache_split_tokens,
                variants: child_rows,
            }
        })
        .collect();

    entries.sort_by_key(|b| std::cmp::Reverse(b.tokens));
    Ok(entries)
}

async fn fetch_tools(pool: &Pool, since: Option<i64>) -> Result<Vec<ToolStats>> {
    let conn = pool.get().await.context("pool")?;
    conn.interact(move |conn| {
        // Exclude rows with empty tool_name. Historically 44+ such rows
        // landed on 2026-04-28 when a model emitted tool_use blocks with
        // missing names; they all errored at dispatch but the failure
        // still got recorded with the empty name, producing a blank row
        // in the dashboard. ToolRepo::record now refuses empty names at
        // write time, but this filter keeps legacy rows from rendering.
        let (query, param): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(s) = since {
            (
                "SELECT tool_name, COUNT(*) as cnt \
                 FROM tool_executions \
                 WHERE created_at >= ?1 AND tool_name <> '' \
                 GROUP BY tool_name ORDER BY cnt DESC",
                vec![Box::new(s)],
            )
        } else {
            (
                "SELECT tool_name, COUNT(*) as cnt \
                 FROM tool_executions \
                 WHERE tool_name <> '' \
                 GROUP BY tool_name ORDER BY cnt DESC",
                vec![],
            )
        };
        let refs: Vec<&dyn rusqlite::types::ToSql> = param.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(query)?;
        let rows = stmt.query_map(refs.as_slice(), |row| {
            Ok(ToolStats {
                tool_name: row.get(0)?,
                call_count: row.get(1)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
    })
    .await
    .map_err(interact_err)?
    .context("Failed to fetch tool stats")
}

async fn fetch_activities(pool: &Pool, since: Option<i64>) -> Result<Vec<ActivityStats>> {
    let conn = pool.get().await.context("pool")?;
    conn.interact(move |conn| -> rusqlite::Result<Vec<ActivityStats>> {
        // Fetch per-session stats with titles for classification
        let (query, param): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(s) = since {
            (
                "SELECT COALESCE(s.title, ''), \
                 COALESCE(SUM(u.cost), 0.0), COUNT(*), \
                 COUNT(DISTINCT u.session_id), s.category \
                 FROM usage_ledger u \
                 LEFT JOIN sessions s ON u.session_id = s.id \
                 WHERE u.created_at >= ?1 \
                 GROUP BY u.session_id",
                vec![Box::new(s)],
            )
        } else {
            (
                "SELECT COALESCE(s.title, ''), \
                 COALESCE(SUM(u.cost), 0.0), COUNT(*), \
                 COUNT(DISTINCT u.session_id), s.category \
                 FROM usage_ledger u \
                 LEFT JOIN sessions s ON u.session_id = s.id \
                 GROUP BY u.session_id",
                vec![],
            )
        };
        let refs: Vec<&dyn rusqlite::types::ToSql> = param.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(query)?;

        // Aggregate by activity category
        // (cost, turns, total_sessions, one_shot_sessions)
        let mut categories: std::collections::HashMap<String, (f64, i64, i64, i64)> =
            std::collections::HashMap::new();
        let rows = stmt.query_map(refs.as_slice(), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        for row in rows {
            let (title, cost, turns, explicit_cat) = row?;
            let category: String = match explicit_cat {
                Some(ref c) if !c.is_empty() => c.clone(),
                _ => classify_activity(&title).to_string(),
            };
            let entry = categories.entry(category).or_insert((0.0, 0, 0, 0));
            entry.0 += cost;
            entry.1 += turns;
            entry.2 += 1; // session count
            if turns <= 1 {
                entry.3 += 1; // one-shot session
            }
        }

        let mut result: Vec<ActivityStats> = categories
            .into_iter()
            .map(|(cat, (cost, turns, sessions, one_shot_sessions))| {
                let one_shot = if sessions > 0 {
                    (one_shot_sessions as f64 / sessions as f64) * 100.0
                } else {
                    0.0
                };
                ActivityStats {
                    category: cat.to_string(),
                    cost,
                    turns,
                    one_shot_pct: one_shot,
                }
            })
            .collect();
        result.sort_by(|a, b| {
            b.cost
                .partial_cmp(&a.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(result)
    })
    .await
    .map_err(interact_err)?
    .context("Failed to fetch activity stats")
}

/// SELECT columns for the cache-efficiency aggregation, yielding
/// `(cached_tokens, total_input_tokens)`.
///
/// Every column is COALESCE'd to 0 *individually*: even within a caching-capable
/// request a provider may report `cache_read` but leave `cache_creation` NULL,
/// and `input + NULL + cache_read` would be NULL — which `SUM()` silently skips.
/// Shared with the regression test so the two can't drift.
pub(crate) const CACHE_STATS_SELECT_COLS: &str = "COALESCE(SUM(cache_read_tokens), 0), \
     COALESCE(SUM(COALESCE(input_tokens, 0) + COALESCE(cache_creation_tokens, 0) + COALESCE(cache_read_tokens, 0)), 0)";

/// Cache-WRITE token sum, appended to [`CACHE_STATS_SELECT_COLS`] for the
/// per-model query (#2027). The money breakdown prices reads and writes at
/// different rates, so it needs the write count; the global efficiency number
/// does not, which is why this stays a separate column rather than a rewrite of
/// the shared select. Shared with the regression test.
pub(crate) const CACHE_COST_SELECT_COLS: &str = "COALESCE(SUM(cache_creation_tokens), 0)";

/// Filter to caching-CAPABLE requests only — the ones where the provider
/// actually reported cache usage (`cache_read`/`cache_creation` present, even if
/// the value is 0 on a cache miss). Requests on providers with no caching at all
/// (local llama.cpp, etc.) store NULL in both and are EXCLUDED, because the card
/// measures "of the input that could be cached, how much hit cache" — not a rate
/// diluted by models that have no caching concept. A cache MISS (value 0, not
/// NULL) stays in the denominator, correctly lowering the efficiency.
pub(crate) const CACHE_CAPABLE_WHERE: &str =
    "cache_read_tokens IS NOT NULL OR cache_creation_tokens IS NOT NULL";

/// Namespace-strip a model name for the per-model cache grouping: drops a
/// leading `vendor/` so `Qwen-Ambassador/Qwen3.7-Plus` groups as `Qwen3.7-Plus`.
/// References `s.model` (the sessions-table column), so callers must join
/// `messages m` to `sessions s`.
pub(crate) const CACHE_MODEL_EXPR: &str =
    "CASE WHEN s.model LIKE '%/%' THEN SUBSTR(s.model, INSTR(s.model, '/') + 1) ELSE s.model END";

async fn fetch_cache_stats(pool: &Pool, since: Option<i64>) -> Result<Option<CacheStats>> {
    // Loaded once per fetch, like the By Model card does, so every row in one
    // render prices against the same table. A table that fails to parse falls
    // back to the empty (all-estimated) default rather than dropping the card.
    let pricing = PricingConfig::load().unwrap_or_default();
    let conn = pool.get().await.context("pool")?;
    let result = conn
        .interact(move |conn| -> rusqlite::Result<Option<CacheStats>> {
            // ── Global efficiency ───────────────────────────────────────────
            let global_sql = if since.is_some() {
                format!(
                    "SELECT {CACHE_STATS_SELECT_COLS} FROM messages \
                     WHERE created_at >= ?1 AND ({CACHE_CAPABLE_WHERE})"
                )
            } else {
                format!(
                    "SELECT {CACHE_STATS_SELECT_COLS} FROM messages WHERE {CACHE_CAPABLE_WHERE}"
                )
            };
            let read_pair = |row: &rusqlite::Row| -> rusqlite::Result<(i64, i64)> {
                Ok((row.get(0)?, row.get(1)?))
            };
            let (cached_tokens, total_input_tokens): (i64, i64) = if let Some(s) = since {
                conn.query_row(&global_sql, params![s], read_pair)?
            } else {
                conn.query_row(&global_sql, [], read_pair)?
            };
            if total_input_tokens == 0 {
                return Ok(None);
            }
            let cache_hit_pct = (cached_tokens as f64 / total_input_tokens as f64) * 100.0;

            // ── Per-model efficiency + cache money (caching-capable rows) ────
            let per_sql = if since.is_some() {
                format!(
                    "SELECT {CACHE_MODEL_EXPR}, {CACHE_STATS_SELECT_COLS}, {CACHE_COST_SELECT_COLS} \
                     FROM messages m JOIN sessions s ON m.session_id = s.id \
                     WHERE m.created_at >= ?1 AND ({CACHE_CAPABLE_WHERE}) \
                     GROUP BY {CACHE_MODEL_EXPR}"
                )
            } else {
                format!(
                    "SELECT {CACHE_MODEL_EXPR}, {CACHE_STATS_SELECT_COLS}, {CACHE_COST_SELECT_COLS} \
                     FROM messages m JOIN sessions s ON m.session_id = s.id \
                     WHERE {CACHE_CAPABLE_WHERE} \
                     GROUP BY {CACHE_MODEL_EXPR}"
                )
            };
            let map_row =
                |row: &rusqlite::Row| -> rusqlite::Result<(Option<String>, i64, i64, i64)> {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                };
            let mut stmt = conn.prepare(&per_sql)?;
            let raw: Vec<(Option<String>, i64, i64, i64)> = if let Some(s) = since {
                stmt.query_map(params![s], map_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                stmt.query_map([], map_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            let mut per_model: Vec<ModelCacheStat> = raw
                .into_iter()
                .filter_map(|(model, cached, total, cache_creation)| {
                    let model = model.unwrap_or_default();
                    if model.is_empty() || total == 0 {
                        return None;
                    }
                    // Money for this model's cached tokens at its own rates.
                    let rates = pricing.rates_for(&model);
                    let cost = CacheCost::compute(cached, cache_creation, &rates);
                    Some(ModelCacheStat {
                        model,
                        cache_hit_pct: cached as f64 / total as f64 * 100.0,
                        cached_tokens: cached,
                        cache_creation_tokens: cache_creation,
                        cost,
                        input_per_m: rates.input_per_m,
                        cache_read_per_m: rates.cache_read_per_m,
                        estimated: rates.origin == CostOrigin::Estimated,
                    })
                })
                .collect();
            // Highest hit-rate first; cached-token volume breaks ties.
            per_model.sort_by(|a, b| {
                b.cache_hit_pct
                    .partial_cmp(&a.cache_hit_pct)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.cached_tokens.cmp(&a.cached_tokens))
            });

            Ok(Some(CacheStats {
                cache_hit_pct,
                cached_tokens,
                total_input_tokens,
                per_model,
            }))
        })
        .await
        .map_err(interact_err)?;

    // Graceful degradation: return None on any query error
    Ok(result.unwrap_or(None))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Format token count for display (e.g., 1292500000 → "1292.5M")
pub fn fmt_tokens(t: i64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.0}K", t as f64 / 1_000.0)
    } else {
        format!("{}", t)
    }
}

/// Format cost for display
pub fn fmt_cost(c: f64) -> String {
    if c >= 1.0 {
        format!("${:.2}", c)
    } else if c >= 0.01 {
        format!("${:.3}", c)
    } else {
        format!("${:.4}", c)
    }
}
