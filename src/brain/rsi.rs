//! RSI (Recursive Self-Improvement) background engine.
//!
//! Runs as a background task after startup:
//! 1. Writes a digest of feedback_ledger stats to `~/.opencrabs/rsi/digest.md`
//! 2. Periodically analyzes feedback and applies improvements autonomously
//! 3. Emits TUI notifications when improvements are applied
//!
//! Uses the provider/model configured in `[agent].self_improvement_provider`
//! and `[agent].self_improvement_model`, falling back to the active provider.

use crate::config::Config;
use crate::db::repository::FeedbackLedgerRepository;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::Instrument;

/// Base interval between RSI cycles (analyze + improve) — the first rung
/// of the backoff ladder (#977).
const RSI_CYCLE_INTERVAL_SECS: u64 = 3600; // 1 hour

/// Backoff ladder for the cycle interval (#977): each consecutive agent
/// run that applied nothing pushes the next cycle one rung out —
/// 1h -> 4h -> 12h -> 24h. The moment an improvement actually applies,
/// the streak resets to the first rung (see the loop below). The fixed
/// hourly interval was part of the burn: an install with zero
/// improvements since 2026-07-30 still got polled every hour.
const RSI_BACKOFF_LADDER_SECS: &[u64] = &[RSI_CYCLE_INTERVAL_SECS, 4 * 3600, 12 * 3600, 24 * 3600];

/// Cycle interval for a given zero-improvement streak (#977). Streak 0 =
/// base 1h; each consecutive agent run that applies nothing climbs one
/// rung; capped at the last rung (24h).
/// Effective RSI enablement, the #1063 master gate. An explicit
/// `rsi_enabled` in config.toml always wins. When the key is absent the
/// default is by run mode: ON for the interactive TUI (the feature users see
/// and expect), OFF for headless daemons, where unattended hourly cycles
/// with auto-approved tools burned provider quota and were read as hangs.
pub(crate) fn rsi_effectively_enabled(config: &Config, headless: bool) -> bool {
    config.agent.rsi_enabled.unwrap_or(!headless)
}

pub(crate) fn rsi_interval_for_streak(streak: u64) -> u64 {
    let idx = (streak as usize).min(RSI_BACKOFF_LADDER_SECS.len() - 1);
    RSI_BACKOFF_LADDER_SECS[idx]
}

/// Minimum feedback entries before RSI attempts improvements.
const RSI_MIN_ENTRIES: i64 = 50;

/// Max tool iterations for the RSI agent (keep it focused).
const RSI_MAX_TOOL_ITERATIONS: usize = 10;

/// Consecutive converged agent cycles after which agent runs pause (#977).
/// Two in a row, not one: a single oddly-phrased summary must not pause
/// the engine, but the live transcript showed the model saying "nothing
/// new" 46 times in 9 days, so real convergence always repeats.
const RSI_CONVERGENCE_PAUSE_AFTER: u64 = 2;

/// Sentinel dimensions that fire as "failures" by design (self-heal
/// detectors, regression probes). Excluded from opportunity surfacing so
/// they don't show up as noise like "phantom_intent_loop has 100% failure".
/// Shared by the digest Metrics block and the cycle opportunity filter so
/// the two stay in sync.
const SENTINEL_DIMENSIONS: &[&str] = &[
    "phantom_intent_loop",
    "phantom_tool_call",
    "self_improve_exact_match_fail",
    "sticky_fallback_regression",
    "thinking_persistence_qwen36",
    "", // empty tool name (internal bookkeeping)
];

/// Ensure `~/.opencrabs/rsi/` and `~/.opencrabs/rsi/history/` exist.
fn ensure_rsi_dirs() -> std::io::Result<PathBuf> {
    let home = crate::config::opencrabs_home();
    let rsi_dir = home.join("rsi");
    let history_dir = rsi_dir.join("history");
    std::fs::create_dir_all(&history_dir)?;
    Ok(rsi_dir)
}

/// SHA-256 hex digest of the cycle's FINDING IDENTITIES (#977). Used to
/// detect cycle-over-cycle telemetry stability so we don't re-emit the
/// same corrections / errors / tool-failure blocks when nothing
/// meaningful has changed.
///
/// v1 hashed the description bodies. #804 stripped the `- session=...`
/// example lines, but the bodies still carry churning counts ("34%
/// (12 of 35)", "17 successful invocations", "typed 6 times"), sample
/// invocations and severity-ordered top-N slices, so the hash still
/// moved on essentially every busy cycle and the gate never fired.
///
/// v2 hashes one stable identity key per finding instead: dimension,
/// subsystem, request signature, tool sequence. Counts, samples and
/// ordering are illustration for the agent once it runs — they are not
/// part of whether the finding is new. Keys are sorted before joining
/// (top-N reordering is not a change) and flattened to single lines so
/// no key can contain the join sentinel; joining with `\n---\n` then
/// keeps two adjacent keys from collapsing into the same hash as one
/// merged key.
///
/// The persisted `rsi/last_opportunities_hash` needs no migration: it
/// holds an opaque hex string, so the first cycle after upgrade simply
/// sees a mismatch, runs once, and rewrites it in the new scheme.
pub(crate) fn hash_opportunities(keys: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut keys: Vec<String> = keys.iter().map(|k| k.replace('\n', " ")).collect();
    keys.sort();
    let mut hasher = Sha256::new();
    hasher.update(keys.join("\n---\n").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Whether an RSI agent summary reports "nothing new to do" or echoes a
/// user stop instruction (#977). Both mean the next cycle would burn a
/// paid turn to say the same thing. Consecutive occurrences pause agent
/// runs until the finding set actually changes (see the loop below); the
/// engine and digest keep running.
///
/// Markers come from the live transcript: "Same data. Stopping." and
/// "Converged. No improvements applied." alternated with "Retired. No
/// further RSI action taken" 46 times over 9 days, each a full paid
/// turn. Matching is lowercase-contains, deliberately loose — the model
/// rephrases, the meaning doesn't change.
pub(crate) fn summary_signals_convergence(summary: &str) -> bool {
    let s = summary.to_lowercase();
    const MARKERS: &[&str] = &[
        // Model self-reports of "nothing to do".
        "same data",
        "no improvements applied",
        "no improvements were applied",
        "nothing to improve",
        "nothing new to improve",
        "converged",
        "retired",
        "no further rsi action",
        // User stop instructions echoed back by the agent.
        "stop rsi",
        "stopping rsi",
        "stopped rsi",
        "stand down",
    ];
    MARKERS.iter().any(|m| s.contains(m))
}

/// Write the startup digest to `~/.opencrabs/rsi/digest.md`.
/// Called once at boot after DB is ready.
pub async fn write_startup_digest(pool: crate::db::Pool) {
    let repo = FeedbackLedgerRepository::new(pool);
    let total = match repo.total_count().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("RSI digest: failed to query feedback_ledger: {e}");
            return;
        }
    };

    if total == 0 {
        tracing::debug!("RSI digest: no feedback data yet, skipping");
        return;
    }

    let rsi_dir = match ensure_rsi_dirs() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("RSI digest: failed to create rsi dir: {e}");
            return;
        }
    };

    let mut out = format!(
        "# RSI Digest\n\n**Generated:** {}\n**Total events:** {total}\n\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
    );

    // RSI Metrics — surface raw failure totals vs surfaced opportunities on
    // the same 7-day window the opportunity filter uses. Without this, a
    // reader sees `tool_failure: N` and `K opportunities` with no bridge
    // between them, which produced a false "RSI underreports failures ~25x"
    // report (raw count vs noise-filtered surface). #888
    {
        let window_since = (chrono::Utc::now() - chrono::Duration::days(7))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        if let Ok(stats) = repo
            .stats_by_dimension_since("tool_", Some(&window_since))
            .await
        {
            let total_failures: i64 = stats
                .iter()
                .filter(|s| !SENTINEL_DIMENSIONS.contains(&s.dimension.as_str()))
                .map(|s| s.failures)
                .sum();
            let tools_with_failures = stats
                .iter()
                .filter(|s| s.failures > 0 && !SENTINEL_DIMENSIONS.contains(&s.dimension.as_str()))
                .count();
            let surfaced = stats
                .iter()
                .filter(|s| {
                    s.total_events >= 5
                        && s.success_rate < 0.8
                        && !SENTINEL_DIMENSIONS.contains(&s.dimension.as_str())
                })
                .count();
            out.push_str("## RSI Metrics (last 7 days)\n\n");
            out.push_str(&format!(
                "- **Raw tool_failure events:** {total_failures}\n\
                 - **Tools with failures:** {tools_with_failures}\n\
                 - **Surfaced as opportunities:** {surfaced} \
                 (filter: >=5 events, <80% success rate, sentinel dimensions excluded)\n\n"
            ));
            out.push_str(
                "_Note: `recoverable_failure`, `gate_denied` and `discovery_miss` are \
                 classified separately and excluded from the success-rate denominator by \
                 design (#236, #2251). The raw count above is the total, not the filtered \
                 opportunity surface._\n\n",
            );
        }
    }

    // Event type breakdown
    if let Ok(summary) = repo.summary().await {
        out.push_str("## Event Breakdown\n\n");
        for (event_type, count) in &summary {
            let pct = (*count as f64 / total as f64) * 100.0;
            out.push_str(&format!("- **{event_type}**: {count} ({pct:.1}%)\n"));
        }
        out.push('\n');
    }

    // Tool stats with failure rates
    if let Ok(stats) = repo.stats_by_dimension("tool_").await {
        let failing: Vec<_> = stats.iter().filter(|s| s.failures > 0).collect();
        if !failing.is_empty() {
            out.push_str("## Tool Performance\n\n");
            out.push_str("| Tool | Total | OK | Fail | Rate |\n");
            out.push_str("|------|------:|---:|-----:|-----:|\n");
            for s in &failing {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {:.0}% |\n",
                    s.dimension,
                    s.total_events,
                    s.successes,
                    s.failures,
                    s.success_rate * 100.0
                ));
            }
            out.push('\n');
        }
    }

    // Recent failures
    if let Ok(entries) = repo.by_event_type("tool_failure", 10).await
        && !entries.is_empty()
    {
        out.push_str("## Recent Failures\n\n");
        for e in &entries {
            let meta = e.metadata.as_deref().unwrap_or("(no details)");
            let short: String = meta.chars().take(120).collect();
            out.push_str(&format!(
                "- `{}` — {} — {}\n",
                e.created_at.format("%Y-%m-%d %H:%M"),
                e.dimension,
                short
            ));
        }
        out.push('\n');
    }

    // User corrections
    if let Ok(corrections) = repo.by_event_type("user_correction", 10).await
        && !corrections.is_empty()
    {
        out.push_str("## User Corrections\n\n");
        for c in &corrections {
            let meta = c.metadata.as_deref().unwrap_or("(no details)");
            let short: String = meta.chars().take(120).collect();
            out.push_str(&format!(
                "- `{}` — {} — {}\n",
                c.created_at.format("%Y-%m-%d %H:%M"),
                c.dimension,
                short
            ));
        }
        out.push('\n');
    }

    // Applied improvements
    if let Ok(improvements) = repo.by_event_type("improvement_applied", 10).await
        && !improvements.is_empty()
    {
        out.push_str("## Applied Improvements\n\n");
        for imp in &improvements {
            out.push_str(&format!(
                "- `{}` — {}\n",
                imp.created_at.format("%Y-%m-%d %H:%M"),
                imp.dimension
            ));
        }
        out.push('\n');
    }

    let digest_path = rsi_dir.join("digest.md");
    match std::fs::File::create(&digest_path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(out.as_bytes()) {
                tracing::warn!("RSI digest: failed to write: {e}");
            } else {
                tracing::info!(
                    "RSI digest written to {} ({total} events)",
                    digest_path.display()
                );
            }
        }
        Err(e) => tracing::warn!("RSI digest: failed to create file: {e}"),
    }
}

/// Notification message from the RSI engine to TUI/channels.
#[derive(Debug, Clone)]
pub enum RsiNotification {
    /// RSI cycle started
    CycleStarted,
    /// Digest written at startup
    DigestWritten { total_events: i64 },
    /// Template sync completed (upstream brain files updated)
    TemplateSyncComplete { summary: String },
    /// Template sync failed
    TemplateSyncFailed { error: String },
    /// An improvement was identified and needs agent execution
    ImprovementOpportunity { description: String },
    /// Autonomous agent completed an improvement cycle
    AgentCycleComplete { summary: String },
    /// Autonomous agent failed
    AgentCycleFailed { error: String },
}

/// Format an RSI notification into its TUI display line, with secrets
/// redacted.
///
/// The `error`, `summary`, and `description` fields are free text sourced
/// from provider errors, feedback records, and tool output — any of which
/// can contain an API key, Bearer token, or credentialed URL. Without
/// redaction these surfaced on screen (2026-06-07). `redact_secrets` masks
/// key prefixes, long opaque tokens, inline "Bearer <token>", and
/// env-style secret assignments. Redaction happens here, at the single
/// formatting point, so every variant and every caller is covered.
pub(crate) fn format_rsi_notification(notification: &RsiNotification) -> String {
    let msg = match notification {
        RsiNotification::DigestWritten { total_events } => {
            format!("RSI: digest written ({total_events} events)")
        }
        RsiNotification::CycleStarted => "RSI: analyzing feedback patterns...".to_string(),
        RsiNotification::ImprovementOpportunity { description } => {
            format!("RSI: {description}")
        }
        RsiNotification::AgentCycleComplete { summary } => {
            format!("RSI: agent cycle complete — {summary}")
        }
        RsiNotification::AgentCycleFailed { error } => {
            format!("RSI: agent cycle failed — {error}")
        }
        RsiNotification::TemplateSyncComplete { summary } => {
            format!("RSI: template sync complete — {summary}")
        }
        RsiNotification::TemplateSyncFailed { error } => {
            format!("RSI: template sync failed — {error}")
        }
    };
    // Compose both redactors: redact_command catches command/URL patterns
    // (api_key= query params, --header secrets, https://user:pass@ URLs)
    // that RSI provider errors commonly carry; redact_secrets then masks
    // key prefixes, long opaque tokens, and inline Bearer values. Each is
    // a no-op on text the other already masked.
    let command_safe = crate::utils::sanitize::redact_command(&msg);
    crate::utils::sanitize::redact_secrets(&command_safe)
}

/// Build a minimal tool registry containing only the RSI tools.
/// Does the RSI session's stored pair disagree with what config selects (#805)?
///
/// The session outlives the config, and `ensure_session_provider_restored`
/// (#704) restores the SESSION's saved provider at turn start. Right for a user
/// session; wrong for this one, where config is the authority. Without this
/// check a session created months ago re-pins its original provider on every
/// cycle and `self_improvement_provider` has no effect at all.
///
/// An unset `self_improvement_model` is not a disagreement: the provider's own
/// default is intended, so the stored model is left alone rather than cleared.
pub(crate) fn rsi_pair_is_stale(
    session_provider: Option<&str>,
    session_model: Option<&str>,
    configured_provider: &str,
    configured_model: Option<&str>,
) -> bool {
    if session_provider != Some(configured_provider) {
        return true;
    }
    matches!(configured_model, Some(m) if session_model != Some(m))
}

fn build_rsi_tool_registry() -> Arc<crate::brain::tools::ToolRegistry> {
    use crate::brain::tools::ToolRegistry;
    use crate::brain::tools::feedback_analyze::FeedbackAnalyzeTool;
    use crate::brain::tools::feedback_record::FeedbackRecordTool;
    use crate::brain::tools::rsi_propose::RsiProposeTool;
    use crate::brain::tools::self_improve::SelfImproveTool;

    let registry = ToolRegistry::new();
    registry.register(Arc::new(FeedbackRecordTool));
    registry.register(Arc::new(FeedbackAnalyzeTool));
    registry.register(Arc::new(SelfImproveTool));
    // rsi_propose lets the loop file tool/command proposals to the inbox.
    // Apply path goes through rsi_proposals (user-facing), not RSI.
    registry.register(Arc::new(RsiProposeTool));
    Arc::new(registry)
}

/// The system prompt for the RSI agent.
pub(crate) const RSI_AGENT_PROMPT: &str = "\
You are the RSI (Recursive Self-Improvement) engine for OpenCrabs. \
Your job is to analyze system feedback and autonomously apply improvements to brain files.

## Analysis Steps

1. Call feedback_analyze with query='summary' to see overall system stats.
2. Call feedback_analyze with query='tool_stats' to identify tools with high failure rates.
3. Call feedback_analyze with query='failures' to see recent failure details.
4. Call feedback_analyze with query='recent' to see the latest events (including self-heal triggers).
5. For each actionable problem, call self_improve to apply a targeted fix, \
   or rsi_propose when the gap is a missing capability rather than missing guidance.
6. Be conservative: only apply improvements when you have clear evidence from the feedback data.
7. Focus on the highest-impact issues first (highest failure rate, most frequent corrections).

## Two Ways To Improve: Guidance vs Capability

- **self_improve** changes how the agent BEHAVES with what it already has: a rule, a routing \
  preference, a correction recorded in a brain file. Use it when the ability exists and only \
  direction is missing.
- **rsi_propose** asks for something the agent CANNOT currently do: a new tool, slash command, \
  or skill. Use it when no rule could fix the finding because the capability itself is absent.

Writing a rule for a missing capability does not work, and stacking such rules is the prompt \
bloat warned about below. Discarding the finding is equally wrong: it is real, and proposing \
is how it gets addressed.

Proposals are NOT installed. They go to the user's inbox for review, so a proposal is a \
suggestion with evidence attached, never a change you made.

Apply the same bar as everything else here: propose from a REPEATED pattern in the feedback \
data, never from a single occurrence. A noisy inbox is as useless as an empty one.

## Tool-Failure Triage (ask binary questions before acting)

A high failure rate is NOT sufficient reason to act. Before writing ANY rule about a tool, \
answer these yes/no questions from the feedback data and act ONLY when the answers say a \
prompt change will actually help:

1. **Enough evidence?** Skip any tool with fewer than 5 recorded calls — a 0/1 or 1/2 sample \
   is noise, not a pattern.
2. **Real defect, or recoverable/environmental?** Stale-hash 'file may have changed' retries \
   (hashline_edit), 'not connected' (channel send tools), and cancelled/timed-out prompts \
   (user-declined prompts) are EXPECTED outcomes, not defects. They are already kept out of the \
   success-rate denominator — never treat them as failures or write rules about them.
3. **Misuse, or broken?** If the failures are the agent calling the tool wrong (bad params, \
   unknown action), the fix is concise USAGE guidance — never avoiding the tool.
4. **Capability, or guidance?** Prompt rules only help when the model HAS the ability and just \
   needs direction. If the failure reflects a hard limitation (the tool genuinely cannot do X), \
   a rule won't fix it and only adds noise — do NOT write one. This is the case rsi_propose \
   exists for: propose the missing capability instead of discarding the finding.

**NEVER tell the agent to avoid, ban, stop using, or 'DO NOT USE' a BUILT-IN tool.** Built-in \
tools are part of the system; banning one removes capability instead of fixing anything, and \
the self_improve guard will reject it. At most write routing guidance ('prefer X over Y for \
case Z'). Only a USER-DEFINED (tools.toml) tool may be disabled.

**Avoid prompt bloat.** Do not bump violation counters, restate an existing rule, or stack \
competing instructions across runs — accumulated lessons degrade the brain into contradictions. \
One clear rule per problem; refine in place (action='update') rather than appending near-duplicates. \
If a rule already covers the problem, leave it.

## Target File Taxonomy

Each brain file controls a different aspect of the agent. Route improvements to the RIGHT file:

- **SOUL.md** — PERSONALITY / voice: response style, tone, reasoning patterns. \
  Fix here when: phantom_tool_call events (model narrates instead of acting), gaslighting \
  preambles, verbose/repetitive responses, wrong tone. \
  NOT the hard rules / safety gates — those go in AGENTS.md (always-loaded).
- **TOOLS.md** — Tool DEFINITIONS: parameter formats, executor types, usage docs. \
  This is a reference file, NOT a dumping ground for failure logs or error notes. \
  Tool failure patterns are tracked by the feedback system (feedback_record, feedback_analyze). \
  Do NOT append error handling guidance, failure counts, or incident logs here. \
  Only edit TOOLS.md when a tool's actual definition or usage docs need updating.
- **USER.md** — How to interact with THIS USER: preferences, corrections, frustrations. \
  Fix here when: user_correction events show a repeated preference the agent keeps violating.
- **MEMORY.md** — Persistent KNOWLEDGE: facts, context, project state, integrations. \
  Fix here when: the agent repeatedly lacks context it should have retained across sessions.
- **AGENTS.md** — Workspace PROCESS + the **enforced hard rules / safety gates** (never \
  delete/push/email/post without approval). It is ALWAYS-LOADED, so any must-always-respect \
  rule a user/feedback teaches goes HERE — never in an on-demand file (MEMORY/TOOLS/CODE) where \
  it wouldn't be enforced on a cold session or after compaction. \
  Fix here when: workspace/process behavior needs adjustment, or a new hard rule is learned. \
  NOT security policy (→ SECURITY.md), NOT personality/tone (→ SOUL.md).
- **CODE.md** — Coding standards, testing, and the user's language/framework preference. \
  Fix here when: code-quality feedback recurs (wrong style, missing tests, bad patterns).
- **SECURITY.md** — Security policy: code review, network posture, data handling, credential/server access. \
  Fix here when: security-related feedback appears.
- **BOOT.md** — Startup + runtime self-maintenance: boot steps, memory-save triggers, upgrade/evolve, \
  running as a service. Fix here when: startup/persistence guidance or the memory-save triggers \
  need updating.

One kind of content per file — never duplicate a rule across files (copies drift and go stale), \
and match each file's `**Owns:**` header. SOUL/AGENTS/CODE/TOOLS/SECURITY/BOOT are generic (same for \
everyone); USER/MEMORY are user-specific.

### Custom Reference Files

Additional `.md` files may exist alongside the core brain files (the user's own custom \
notes or skill-specific docs). These are NOT core brain files. They are user-curated reference material \
loaded on demand via `load_brain_file` for inflight context. \
You may read them for context, but do NOT autonomously write to them via self_improve. \
If feedback relates to content in a custom file, suggest the change to the user instead.

## Self-Heal Event Types

These events in the feedback ledger represent behaviors the self-heal layer had to correct at runtime. \
Your job is to write improvements that PREVENT these from recurring:

- **phantom_tool_call** — Model described file changes in prose but executed zero tool calls. \
  Self-heal injected a retry prompt. Write to SOUL.md: reinforce 'execute tools, don't narrate'.
- **user_correction** — User said 'no', 'wrong', 'try again', etc. \
  Analyze the correction content to determine if it's behavioral (SOUL), tool-usage (TOOLS), or preference (USER).
- **context_compaction** — Context exceeded budget, had to be compacted. \
  If frequent, check if the agent is loading too many brain files or being too verbose (SOUL).
- **provider_error** — Provider returned an error. Usually not actionable unless the agent is \
  sending bad requests (TOOLS) or using the wrong model.
- **tool_failure** — A specific tool failed. Use feedback_record/feedback_analyze to log \
  and review patterns. Do NOT append failure notes to TOOLS.md — it's for tool definitions only.

## Workflow — MANDATORY

1. **Read first**: Before ANY modification, call self_improve with action='read' on the target file. \
   You MUST see the current content to judge whether your improvement is new, redundant, or refines something existing.
2. **Decide action**: After reading:
   - If the file has NO existing instruction covering your improvement → use action='apply' to append.
   - If the file ALREADY has an instruction that covers the same topic but needs refinement → use action='update' with the exact old_content copied from what you just read, and your improved content in 'content'.
   - If the file already covers the topic AND the feedback shows a FRESH repeat violation (new incident since the rule was written) → use action='update' to reinforce: append the new date/incident as evidence, and tighten the wording if the model keeps slipping past it. Do NOT bump inline counters — see \"Reinforcing Repeat Violations\" below. 
   Repeat violations of an existing rule are NOT a 'covered, skip' case — they signal the rule needs reinforcement.
   - If the file already says what you want to say AND there is no fresh evidence of new violations → SKIP. Do not duplicate.
3. **Never rewrite the whole file**. The 'update' action replaces ONE specific section/paragraph. \
   The 'apply' action appends. Neither should be used to rewrite the entire file. \
   Brain files contain user-written content — you must preserve it and only add/refine specific instructions.

## Reinforcing Repeat Violations

When feedback shows the same correction pattern recurring (same dimension in user_correction or
self_heal events, same root cause), update the existing rule to document the new incident:

- Find the existing rule in the brain file via action='read'.
- Use action='update' with old_content being the exact current rule text.
- **Cap at 2 evidence entries** in the rule itself. After 2 dated entries, replace
  subsequent appends with a single inline counter: `Violations: N, last: YYYY-MM-DD`
  and increment N each time. Do NOT keep appending new date/session paragraphs.
  Full incident history lives in the feedback ledger (feedback_analyze), not the
  brain file. Two evidence entries is enough to prove recurrence; more just bloat.
- Tighten the wording if the model keeps slipping past it.

**Do NOT bump inline counters** (e.g. do NOT write `Violations: 6 → 7`). The feedback ledger SQLite
database (`feedback.db` in your OpenCrabs home) is the canonical source of truth for event counts. SOUL.md
counters are decorative and go stale — they are not read by the runtime. Only the DB is queried
by feedback_analyze and the tool_loop.rs runtime.

**Do NOT append unbounded incident logs.** Each new date/session entry looks like 'new content'
to the dedup guard (issue #197) because the timestamp is unique. This causes brain-file bloat.
Use the 2-entry cap above, then the inline counter.

Skipping a repeat-violation case because 'the rule already exists' is the most common RSI
failure mode. Don't do it. The rule existing IS the reason to reinforce — but document via
evidence appends, not counter bumps.

## Proposing New Tools / Commands (rsi_propose)

You can also propose NEW dynamic tools (`tools.toml`) or NEW slash \
commands (`commands.toml`) when feedback shows the agent worked around \
a missing capability. Use `rsi_propose` for this. You do NOT install — proposals \
land in your `rsi/proposed_*.toml` inbox. The user (or the user-facing \
agent on their behalf) reviews and applies via the `rsi_proposals` tool.

Once applied, a new slash command or skill is **discoverable automatically** — the \
agent's system prompt injects a live Available Commands & Skills index every turn \
(built from `commands.toml` + `skills/`), so you do NOT need to also document it in a \
brain file for the agent to find it. Write a clear `description` — that's what the \
agent reads to decide when to run it.

When to propose a tool (kind='tool'):
- A specific bash invocation appears repeatedly across sessions (e.g. `gh issue list`, \
  `docker ps`, a curl to a private API). Wrap it as a shell tool with named params.
- The agent calls `http_request` to the same endpoint multiple times with similar \
  payloads. Wrap it as an http tool.
- Only propose tools whose execution is safe by default (read-only verbs, \
  GET requests). Set `requires_approval=true` for anything shell-based.

EFFICIENCY GATE (required for all tool proposals):
The rationale MUST explicitly state which of these applies. If none apply, do NOT \
propose the tool:
1. TOKEN SAVINGS — wrapper eliminates boilerplate (multi-step resolution, auth headers, \
   JSON construction, repeated argument patterns)
2. ERROR REDUCTION — wrapper prevents a known class of failures (quoting bugs, escaping \
   issues, parameter validation, environment setup)
3. CAPABILITY ADDITION — wrapper enables something bash cannot do alone (structured output \
   parsing, protocol handling, binary data processing)

Pure passthrough wrappers (e.g. `ssh_exec` that just wraps `bash ssh user@host 'cmd'`) \
fail this gate: same token cost, no error reduction, no new capability. Reject them.

When to propose a command (kind='command'):
- The user types `/something` repeatedly that doesn't exist (look at user_correction \
  events or recent input patterns).
- A common multi-step prompt the user reuses verbatim — a slash command saves typing.

Strict rules for rsi_propose:
- The `rationale` MUST cite the feedback evidence (event types and counts) that \
  drove the proposal. No speculation.
- One proposal per cycle is plenty. Quality over quantity.
- Never propose a destructive shell tool (`rm`, `dd`, `mv`, `>`, `|sh`, etc.) — \
  those should always go through tool_manage with explicit user approval, not \
  through RSI.
- Don't repropose: rsi_propose dedups by name, but rapid resubmission still wastes \
  the user's review time. If a proposal was already filed and not applied, the \
  user has a reason; don't insist.

## Rules

Do NOT apply improvements if the data is insufficient or ambiguous. \
Quality over quantity — one well-reasoned improvement is better than many speculative ones. \
Never duplicate an existing instruction in a brain file — you have the 'read' action to check first. \
If an improvement was already applied (check self_improve action='list'), skip it. \
Use 'update' over 'apply' when an existing instruction needs rewording, not a new one added. \
When a brain rule you write names a specific non-core tool (`telegram_send`, `analyze_image`, \
`cron_manage`, `message`, ...), pair the mention with a `tool_search` reminder — a bare tool name \
reads as directly callable and invites a schema-less guess that wastes the first call. The runtime \
preamble already states this discipline globally; do not restate the general rule, just keep \
individual tool mentions you author consistent with it.\n\nTOOL LIFECYCLE: when you find instructions \
referencing a tool the agent should use, reinforce the full cycle — `tool_search` before calling, \
fallback to `tool_search` on failure, never assume a tool is unavailable without searching. If a \
brain rule only names the tool without the search reminder, the agent will guess parameters or skip \
the tool entirely. Always include the search-then-call pattern.";

/// Resolve the provider (and model) RSI cycles run on (#977), in preference order:
///
/// 1. `[agent] self_improvement_provider` — explicit override, normalised
///    (#1314) then trusted (a dead one is caught and demoted at runtime by
///    the #469 fallback).
/// 2. `[agent] default_provider` — the pair the user declared for their own
///    sessions, when it is healthy.
/// 3. The first healthy entry of the `[providers.fallback]` chain, in the
///    user's configured order.
/// 4. `active_provider_and_model()` registry walk — last resort.
///
/// The old code jumped straight to (4), and that walk starts with xiaomi, so
/// any install with an enabled keyed xiaomi section ran RSI on that expensive
/// 1M-window pair regardless of what the user actually chatted on. No
/// hardcoded provider preference survives this ladder.
///
/// The explicit override is normalised first: a `custom:` table-path prefix
/// is dropped and a `<provider>/<model>` spelling is split, because a value
/// taken verbatim fails to build a provider and every cycle dies before it
/// starts (#1314). The returned `note` names what was corrected; the cycle
/// logs it once.
pub(crate) fn resolve_rsi_pair(config: &Config) -> crate::brain::provider_spec::ProviderPair {
    let model_key = config.agent.self_improvement_model.as_deref();
    match config.agent.self_improvement_provider.as_deref() {
        Some(explicit) => crate::brain::provider_spec::normalize_in(
            config,
            crate::brain::provider_spec::ProviderKey::SELF_IMPROVEMENT,
            explicit,
            model_key,
        ),
        None => crate::brain::provider_spec::ProviderPair {
            provider: resolve_rsi_provider_default(config),
            model: model_key
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(str::to_string),
            note: None,
        },
    }
}

/// The ladder without the explicit self-improvement override — where a dead
/// override lands at runtime (#469).
fn resolve_rsi_provider_default(config: &Config) -> String {
    if let Some(default) = config.agent.default_provider.as_deref()
        && config.providers.is_healthy(default)
    {
        return default.to_string();
    }
    if let Some(fallback) = config.providers.fallback.as_ref()
        && fallback.enabled
        && let Some(first) = fallback
            .providers
            .iter()
            .chain(fallback.provider.iter())
            .find(|p| config.providers.is_healthy(p))
    {
        return first.clone();
    }
    config.providers.active_provider_and_model().0
}

/// Run a single autonomous RSI agent cycle.
///
/// Creates a lightweight AgentService with only RSI tools, sends the improvement
/// prompt, and returns the agent's summary of what it did.
/// Pending proposals across every kind, for the before/after cycle audit.
fn pending_proposal_count() -> usize {
    let store = crate::brain::rsi_proposals::ProposalsStore::new();
    store.list_tool_proposals().len()
        + store.list_command_proposals().len()
        + store.list_skill_proposals().len()
}

// ------------------------------------------------- #1240 stale-claim scan

/// Structured stale-scan input for the improvement step (#1240 RFC step 5).
///
/// The RFC's whole point in one struct: `self_improve action='update'`
/// exists and is the correct verb for sharpening a rule whose wording is
/// wrong about the world, but nothing ever PRODUCED that input — so stale
/// prescriptions sat in brain files forever while the verifier checked
/// structure only. This is that input, built by the gated scan ahead of
/// the improvement step and partitioned exactly along the RFC's action
/// matrix (`rsi_stale_scan::decide_action`):
///
/// - `reword_via_update` — the actionable set. Anchor-verified stale
///   prescriptions (dead binary / unconfigured provider / unknown config
///   key) where the RULE stays and only its claim about the world is
///   wrong. The cycle agent sharpens each in place via
///   `self_improve action='update'`, which rewords and never deletes, so
///   append-only protected brains stay append-only (RFC design decision
///   2).
/// - `owner_signoff` — stale anchors (vanished paths) whose natural fix
///   is a REMOVAL. Proposed removals never execute autonomously: they are
///   quoted to the owner via the cycle summary for explicit sign-off.
/// - `cleared` — previously-stale anchors positively re-verified healthy
///   again. Informational; a flag going quiet is news the cycle agent
///   should see, never a silent drop (ledger semantics).
///
/// An EMPTY input is the common case by construction: the cadence gate
/// skips most cycles (daily, forced on binary-version change), and the
/// ledger's dedup suppresses same-verdict repeats, so each finding reaches
/// a cycle prompt at most once. Empty input renders as an empty prompt
/// block and the cycle is byte-identical to the pre-#1240 baseline.
#[derive(Debug, Clone, Default)]
pub(crate) struct StaleScanInput {
    /// Stale prescriptions the agent may reword via
    /// `self_improve action='update'`.
    pub(crate) reword_via_update: Vec<crate::brain::rsi_stale_scan::StaleFinding>,
    /// Stale anchors whose fix is a removal — owner sign-off required,
    /// never edited by the agent.
    pub(crate) owner_signoff: Vec<crate::brain::rsi_stale_scan::StaleFinding>,
    /// Previously-stale anchors now verified healthy (informational).
    pub(crate) cleared: Vec<crate::brain::rsi_stale_scan::StaleFinding>,
}

impl StaleScanInput {
    /// Partition one gated scan outcome into the improvement-step input.
    ///
    /// [`crate::brain::rsi_stale_ledger::ScanRunOutcome::Skipped`] — the
    /// cadence gate said not now, which is most cycles — maps to the empty
    /// input: zero prompt impact, one ledger-file read of cost. A run that
    /// surfaced nothing new (every anchor healthy, or same-verdict repeats
    /// suppressed by the ledger) also maps to empty — findings reach the
    /// prompt exactly once each, which is the no-spam guarantee.
    pub(crate) fn from_outcome(outcome: &crate::brain::rsi_stale_ledger::ScanRunOutcome) -> Self {
        let crate::brain::rsi_stale_ledger::ScanRunOutcome::Ran { report, .. } = outcome else {
            return Self::default();
        };
        let mut reword_via_update = Vec::new();
        let mut owner_signoff = Vec::new();
        for finding in &report.to_surface {
            match finding.action {
                crate::brain::rsi_stale_scan::FindingAction::RewordViaUpdate => {
                    reword_via_update.push(finding.clone())
                }
                crate::brain::rsi_stale_scan::FindingAction::SurfaceToUser => {
                    owner_signoff.push(finding.clone())
                }
                crate::brain::rsi_stale_scan::FindingAction::None => {}
            }
        }
        Self {
            reword_via_update,
            owner_signoff,
            cleared: report.cleared.clone(),
        }
    }

    /// True when the scan has nothing to say this cycle (the byte-identical
    /// fast path).
    pub(crate) fn is_empty(&self) -> bool {
        self.reword_via_update.is_empty()
            && self.owner_signoff.is_empty()
            && self.cleared.is_empty()
    }
}

/// Human label for one anchor kind, for the prompt block.
fn anchor_kind_label(kind: crate::brain::rsi_stale_scan::AnchorKind) -> &'static str {
    match kind {
        crate::brain::rsi_stale_scan::AnchorKind::Binary => "binary",
        crate::brain::rsi_stale_scan::AnchorKind::ProviderName => "provider",
        crate::brain::rsi_stale_scan::AnchorKind::ConfigKey => "config key",
        crate::brain::rsi_stale_scan::AnchorKind::FilePath => "path",
    }
}

/// Render the structured scan input as the prompt block fed to the
/// improvement step (#1240). Empty input → empty string, so the cycle
/// prompt keeps its exact pre-#1240 bytes on the fast path.
///
/// Ordering inside the block mirrors the RFC's action matrix: the
/// actionable reword set first (each item carrying file, line, the dead
/// anchor, and the evidence, plus the explicit `self_improve
/// action='update'` recommendation), then the owner-sign-off set with an
/// explicit do-not-edit instruction (removals are the owner's call), then
/// cleared anchors as a one-line informational note.
pub(crate) fn stale_scan_prompt_block(input: &StaleScanInput) -> String {
    if input.is_empty() {
        return String::new();
    }
    let mut block = String::from(
        "\nSTALE-CLAIM SCAN FINDINGS (#1240)\n\n\
         A read-only deterministic scan verified the world-state anchors \
         (binaries on PATH, configured providers, config-schema keys, file \
         paths) cited by brain-file rules. Everything below is \
         anchor-verified — never a guess that a rule seems old.\n\n",
    );
    if !input.reword_via_update.is_empty() {
        block.push_str(
            "ACTIONABLE — reword each rule below with self_improve action='update'. \
             The rule's wording is wrong about this install's reality; the rule itself \
             stays. 'update' sharpens an existing instruction in place and never \
             deletes content, so append-only protected brains stay append-only. \
             Reword each so it no longer cites the dead anchor (name the live \
             replacement, or drop the anchor from the sentence). Do not add new \
             rules for these — the rule exists, only its claim about the world \
             is wrong:\n\n",
        );
        for (i, f) in input.reword_via_update.iter().enumerate() {
            block.push_str(&format!(
                "{}. {}:{}\n   line: {}\n   dead anchor: {} `{}` — {}\n",
                i + 1,
                f.file,
                f.line_no,
                f.line,
                anchor_kind_label(f.anchor_kind),
                f.anchor,
                f.evidence
            ));
        }
    }
    if !input.owner_signoff.is_empty() {
        block.push_str(
            "\nOWNER SIGN-OFF REQUIRED — do NOT edit these yourself. The natural fix \
             is a removal, and removals are the owner's call, never the cycle's \
             (append-only reality). Quote each verbatim in your cycle summary so \
             the owner decides:\n\n",
        );
        for f in &input.owner_signoff {
            block.push_str(&format!(
                "- {}:{}\n  line: {}\n  dead anchor: {} `{}` — {}\n",
                f.file,
                f.line_no,
                f.line,
                anchor_kind_label(f.anchor_kind),
                f.anchor,
                f.evidence
            ));
        }
    }
    if !input.cleared.is_empty() {
        block.push_str(
            "\nPREVIOUSLY STALE, NOW HEALTHY AGAIN — informational only, no action \
             needed; the anchor cited by each rule verified back to Ok:\n\n",
        );
        for f in &input.cleared {
            block.push_str(&format!(
                "- {}:{} — `{}` verified ok ({})\n",
                f.file, f.line_no, f.anchor, f.evidence
            ));
        }
    }
    block
}

/// Run the gated stale-claim scan for one improvement step (#1240): daily
/// cadence with a binary-version force trigger, plus ledger dedup — both
/// inside `rsi_stale_ledger`, so most cycles cost one ledger-file read and
/// map to an empty [`StaleScanInput`]. The scan lives HERE, inside the
/// cycle, deliberately: it never spawns a cycle of its own, never emits an
/// `ImprovementOpportunity` notification, and never touches the
/// finding-set hash or the convergence pause — the existing inbox-noise
/// gates stay exactly as they were, and findings ride along only in cycles
/// that were already going to run, at most once each.
///
/// A failed ledger persist is logged loudly but is never fatal: findings
/// still feed this cycle; the only cost is that the next successful run
/// may re-flag them.
fn stale_scan_cycle_input(config: &Config) -> StaleScanInput {
    let home = crate::config::opencrabs_home();
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Best-effort cycle stamp: the engine increments rsi/cycle_number after
    // each loop pass, so this is the count of completed cycles — good
    // enough for the ledger's `last_verified_cycle` bookkeeping.
    let cycle = std::fs::read_to_string(home.join("rsi/cycle_number"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let outcome = crate::brain::rsi_stale_ledger::run_scan_with_ledger(
        config,
        &home,
        &crate::brain::rsi_stale_ledger::default_ledger_path(),
        now_unix,
        cycle,
        crate::VERSION,
    );
    if let crate::brain::rsi_stale_ledger::ScanRunOutcome::Ran {
        persisted: Err(e), ..
    } = &outcome
    {
        tracing::warn!(
            "RSI stale scan: ledger persist failed ({e}) — findings still feed this \
             cycle; the next successful scan may re-flag them"
        );
    }
    let input = StaleScanInput::from_outcome(&outcome);
    if !input.is_empty() {
        tracing::info!(
            "RSI stale scan fed the cycle: {} reword-via-update finding(s), {} \
             owner-signoff, {} cleared",
            input.reword_via_update.len(),
            input.owner_signoff.len(),
            input.cleared.len()
        );
    }
    input
}

/// Build the improvement-step user prompt.
///
/// The construction is the pre-#1240 body lifted verbatim out of
/// `run_rsi_agent_cycle` (extracted so the empty-scan path is testable
/// byte-for-byte against the baseline), plus one addition: a non-empty
/// `stale_block` is inserted BETWEEN the analyze line and the
/// capability-gap actions, so the #842 rule that capability gaps close the
/// prompt still holds. An empty `stale_block` is a no-op push — the prompt
/// is byte-identical to what the cycle built before the stale scan
/// existed.
pub(crate) fn build_cycle_prompt(opportunities: &[String], stale_block: &str) -> String {
    let mut prompt = "Run an autonomous self-improvement cycle.\n\n".to_string();
    if !opportunities.is_empty() {
        prompt.push_str("Detected opportunities:\n");
        for (i, opp) in opportunities.iter().enumerate() {
            prompt.push_str(&format!("{}. {opp}\n", i + 1));
        }
        prompt.push('\n');
    }
    prompt.push_str(
        "Analyze the feedback data, identify the highest-impact issues, and apply improvements.\n",
    );
    if !stale_block.is_empty() {
        prompt.push_str(stale_block);
    }
    // Capability gaps last, so the closing instruction is not "apply
    // improvements" — which reads as `self_improve` and was answered that way
    // on every cycle while the proposal path went unused (#842).
    prompt.push_str(&crate::brain::rsi_disposition::required_actions_block(
        opportunities,
    ));
    prompt
}

async fn run_rsi_agent_cycle(
    pool: crate::db::Pool,
    config: &Config,
    opportunities: &[String],
) -> anyhow::Result<String> {
    use crate::brain::agent::AgentService;
    use crate::services::{MessageService, ServiceContext, SessionService};

    // Resolve RSI provider (#977): explicit self_improvement_provider, then
    // the user's declared session default, then the first healthy
    // fallback-chain provider, then the registry walk as last resort. The
    // old code started from the registry walk, which prefers xiaomi by
    // hardcoded order and put RSI on the expensive 1M-window pair.
    let active_provider = resolve_rsi_provider_default(config);
    let pair = resolve_rsi_pair(config);
    if let Some(note) = pair.note.as_deref() {
        // Once per cycle, so a config that keeps the wrong spelling keeps
        // being told the canonical one (#1314).
        tracing::warn!(
            "RSI: self_improvement_provider corrected to '{}': {note}",
            pair.provider
        );
    }
    let provider_name = pair.provider.as_str();
    let configured_model = pair.model.clone();

    // #469 part A: a dead self_improvement_provider (missing key, typo,
    // removed section) must not kill the cycle — fall back to the user's
    // active provider before the chain wrap below. Errors are loud, never
    // swallowed.
    let provider =
        match crate::brain::provider::factory::create_provider_by_name(config, provider_name).await
        {
            Ok(p) => p,
            Err(e) if provider_name != active_provider => {
                tracing::warn!(
                    "RSI: self_improvement_provider '{provider_name}' failed to create ({e:#}) — \
                 falling back to active provider '{active_provider}' (#469)"
                );
                crate::brain::provider::factory::create_provider_by_name(config, &active_provider)
                    .await?
            }
            Err(e) => return Err(e),
        };

    // Apply the [providers.fallback] chain (if any) to the RSI provider
    // — same wrapping the main session path gets via
    // `create_provider_with_warning`. Before this call, the autonomous
    // loop bypassed the chain entirely: an RSI rate limit killed the
    // cycle instead of cascading to the configured fallback.
    let provider =
        crate::brain::provider::factory::wrap_with_fallback_chain(config, provider).await?;

    // A CLI provider cannot run OpenCrabs tools: `cli_handles_tools()` makes the
    // tool loop skip local execution entirely, on the understanding that the CLI
    // runs tools internally. RSI exists ONLY to call `feedback_analyze`,
    // `self_improve` and `rsi_propose`, so on such a provider every cycle is a
    // guaranteed no-op that still pays for a full turn.
    //
    // It also fails silently, which is why this is a hard refusal rather than a
    // warning: the same flag disables phantom detection, so the model narrates
    // tool calls that never execute and nothing corrects it. Observed on a live
    // install as hourly cycles reporting "I described actions but did not
    // actually execute any tool this turn" and "Same data. Stopping." for
    // months, because nothing could ever be applied (#805).
    if provider.cli_handles_tools() {
        return Err(anyhow::anyhow!(
            "RSI cannot run on '{}': it is a CLI provider, so OpenCrabs tools \
             (feedback_analyze, self_improve, rsi_propose) never execute and every cycle \
             is a silent no-op. Set [agent] self_improvement_provider to an API provider.",
            provider.name()
        ));
    }

    let service_ctx = ServiceContext::new(pool);
    let tool_registry = build_rsi_tool_registry();
    let brain_path = crate::config::opencrabs_home();

    let agent = AgentService::new(provider, service_ctx.clone(), config)
        .await
        .with_tool_registry(tool_registry)
        .with_auto_approve_tools(true)
        .with_max_tool_iterations(RSI_MAX_TOOL_ITERATIONS)
        .with_system_brain(RSI_AGENT_PROMPT.to_string())
        .with_brain_path(brain_path);

    // Reuse one persistent RSI session ROW (keeps the session list clean and the
    // #805 pair-repinning logic working), but seal its history before each cycle
    // with a compaction marker (#977): the context loader then picks up only the
    // marker + the fresh prompt instead of every cycle since creation. Cross-cycle
    // continuity lives in rsi/improvements.md, rsi/digest.md and the feedback
    // ledger, not in session history, which the agent cannot act on anyway: each
    // cycle starts from feedback_analyze.
    let session_service = SessionService::new(service_ctx.clone());
    let mut session = match session_service
        .find_session_by_title("RSI autonomous cycle")
        .await?
    {
        Some(s) => s,
        None => {
            session_service
                .create_session_with_provider(
                    Some("RSI autonomous cycle".to_string()),
                    Some(provider_name.to_string()),
                    configured_model.clone(),
                    None,
                )
                .await?
        }
    };

    // The session outlives the config, so its stored pair must follow config
    // rather than override it (#805). `ensure_session_provider_restored` (#704)
    // restores the SESSION's saved provider at turn start, which is right for a
    // user session and wrong here: this session was created in April on
    // whatever was active then, and re-pinned that provider on every cycle
    // since, so changing `self_improvement_provider` had no effect at all.
    //
    // Re-checked every cycle, not just at creation, or a later config change
    // silently keeps the old pair, which is exactly the bug.
    {
        let configured_model = configured_model.clone();
        let pair_is_stale = rsi_pair_is_stale(
            session.provider_name.as_deref(),
            session.model.as_deref(),
            provider_name,
            configured_model.as_deref(),
        );
        if pair_is_stale {
            tracing::warn!(
                "RSI session pinned to {:?}/{:?} but config selects '{}'/{:?} — repinning to config",
                session.provider_name,
                session.model,
                provider_name,
                configured_model
            );
            let mut repinned = session.clone();
            repinned.provider_name = Some(provider_name.to_string());
            if configured_model.is_some() {
                repinned.model = configured_model;
            }
            match session_service.update_session(&repinned).await {
                Ok(()) => session = repinned,
                Err(e) => tracing::warn!("Failed to repin RSI session to the configured pair: {e}"),
            }
        }
    }

    // #1240 stale-claim scan, ahead of the improvement step: cadence +
    // ledger dedup make most cycles cost one ledger-file read and nothing
    // else (a Skip — or a run with nothing new — yields an empty block and
    // the prompt below is byte-identical to the pre-#1240 baseline). The
    // scan rides inside this cycle deliberately: it cannot spawn agent
    // runs, add notifications, or move the finding-set hash / convergence
    // pause, so the existing inbox-noise gates are untouched.
    let stale_input = stale_scan_cycle_input(config);
    // Build the user prompt with detected opportunities (+ stale-scan
    // findings when the gated scan produced any).
    let prompt = build_cycle_prompt(opportunities, &stale_scan_prompt_block(&stale_input));

    let model = configured_model;

    // Counted before and after so a cycle that ignores its capability gaps is
    // visible in the log instead of closing as a success (#842).
    let gaps = crate::brain::rsi_disposition::capability_count(opportunities);
    let proposals_before = pending_proposal_count();

    // Fresh context per cycle (#977): seal all prior cycles behind a compaction
    // marker so `messages_from_last_compaction` loads only the marker + this
    // cycle's prompt. The old behavior re-fed the entire history every cycle:
    // one session had grown to 1,985 messages (~313M tokens, $50) inside a 1M
    // context window, with every cycle paying for all of it while adding
    // nothing. The banner line is stripped before the model sees it
    // (strip_compaction_banner), so only the short note below reaches it.
    let message_service = MessageService::new(service_ctx.clone());
    message_service
        .create_message(
            session.id,
            "user".to_string(),
            "[CONTEXT COMPACTION — RSI cycles are stateless by design. Prior cycles \
             are sealed; durable state lives in rsi/improvements.md, rsi/digest.md \
             and the feedback ledger. Start fresh from the feedback data.]"
                .to_string(),
        )
        .await?;

    let response = agent
        .send_message_with_tools(session.id, prompt, model)
        .await?;

    let filed = pending_proposal_count().saturating_sub(proposals_before);
    if gaps > 0 && filed == 0 {
        tracing::warn!(
            "RSI cycle answered {gaps} capability-gap opportunity/opportunities with zero \
             rsi_propose calls: the gaps will be re-detected next cycle (#842)"
        );
    } else if gaps > 0 {
        tracing::info!("RSI cycle: {gaps} capability gap(s), {filed} proposal(s) filed");
    }

    tracing::info!(
        "RSI agent cycle complete: {} tokens used, ${:.4} cost",
        response.usage.input_tokens + response.usage.output_tokens,
        response.cost
    );

    Ok(response.content)
}

/// Spawn the background RSI engine.
///
/// - Writes startup digest immediately
/// - Checks for actionable patterns on the backoff-ladder interval
///   (`rsi_interval_for_streak`: 1h base, out to 24h on consecutive
///   zero-improvement agent runs, reset by an applied improvement)
/// - When opportunities are found, spawns an autonomous agent to apply improvements
/// - Emits notifications to TUI via the provided channel
///
/// Takes no `Config` argument on purpose (#1696). It used to accept one and
/// clone it for the life of the task, which froze everything the cycle builds
/// from it (the RSI provider, its `[providers.fallback]` chain, the session
/// pair) at process start, while the enable gate a few lines below already
/// read the live mirror. Every config touch in this task now goes through
/// [`Config::current()`], read once per cycle boundary so a save landing
/// mid-cycle cannot tear the provider against the chain.
pub fn spawn_rsi_engine(
    pool: crate::db::Pool,
    notification_tx: mpsc::UnboundedSender<RsiNotification>,
    headless: bool,
) {
    let pool_clone = pool.clone();
    tokio::spawn(async move {
        // Delay to let the app fully start
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        // #1063 master gate. Absent key defaults by run mode: ON for the
        // interactive TUI, OFF for headless daemons (an unattended daemon
        // burning provider quota and appending to brain files hourly is the
        // bug this fixes). Re-read from the live config mirror every cycle,
        // so flipping `rsi_enabled` in config.toml takes effect on the next
        // boundary without a restart, in both directions. The engine task
        // itself always spawns so an enable can hot-reload in.
        if !rsi_effectively_enabled(&Config::current(), headless) {
            tracing::info!(
                "RSI engine disabled (rsi_enabled unset or false, headless={headless}); \
                 skipping template sync + startup digest"
            );
        } else {
            // 1. Upstream template sync. No version gate (#820): whether the app
            // was upgraded says nothing about whether a template changed, and
            // gating on it left #816/#817 undeliverable for as long as the release
            // took. sync_templates decides per file by content, and one whose
            // upstream is unchanged writes nothing.
            {
                let results = crate::brain::rsi_sync::sync_templates().await;
                if results.is_empty() {
                    tracing::info!("RSI template sync: no files to sync");
                } else {
                    let synced = results.iter().filter(|r| r.synced).count();
                    let failed = results.iter().filter(|r| r.error.is_some()).count();
                    let sections: usize = results.iter().map(|r| r.sections_added).sum();
                    let summary = format!(
                        "{} files synced, {} failed, {} new sections (v{})",
                        synced,
                        failed,
                        sections,
                        crate::VERSION
                    );
                    if failed > 0 {
                        // One line per distinct cause, not one per file: a
                        // single DNS flap used to render eleven near-identical
                        // sentences that named each file twice (#2169).
                        if let Some(error) =
                            crate::brain::rsi_sync::summarize_sync_failures(&results)
                        {
                            let _ = notification_tx.send(RsiNotification::TemplateSyncFailed {
                                error,
                            });
                        }
                    }
                    if synced > 0 {
                        let _ =
                            notification_tx.send(RsiNotification::TemplateSyncComplete { summary });
                    }
                }
            }

            // 2. Write startup digest
            write_startup_digest(pool_clone.clone()).await;
            let repo = FeedbackLedgerRepository::new(pool_clone.clone());
            if let Ok(total) = repo.total_count().await {
                let _ = notification_tx.send(RsiNotification::DigestWritten {
                    total_events: total,
                });
            }
        }
        let repo = FeedbackLedgerRepository::new(pool_clone.clone());

        // 2. Periodic analysis + autonomous improvement cycle
        //
        // On startup, check how long ago the last cycle ran. If the app was
        // restarted before the interval elapsed (e.g. dev recompile every
        // ~20 min), only sleep the remaining time instead of a full hour.
        // Without this, frequent restarts prevent RSI from ever firing.
        let last_cycle_path = crate::config::opencrabs_home().join("rsi/last_cycle");
        // Hash of the previous cycle's `opportunities` Vec. When the new
        // cycle's hash matches, the RSI engine skips re-emitting the same
        // top-N corrections / errors / tool-failure descriptions to the
        // TUI and channels, and skips the autonomous agent run (the LLM
        // would just write "Converged. No improvements applied." again).
        let opportunities_hash_path =
            crate::config::opencrabs_home().join("rsi/last_opportunities_hash");
        // Zero-improvement backoff state (#977): consecutive agent runs that
        // applied nothing push the cycle interval out along
        // RSI_BACKOFF_LADDER_SECS. Persisted across restarts — without the
        // file, every restart reset the ladder and a converged install went
        // back to hourly polling.
        let zero_improvement_streak_path =
            crate::config::opencrabs_home().join("rsi/zero_improvement_streak");
        let mut zero_improvement_streak: u64 =
            std::fs::read_to_string(&zero_improvement_streak_path)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
        // Append-only ledger of applied improvements; growth across an agent
        // run is the mechanical "a real improvement happened" signal that
        // resets the ladder (no text matching on summaries).
        let improvements_ledger_path = crate::config::opencrabs_home().join("rsi/improvements.md");
        let current_interval = rsi_interval_for_streak(zero_improvement_streak);
        let initial_delay = if let Ok(meta) = std::fs::metadata(&last_cycle_path) {
            let elapsed = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs())
                .unwrap_or(current_interval);
            if elapsed >= current_interval {
                // Overdue — run soon (30s grace for app to stabilize)
                30
            } else {
                current_interval - elapsed
            }
        } else {
            // First run ever — use full interval
            current_interval
        };
        tracing::info!(
            "RSI engine: first cycle in {}m{}s",
            initial_delay / 60,
            initial_delay % 60
        );

        let cycle_number_path = crate::config::opencrabs_home().join("rsi/cycle_number");

        let mut first_iteration = true;
        // #1063: gate state for transition logging. The engine spawns in both
        // modes; when disabled it just sleeps through cycles, so an enable in
        // config.toml hot-reloads without a restart.
        let mut gate_announced_state = rsi_effectively_enabled(&Config::current(), headless);
        // Baseline of ACTIONABLE ledger events (tool_failure + user_correction
        // + provider_error) seen at the last cycle, persisted across restarts
        // (#977): without the file, every restart reset the baseline to 0 and
        // the first cycle after it re-analyzed stale data.
        let last_actionable_path =
            crate::config::opencrabs_home().join("rsi/last_actionable_count");
        let mut last_seen_actionable: i64 = std::fs::read_to_string(&last_actionable_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        // Persist cycle_number across restarts. Without this, frequent
        // restarts reset the counter.
        let mut cycle_number: u64 = std::fs::read_to_string(&cycle_number_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        // Convergence-pause state (#977): consecutive "nothing new" agent
        // summaries increment the streak; at RSI_CONVERGENCE_PAUSE_AFTER
        // the `rsi/paused` marker stops agent runs until the finding set
        // changes. Both persist across restarts — without that, every
        // restart re-paid for the same converged cycles. Delete
        // `rsi/paused` to unpause manually.
        let convergence_streak_path =
            crate::config::opencrabs_home().join("rsi/convergence_streak");
        let paused_path = crate::config::opencrabs_home().join("rsi/paused");
        let mut convergence_streak: u64 = std::fs::read_to_string(&convergence_streak_path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        loop {
            let delay = if first_iteration {
                first_iteration = false;
                initial_delay
            } else {
                rsi_interval_for_streak(zero_improvement_streak)
            };
            tokio::time::sleep(std::time::Duration::from_secs(delay)).await;

            // #1063: master gate, re-read from the live config mirror every
            // cycle so `rsi_enabled` edits apply without a restart.
            let gate_now = rsi_effectively_enabled(&Config::current(), headless);
            if gate_now != gate_announced_state {
                tracing::info!(
                    "RSI engine {} via config (rsi_enabled / hot reload)",
                    if gate_now { "enabled" } else { "disabled" }
                );
                gate_announced_state = gate_now;
            }
            if !gate_now {
                continue;
            }

            let total = match repo.total_count().await {
                Ok(t) => t,
                Err(_) => continue,
            };

            if total < RSI_MIN_ENTRIES {
                tracing::debug!(
                    "RSI cycle: only {total} entries (need {RSI_MIN_ENTRIES}), skipping"
                );
                continue;
            }

            // Skip if no new ACTIONABLE feedback since last cycle (#977). The
            // old gate compared `total`, which includes `tool_success`
            // (recorded on every tool call anywhere), so it climbed on any
            // busy install and the skip never fired: every cycle paid for a
            // full analysis even when nothing had failed. Deltas of real
            // failures, user corrections and provider errors are the only
            // signal that new improvements are possible.
            let actionable = match repo.count_actionable().await {
                Ok(c) => c,
                Err(_) => continue,
            };
            if actionable == last_seen_actionable {
                tracing::debug!(
                    "RSI cycle: actionable feedback unchanged ({actionable} failures/corrections/provider errors), skipping"
                );
                // Still stamp the file so restart timer stays accurate
                let _ = std::fs::write(&last_cycle_path, "");
                continue;
            }
            last_seen_actionable = actionable;
            let _ = std::fs::write(&last_actionable_path, actionable.to_string());

            let _ = notification_tx.send(RsiNotification::CycleStarted);
            tracing::info!("RSI cycle: analyzing {total} feedback entries");

            // Refresh digest file
            write_startup_digest(repo.pool().clone()).await;

            // Collect actionable opportunities
            let mut opportunities = Vec::new();
            // Stable finding-identity keys, one per opportunity, hashed by
            // the dedup gate below (#977). Descriptions carry counts and
            // samples that churn every busy cycle; the keys don't.
            let mut opportunity_keys: Vec<String> = Vec::new();

            // Tools with >20% failure rate and >5 executions over the
            // last 7 days. Without the window, a tool that broke once
            // and was fixed shows "100% failure" forever — the
            // 2026-04-25 RSI logs were full of stale alerts about
            // exa_search and wait_agent long after both bugs landed.
            let window_since = (chrono::Utc::now() - chrono::Duration::days(7))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            // Resolve the opencrabs source repo once per cycle so we
            // can ask `git log` whether a given tool's failures already
            // have a fix commit between them and now. Returns None when
            // we can't find a checkout (installed binary launched from
            // an unrelated cwd, no OPENCRABS_SRC env var) — we then
            // skip the git-context check, falling back to the
            // window-only behaviour.
            let source_repo = crate::brain::rsi_git_history::resolve_source_repo();
            if let Ok(stats) = repo
                .stats_by_dimension_since("tool_", Some(&window_since))
                .await
            {
                // Sentinel dimensions that fire as "failures" by design
                // (self-heal detectors, regression probes) — excluded via
                // the module-level SENTINEL_DIMENSIONS above so the digest
                // Metrics block and this filter stay in sync.
                for s in stats
                    .iter()
                    .filter(|s| s.total_events >= 5 && s.success_rate < 0.8)
                    .filter(|s| !SENTINEL_DIMENSIONS.contains(&s.dimension.as_str()))
                {
                    // Suppress the alert when the source repo has a
                    // commit since the window opened whose subject
                    // mentions this dimension (= tool name). Convention
                    // here: nearly every fix commit names the tool in
                    // its subject ("fix(provider): unwrap proxy",
                    // "fix(browser): name the actual browser"), so a
                    // grep on `dimension` against `--since=window_start`
                    // catches "we already fixed that".
                    if let Some(ref repo_path) = source_repo {
                        let commits = crate::brain::rsi_git_history::commits_matching_since(
                            repo_path,
                            &window_since,
                            &s.dimension,
                        );
                        if !commits.is_empty() {
                            tracing::info!(
                                "RSI suppress '{}': {} fix commit(s) since window open — first: {} {}",
                                s.dimension,
                                commits.len(),
                                &commits[0].sha[..7.min(commits[0].sha.len())],
                                commits[0].subject,
                            );
                            continue;
                        }
                    }
                    // Pull recent failures for this tool to give agent context
                    let mut detail = format!(
                        "Tool '{}' has {:.0}% failure rate ({} failures out of {}). \
                         Review failure patterns via feedback_analyze and record \
                         derived rules with feedback_record.",
                        s.dimension,
                        (1.0 - s.success_rate) * 100.0,
                        s.failures,
                        s.total_events
                    );
                    if let Ok(recent) = repo.by_event_type("tool_failure", 10).await {
                        let relevant: Vec<_> = recent
                            .iter()
                            .filter(|e| e.dimension == s.dimension)
                            .take(3)
                            .collect();
                        if !relevant.is_empty() {
                            detail.push_str("\n  Recent failures:");
                            for e in relevant {
                                detail.push_str(&format!(
                                    "\n  - session={}, time={}, meta={}",
                                    &e.session_id[..8.min(e.session_id.len())],
                                    e.created_at.format("%Y-%m-%d %H:%M"),
                                    e.metadata.as_deref().unwrap_or("none")
                                ));
                            }
                        }
                    }
                    tracing::info!("RSI opportunity: {}", detail);
                    opportunities.push(detail);
                    opportunity_keys.push(format!("tool_failure:{}", s.dimension));
                }
            }

            // Repeated user corrections — include recent examples with session/model
            if let Ok(corrections) = repo.by_event_type("user_correction", 50).await
                && corrections.len() >= 3
            {
                let mut desc = format!(
                    "{} user corrections recorded. Review patterns and update brain files.",
                    corrections.len()
                );
                desc.push_str("\n  Recent corrections:");
                for e in corrections.iter().take(5) {
                    desc.push_str(&format!(
                        "\n  - session={}, model={}, time={}, text={}",
                        &e.session_id[..8.min(e.session_id.len())],
                        e.dimension,
                        e.created_at.format("%Y-%m-%d %H:%M"),
                        e.metadata.as_deref().unwrap_or("none")
                    ));
                }
                tracing::info!("RSI opportunity: {}", desc);
                opportunities.push(desc);
                opportunity_keys.push("user_corrections".to_string());
            }

            // Provider errors — surface model/provider info so agent knows which
            // provider is failing and can adjust brain files accordingly
            if let Ok(errors) = repo.by_event_type("provider_error", 20).await
                && errors.len() >= 3
            {
                let mut desc = format!("{} provider errors recorded.", errors.len());
                desc.push_str("\n  Recent errors:");
                for e in errors.iter().take(5) {
                    desc.push_str(&format!(
                        "\n  - session={}, provider/model={}, time={}, detail={}",
                        &e.session_id[..8.min(e.session_id.len())],
                        e.dimension,
                        e.created_at.format("%Y-%m-%d %H:%M"),
                        e.metadata.as_deref().unwrap_or("none")
                    ));
                }
                tracing::info!("RSI opportunity: {}", desc);
                opportunities.push(desc);
                opportunity_keys.push("provider_errors".to_string());
            }

            // Successful bash patterns — high-frequency subsystems
            // (gh, git, docker, …) flag tool-extraction candidates.
            // RSI's previous passes only walked failures, which meant
            // a workflow the agent ran 50 times successfully (e.g.
            // `gh issue comment`) never surfaced as an improvement
            // opportunity. This pass closes that gap: cmd= metadata
            // (now recorded on both success + failure events) is
            // classified by `rsi_subsystem` and grouped — subsystems
            // above PROMOTE_BASH_THRESHOLD bubble up so the RSI
            // agent can decide whether to file a tool / skill
            // proposal via rsi_propose.
            //
            // The threshold is deliberately high (~15 in a 24h
            // window) so we don't propose tools for trivial
            // one-offs. If the agent ran the same subsystem 15+
            // times in a day, it's a real pattern worth codifying.
            const PROMOTE_BASH_THRESHOLD: usize = 15;
            if let Ok(successes) = repo.by_event_type("tool_success", 2000).await {
                use std::collections::HashMap;
                let mut by_subsystem: HashMap<&'static str, Vec<&str>> = HashMap::new();
                for e in &successes {
                    if e.dimension != "bash" {
                        continue;
                    }
                    // Stay inside the analysis window so old data
                    // doesn't dominate the count.
                    if e.created_at.to_rfc3339() < window_since {
                        continue;
                    }
                    let Some(meta) = e.metadata.as_deref() else {
                        continue;
                    };
                    let Some(cmd) = crate::brain::rsi_subsystem::extract_cmd_from_meta(meta) else {
                        continue;
                    };
                    if let Some(subsystem) = crate::brain::rsi_subsystem::classify_bash_command(cmd)
                    {
                        by_subsystem.entry(subsystem).or_default().push(cmd);
                    }
                }
                // Stable order so the dedup hash below doesn't churn
                // on equivalent state.
                let mut subsystems: Vec<(&'static str, Vec<&str>)> =
                    by_subsystem.into_iter().collect();
                subsystems.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
                for (subsystem, cmds) in subsystems {
                    if cmds.len() < PROMOTE_BASH_THRESHOLD {
                        continue;
                    }
                    let sample: Vec<String> = cmds
                        .iter()
                        .take(5)
                        .map(|c| c.chars().take(140).collect::<String>())
                        .collect();
                    let desc = format!(
                        "Bash subsystem '{subsystem}' has {} successful invocations in the window. \
                         Promotion candidate: file a tool (rsi_propose kind=tool) for the recurring \
                         command shape, or a skill (kind=skill) for the workflow it codifies. \
                         The right shape depends on whether the calls share a parameterised invocation \
                         (→ tool) or are a multi-step sequence (→ skill). \
                         Sample invocations:\n  - {}",
                        cmds.len(),
                        sample.join("\n  - "),
                    );
                    tracing::info!("RSI opportunity: {}", desc);
                    opportunities.push(desc);
                    opportunity_keys.push(format!("bash_subsystem:{subsystem}"));
                }
            }

            // Repeated USER REQUESTS — slash-command candidates (#504).
            // The bash pass above codifies what the AGENT runs; this codifies
            // what the USER keeps typing (the /standup case). Group recent
            // user-message texts by normalized signature; a signature seen
            // COMMAND_PATTERN_THRESHOLD+ times is a promotion candidate the
            // agent files via rsi_propose kind=command. Already-applied /
            // rejected names never re-surface (#502), so this is safe to
            // suggest freely.
            {
                let cm_repo = crate::db::ChannelMessageRepository::new(repo.pool().clone());
                if let Ok(requests) = cm_repo.recent_user_requests(1000).await {
                    let candidates = crate::brain::rsi_command_patterns::command_candidates(
                        &requests,
                        crate::brain::rsi_command_patterns::COMMAND_PATTERN_THRESHOLD,
                    );
                    for c in candidates {
                        let desc = format!(
                            "User request pattern '{}' typed {} times — candidate for a slash \
                             command (rsi_propose kind=command). File a concise /<name> whose \
                             prompt captures the recurring intent. Sample phrasings:\n  - {}",
                            c.signature,
                            c.count,
                            c.samples.join("\n  - "),
                        );
                        tracing::info!("RSI opportunity: {}", desc);
                        opportunities.push(desc);
                        opportunity_keys.push(format!("command_pattern:{}", c.signature));
                    }
                }
            }

            // Recurring tool SEQUENCES — skill candidates (#504). Where the
            // command pass codifies what the user types, this codifies what
            // the agent DOES: an ordered tool run recurring across many
            // sessions is a workflow worth a skill. Cross-session recurrence
            // is the signal, so a within-session loop cannot inflate it.
            {
                let te_repo =
                    crate::db::repository::ToolExecutionRepository::new(repo.pool().clone());
                if let Ok(rows) = te_repo.recent_session_tool_sequences(5000).await {
                    let sessions = crate::brain::rsi_skill_sequences::group_sessions(&rows);
                    let candidates = crate::brain::rsi_skill_sequences::skill_sequence_candidates(
                        &sessions,
                        crate::brain::rsi_skill_sequences::SEQUENCE_LEN,
                        crate::brain::rsi_skill_sequences::SEQUENCE_MIN_SESSIONS,
                    );
                    for c in candidates.into_iter().take(5) {
                        let desc = format!(
                            "Tool sequence '{}' ran in {} distinct sessions — candidate for a \
                             skill (rsi_propose kind=skill). File a SKILL.md codifying the \
                             workflow so it runs as one step instead of the manual sequence.",
                            c.sequence.join(" -> "),
                            c.sessions,
                        );
                        tracing::info!("RSI opportunity: {}", desc);
                        opportunities.push(desc);
                        opportunity_keys
                            .push(format!("skill_sequence:{}", c.sequence.join(" -> ")));
                    }
                }
            }

            // 2b. An available upgrade is the most actionable finding there
            // is, and RSI was the one component running on a schedule that
            // never mentioned it (#821). Proposed, never applied: replacing
            // the running binary is the user's call.
            //
            // Recorded once. RSI runs hourly, and a proposal per hour for the
            // same version trains the user to ignore the inbox. check_for_update
            // already returns None when no asset exists for this platform or
            // the network failed, so neither produces a finding.
            if let Some(latest) = crate::brain::tools::evolve::check_for_update().await {
                let marker = crate::config::opencrabs_home().join("rsi/last_proposed_version");
                let already = std::fs::read_to_string(&marker)
                    .ok()
                    .map(|s| s.trim().to_string());
                if already.as_deref() != Some(latest.as_str()) {
                    if let Err(e) = std::fs::write(&marker, &latest) {
                        tracing::warn!("RSI: failed to record proposed version: {e}");
                    }
                    tracing::info!("RSI: upgrade available, {} -> {latest}", crate::VERSION);
                    opportunities.push(format!(
                        "OpenCrabs {latest} is available (running {}). This is a capability \
                         gap, not a guidance one: propose it with rsi_propose so the user can \
                         review and run /evolve. Do NOT upgrade autonomously — replacing the \
                         running binary is the user's decision.",
                        crate::VERSION
                    ));
                    opportunity_keys.push(format!("upgrade:{latest}"));
                }
            }

            // 3. Dedup: hash the cycle's stable finding-identity keys and
            // compare against the previous cycle's hash. When identical,
            // the autonomous agent would have nothing new to act on — its
            // own summary on those cycles was literally "Converged. No
            // improvements applied." (seen in the 2026-05-18 transcript
            // where #426 just re-printed the top-5 corrections / errors
            // from #425). Skip emission of every `ImprovementOpportunity`
            // notification AND the agent run, keeping only a compact
            // `AgentCycleComplete` so the user sees the cycle happened.
            //
            // The hash covers ONLY the identity keys (#977): which tools
            // fail, which subsystems/prompts/sequences recur, whether an
            // upgrade is out. Counts, sample events and top-N ordering
            // churn on every busy cycle and are deliberately NOT part of
            // the hash — a finding whose numbers moved is the same
            // finding. A new or disappeared finding changes the key set
            // and re-enables the full path. `tracing::info!` logs above
            // stay regardless.
            let current_hash = hash_opportunities(&opportunity_keys);
            let previous_hash = std::fs::read_to_string(&opportunities_hash_path)
                .ok()
                .map(|s| s.trim().to_string());
            let is_duplicate = previous_hash.as_deref() == Some(current_hash.as_str());
            let _ = std::fs::write(&opportunities_hash_path, &current_hash);

            if is_duplicate {
                if !opportunities.is_empty() {
                    tracing::info!(
                        "RSI cycle: {} opportunity/opportunities identical to previous cycle \
                         (hash={}) — skipping emission and agent run",
                        opportunities.len(),
                        &current_hash[..12.min(current_hash.len())]
                    );
                    // While paused (#977), stay fully silent: the hourly
                    // "Converged" notification is itself the noise the
                    // pause exists to stop.
                    if !paused_path.exists() {
                        let _ = notification_tx.send(RsiNotification::AgentCycleComplete {
                            summary: format!(
                                "Converged — {} opportunity/opportunities identical to previous cycle; \
                                 no agent run.",
                                opportunities.len()
                            ),
                        });
                    }
                }
                // empty + duplicate = baseline match, stay silent
            } else {
                // Finding set changed — a new finding appeared or one
                // cleared. This is also the unpause trigger (#977):
                // convergence pause releases on genuinely new findings,
                // never on a timer.
                if paused_path.exists() {
                    let _ = std::fs::remove_file(&paused_path);
                    convergence_streak = 0;
                    let _ = std::fs::write(&convergence_streak_path, "0");
                    tracing::info!("RSI: finding set changed — unpausing autonomous agent");
                }
                // Surface every opportunity to the TUI / channels, then
                // spawn the autonomous improvement agent.
                for opp in &opportunities {
                    let _ = notification_tx.send(RsiNotification::ImprovementOpportunity {
                        description: opp.clone(),
                    });
                }
                if !opportunities.is_empty() {
                    tracing::info!(
                        "RSI cycle: {} improvement opportunities surfaced (filtered: \
                         tools with >=5 events and <80% success rate over 7d, sentinel \
                         dimensions excluded; not a raw failure count), spawning autonomous agent",
                        opportunities.len()
                    );
                    let ledger_before = std::fs::metadata(&improvements_ledger_path)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    // #1696: one live read per cycle. The engine used to carry
                    // a `Config` cloned at spawn, so the provider and the
                    // fallback chain this cycle builds were the ones from
                    // process start, and a rotated key or a provider deleted
                    // from `[providers.fallback].providers` never reached RSI.
                    // Read once here rather than per call inside, so a save
                    // landing mid-cycle cannot tear the provider against the
                    // chain across two config versions.
                    let cycle_config = Config::current();
                    match run_rsi_agent_cycle(
                        repo.pool().clone(),
                        &cycle_config,
                        &opportunities,
                    )
                    .await
                    {
                        Ok(summary) => {
                            let short: String = summary.chars().take(200).collect();
                            tracing::info!("RSI agent completed: {short}");
                            let _ = notification_tx.send(RsiNotification::AgentCycleComplete {
                                summary: summary.clone(),
                            });

                            // Convergence pause (#977): summaries that say
                            // "nothing new" (or echo a user stop) repeat
                            // until the finding set changes. Count them;
                            // at threshold, stop paying for agent runs.
                            if summary_signals_convergence(&summary) {
                                convergence_streak += 1;
                                let _ = std::fs::write(
                                    &convergence_streak_path,
                                    convergence_streak.to_string(),
                                );
                                if convergence_streak >= RSI_CONVERGENCE_PAUSE_AFTER
                                    && !paused_path.exists()
                                {
                                    let _ = std::fs::write(
                                        &paused_path,
                                        convergence_streak.to_string(),
                                    );
                                    tracing::info!(
                                        "RSI: {convergence_streak} consecutive converged cycles \
                                         — pausing agent runs until the finding set changes"
                                    );
                                    let _ = notification_tx
                                        .send(RsiNotification::AgentCycleComplete {
                                            summary: format!(
                                                "RSI paused after {convergence_streak} consecutive \
                                                 converged cycles; resumes automatically when a new \
                                                 finding appears."
                                            ),
                                        });
                                }
                            } else {
                                convergence_streak = 0;
                                let _ = std::fs::write(&convergence_streak_path, "0");
                            }

                            // Zero-improvement backoff (#977): the ledger grows
                            // iff this run APPLIED an improvement. No growth →
                            // one rung out; growth → back to the base hour.
                            // Provider failures (the Err arm) don't move the
                            // ladder — they are transient, not a verdict on the
                            // data.
                            let ledger_after = std::fs::metadata(&improvements_ledger_path)
                                .map(|m| m.len())
                                .unwrap_or(0);
                            if ledger_after > ledger_before {
                                if zero_improvement_streak > 0 {
                                    tracing::info!(
                                        "RSI: improvement applied — cycle interval back to 1h"
                                    );
                                }
                                zero_improvement_streak = 0;
                            } else {
                                zero_improvement_streak += 1;
                            }
                            let _ = std::fs::write(
                                &zero_improvement_streak_path,
                                zero_improvement_streak.to_string(),
                            );
                            tracing::debug!(
                                "RSI: zero-improvement streak {zero_improvement_streak}, \
                                 next interval {}h",
                                rsi_interval_for_streak(zero_improvement_streak) / 3600
                            );
                        }
                        Err(e) => {
                            tracing::warn!("RSI agent cycle failed: {e}");
                            let _ = notification_tx.send(RsiNotification::AgentCycleFailed {
                                error: e.to_string(),
                            });
                        }
                    }
                }
            }

            // Stamp last_cycle so restarts resume from here, not from scratch
            let _ = std::fs::write(&last_cycle_path, "");
            cycle_number += 1;
            let _ = std::fs::write(&cycle_number_path, cycle_number.to_string());
        }
    }.instrument(tracing::info_span!("rsi_engine")));
}
