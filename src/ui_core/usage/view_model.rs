//! Plain-data view model for the usage dashboard.
//!
//! Every presentation decision that is not terminal geometry lives here:
//! formatted money and token cells, estimate (`~`) markers, bar ratios,
//! cache hit-rate bands, date slicing, variant alias filtering and the
//! breakdown footnotes. A terminal renderer and a GUI renderer both consume
//! these rows and cannot drift on what the numbers say, only on how they
//! paint them. No ratatui, no crossterm, no terminal types.

use super::data::{
    DashboardData, ModelEntry, Period, fmt_cost, fmt_tokens, is_cosmetic_alias_of_parent,
    normalize_model_for_grouping,
};
use crate::VERSION;

/// Focusable cards in the dashboard, in focus order: 0 daily, 1 project,
/// 2 model, 3 tools, 4 activity, 5 cache. The layout consumes this as the
/// grid shape, so the count has one owner (here) and every renderer and
/// key handler follows it.
pub const USAGE_CARD_COUNT: usize = 6;

/// Traffic-light band for a cache hit rate. Thresholds match the terminal
/// card: high >= 60%, mid >= 30%, low below that. Renderers map the band to
/// their own palette, so both stay in agreement on WHICH rates are bad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitBand {
    High,
    Mid,
    Low,
}

impl HitBand {
    fn from_pct(p: f64) -> Self {
        if p >= 60.0 {
            HitBand::High
        } else if p >= 30.0 {
            HitBand::Mid
        } else {
            HitBand::Low
        }
    }
}

/// Money cell with the default-table estimate marker (#1717): `~` in front
/// when the price came from the fallback table, so an estimate is never
/// read as a quoted rate.
fn money(cost: f64, estimated: bool) -> String {
    if estimated {
        format!("~{}", fmt_cost(cost))
    } else {
        fmt_cost(cost)
    }
}

/// The whole dashboard's prepared content for one period.
#[derive(Debug, Clone)]
pub struct UsageViewModel {
    pub period_label: &'static str,
    pub version: String,
    pub summary: SummaryView,
    pub daily: Vec<DailyRow>,
    pub projects: Vec<ProjectRow>,
    pub models: Vec<ModelRow>,
    pub tools: Vec<ToolRow>,
    pub activities: Vec<ActivityRow>,
    pub cache: Option<CacheView>,
}

/// Pinned summary bar cells.
#[derive(Debug, Clone)]
pub struct SummaryView {
    pub tokens_str: String,
    /// Total cost, `~`-prefixed when any By Model row is an estimate (#2249).
    pub cost_str: String,
    pub sessions_str: String,
    pub calls_str: String,
    /// `(N% unpriced)` marker, `None` when the ledger has no unpriced tokens.
    pub unpriced_marker: Option<String>,
}

/// One Daily Activity row, most recent day first. `bar_ratio` is this
/// day's tokens over the busiest day (0.0-1.0); renderers scale it to
/// their own bar width.
#[derive(Debug, Clone)]
pub struct DailyRow {
    pub short_date: String,
    pub tokens_str: String,
    pub bar_ratio: f64,
}

/// One By Project row with its display cells.
#[derive(Debug, Clone)]
pub struct ProjectRow {
    pub name: String,
    pub cost_str: String,
    pub tokens_str: String,
    pub sessions_str: String,
}

/// One By Model parent row. Variants are already filtered: cosmetic
/// aliases of the parent (separator/case differences only) are dropped,
/// because they duplicate the parent line without adding information.
#[derive(Debug, Clone)]
pub struct ModelRow {
    pub name: String,
    pub cost_str: String,
    pub estimated: bool,
    pub tokens_str: String,
    pub calls_str: String,
    /// `NN% cache` when the ledger recorded a split for this model, `None`
    /// renders blank rather than an unmeasured zero (#2027).
    pub cache_cell: Option<String>,
    pub variants: Vec<ModelVariantRow>,
}

/// One quantization variant row under a parent model.
#[derive(Debug, Clone)]
pub struct ModelVariantRow {
    pub name: String,
    pub cost_str: String,
    pub tokens_str: String,
    pub calls_str: String,
}

/// One Core Tools row. `bar_ratio` is this tool's calls over the top
/// tool's calls.
#[derive(Debug, Clone)]
pub struct ToolRow {
    pub name: String,
    pub call_count: i64,
    pub bar_ratio: f64,
}

/// One By Activity row with its display cells. The raw cost and pct ride
/// along because column-width fitting measures the widest raw value, which
/// can exceed the rounded display string.
#[derive(Debug, Clone)]
pub struct ActivityRow {
    pub category: String,
    pub cost: f64,
    pub cost_str: String,
    pub turns: i64,
    pub one_shot_pct: f64,
    /// `NN%` one-shot share as displayed.
    pub one_shot_str: String,
    pub bar_ratio: f64,
}

/// Cache Efficiency card content, both the compact header view and the
/// price breakdown the Enter key opens (#2027).
#[derive(Debug, Clone)]
pub struct CacheView {
    pub overall_pct: f64,
    pub overall_band: HitBand,
    /// Per-model rows, highest hit-rate first (as recorded).
    pub models: Vec<CacheModelRow>,
    /// `X / Y cached` line for ledgers with no per-model rows.
    pub fallback_line: Option<String>,
    /// TOTAL row cells of the price breakdown.
    pub total: BreakdownTotal,
    /// `NN% of input tokens were cache reads; writes cost $X on top`.
    pub footnote1: String,
    /// Static column legend under the breakdown.
    pub footnote2: &'static str,
}

/// One per-model row: the card's `model … NN%` line and the breakdown's
/// money cells, `~`-marked when the rate is the default-table estimate.
#[derive(Debug, Clone)]
pub struct CacheModelRow {
    pub model: String,
    pub hit_pct: f64,
    pub band: HitBand,
    pub cached_str: String,
    /// `~$0.12` cache-read rate per million.
    pub rate_str: String,
    pub read_str: String,
    pub full_rate_str: String,
    pub saved_str: String,
    pub estimated: bool,
}

/// Summed money cells for the breakdown's TOTAL row.
#[derive(Debug, Clone)]
pub struct BreakdownTotal {
    pub cached_str: String,
    pub read_str: String,
    pub full_rate_str: String,
    pub saved_str: String,
}

impl UsageViewModel {
    /// Prepare the whole dashboard from one data fetch. Cheap: formatting
    /// only, no I/O, so renderers may call it per draw.
    pub fn build(data: &DashboardData, period: Period) -> Self {
        Self {
            period_label: period.label(),
            version: VERSION.to_string(),
            summary: Self::build_summary(data),
            daily: Self::build_daily(&data.daily),
            projects: Self::build_projects(&data.projects),
            models: Self::build_models(&data.models),
            tools: Self::build_tools(&data.tools),
            activities: Self::build_activities(&data.activities),
            cache: data.cache.as_ref().map(Self::build_cache),
        }
    }

    fn build_summary(data: &DashboardData) -> SummaryView {
        let s = &data.summary;
        let estimated = data.models.iter().any(|m| m.estimated);
        let unpriced_marker = if s.total_tokens > 0 && s.unpriced_tokens > 0 {
            let pct = s.unpriced_tokens as f64 / s.total_tokens as f64 * 100.0;
            Some(format!(" ({pct:.0}% unpriced)"))
        } else {
            None
        };
        SummaryView {
            tokens_str: fmt_tokens(s.total_tokens),
            cost_str: money(s.total_cost, estimated),
            sessions_str: s.session_count.to_string(),
            calls_str: s.call_count.to_string(),
            unpriced_marker,
        }
    }

    fn build_daily(daily: &[super::data::DailyStats]) -> Vec<DailyRow> {
        let max_tokens = daily.iter().map(|d| d.tokens).max().unwrap_or(1);
        daily
            .iter()
            .rev()
            .map(|d| DailyRow {
                short_date: short_date(&d.date),
                tokens_str: fmt_tokens(d.tokens),
                bar_ratio: ratio(d.tokens as f64, max_tokens as f64),
            })
            .collect()
    }

    fn build_projects(projects: &[super::data::ProjectStats]) -> Vec<ProjectRow> {
        projects
            .iter()
            .map(|p| ProjectRow {
                name: p.project.clone(),
                cost_str: fmt_cost(p.cost),
                tokens_str: fmt_tokens(p.tokens),
                sessions_str: p.sessions.to_string(),
            })
            .collect()
    }

    fn build_models(models: &[ModelEntry]) -> Vec<ModelRow> {
        models
            .iter()
            .map(|m| ModelRow {
                name: m.model.clone(),
                cost_str: money(m.cost, m.estimated),
                estimated: m.estimated,
                tokens_str: fmt_tokens(m.tokens),
                calls_str: m.calls.to_string(),
                cache_cell: m.cache_read_pct().map(|pct| format!("{pct:.0}% cache")),
                variants: m
                    .variants
                    .iter()
                    .filter(|v| {
                        normalize_model_for_grouping(&v.name) != m.model
                            && !is_cosmetic_alias_of_parent(&v.name, &m.model)
                    })
                    .map(|v| ModelVariantRow {
                        name: v.name.clone(),
                        cost_str: fmt_cost(v.cost),
                        tokens_str: fmt_tokens(v.tokens),
                        calls_str: v.calls.to_string(),
                    })
                    .collect(),
            })
            .collect()
    }

    fn build_tools(tools: &[super::data::ToolStats]) -> Vec<ToolRow> {
        let max_count = tools.first().map(|t| t.call_count).unwrap_or(1);
        tools
            .iter()
            .map(|t| ToolRow {
                name: t.tool_name.clone(),
                call_count: t.call_count,
                bar_ratio: ratio(t.call_count as f64, max_count as f64),
            })
            .collect()
    }

    fn build_activities(activities: &[super::data::ActivityStats]) -> Vec<ActivityRow> {
        let max_cost = activities.iter().map(|a| a.cost).fold(0.0_f64, f64::max);
        activities
            .iter()
            .map(|a| ActivityRow {
                category: a.category.clone(),
                cost: a.cost,
                cost_str: fmt_cost(a.cost),
                turns: a.turns,
                one_shot_pct: a.one_shot_pct,
                one_shot_str: format!("{}%", a.one_shot_pct as u32),
                bar_ratio: ratio(a.cost, max_cost),
            })
            .collect()
    }

    fn build_cache(cache: &super::data::CacheStats) -> CacheView {
        let totals = cache.cost_totals();
        let any_estimated = cache.per_model.iter().any(|m| m.estimated);
        CacheView {
            overall_pct: cache.cache_hit_pct,
            overall_band: HitBand::from_pct(cache.cache_hit_pct),
            fallback_line: if cache.per_model.is_empty() {
                Some(format!(
                    "{} / {} cached",
                    fmt_tokens(cache.cached_tokens),
                    fmt_tokens(cache.total_input_tokens)
                ))
            } else {
                None
            },
            models: cache
                .per_model
                .iter()
                .map(|ms| CacheModelRow {
                    model: ms.model.clone(),
                    hit_pct: ms.cache_hit_pct,
                    band: HitBand::from_pct(ms.cache_hit_pct),
                    cached_str: fmt_tokens(ms.cached_tokens),
                    // The rate column is always `$X.XX`; the estimate state
                    // is styling-only here (the text marker lives on the
                    // money columns).
                    rate_str: format!("${:.2}", ms.cache_read_per_m),
                    read_str: money(ms.cost.read_cost, ms.estimated),
                    full_rate_str: money(ms.cost.full_rate_cost, ms.estimated),
                    saved_str: money(ms.cost.saved, ms.estimated),
                    estimated: ms.estimated,
                })
                .collect(),
            total: BreakdownTotal {
                cached_str: fmt_tokens(cache.cached_tokens),
                read_str: money(totals.read_cost, any_estimated),
                full_rate_str: money(totals.full_rate_cost, any_estimated),
                saved_str: money(totals.saved, any_estimated),
            },
            footnote1: format!(
                " {:.0}% of input tokens were cache reads; writes cost {} on top",
                cache.cache_hit_pct,
                fmt_cost(totals.write_cost)
            ),
            footnote2: " saved = full rate - read rate; ~ = default table price",
        }
    }
}

/// `MM-DD` short form of an ISO date, or the input untouched when it is
/// already short.
fn short_date(date: &str) -> String {
    if date.len() >= 10 {
        date[5..10].to_string()
    } else {
        date.to_string()
    }
}

/// Share of `value` against `max`, guarded for a zero/absent maximum so
/// the renderer never divides by zero.
fn ratio(value: f64, max: f64) -> f64 {
    if max > 0.0 { value / max } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_core::usage::data::{
        ActivityStats, CacheStats, DailyStats, ModelCacheStat, ModelVariant, ProjectStats,
        SummaryStats, ToolStats,
    };
    use crate::ui_core::usage::pricing::CacheCost;

    fn fixture() -> DashboardData {
        DashboardData {
            summary: SummaryStats {
                total_tokens: 1_000_000,
                total_cost: 12.5,
                session_count: 7,
                call_count: 42,
                unpriced_tokens: 250_000,
            },
            daily: vec![
                DailyStats {
                    date: "2026-04-14".into(),
                    tokens: 300,
                    cost: 0.1,
                    calls: 3,
                },
                DailyStats {
                    date: "2026-04-15".into(),
                    tokens: 900,
                    cost: 0.3,
                    calls: 9,
                },
            ],
            projects: vec![ProjectStats {
                project: "opencrabs".into(),
                cost: 5.0,
                tokens: 500,
                sessions: 2,
            }],
            models: vec![ModelEntry {
                model: "qwen3.6-35b-a3b".into(),
                tokens: 700,
                cost: 1.5,
                calls: 11,
                estimated: true,
                cache_read_tokens: Some(350),
                cache_split_tokens: 700,
                variants: vec![
                    // Cosmetic alias of the parent: separators differ only.
                    ModelVariant {
                        name: "qwen-3.6-35b-a3b".into(),
                        tokens: 10,
                        cost: 0.01,
                        calls: 1,
                    },
                    // Real variant: a preview suffix survives normalization,
                    // so it is not the parent and is not cosmetic.
                    ModelVariant {
                        name: "qwen3.6-35b-a3b-preview".into(),
                        tokens: 100,
                        cost: 0.2,
                        calls: 2,
                    },
                ],
            }],
            tools: vec![
                ToolStats {
                    tool_name: "bash".into(),
                    call_count: 40,
                },
                ToolStats {
                    tool_name: "read".into(),
                    call_count: 10,
                },
            ],
            activities: vec![ActivityStats {
                category: "coding".into(),
                cost: 3.0,
                turns: 12,
                one_shot_pct: 58.6,
            }],
            cache: Some(CacheStats {
                cache_hit_pct: 64.0,
                cached_tokens: 640,
                total_input_tokens: 1000,
                per_model: vec![ModelCacheStat {
                    model: "m1".into(),
                    cache_hit_pct: 25.0,
                    cached_tokens: 250,
                    cache_creation_tokens: 10,
                    cost: CacheCost {
                        read_cost: 0.10,
                        write_cost: 0.05,
                        full_rate_cost: 0.40,
                        saved: 0.30,
                    },
                    input_per_m: 1.0,
                    cache_read_per_m: 0.25,
                    estimated: true,
                }],
            }),
        }
    }

    #[test]
    fn builds_all_six_card_sections() {
        let vm = UsageViewModel::build(&fixture(), Period::Month);
        assert_eq!(vm.period_label, Period::Month.label());
        assert_eq!(vm.daily.len(), 2);
        assert_eq!(vm.projects.len(), 1);
        assert_eq!(vm.models.len(), 1);
        assert_eq!(vm.tools.len(), 2);
        assert_eq!(vm.activities.len(), 1);
        assert!(vm.cache.is_some());
    }

    #[test]
    fn summary_carries_estimate_marker_and_unpriced_share() {
        let vm = UsageViewModel::build(&fixture(), Period::AllTime);
        assert_eq!(vm.summary.cost_str, "~$12.50");
        assert_eq!(vm.summary.unpriced_marker, Some(" (25% unpriced)".into()));
        assert_eq!(vm.summary.sessions_str, "7");
        assert_eq!(vm.summary.calls_str, "42");
    }

    #[test]
    fn daily_rows_are_most_recent_first_with_ratios() {
        let vm = UsageViewModel::build(&fixture(), Period::Today);
        assert_eq!(vm.daily[0].short_date, "04-15");
        assert_eq!(vm.daily[1].short_date, "04-14");
        assert!((vm.daily[0].bar_ratio - 1.0).abs() < 1e-9);
        assert!((vm.daily[1].bar_ratio - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn model_variants_drop_cosmetic_aliases_only() {
        let vm = UsageViewModel::build(&fixture(), Period::Week);
        let m = &vm.models[0];
        assert_eq!(m.variants.len(), 1);
        assert_eq!(m.variants[0].name, "qwen3.6-35b-a3b-preview");
        assert_eq!(m.cost_str, "~$1.50");
        assert_eq!(m.cache_cell.as_deref(), Some("50% cache"));
    }

    #[test]
    fn tool_ratios_measure_against_the_top_tool() {
        let vm = UsageViewModel::build(&fixture(), Period::AllTime);
        assert!((vm.tools[0].bar_ratio - 1.0).abs() < 1e-9);
        assert!((vm.tools[1].bar_ratio - 0.25).abs() < 1e-9);
    }

    #[test]
    fn cache_bands_follow_the_traffic_thresholds() {
        let vm = UsageViewModel::build(&fixture(), Period::AllTime);
        let cache = vm.cache.expect("fixture has cache");
        assert_eq!(cache.overall_band, HitBand::High);
        assert_eq!(cache.models[0].band, HitBand::Low);
        assert_eq!(cache.models[0].rate_str, "$0.25");
        assert_eq!(cache.models[0].read_str, "~$0.100");
        // Totals: only one model, so marked estimated like its rows.
        assert_eq!(cache.total.read_str, "~$0.100");
        assert_eq!(cache.total.saved_str, "~$0.300");
        assert!(cache.footnote1.contains("64% of input tokens"));
        assert!(cache.fallback_line.is_none());
    }

    #[test]
    fn empty_ledger_builds_an_empty_model_without_markers() {
        let vm = UsageViewModel::build(&DashboardData::default(), Period::Today);
        assert_eq!(vm.summary.unpriced_marker, None);
        assert!(vm.daily.is_empty() && vm.models.is_empty() && vm.tools.is_empty());
        assert!(vm.cache.is_none());
    }

    #[test]
    fn short_date_handles_short_inputs() {
        assert_eq!(short_date("2026-04-15"), "04-15");
        assert_eq!(short_date("04-15"), "04-15");
    }
}
