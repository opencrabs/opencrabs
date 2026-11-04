//! Fire-and-forget feedback-ledger writes.
//!
//! Extracted from `tool_loop.rs` (was lines 137-181) as part of the
//! 2026-05-04 Linor-flagged refactor: `tool_loop.rs` was 4,047 lines.
//! Both methods spawn a detached task that writes to the
//! `FeedbackLedgerRepository`; neither blocks the caller and write
//! failures only log at `debug!`. This is the RSI feedback signal
//! source — the data the hourly RSI loop later analyses for tool /
//! provider success patterns.
//!
//! Behaviour is unchanged from the pre-extraction version. Lives in
//! the same `impl AgentService { ... }` block as the rest of the
//! service so call sites inside `tool_loop.rs` keep using
//! `self.record_tool_feedback(...)` / `self.record_provider_feedback(...)`
//! exactly as before.

use super::builder::AgentService;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Sentinel dimensions recorded by diagnostics (phantom-call detection,
/// regression probes) that ride the same `tool_`-prefixed event types but
/// are not real tools. Mirrors `SENTINEL_DIMENSIONS` in `brain/rsi.rs`;
/// kept as a local copy so the feedback module does not depend on the RSI
/// subsystem (rsi.rs is feature-gated, this is not).
const NON_TOOL_DIMENSIONS: &[&str] = &[
    "phantom_intent_loop",
    "phantom_tool_call",
    "self_improve_exact_match_fail",
    "sticky_fallback_regression",
    "thinking_persistence_qwen36",
    "", // empty tool name (internal bookkeeping)
];

/// Window over which tool health is judged (#1706). Matches the 7-day
/// window the RSI analysis uses: long enough to span intermittent use,
/// short enough that a fixed tool stops being flagged a week after it
/// was repaired.
pub(super) const TOOL_HEALTH_WINDOW_DAYS: i64 = 7;

/// Below this many recorded attempts a tool is not flagged. One failure in
/// one session is noise; the issue's evidence tools had 6 and 14 attempts.
pub(crate) const TOOL_HEALTH_MIN_SAMPLES: i64 = 5;

/// A tool succeeding less than half its recent attempts gets the warning.
pub(crate) const TOOL_HEALTH_MAX_RATE: f64 = 0.5;

/// How long a health snapshot is served before a refresh is spawned. The
/// request path must stay sync and DB-free, so staleness up to this bound
/// is the accepted trade; tool health changes on the scale of turns, not
/// milliseconds.
pub(super) const TOOL_HEALTH_REFRESH_INTERVAL: Duration = Duration::from_secs(300);

/// Cached tool-health annotations, keyed by tool name. `fetched_at: None`
/// means no successful fetch has happened yet (fresh process, or the DB
/// has been unreachable since start).
#[derive(Default)]
pub(crate) struct ToolHealthCache {
    fetched_at: Option<Instant>,
    annotations: HashMap<String, String>,
}

/// Pure builder from ledger stats to annotation strings. A tool is flagged
/// when it has at least `TOOL_HEALTH_MIN_SAMPLES` attempts in the window
/// AND its success rate is below `TOOL_HEALTH_MAX_RATE`. Sentinel
/// dimensions are excluded so diagnostic bookkeeping can never flag a real
/// tool (nor be flagged itself).
pub(crate) fn build_tool_health_annotations(
    stats: &[crate::db::repository::feedback_ledger::DimensionStats],
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for s in stats {
        if NON_TOOL_DIMENSIONS.contains(&s.dimension.as_str()) {
            continue;
        }
        if s.total_events < TOOL_HEALTH_MIN_SAMPLES || s.success_rate >= TOOL_HEALTH_MAX_RATE {
            continue;
        }
        out.insert(s.dimension.clone(), health_annotation_text(s));
    }
    out
}

/// The model-facing suffix appended to a flagged tool's description. Terse,
/// factual, and actionable: state the numbers, name the window, tell the
/// model what to do about it. Never says "disabled" (nothing is disabled)
/// and never speculates about WHY the tool fails.
fn health_annotation_text(s: &crate::db::repository::feedback_ledger::DimensionStats) -> String {
    let successes = s.successes.max(0);
    let total = s.total_events.max(0);
    if successes == 0 {
        format!(
            " [TOOL HEALTH WARNING: 0/{total} recent calls succeeded in the last \
             {TOOL_HEALTH_WINDOW_DAYS} days; this tool appears broken, prefer an \
             alternative tool if one exists]"
        )
    } else {
        format!(
            " [TOOL HEALTH WARNING: only {successes}/{total} recent calls succeeded \
             in the last {TOOL_HEALTH_WINDOW_DAYS} days; verify this tool works \
             before relying on it]"
        )
    }
}

/// Enrich the metadata snippet for tool failures so RSI / SQL
/// analyses can categorize them by subsystem. Without this every
/// `bash` failure lands in the same dimension and the question
/// "which kind of bash fails most?" has no answer (issue #132).
///
/// Today this only enriches `bash`: we append `| cmd=<text>` so
/// queries like `WHERE meta LIKE '%cmd=git%'` work. The command
/// is truncated independently of the error snippet so a giant
/// command line can't crowd out the error itself; the full meta
/// then gets the 500-char outer cap below.
///
/// A bash FAILURE additionally leads with `class=` and `exit=` and
/// ends with the head of stderr rather than the raw snippet (#1068),
/// so `tool_failure|bash` can be split by hand or by the RSI pass
/// without a new event type. Ordering is load-bearing: the raw
/// snippet carries captured stdout and routinely runs past the
/// 500-char cap, which silently truncated away the `cmd=` this
/// function exists to add. Short discriminators go first so the cap
/// can only ever eat the least useful tail.
pub(crate) fn enrich_metadata(
    tool_name: &str,
    error_snippet: Option<&str>,
    tool_input: Option<&serde_json::Value>,
) -> Option<String> {
    let snippet = error_snippet.unwrap_or("");
    if tool_name == "bash" && !snippet.is_empty() {
        return Some(bash_failure_metadata(snippet, tool_input));
    }
    let cmd_suffix = if tool_name == "bash"
        && let Some(input) = tool_input
        && let Some(cmd) = input.get("command").and_then(|v| v.as_str())
        && !cmd.is_empty()
    {
        // Cap the command alone at 300 chars so the error snippet
        // still gets meaningful room within the outer 500-char cap.
        //
        // Enriched on BOTH success and failure so RSI's
        // success-pattern detection pass can group bash invocations
        // by subsystem (gh, git, docker, ...) — without this, only
        // failures would carry the command and RSI couldn't see
        // patterns in the (much more common) successful calls.
        let cmd_short: String = cmd.chars().take(300).collect();
        Some(format!(" | cmd={cmd_short}"))
    } else {
        None
    };
    match (snippet.is_empty(), cmd_suffix) {
        (true, None) => None,
        // Strip the " | " prefix the suffix carries (used when
        // joining with a snippet). With no snippet to join against
        // we emit a bare `cmd=...` so downstream LIKE queries can
        // still match without the leading separator.
        (true, Some(suffix)) => Some(suffix.strip_prefix(" | ").unwrap_or(&suffix).to_string()),
        (false, None) => Some(snippet.to_string()),
        (false, Some(suffix)) => Some(format!("{snippet}{suffix}")),
    }
}

/// How much stderr to carry. 80 chars, as first sketched, cuts a typical git
/// or ssh diagnostic mid-sentence; 160 holds one whole line in nearly every
/// case and still leaves room under the 500-char cap for a long `cmd=`.
const STDERR_HEAD_CAP: usize = 160;

/// The metadata row for a failed bash call.
///
/// Captured stdout is deliberately dropped. `exit=` already carries the tool's
/// error line and `stderr_head=` carries the diagnostic, so what stdout adds to
/// a FAILURE row is bulk that pushes the fields above out past the cap.
fn bash_failure_metadata(snippet: &str, tool_input: Option<&serde_json::Value>) -> String {
    use crate::brain::bash_failure::{BashFailureKind, classify, exit_code, stderr_head};

    let class = match classify(snippet) {
        BashFailureKind::ModelError => "model_error",
        BashFailureKind::Environmental => "environmental",
        BashFailureKind::Unknown => "unknown",
    };
    let mut meta = format!("class={class}");
    if let Some(code) = exit_code(snippet) {
        meta.push_str(&format!(" | exit={code}"));
    }
    if let Some(cmd) = tool_input
        .and_then(|i| i.get("command"))
        .and_then(|v| v.as_str())
        .filter(|c| !c.is_empty())
    {
        let cmd_short: String = cmd.chars().take(300).collect();
        meta.push_str(&format!(" | cmd={cmd_short}"));
    }
    // No stderr section means the command wrote its diagnostic to stdout (or
    // nowhere). Fall back to the snippet head so the row is not left with only
    // a class and an exit code to explain itself.
    match stderr_head(snippet, STDERR_HEAD_CAP) {
        Some(head) => meta.push_str(&format!(" | stderr_head={head}")),
        None => {
            let head: String = snippet.trim().chars().take(STDERR_HEAD_CAP).collect();
            if !head.is_empty() {
                meta.push_str(&format!(" | {head}"));
            }
        }
    }
    meta
}

impl AgentService {
    /// Fire-and-forget recording of a tool execution to the feedback ledger.
    /// Never blocks, never fails visibly — if the DB is unavailable or the
    /// write fails we just log and move on.
    ///
    /// `tool_input` is the JSON the agent passed to the tool. We use it
    /// to enrich failure metadata for subsystem-specific analysis (e.g.
    /// appending `cmd=...` for bash failures so RSI can group by
    /// `git` / `python` / `docker` prefixes). Pass `None` when the
    /// call site doesn't have a meaningful input (e.g. user-denial
    /// before execution).
    /// Tool-health annotations for the schema-build path (#1706). Returns
    /// the cached map, spawning a fire-and-forget ledger refresh when the
    /// snapshot is older than `TOOL_HEALTH_REFRESH_INTERVAL` (or missing).
    /// The current call always gets the previous snapshot: the request path
    /// is sync and must not block on the DB, and health data moving one
    /// turn late is harmless. DB errors leave the cache untouched and only
    /// log at debug, same contract as `record_tool_feedback`.
    pub(super) fn tool_health_annotations(&self) -> HashMap<String, String> {
        let stale = {
            let cache = self.tool_health_cache.read().unwrap();
            cache
                .fetched_at
                .map(|t| t.elapsed() >= TOOL_HEALTH_REFRESH_INTERVAL)
                .unwrap_or(true)
        };
        if stale {
            let pool = self.context.pool();
            let cache_handle = std::sync::Arc::clone(&self.tool_health_cache);
            tokio::spawn(async move {
                let repo = crate::db::repository::FeedbackLedgerRepository::new(pool);
                let window_since = (chrono::Utc::now()
                    - chrono::Duration::days(TOOL_HEALTH_WINDOW_DAYS))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
                match repo
                    .stats_by_dimension_since("tool_", Some(&window_since))
                    .await
                {
                    Ok(stats) => {
                        let annotations = build_tool_health_annotations(&stats);
                        let mut cache = cache_handle.write().unwrap();
                        cache.fetched_at = Some(Instant::now());
                        cache.annotations = annotations;
                    }
                    Err(e) => {
                        tracing::debug!("tool health refresh failed: {e}");
                    }
                }
            });
        }
        self.tool_health_cache.read().unwrap().annotations.clone()
    }

    pub(super) fn record_tool_feedback(
        &self,
        session_id: Uuid,
        tool_name: &str,
        tool_input: Option<&serde_json::Value>,
        success: bool,
        error_snippet: Option<&str>,
    ) {
        let pool = self.context.pool();
        let sid = session_id.to_string();
        let tname = tool_name.to_string();
        let enriched = enrich_metadata(tool_name, error_snippet, tool_input);
        let meta = enriched.map(|s| s.chars().take(500).collect::<String>());
        // A failed call is not automatically a tool DEFECT. `tool_event_type`
        // splits it three ways: a gate refusal (plan gate, skill gate, guest
        // scope, a declined approval prompt) becomes `gate_denied`, an
        // environmental outcome (stale hash, channel not connected) becomes
        // `recoverable_failure`, and only a real defect stays `tool_failure`.
        // The first two carry no `tool_` prefix, so they stay queryable but OUT
        // of the success-rate denominator. Otherwise they drove the RSI to ban
        // working built-in tools (#236) and to warn that a healthy channel was
        // 50% broken (#2251).
        let event =
            crate::brain::feedback_policy::tool_event_type(success, tool_name, error_snippet);
        tokio::spawn(async move {
            let repo = crate::db::repository::FeedbackLedgerRepository::new(pool);
            let val = if success { 1.0 } else { 0.0 };
            if let Err(e) = repo.record(&sid, event, &tname, val, meta.as_deref()).await {
                tracing::debug!("feedback ledger write failed: {e}");
            }
        });
    }

    /// Fire-and-forget recording of a pre-execution tool miss (#214): the model
    /// called a tool that didn't resolve, or with arguments that failed
    /// validation, so the tool never ran. Recorded as `discovery_miss` (NOT a
    /// `tool_`-prefixed event) so it stays OUT of the success-rate denominator
    /// in `stats_by_dimension("tool_")`, while staying visible in the
    /// event-type breakdown and queryable for "what is the model guessing
    /// blind" analysis. `dimension` is the tool name.
    pub(super) fn record_tool_discovery_miss(
        &self,
        session_id: Uuid,
        tool_name: &str,
        tool_input: Option<&serde_json::Value>,
        error_snippet: Option<&str>,
    ) {
        let pool = self.context.pool();
        let sid = session_id.to_string();
        let tname = tool_name.to_string();
        let enriched = enrich_metadata(tool_name, error_snippet, tool_input);
        let meta = enriched.map(|s| s.chars().take(500).collect::<String>());
        tokio::spawn(async move {
            let repo = crate::db::repository::FeedbackLedgerRepository::new(pool);
            if let Err(e) = repo
                .record(&sid, "discovery_miss", &tname, 0.0, meta.as_deref())
                .await
            {
                tracing::debug!("feedback ledger write failed: {e}");
            }
        });
    }

    /// Fire-and-forget recording of a provider error to the feedback ledger.
    pub(super) fn record_provider_feedback(
        &self,
        session_id: Uuid,
        event_type: &str,
        dimension: &str,
        metadata: Option<&str>,
    ) {
        let pool = self.context.pool();
        let sid = session_id.to_string();
        let et = event_type.to_string();
        let dim = dimension.to_string();
        let meta = metadata.map(|s| s.chars().take(500).collect::<String>());
        tokio::spawn(async move {
            let repo = crate::db::repository::FeedbackLedgerRepository::new(pool);
            if let Err(e) = repo.record(&sid, &et, &dim, 0.0, meta.as_deref()).await {
                tracing::debug!("feedback ledger write failed: {e}");
            }
        });
    }
}
