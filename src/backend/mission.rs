//! Mission control: load and decide.
//!
//! The Mission Control types carry no serde of their own, so the snapshot
//! crosses the wire as the plain mirrors below, each converted from and back
//! into the type the panels draw. A field added to a panel type has to be
//! added to its mirror too, or the conversion stops compiling.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use chrono::{DateTime, Utc};

use crate::brain::mission_control::types::{
    McBrainVerifyStats, McDecisionStat, McModelToolStat, McPhantomStats, McStreamingStats,
};
use crate::brain::mission_control::{
    ACTIVITY_LIMIT, McActivity, McActivityLevel, McAnalytics, McBrainFile, McInboxItem,
    McInboxKind, McScheduleItem, McScheduleKind, McToolStat, TimeWindow, activity_service,
    analytics_service, inbox_service, schedule_service,
};
use crate::db::Pool;
use crate::brain::prompt_builder::BrainLoader;
use crate::brain::tools::ToolRegistry;
use crate::brain::tools::dynamic::DynamicToolLoader;
use crate::brain::tools::rsi_proposals::RsiProposalsTool;

use super::{BackendError, Ctx, Op, Scope};

/// What a key on the inbox asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// Install the proposal: the loaders write it live.
    Apply,
    /// Archive it without installing.
    Reject,
}

/// The wire shape of [`McInboxKind`], which carries no serde of its own. The
/// variant names are the remote type's, which is what `serde(remote)` needs.
#[allow(dead_code, clippy::enum_variant_names)]
#[derive(Serialize, Deserialize)]
#[serde(remote = "McInboxKind")]
enum McInboxKindWire {
    ProposedTool,
    ProposedCommand,
    ProposedSkill,
}

/// Apply or reject one proposal with the paths already resolved.
///
/// Sync and path-explicit so a test can point every write at a scratch home
/// without the override having to reach a spawned thread (the profile
/// override is a task-local and does not cross `std::thread::spawn`).
pub fn decide_with(
    registry: Arc<ToolRegistry>,
    id: &str,
    kind: &McInboxKind,
    decision: Decision,
    tools_path: PathBuf,
    brain_path: PathBuf,
) -> Result<String, String> {
    let tool = RsiProposalsTool::new(registry, tools_path, brain_path);
    match decision {
        Decision::Apply => tool.apply(id, kind),
        Decision::Reject => tool.reject(id, None),
    }
}

/// Apply or reject one RSI proposal from the Mission Control inbox. The
/// reply is the tool's own message.
///
/// The writes (tools.toml, commands.toml, SKILL.md, the archive) land in this
/// crab's own files: the two paths are resolved here, the same ones the
/// terminal resolves, and the tool registry is the crab's service's. Before
/// boot finished there is no registry, and the answer is `NotReady`.
///
/// Scope `manage` gets a caller to the inbox. Installing a proposal writes a
/// tool the crab will run, so at the remote door `Apply` also needs
/// `a2a:admin`; `Reject` only archives. The local door decides either.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionDecide {
    /// The proposal's inbox id (`prop_tool_<uuid>`, `prop_cmd_<uuid>`, ...).
    pub id: String,
    /// What kind of artifact it is.
    #[serde(with = "McInboxKindWire")]
    pub kind: McInboxKind,
    /// Apply or reject.
    pub decision: Decision,
}

impl Op for MissionDecide {
    const NAME: &'static str = "mission.decide";
    const SCOPE: Scope = Scope::Manage;
    type Reply = String;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        if self.decision == Decision::Apply
            && let Some(caller) = super::door::caller()
            && !caller.holds(Scope::Admin)
        {
            return Err(BackendError::Forbidden {
                scope: Scope::Admin,
            });
        }
        let Some(tools_path) = DynamicToolLoader::default_path() else {
            return Err(BackendError::Failed(
                "no tools.toml path for this instance".to_string(),
            ));
        };
        let brain_path = BrainLoader::resolve_path();
        let registry = ctx.service()?.tool_registry().clone();
        decide_with(
            registry,
            &self.id,
            &self.kind,
            self.decision,
            tools_path,
            brain_path,
        )
        .map_err(BackendError::Failed)
    }
}

/// The wire shape of [`TimeWindow`].
#[allow(dead_code)]
#[derive(Serialize, Deserialize)]
#[serde(remote = "TimeWindow")]
enum TimeWindowWire {
    Day,
    Week,
    Month,
    All,
}

/// The wire shape of [`McActivityLevel`].
#[allow(dead_code)]
#[derive(Serialize, Deserialize)]
#[serde(remote = "McActivityLevel")]
enum McActivityLevelWire {
    Info,
    Success,
    Warn,
    Error,
}

/// The wire shape of [`McScheduleKind`].
#[allow(dead_code)]
#[derive(Serialize, Deserialize)]
#[serde(remote = "McScheduleKind")]
enum McScheduleKindWire {
    Cron,
    PendingApproval,
}

/// One inbox row on the wire. `McInboxItem::detail` has no variant to carry
/// (its type is empty), so it is always `None` and does not travel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxRow {
    pub id: String,
    pub label: String,
    pub summary: String,
    #[serde(with = "McInboxKindWire")]
    pub kind: McInboxKind,
    pub source: String,
    pub created_at: DateTime<Utc>,
}

impl From<McInboxItem> for InboxRow {
    fn from(item: McInboxItem) -> Self {
        Self {
            id: item.id,
            label: item.label,
            summary: item.summary,
            kind: item.kind,
            source: item.source,
            created_at: item.created_at,
        }
    }
}

impl From<InboxRow> for McInboxItem {
    fn from(row: InboxRow) -> Self {
        Self {
            id: row.id,
            label: row.label,
            summary: row.summary,
            kind: row.kind,
            source: row.source,
            created_at: row.created_at,
            detail: None,
        }
    }
}

/// One activity row on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityRow {
    pub timestamp: Option<DateTime<Utc>>,
    pub detail: String,
    #[serde(with = "McActivityLevelWire")]
    pub level: McActivityLevel,
    pub source: String,
}

impl From<McActivity> for ActivityRow {
    fn from(a: McActivity) -> Self {
        Self {
            timestamp: a.timestamp,
            detail: a.detail,
            level: a.level,
            source: a.source,
        }
    }
}

impl From<ActivityRow> for McActivity {
    fn from(a: ActivityRow) -> Self {
        Self {
            timestamp: a.timestamp,
            detail: a.detail,
            level: a.level,
            source: a.source,
        }
    }
}

/// One schedule row on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRow {
    pub id: String,
    pub label: String,
    pub schedule: String,
    #[serde(with = "McScheduleKindWire")]
    pub kind: McScheduleKind,
    pub awaiting_user: bool,
    pub prompt: String,
    pub deliver_to: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub enabled: bool,
    pub profile_name: Option<String>,
    pub last_run_status: Option<String>,
    pub last_run_cost: Option<f64>,
    pub last_run_duration_secs: Option<i64>,
}

impl From<McScheduleItem> for ScheduleRow {
    fn from(s: McScheduleItem) -> Self {
        Self {
            id: s.id,
            label: s.label,
            schedule: s.schedule,
            kind: s.kind,
            awaiting_user: s.awaiting_user,
            prompt: s.prompt,
            deliver_to: s.deliver_to,
            last_run_at: s.last_run_at,
            next_run_at: s.next_run_at,
            created_at: s.created_at,
            enabled: s.enabled,
            profile_name: s.profile_name,
            last_run_status: s.last_run_status,
            last_run_cost: s.last_run_cost,
            last_run_duration_secs: s.last_run_duration_secs,
        }
    }
}

impl From<ScheduleRow> for McScheduleItem {
    fn from(s: ScheduleRow) -> Self {
        Self {
            id: s.id,
            label: s.label,
            schedule: s.schedule,
            kind: s.kind,
            awaiting_user: s.awaiting_user,
            prompt: s.prompt,
            deliver_to: s.deliver_to,
            last_run_at: s.last_run_at,
            next_run_at: s.next_run_at,
            created_at: s.created_at,
            enabled: s.enabled,
            profile_name: s.profile_name,
            last_run_status: s.last_run_status,
            last_run_cost: s.last_run_cost,
            last_run_duration_secs: s.last_run_duration_secs,
        }
    }
}

/// One tool's usage on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolStatRow {
    pub name: String,
    pub total: i64,
    pub failures: i64,
    pub fail_rate: f64,
}

impl From<McToolStat> for ToolStatRow {
    fn from(t: McToolStat) -> Self {
        Self {
            name: t.name,
            total: t.total,
            failures: t.failures,
            fail_rate: t.fail_rate,
        }
    }
}

impl From<ToolStatRow> for McToolStat {
    fn from(t: ToolStatRow) -> Self {
        Self {
            name: t.name,
            total: t.total,
            failures: t.failures,
            fail_rate: t.fail_rate,
        }
    }
}

/// One brain file's size on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainFileRow {
    pub name: String,
    pub kb: f64,
}

impl From<McBrainFile> for BrainFileRow {
    fn from(b: McBrainFile) -> Self {
        Self {
            name: b.name,
            kb: b.kb,
        }
    }
}

impl From<BrainFileRow> for McBrainFile {
    fn from(b: BrainFileRow) -> Self {
        Self {
            name: b.name,
            kb: b.kb,
        }
    }
}

/// One model's tool reliability on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelToolRow {
    pub model: String,
    pub total: i64,
    pub failures: i64,
    pub fail_rate: f64,
}

impl From<McModelToolStat> for ModelToolRow {
    fn from(m: McModelToolStat) -> Self {
        Self {
            model: m.model,
            total: m.total,
            failures: m.failures,
            fail_rate: m.fail_rate,
        }
    }
}

impl From<ModelToolRow> for McModelToolStat {
    fn from(m: ModelToolRow) -> Self {
        Self {
            model: m.model,
            total: m.total,
            failures: m.failures,
            fail_rate: m.fail_rate,
        }
    }
}

/// One decision tier's reuse on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRow {
    pub tier_id: String,
    pub calls: i64,
    pub would_hit: i64,
    pub live_hit: i64,
}

impl From<McDecisionStat> for DecisionRow {
    fn from(d: McDecisionStat) -> Self {
        Self {
            tier_id: d.tier_id,
            calls: d.calls,
            would_hit: d.would_hit,
            live_hit: d.live_hit,
        }
    }
}

impl From<DecisionRow> for McDecisionStat {
    fn from(d: DecisionRow) -> Self {
        Self {
            tier_id: d.tier_id,
            calls: d.calls,
            would_hit: d.would_hit,
            live_hit: d.live_hit,
        }
    }
}

/// The analytics panel's numbers on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyticsWire {
    pub tool_total_calls: i64,
    pub tool_total_fails: i64,
    pub top_tools: Vec<ToolStatRow>,
    pub flakiest_tools: Vec<ToolStatRow>,
    pub rsi_applied_total: i64,
    pub rsi_last_call_ts: Option<i64>,
    pub tool_events_since_rsi: i64,
    pub rsi_top_dimensions: Vec<(String, i64)>,
    pub brain_files: Vec<BrainFileRow>,
    pub brain_total_kb: f64,
    pub phantom_total: i64,
    pub phantom_resolved: i64,
    pub phantom_resolved_pct: f64,
    pub phantom_by_model: Vec<(String, i64, i64)>,
    pub streaming_total: i64,
    pub streaming_total_tools: i64,
    pub streaming_by_model: Vec<(String, i64)>,
    pub brain_verify_passes: i64,
    pub brain_verify_rollbacks: i64,
    pub brain_verify_fail_closed: i64,
    pub model_tools: Vec<ModelToolRow>,
    pub decisions: Vec<DecisionRow>,
}

fn into_all<A, B: From<A>>(items: Vec<A>) -> Vec<B> {
    items.into_iter().map(B::from).collect()
}

impl From<McAnalytics> for AnalyticsWire {
    fn from(a: McAnalytics) -> Self {
        Self {
            tool_total_calls: a.tool_total_calls,
            tool_total_fails: a.tool_total_fails,
            top_tools: into_all(a.top_tools),
            flakiest_tools: into_all(a.flakiest_tools),
            rsi_applied_total: a.rsi_applied_total,
            rsi_last_call_ts: a.rsi_last_call_ts,
            tool_events_since_rsi: a.tool_events_since_rsi,
            rsi_top_dimensions: a.rsi_top_dimensions,
            brain_files: into_all(a.brain_files),
            brain_total_kb: a.brain_total_kb,
            phantom_total: a.phantom.total,
            phantom_resolved: a.phantom.resolved,
            phantom_resolved_pct: a.phantom.resolved_pct,
            phantom_by_model: a.phantom.by_model,
            streaming_total: a.streaming.total,
            streaming_total_tools: a.streaming.total_tools,
            streaming_by_model: a.streaming.by_model,
            brain_verify_passes: a.brain_verify.passes,
            brain_verify_rollbacks: a.brain_verify.rollbacks,
            brain_verify_fail_closed: a.brain_verify.fail_closed,
            model_tools: into_all(a.model_tools),
            decisions: into_all(a.decisions),
        }
    }
}

impl From<AnalyticsWire> for McAnalytics {
    fn from(a: AnalyticsWire) -> Self {
        Self {
            tool_total_calls: a.tool_total_calls,
            tool_total_fails: a.tool_total_fails,
            top_tools: into_all(a.top_tools),
            flakiest_tools: into_all(a.flakiest_tools),
            rsi_applied_total: a.rsi_applied_total,
            rsi_last_call_ts: a.rsi_last_call_ts,
            tool_events_since_rsi: a.tool_events_since_rsi,
            rsi_top_dimensions: a.rsi_top_dimensions,
            brain_files: into_all(a.brain_files),
            brain_total_kb: a.brain_total_kb,
            phantom: McPhantomStats {
                total: a.phantom_total,
                resolved: a.phantom_resolved,
                resolved_pct: a.phantom_resolved_pct,
                by_model: a.phantom_by_model,
            },
            streaming: McStreamingStats {
                total: a.streaming_total,
                total_tools: a.streaming_total_tools,
                by_model: a.streaming_by_model,
            },
            brain_verify: McBrainVerifyStats {
                passes: a.brain_verify_passes,
                rollbacks: a.brain_verify_rollbacks,
                fail_closed: a.brain_verify_fail_closed,
            },
            model_tools: into_all(a.model_tools),
            decisions: into_all(a.decisions),
        }
    }
}

/// Everything one Mission Control screen draws, read once, as it travels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionSnapshot {
    /// RSI proposals waiting on a decision, newest first.
    pub inbox: Vec<InboxRow>,
    /// Recent RSI activity, newest first.
    pub activity: Vec<ActivityRow>,
    /// Every cron job, enabled and paused.
    pub schedule: Vec<ScheduleRow>,
    /// Usage and reliability numbers for the analytics window.
    pub analytics: AnalyticsWire,
    /// The window those analytics were read for.
    #[serde(with = "TimeWindowWire")]
    pub window: TimeWindow,
}

impl MissionSnapshot {
    /// The four reads the terminal makes on open (`tui::app::mission_control`),
    /// given a pool: the inbox and the activity from this crab's files, the
    /// schedule and the analytics from its ledger.
    pub async fn read(pool: Pool, window: TimeWindow) -> Self {
        MissionParts::read(pool, window).await.into()
    }

    /// The panels' own types back.
    pub fn into_parts(self) -> MissionParts {
        MissionParts {
            inbox: into_all(self.inbox),
            activity: into_all(self.activity),
            schedule: into_all(self.schedule),
            analytics: self.analytics.into(),
            window: self.window,
        }
    }
}

/// A [`MissionSnapshot`] in the types the panels draw.
#[derive(Debug, Clone)]
pub struct MissionParts {
    pub inbox: Vec<McInboxItem>,
    pub activity: Vec<McActivity>,
    pub schedule: Vec<McScheduleItem>,
    pub analytics: McAnalytics,
    pub window: TimeWindow,
}

impl MissionParts {
    /// The four reads, in the panels' own types.
    pub async fn read(pool: Pool, window: TimeWindow) -> Self {
        Self {
            inbox: inbox_service::list(),
            activity: activity_service::recent(ACTIVITY_LIMIT),
            schedule: schedule_service::list(pool.clone()).await,
            analytics: analytics_service::summary(pool, window).await,
            window,
        }
    }
}

impl From<MissionParts> for MissionSnapshot {
    fn from(p: MissionParts) -> Self {
        Self {
            inbox: into_all(p.inbox),
            activity: into_all(p.activity),
            schedule: into_all(p.schedule),
            analytics: p.analytics.into(),
            window: p.window,
        }
    }
}

/// Read the Mission Control snapshot for `window`.
///
/// A ledger that does not open answers `db connect: {e:#}`, the line the
/// screen has always shown; the four reads themselves never fail, each
/// service answers empty instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionLoad {
    /// The analytics window.
    #[serde(with = "TimeWindowWire")]
    pub window: TimeWindow,
}

impl Op for MissionLoad {
    const NAME: &'static str = "mission.load";
    const SCOPE: Scope = Scope::Read;
    type Reply = MissionSnapshot;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx
            .database()
            .await
            .map_err(|e| BackendError::Failed(format!("db connect: {e:#}")))?;
        Ok(MissionSnapshot::read(db.pool().clone(), self.window).await)
    }
}

ops! { MissionDecide, MissionLoad }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};

    #[test]
    fn the_inbox_kind_crosses_the_wire() {
        for kind in [
            McInboxKind::ProposedTool,
            McInboxKind::ProposedCommand,
            McInboxKind::ProposedSkill,
        ] {
            let op = MissionDecide {
                id: "prop_tool_x".into(),
                kind,
                decision: Decision::Reject,
            };
            let json = serde_json::to_value(&op).expect("encode");
            let back: MissionDecide = serde_json::from_value(json).expect("decode");
            assert_eq!(back.kind, kind);
            assert_eq!(back.decision, Decision::Reject);
        }
    }

    #[tokio::test]
    async fn mission_load_has_parity() {
        // An empty home and a fresh ledger: no proposals, no journal, no
        // jobs. Both doors read the same nothing, through the wire mirrors.
        let gateway = Gateway::start().await;
        for window in [TimeWindow::Day, TimeWindow::All] {
            let answer = parity!(gateway, MissionLoad { window });
            let snapshot = &answer["Ok"];
            assert_eq!(snapshot["inbox"], serde_json::json!([]), "{answer}");
            assert_eq!(snapshot["activity"], serde_json::json!([]), "{answer}");
            assert_eq!(snapshot["schedule"], serde_json::json!([]), "{answer}");
        }
        forbidden!(
            gateway,
            MissionLoad {
                window: TimeWindow::Week,
            }
        );
    }

    #[test]
    fn a_snapshot_survives_the_wire_and_comes_back_whole() {
        let at = DateTime::<Utc>::from_timestamp(1_764_000_000, 0).expect("a time");
        let snapshot = MissionSnapshot {
            inbox: vec![InboxRow {
                id: "prop_tool_x".into(),
                label: "hello_tool".into(),
                summary: "repetitive work".into(),
                kind: McInboxKind::ProposedSkill,
                source: "rsi-cycle".into(),
                created_at: at,
            }],
            activity: vec![ActivityRow {
                timestamp: Some(at),
                detail: "applied".into(),
                level: McActivityLevel::Warn,
                source: "rsi".into(),
            }],
            schedule: vec![ScheduleRow {
                id: "job".into(),
                label: "nightly".into(),
                schedule: "0 3 * * *".into(),
                kind: McScheduleKind::PendingApproval,
                awaiting_user: true,
                prompt: "tidy".into(),
                deliver_to: None,
                last_run_at: Some(at),
                next_run_at: None,
                created_at: at,
                enabled: false,
                profile_name: Some("work".into()),
                last_run_status: Some("ok".into()),
                last_run_cost: Some(0.25),
                last_run_duration_secs: Some(12),
            }],
            analytics: McAnalytics {
                tool_total_calls: 9,
                brain_total_kb: 1.5,
                phantom: McPhantomStats {
                    total: 2,
                    resolved: 1,
                    resolved_pct: 50.0,
                    by_model: vec![("m".into(), 2, 1)],
                },
                ..McAnalytics::default()
            }
            .into(),
            window: TimeWindow::Month,
        };
        let json = serde_json::to_string(&snapshot).expect("encode");
        let back: MissionSnapshot = serde_json::from_str(&json).expect("decode");
        let back = back.into_parts();
        assert_eq!(back.inbox[0].kind, McInboxKind::ProposedSkill);
        assert!(back.inbox[0].detail.is_none());
        assert_eq!(back.activity[0].level, McActivityLevel::Warn);
        assert_eq!(back.schedule[0].kind, McScheduleKind::PendingApproval);
        assert_eq!(back.schedule[0].last_run_cost, Some(0.25));
        assert_eq!(back.analytics.tool_total_calls, 9);
        assert_eq!(
            back.analytics.phantom.by_model,
            vec![("m".to_string(), 2, 1)]
        );
        assert_eq!(back.window, TimeWindow::Month);
    }

    #[tokio::test]
    async fn mission_decide_has_parity() {
        // The gateway crab has no agent service, so both doors answer
        // `NotReady` and nothing is written: the parity is in the refusal.
        let gateway = Gateway::start().await;
        let answer = parity!(
            gateway,
            MissionDecide {
                id: "prop_tool_missing".into(),
                kind: McInboxKind::ProposedTool,
                decision: Decision::Reject,
            }
        );
        assert_eq!(answer, serde_json::json!({ "Err": { "kind": "not_ready" } }));

        forbidden!(
            gateway,
            MissionDecide {
                id: "prop_tool_missing".into(),
                kind: McInboxKind::ProposedTool,
                decision: Decision::Apply,
            }
        );
    }

    fn decide(decision: Decision) -> MissionDecide {
        MissionDecide {
            id: "prop_tool_missing".into(),
            kind: McInboxKind::ProposedTool,
            decision,
        }
    }

    fn caller(scopes: &[Scope]) -> crate::backend::door::Caller {
        crate::backend::door::Caller {
            principal: "api_key".into(),
            scopes: scopes.to_vec(),
        }
    }

    /// Installing a tool by the remote door needs `a2a:admin`, which no wire
    /// credential carries: the owner's own key is refused before anything
    /// is read or written.
    #[tokio::test]
    async fn a_remote_apply_needs_admin() {
        let gateway = Gateway::start().await;
        let wire = caller(&[Scope::Read, Scope::Send, Scope::Manage]);
        let answer =
            crate::backend::door::remote_as(wire, gateway.local(decide(Decision::Apply))).await;
        assert_eq!(
            answer,
            Err(BackendError::Forbidden {
                scope: Scope::Admin
            })
        );
    }

    /// `Reject` stays at `manage` by the remote door: it reaches the crab,
    /// which here has no service and answers `NotReady`.
    #[tokio::test]
    async fn a_remote_reject_needs_only_manage() {
        let gateway = Gateway::start().await;
        let manage = caller(&[Scope::Manage]);
        let answer =
            crate::backend::door::remote_as(manage, gateway.local(decide(Decision::Reject))).await;
        assert_eq!(answer, Err(BackendError::NotReady));
    }

    /// A remote caller that does hold `a2a:admin` gets past the check.
    #[tokio::test]
    async fn a_remote_apply_with_admin_reaches_the_crab() {
        let gateway = Gateway::start().await;
        let admin = caller(&[Scope::Manage, Scope::Admin]);
        let answer =
            crate::backend::door::remote_as(admin, gateway.local(decide(Decision::Apply))).await;
        assert_eq!(answer, Err(BackendError::NotReady));
    }

    /// The local door applies as it always did: no scope is asked.
    #[tokio::test]
    async fn a_local_apply_is_not_refused() {
        let gateway = Gateway::start().await;
        let answer = gateway.local(decide(Decision::Apply)).await;
        assert_eq!(answer, Err(BackendError::NotReady));
    }
}
