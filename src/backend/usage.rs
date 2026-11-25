//! Token and cost usage views.
//!
//! The crab reads the usage numbers; the surface builds its view model from
//! them (`UsageViewModel::build`), so the formatting stays where the drawing
//! is. The usage types carry no serde of their own, so the data crosses the
//! wire as the plain mirrors below, each converted from and back into the
//! type the view model is built from. A field added to a usage type has to be
//! added to its mirror too, or the conversion stops compiling.

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::ui_core::usage::data::{
    ActivityStats, CacheStats, DailyStats, DashboardData, ModelCacheStat, ModelEntry,
    ModelVariant, Period, ProjectStats, SummaryStats, ToolStats,
};
use crate::ui_core::usage::pricing::CacheCost;

use super::{BackendError, Ctx, Op, Scope};

/// The wire shape of [`Period`].
#[allow(dead_code)]
#[derive(Serialize, Deserialize)]
#[serde(remote = "Period")]
enum PeriodWire {
    Today,
    Week,
    Month,
    AllTime,
}

fn into_all<A, B: From<A>>(items: Vec<A>) -> Vec<B> {
    items.into_iter().map(B::from).collect()
}

/// The summary bar on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryWire {
    pub total_tokens: i64,
    pub total_cost: f64,
    pub session_count: i64,
    pub call_count: i64,
    pub unpriced_tokens: i64,
}

impl From<SummaryStats> for SummaryWire {
    fn from(s: SummaryStats) -> Self {
        Self {
            total_tokens: s.total_tokens,
            total_cost: s.total_cost,
            session_count: s.session_count,
            call_count: s.call_count,
            unpriced_tokens: s.unpriced_tokens,
        }
    }
}

impl From<SummaryWire> for SummaryStats {
    fn from(s: SummaryWire) -> Self {
        Self {
            total_tokens: s.total_tokens,
            total_cost: s.total_cost,
            session_count: s.session_count,
            call_count: s.call_count,
            unpriced_tokens: s.unpriced_tokens,
        }
    }
}

/// One day on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyWire {
    pub date: String,
    pub tokens: i64,
    pub cost: f64,
    pub calls: i64,
}

impl From<DailyStats> for DailyWire {
    fn from(d: DailyStats) -> Self {
        Self {
            date: d.date,
            tokens: d.tokens,
            cost: d.cost,
            calls: d.calls,
        }
    }
}

impl From<DailyWire> for DailyStats {
    fn from(d: DailyWire) -> Self {
        Self {
            date: d.date,
            tokens: d.tokens,
            cost: d.cost,
            calls: d.calls,
        }
    }
}

/// One project on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectWire {
    pub project: String,
    pub cost: f64,
    pub tokens: i64,
    pub sessions: i64,
}

impl From<ProjectStats> for ProjectWire {
    fn from(p: ProjectStats) -> Self {
        Self {
            project: p.project,
            cost: p.cost,
            tokens: p.tokens,
            sessions: p.sessions,
        }
    }
}

impl From<ProjectWire> for ProjectStats {
    fn from(p: ProjectWire) -> Self {
        Self {
            project: p.project,
            cost: p.cost,
            tokens: p.tokens,
            sessions: p.sessions,
        }
    }
}

/// One model variant on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantWire {
    pub name: String,
    pub tokens: i64,
    pub cost: f64,
    pub calls: i64,
}

impl From<ModelVariant> for VariantWire {
    fn from(v: ModelVariant) -> Self {
        Self {
            name: v.name,
            tokens: v.tokens,
            cost: v.cost,
            calls: v.calls,
        }
    }
}

impl From<VariantWire> for ModelVariant {
    fn from(v: VariantWire) -> Self {
        Self {
            name: v.name,
            tokens: v.tokens,
            cost: v.cost,
            calls: v.calls,
        }
    }
}

/// One model on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelWire {
    pub model: String,
    pub tokens: i64,
    pub cost: f64,
    pub calls: i64,
    pub estimated: bool,
    pub cache_read_tokens: Option<i64>,
    pub cache_split_tokens: i64,
    pub variants: Vec<VariantWire>,
}

impl From<ModelEntry> for ModelWire {
    fn from(m: ModelEntry) -> Self {
        Self {
            model: m.model,
            tokens: m.tokens,
            cost: m.cost,
            calls: m.calls,
            estimated: m.estimated,
            cache_read_tokens: m.cache_read_tokens,
            cache_split_tokens: m.cache_split_tokens,
            variants: into_all(m.variants),
        }
    }
}

impl From<ModelWire> for ModelEntry {
    fn from(m: ModelWire) -> Self {
        Self {
            model: m.model,
            tokens: m.tokens,
            cost: m.cost,
            calls: m.calls,
            estimated: m.estimated,
            cache_read_tokens: m.cache_read_tokens,
            cache_split_tokens: m.cache_split_tokens,
            variants: into_all(m.variants),
        }
    }
}

/// One tool's call count on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolWire {
    pub tool_name: String,
    pub call_count: i64,
}

impl From<ToolStats> for ToolWire {
    fn from(t: ToolStats) -> Self {
        Self {
            tool_name: t.tool_name,
            call_count: t.call_count,
        }
    }
}

impl From<ToolWire> for ToolStats {
    fn from(t: ToolWire) -> Self {
        Self {
            tool_name: t.tool_name,
            call_count: t.call_count,
        }
    }
}

/// One activity category on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityWire {
    pub category: String,
    pub cost: f64,
    pub turns: i64,
    pub one_shot_pct: f64,
}

impl From<ActivityStats> for ActivityWire {
    fn from(a: ActivityStats) -> Self {
        Self {
            category: a.category,
            cost: a.cost,
            turns: a.turns,
            one_shot_pct: a.one_shot_pct,
        }
    }
}

impl From<ActivityWire> for ActivityStats {
    fn from(a: ActivityWire) -> Self {
        Self {
            category: a.category,
            cost: a.cost,
            turns: a.turns,
            one_shot_pct: a.one_shot_pct,
        }
    }
}

/// Cache money on the wire.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CacheCostWire {
    pub read_cost: f64,
    pub write_cost: f64,
    pub full_rate_cost: f64,
    pub saved: f64,
}

impl From<CacheCost> for CacheCostWire {
    fn from(c: CacheCost) -> Self {
        Self {
            read_cost: c.read_cost,
            write_cost: c.write_cost,
            full_rate_cost: c.full_rate_cost,
            saved: c.saved,
        }
    }
}

impl From<CacheCostWire> for CacheCost {
    fn from(c: CacheCostWire) -> Self {
        Self {
            read_cost: c.read_cost,
            write_cost: c.write_cost,
            full_rate_cost: c.full_rate_cost,
            saved: c.saved,
        }
    }
}

/// One model's cache efficiency on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCacheWire {
    pub model: String,
    pub cache_hit_pct: f64,
    pub cached_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost: CacheCostWire,
    pub input_per_m: f64,
    pub cache_read_per_m: f64,
    pub estimated: bool,
}

impl From<ModelCacheStat> for ModelCacheWire {
    fn from(m: ModelCacheStat) -> Self {
        Self {
            model: m.model,
            cache_hit_pct: m.cache_hit_pct,
            cached_tokens: m.cached_tokens,
            cache_creation_tokens: m.cache_creation_tokens,
            cost: m.cost.into(),
            input_per_m: m.input_per_m,
            cache_read_per_m: m.cache_read_per_m,
            estimated: m.estimated,
        }
    }
}

impl From<ModelCacheWire> for ModelCacheStat {
    fn from(m: ModelCacheWire) -> Self {
        Self {
            model: m.model,
            cache_hit_pct: m.cache_hit_pct,
            cached_tokens: m.cached_tokens,
            cache_creation_tokens: m.cache_creation_tokens,
            cost: m.cost.into(),
            input_per_m: m.input_per_m,
            cache_read_per_m: m.cache_read_per_m,
            estimated: m.estimated,
        }
    }
}

/// The cache card on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheWire {
    pub cache_hit_pct: f64,
    pub cached_tokens: i64,
    pub total_input_tokens: i64,
    pub per_model: Vec<ModelCacheWire>,
}

impl From<CacheStats> for CacheWire {
    fn from(c: CacheStats) -> Self {
        Self {
            cache_hit_pct: c.cache_hit_pct,
            cached_tokens: c.cached_tokens,
            total_input_tokens: c.total_input_tokens,
            per_model: into_all(c.per_model),
        }
    }
}

impl From<CacheWire> for CacheStats {
    fn from(c: CacheWire) -> Self {
        Self {
            cache_hit_pct: c.cache_hit_pct,
            cached_tokens: c.cached_tokens,
            total_input_tokens: c.total_input_tokens,
            per_model: into_all(c.per_model),
        }
    }
}

/// Every number the usage dashboard draws for one period, as it travels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageData {
    pub summary: SummaryWire,
    pub daily: Vec<DailyWire>,
    pub projects: Vec<ProjectWire>,
    pub models: Vec<ModelWire>,
    pub tools: Vec<ToolWire>,
    pub activities: Vec<ActivityWire>,
    pub cache: Option<CacheWire>,
}

impl From<DashboardData> for UsageData {
    fn from(d: DashboardData) -> Self {
        Self {
            summary: d.summary.into(),
            daily: into_all(d.daily),
            projects: into_all(d.projects),
            models: into_all(d.models),
            tools: into_all(d.tools),
            activities: into_all(d.activities),
            cache: d.cache.map(CacheWire::from),
        }
    }
}

impl From<UsageData> for DashboardData {
    fn from(d: UsageData) -> Self {
        Self {
            summary: d.summary.into(),
            daily: into_all(d.daily),
            projects: into_all(d.projects),
            models: into_all(d.models),
            tools: into_all(d.tools),
            activities: into_all(d.activities),
            cache: d.cache.map(CacheStats::from),
        }
    }
}

/// Read the usage numbers for `period`.
///
/// The loader's by-path door: pragmas only, no migrations, the daemon and the
/// TUI stay the owners of the schema. A failure is its `{e:#}` text, with the
/// same context lines the loader writes (the database that did not open, the
/// period that could not be read).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageView {
    /// The period the numbers cover.
    #[serde(with = "PeriodWire")]
    pub period: Period,
}

impl Op for UsageView {
    const NAME: &'static str = "usage.view";
    const SCOPE: Scope = Scope::Read;
    type Reply = UsageData;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let period = self.period;
        let read = async {
            let db = ctx.database().await.with_context(|| {
                format!(
                    "failed to open the usage database at {}",
                    ctx.db_path().display()
                )
            })?;
            DashboardData::fetch(db.pool(), period)
                .await
                .with_context(|| format!("failed to read usage data for {}", period.label()))
        };
        read.await
            .map(UsageData::from)
            .map_err(|e: anyhow::Error| BackendError::Failed(format!("{e:#}")))
    }
}

ops! { UsageView }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};
    use crate::ui_core::usage::view_model::UsageViewModel;

    #[tokio::test]
    async fn usage_view_has_parity() {
        let gateway = Gateway::start().await;
        for period in [Period::Today, Period::AllTime] {
            let answer = parity!(gateway, UsageView { period });
            assert_eq!(answer["Ok"]["daily"], serde_json::json!([]), "{answer}");
        }
        forbidden!(
            gateway,
            UsageView {
                period: Period::Week,
            }
        );
    }

    /// The view the window builds from the crab's numbers is the view the
    /// loader builds from the ledger itself.
    #[tokio::test]
    async fn the_view_built_from_the_wire_is_the_loaders_view() {
        let gateway = Gateway::start().await;
        let data = UsageView {
            period: Period::Month,
        }
        .run(&gateway.ctx)
        .await
        .expect("usage reads on a fresh ledger");
        let json = serde_json::to_string(&data).expect("encode");
        let back: UsageData = serde_json::from_str(&json).expect("decode");
        let built = UsageViewModel::build(&back.into(), Period::Month);
        let loaded = crate::ui_core::usage::loader::usage_view(gateway.ctx.db_path(), Period::Month)
            .await
            .expect("the loader reads the same ledger");
        assert_eq!(format!("{built:?}"), format!("{loaded:?}"));
    }

    #[test]
    fn usage_data_survives_the_wire() {
        let data = DashboardData {
            summary: SummaryStats {
                total_tokens: 1200,
                total_cost: 0.5,
                session_count: 2,
                call_count: 7,
                unpriced_tokens: 0,
            },
            models: vec![ModelEntry {
                model: "claude-x".into(),
                tokens: 1200,
                cost: 0.5,
                calls: 7,
                estimated: true,
                cache_read_tokens: Some(300),
                cache_split_tokens: 900,
                variants: vec![ModelVariant {
                    name: "claude-x-latest".into(),
                    tokens: 1200,
                    cost: 0.5,
                    calls: 7,
                }],
            }],
            cache: Some(CacheStats {
                cache_hit_pct: 25.0,
                cached_tokens: 300,
                total_input_tokens: 1200,
                per_model: Vec::new(),
            }),
            ..DashboardData::default()
        };
        let json = serde_json::to_string(&UsageData::from(data.clone())).expect("encode");
        let back: DashboardData = serde_json::from_str::<UsageData>(&json)
            .expect("decode")
            .into();
        assert_eq!(format!("{back:?}"), format!("{data:?}"));
        let period: Period = serde_json::from_value::<UsageView>(
            serde_json::json!({ "period": "AllTime" }),
        )
        .expect("a period decodes")
        .period;
        assert_eq!(period, Period::AllTime);
    }
}
