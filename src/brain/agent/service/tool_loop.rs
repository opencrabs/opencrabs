use super::builder::AgentService;
use super::compaction_notice::CompactionNotifier;
use super::failure_window;
use super::types::*;
use crate::brain::agent::context::AgentContext;
use crate::brain::agent::error::{AgentError, Result};
use crate::brain::provider::{ContentBlock, LLMRequest, LLMResponse, Message, Role};
use crate::brain::tools::ToolExecutionContext;
use crate::services::{MessageService, SessionService};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// How many primary-provider failures (each rescued by a successful
/// fallback) inside `failure_window::STICKY_FALLBACK_WINDOW` before the
/// fallback gets persisted into the session as the new active provider.
/// Below this, every primary failure triggers a one-shot rescue and the
/// primary is restored for the next request — so a brief outage doesn't
/// permanently demote the primary. User-stated intent (2026-05-30): "if
/// fallback 3 times successfully, the 4th it sticks"; counted over a
/// recent window rather than strictly back-to-back, because a primary
/// that alternates fail/ok never builds a run and was therefore immune
/// to the gate entirely (#1667).
const STICKY_FALLBACK_THRESHOLD: u32 = 4;

/// Default interval in seconds between mid-turn intra-loop time markers (#153).
pub const DEFAULT_TIME_MARKER_INTERVAL_SECS: u64 = 900;

/// Self-healing alert texts that reach channel posts verbatim (#1745
/// follow-up): user-facing, so they carry no em dashes. The renderer arms
/// normalize dashes anyway; `em_dash_guard_test` pins these constants.
pub(crate) const PHANTOM_RETRY_ENFORCEMENT_ALERT: &str =
    "Phantom tool calls detected: retrying with enforcement";
pub(crate) const SELF_HEAL_BUDGET_ROLLED_ALERT: &str =
    "Self-heal retry budget rolled: forcing another retry";
pub(crate) const SELF_HEAL_EXHAUSTED_ALERT: &str =
    "Self-heal exhausted: the model kept narrating without calling tools; ending the turn.";
pub(crate) const ACCOUNT_ROTATION_ALERT: &str =
    "Account rotation mid-task: retrying with continuation context";
pub(crate) const CONTINUATION_EMPTY_ALERT: &str =
    "Continuation added nothing: retrying with an anchored prompt";
pub(crate) const EMPTY_ANSWER_ANALYSIS_ALERT: &str =
    "Empty answer after data fetch: nudging the model to write the analysis";

/// Check whether the intra-turn elapsed time warrants injecting a new time notice (#153).
/// Returns `Some((notice_string, now))` if the interval has passed, or `None`.
pub(crate) fn check_intra_turn_time_marker(
    last_marker: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
    interval_secs: u64,
    brain_dir: Option<&std::path::Path>,
) -> Option<(String, chrono::DateTime<chrono::Utc>)> {
    if interval_secs == 0 {
        return None;
    }
    let elapsed = (now - last_marker).num_seconds();
    if elapsed >= interval_secs as i64 {
        let tz_info = brain_dir
            .and_then(|dir| crate::brain::timezone::GLOBAL_TZ_CACHE.resolve_from_brain_dir(dir));
        let time_str = match tz_info {
            Some(ref info) => info.format_dual_time(&now),
            None => crate::brain::timezone::format_utc_time(&now),
        };
        Some((format!("[System: Current time: {time_str}]"), now))
    } else {
        None
    }
}

/// True when a provider's reported `input_tokens` is implausibly larger than
/// the real content size (system + messages + tool schemas) — the signature of
/// an OVER-REPORTING endpoint. The zhipu "coding" endpoint was observed adding
/// a flat ~20k to every call's reported input regardless of content size (a
/// fixed additive overhead, not a tokenizer ratio): an 8.4k request came back
/// as 28.8k. tiktoken and any real model tokenizer agree within ~2× for normal
/// text, so beyond that we don't trust the provider's number for calibrating
/// the ctx counter (and the billed-cost display). `local_estimate` is the
/// pre-calibration `context.token_count` (system + messages); `tool_tokens` is
/// the tool-schema size the provider also receives but the local estimate omits.
pub(crate) fn is_implausible_token_report(
    local_estimate: usize,
    tool_tokens: usize,
    reported: usize,
) -> bool {
    let expected = local_estimate + tool_tokens;
    expected >= 1000 && reported > expected.saturating_mul(2)
}

/// Emit every retry the provider recorded, then drain them.
///
/// Called on both the success and the failure path. The success path had the
/// only drain, so a turn that retried and then gave up reported nothing at all
/// — the resilience was visible exactly when it did not matter and hidden on
/// the one occasion the user is staring at an error wondering whether anything
/// happened (#949). Draining on failure too also stops the notices leaking into
/// whichever later turn happens to succeed next.
fn emit_retry_notices(
    provider: &std::sync::Arc<dyn crate::brain::provider::Provider>,
    session_id: Uuid,
    progress_callback: Option<&ProgressCallback>,
) {
    let notices = provider.take_retry_notices();
    let Some(cb) = progress_callback else {
        // No UI wired (a2a, RSI, subagent). Still drained above, so nothing
        // carries into the next turn.
        return;
    };
    for (attempt, max, reason) in notices {
        cb(
            session_id,
            ProgressEvent::RetryAttempt {
                attempt,
                max,
                reason,
            },
        );
    }
}

/// Re-read the two flags that decide **who executes tool calls** and **who
/// owns the conversation history** for the entry that is active right now.
///
/// Both are cached once per turn because they cannot change for a plain
/// provider. A `FallbackProvider` breaks that assumption: a sticky swap
/// mid-turn can move the chain between an API provider (OpenCrabs executes
/// the tools) and an agentic CLI (the subprocess already executed them).
/// Acting on the pre-swap value ran every tool twice: two commits, two
/// `gh issue create`s, one `sed -i` applied twice into code that no longer
/// compiled (#1100).
pub(crate) fn refresh_cli_flags(
    provider: &std::sync::Arc<dyn crate::brain::provider::Provider>,
    is_cli_provider: &mut bool,
    cli_owns_context: &mut bool,
) {
    let now_cli = provider.cli_handles_tools();
    let now_owns_context = provider.cli_manages_context();
    if now_cli != *is_cli_provider || now_owns_context != *cli_owns_context {
        tracing::info!(
            "Provider swap changed tool ownership: cli_handles_tools {} -> {}, \
             cli_manages_context {} -> {} (active '{}'). Tool calls will be {} this turn.",
            *is_cli_provider,
            now_cli,
            *cli_owns_context,
            now_owns_context,
            provider
                .active_subprovider_name()
                .unwrap_or_else(|| provider.name().to_string()),
            if now_cli {
                "rendered only (the CLI ran them)"
            } else {
                "executed by OpenCrabs"
            },
        );
    }
    *is_cli_provider = now_cli;
    *cli_owns_context = now_owns_context;
}

/// What to do with a provider-reported input-token count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenReport {
    /// Trust it: this becomes the session's context count.
    Adopt(usize),
    /// The endpoint is over-reporting by an implausible factor; keep the
    /// local estimate so the ctx counter and cost stay usable.
    RejectImplausible,
    /// Too small to be a real prompt — a truncated or malformed usage block.
    BelowSanityFloor,
    /// A drop so steep it would have to mean the context was rebuilt, which
    /// this path cannot distinguish from a bad report.
    ImplausibleDrop,
}

/// Decide whether to adopt a provider's reported message-token count.
///
/// Pure so the policy can be tested directly: it used to live inline in the
/// tool loop, where the assignment was nested under a drift threshold meant
/// only for the over-reporting guard. A report agreeing within that threshold
/// was therefore never adopted, leaving the count on the local tiktoken
/// estimate and making the displayed ctx wander (#942).
pub(crate) fn evaluate_token_report(
    local_estimate: usize,
    tool_tokens: usize,
    reported: usize,
) -> TokenReport {
    const MIN_SANE: usize = 100;
    const MAX_DROP_RATIO: f64 = 0.2;
    const IMPLAUSIBILITY_DRIFT_FLOOR: f64 = 5000.0;

    if reported < MIN_SANE {
        return TokenReport::BelowSanityFloor;
    }
    if (reported as f64) < local_estimate as f64 * MAX_DROP_RATIO {
        return TokenReport::ImplausibleDrop;
    }
    // Only a LARGE disagreement can be implausible; a small one is ordinary
    // tokenizer variance and must still be adopted.
    let drift = (local_estimate as f64 - reported as f64).abs();
    if drift > IMPLAUSIBILITY_DRIFT_FLOOR
        && is_implausible_token_report(local_estimate, tool_tokens, reported)
    {
        return TokenReport::RejectImplausible;
    }
    TokenReport::Adopt(reported)
}

/// Minimum summed active-streaming time (seconds) below which a tok/s
/// reading is not credible. Burst-delivering providers (e.g. glm-5.1)
/// can dump an entire short response in a single sub-second SSE chunk,
/// making the active window ~8ms and the computed rate physically
/// impossible (37203 tok/s observed 2026-06-06). Below this floor the
/// timing is too coarse to represent a generation rate.
const MIN_ACTIVE_SECS_FOR_TOK_S: f64 = 0.3;

/// Upper bound on a believable streaming tok/s. Frontier models stream
/// to end users at tens to low-hundreds tok/s; the fastest specialized
/// inference (Groq/Cerebras on small models) tops out around 1-2k. A
/// computed rate above this is a network-burst measurement artifact,
/// not a real generation rate, so we show nothing rather than a fantasy
/// number.
const MAX_PLAUSIBLE_TOK_S: f64 = 2000.0;

/// Compute the streaming tokens-per-second for a turn, returning `None`
/// when the measurement isn't credible.
///
/// `total_output_tokens` is the turn's summed output tokens; `active_secs`
/// is the summed per-iteration active-streaming windows (idle gaps and
/// tool/approval time already excluded by `helpers::stream_complete`).
///
/// Returns `None` when:
/// - there are no output tokens, OR
/// - the active window is below `MIN_ACTIVE_SECS_FOR_TOK_S` (burst
///   delivery — timing too coarse to be a rate), OR
/// - the resulting rate exceeds `MAX_PLAUSIBLE_TOK_S` (multi-burst
///   artifact that still clears the floor).
///
/// Pure + free-function so the channel-footer rate can be unit-tested
/// without spinning the whole tool loop.
pub(crate) fn compute_streaming_tok_per_sec(
    total_output_tokens: u32,
    active_secs: f64,
) -> Option<f64> {
    if total_output_tokens == 0 || active_secs < MIN_ACTIVE_SECS_FOR_TOK_S {
        return None;
    }
    let rate = total_output_tokens as f64 / active_secs;
    if rate.is_finite() && rate <= MAX_PLAUSIBLE_TOK_S {
        Some(rate)
    } else {
        None
    }
}

/// Cross-provider model leak guard. Returns the model the next LLM call
/// should ship with, plus `Some(stale)` when the resolved pin had to be
/// substituted (so the caller can log the swap once with rich context).
///
/// Logic: if the active provider's `supported_models()` list is non-empty
/// AND the pinned model isn't in it, substitute the provider's own default
/// and report the original as stale. Empty `supported_models()` means the
/// provider hasn't declared a catalogue (no `/v1/models` impl, manual config
/// without a `models = [...]` array) — accept the pin in that case so those
/// providers still work.
///
/// Why a free function: the caller is an async method on `AgentService` with
/// DB / provider lookups, but the substitution itself is pure. Factoring it
/// out keeps the guard unit-testable from a synchronous test file under
/// `src/tests/` without spinning a runtime or mocking SessionService.
pub(crate) fn guard_cross_provider_model_leak(
    resolved: String,
    provider_default: &str,
    supported: &[String],
) -> (String, Option<String>) {
    if supported.is_empty() || supported.iter().any(|m| m == &resolved) {
        (resolved, None)
    } else {
        (provider_default.to_string(), Some(resolved))
    }
}

/// Strip ANSI escape codes from raw tool output before persisting to DB.
/// Prevents garbled artifacts in session history.
/// Build the content string sent to the LLM for a tool result.
///
/// On success, returns the raw output. On failure, includes the error
/// message plus captured output (ANSI-stripped, size-capped to 8000 chars).
pub(crate) fn build_tool_result_content(
    success: bool,
    error: Option<String>,
    output: &str,
) -> String {
    if success {
        output.to_string()
    } else {
        let mut msg =
            strip_ansi_output(&error.unwrap_or_else(|| "Tool execution failed".to_string()));
        if !output.is_empty() {
            let captured: String = strip_ansi_output(output).chars().take(8000).collect();
            msg.push_str("\n\n-- output captured before error --\n");
            msg.push_str(&captured);
            if output.len() > 8000 {
                msg.push_str("\n... (output truncated)");
            }
        }
        msg
    }
}

pub(crate) fn strip_ansi_output(raw: &str) -> String {
    strip_ansi::strip_ansi(raw)
}

/// Whether `candidate` is text the turn has already accumulated, i.e. a
/// re-emission that must not be appended a second time (#1070).
///
/// `accumulated_text` deliberately collects text from EVERY loop iteration,
/// not just the last one. That is correct for a model that narrates as it
/// works, but it also means a model that RESTATES its whole answer after a
/// tool round gets that answer delivered twice. CLI providers make this the
/// common case rather than the corner case: they are stateless per
/// invocation, so each iteration replays the full prompt including the
/// model's own previous answer, and restating beats continuing.
///
/// Whitespace-only candidates are never duplicates. `"".contains("")` is
/// trivially true, so treating them as such would swallow the separator
/// bookkeeping the call sites depend on.
///
/// Containment catches verbatim re-emission only. A reworded restatement
/// still gets through — that needs similarity scoring, not substring
/// matching, and is deliberately out of scope: a false positive here would
/// silently delete real content the user is waiting on.
pub(crate) fn is_duplicate_iteration_text(accumulated: &str, candidate: &str) -> bool {
    let trimmed = candidate.trim();
    !trimmed.is_empty() && accumulated.contains(trimmed)
}

/// What to do about a non-modification tool call that keeps recurring (#507).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepeatLoopAction {
    /// Not repeating enough to act on.
    Continue,
    /// Dominant repeat detected and we have not nudged yet: warn the model
    /// once and give it a chance to change course.
    Nudge,
    /// Repeat persisted after the nudge: break the turn.
    Break,
}

/// Decide how to handle a possibly-looping non-modification tool call.
///
/// Counts how many of the recent call signatures (a bounded window ending at
/// the current call) equal `current` — so an identical call (same name+args)
/// that DOMINATES the window is caught even when interleaved with a few other
/// calls, which the strictly-consecutive check misses. Nudge once at
/// `nudge_at`, then break at `break_at` if the model ignored the nudge and the
/// signature still dominates. Pure so the thresholds are unit tested without
/// the surrounding stream/DB machinery.
pub(crate) fn repeat_loop_action(
    recent: &[String],
    current: &str,
    window: usize,
    nudge_at: usize,
    break_at: usize,
    already_nudged: bool,
) -> RepeatLoopAction {
    let start = recent.len().saturating_sub(window);
    let repeat_in_window = recent[start..]
        .iter()
        .filter(|c| c.as_str() == current)
        .count();
    if already_nudged {
        if repeat_in_window >= break_at {
            RepeatLoopAction::Break
        } else {
            RepeatLoopAction::Continue
        }
    } else if repeat_in_window >= nudge_at {
        RepeatLoopAction::Nudge
    } else {
        RepeatLoopAction::Continue
    }
}

/// Pull the file path the agent just touched out of a successful tool
/// call, ready for the persistent recent-paths store. Returns `None`
/// for tools that don't address a single file (`bash`, `glob`, …),
/// for inputs that don't carry a path field, or for empty strings.
///
/// The 2026-04-25/26 logs showed that the agent's wrong-path failures
/// concentrate on `read_file`, `edit_file`, `grep`, `ls` — those are
/// the tools whose successful inputs are worth re-surfacing later.
/// `write_file` is included for symmetry: if the agent just wrote a
/// file, it'll likely want to read or edit it again next turn.
///
/// The returned path is resolved against `working_directory` so we
/// always store an absolute, then-collapsed `~/...` form.
pub(crate) fn extract_path_for_recent_buffer(
    tool_name: &str,
    input: &Value,
    working_directory: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let raw_path = match tool_name {
        "read_file" | "edit_file" | "write_file" | "ls" | "grep" => {
            input.get("path").and_then(|v| v.as_str())?
        }
        _ => return None,
    };
    if raw_path.trim().is_empty() {
        return None;
    }
    Some(crate::brain::tools::error::resolve_tool_path(
        raw_path,
        working_directory,
    ))
}

/// RAII guard that restores a session's provider on drop.
///
/// The fallback arms in `run_tool_loop_inner` swap the session's
/// provider to a fallback, await the fallback's stream, then restore
/// the original. That pattern was cancellation-unsafe: when the user
/// sent a new message mid-fallback, the containing future was
/// dropped, the line after `.await` never ran, and
/// `session_providers[session_id]` stayed on the fallback. The next
/// turn then built a request with the session's saved model (primary)
/// but sent it to the fallback provider — producing the
/// 2026-04-18 18:14 "400 Unknown Model, please check the model code"
/// from zhipu after it received `model=qwen3.6-plus`.
///
/// Drop runs whether the future completes, errors, or is cancelled.
struct FallbackProviderGuard<'a> {
    service: &'a AgentService,
    session_id: Uuid,
    original: Option<Arc<dyn crate::brain::provider::Provider>>,
}

impl Drop for FallbackProviderGuard<'_> {
    fn drop(&mut self) {
        if let Some(original) = self.original.take() {
            // Restore the original provider with its own paired model — the
            // guard is a transient temp-swap, so put the pair back as it was.
            let model = original
                .active_subprovider_model()
                .unwrap_or_else(|| original.default_model().to_string());
            self.service
                .swap_provider_for_session(self.session_id, original, model);
        }
    }
}

/// Detect whether a user message is a correction or negative feedback.
///
/// Public for testing — used internally by the tool loop.
///
/// Looks for patterns like "no", "wrong", "that's not what I meant", "try again",
/// "you broke", etc. Only checks the first 300 chars and requires the message
/// to be short-ish (under 500 chars) — long messages are usually new instructions,
/// not corrections.
pub fn is_user_correction(msg: &str) -> bool {
    let len = msg.len();
    // Long messages are usually new prompts, not corrections
    if !(2..=500).contains(&len) {
        return false;
    }
    let lower: String = msg.chars().take(300).collect::<String>().to_lowercase();
    // Strong negative signals — short phrases that clearly indicate correction
    const PATTERNS: &[&str] = &[
        "no,",
        "no.",
        "no!",
        "no that",
        "no not",
        "nope",
        "wrong",
        "that's not",
        "thats not",
        "not what i",
        "try again",
        "redo",
        "revert",
        "undo",
        "you broke",
        "broke it",
        "doesn't work",
        "doesnt work",
        "didn't work",
        "didnt work",
        "not working",
        "stop",
        "don't do",
        "dont do",
        "i said",
        "i asked",
        "that's wrong",
        "thats wrong",
        "not correct",
        "fix it",
        "fix this",
    ];
    PATTERNS.iter().any(|p| lower.contains(p))
}

impl AgentService {
    /// #1960: the session's live skill inventory, read from the SAME two
    /// registries the continuation stamp uses (#125/#131): slash-invoked skills
    /// (#219) unioned with the skills the session consumed by reading.
    ///
    /// Both compaction surfaces stamp this list: the continuation prompt and the
    /// summariser's §10 manifest instructions. Neither can then hold a different
    /// idea of what is active, and a manifest that prunes one of them is a
    /// detectable contradiction instead of a silent one.
    pub(crate) fn skill_inventory_for_session(&self, session_id: Uuid) -> Vec<String> {
        let active: std::collections::BTreeSet<String> = self
            .active_skills_for_session(session_id)
            .into_iter()
            .collect();
        let seen: std::collections::BTreeSet<String> =
            crate::brain::tools::seen_skills::seen_for_session(session_id)
                .into_iter()
                .collect();
        active.union(&seen).cloned().collect()
    }

    /// Build the post-compaction continuation prompt for `kind` — the single
    /// construction path shared by all five compaction sites (Regular,
    /// MidLoop, Emergency, PostTool, Manual): continuation body + plan
    /// recovery + advisory skill-inventory stamp (issue #125).
    async fn continuation_prompt(
        &self,
        session_id: Uuid,
        kind: super::compaction_prompts::CompactionKind,
    ) -> String {
        // Epoch bump (#150) BEFORE the seen-inventory read inside
        // `skill_inventory_for_session` is NOT required and after it is not
        // harmful: `note_compaction` bumps a per-session epoch counter and
        // clears nothing, so `seen_for_session` is unaffected either way
        // (decision 5: a clearing implementation would empty the stamp's
        // inventory).
        crate::brain::tools::seen_skills::note_compaction(session_id);
        let skills = self.skill_inventory_for_session(session_id);
        tracing::debug!("continuation_prompt({kind:?}): skill inventory stamp = {skills:?}");
        super::compaction_prompts::append_skill_stamp(
            super::compaction_prompts::build_continuation(
                kind,
                self.silent_compaction,
                self.auto_approve_tools,
                super::compaction_prompts::PlanRecovery::for_session(session_id).await,
            ),
            &skills,
        )
    }

    /// Core tool-execution loop — called by all public shims.
    /// `override_approval_callback` and `override_progress_callback` take
    /// precedence over the service-level callbacks (used by Telegram, etc.)
    ///
    /// `display_text_override`: when `Some`, channels can supply a human-
    /// readable user message for DB persistence/TUI display while the LLM
    /// still receives the full `user_message` (typically wrapped with
    /// channel/sender/reply metadata for context).
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(skip_all, fields(session_id = %session_id, channel))]
    pub(super) async fn run_tool_loop(
        &self,
        session_id: Uuid,
        user_message: String,
        display_text_override: Option<String>,
        model: Option<String>,
        cancel_token: Option<CancellationToken>,
        override_approval_callback: Option<ApprovalCallback>,
        override_progress_callback: Option<ProgressCallback>,
        channel: &str,
        channel_chat_id: Option<&str>,
        channel_thread_id: Option<&str>,
        track_origin: Option<PendingOrigin>,
    ) -> Result<AgentResponse> {
        // #1008: one-shot proactive fallback-chain setup suggestion. Rides
        // the first REAL user turn (never a [System: resume / background
        // note) so it lands on whichever channel the user is talking on;
        // a marker file in the profile home keeps it strictly one-shot.
        let user_message = super::fallback_suggest::maybe_inject(
            &crate::config::profile::resolve_profile_home(),
            self.has_fallback_provider(),
            user_message,
        );

        // Track this request for restart recovery. Resume turns pass
        // `track_origin == None`: a resume is a one-shot best-effort recovery,
        // so it must NOT re-insert its own pending row. Otherwise an interrupted
        // resume (cancel, crash, another restart) leaves a row that resumes the
        // same already-done session on every subsequent startup — a perpetual
        // loop with rows piling up (#729).
        //
        // Tracked turns record their origin (#12): user-initiated rows replay
        // with the continuation prompt at boot; push-initiated rows (a
        // session_notify / background-task completion woke the session) are
        // re-delivered as the original push text instead, because replaying
        // the LLM turn there could double-execute the interrupted tool call's
        // side effects.
        let pending_repo = crate::db::PendingRequestRepository::new(self.context.pool());
        let request_id = Uuid::new_v4();
        if let Some(origin) = track_origin
            && let Err(e) = pending_repo
                .insert(
                    request_id,
                    session_id,
                    &user_message,
                    channel,
                    channel_chat_id,
                    channel_thread_id,
                    origin.as_db_str(),
                )
                .await
        {
            tracing::warn!("Failed to track pending request: {}", e);
        }

        // Per-call effective callbacks (override wins over service-level).
        // Track whether an explicit per-call override was provided so we can honour
        // channel approval callbacks even when the factory set auto_approve_tools=true.
        let has_override_approval = override_approval_callback.is_some();
        let approval_callback: Option<ApprovalCallback> =
            override_approval_callback.or_else(|| self.approval_callback.clone());
        let has_progress_override = override_progress_callback.is_some();
        // A channel passes its own callback per message, and `or_else` picked
        // exactly one — so a Telegram-driven turn never reached the
        // service-level callback the TUI installs, and every counter the TUI
        // derives from progress events sat frozen while the turn ran (#1092).
        //
        // Only non-textual telemetry is mirrored. Events carrying text
        // (StreamingChunk, IntermediateText) also drive the TUI's own display
        // through `cli/ui.rs`, so forwarding them would render the mirrored
        // turn's content a second time. Counters are safe; content is not.
        let progress_callback: Option<ProgressCallback> =
            match (override_progress_callback, self.progress_callback.clone()) {
                (Some(channel_cb), Some(service_cb)) => {
                    Some(Arc::new(move |sid: Uuid, event: ProgressEvent| {
                        if matches!(event, ProgressEvent::TokenCount(_)) {
                            service_cb(sid, event.clone());
                        }
                        channel_cb(sid, event);
                    }) as ProgressCallback)
                }
                (Some(channel_cb), None) => Some(channel_cb),
                (None, service_cb) => service_cb,
            };
        // Effective question callback: per-call override wins over the
        // service-level fallback. Channels with native button surfaces
        // pass their own callback per message; everyone else passes
        // Notify TUI when a remote channel starts/finishes processing so it can
        // block concurrent sends on the same session and avoid garbled display.
        if has_progress_override && let Some(ref tx) = self.session_updated_tx {
            let _ = tx.send(crate::brain::agent::ChannelSessionEvent::ProcessingStarted(
                session_id,
            ));
        }

        // Run the actual loop, rotating the chain if the loop detector kills it.
        //
        // The abort inside the loop returns `AnnouncementLoop`, a
        // provider-attributable error chosen so a loop-detector kill could
        // reach the fallback walk (#1023). It never did: the walk lives in the
        // arm that handles an error from the PROVIDER CALL, and this arrives by
        // `return` from deep inside the loop, so it bypassed the arm entirely
        // and the turn was dropped with ten providers untried. Retrying here
        // puts it back on the chain, which is what #1023 said it should do.
        //
        // Bounded by the chain itself: `force_next_fallback` reports false when
        // there is nowhere left to go, and the counter is only a backstop.
        const MAX_CHAIN_ROTATIONS: u32 = 12;
        let mut rotations: u32 = 0;
        let result = loop {
            let attempt = self
                .run_tool_loop_inner(
                    session_id,
                    user_message.clone(),
                    display_text_override.clone(),
                    model.clone(),
                    cancel_token.clone(),
                    has_override_approval,
                    approval_callback.clone(),
                    has_progress_override,
                    progress_callback.clone(),
                    // Only the first entry persists the turn start. A rotation
                    // reuses the same message, so the user row already exists.
                    rotations == 0,
                )
                .await;

            let killed_by_loop_detector = matches!(
                &attempt,
                Err(AgentError::Provider(
                    crate::brain::provider::ProviderError::AnnouncementLoop(_)
                ))
            );
            if !killed_by_loop_detector || rotations >= MAX_CHAIN_ROTATIONS {
                break attempt;
            }

            let fb = self.provider_for_session(session_id);
            if !fb.force_next_fallback(
                "announcement_loop",
                &self.provider_model_for_session(session_id),
            ) {
                // Chain exhausted: every provider was asked and each hit the
                // loop. Now the error is the honest answer, and it says so,
                // naming the dropped call and that nothing is queued (#1397).
                break attempt.map_err(|e| super::loop_break::chain_exhausted(e, rotations + 1));
            }
            rotations += 1;
            tracing::warn!(
                "Loop-detector kill: handing the turn to the next provider (rotation \
                 {rotations}) instead of dropping it"
            );
        };

        if has_progress_override && let Some(ref tx) = self.session_updated_tx {
            let _ =
                tx.send(crate::brain::agent::ChannelSessionEvent::ProcessingFinished(session_id));
        }

        // Request finished — delete the tracking row. Only PROCESSING rows
        // survive (meaning the process crashed/restarted mid-request).
        // Untracked (resume) turns never inserted a row, so nothing to clean up.
        //
        // Exception: a turn cancelled *because the process is shutting down*
        // keeps its row, so the next boot resumes it (#1462). Quitting with
        // Ctrl+C twice cancels the in-flight token before exiting, and the
        // cancelled turn then unwound through here and deleted its own
        // recovery ticket — the work was gone with nothing left to resume.
        // A turn the user deliberately stopped (Esc twice, /stop, /discard)
        // is still deleted: they abandoned it and must not have it replayed.
        // Both arrive as AgentError::Cancelled, so the shutdown flag is the
        // only thing that tells them apart.
        let cancelled_by_shutdown = super::shutdown::keeps_recovery_row(&result);
        if cancelled_by_shutdown {
            tracing::info!(
                "Turn cancelled by shutdown — keeping the recovery row so the next boot \
                 resumes it (#1462)"
            );
        }
        if track_origin.is_some()
            && !cancelled_by_shutdown
            && let Err(e) = pending_repo.delete(request_id).await
        {
            tracing::warn!("Failed to clean up pending request: {}", e);
        }

        result
    }

    /// Inner tool loop — separated so `run_tool_loop` can wrap with request tracking.
    ///
    /// `display_text_override`: optional clean message for DB persistence/TUI.
    /// The LLM context still gets `user_message` (full agent input including
    /// channel-injected sender metadata, reply context, group history, etc.).
    /// When `None`, behaves identically to before — `user_message` is used
    /// for both context and DB.
    /// Honor a mid-turn manual provider/model switch AFTER a turn finishes.
    /// If the user switched while this turn was running (`start_epoch` no
    /// longer matches), re-install their pinned provider+model pair in memory
    /// and persist it to the session DB row — so the next turn (and channel
    /// restore) use the user's pick, not the fallback the turn happened to
    /// take. Called only after the response is fully built, so it cannot drop
    /// or change the current turn's result. No-op when nothing changed.
    async fn finalize_manual_switch(
        &self,
        session_id: Uuid,
        start_epoch: u64,
        session_service: &SessionService,
    ) {
        let Some(model) = self.restore_manual_switch_if_changed(session_id, start_epoch) else {
            return;
        };
        let provider_name = self.provider_name_for_session(session_id);
        if let Ok(Some(mut s)) = session_service.get_session(session_id).await {
            s.provider_name = Some(provider_name.clone());
            s.model = Some(model.clone());
            if let Err(e) = session_service.update_session(&s).await {
                tracing::warn!("finalize_manual_switch: DB persist failed for {session_id}: {e}");
            }
        }
        tracing::info!(
            "Restored user's mid-turn switch for session {session_id}: {provider_name}/{model}"
        );
    }

    /// Everything the conversation has actually seen, for fact-checking a
    /// text-only iteration against it (#1423).
    ///
    /// Evidence only: what tools printed, what was actually run, and the user's
    /// own words. Assistant text and thinking are deliberately left out,
    /// because the model's earlier claims must not vouch for its later ones. A
    /// sha invented three iterations ago would otherwise count as known by the
    /// time it is repeated, and the check would go quiet on exactly the
    /// fabrication it exists to catch.
    ///
    /// Built on demand rather than accumulated, so it also covers results from
    /// earlier turns. That is what keeps the check honest about a sha the model
    /// legitimately cites from work it did ten minutes ago.
    fn conversation_evidence(context: &AgentContext, turn_tool_output: &[String]) -> String {
        let mut evidence = String::new();
        for msg in &context.messages {
            for block in &msg.content {
                match block {
                    ContentBlock::ToolResult { content, .. } => {
                        evidence.push_str(content);
                        evidence.push('\n');
                    }
                    ContentBlock::Text { text } if msg.role == Role::User => {
                        evidence.push_str(text);
                        evidence.push('\n');
                    }
                    // What was actually run: a sha in a `git show` argument is
                    // as real as one in its output.
                    ContentBlock::ToolUse { input, .. } => {
                        evidence.push_str(&input.to_string());
                        evidence.push('\n');
                    }
                    _ => {}
                }
            }
        }
        for out in turn_tool_output {
            evidence.push_str(out);
            evidence.push('\n');
        }
        evidence
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_tool_loop_inner(
        &self,
        session_id: Uuid,
        user_message: String,
        display_text_override: Option<String>,
        model: Option<String>,
        cancel_token: Option<CancellationToken>,
        has_override_approval: bool,
        approval_callback: Option<ApprovalCallback>,
        has_progress_override: bool,
        progress_callback: Option<ProgressCallback>,
        // True on the first entry of a turn. The rotation loop in `run_tool_loop`
        // re-enters this function with the SAME `user_message` when a provider is
        // killed by the loop detector, and turn-start persistence lives here, so
        // every rotation wrote the user's message a second time (#2009).
        first_attempt: bool,
    ) -> Result<AgentResponse> {
        // Restore the directory `/cd` persisted for this session BEFORE anything
        // else in this function (#1810). Everything below (context build, brain
        // assembly, tool-context creation) resolves the session's cwd, and a
        // touch before this restore would create the handle from the launch
        // directory of the process hosting the channels, leaving the DB row
        // ignored forever. See `restore_persisted_working_directory`.
        self.restore_persisted_working_directory(session_id).await;

        // #1776 seam 3: a fresh turn invalidates the claude-task turn
        // markers. Notifications arriving this turn for tasks NOT started
        // this turn are post-exit survivors and get delivered synthetically;
        // tasks started this turn stay silent (claude sees those natively).
        // retain, not remove: other sessions' in-flight turns keep theirs.
        self.claude_turn_tasks
            .lock()
            .expect("claude turn-task lock")
            .retain(|(sid, _)| *sid != session_id);

        // Snapshot the manual-switch epoch at turn start. If the user
        // switches provider/model while this turn is in flight, an automatic
        // fallback the turn takes could otherwise stick over their pick. We
        // detect the change AFTER the turn completes (see the restore near
        // the final return) and re-apply the user's pick then — off the
        // completion path, so it can never drop or contaminate the request.
        let start_switch_epoch = self.manual_switch_epoch(session_id);

        // Get or create session
        let session_service = SessionService::new(self.context.clone());
        let session = session_service
            .get_session(session_id)
            .await
            .map_err(AgentError::db)?
            .ok_or(AgentError::SessionNotFound(session_id))?;

        // Load conversation context with budget-aware message trimming
        let message_service = MessageService::new(self.context.clone());
        let all_db_messages = message_service
            .list_messages_for_session(session_id)
            .await
            .map_err(AgentError::db)?;

        // Resolve model name: explicit caller arg > session's saved model >
        // current provider's default. The session.model fallback is critical
        // for restart-recovery: when the resume task races with TUI session
        // restore, reading provider.default_model() can capture the wrong
        // (pre-swap) provider's model. session.model is read from DB and is
        // always provider-correct.
        // Mutable so the sticky-fallback path can rebind this to the
        // successful fallback's model — otherwise subsequent tool-loop
        // iterations in the same turn rebuild requests with the primary
        // model name pointed at the fallback provider → 400 unknown model.
        // A session must run on ITS OWN saved provider, not the global default
        // (#704). After a restart `session_providers` is empty, so without this
        // restore a resume/channel/not-yet-switched turn would fall to the
        // global default and `guard_cross_provider_model_leak` would silently
        // remap the model — a switch the user never made.
        self.ensure_session_provider_restored(
            session_id,
            session.provider_name.as_deref(),
            session.model.as_deref(),
        )
        .await;
        // Then route by plan state (#792): drafting and executing can each run
        // on their own provider/model. Must come AFTER the restore above, or
        // the restore would measure the override as the session's own pair;
        // must come BEFORE the read below, which is what the turn runs on.
        // A no-op unless the config sets plan-mode keys.
        self.apply_plan_mode_provider(session_id).await;
        let session_provider = self.provider_for_session(session_id);
        // Did the turn START on the session's OWN saved provider (#705)? If the
        // saved provider is set and doesn't match the resolved one, the turn is
        // running on the wrong provider (a #704 restore gap) — an involuntary
        // remap that `complete_response` must NOT persist over the saved pair. A
        // session with no saved provider counts as matched (it captures its
        // first pair legitimately); a real fallback starts matched and diverges
        // later, so it stays `true` and persists via ProviderSwitched.
        let started_on_session_provider = super::helpers::provider_matches_session(
            session.provider_name.as_deref(),
            session_provider.name(),
        );
        let resolved_model = model
            .or_else(|| session.model.clone())
            .unwrap_or_else(|| session_provider.default_model().to_string());
        let provider_default = session_provider.default_model().to_string();
        let supported = session_provider.supported_models();
        let (mut model_name, leaked) =
            guard_cross_provider_model_leak(resolved_model, &provider_default, &supported);
        if let Some(stale) = leaked {
            tracing::warn!(
                "Stale model pin '{}' for session {} is not in active provider '{}' catalogue ({} entries) — \
                 substituting provider default '{}'. \
                 This usually means an earlier sticky-fallback or provider switch left a cross-provider model pinned in session.model.",
                stale,
                session_id,
                session_provider.name(),
                supported.len(),
                provider_default
            );
        }
        let context_window = self.context_limit_for_session(session_id);

        // Load from last compaction point — find the last CONTEXT COMPACTION marker
        // and only load messages from there forward. No arbitrary trimming.
        let mut db_messages = Self::messages_from_last_compaction(all_db_messages);

        // Auto-title: fire a one-shot background LLM call using the current
        // user_message as the seed. Works on ALL channels (TUI, Telegram,
        // Discord, Slack, etc). Issue #118 + #120.
        //
        // Why no `db_message_count >= 1` guard: the previous version only
        // fired from the SECOND user message onward (because db_message_count
        // is taken before the current message is stored). The reporter's
        // sessions all had exactly 1 turn each (one /new, one message, done),
        // so auto-title never ran and every session sat with the default
        // `Telegram: DM <name> (<id>) [chat:<id>]` title forever — looking
        // identical in `/sessions`. We already have `user_message` in scope,
        // so fire on the first turn directly.
        //
        // Why a reset-on-failure path: `mark_auto_title_attempted` runs
        // BEFORE the LLM call to prevent race conditions if the user sends
        // a second message while the title generation is still in flight.
        // But if the LLM call fails (provider down, 5xx, timeout), the flag
        // stays true forever and the session is stuck. The Err arm now
        // resets the flag so the next message retries.
        // Link the session to the project its working directory names, on the
        // same first turn that titles it (#1445). Deliberately NOT inside the
        // spawned title future: the title is an LLM call that can fail, time
        // out, or hit a rate-limited provider, while this is a string match
        // and one UPDATE. Nesting it would make a provider outage leave
        // sessions unlinked, which is the failure this fixes. Gated on the
        // first turn so it is attempted once, and idempotent besides.
        if !session.auto_title_attempted && session.project_id.is_none() {
            let project_svc = crate::services::ProjectService::new(self.context.clone());
            if let Err(e) = project_svc.link_session_by_directory(&session).await {
                tracing::warn!(error = %e, "failed to link session to a project");
            }
        }

        if !user_message.trim().is_empty()
            && !session.auto_title_attempted
            && session
                .title
                .as_deref()
                .map(|t| t.is_empty() || Self::is_default_channel_title(t))
                .unwrap_or(true)
        {
            let title_provider = self.provider_for_session(session_id);
            let title_model = model_name.clone();
            // Use the clean display text for title generation when available.
            // Channels inject preamble blocks ([Channel: …], [Reaction
            // directive: …], [Recent group history …]) into user_message for
            // LLM context, but the title LLM should only see the actual user
            // text. display_text_override is already the clean message;
            // strip_channel_preamble is defense-in-depth for the fallback. #688
            let title_source = display_text_override
                .as_deref()
                .map(Self::strip_channel_preamble)
                .unwrap_or_else(|| Self::strip_channel_preamble(&user_message));
            let title_msg = title_source.chars().take(500).collect::<String>();
            let session_svc = SessionService::new(self.context.clone());
            // Capture the channel BEFORE spawn so the new title can fan
            // out to the TUI/footer the moment it lands in DB. Without
            // this, the footer kept showing "New Chat" after Ctrl+N
            // until the user switched sessions and load_session re-read
            // the row from DB.
            let title_update_tx = self.session_updated_tx.clone();
            // Mark auto_title_attempted BEFORE spawning to prevent race
            // conditions where the next message arrives before the
            // background task completes. The Err arm resets it.
            if let Err(e) = session_svc.mark_auto_title_attempted(session_id).await {
                tracing::warn!(error = %e, "failed to mark auto title attempted");
            }
            // Capture the old title to preserve channel prefix
            let old_title = session.title.clone().unwrap_or_default();
            tokio::spawn(async move {
                let title_request = LLMRequest::new(
                    title_model,
                    vec![Message::user(format!(
                        "Generate a concise session title (3-7 words) based on this user message. \
                         Return ONLY the title text, nothing else. No quotes, no punctuation at the end.\n\n\
                         Message: {}",
                        title_msg
                    ))],
                );
                match title_provider.complete(title_request).await {
                    Ok(response) => {
                        // Use the thinking-aware extractor — reasoning
                        // models sometimes return ONLY a Thinking block
                        // for short prompts and never produce a Text
                        // block. The old `extract_text_from_response`
                        // returned "" in that case, sessions stayed
                        // stuck on default titles forever (issue #121).
                        let clean_title = Self::extract_title_candidate(&response);
                        if !clean_title.is_empty() {
                            // Preserve channel prefix if it existed
                            let prefix = Self::extract_channel_prefix(&old_title);
                            // Preserve [chat:ID] suffix — critical for session resolution
                            // via find_session_by_title_suffix (issue #115)
                            let chat_suffix = Self::extract_chat_id_suffix(&old_title);
                            let final_title = if prefix.is_empty() {
                                if chat_suffix.is_empty() {
                                    clean_title
                                } else {
                                    format!("{} {}", clean_title, chat_suffix)
                                }
                            } else if chat_suffix.is_empty() {
                                format!("{}{}", prefix, clean_title)
                            } else {
                                format!("{}{} {}", prefix, clean_title, chat_suffix)
                            };
                            match session_svc
                                .update_session_title(session_id, Some(final_title.clone()))
                                .await
                            {
                                Ok(()) => {
                                    if let Some(tx) = title_update_tx.as_ref()
                                        && let Err(e) = tx.send(
                                            crate::brain::agent::ChannelSessionEvent::TitleUpdated(
                                                session_id,
                                                final_title,
                                            ),
                                        )
                                    {
                                        tracing::warn!(
                                            "Auto-title: title written to DB but TUI notify channel \
                                             closed, footer will lag until next session switch: {}",
                                            e
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Auto-title: update_session_title failed for {}: {}",
                                        session_id,
                                        e
                                    );
                                }
                            }
                        } else {
                            // Empty/unusable title — allow the next message
                            // to retry. Same recovery path as the Err arm.
                            if let Err(e) = session_svc.reset_auto_title_attempted(session_id).await
                            {
                                tracing::warn!(error = %e, "failed to reset auto title attempted");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Auto-title generation failed for session {}: {} — resetting flag so the next message retries",
                            session_id,
                            e,
                        );
                        if let Err(e) = session_svc.reset_auto_title_attempted(session_id).await {
                            tracing::warn!(error = %e, "failed to reset auto title attempted");
                        }
                    }
                }
            });
        }

        // Detect CLI + local provider up front.
        //
        // `is_cli_provider` controls TWO unrelated behaviors:
        //   1. Skip local tool execution (CLI runs tools internally)
        //   2. Skip OpenCrabs-side context compaction (CLI persists session)
        //
        // Both are re-read whenever a sticky fallback swap fires mid-turn
        // (see `refresh_cli_flags` at the swap site below): a chain that
        // rotates between an API provider and an agentic CLI changes who
        // executes the tools, and a stale `false` means both sides run them
        // (#1100).
        //
        // `is_local_provider` relaxes the phantom-tool-call detector so
        // local llama.cpp/MLX models that answer in prose when they should
        // have called a tool get re-prompted, matching what Unsloth Studio
        // does out of the box.
        let (mut is_cli_provider, mut cli_owns_context, is_dialagram, is_local_provider) = {
            let p = self.provider_for_session(session_id);
            let base = p.base_url();
            // Detect dialagram by base_url — users add it as a custom
            // provider under any name they choose (typos included),
            // but the proxy URL is always https://www.dialagram.me/...
            let is_dialagram = base
                .map(|u| u.to_lowercase().contains("dialagram.me"))
                .unwrap_or(false);
            let is_local = base
                .map(crate::brain::provider::factory::is_local_base_url)
                .unwrap_or(false);
            (
                p.cli_handles_tools(),
                p.cli_manages_context(),
                is_dialagram,
                is_local,
            )
        };

        // Strip persisted `<!-- tools-v2: ... -->` ledgers and
        // `<!-- reasoning -->` blocks from DB content before loading into the
        // LLM context. The markers exist for TUI replay/cancel-persist;
        // feeding them back teaches the model to echo the JSON tool-result
        // format verbatim (a closed feedback loop that produces raw JSON dumps
        // and dropped streaming responses for API providers like qwen3.6-plus
        // on OpenRouter).
        //
        // This ran for API providers only, on the belief that the CLI
        // subprocess never saw the content. It did: the CLI prompt builder
        // flattens every text block verbatim, so a session that had run on
        // API providers handed the CLI its whole replay ledger and every
        // other model's reasoning as conversation, about five times the real
        // text, and could not be switched to claude-cli at all (#1522). The
        // in-memory copy is all that is cleaned; rows are only ever appended
        // to, so TUI replay keeps its ledgers.
        //
        // preserve_thinking models (Qwen Model Studio / DashScope, Moonshot
        // kimi) require reasoning returned as the separate `reasoning_content`
        // field and reject it inside `content` (#654), so for them the
        // reasoning is hoisted into the `thinking` column and re-emitted by
        // the encoder. A CLI provider gets no hoist: another model's chain of
        // thought must not reach its prompt at all.
        let preserve_thinking = !is_cli_provider
            && crate::brain::provider::custom_openai_compatible::preserves_thinking(
                self.provider_for_session(session_id).base_url(),
                &model_name,
            );
        super::context_rows::clean_rows_for_llm(&mut db_messages, preserve_thinking);

        let mut context =
            AgentContext::from_db_messages(session_id, db_messages, context_window as usize);

        // Add system brain if available (count its tokens so context.token_count
        // reflects the full API input from the start — prevents gross undercount
        // that causes the TUI context counter to jump wildly on first calibration)
        // `live_system_brain` rebuilds from disk when a brain file changed so
        // edits take effect on the next turn without a restart (#213). The
        // session-aware variant patches the Runtime Info Model/Provider lines
        // to the session's resolved pair, not the startup default.
        if let Some(brain) = self.live_system_brain_for_session(session_id) {
            // mimo narrates/text-emits tool calls instead of using the
            // structured field; remind it up front (the self-heal + the
            // <tool_call_list> parser are the after-the-fact safety nets).
            let brain = if super::helpers::is_mimo_model(&model_name) {
                format!("{brain}\n\n{}", super::helpers::MIMO_TOOL_CALL_HINT)
            } else {
                brain
            };
            context.token_count += AgentContext::estimate_tokens(&brain);
            context.system_brain = Some(brain);
        }

        // Re-inject active skill bodies into system brain so they survive
        // compaction (#219). Skills are invoked as UserPrompt messages that
        // get compacted away; this ensures the full instructions are always
        // present in the system prompt for the current session.
        let active_skills = self.active_skills_for_session(session_id);
        if !active_skills.is_empty() {
            let skills = crate::brain::skills::load_all_skills();
            let mut skill_section = String::new();
            for skill in &skills {
                if active_skills.contains(&skill.slash_name) {
                    // `prompt_body()` carries the review-gate reminder for
                    // flagged skills so the gate survives compaction too.
                    skill_section.push_str(&format!(
                        "\n\n--- Active Skill: {} ---\n{}",
                        skill.slash_name,
                        skill.prompt_body()
                    ));
                }
            }
            if !skill_section.is_empty()
                && let Some(ref mut brain) = context.system_brain
            {
                brain.push_str(&skill_section);
                context.token_count += AgentContext::estimate_tokens(&skill_section);
            }
        }

        // Emit token count immediately after DB reload so the TUI reflects the
        // real post-compaction value. Without this, the TUI shows the stale
        // pre-compaction count from the previous request until the API responds.
        if let Some(ref cb) = progress_callback {
            cb(session_id, ProgressEvent::TokenCount(context.token_count));
        }

        // Detect user corrections / negative feedback and record automatically.
        // Only fires on real user messages (not system continuations).
        if !user_message.starts_with("[System:")
            && !user_message.starts_with("[SYSTEM:")
            && is_user_correction(&user_message)
        {
            self.record_provider_feedback(
                session_id,
                "user_correction",
                "user_message",
                Some(
                    &display_text_override
                        .as_deref()
                        .unwrap_or(&user_message)
                        .chars()
                        .take(200)
                        .collect::<String>(),
                ),
            );
        }

        // Check for manual /compact before user_message is consumed
        let is_manual_compact = user_message.contains("[SYSTEM: Compact context now.");

        // Build user message — `<<IMG:path>>` markers become text hints; the
        // agent views images via analyze_image (no inline image_url content).
        //
        // Append the plan-mode per-turn reminder (Editing template rules, or the
        // Active incomplete-task nudge) so it rides at the END of the prompt
        // every turn, exactly like the simple send_message path does in
        // prepare_message_context. This tool-loop path drives Telegram and every
        // channel that uses tools; it previously never injected the reminder, so
        // in plan mode the agent was never told the plan-template rules and wrote
        // an empty template that the approval gate then refused forever. The DB
        // still stores the clean `user_message` below (persistence uses it
        // directly), so the reminder is context-only and never piles up (#571
        // follow-up).
        let brain_dir = self.brain_workspace_path();
        let context_user_message =
            Self::augment_user_message(session_id, &user_message, brain_dir.as_deref()).await;
        let user_msg = Self::build_user_message(&context_user_message);
        context.add_message(user_msg);

        // Save user message to database (text only — images are ephemeral).
        // Skip DB persistence for internal system continuations (restart recovery)
        // — they go to context for the LLM but never appear in chat history.
        // Redact secrets so Bearer tokens, API keys etc. from cron prompts
        // never persist to DB or appear in TUI chat history.
        //
        // When a channel handler supplies `display_text_override`, the DB row
        // (and therefore the TUI chat history) shows that clean text instead
        // of the LLM-context-augmented `user_message`. This keeps Telegram /
        // Discord / Slack / WhatsApp / Trello sessions readable in OpenCrabs
        // — no sender brackets, no reply context, no recent-history dump.
        // `first_attempt` keeps a rotation from re-writing the user row: the
        // retry re-enters with the same message and the history would otherwise
        // show it once per provider tried (#2009).
        let is_system_continuation = user_message.starts_with("[System:");
        if !is_system_continuation && first_attempt {
            let raw_for_db = display_text_override.as_deref().unwrap_or(&user_message);
            let safe_message = crate::utils::sanitize::redact_secrets(raw_for_db);
            let _user_db_msg = message_service
                .create_message(session_id, "user".to_string(), safe_message)
                .await
                .map_err(AgentError::db)?;
        }

        // Create assistant message placeholder NOW for real-time persistence.
        // We'll append content as we go and update with final tokens at the end.
        let mut assistant_db_msg = message_service
            .create_message(session_id, "assistant".to_string(), String::new())
            .await
            .map_err(AgentError::db)?;

        // Manual /compact: force compaction, persist summary to DB, return a brief
        // confirmation to the user. The full summary is for the agent, not the user.
        if is_manual_compact {
            let compacted = self
                .compact_context(
                    session_id,
                    &mut context,
                    &model_name,
                    None,
                    CompactionNotifier::manual_from(session_id, progress_callback.as_ref()),
                )
                .await;
            // The summariser retried in silence before #1520: its provider
            // records the attempts, nothing drained them until the next chat
            // turn. Surface them here, success or failure.
            emit_retry_notices(
                &self.provider_for_session(session_id),
                session_id,
                progress_callback.as_ref(),
            );
            match compacted {
                Ok((summary, applied_marker)) => {
                    // Persist compaction marker to DB so restarts load from this
                    // point. #1928: the applied marker the swap welded into the
                    // live context, not a banner rebuilt here.
                    message_service
                        .create_message(session_id, "user".to_string(), applied_marker)
                        .await
                        .map_err(AgentError::db)?;
                    // Persist summary as the assistant response (for DB/search continuity)
                    message_service
                        .append_content(assistant_db_msg.id, &summary)
                        .await
                        .map_err(AgentError::db)?;

                    // Add a brief continuation prompt to context — matches
                    // auto-compaction behavior but uses a short sentence instead
                    // of the full POST-COMPACTION PROTOCOL. Persisted to DB so
                    // the next turn sees it.
                    let cont_text = self
                        .continuation_prompt(
                            session_id,
                            super::compaction_prompts::CompactionKind::Manual,
                        )
                        .await;
                    message_service
                        .create_message(session_id, "user".to_string(), cont_text.clone())
                        .await
                        .map_err(AgentError::db)?;
                    context.add_message(Message::user(cont_text));

                    if let Some(ref cb) = progress_callback {
                        cb(session_id, ProgressEvent::TokenCount(context.token_count));
                    }

                    // Return a brief confirmation to the user — not the full internal summary.
                    // `.round()`, not a truncating cast: the compaction log
                    // line formats the same figure with `{:.0}`, and the two
                    // printed different percentages for one event (#1676).
                    let pct = context.usage_percentage().round() as u32;
                    let confirmation = format!(
                        "✅ Context compacted — now at {}% ({} tokens).",
                        pct, context.token_count
                    );
                    return Ok(AgentResponse {
                        message_id: assistant_db_msg.id,
                        content: confirmation,
                        stop_reason: Some(crate::brain::provider::StopReason::EndTurn),
                        usage: crate::brain::provider::TokenUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                            ..Default::default()
                        },
                        context_tokens: context.token_count as u32,
                        tokens_per_second: None,
                        cost: 0.0,
                        model: model_name,
                        provider_name: self.provider_name_for_session(session_id),
                        started_on_session_provider,
                    });
                }
                Err(e) => {
                    tracing::error!("Manual compaction failed: {}", e);
                    let error_msg = format!(
                        "Compaction failed: {}\n\nThis can happen if:\n\
                         - The session has too few messages to summarize\n\
                         - The AI provider returned an error\n\
                         - The database is locked or inaccessible\n\n\
                         Try again, or continue the conversation normally — \
                         auto-compaction will trigger at 65% context usage.\n\n{}",
                        e,
                        super::clear::CLEAR_HINT
                    );
                    message_service
                        .append_content(assistant_db_msg.id, &error_msg)
                        .await
                        .map_err(AgentError::db)?;

                    return Ok(AgentResponse {
                        message_id: assistant_db_msg.id,
                        content: error_msg,
                        stop_reason: Some(crate::brain::provider::StopReason::EndTurn),
                        usage: crate::brain::provider::TokenUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                            ..Default::default()
                        },
                        context_tokens: context.token_count as u32,
                        tokens_per_second: None,
                        cost: 0.0,
                        model: model_name,
                        provider_name: self.provider_name_for_session(session_id),
                        started_on_session_provider,
                    });
                }
            }
        }

        // CLI providers manage their own context window internally.
        // Keep our tiktoken estimate from DB messages as-is — it's a reasonable
        // approximation. Don't reset to 0, because CLI cache tokens (used for
        // calibration below) are cumulative across internal tool rounds, not
        // the actual context window size.

        // Auto-compact: triggers at >65% usage.
        // Skip ONLY when the CLI manages its own session (qwen-code with
        // --resume). Claude CLI handles tools internally but DOES NOT
        // manage the context we feed it via stdin — we send the full
        // conversation each turn, so we MUST compact for Claude CLI too.
        // The other 3 call sites in this file already use cli_owns_context;
        // this one was using is_cli_provider, which let Claude CLI
        // bypass compaction entirely and inflated the ctx counter past
        // 200% (user report 2026-05-04: 484k/200k = 242%).
        let compaction_result = if cli_owns_context {
            None
        } else {
            self.enforce_context_budget(
                session_id,
                &mut context,
                &model_name,
                cancel_token.as_ref(),
                &progress_callback,
                super::compaction::BudgetPhase::TurnStart,
            )
            .await
        };

        if let Some(ref outcome) = compaction_result {
            // Persist compaction marker to DB so restarts load from this point
            if let Err(e) = message_service
                .create_message(session_id, "user".to_string(), outcome.marker(""))
                .await
            {
                tracing::error!("Failed to persist compaction marker to DB: {}", e);
            }

            let cont_text = self
                .continuation_prompt(
                    session_id,
                    super::compaction_prompts::CompactionKind::Regular,
                )
                .await;
            context.add_message(Message::user(cont_text));
        }

        // Create tool execution context. The working directory is per-session
        // (#703): resolve THIS session's own handle so a `cd` here mutates only
        // this session's cwd, and a concurrent session's `cd` can never move it.
        let session_cwd = self.working_dir_handle_for_session(session_id);
        // `tool_context.auto_approve` seeds from the POLICY, but a per-call
        // override callback means this surface CAN ask (ACP, channels) — the
        // override must win over `auto_approve_tools=true`, or config
        // `approval_policy = "auto-always"` silently suppresses the ask the
        // override exists to make. The loop gate below already honours the
        // override; this mirror must not un-do it.
        let context_auto_approve = self.auto_approve_tools && !has_override_approval;
        let mut tool_context = ToolExecutionContext::new(session_id)
            .with_auto_approve(context_auto_approve)
            .with_working_directory(
                session_cwd
                    .read()
                    .expect("session working_directory lock poisoned")
                    .clone(),
            );
        tool_context.sudo_callback = self.sudo_callback.clone();
        tool_context.ssh_callback = self.ssh_callback.clone();
        tool_context.shared_working_directory = Some(Arc::clone(&session_cwd));
        tool_context.service_context = Some(self.context.clone());
        tool_context.progress_callback = progress_callback.clone();
        tool_context.background_manager = self.background_manager.clone();
        tool_context.plan_session_override = self.plan_session_override;
        tool_context.subagent_manager = self.subagent_manager.clone();
        // Vision resolves the CURRENT provider first (#1318); a tool cannot
        // ask AgentService for it, and this loop has both.
        tool_context.session_provider = Some(self.provider_name_for_session(session_id));
        // #1802: the session_notify kill switch rides the same single
        // stamping site; the mirror reads [agent] session_notify_enabled.
        tool_context.session_notify_enabled = crate::config::Config::current()
            .agent
            .session_notify_enabled;
        // Ambient conversation origin (#148): derived HERE — the single
        // stamping site, mirroring `session_provider` directly above — from
        // the session ownership maps via the channel manager. `None` on
        // surfaces without a manager or binding (cron, CLI, sub-agents),
        // where `here` resolution must be refused, never guessed.
        tool_context.origin_target = self.origin_target_for_session(session_id).await;
        // Live resolution world (#148): the same channel manager, handed to
        // targeting tools so `oc://` URLs resolve against the real ownership
        // maps. `None` together with `origin_target` — same refusal law.
        tool_context.world = self.channel_manager().map(|mgr| {
            mgr as Arc<dyn crate::channels::target_resolver::TargetResolution + Send + Sync>
        });
        tool_context.parent_tool_registry = Some(self.tool_registry.clone());
        // #129 belt-and-braces: interactive-only tools check this flag and
        // hard-error instead of parking a verdict nobody sees.
        tool_context.headless = self.headless;

        // Tool execution loop
        let mut iteration = 0;
        // Number of tools that completed SUCCESSFULLY in this turn so
        // far. Drives the post-success exemption in the phantom-tool-call
        // detector below: once the turn has produced at least one real
        // tool result, a subsequent text-only iteration is a completion
        // acknowledgement ("Done.", "Pushed.", "Committed."), not
        // phantom intent. Without this counter the detector mistook
        // every successful turn's wrap-up text for "model narrated
        // without executing", forced retries, eventually rolled the
        // self-heal budget, and produced minute-long loops on already-
        // completed work (logged in user reports as "phantom detected"
        // x8+ after a clean commit+push).
        let mut tool_calls_completed_this_turn: usize = 0;
        // Every tool result this turn, for checking quoted evidence against
        // what the tools actually returned (#785). Turn-scoped because the
        // fabrication appears in a LATER iteration than the call it claims.
        let mut turn_tool_output: Vec<String> = Vec::new();
        // Every tool INPUT this turn, so a claim naming a command can be
        // checked against what actually ran rather than inferred from its
        // wording (#789).
        let mut turn_tool_input: Vec<String> = Vec::new();
        // Raw search-tool outputs this turn, harvested into the delivered
        // answer's Sources footer at settle (#1883). Kept separate from
        // `turn_tool_output` so non-search tool text never leaks links.
        let mut turn_search_outputs: Vec<String> = Vec::new();
        // One-shot nudge budget for the empty-analysis case: model ran
        // tool calls (e.g. `gh pr view`) on a user request whose verb
        // signals analysis ("audit the PR") but ended with
        // `finish_reason: stop` and zero text. The user expected an
        // analytical answer that uses the fetched data; the FINISHING
        // A TURN directive earlier in the brain prompt explains both
        // task shapes (side-effect vs. analysis). Once is enough — if
        // the model still won't write analysis after the nudge, the
        // text-completion path takes over and a one-line "Done." is
        // the user-visible behaviour.
        let mut analysis_nudge_used: bool = false;
        // One-shot nudge when a work turn closes with ONLY a <<react:emoji>>
        // directive and no completion text (#439): the reaction is an ack
        // for no-op turns, never a substitute for reporting executed work.
        let mut reaction_only_nudge_used: bool = false;
        // Wall clock for the turn, stamped onto the assistant row at the end
        // (#964). Deliberately wall time, not `total_streaming_active_secs`:
        // the header answers "how long did this take" and tool execution and
        // approval waits are part of that answer. Monotonic, so it is immune
        // to clock adjustments mid-turn.
        let turn_started_at = std::time::Instant::now();
        // Last time marker timestamp for intra-turn periodic time notice (#153).
        let mut last_time_marker = chrono::Utc::now();
        let time_marker_interval_secs = crate::config::Config::current()
            .agent
            .time_marker_interval_secs;
        let mut total_input_tokens = 0u32;
        // Provider-reported dollars summed across counted iterations, and a
        // count of iterations that reported no cost. The sum may price the
        // ledger only when nothing is missing from it (#1707).
        let mut reported_cost_sum: Option<f64> = None;
        let mut cost_missing_iters = 0u32;
        let mut total_output_tokens = 0u32;
        let mut total_cache_creation = 0u32;
        let mut total_cache_read = 0u32;
        // Sum of per-iteration active-streaming time, used as the tok/s
        // denominator. Replaces the previous `turn_start.elapsed()`
        // wall-clock which silently halved the displayed rate on every
        // tool-heavy turn by including bash exec / approval waits / DB
        // persistence in the denominator. Per-iteration values come
        // from `LLMResponse.streaming_active_secs` populated by
        // `stream_complete` in helpers.rs.
        let mut total_streaming_active_secs: f64 = 0.0;
        // Last iteration's prompt size, used for the "current context
        // usage" indicator. Distinct from `total_input_tokens` which
        // sums across every iteration for cost/billing. The UI ctx
        // meter must show the LAST call's prompt — summing all
        // iterations inflates by a factor of N and showed 150K for a
        // turn whose final prompt was 22K (2026-04-17 05:55 logs).
        let mut last_iter_input_tokens = 0u32;
        let mut final_response: Option<LLMResponse> = None;
        // Registered tool names for the language-agnostic phantom tell (#463).
        let phantom_tool_names: Vec<String> = self.tool_registry.list_tools();
        let mut accumulated_text = String::new(); // Collect text from all iterations (not just final)
        // Iteration content withheld from the DB by the phantom persist-skip.
        // Flushed at turn close if that very iteration ends the turn (#458):
        // the reloadable history must always contain what the user saw.
        let mut recent_tool_calls: Vec<String> = Vec::new(); // Track tool calls to detect loops
        // Normalized call signatures (#957, generalized #961): the
        // exact-match hard-break only fires on identical args, so loops
        // that differ only by a counter or incrementing number slip past
        // it. Store a normalized signature (name + normalized args) for
        // every iteration containing a tool other than `read_file` so the
        // near-match detector below can count collisions across all tools.
        let mut recent_normalized_calls: Vec<String> = Vec::new();
        let mut stream_retry_count = 0u32; // Track consecutive stream drop retries
        // Retry up to 5 times on dropped streams / transient provider errors —
        // flaky providers (e.g. intermittent 404s mid-run) recover with a few
        // more patient backoff attempts instead of dying the run (#749).
        const MAX_STREAM_RETRIES: u32 = 5;
        // Phantom-retry budget per turn. Single-shot proved insufficient —
        // when the model is stuck in a "Let me check…" narration loop, one
        // correction nudges it for one iteration and it drifts right back.
        // Five retries per turn gives the model room to recover from a
        // bumpy start while still capping pathological cases before they
        // chew the whole quota; once the cap is hit we force a sticky
        // fallback to a different provider rather than giving up.
        let mut phantom_retries_used: u32 = 0;
        const MAX_PHANTOM_RETRIES: u32 = 5;
        // #31: set when a suggest_options surface halts the turn (#1178 M1).
        // The NEXT text-only iteration is then the model's sign-off, not a
        // phantom threat — the ack-skip path keeps it, and this flag only
        // tags that exemption surgically in the log (the skip itself fires
        // 70+ times/day for ordinary acks and must not be disturbed).
        let mut option_surface_halt_seen = false;
        // Whether the iteration that halted on an option surface produced text
        // (#2088). Without text there is no answer on screen, so the turn asks
        // the model once more instead of ending empty.
        let mut option_halt_iteration_has_text = false;
        // Consecutive identical tool rounds (#1030). Observes only; the
        // repeated call still runs and its result still reaches the model.
        // Reset on a provider swap below, because a fallback replays the
        // failed attempt's calls and those copies must not stack onto the
        // count for calls the model made once.
        let mut tool_repeat = super::tool_repeat::ToolRepeatTracker::new();
        // How many times the phantom retry budget may ROLL (reset + keep
        // nudging) before we give up. Previously the roll was unbounded — the
        // counter reset and re-nudged forever, so a model stuck narrating
        // instead of calling tools looped until the user hit Stop (#746). After
        // this many rolls we end the turn with the model's narration as the
        // answer. Total phantom attempts are bounded at ~MAX_PHANTOM_RETRIES *
        // (MAX_PHANTOM_ROLLS + 1).
        let mut phantom_rolls: u32 = 0;
        const MAX_PHANTOM_ROLLS: u32 = 2;
        // Analytics (#897): when Some(retries), a phantom was detected this
        // turn and not yet resolved. Cleared (and a resolution emitted) once a
        // subsequent iteration produces real tool calls.
        let mut phantom_pending: Option<u32> = None;
        // Set to true after we have forced a sticky fallback because
        // phantom retries exhausted. Guarantees we only swap once per
        // turn even if the fallback provider is also phantom-prone.
        // How many times one turn may hand the work to the next provider when
        // the current one will not call tools. The chain itself ends the
        // rotation: `force_next_fallback` returns false once it is exhausted.
        // This is only a backstop against a pathologically long chain.
        const MAX_PHANTOM_SWAPS: u32 = 8;
        let mut phantom_swaps_done: u32 = 0;
        // Global detection ceiling (#1172): provider swaps reset the
        // per-provider retry budget BY DESIGN (a fresh provider gets a fresh
        // chance), but rotation then multiplies total detections — production
        // saw 25 detections / 123 requests across 8 swaps in one stuck turn
        // ($0 metered). This counter NEVER resets; crossing it forces the
        // give-up path no matter how fresh the active provider's budget is.
        let mut phantom_detections_total: u32 = 0;
        const MAX_PHANTOM_DETECTIONS_TOTAL: u32 = MAX_PHANTOM_RETRIES * (MAX_PHANTOM_ROLLS + 1);
        // Bounded retry for the "reasoning-only, no answer" failure mode:
        // MLX Qwen models periodically emit finish_reason=stop after only
        // reasoning_content chunks — zero text, zero tool calls — so the
        // user sees a dropped request. We nudge up to 5 times, escalating
        // the system instruction each round, and if the model STILL refuses
        // to emit visible text we walk the fallback chain (sticky swap) so
        // the turn never silently disappears.
        let mut empty_reasoning_retries: u32 = 0;
        // Message count before the first empty-answer nudge, so the fallback
        // chain retries against the user's real request rather than the nudge
        // scaffolding that already failed on the primary (#979).
        let mut pre_nudge_len: Option<usize> = None;
        const EMPTY_REASONING_MAX_NUDGES: u32 = 5;
        // Mermaid regen budget (#37): how many parse-error nudges a fence
        // gets before the reply ships and degrades to the usual failure
        // block. Owner dial: 3 (2026-08-29).
        #[cfg_attr(not(feature = "telegram"), allow(unused))]
        const MERMAID_REGEN_MAX_NUDGES: u32 = 3;
        // Local reasoning models (notably Qwen3.6-35B on MLX) periodically
        // emit an EOS token mid-sentence — the response looks complete from
        // a protocol standpoint (proper finish_reason=stop + usage chunk)
        // but the visible text ends mid-word ("Standard Get I"). Bounded
        // continuation budget (#1737): the original anchored nudge plus one
        // anchored retry when a continuation degenerates into echoing the
        // tail or merely closing dangling markup.
        let mut truncation_continue_attempts: u32 = 0;
        // Mermaid regen attempts spent (#37): each spend echoes the broken
        // text as an assistant message and injects the renderer's error as
        // a user-role [System: ...] nudge — same shape as the empty-answer
        // ladder.
        #[cfg_attr(not(feature = "telegram"), allow(unused))]
        let mut mermaid_regen_retries: u32 = 0;
        // Tool-text leak (leshchenko1979/opencrabs#66, ex-upstream
        // opencrabs/opencrabs#1260): the provider stripped unrecoverable
        // tool-call JSON and set tool_text_leak. One corrective retry with a
        // structured-calls nudge; a second leak fails the turn clean instead
        // of delivering raw internals as the final answer.
        let mut tool_text_leak_retry_used: bool = false;
        // One-shot nudge for the browser screenshot-spam pattern detected
        // by the semantic-loop check below the per-iteration tool dispatch.
        // Reset per turn; fires at most once.
        let mut browser_screenshot_loop_nudged: bool = false;
        // Fires at most once per turn: the first time a non-modification call
        // (identical name+args) dominates the recent window, nudge the model to
        // stop instead of cutting the turn silently (#507).
        let mut identical_call_loop_nudged: bool = false;
        // Fires at most once per turn: the first time near-identical calls
        // (any tool except `read_file`, differing only in counters, numbers,
        // or whitespace) dominate the normalized window, nudge the model to
        // stop repeating (#957, generalized #961).
        let mut near_match_nudged: bool = false;
        // Tracks whether the CURRENT iteration is a same-provider continuation
        // requested after a truncated-mid-sentence detection. Reset at the top
        // of every iteration; set true just before `continue;` from the
        // truncation-continue branch. When true, the stream-error fallback
        // path skips cross-provider fallback — switching providers mid-table
        // (e.g. qwen vertical-label format → glm pipe-table format) produces
        // garbled output stitched at the seam. Better to abort the continue
        // and leave the visibly truncated response than fabricate a Frankenstein
        // continuation in a different format.
        let mut current_iter_is_truncation_continue: bool = false;
        // Text from the iteration that looked cut off, held so the continuation
        // can be joined onto it rather than replacing it (#859).
        let mut truncation_partial: Option<String> = None;
        let mut rotation_retry_used = false; // Single retry when Qwen rotation yields 0 tools

        // Ordered content segments for CLI providers — tracks text and tool markers
        // in the exact order they stream, so DB persistence preserves interleaving.
        #[derive(Clone)]
        enum CliSegment {
            Text(String),
            Tool(serde_json::Value),
        }
        let cli_segments: std::sync::Arc<std::sync::Mutex<Vec<CliSegment>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        // Progressive persister for CLI turns (#269). The CLI subprocess runs
        // the whole agentic loop internally, so until now NOTHING reached the
        // messages table before the turn ended: a restart mid-turn lost every
        // intermediate and tool result that had been on screen (the display
        // and the DB disagreed). This writer appends each displayed segment
        // to the assistant row AS IT STREAMS, mirroring the non-CLI path's
        // per-iteration persistence, and bumps the pending-request row's
        // last-interaction so long turns stay inside the resume window.
        // A `Flush` message barriers pending tool markers before the drain
        // sites append reasoning / cancel banners, keeping content ordered.
        enum CliPersist {
            Seg(CliSegment),
            Flush(tokio::sync::oneshot::Sender<()>),
        }
        // The writer targets the CURRENT assistant row — it is re-created
        // mid-turn when a queued user message is injected, so the id lives
        // in a shared cell rather than being captured once.
        let cli_persist_msg_id = std::sync::Arc::new(std::sync::Mutex::new(assistant_db_msg.id));
        let cli_persist_tx: Option<tokio::sync::mpsc::UnboundedSender<CliPersist>> =
            if is_cli_provider {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CliPersist>();
                let svc = message_service.clone();
                let msg_id_cell = cli_persist_msg_id.clone();
                let pending_repo = crate::db::PendingRequestRepository::new(self.context.pool());
                let persist_session = session_id;
                tokio::spawn(async move {
                    let mut pending_tools: Vec<serde_json::Value> = Vec::new();
                    let drain_tools = |tools: &mut Vec<serde_json::Value>| -> String {
                        let marker = format!(
                            "\n<!-- tools-v2: {} -->\n",
                            serde_json::to_string(tools).unwrap_or_default()
                        );
                        tools.clear();
                        marker
                    };
                    let append = |delta: String| {
                        let svc = svc.clone();
                        let cell = msg_id_cell.clone();
                        async move {
                            let id = *cell.lock().unwrap_or_else(|e| e.into_inner());
                            if let Err(e) = svc.append_content(id, &delta).await {
                                tracing::warn!("CLI live persist: append failed: {e}");
                            }
                        }
                    };
                    while let Some(m) = rx.recv().await {
                        match m {
                            CliPersist::Seg(CliSegment::Text(text)) => {
                                let mut delta = String::new();
                                if !pending_tools.is_empty() {
                                    delta.push_str(&drain_tools(&mut pending_tools));
                                }
                                delta.push_str(&format!("{}\n\n", text));
                                append(delta).await;
                                if let Err(e) = pending_repo.touch_session(persist_session).await {
                                    tracing::debug!("CLI live persist: touch failed: {e}");
                                }
                            }
                            CliPersist::Seg(CliSegment::Tool(entry)) => {
                                pending_tools.push(entry);
                            }
                            CliPersist::Flush(ack) => {
                                if !pending_tools.is_empty() {
                                    append(drain_tools(&mut pending_tools)).await;
                                }
                                if ack.send(()).is_err() {
                                    tracing::debug!("CLI live persist: flush ack receiver dropped");
                                }
                            }
                        }
                    }
                    // All senders gone (turn over) — flush trailing tools.
                    if !pending_tools.is_empty() {
                        append(drain_tools(&mut pending_tools)).await;
                    }
                });
                Some(tx)
            } else {
                None
            };

        // Wrap progress_callback for CLI providers to intercept IntermediateText
        // and ToolCompleted events, preserving their streaming order.
        let progress_callback: Option<ProgressCallback> = if is_cli_provider {
            if let Some(ref original_cb) = progress_callback {
                let orig = original_cb.clone();
                let segs = cli_segments.clone();
                let persist_tx = cli_persist_tx.clone();
                Some(std::sync::Arc::new(
                    move |sid: Uuid, event: ProgressEvent| {
                        match event {
                            ProgressEvent::IntermediateText { ref text, .. }
                                if !text.is_empty() =>
                            {
                                if let Ok(mut acc) = segs.lock() {
                                    acc.push(CliSegment::Text(text.clone()));
                                }
                                if let Some(ref tx) = persist_tx
                                    && tx
                                        .send(CliPersist::Seg(CliSegment::Text(text.clone())))
                                        .is_err()
                                {
                                    tracing::warn!(
                                        "CLI live persist: writer gone, text segment not persisted live"
                                    );
                                }
                            }
                            ProgressEvent::ToolCompleted {
                                ref tool_name,
                                ref tool_input,
                                success,
                                ref summary,
                                ..
                            } => {
                                let desc = AgentService::format_tool_summary(
                                    &tool_name.to_lowercase(),
                                    tool_input,
                                );
                                let entry = if summary.is_empty() {
                                    serde_json::json!({"d": desc, "s": success, "i": tool_input})
                                } else {
                                    serde_json::json!({"d": desc, "s": success, "o": summary, "i": tool_input})
                                };
                                if let Ok(mut acc) = segs.lock() {
                                    acc.push(CliSegment::Tool(entry.clone()));
                                }
                                if let Some(ref tx) = persist_tx
                                    && tx.send(CliPersist::Seg(CliSegment::Tool(entry))).is_err()
                                {
                                    tracing::warn!(
                                        "CLI live persist: writer gone, tool segment not persisted live"
                                    );
                                }
                            }
                            _ => {}
                        }
                        orig(sid, event);
                    },
                ))
            } else {
                None
            }
        } else {
            progress_callback
        };

        loop {
            // Snapshot + reset the truncation-continue marker at the top of
            // every iteration. The branch that sets it does so just before
            // `continue;`, so it's always one-shot — true for exactly the
            // iteration that follows a truncated response, false otherwise.
            let iter_is_truncation_continue = current_iter_is_truncation_continue;
            current_iter_is_truncation_continue = false;
            // Safety: warn every 50 iterations but never hard-stop
            // Loop detection (below) is the real safety net
            if self.max_tool_iterations > 0 && iteration >= self.max_tool_iterations {
                tracing::warn!(
                    "Tool iteration {} exceeded configured max of {} — continuing (loop detection is active)",
                    iteration,
                    self.max_tool_iterations
                );
            }
            // Check for cancellation
            if let Some(ref token) = cancel_token
                && token.is_cancelled()
            {
                tracing::warn!(
                    "🛑 Tool loop cancelled at iteration {} (cancel_token fired). \
                     Accumulated text: {} chars, tool iterations so far: {}",
                    iteration,
                    accumulated_text.len(),
                    iteration,
                );
                break;
            }

            iteration += 1;

            // An option-surface halt ends the turn (#2086). The halting
            // iteration's text is the answer and its tool result is already
            // persisted, so no follow-up model request is made. The loop-break
            // path below synthesizes the response from the accumulated text.
            if option_surface_halt_seen && option_halt_iteration_has_text {
                tracing::info!("🛑 Turn ended after option-surface halt (no follow-up request)");
                break;
            }

            // Emit thinking progress
            if let Some(ref cb) = progress_callback {
                cb(session_id, ProgressEvent::Thinking);
            }

            // Enforce 65% budget before every API call. Skip ONLY when the
            // CLI manages its own session (claude-cli with --resume). Qwen
            // is spawned cold every turn so we MUST compact for it.
            if let Some(ref outcome) = if cli_owns_context {
                None
            } else {
                self.enforce_context_budget(
                    session_id,
                    &mut context,
                    &model_name,
                    cancel_token.as_ref(),
                    &progress_callback,
                    super::compaction::BudgetPhase::MidLoop,
                )
                .await
            } {
                // Persist compaction marker to DB so restarts load from this point
                if let Err(e) = message_service
                    .create_message(session_id, "user".to_string(), outcome.marker(""))
                    .await
                {
                    tracing::error!("Failed to persist mid-loop compaction marker to DB: {}", e);
                }

                let cont_text = self
                    .continuation_prompt(
                        session_id,
                        super::compaction_prompts::CompactionKind::MidLoop,
                    )
                    .await;
                context.add_message(Message::user(cont_text));
            }

            // Build LLM request with tools if available
            let mut request = LLMRequest::new(model_name.clone(), context.messages.clone())
                .with_max_tokens(self.request_max_tokens_for_session(session_id));
            request.working_directory = Some(
                self.get_working_directory_for_session(session_id)
                    .to_string_lossy()
                    .to_string(),
            );
            request.session_id = Some(session_id);

            if let Some(system) = &context.system_brain {
                request = request.with_system(system.clone());
            }

            // Add tools if registry has any
            let tool_count = self.tool_registry.count();
            tracing::debug!("Tool registry contains {} tools", tool_count);
            if tool_count > 0 {
                let tool_defs = self.tool_schemas_for_session(session_id);
                tracing::debug!("Adding {} tool definitions to request", tool_defs.len());
                request = request.with_tools(tool_defs);
            } else {
                tracing::warn!("No tools registered in tool registry!");
            }

            // CLI providers: pass queue callback so stream_complete can check
            // for queued user messages at tool boundaries mid-stream.
            let queued_buf = tokio::sync::Mutex::new(None);

            // Send to provider via streaming — retry once after emergency compaction if prompt is too long
            let (mut response, reasoning_text): (LLMResponse, Option<String>) = match self
                .stream_complete(
                    session_id,
                    request,
                    cancel_token.as_ref(),
                    progress_callback.as_ref(),
                    if is_cli_provider {
                        self.message_queue_callback.as_ref()
                    } else {
                        None
                    },
                    if is_cli_provider {
                        Some(&queued_buf)
                    } else {
                        None
                    },
                    false,
                )
                .await
            {
                Ok(resp) => {
                    // Primary succeeded on first try. Deliberately does NOT
                    // clear the rescue history (#1667): wiping it here made
                    // STICKY_FALLBACK_THRESHOLD unreachable for a flapping
                    // primary, which is precisely what sticky fallback is
                    // for. Rescues age out of STICKY_FALLBACK_WINDOW on
                    // their own, so an unrelated outage long ago still
                    // cannot stick the fallback today.
                    resp
                }
                // /stop beats every recovery path (#1148): if the token fired
                // while the call was in flight (handshake race, provider-
                // internal rate-limit backoff, fallback walk), classify as
                // Cancelled BEFORE any recovery machinery runs. Without this,
                // a cancel landing in the pre-first-token window surfaces as
                // a noisy Provider(Internal) after the chain was walked for
                // nothing.
                Err(_) if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) => {
                    tracing::info!("🛑 Stream aborted by cancellation (token fired during call)");
                    return Err(AgentError::Cancelled);
                }
                // Budget-gated on purpose (#1021): once the nudges are spent
                // this arm stops matching, so the error falls through to the
                // fallback walk below instead of dead-ending here.
                Err(ref e)
                    if matches!(
                        e,
                        crate::brain::provider::ProviderError::ThinkingLoopTimeout(_)
                    ) && phantom_retries_used < MAX_PHANTOM_RETRIES
                        && phantom_detections_total < MAX_PHANTOM_DETECTIONS_TOTAL =>
                {
                    let secs =
                        if let crate::brain::provider::ProviderError::ThinkingLoopTimeout(s) = e {
                            *s
                        } else {
                            0
                        };
                    tracing::warn!(
                        "Thinking-loop timeout (#890): {}s with zero tool calls — injecting \
                         phantom enforcement and retrying (attempt {}/{})",
                        secs,
                        phantom_retries_used + 1,
                        MAX_PHANTOM_RETRIES
                    );
                    self.record_provider_feedback(
                        session_id,
                        "thinking_loop_timeout",
                        &model_name,
                        Some(&format!("{}s no tool calls", secs)),
                    );
                    // Strip the partial reasoning tokens that streamed before
                    // the timeout fired, then surface a self-heal alert so the
                    // user sees what happened instead of a silent hang.
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::StripStreamedContent {
                                bytes: usize::MAX,
                                reason: "thinking-loop timeout — partial reasoning discarded"
                                    .to_string(),
                            },
                        );
                        cb(
                            session_id,
                            ProgressEvent::SelfHealingAlert {
                                message: format!(
                                    "Thinking loop detected — {}s with no tool calls. \
                                     Retrying with enforcement…",
                                    secs
                                ),
                            },
                        );
                    }
                    // Bound retries through the existing phantom budget so a
                    // pathological model can't loop forever: once the cap is
                    // hit the normal phantom give-up path takes over.
                    phantom_retries_used += 1;
                    phantom_detections_total += 1;
                    context.add_message(Message::user(super::nudge::no_tool_calls_nudge(
                        is_local_provider,
                    )));
                    continue;
                }
                Err(e) if super::repetition::is_repetitive_tool_error(&e.to_string()) => {
                    // Provider's server-side loop guardrail fired: the history is
                    // poisoned with repeated identical tool-call/result pairs and
                    // will 500 forever until pruned, permanently bricking the
                    // session (#740). Collapse the consecutive duplicate rounds
                    // and retry once with a healed history so it self-heals
                    // instead of dying (no more manual DB delete).
                    let (pruned, removed) =
                        super::repetition::prune_repetitive_tool_calls(&context.messages);
                    if removed == 0 {
                        tracing::warn!(
                            "Repetitive-tool 500 but no consecutive duplicates to prune — surfacing: {e}"
                        );
                        return Err(AgentError::Provider(e));
                    }
                    tracing::warn!(
                        "Repetitive-tool-call poison — pruned {} duplicate messages from context; \
                         retrying (#740)",
                        removed
                    );
                    self.record_provider_feedback(
                        session_id,
                        "repetitive_tool_recovery",
                        &model_name,
                        Some(&format!("pruned {removed} duplicate messages")),
                    );
                    context.messages = pruned;
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::SelfHealingAlert {
                                message: format!(
                                    "Recovered from a repetitive-tool-call loop — pruned {removed} \
                                     duplicate steps and retried"
                                ),
                            },
                        );
                    }
                    let mut retry_req =
                        LLMRequest::new(model_name.clone(), context.messages.clone())
                            .with_max_tokens(self.request_max_tokens_for_session(session_id));
                    retry_req.working_directory = Some(
                        self.get_working_directory_for_session(session_id)
                            .to_string_lossy()
                            .to_string(),
                    );
                    retry_req.session_id = Some(session_id);
                    if let Some(system) = &context.system_brain {
                        retry_req = retry_req.with_system(system.clone());
                    }
                    if self.tool_registry.count() > 0 {
                        retry_req = retry_req.with_tools(self.tool_schemas_for_session(session_id));
                    }
                    self.stream_complete(
                        session_id,
                        retry_req,
                        cancel_token.as_ref(),
                        progress_callback.as_ref(),
                        if is_cli_provider {
                            self.message_queue_callback.as_ref()
                        } else {
                            None
                        },
                        if is_cli_provider {
                            Some(&queued_buf)
                        } else {
                            None
                        },
                        false,
                    )
                    .await
                    .map_err(AgentError::Provider)?
                }
                Err(ref e)
                    if e.to_string().contains("prompt is too long")
                        || e.to_string().contains("too many tokens")
                        || e.to_string().contains("Argument list too long")
                        || matches!(
                            e,
                            crate::brain::provider::ProviderError::ContextLengthExceeded(_)
                        ) =>
                {
                    tracing::warn!("Prompt too long for provider — emergency compaction");
                    self.record_provider_feedback(
                        session_id,
                        "context_compaction",
                        &model_name,
                        Some(&format!("tokens={}", context.token_count)),
                    );

                    // Pre-truncate to 85% of max so compact_context() can actually run.
                    // For 200k models: ~170k. For custom providers: scales proportionally.
                    const PRE_TRUNCATE_PCT: f64 = 0.85;
                    let pre_truncate_target =
                        (context.max_tokens as f64 * PRE_TRUNCATE_PCT).max(16_000.0) as usize;
                    if context.token_count > pre_truncate_target {
                        tracing::warn!(
                            "Context too large for compaction ({} tokens) — pre-truncating to {}K",
                            context.token_count,
                            pre_truncate_target / 1000
                        );
                        context.hard_truncate_to(pre_truncate_target);
                        tracing::info!(
                            "Pre-truncated to {} messages ({} tokens) — now attempting compaction",
                            context.messages.len(),
                            context.token_count
                        );
                    }

                    match self
                        .compact_context(
                            session_id,
                            &mut context,
                            &model_name,
                            cancel_token.as_ref(),
                            CompactionNotifier::auto_from(session_id, progress_callback.as_ref()),
                        )
                        .await
                    {
                        Ok((_summary, applied_marker)) => {
                            // Persist compaction marker to DB so restarts load
                            // from this point. #1928: the applied marker, not a
                            // banner rebuilt here.
                            if let Err(e) = message_service
                                .create_message(session_id, "user".to_string(), applied_marker)
                                .await
                            {
                                tracing::error!(
                                    "Failed to persist emergency compaction marker to DB: {}",
                                    e
                                );
                            }

                            let cont_text = self
                                .continuation_prompt(
                                    session_id,
                                    super::compaction_prompts::CompactionKind::Emergency,
                                )
                                .await;
                            context.add_message(Message::user(cont_text));

                            // Notify user about emergency compaction
                            if let Some(ref cb) = progress_callback {
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message: "Emergency compaction: context was too large for the provider. Conversation has been compacted automatically.".to_string(),
                                    },
                                );
                            }
                        }
                        Err(compact_err) => {
                            tracing::error!(
                                "Emergency compaction also failed: {} — falling back to hard truncation",
                                compact_err
                            );

                            // Hard truncate: keep last 12 message pairs (24 messages).
                            // Full conversation is in the DB — agent can search_session for older context.
                            const KEEP_MESSAGES: usize = 24;
                            let total = context.messages.len();
                            if total > KEEP_MESSAGES {
                                let dropped = total - KEEP_MESSAGES;
                                context.messages = context.messages.split_off(dropped);
                                tracing::warn!(
                                    "Hard truncated context: dropped {} messages, kept {}",
                                    dropped,
                                    context.messages.len()
                                );
                            }

                            // Insert truncation marker so the agent knows context was lost
                            let truncation_marker = format!(
                                "[CONTEXT TRUNCATION — The conversation was too large for the provider \
                                 and compaction failed. The {} oldest messages were dropped. \
                                 The full conversation history is still in the database — use the \
                                 search_session tool if you need to recall earlier context. \
                                 Continue from where you left off.]",
                                total.saturating_sub(KEEP_MESSAGES)
                            );
                            context
                                .messages
                                .insert(0, Message::user(truncation_marker.clone()));

                            // Persist truncation marker to DB
                            if let Err(e) = message_service
                                .create_message(session_id, "user".to_string(), truncation_marker)
                                .await
                            {
                                tracing::error!("Failed to persist truncation marker: {}", e);
                            }

                            // Notify user about hard truncation
                            if let Some(ref cb) = progress_callback {
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message: format!(
                                            "Hard truncation: compaction failed, {} oldest messages were dropped. Full history is still in the database.",
                                            total.saturating_sub(KEEP_MESSAGES)
                                        ),
                                    },
                                );
                            }

                            // Re-estimate token count after truncation
                            context.token_count = context
                                .messages
                                .iter()
                                .map(|m| {
                                    m.content
                                        .iter()
                                        .map(|b| match b {
                                            ContentBlock::Text { text } => {
                                                crate::brain::tokenizer::count_tokens(text)
                                            }
                                            ContentBlock::ToolUse { input, .. } => {
                                                crate::brain::tokenizer::count_tokens(
                                                    &input.to_string(),
                                                )
                                            }
                                            ContentBlock::ToolResult { content, .. } => {
                                                crate::brain::tokenizer::count_tokens(content)
                                            }
                                            ContentBlock::Thinking { thinking, .. } => {
                                                crate::brain::tokenizer::count_tokens(thinking)
                                            }
                                            ContentBlock::Image { .. } => 1000,
                                        })
                                        .sum::<usize>()
                                })
                                .sum();
                        }
                    }

                    // Rebuild request with compacted context
                    let mut retry_req =
                        LLMRequest::new(model_name.clone(), context.messages.clone())
                            .with_max_tokens(self.request_max_tokens_for_session(session_id));
                    retry_req.working_directory = Some(
                        self.get_working_directory_for_session(session_id)
                            .to_string_lossy()
                            .to_string(),
                    );
                    retry_req.session_id = Some(session_id);
                    if let Some(system) = &context.system_brain {
                        retry_req = retry_req.with_system(system.clone());
                    }
                    if self.tool_registry.count() > 0 {
                        retry_req = retry_req.with_tools(self.tool_schemas_for_session(session_id));
                    }
                    self.stream_complete(
                        session_id,
                        retry_req,
                        cancel_token.as_ref(),
                        progress_callback.as_ref(),
                        if is_cli_provider {
                            self.message_queue_callback.as_ref()
                        } else {
                            None
                        },
                        if is_cli_provider {
                            Some(&queued_buf)
                        } else {
                            None
                        },
                        false,
                    )
                    .await
                    .map_err(AgentError::Provider)?
                }
                Err(e)
                    if matches!(
                        &e,
                        crate::brain::provider::ProviderError::RateLimitExceeded(_)
                    ) || matches!(
                        &e,
                        crate::brain::provider::ProviderError::StreamError(s) if s.contains("rate limit") || s.contains("hit your limit")
                    ) || matches!(
                        &e,
                        crate::brain::provider::ProviderError::StreamError(s)
                            if crate::brain::provider::error::is_quota_exhausted_message(s)
                    ) || matches!(
                        &e,
                        crate::brain::provider::ProviderError::ApiError { status, .. }
                            if *status == 429
                    ) || matches!(
                        &e,
                        crate::brain::provider::ProviderError::ApiError { status, .. }
                            if *status == 401 || *status == 403 || *status == 402
                    ) || matches!(&e, crate::brain::provider::ProviderError::InvalidApiKey)
                        // #1021: the nudge budget above is spent, so this model
                        // does not emit tool calls on this history. Nudging
                        // harder cannot fix that; a different provider can.
                        // Unreachable until the budget is gone — the arm above
                        // is guarded on it.
                        || matches!(
                            &e,
                            crate::brain::provider::ProviderError::ThinkingLoopTimeout(_)
                        )
                        // #1023: the loop detector already nudged and the model
                        // still would not emit the call. Same reasoning as the
                        // thinking-loop case above — in-place retry repeats the
                        // loop, another provider usually does not.
                        || matches!(
                            &e,
                            crate::brain::provider::ProviderError::AnnouncementLoop(_)
                        ) =>
                {
                    // 401/403 auth failures and missing-key errors are
                    // unrecoverable on the current provider (retry with
                    // the same bad key is pointless) but perfectly
                    // fallback-able: the next provider in the chain has
                    // its own key and may work fine. The 2026-04-18
                    // a custom provider was caught returning 401
                    // "Missing API key" and falling straight through
                    // to the terminal AgentError — no retry, no
                    // fallback, user saw a raw error string in
                    // Telegram. Treat it like a rate-limit: skip
                    // in-place retry, walk the fallback chain.
                    // Distinguish three failure flavours that all arrive here:
                    //   • genuine auth: 401/403 with an auth-ish error_type or
                    //     InvalidApiKey. Key is bad — walk fallback.
                    //   • model rejection disguised as 401: some proxies
                    //     (opencode.ai/zen) return 401 with
                    //     `error_type: "ModelError"` when the key is valid
                    //     but the requested model isn't in their allowlist.
                    //     Reporting "Auth error" here misled the user into
                    //     thinking keys were wrong.
                    //   • rate/account limits: 429 / RateLimitExceeded.
                    let is_model_mismatch = e.is_model_unsupported();
                    let is_payment_required = matches!(
                        &e,
                        crate::brain::provider::ProviderError::ApiError { status, .. }
                            if *status == 402
                    );
                    let (is_auth, reason) = if is_model_mismatch {
                        (false, "model_unsupported")
                    } else if is_payment_required {
                        // 402 means the upstream account has run out of
                        // credit / hit a hard billing cap. Same fallback
                        // path as auth/rate, but reported distinctly so
                        // the user knows it's a quota issue, not a key
                        // misconfiguration or temporary throttle.
                        (false, "payment_required")
                    } else if matches!(
                        &e,
                        crate::brain::provider::ProviderError::ApiError { status, .. }
                            if *status == 401 || *status == 403
                    ) || matches!(
                        &e,
                        crate::brain::provider::ProviderError::InvalidApiKey
                    ) {
                        (true, "auth_error")
                    } else {
                        (false, "rate_limit_exceeded")
                    };
                    let flavour_label = if is_model_mismatch {
                        "Model not supported by provider"
                    } else if is_payment_required {
                        "Quota/payment limit"
                    } else if is_auth {
                        "Auth error"
                    } else {
                        "Rate/account limit"
                    };
                    tracing::warn!(
                        "{} hit ({}) — checking for fallback provider",
                        flavour_label,
                        e
                    );

                    // Resolve the session's CURRENT primary provider name
                    // for the alert AND feedback dimension — never just the model.
                    // A user can have opencode, opencode2, opencode3 … all routing to
                    // the same underlying model name. "Rate limit on
                    // 'claude-sonnet-4-6'" hides WHICH subscription got
                    // rate-limited. The session's provider is the truth
                    // source (global `self.provider` may differ after
                    // per-session swaps).
                    let primary_from_name = self.provider_name_for_session(session_id);
                    let primary_from_model = model_name.clone();
                    // Record the ACTUAL provider/model pair that will be sent
                    // (not the requested one). helpers.rs remaps mismatched
                    // pairs silently — RSI must reflect what actually hit the
                    // wire so entries like "dialagram/zhipu" (where "zhipu"
                    // is a provider name that leaked into the model slot from
                    // a reversed cron config) never appear in feedback.
                    let actual_model = {
                        let p = self.provider_for_session(session_id);
                        let supported = p.supported_models();
                        if !supported.is_empty() && !supported.iter().any(|m| m == &model_name) {
                            p.default_model().to_string()
                        } else {
                            model_name.clone()
                        }
                    };
                    let provider_model_dim = format!("{}/{}", primary_from_name, actual_model);

                    self.record_provider_feedback(
                        session_id,
                        "provider_error",
                        &provider_model_dim,
                        Some(reason),
                    );

                    if let Some(ref cb) = progress_callback {
                        let prefix = if is_model_mismatch {
                            format!(
                                "Model '{}' not supported by '{}'",
                                model_name, primary_from_name
                            )
                        } else if is_payment_required {
                            format!(
                                "Quota/payment limit on '{}/{}'",
                                primary_from_name, model_name
                            )
                        } else if is_auth {
                            format!("Auth error on '{}/{}'", primary_from_name, model_name)
                        } else {
                            format!(
                                "Rate limit on '{}/{}' (retried 3x in-place)",
                                primary_from_name, model_name
                            )
                        };
                        let message = if !self.has_fallback_provider() {
                            // #1006: a bare status teaches nobody anything —
                            // point at the exact config block that fixes it.
                            format!(
                                "{} — {}",
                                prefix,
                                crate::brain::provider::error::no_chain_setup_guidance()
                            )
                        } else {
                            format!("{} — walking fallback chain...", prefix)
                        };
                        cb(session_id, ProgressEvent::SelfHealingAlert { message });
                    }

                    // Walk the entire fallback chain, skipping the SESSION's
                    // active provider (not the global default — a per-session
                    // swap may already have this session on opencode2 while
                    // `self.provider` still holds opencode).
                    let active_name = self.provider_name_for_session(session_id);
                    // #1251: the whole configured chain is walked, in
                    // configured order, every turn. A provider is never
                    // dropped from the walk for failing earlier — the user
                    // owns that decision, the agent only reports.
                    let chain = self.fallback_chain_snapshot();
                    let candidates: Vec<_> =
                        chain.iter().filter(|p| p.name() != active_name).collect();

                    if candidates.is_empty() {
                        // #1006: with NO chain configured there is nothing to
                        // summarise, and "all providers in the chain failed"
                        // would be a lie — the alert above already carried
                        // the setup guidance, so skip the summary entirely.
                        if self.has_fallback_provider()
                            && let Some(ref cb) = progress_callback
                        {
                            let reason = crate::brain::provider::error::short_error_reason(&e);
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: crate::brain::provider::error::chain_exhausted_summary(
                                        &primary_from_name,
                                        &reason,
                                        &[],
                                    ),
                                },
                            );
                        }
                        return Err(AgentError::Provider(e));
                    }

                    // #952: name the PRIMARY's failure reason in the final
                    // summary — capture before the walk overwrites last_err.
                    let primary_reason = crate::brain::provider::error::short_error_reason(&e);
                    let mut last_err = e;
                    // Per-candidate failure ledger for the final summary
                    // (#952): "provider/model: reason" for every fallback
                    // that was tried and died.
                    let mut tried: Vec<String> = Vec::new();
                    // stream_complete returns (LLMResponse, Option<String>);
                    // we also need fb_name / fb_model alongside for the
                    // ProviderSwitched event emitted once on success.
                    let mut succeeded: Option<(
                        (crate::brain::provider::LLMResponse, Option<String>),
                        String,
                        String,
                    )> = None;
                    for fallback in &candidates {
                        let fb_name = fallback.name().to_string();
                        let fb_model = fallback.default_model().to_string();
                        tracing::info!(
                            "Trying fallback provider '{}' (model '{}')",
                            fb_name,
                            fb_model
                        );
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "Trying fallback '{}/{}'...",
                                        fb_name, fb_model
                                    ),
                                },
                            );
                        }

                        let mut fb_req =
                            LLMRequest::new(fb_model.clone(), context.messages.clone())
                                .with_max_tokens(self.request_max_tokens_for_session(session_id));
                        fb_req.working_directory = Some(
                            self.get_working_directory_for_session(session_id)
                                .to_string_lossy()
                                .to_string(),
                        );
                        fb_req.session_id = Some(session_id);
                        if let Some(system) = &context.system_brain {
                            fb_req = fb_req.with_system(system.clone());
                        }
                        if self.tool_registry.count() > 0 {
                            fb_req = fb_req.with_tools(self.tool_schemas_for_session(session_id));
                        }

                        // STICKY FALLBACK (rate-limit / auth path): swap
                        // session provider to the fallback, and on success
                        // DON'T restore. Rate limits from subscription
                        // quotas can last hours; without stickiness every
                        // subsequent turn hits the same 429, walks the
                        // chain again, and bounces back — the user sees a
                        // warning every turn and never settles on the
                        // working provider.
                        //
                        // Guard pattern: if the await errors OR is
                        // cancelled (future dropped mid-stream), Drop fires
                        // and restores the original. Avoids the nightmare
                        // where session_providers points at fallback but
                        // session.model in DB is still primary → 400
                        // "unknown model" on next turn. On success we
                        // disable the guard (set original=None) so the
                        // swap STICKS, then emit ProviderSwitched to
                        // persist the pairing to DB via state.rs:2205.
                        let original_provider = self.provider_for_session(session_id);
                        self.swap_provider_for_session(
                            session_id,
                            (*fallback).clone(),
                            (*fallback)
                                .active_subprovider_model()
                                .unwrap_or_else(|| (*fallback).default_model().to_string()),
                        );
                        let mut restore_guard = FallbackProviderGuard {
                            service: self,
                            session_id,
                            original: Some(original_provider),
                        };
                        let fb_result = self
                            .stream_complete(
                                session_id,
                                fb_req,
                                cancel_token.as_ref(),
                                progress_callback.as_ref(),
                                None,
                                None,
                                false,
                            )
                            .await;
                        match fb_result {
                            Ok(resp) => {
                                // Disable restore — the swap must stick.
                                restore_guard.original = None;
                                drop(restore_guard);
                                succeeded = Some((resp, fb_name, fb_model));
                                break;
                            }
                            Err(fb_err) => {
                                // Guard's Drop restores the original so
                                // the next candidate iteration starts
                                // clean.
                                drop(restore_guard);
                                tried.push(format!(
                                    "{}/{}: {}",
                                    fb_name,
                                    fb_model,
                                    crate::brain::provider::error::short_error_reason(&fb_err)
                                ));
                                tracing::warn!(
                                    "Fallback '{}' failed: {} — trying next",
                                    fallback.name(),
                                    fb_err
                                );
                                last_err = fb_err;
                            }
                        }
                    }
                    match succeeded {
                        Some((resp, fb_name, fb_model)) => {
                            // Emit ProviderSwitched so the TUI persists the
                            // swap to the session DB (session.provider_name
                            // and session.model). state.rs:2205 picks this
                            // up and calls session_service.update_session
                            // so the NEXT turn resolves model_name from
                            // the fallback, not the rate-limited primary.
                            if let Some(ref cb) = progress_callback {
                                let reason = if is_model_mismatch {
                                    "model_unsupported".to_string()
                                } else if is_auth {
                                    "auth_error".to_string()
                                } else {
                                    "rate_limit".to_string()
                                };
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message: format!(
                                            "Sticky fallback → '{}/{}' (was '{}/{}', {}). \
                                             Pinned until you change via /models.",
                                            fb_name,
                                            fb_model,
                                            primary_from_name,
                                            primary_from_model,
                                            reason
                                        ),
                                    },
                                );
                                cb(
                                    session_id,
                                    ProgressEvent::ProviderSwitched {
                                        from_name: primary_from_name.clone(),
                                        from_model: primary_from_model.clone(),
                                        to_name: fb_name.clone(),
                                        to_model: fb_model.clone(),
                                        reason,
                                    },
                                );
                            }
                            // Persist the locked {provider, model} pair to
                            // DB independently of the progress callback —
                            // channel handlers (Slack/Telegram/Discord/
                            // WhatsApp) historically dropped ProviderSwitched
                            // on the floor, leaving DB stale while
                            // session_providers[sid] was already swapped to
                            // the fallback. Stale DB → next turn's
                            // sync_provider_for_session "restored" memory
                            // from the wrong row → cross-pair leak.
                            self.persist_sticky_pair(session_id, fb_name.clone(), fb_model.clone());
                            // Update the local model_name binding so any
                            // further iterations in THIS turn build
                            // requests with the fallback's model
                            // (otherwise the next tool-loop iteration
                            // would send the primary's model name to the
                            // fallback provider → 400).
                            model_name = fb_model;
                            resp
                        }
                        None => {
                            tracing::error!(
                                "All {} fallback providers exhausted: [{}]",
                                candidates.len(),
                                candidates
                                    .iter()
                                    .map(|p| p.name())
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            );
                            emit_retry_notices(
                                &self.provider_for_session(session_id),
                                session_id,
                                progress_callback.as_ref(),
                            );
                            // Human-readable chain-exhaustion summary naming
                            // the dead primary and every fallback tried.
                            if let Some(ref cb) = progress_callback {
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message:
                                            crate::brain::provider::error::chain_exhausted_summary(
                                                &primary_from_name,
                                                &primary_reason,
                                                &tried,
                                            ),
                                    },
                                );
                            }
                            return Err(AgentError::Provider(last_err));
                        }
                    }
                }
                Err(e)
                    if matches!(
                        &e,
                        crate::brain::provider::ProviderError::StreamError(_)
                            | crate::brain::provider::ProviderError::Timeout(_)
                    ) && !e.to_string().contains("rate limit")
                        && !e.to_string().contains("hit your limit") =>
                {
                    // Timeout covers the new handshake-timeout path: a wedged
                    // local server that accepts TCP but never emits headers.
                    // Funnel it into the same MAX_STREAM_RETRIES + fallback chain as
                    // mid-stream StreamError so the user sees recovery
                    // activity instead of a dead turn.
                    let err_msg = e.to_string();
                    tracing::warn!(
                        "Mid-stream error: {} — retrying up to {} times",
                        err_msg,
                        MAX_STREAM_RETRIES
                    );
                    let primary_from_name = self.provider_name_for_session(session_id);
                    let actual_model = {
                        let p = self.provider_for_session(session_id);
                        let supported = p.supported_models();
                        if !supported.is_empty() && !supported.iter().any(|m| m == &model_name) {
                            p.default_model().to_string()
                        } else {
                            model_name.clone()
                        }
                    };
                    let provider_model_dim = format!("{}/{}", primary_from_name, actual_model);
                    self.record_provider_feedback(
                        session_id,
                        "provider_error",
                        &provider_model_dim,
                        Some(&err_msg),
                    );

                    let mut last_err = e;
                    let mut succeeded = None;

                    for attempt in 1..=MAX_STREAM_RETRIES {
                        // /stop beats the retry budget (#1148): bail out
                        // before spending another attempt.
                        if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                            tracing::info!(
                                "🛑 Stream retry loop aborted — cancelled before attempt {attempt}"
                            );
                            return Err(AgentError::Cancelled);
                        }
                        tracing::info!(
                            "Stream retry attempt {}/{} after: {}",
                            attempt,
                            MAX_STREAM_RETRIES,
                            last_err
                        );

                        // Exponential backoff: 500ms, 1s, 2s, 4s, 8s — raced against /stop
                        let backoff_ms = 500u64 * (1u64 << (attempt - 1));
                        if !crate::brain::agent::service::helpers::cancellable_backoff(
                            cancel_token.as_ref(),
                            tokio::time::Duration::from_millis(backoff_ms),
                        )
                        .await
                        {
                            tracing::info!(
                                "🛑 Stream retry loop aborted — cancelled during backoff"
                            );
                            return Err(AgentError::Cancelled);
                        }

                        // Rebuild request
                        let mut retry_req =
                            LLMRequest::new(model_name.clone(), context.messages.clone())
                                .with_max_tokens(self.request_max_tokens_for_session(session_id));
                        retry_req.working_directory = Some(
                            self.get_working_directory_for_session(session_id)
                                .to_string_lossy()
                                .to_string(),
                        );
                        retry_req.session_id = Some(session_id);
                        if let Some(system) = &context.system_brain {
                            retry_req = retry_req.with_system(system.clone());
                        }
                        if self.tool_registry.count() > 0 {
                            retry_req =
                                retry_req.with_tools(self.tool_schemas_for_session(session_id));
                        }

                        match self
                            .stream_complete(
                                session_id,
                                retry_req,
                                cancel_token.as_ref(),
                                progress_callback.as_ref(),
                                if is_cli_provider {
                                    self.message_queue_callback.as_ref()
                                } else {
                                    None
                                },
                                if is_cli_provider {
                                    Some(&queued_buf)
                                } else {
                                    None
                                },
                                false,
                            )
                            .await
                        {
                            Ok(resp) => {
                                tracing::info!(
                                    "Stream retry {}/{} succeeded",
                                    attempt,
                                    MAX_STREAM_RETRIES
                                );
                                succeeded = Some(resp);
                                break;
                            }
                            Err(retry_err) => {
                                tracing::warn!(
                                    "Stream retry {}/{} failed: {}",
                                    attempt,
                                    MAX_STREAM_RETRIES,
                                    retry_err
                                );
                                last_err = retry_err;
                            }
                        }
                    }

                    if let Some(resp) = succeeded {
                        resp
                    } else if iter_is_truncation_continue {
                        // This iteration is the same-provider continuation we
                        // asked for after a truncated-mid-sentence response.
                        // Falling back to a different provider here produces
                        // garbled output: providers don't share format style,
                        // so the continuation gets stitched in a different
                        // shape (e.g. qwen vertical-label labels → glm pipe-
                        // table syntax) and the result is unreadable. Better
                        // to abort the continue and leave the visibly cut-off
                        // response than fabricate a Frankenstein answer.
                        tracing::warn!(
                            "All {} stream retries failed during a truncation-continue: \
                             aborting continuation rather than falling back to a different \
                             provider (would cause format drift).",
                            MAX_STREAM_RETRIES
                        );
                        if let Some(ref cb) = progress_callback {
                            let active_name = self.provider_name_for_session(session_id);
                            let err_snippet: String =
                                last_err.to_string().chars().take(120).collect();
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "Continuation request to '{}/{}' failed after {} retries: {}. \
                                         Leaving the previous response truncated.",
                                        active_name, model_name, MAX_STREAM_RETRIES, err_snippet,
                                    ),
                                },
                            );
                        }
                        emit_retry_notices(
                            &self.provider_for_session(session_id),
                            session_id,
                            progress_callback.as_ref(),
                        );
                        return Err(AgentError::Provider(last_err));
                    } else {
                        // All retries failed — try fallback provider
                        tracing::warn!(
                            "All {} stream retries failed, checking for fallback provider",
                            MAX_STREAM_RETRIES
                        );

                        if let Some(ref cb) = progress_callback {
                            let active_name = self.provider_name_for_session(session_id);
                            // Surface the underlying error so users (and the
                            // maintainer when users report) can tell whether
                            // it was a timeout, TLS issue, 5xx, etc., instead
                            // of a generic "Stream error" banner.
                            let err_snippet: String =
                                last_err.to_string().chars().take(120).collect();
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "Stream error on '{}/{}' after {} retries: {}. {}",
                                        active_name,
                                        model_name,
                                        MAX_STREAM_RETRIES,
                                        err_snippet,
                                        if self.has_fallback_provider() {
                                            "Switching to fallback provider..."
                                        } else {
                                            "No fallback provider configured."
                                        }
                                    ),
                                },
                            );
                        }

                        // Walk the entire fallback chain, skipping the active provider.
                        let stream_active_name = self
                            .provider
                            .read()
                            .ok()
                            .map(|p| p.name().to_string())
                            .unwrap_or_default();
                        let chain = self.fallback_chain_snapshot();
                        let stream_candidates: Vec<_> = chain
                            .iter()
                            .filter(|p| p.name() != stream_active_name)
                            .collect();

                        if stream_candidates.is_empty() {
                            emit_retry_notices(
                                &self.provider_for_session(session_id),
                                session_id,
                                progress_callback.as_ref(),
                            );
                            // Report the chain state instead of a bare error.
                            if let Some(ref cb) = progress_callback {
                                let reason =
                                    crate::brain::provider::error::short_error_reason(&last_err);
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message:
                                            crate::brain::provider::error::chain_exhausted_summary(
                                                &stream_active_name,
                                                &reason,
                                                &[],
                                            ),
                                    },
                                );
                            }
                            return Err(AgentError::Provider(last_err));
                        }

                        // #952: capture the PRIMARY's reason before the walk
                        // overwrites last_err, and ledger every tried fallback.
                        let stream_primary_reason =
                            crate::brain::provider::error::short_error_reason(&last_err);
                        let mut stream_tried: Vec<String> = Vec::new();
                        let mut stream_succeeded = None;
                        for fallback in &stream_candidates {
                            let fb_name = fallback.name().to_string();
                            let fb_model = fallback.default_model().to_string();
                            tracing::info!(
                                "Stream fallback trying '{}' (model '{}')",
                                fb_name,
                                fb_model
                            );
                            // Tell the user which fallback we're attempting —
                            // the earlier "Switching to fallback provider..."
                            // banner named the origin but not the destination,
                            // so once the retries were exhausted users saw a provider swap
                            // with no hint what they're now talking to.
                            if let Some(ref cb) = progress_callback {
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message: format!(
                                            "Trying fallback '{}/{}'...",
                                            fb_name, fb_model
                                        ),
                                    },
                                );
                            }

                            let mut fb_req =
                                LLMRequest::new(fb_model.clone(), context.messages.clone())
                                    .with_max_tokens(
                                        self.request_max_tokens_for_session(session_id),
                                    );
                            fb_req.working_directory = Some(
                                self.get_working_directory_for_session(session_id)
                                    .to_string_lossy()
                                    .to_string(),
                            );
                            fb_req.session_id = Some(session_id);
                            if let Some(system) = &context.system_brain {
                                fb_req = fb_req.with_system(system.clone());
                            }
                            if self.tool_registry.count() > 0 {
                                fb_req =
                                    fb_req.with_tools(self.tool_schemas_for_session(session_id));
                            }

                            // Swap only this session's provider for the
                            // stream-fallback attempt; guard ensures restore
                            // runs even if the outer future is cancelled
                            // mid-await (see FallbackProviderGuard doc).
                            let original_provider = self.provider_for_session(session_id);
                            self.swap_provider_for_session(
                                session_id,
                                (*fallback).clone(),
                                (*fallback)
                                    .active_subprovider_model()
                                    .unwrap_or_else(|| (*fallback).default_model().to_string()),
                            );
                            let mut restore_guard = FallbackProviderGuard {
                                service: self,
                                session_id,
                                original: Some(original_provider),
                            };
                            let fb_result = self
                                .stream_complete(
                                    session_id,
                                    fb_req,
                                    cancel_token.as_ref(),
                                    progress_callback.as_ref(),
                                    None,
                                    None,
                                    false,
                                )
                                .await;
                            match fb_result {
                                Ok(resp) => {
                                    // Streak gate: only stick the fallback as
                                    // the session's persistent provider once
                                    // STICKY_FALLBACK_THRESHOLD rescues land
                                    // inside STICKY_FALLBACK_WINDOW. Most
                                    // primary outages are transient (network
                                    // blip, model warm-up, brief 5xx), so
                                    // making the first rescue sticky meant a
                                    // 5-second hiccup permanently demoted the
                                    // primary until the user noticed and
                                    // reset via /models. Recent-window rather
                                    // than consecutive because a primary that
                                    // alternates fail/ok never accumulates a
                                    // run, and that is the provider most
                                    // worth demoting (#1667).
                                    let streak = self.bump_primary_failure_streak(session_id);
                                    let sticky = streak >= STICKY_FALLBACK_THRESHOLD;
                                    if sticky {
                                        // Disable restore — the swap stays.
                                        restore_guard.original = None;
                                    }
                                    // Either way: dropping the guard either
                                    // restores the primary (non-sticky) or is
                                    // a no-op (sticky). Done either way before
                                    // emitting events so the post-event state
                                    // matches what callers see.
                                    drop(restore_guard);
                                    stream_succeeded = Some(resp);
                                    if let Some(ref cb) = progress_callback {
                                        let primary_from_name =
                                            self.provider_name_for_session(session_id);
                                        cb(
                                            session_id,
                                            ProgressEvent::SelfHealingAlert {
                                                message: if sticky {
                                                    format!(
                                                        "Stream error → switched to {}/{} (sticky after {} rescues within {}m)",
                                                        fb_name,
                                                        fb_model,
                                                        streak,
                                                        failure_window::STICKY_FALLBACK_WINDOW
                                                            .as_secs()
                                                            / 60
                                                    )
                                                } else {
                                                    format!(
                                                        "Stream error → rescued by {}/{} ({}/{} within {}m; primary will be tried again next turn)",
                                                        fb_name,
                                                        fb_model,
                                                        streak,
                                                        STICKY_FALLBACK_THRESHOLD,
                                                        failure_window::STICKY_FALLBACK_WINDOW
                                                            .as_secs()
                                                            / 60
                                                    )
                                                },
                                            },
                                        );
                                        if sticky {
                                            cb(
                                                session_id,
                                                ProgressEvent::ProviderSwitched {
                                                    from_name: primary_from_name,
                                                    from_model: self
                                                        .provider_model_for_session(session_id),
                                                    to_name: fb_name.to_string(),
                                                    to_model: fb_model.to_string(),
                                                    reason: "stream_error".to_string(),
                                                },
                                            );
                                        }
                                    }
                                    // Persist the locked pair to DB only on
                                    // sticky — a transient rescue should not
                                    // mutate persistent session state.
                                    if sticky {
                                        self.persist_sticky_pair(
                                            session_id,
                                            fb_name.to_string(),
                                            fb_model.to_string(),
                                        );
                                    }
                                    break;
                                }
                                Err(fb_err) => {
                                    // Guard's Drop restores the original so the
                                    // next candidate iteration starts clean.
                                    drop(restore_guard);
                                    tracing::warn!(
                                        "Stream fallback '{}' failed: {} — trying next",
                                        fb_name,
                                        fb_err
                                    );
                                    stream_tried.push(format!(
                                        "{fb_name}/{fb_model}: {}",
                                        crate::brain::provider::error::short_error_reason(&fb_err)
                                    ));
                                    last_err = fb_err;
                                }
                            }
                        }
                        match stream_succeeded {
                            Some(resp) => resp,
                            None => {
                                tracing::error!(
                                    "All {} stream fallback providers exhausted: [{}]",
                                    stream_candidates.len(),
                                    stream_candidates
                                        .iter()
                                        .map(|p| p.name())
                                        .collect::<Vec<_>>()
                                        .join(", "),
                                );
                                emit_retry_notices(
                                    &self.provider_for_session(session_id),
                                    session_id,
                                    progress_callback.as_ref(),
                                );
                                // #952: human-readable chain-exhaustion summary.
                                if let Some(ref cb) = progress_callback {
                                    cb(
                                        session_id,
                                        ProgressEvent::SelfHealingAlert {
                                            message: crate::brain::provider::error::chain_exhausted_summary(
                                                &stream_active_name,
                                                &stream_primary_reason,
                                                &stream_tried,
                                            ),
                                        },
                                    );
                                }
                                return Err(AgentError::Provider(last_err));
                            }
                        }
                    }
                }
                Err(e) if matches!(&e, crate::brain::provider::ProviderError::ApiError { status, .. } if *status >= 500 && *status < 600) =>
                {
                    // 5xx upstream errors (500/502/503/504) are transient — retry
                    // up to MAX_STREAM_RETRIES times with backoff before falling back, same as
                    // StreamError/Timeout. Without this the user sees a hard
                    // failure on every blip from the provider.
                    let err_msg = e.to_string();
                    tracing::warn!(
                        "Upstream 5xx error: {} — retrying up to {} times",
                        err_msg,
                        MAX_STREAM_RETRIES
                    );
                    let primary_from_name = self.provider_name_for_session(session_id);
                    let actual_model = {
                        let p = self.provider_for_session(session_id);
                        let supported = p.supported_models();
                        if !supported.is_empty() && !supported.iter().any(|m| m == &model_name) {
                            p.default_model().to_string()
                        } else {
                            model_name.clone()
                        }
                    };
                    let provider_model_dim = format!("{}/{}", primary_from_name, actual_model);
                    self.record_provider_feedback(
                        session_id,
                        "provider_error",
                        &provider_model_dim,
                        Some(&err_msg),
                    );

                    let mut last_err = e;
                    let mut succeeded = None;

                    for attempt in 1..=MAX_STREAM_RETRIES {
                        // /stop beats the retry budget (#1148): bail out
                        // before spending another attempt.
                        if cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
                            tracing::info!(
                                "🛑 5xx retry loop aborted — cancelled before attempt {attempt}"
                            );
                            return Err(AgentError::Cancelled);
                        }
                        tracing::info!(
                            "5xx retry attempt {}/{} after: {}",
                            attempt,
                            MAX_STREAM_RETRIES,
                            last_err
                        );

                        // Exponential backoff: 500ms, 1s, 2s, 4s, 8s — raced against /stop
                        let backoff_ms = 500u64 * (1u64 << (attempt - 1));
                        if !crate::brain::agent::service::helpers::cancellable_backoff(
                            cancel_token.as_ref(),
                            tokio::time::Duration::from_millis(backoff_ms),
                        )
                        .await
                        {
                            tracing::info!("🛑 5xx retry loop aborted — cancelled during backoff");
                            return Err(AgentError::Cancelled);
                        }

                        let mut retry_req =
                            LLMRequest::new(model_name.clone(), context.messages.clone())
                                .with_max_tokens(self.request_max_tokens_for_session(session_id));
                        retry_req.working_directory = Some(
                            self.get_working_directory_for_session(session_id)
                                .to_string_lossy()
                                .to_string(),
                        );
                        retry_req.session_id = Some(session_id);
                        if let Some(system) = &context.system_brain {
                            retry_req = retry_req.with_system(system.clone());
                        }
                        if self.tool_registry.count() > 0 {
                            retry_req =
                                retry_req.with_tools(self.tool_schemas_for_session(session_id));
                        }

                        match self
                            .stream_complete(
                                session_id,
                                retry_req,
                                cancel_token.as_ref(),
                                progress_callback.as_ref(),
                                if is_cli_provider {
                                    self.message_queue_callback.as_ref()
                                } else {
                                    None
                                },
                                if is_cli_provider {
                                    Some(&queued_buf)
                                } else {
                                    None
                                },
                                false,
                            )
                            .await
                        {
                            Ok(resp) => {
                                tracing::info!(
                                    "5xx retry {}/{} succeeded",
                                    attempt,
                                    MAX_STREAM_RETRIES
                                );
                                succeeded = Some(resp);
                                break;
                            }
                            Err(retry_err) => {
                                tracing::warn!(
                                    "5xx retry {}/{} failed: {}",
                                    attempt,
                                    MAX_STREAM_RETRIES,
                                    retry_err
                                );
                                last_err = retry_err;
                            }
                        }
                    }

                    if let Some(resp) = succeeded {
                        resp
                    } else {
                        tracing::warn!(
                            "All {} 5xx retries failed, checking for fallback provider",
                            MAX_STREAM_RETRIES
                        );

                        if let Some(ref cb) = progress_callback {
                            let active_name = self.provider_name_for_session(session_id);
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "5xx error on '{}/{}' after {} retries. {}",
                                        active_name,
                                        model_name,
                                        MAX_STREAM_RETRIES,
                                        if self.has_fallback_provider() {
                                            "Switching to fallback provider..."
                                        } else {
                                            "No fallback provider configured."
                                        }
                                    ),
                                },
                            );
                        }

                        let stream_active_name = self
                            .provider
                            .read()
                            .ok()
                            .map(|p| p.name().to_string())
                            .unwrap_or_default();
                        let chain = self.fallback_chain_snapshot();
                        let stream_candidates: Vec<_> = chain
                            .iter()
                            .filter(|p| p.name() != stream_active_name)
                            .collect();

                        if stream_candidates.is_empty() {
                            emit_retry_notices(
                                &self.provider_for_session(session_id),
                                session_id,
                                progress_callback.as_ref(),
                            );
                            // Report the chain state instead of a bare error.
                            if let Some(ref cb) = progress_callback {
                                let reason =
                                    crate::brain::provider::error::short_error_reason(&last_err);
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message:
                                            crate::brain::provider::error::chain_exhausted_summary(
                                                &stream_active_name,
                                                &reason,
                                                &[],
                                            ),
                                    },
                                );
                            }
                            return Err(AgentError::Provider(last_err));
                        }

                        // #952: capture the PRIMARY's reason before the walk
                        // overwrites last_err, and ledger every tried fallback.
                        let stream_primary_reason =
                            crate::brain::provider::error::short_error_reason(&last_err);
                        let mut stream_tried: Vec<String> = Vec::new();
                        let mut stream_succeeded = None;
                        for fallback in &stream_candidates {
                            let fb_name = fallback.name().to_string();
                            let fb_model = fallback.default_model().to_string();
                            tracing::info!("5xx fallback trying '{}/{}'", fb_name, fb_model);
                            if let Some(ref cb) = progress_callback {
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message: format!(
                                            "Trying fallback '{}/{}'...",
                                            fb_name, fb_model
                                        ),
                                    },
                                );
                            }

                            let mut fb_req =
                                LLMRequest::new(fb_model.clone(), context.messages.clone())
                                    .with_max_tokens(
                                        self.request_max_tokens_for_session(session_id),
                                    );
                            fb_req.working_directory = Some(
                                self.get_working_directory_for_session(session_id)
                                    .to_string_lossy()
                                    .to_string(),
                            );
                            fb_req.session_id = Some(session_id);
                            if let Some(system) = &context.system_brain {
                                fb_req = fb_req.with_system(system.clone());
                            }
                            if self.tool_registry.count() > 0 {
                                fb_req =
                                    fb_req.with_tools(self.tool_schemas_for_session(session_id));
                            }

                            match self
                                .stream_complete(
                                    session_id,
                                    fb_req,
                                    cancel_token.as_ref(),
                                    progress_callback.as_ref(),
                                    if is_cli_provider {
                                        self.message_queue_callback.as_ref()
                                    } else {
                                        None
                                    },
                                    if is_cli_provider {
                                        Some(&queued_buf)
                                    } else {
                                        None
                                    },
                                    false,
                                )
                                .await
                            {
                                Ok(resp) => {
                                    tracing::info!(
                                        "5xx fallback succeeded with '{}/{}'",
                                        fb_name,
                                        fb_model
                                    );
                                    stream_succeeded = Some(resp);
                                    // Same streak gate as the stream-error
                                    // path: only swap the session's provider
                                    // permanently after the threshold.
                                    let streak = self.bump_primary_failure_streak(session_id);
                                    let sticky = streak >= STICKY_FALLBACK_THRESHOLD;
                                    if let Some(ref cb) = progress_callback {
                                        let primary_from_name =
                                            self.provider_name_for_session(session_id);
                                        cb(
                                            session_id,
                                            ProgressEvent::SelfHealingAlert {
                                                message: if sticky {
                                                    format!(
                                                        "5xx error → switched to {}/{} (sticky after {} rescues within {}m)",
                                                        fb_name,
                                                        fb_model,
                                                        streak,
                                                        failure_window::STICKY_FALLBACK_WINDOW
                                                            .as_secs()
                                                            / 60
                                                    )
                                                } else {
                                                    format!(
                                                        "5xx error → rescued by {}/{} ({}/{} within {}m; primary will be tried again next turn)",
                                                        fb_name,
                                                        fb_model,
                                                        streak,
                                                        STICKY_FALLBACK_THRESHOLD,
                                                        failure_window::STICKY_FALLBACK_WINDOW
                                                            .as_secs()
                                                            / 60
                                                    )
                                                },
                                            },
                                        );
                                        if sticky {
                                            cb(
                                                session_id,
                                                ProgressEvent::ProviderSwitched {
                                                    from_name: primary_from_name,
                                                    from_model: self
                                                        .provider_model_for_session(session_id),
                                                    to_name: fb_name.clone(),
                                                    to_model: fb_model.clone(),
                                                    reason: "5xx_error".to_string(),
                                                },
                                            );
                                        }
                                    }
                                    if sticky {
                                        // Sticky: swap session provider AND
                                        // persist so subsequent iterations +
                                        // future turns use the fallback.
                                        self.swap_provider_for_session(
                                            session_id,
                                            (*fallback).clone(),
                                            (*fallback).active_subprovider_model().unwrap_or_else(
                                                || (*fallback).default_model().to_string(),
                                            ),
                                        );
                                        self.persist_sticky_pair(
                                            session_id,
                                            fb_name.clone(),
                                            fb_model.clone(),
                                        );
                                    }
                                    break;
                                }
                                Err(fb_err) => {
                                    tracing::warn!(
                                        "5xx fallback '{}/{}' failed: {}",
                                        fb_name,
                                        fb_model,
                                        fb_err
                                    );
                                    stream_tried.push(format!(
                                        "{fb_name}/{fb_model}: {}",
                                        crate::brain::provider::error::short_error_reason(&fb_err)
                                    ));
                                }
                            }
                        }

                        if let Some(resp) = stream_succeeded {
                            resp
                        } else {
                            tracing::error!(
                                "All {} 5xx fallback providers exhausted: [{}]",
                                stream_candidates.len(),
                                stream_candidates
                                    .iter()
                                    .map(|p| p.name())
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            );
                            emit_retry_notices(
                                &self.provider_for_session(session_id),
                                session_id,
                                progress_callback.as_ref(),
                            );
                            // #952: human-readable chain-exhaustion summary.
                            if let Some(ref cb) = progress_callback {
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message:
                                            crate::brain::provider::error::chain_exhausted_summary(
                                                &stream_active_name,
                                                &stream_primary_reason,
                                                &stream_tried,
                                            ),
                                    },
                                );
                            }
                            return Err(AgentError::Provider(last_err));
                        }
                    }
                }
                Err(e) => {
                    // Any non-5xx provider error (405, 404, 400, etc.) —
                    // walk the entire fallback chain before giving up.
                    let err_msg = e.to_string();
                    let active_name = self.provider_name_for_session(session_id);
                    tracing::warn!(
                        "Provider error from {}: {} — walking fallback chain",
                        active_name,
                        err_msg
                    );
                    self.record_provider_feedback(
                        session_id,
                        "provider_error",
                        &format!("{}/{}", active_name, model_name),
                        Some(&err_msg),
                    );

                    let chain = self.fallback_chain_snapshot();
                    let fallback_candidates: Vec<_> =
                        chain.iter().filter(|p| p.name() != active_name).collect();

                    if fallback_candidates.is_empty() {
                        tracing::warn!(
                            "No fallback providers configured for {} error — \
                             user should configure a fallback chain",
                            active_name
                        );
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: "No fallback provider available. \
                                         Configure one with /models"
                                        .to_string(),
                                },
                            );
                        }
                        return Err(AgentError::Provider(e));
                    }

                    // #952: capture the PRIMARY's reason before the walk
                    // overwrites last_err, and ledger every tried fallback.
                    let primary_reason = crate::brain::provider::error::short_error_reason(&e);
                    let mut last_err = e;
                    let mut tried: Vec<String> = Vec::new();
                    let mut succeeded = None;

                    for fallback in &fallback_candidates {
                        let fb_name = fallback.name().to_string();
                        let fb_model = fallback.default_model().to_string();
                        tracing::info!(
                            "Fallback trying '{}/{}' for provider error",
                            fb_name,
                            fb_model
                        );
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "Trying fallback '{}/{}'...",
                                        fb_name, fb_model
                                    ),
                                },
                            );
                        }

                        let mut fb_req =
                            LLMRequest::new(fb_model.clone(), context.messages.clone())
                                .with_max_tokens(self.request_max_tokens_for_session(session_id));
                        fb_req.working_directory = Some(
                            self.get_working_directory_for_session(session_id)
                                .to_string_lossy()
                                .to_string(),
                        );
                        fb_req.session_id = Some(session_id);
                        if let Some(system) = &context.system_brain {
                            fb_req = fb_req.with_system(system.clone());
                        }
                        if self.tool_registry.count() > 0 {
                            fb_req = fb_req.with_tools(self.tool_schemas_for_session(session_id));
                        }

                        // Swap provider for this session so stream_complete
                        // uses the fallback
                        self.swap_provider_for_session(
                            session_id,
                            (*fallback).clone(),
                            (*fallback)
                                .active_subprovider_model()
                                .unwrap_or_else(|| (*fallback).default_model().to_string()),
                        );

                        match self
                            .stream_complete(
                                session_id,
                                fb_req,
                                cancel_token.as_ref(),
                                progress_callback.as_ref(),
                                if is_cli_provider {
                                    self.message_queue_callback.as_ref()
                                } else {
                                    None
                                },
                                if is_cli_provider {
                                    Some(&queued_buf)
                                } else {
                                    None
                                },
                                false,
                            )
                            .await
                        {
                            Ok(resp) => {
                                tracing::info!("Fallback '{}/{}' succeeded", fb_name, fb_model);
                                succeeded = Some(resp);
                                break;
                            }
                            Err(fb_err) => {
                                tracing::warn!(
                                    "Fallback '{}/{}' also failed: {}",
                                    fb_name,
                                    fb_model,
                                    fb_err
                                );
                                tried.push(format!(
                                    "{fb_name}/{fb_model}: {}",
                                    crate::brain::provider::error::short_error_reason(&fb_err)
                                ));
                                last_err = fb_err;
                            }
                        }
                    }

                    if let Some(resp) = succeeded {
                        resp
                    } else {
                        tracing::error!(
                            "All {} fallback providers exhausted: [{}]",
                            fallback_candidates.len(),
                            fallback_candidates
                                .iter()
                                .map(|p| p.name())
                                .collect::<Vec<_>>()
                                .join(", "),
                        );
                        emit_retry_notices(
                            &self.provider_for_session(session_id),
                            session_id,
                            progress_callback.as_ref(),
                        );
                        // #952: human-readable chain-exhaustion summary.
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: crate::brain::provider::error::chain_exhausted_summary(
                                        &active_name,
                                        &primary_reason,
                                        &tried,
                                    ),
                                },
                            );
                        }
                        return Err(AgentError::Provider(last_err));
                    }
                }
            };

            // Surface any in-place retries the provider performed (connection
            // blip, 5xx, rate limit) so the user SEES the resilience working
            // instead of an apparent instant jump to fallback. Drained once
            // per iteration; the FallbackProvider aggregates retries from the
            // primary and every fallback tried this turn. The failure exits
            // above drain too, so a turn that retried and then gave up still
            // reports the attempts (#949).
            emit_retry_notices(
                &self.provider_for_session(session_id),
                session_id,
                progress_callback.as_ref(),
            );

            // Surface any sticky-fallback swap that the FallbackProvider
            // performed during this turn so the user sees which provider/model
            // is now active. Fires at most once per swap.
            // Re-read who owns tool execution and context for THIS iteration,
            // before the tool-execution decision below (#1100).
            //
            // Six sites can move this session onto a different provider
            // mid-turn: the rate-limit walk, the stream-fallback walk, the
            // 5xx walk, the empty-response rescue, a `FallbackProvider`
            // sticky promotion, and `force_next_fallback`. Refreshing at each
            // of them is how the bug happened. The rate-limit walk already
            // refreshes `model_name` for exactly this reason and simply did
            // not know there were two more bindings to carry. One refresh at
            // the single point every path converges on cannot be forgotten by
            // the seventh site.
            refresh_cli_flags(
                &self.provider_for_session(session_id),
                &mut is_cli_provider,
                &mut cli_owns_context,
            );

            let rotated_this_iteration = self.provider_for_session(session_id).take_swap_event();
            if let Some(ref swap) = rotated_this_iteration {
                let reason = if swap.reason.is_empty() {
                    "unavailable".to_string()
                } else {
                    swap.reason.clone()
                };
                if let Some(ref cb) = progress_callback {
                    cb(
                        session_id,
                        ProgressEvent::SelfHealingAlert {
                            message: format!(
                                "Switched to {}/{} — {}/{} {}",
                                swap.to_name,
                                swap.to_model,
                                swap.from_name,
                                swap.from_model,
                                reason
                            ),
                        },
                    );
                    // Structured follow-up so UIs can update the session footer
                    // without parsing the alert text above.
                    cb(
                        session_id,
                        ProgressEvent::ProviderSwitched {
                            from_name: swap.from_name.clone(),
                            from_model: swap.from_model.clone(),
                            to_name: swap.to_name.clone(),
                            to_model: swap.to_model.clone(),
                            reason,
                        },
                    );
                }
                // Persist the locked pair to DB even when no progress
                // callback is wired (e.g. a2a, RSI, subagent paths) and
                // independently of whether the consuming UI handles
                // ProviderSwitched.
                self.persist_sticky_pair(session_id, swap.to_name.clone(), swap.to_model.clone());
            }

            // CLI providers return "Prompt is too long" as a successful response
            // with is_error=true in the content — detect and re-route to the
            // same emergency compaction path used for Err cases above.
            let is_cli_too_long = is_cli_provider
                && response.content.iter().any(|b| {
                    if let ContentBlock::Text { text } = b {
                        text.trim().starts_with("Prompt is too long")
                            || text.contains("prompt is too long")
                    } else {
                        false
                    }
                });

            if is_cli_too_long {
                tracing::warn!(
                    "CLI returned 'Prompt is too long' as content — triggering emergency compaction"
                );
                // Emergency pre-truncate: 85% of max (scales with custom providers)
                let too_long_pre_truncate =
                    (context.max_tokens as f64 * 0.85).max(16_000.0) as usize;
                if context.token_count > too_long_pre_truncate {
                    context.hard_truncate_to(too_long_pre_truncate);
                }
                let compacted = self
                    .compact_context(
                        session_id,
                        &mut context,
                        &model_name,
                        cancel_token.as_ref(),
                        CompactionNotifier::auto_from(session_id, progress_callback.as_ref()),
                    )
                    .await;
                emit_retry_notices(
                    &self.provider_for_session(session_id),
                    session_id,
                    progress_callback.as_ref(),
                );
                match compacted {
                    Ok((_summary, applied_marker)) => {
                        // #1928: persist the marker the apply welded in, not a
                        // banner rebuilt here — this inline copy was one of the
                        // construction sites that could drift from the live
                        // context.
                        let _ = message_service
                            .create_message(session_id, "user".to_string(), applied_marker)
                            .await;
                    }
                    Err(e) => {
                        tracing::error!(
                            "Emergency compaction also failed: {} — hard truncating",
                            e
                        );
                        const KEEP_MESSAGES: usize = 24;
                        let total = context.messages.len();
                        if total > KEEP_MESSAGES {
                            context.messages.drain(..total - KEEP_MESSAGES);
                        }
                    }
                }
                // Emit updated token count so TUI reflects post-compaction value.
                if let Some(ref cb) = progress_callback {
                    cb(session_id, ProgressEvent::TokenCount(context.token_count));
                }
                // Re-run the loop iteration with the compacted context
                continue;
            }

            // Track token usage — fall back to tiktoken estimate when provider
            // doesn't report usage (e.g. MiniMax streaming ignores include_usage,
            // some MLX streaming paths drop the final usage chunk).
            //
            // The fallback must match the server's `prompt_tokens` semantic:
            // messages + system prompt + tool schemas. `base_context_tokens()`
            // already sums system + tool schemas, so we add
            // `context.token_count` (messages) on top. Previously we only
            // added tool tokens and dropped the 20k system prompt baseline,
            // producing a ~20k undercount that made the UI ctx counter
            // display 7k when the real prompt was 23k+ (post-compaction).
            //
            // Two different numbers come out of one usage report and they must
            // not be conflated (#1636):
            //
            //   * `call_input_tokens` is the BILLING input, non-cached only,
            //     because `calculate_cost_with_cache` charges `cache_read` and
            //     `cache_creation` on top of it.
            //   * `call_context_tokens` is the PROMPT SIZE the model actually
            //     saw, cached prefix included, which is what the ctx meter
            //     displays and what the compaction budget anchors on.
            //
            // On a caching provider these differ by two orders of magnitude: a
            // 98% hit rate makes the non-cached delta a few hundred tokens
            // against a 35k prompt. Anchoring the budget on the billing figure
            // would hold the context at ~2% forever and compaction would never
            // fire.
            //
            // The "did the provider report anything" guard is `context_input()`,
            // not `input_tokens`: a fully cached prefix is a legitimate zero in
            // the billing field, and that is not the same as silence.
            // Claude CLI reports its OWN internal cache in
            // `context_input()`, not the prompt we sent. The calibration
            // branch below already refuses that field for this provider;
            // the ctx meter and the compaction budget read the same field
            // and have to refuse it here too. Anchoring on the cache
            // figure pushed the budget to 118% of the window on one turn
            // and collapsed it to 2 tokens on another, so compaction both
            // fired unprompted and stopped firing at all (#1677).
            let is_claude_cli = self.provider_for_session(session_id).name() == "claude-cli";
            let reported_usage = response.usage.context_input() > 0;
            let tiktoken_estimate = || {
                let baseline = self.base_context_tokens();
                let estimate = context.token_count as u32 + baseline;
                tracing::debug!(
                    "Provider reported no usage, using tiktoken estimate: {} ({} msg + {} baseline (system + tool schemas))",
                    estimate,
                    context.token_count,
                    baseline
                );
                estimate
            };
            let call_input_tokens = if reported_usage {
                response.usage.input_tokens
            } else {
                tiktoken_estimate()
            };
            // Real-time data only: whatever the provider reported, no
            // local-tokenizer calibration and no learned ratio. The ctx footer
            // reads `response.context_tokens` downstream and shows the user the
            // exact same prompt size the API just told us about.
            let call_context_tokens = if reported_usage && !is_claude_cli {
                response.usage.context_input()
            } else {
                tiktoken_estimate()
            };
            // Anchor the context budget on what the provider counted, so
            // compaction measures the request that will actually be sent
            // rather than a local estimate that omits the tool schemas and
            // disagrees with their tokenizer. Guarded by the same
            // over-reporting check the ctx counter uses: an endpoint adding a
            // flat overhead to every call must not drag the budget up and
            // compact a context that was never close to full.
            if reported_usage
                && !is_claude_cli
                && !is_implausible_token_report(
                    context.token_count,
                    self.base_context_tokens() as usize,
                    call_context_tokens as usize,
                )
            {
                context.record_provider_reported_tokens(call_context_tokens as usize);
            }
            total_input_tokens += call_input_tokens;
            last_iter_input_tokens = call_context_tokens;
            total_output_tokens += response.usage.output_tokens;
            if let Some(secs) = response.streaming_active_secs {
                total_streaming_active_secs += secs;
            }
            // Use billing fields (cumulative across CLI rounds) when available
            total_cache_creation += if response.usage.billing_cache_creation > 0 {
                response.usage.billing_cache_creation
            } else {
                response.usage.cache_creation_tokens
            };
            total_cache_read += if response.usage.billing_cache_read > 0 {
                response.usage.billing_cache_read
            } else {
                response.usage.cache_read_tokens
            };
            // Fold the provider's reported dollars for this iteration into
            // the turn total; iterations that report none disqualify the sum
            // and the turn falls back to table math (#1707).
            match response.usage.cost_usd {
                Some(c) => reported_cost_sum = Some(reported_cost_sum.unwrap_or(0.0) + c),
                None => cost_missing_iters += 1,
            };

            // Calibrate context token count from the provider's reported usage.
            //
            // Claude CLI handles caching internally — its reported cache_read /
            // cache_creation tokens reflect Claude's own cached system prompt +
            // tool schemas + accumulated session state, NOT the conversation
            // OpenCrabs sent. Adding those to context_input() inflates the
            // counter past the model's window (e.g. 484k/200k = 242%) on every
            // turn, which then triggers spurious auto-compaction that drops
            // the in-flight request. We manage the context we send; Claude
            // manages its own cache. Trust the local tiktoken estimate that
            // already reflects what we sent in `request.messages`.
            //
            // Other CLI providers (qwen-code) re-spawn cold each turn — their
            // reported context_input() IS what we sent and is calibration-worthy.
            if is_cli_provider && !is_claude_cli {
                let cli_context = response.usage.context_input() as usize;
                if cli_context > 0 {
                    // Sanity guard: if the CLI's reported context is more
                    // than 10× the local tiktoken estimate AND the estimate
                    // is non-trivial, the CLI is almost certainly reporting
                    // a cumulative-across-rounds figure rather than the
                    // last round's prompt size. Don't trust it — keep the
                    // local estimate so the ctx % display stays sane.
                    let estimate = context.token_count;
                    if estimate >= 1000 && cli_context > estimate.saturating_mul(10) {
                        tracing::warn!(
                            "CLI context calibration REJECTED: {} → {} ({}× estimate, \
                             likely cumulative/inflated; keeping local estimate). \
                             Provider: {}, model: {}",
                            estimate,
                            cli_context,
                            cli_context / estimate.max(1),
                            self.provider_for_session(session_id).name(),
                            model_name,
                        );
                    } else {
                        tracing::info!(
                            "CLI context calibration: {} → {} (from per-call cache tokens)",
                            context.token_count,
                            cli_context,
                        );
                        context.token_count = cli_context;
                    }
                }
            } else if is_claude_cli {
                // Local estimate stays authoritative for Claude CLI — already
                // computed from `request.messages`, no API calibration needed.
                tracing::debug!(
                    "Claude CLI: keeping local estimate {} (reported context_input={} \
                     ignored — represents Claude's internal cache, not our sent context)",
                    context.token_count,
                    response.usage.context_input(),
                );
            } else {
                // The full prompt the API saw: system prompt + tool schemas +
                // messages, cached prefix included. `input_tokens` alone is the
                // non-cached remainder and would calibrate the context down to
                // the last uncached block (#1636).
                let api_input = response.usage.context_input() as usize;
                // Subtract the overhead to get the real message-only token count.
                let overhead = self.base_context_tokens() as usize;
                let real_message_tokens = api_input.saturating_sub(overhead);
                let tool_tokens = self.actual_tool_schema_tokens();
                match evaluate_token_report(context.token_count, tool_tokens, real_message_tokens) {
                    TokenReport::Adopt(actual) => {
                        tracing::debug!(
                            "Token calibration: estimated {} → API actual {}",
                            context.token_count,
                            actual,
                        );
                        context.token_count = actual;
                    }
                    TokenReport::RejectImplausible => {
                        let expected = context.token_count + tool_tokens;
                        tracing::warn!(
                            "Token usage REJECTED: provider '{}' reported {} input tokens, but \
                             the real content is ~{} ({} system+messages + {} tool schemas) — \
                             {}× over. Endpoint is over-reporting; keeping local estimate {} so \
                             the ctx counter and cost stay accurate.",
                            self.provider_for_session(session_id).name(),
                            api_input,
                            expected,
                            context.token_count,
                            tool_tokens,
                            real_message_tokens / expected.max(1),
                            context.token_count,
                        );
                    }
                    TokenReport::BelowSanityFloor => {
                        if real_message_tokens > 0 {
                            tracing::warn!(
                                "Token calibration skipped: api_input={}, overhead={}, result={} (below sanity threshold)",
                                api_input,
                                overhead,
                                real_message_tokens,
                            );
                        }
                    }
                    TokenReport::ImplausibleDrop => {
                        tracing::warn!(
                            "Token calibration skipped: provider '{}' reported {} message tokens \
                             against a local estimate of {} — too steep a drop to be a real \
                             prompt, keeping the estimate.",
                            self.provider_for_session(session_id).name(),
                            real_message_tokens,
                            context.token_count,
                        );
                    }
                }
            }
            // Fire real-time token count update after every API response
            if let Some(ref cb) = progress_callback {
                cb(session_id, ProgressEvent::TokenCount(context.token_count));
            }
            // When a channel override is active, also fire to the service-level callback
            // so the TUI ctx display stays in sync with channel interactions.
            if has_progress_override && let Some(ref cb) = self.progress_callback {
                cb(session_id, ProgressEvent::TokenCount(context.token_count));
            }

            // Post-calibration compaction check. Skip ONLY when the CLI
            // owns its session (claude-cli with --resume). Qwen is spawned
            // cold every turn so we MUST compact for it.
            if let Some(ref outcome) = if cli_owns_context {
                None
            } else {
                self.enforce_context_budget(
                    session_id,
                    &mut context,
                    &model_name,
                    cancel_token.as_ref(),
                    &progress_callback,
                    super::compaction::BudgetPhase::MidLoop,
                )
                .await
            } {
                if let Err(e) = message_service
                    .create_message(
                        session_id,
                        "user".to_string(),
                        outcome.marker(" after token calibration revealed high context usage"),
                    )
                    .await
                {
                    tracing::error!(
                        "Failed to persist post-calibration compaction marker: {}",
                        e
                    );
                }
                context.add_message(Message::user(
                    "[SYSTEM: Context was auto-compacted after calibration. \
                     Review the summary above. The \"IMMEDIATE TASK\" section tells you \
                     exactly what to do next. Continue that task immediately. \
                     Do NOT start a new topic or deviate to unrelated work.]"
                        .to_string(),
                ));
            }

            // --- CANCEL CHECK BEFORE STREAM DROP RETRY ---
            // If the user cancelled during streaming, don't retry — save partial text and break.
            if response.stop_reason.is_none()
                && let Some(ref token) = cancel_token
                && token.is_cancelled()
            {
                if is_cli_provider {
                    // CLI providers: text + tool segments were already written
                    // to the assistant row by the live persister (#269) — the
                    // cancel only needs to rebuild accumulated_text for the
                    // in-memory response and append what the persister does
                    // not cover (reasoning + trailing response text).
                    let segments: Vec<CliSegment> = cli_segments
                        .lock()
                        .map(|mut s| s.drain(..).collect())
                        .unwrap_or_default();
                    let mut pending_tools: Vec<serde_json::Value> = Vec::new();
                    for seg in segments {
                        match seg {
                            CliSegment::Text(text) => {
                                // Flush pending tools before text
                                if !pending_tools.is_empty() {
                                    let marker = format!(
                                        "\n<!-- tools-v2: {} -->\n",
                                        serde_json::to_string(&pending_tools).unwrap_or_default()
                                    );
                                    accumulated_text.push_str(&marker);
                                    pending_tools.clear();
                                }
                                // Skip a segment this turn already carries
                                // (#1070) — tool markers above still flush.
                                if is_duplicate_iteration_text(&accumulated_text, &text) {
                                    continue;
                                }
                                if !accumulated_text.is_empty() {
                                    accumulated_text.push_str("\n\n");
                                }
                                accumulated_text.push_str(&text);
                            }
                            CliSegment::Tool(entry) => {
                                pending_tools.push(entry);
                            }
                        }
                    }
                    // Flush trailing tools
                    if !pending_tools.is_empty() {
                        let marker = format!(
                            "\n<!-- tools-v2: {} -->\n",
                            serde_json::to_string(&pending_tools).unwrap_or_default()
                        );
                        accumulated_text.push_str(&marker);
                    }

                    // Barrier: buffered tool markers land before the cancel tail.
                    if let Some(ref tx) = cli_persist_tx {
                        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                        if tx.send(CliPersist::Flush(ack_tx)).is_ok() && ack_rx.await.is_err() {
                            tracing::warn!("CLI live persist: flush ack dropped (cancel)");
                        }
                    }

                    let mut cancel_content = String::new();

                    // Reasoning
                    if let Some(ref reasoning) = reasoning_text
                        && !reasoning.trim().is_empty()
                    {
                        cancel_content.push_str(&format!(
                            "<!-- reasoning -->\n{}\n<!-- /reasoning -->\n\n",
                            reasoning
                        ));
                    }

                    // Also extract any text from the partial response not yet
                    // emitted as IntermediateText (trailing text after last tool)
                    for block in &response.content {
                        if let ContentBlock::Text { text } = block
                            && !text.trim().is_empty()
                        {
                            // Only append if not already covered by streamed
                            // segments. Shares the #1070 helper so every append
                            // site applies one rule instead of drifting apart.
                            if !is_duplicate_iteration_text(&accumulated_text, text) {
                                cancel_content.push_str(&format!("{}\n\n", text));
                                if !accumulated_text.is_empty() {
                                    accumulated_text.push_str("\n\n");
                                }
                                accumulated_text.push_str(text);
                            }
                        }
                    }

                    if !cancel_content.is_empty()
                        && let Err(e) = message_service
                            .append_content(assistant_db_msg.id, &cancel_content)
                            .await
                    {
                        tracing::warn!("CLI cancel persist: append failed: {e}");
                    }
                } else {
                    // Non-CLI: persist partial reasoning + text from response blocks
                    // as a single append so the `<!-- reasoning -->` marker stays
                    // attached to its iteration's text (same chronological-layout
                    // contract as the regular per-iteration persist below).
                    let mut cancel_content = String::new();
                    if let Some(ref reasoning) = reasoning_text
                        && !reasoning.trim().is_empty()
                    {
                        cancel_content.push_str(&format!(
                            "<!-- reasoning -->\n{}\n<!-- /reasoning -->\n\n",
                            reasoning
                        ));
                    }
                    for block in &response.content {
                        if let ContentBlock::Text { text } = block
                            && !text.trim().is_empty()
                            // Same guard its sibling above already had (#1070):
                            // don't re-append text the turn already carries.
                            && !is_duplicate_iteration_text(&accumulated_text, text)
                        {
                            if !accumulated_text.is_empty() {
                                accumulated_text.push_str("\n\n");
                            }
                            accumulated_text.push_str(text);
                            cancel_content.push_str(&format!("{}\n\n", text));
                        }
                    }
                    if !cancel_content.is_empty() {
                        let _ = message_service
                            .append_content(assistant_db_msg.id, &cancel_content)
                            .await;
                    }
                }
                tracing::info!(
                    "Stream cancelled by user — saving partial text ({} chars)",
                    accumulated_text.len()
                );
                break;
            }

            // --- STREAM DROP DETECTION ---
            // If stop_reason is None, the stream ended without [DONE]/MessageStop.
            // This means a network interruption, provider timeout, or dropped connection.
            // The response may contain partial/corrupt data. Retry instead of proceeding
            // with garbage that silently drops the task.
            if response.stop_reason.is_none() {
                if stream_retry_count < MAX_STREAM_RETRIES {
                    stream_retry_count += 1;
                    tracing::warn!(
                        "🔄 Stream dropped without completion (no stop_reason) at iteration {}. \
                         Retrying ({}/{}) — partial content discarded.",
                        iteration,
                        stream_retry_count,
                        MAX_STREAM_RETRIES,
                    );
                    // Emit transient retry notification
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::RetryAttempt {
                                attempt: stream_retry_count,
                                max: MAX_STREAM_RETRIES,
                                reason: "stream dropped".to_string(),
                            },
                        );
                    }
                    // Subtract the tokens we just counted — they'll be re-counted on retry
                    total_input_tokens -= response.usage.input_tokens;
                    total_output_tokens -= response.usage.output_tokens;
                    total_cache_creation =
                        total_cache_creation.saturating_sub(response.usage.cache_creation_tokens);
                    total_cache_read =
                        total_cache_read.saturating_sub(response.usage.cache_read_tokens);
                    // Mirror the token subtraction for the cost accumulator:
                    // this iteration will be re-counted on the retry.
                    match response.usage.cost_usd {
                        Some(c) => reported_cost_sum = reported_cost_sum.map(|s| s - c),
                        None => cost_missing_iters = cost_missing_iters.saturating_sub(1),
                    }
                    // Don't increment iteration — this is a retry, not a new turn
                    iteration -= 1;
                    continue;
                } else {
                    // Primary stream exhausted its retries. This is NOT a
                    // definitive failure yet — a fallback provider may still
                    // complete the request, so log a WARN here and reserve the
                    // "could not be completed" ERROR for the no-fallback branch
                    // below. Emitting the scary ERROR before the fallback made
                    // operators blame the primary for an outcome the fallback
                    // recovered from (#260).
                    tracing::warn!(
                        "⚠️ Provider stream dropped {} times consecutively with 0 content \
                         (content_blocks: {}, stop_reason: None) — attempting fallback before failing.",
                        MAX_STREAM_RETRIES,
                        response.content.len(),
                    );

                    // Record as feedback for RSI analysis
                    self.record_provider_feedback(
                        session_id,
                        "stream_drop",
                        &model_name,
                        Some(&format!(
                            "retries={}, content_blocks={}, provider={}",
                            MAX_STREAM_RETRIES,
                            response.content.len(),
                            self.provider_for_session(session_id).name(),
                        )),
                    );

                    // Try to fallback to next provider before giving up
                    let fallback_reason =
                        format!("stream dropped {} times with 0 content", MAX_STREAM_RETRIES,);
                    if self.provider_for_session(session_id).force_next_fallback(
                        &fallback_reason,
                        &self.provider_model_for_session(session_id),
                    ) {
                        tracing::info!(
                            "🔄 Fallback triggered after stream drops — retrying with next provider"
                        );
                        // Emit self-heal alert so user sees the fallback in TUI
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "Stream dropped {} times — switching to fallback provider",
                                        MAX_STREAM_RETRIES,
                                    ),
                                },
                            );
                        }
                        // Reset and retry with the new provider
                        stream_retry_count = 0;
                        total_input_tokens -= response.usage.input_tokens;
                        total_output_tokens -= response.usage.output_tokens;
                        total_cache_creation = total_cache_creation
                            .saturating_sub(response.usage.cache_creation_tokens);
                        total_cache_read =
                            total_cache_read.saturating_sub(response.usage.cache_read_tokens);
                        // Cost accumulator mirrors the token subtraction —
                        // the fallback provider will re-report this call.
                        match response.usage.cost_usd {
                            Some(c) => reported_cost_sum = reported_cost_sum.map(|s| s - c),
                            None => cost_missing_iters = cost_missing_iters.saturating_sub(1),
                        }
                        iteration -= 1;
                        continue;
                    }

                    // No fallback available — NOW it's a definitive failure.
                    // Log the ERROR here (not before the fallback attempt) and
                    // inject the message so the partial response carries it.
                    let drop_msg = format!(
                        "Provider stream dropped {} times consecutively. \
                         The request could not be completed. \
                         Check the logs (see Known paths) for details.",
                        MAX_STREAM_RETRIES,
                    );
                    tracing::error!(
                        "🚨 {} No fallback provider available. Content blocks: {}, stop_reason: None",
                        drop_msg,
                        response.content.len(),
                    );
                    if response.content.iter().all(
                        |b| !matches!(b, ContentBlock::Text { text } if !text.trim().is_empty()),
                    ) {
                        response.content.push(ContentBlock::Text {
                            text: format!("⚠️ {}", drop_msg),
                        });
                    }
                    stream_retry_count = 0;
                }
            } else {
                // Successful stream completion — reset retry counter
                // Analytics (#897): if the stream dropped and retried before
                // succeeding, record a recovery. tool_count = tool calls in the
                // recovered response.
                if stream_retry_count > 0 {
                    let tool_count = response
                        .content
                        .iter()
                        .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                        .count() as i64;
                    let prov = self.provider_name_for_session(session_id);
                    let mdl = Some(self.provider_model_for_session(session_id));
                    let sid = session_id.to_string();
                    crate::db::repository::AnalyticsEventRepository::emit_streaming_recovery(
                        &sid,
                        Some(&prov),
                        mdl.as_deref(),
                        tool_count,
                    );
                }
                stream_retry_count = 0;
            }

            // Separate text blocks and tool use blocks from the response
            tracing::debug!("Response has {} content blocks", response.content.len());

            // ── Gaslighting refusal strip ───────────────────────────────
            // Some providers (notably dialagram qwen-thinking) emit canned
            // "I can't analyze this image / tool isn't available" refusals
            // even though the tools ARE available globally. The detector
            // is narrow enough (first-person refusal opening + image
            // context, OR exact phrase from known quirks) that we can
            // strip unconditionally without false-positive risk.
            if is_dialagram {
                let mut stripped_bytes = 0usize;
                let mut stripped_preview = String::new();
                response.content.retain_mut(|b| match b {
                    ContentBlock::Text { text } => {
                        // First try stripping just a leading preamble so
                        // we keep any legitimate draft that follows the
                        // gaslighting opener in the same block.
                        if let Some(remainder) =
                            super::gaslighting::strip_gaslighting_preamble(text)
                        {
                            let removed = text.len().saturating_sub(remainder.len());
                            stripped_bytes += removed;
                            if stripped_preview.is_empty() {
                                stripped_preview = text.chars().take(80).collect::<String>();
                            }
                            if remainder.trim().is_empty() {
                                return false;
                            }
                            *text = remainder;
                            return true;
                        }
                        // Fallback: whole-block match (small pure refusals)
                        if super::gaslighting::is_gaslighting_preamble(text) {
                            stripped_bytes += text.len();
                            if stripped_preview.is_empty() {
                                stripped_preview = text.chars().take(80).collect::<String>();
                            }
                            return false;
                        }
                        true
                    }
                    _ => true,
                });
                if stripped_bytes > 0 {
                    let had_tool_use = response
                        .content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
                    tracing::warn!(
                        "[GASLIGHT_STRIP] dropped {} bytes of refusal (had_tool_use={}) — preview: {:?}",
                        stripped_bytes,
                        had_tool_use,
                        stripped_preview
                    );
                    // Wipe the TUI's in-progress streaming buffer so the
                    // lie doesn't stay on screen.
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::StripStreamedContent {
                                bytes: stripped_bytes,
                                reason: format!(
                                    "gaslighting refusal ({} bytes) stripped (had_tool_use={})",
                                    stripped_bytes, had_tool_use
                                ),
                            },
                        );
                    }
                }
            }

            let mut iteration_text = String::new();
            let mut tool_uses: Vec<(String, String, Value)> = Vec::new();

            for (i, block) in response.content.iter().enumerate() {
                match block {
                    ContentBlock::Text { text } => {
                        tracing::debug!(
                            "Block {}: Text ({}...)",
                            i,
                            &text.chars().take(50).collect::<String>()
                        );
                        if !text.trim().is_empty() {
                            if !iteration_text.is_empty() {
                                iteration_text.push_str("\n\n");
                            }
                            iteration_text.push_str(text);
                        }
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        // GRANULAR LOG: Tool call received from provider
                        let input_keys: Vec<_> = input
                            .as_object()
                            .map(|o| o.keys().cloned().collect())
                            .unwrap_or_default();
                        tracing::info!(
                            "[TOOL_EXEC] 📥 Tool call received: name={}, id={}, input_keys={:?}",
                            name,
                            id,
                            input_keys
                        );

                        // Check for empty/Invalid input — only warn when the
                        // tool actually has required parameters. Tools like
                        // browser_screenshot accept zero args (selector is
                        // optional) and call validly with `{}`; logging an
                        // ERROR there is pure noise and showed up in logs as
                        // 4+ false positives per browser session.
                        if input.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                            let has_required = self
                                .tool_registry
                                .get(name.as_str())
                                .map(|t| {
                                    t.input_schema()
                                        .get("required")
                                        .and_then(|r| r.as_array())
                                        .map(|a| !a.is_empty())
                                        .unwrap_or(false)
                                })
                                .unwrap_or(true);
                            if has_required {
                                tracing::error!(
                                    "[TOOL_EXEC] ⚠️ Tool '{}' received empty input — tool call will fail",
                                    name
                                );
                            } else {
                                tracing::debug!(
                                    "[TOOL_EXEC] Tool '{}' called with empty input (schema has no required fields, this is fine)",
                                    name
                                );
                            }
                        }

                        // Normalize hallucinated tool names: some providers send
                        // "Plan: complete_task" instead of tool="plan" + operation="complete_task".
                        let (norm_name, norm_input) =
                            Self::normalize_tool_call(name.clone(), input.clone());

                        tool_uses.push((id.clone(), norm_name, norm_input));
                    }
                    _ => {
                        tracing::debug!("Block {}: Other content block", i);
                    }
                }
            }

            // ── Strip echoed markup ──────────────────────────────────────
            // The LLM echoes or invents HTML comment markers from context:
            // <!-- tools-v2: ... -->, <!-- lens -->, <!-- /tools-v2>, etc.
            // Strip ALL HTML comments from iteration text to prevent any
            // from leaking into Telegram/channel output or the TUI.
            if iteration_text.contains("<!--") {
                iteration_text = Self::strip_html_comments(&iteration_text);
            }

            // ── XML tool-call recovery ──────────────────────────────────
            // MiniMax (and some other providers) sometimes emit tool calls as
            // XML in the content instead of using the API's tool_calls field.
            // Parse them into real tool_uses AND inject into response.content
            // so the context has matching ToolUse blocks for ToolResult messages.
            //
            // CRITICAL: Only strip XML blocks that were SUCCESSFULLY parsed as
            // valid tool calls. If the model is just talking ABOUT XML tags in
            // prose (e.g. release notes), parsing finds no valid JSON inside
            // the tags and we leave the text untouched.
            if Self::has_xml_tool_block(&iteration_text) {
                let parsed = Self::parse_xml_tool_calls(&iteration_text);
                if !parsed.is_empty() {
                    tracing::info!(
                        "Recovered {} XML tool call(s) from content text",
                        parsed.len()
                    );
                    for (name, input) in parsed {
                        let synthetic_id = format!("xml-{}", uuid::Uuid::new_v4().simple());
                        tool_uses.push((synthetic_id.clone(), name.clone(), input.clone()));
                        response.content.push(ContentBlock::ToolUse {
                            id: synthetic_id,
                            name,
                            input,
                        });
                    }
                    // Only strip after successful parse — prose mentions are left alone
                    iteration_text = Self::strip_xml_tool_calls(&iteration_text);
                }
            }

            // ── DB persistence ──────────────────────────────────────────
            // CLI providers: text + tool segments were already appended to the
            // assistant row AS THEY STREAMED by the live persister (#269) — a
            // restart mid-turn keeps everything up to the last completed
            // segment. Here we only rebuild accumulated_text from the ordered
            // segments (for the in-memory response) and append the reasoning
            // block, after a Flush barrier so trailing tool markers land first.
            if is_cli_provider {
                // Interleaved text + tool markers from streaming events —
                // display/accumulation only, the DB copy is already written.
                let segments: Vec<CliSegment> = cli_segments
                    .lock()
                    .map(|mut s| s.drain(..).collect())
                    .unwrap_or_default();
                let mut pending_tools: Vec<serde_json::Value> = Vec::new();
                for seg in segments {
                    match seg {
                        CliSegment::Text(text) => {
                            // Flush pending tools before text
                            if !pending_tools.is_empty() {
                                let marker = format!(
                                    "\n<!-- tools-v2: {} -->\n",
                                    serde_json::to_string(&pending_tools).unwrap_or_default()
                                );
                                accumulated_text.push_str(&marker);
                                pending_tools.clear();
                            }
                            // Skip a segment this turn already carries (#1070):
                            // CLI providers replay the whole prompt each
                            // iteration, so the model restates its full answer
                            // after a tool round instead of continuing.
                            if is_duplicate_iteration_text(&accumulated_text, &text) {
                                continue;
                            }
                            if !accumulated_text.is_empty() {
                                accumulated_text.push_str("\n\n");
                            }
                            accumulated_text.push_str(&text);
                        }
                        CliSegment::Tool(entry) => {
                            pending_tools.push(entry);
                        }
                    }
                }
                // Flush trailing tools
                if !pending_tools.is_empty() {
                    let marker = format!(
                        "\n<!-- tools-v2: {} -->\n",
                        serde_json::to_string(&pending_tools).unwrap_or_default()
                    );
                    accumulated_text.push_str(&marker);
                }

                // Barrier: let the live persister write any buffered tool
                // markers before the reasoning block so content stays ordered.
                if let Some(ref tx) = cli_persist_tx {
                    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
                    if tx.send(CliPersist::Flush(ack_tx)).is_ok() && ack_rx.await.is_err() {
                        tracing::warn!("CLI live persist: flush ack dropped (final)");
                    }
                }

                // CLI providers (opencode, claude, qwen-cli) maintain their own
                // conversation history server-side via session IDs, so writing
                // reasoning markers into our DB content doesn't feed back into
                // the model's context — no leak risk like the non-CLI path.
                // The block lands after the streamed segments (chronologically
                // it belongs to this turn either way; only the position within
                // the row changed when live persistence arrived).
                if let Some(ref reasoning) = reasoning_text
                    && !reasoning.trim().is_empty()
                {
                    let reasoning_block =
                        format!("<!-- reasoning -->\n{}\n<!-- /reasoning -->\n\n", reasoning);
                    if let Err(e) = message_service
                        .append_content(assistant_db_msg.id, &reasoning_block)
                        .await
                    {
                        tracing::warn!("CLI persist: reasoning append failed: {e}");
                    }
                }
            } else {
                // Non-CLI: per-iteration write of `<!-- reasoning -->` marker +
                // iteration text into the SAME `content` column the CLI path
                // uses. Markers are stripped before the next turn's LLM
                // context is built (see top of run_tool_loop:
                // `strip_llm_artifacts` on db_messages when !is_cli_provider),
                // so the model never sees them in its history and can't echo
                // them back. Persisting per-iteration keeps the chronological
                // layout (think → text → tools → think → text → …) intact on
                // session reload, matching the live streamed view exactly.
                //
                // EXCEPTION: phantom iterations (narrated actions, zero
                // tool_use blocks) are operational scaffolding for the
                // self-heal retry loop, not turn history. Persisting them
                // pollutes the DB row that gets reloaded as assistant
                // context on the next turn (and on session reconnect —
                // matches the 34-entry Telegram session reported in
                // discussion #86 / gist 85cfdc26), so the model sees its
                // own past phantoms and repeats the pattern. Skip the
                // append on phantom iterations; the eventual successful
                // iteration (where a real tool runs after the self-heal
                // nudge or after a sticky-fallback swap) gets persisted
                // normally because by then the phantom signature has
                // been replaced.
                // The skip must agree with the turn-end phantom verdict
                // (#458): after successful tool calls, a text with no
                // forward-looking intent phrase is a completion ack, not a
                // phantom — the detector exonerates it, so the persist must
                // too, or the final completion streams to the screen and
                // vanishes from the DB on reload.
                let iteration_is_phantom = !iteration_text.is_empty()
                    && tool_uses.is_empty()
                    && super::phantom::has_phantom_tool_intent_no_tools(&iteration_text)
                    && (tool_calls_completed_this_turn == 0
                        || super::phantom::has_forward_intent_post_success(&iteration_text));

                let mut iter_content = String::new();
                if let Some(ref reasoning) = reasoning_text
                    && !reasoning.trim().is_empty()
                {
                    iter_content.push_str(&format!(
                        "<!-- reasoning -->\n{}\n<!-- /reasoning -->\n\n",
                        reasoning
                    ));
                }
                if iteration_is_phantom {
                    tracing::debug!(
                        "[phantom] Persisting phantom-blocked iteration with flag \
                         (text_len={}, has_reasoning={})",
                        iteration_text.len(),
                        reasoning_text
                            .as_deref()
                            .map(|r| !r.trim().is_empty())
                            .unwrap_or(false),
                    );
                    if !iteration_text.is_empty() {
                        iter_content.push_str(&format!("{}\n\n", iteration_text));
                    }
                    // #1172: a "phantom" iteration can BE the deliverable — a
                    // verbose model narrating its (real, completed) work before
                    // EndTurn. Persist every blocked iteration under an
                    // explicit HTML-comment flag so it stays recoverable from
                    // the DB while rendering invisibly in markdown. This
                    // supersedes the #458 turn-close flush: nothing is withheld
                    // any more, so there is nothing left to flush at close.
                    // Never touches accumulated_text — the user surface still
                    // sees none of it.
                    if !iter_content.is_empty() {
                        iter_content.insert_str(0, "<!-- phantom_blocked=1 -->\n");
                        iter_content.push_str("\n<!-- /phantom_blocked=1 -->\n");
                        if let Err(e) = message_service
                            .append_content(assistant_db_msg.id, &iter_content)
                            .await
                        {
                            tracing::warn!("failed to persist phantom-blocked iteration: {e}");
                        }
                    }
                } else {
                    // Skip a verbatim restatement of what the turn already
                    // carries (#1070). The DB append is skipped with it, so a
                    // resumed session doesn't surface the duplicate either.
                    if !iteration_text.is_empty()
                        && !is_duplicate_iteration_text(&accumulated_text, &iteration_text)
                    {
                        if !accumulated_text.is_empty() {
                            accumulated_text.push_str("\n\n");
                        }
                        accumulated_text.push_str(&iteration_text);
                        iter_content.push_str(&format!("{}\n\n", iteration_text));
                    }
                    if !iter_content.is_empty()
                        && let Err(e) = message_service
                            .append_content(assistant_db_msg.id, &iter_content)
                            .await
                    {
                        tracing::warn!("failed to append iteration content to DB: {e}");
                    }
                }
            }

            tracing::debug!("Found {} tool uses to execute", tool_uses.len());

            // CLI providers handle tools internally — emit progress events for
            // TUI display (expandable tool groups) but don't execute them.
            // Break immediately after — the CLI already completed its full run.
            if is_cli_provider && !tool_uses.is_empty() {
                // Text/tool interleaving and ToolStarted/ToolCompleted events
                // are already emitted during streaming by helpers.rs
                // (cli_unflushed_text flushes at tool boundaries + stream end).
                // Tool markers already persisted atomically above via cli_segments.
                //
                // Do NOT re-emit IntermediateText here — helpers.rs already sent
                // all text blocks during streaming. Emitting again causes the
                // entire conversation text to appear duplicated in the TUI.
                iteration_text.clear();
                tool_uses.clear();
            }

            if tool_uses.is_empty() {
                // Check queued messages — stream_complete may have consumed
                // one mid-stream (stored in queued_buf), or check the queue now.
                let (queued_msg, from_buf) = {
                    let buffered = queued_buf.lock().await.take();
                    if buffered.is_some() {
                        (buffered, true)
                    } else if let Some(ref queue_cb) = self.message_queue_callback {
                        (queue_cb(session_id).await, false)
                    } else {
                        (None, false)
                    }
                };
                if let Some(queued_msg) = queued_msg {
                    tracing::info!("Injecting queued user message (from_buf={})", from_buf);
                    // #1784: the draft this round finished is NOT relayed. On a
                    // text-only round `iteration_text` is the complete answer to
                    // the PREVIOUS request, and a deliverable `IntermediateText`
                    // lands in the channel as a regular message, so the user got
                    // the superseded draft and then the folded final: two replies
                    // for one burst. The text still goes into the model's context
                    // below, so nothing is lost, and the next round produces the
                    // single final reply.
                    //
                    // The Kimi-coding reroute (#616) stays. On that endpoint this
                    // text is reasoning, and an event with an empty `text` is not
                    // deliverable by any channel: it renders in the reasoning pane
                    // and the draft stays visible where it does no harm.
                    if !iteration_text.is_empty()
                        && let Some(ref cb) = progress_callback
                        && crate::brain::provider::kimi_reasoning::streams_reasoning_inline(
                            self.provider_for_session(session_id).base_url(),
                        )
                    {
                        let combined = match reasoning_text.as_deref() {
                            Some(r) if !r.trim().is_empty() => {
                                format!("{r}\n{iteration_text}")
                            }
                            _ => iteration_text.clone(),
                        };
                        cb(
                            session_id,
                            ProgressEvent::IntermediateText {
                                text: String::new(),
                                reasoning: Some(combined),
                            },
                        );
                    }
                    // Emit QueuedUserMessage — always here, never in stream_complete
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::QueuedUserMessage {
                                text: queued_msg.display_text.clone(),
                            },
                        );
                    }
                    // Add assistant response + queued user message to context
                    let assistant_text = response
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let had_draft = !assistant_text.trim().is_empty();
                    context.add_message(Message::assistant(assistant_text));
                    // #1784: say out loud that the draft above was never sent, so
                    // the model writes one complete final reply instead of a delta
                    // pointing at an answer nobody received.
                    let injected = Message::user(super::queued_fold::injected_context(
                        had_draft,
                        &queued_msg.context_text,
                    ));
                    context.add_message(injected);
                    if let Err(e) = message_service
                        .create_message(session_id, "user".to_string(), queued_msg.display_text)
                        .await
                    {
                        tracing::error!("Failed to persist queued user message: {e}");
                    }
                    // Create a NEW assistant placeholder so the next response
                    // gets a sequence number AFTER the queued user message.
                    // Without this, the next LLM response appends to the old
                    // placeholder (created before the user message), causing
                    // the reply to appear ABOVE the user's message in the DB.
                    assistant_db_msg = message_service
                        .create_message(session_id, "assistant".to_string(), String::new())
                        .await
                        .map_err(AgentError::db)?;
                    // Retarget the CLI live persister at the fresh row so
                    // segments streamed after the queued message land below it.
                    *cli_persist_msg_id.lock().unwrap_or_else(|e| e.into_inner()) =
                        assistant_db_msg.id;
                    continue;
                }

                // ── Phantom tool call detection ──────────────────────────
                // We're inside `if tool_uses.is_empty()`. The narrow
                // intent-phrase detector is the gate: extracting real
                // tool calls from text-shaped leak formats already runs
                // upstream, so zero tool_uses + no intent phrases is a
                // legitimate text answer that must pass through.
                //
                // POST-SUCCESS EXEMPTION: if at least one tool already
                // succeeded in this turn, the text-only iteration is a
                // completion acknowledgement ("Done.", "Pushed.",
                // "Committed.") — not phantom intent. Without this
                // guard the detector mistook every successful turn's
                // wrap-up for "described actions without executing",
                // forced phantom retries, eventually rolled the
                // self-heal budget, and switched providers — all on
                // already-completed work. Symptom: 8+ "Phantom tool
                // calls detected" alerts after a clean commit+push,
                // 293s and 4683 tokens wasted finalising nothing.
                // POST-SUCCESS EXEMPTION (refined). Phantom-eligibility gate. Two regimes:
                //
                //   1. No tool call completed this turn: standard
                //      phantom check — the iteration's text must not
                //      narrate an action without a tool call.
                //   2. Tool call(s) ALREADY completed: phantom stays
                //      exempt for pure completion acks (`Done.` /
                //      `Pushed.` / `On main.`) BUT re-engages when
                //      the text carries a FORWARD-looking intent
                //      phrase (`Let me dig into …`, `I'll check the
                //      …`). Forward intent after a tool call means
                //      the model promised more work and dropped it.
                //      Logs 2026-06-03 captured this regression:
                //      "Good, on main. Let me dig into the delete
                //      invitation endpoint, the email send path, and
                //      the invite flow to find the bugs." silently
                //      closed with three promised investigations un-
                //      dispatched because the original exemption
                //      disabled phantom for the whole post-tool
                //      portion of the turn.
                // Computed once, not thrown away: the exact commands claimed
                // but never run are the strongest evidence we hold, and the
                // correction quotes them back rather than gesturing (#797).
                let uncalled_commands =
                    super::phantom::claims_uncalled_commands(&iteration_text, &turn_tool_input);
                // Facts the iteration states that exist nowhere in the
                // conversation (#1423). This is the check the post-success
                // exemption was missing: it classed a 4,664-character report
                // of invented counts and shas as a completion ack because 24
                // tools had succeeded earlier in the turn, and every other
                // fact-based branch here reads a shape that report prose does
                // not have.
                //
                // Runs on zero-tool turns too (#2038). A zero-tool turn that
                // states facts absent from the conversation is fabricated by
                // construction, and the structured-report exemption must not
                // hide it. The haystack is built only once the cheap extraction
                // finds a candidate, which keeps it off the iterations that
                // assert nothing.
                let unbacked_facts = {
                    let asserted = super::phantom::asserted_facts(&iteration_text);
                    if asserted.is_empty() {
                        Vec::new()
                    } else {
                        let evidence = Self::conversation_evidence(&context, &turn_tool_output);
                        super::phantom::unbacked_facts(&asserted, &evidence)
                    }
                };
                let phantom_eligible = !is_cli_provider
                    && (tool_calls_completed_this_turn == 0
                        // Every call this turn did nothing (#825). `true` and a
                        // bare `echo` naming a tool are the SUBSTITUTE for the
                        // work, not the work: seven green calls, none of them
                        // the one being claimed, then "I've tried telegram_send
                        // a dozen times". The exemption asks whether a tool
                        // succeeded when what vouches for a claim is whether
                        // one did anything.
                        || super::phantom::all_calls_were_null_effect(&turn_tool_input)
                        // Claimed a file was delivered when nothing sent one
                        // (#825). Catches the case null-effect misses: the
                        // turn DID do real work (write_file) and still
                        // asserted "File sent above" with no send invoked.
                        || super::phantom::claims_unsent_file(
                            &iteration_text,
                            &turn_tool_input,
                        )
                        || super::phantom::has_forward_intent_post_success(&iteration_text)
                        // A successful call vouches for the work IT did, not for
                        // every claim in the turn. One trivial `echo` used to buy
                        // blanket immunity: self-heal forced a tool, the model ran
                        // it, then reported the output of two greps it never made.
                        // Quoted evidence absent from every real result was
                        // written, not read, whatever verb introduced it (#785).
                        || super::phantom::claims_unbacked_evidence(
                            &iteration_text,
                            &turn_tool_output,
                        )
                        // A named command the turn never ran. Unlike every
                        // other check here this one does not read the wording
                        // for signals — the loop knows what it executed, so a
                        // sentence claiming `gh issue list` ran when no tool
                        // input contains it is false as a matter of fact
                        // (#789).
                        || !uncalled_commands.is_empty()
                        // A stated sha or tally that is in no tool result and
                        // no message (#1423). Same footing as the command
                        // check: the conversation is the record of what
                        // happened, so a fact absent from it was invented
                        // however the sentence around it is phrased.
                        || !unbacked_facts.is_empty());
                // Analytics (#897): if a phantom was detected earlier this turn
                // and the current iteration produced real tool calls, the
                // self-heal recovered. Mark the phantom resolved.
                if let Some(retries) = phantom_pending
                    && tool_calls_completed_this_turn > 0
                {
                    phantom_pending = None;
                    let sid = session_id.to_string();
                    crate::db::repository::AnalyticsEventRepository::emit_resolve_phantom(
                        &sid,
                        retries as i64,
                        tool_calls_completed_this_turn as i64,
                    );
                }
                if !phantom_eligible && tool_calls_completed_this_turn > 0 {
                    if option_surface_halt_seen {
                        // #31: this text-only iteration follows a suggest_options
                        // halt — it is the model's sign-off, not a phantom threat.
                        // The ack-skip keeps its text as a trailing Text entry in
                        // the flow (AFTER the option-surface Tool entry), where the
                        // options-pending reclaim lifts it as the trailer. The text
                        // preview closes the ~50-char truncation gap in forensics.
                        tracing::info!(
                            target: "phantom",
                            tools_completed = tool_calls_completed_this_turn,
                            text_len = iteration_text.len(),
                            trailer_preview = %iteration_text.chars().take(160).collect::<String>(),
                            "phantom ack classification exempted: text-only iteration follows an \
                             option-surface halt — kept as trailing trailer entry (#31)"
                        );
                    } else {
                        tracing::info!(
                            target: "phantom",
                            tools_completed = tool_calls_completed_this_turn,
                            text_len = iteration_text.len(),
                            "phantom detection skipped: turn already produced successful tool calls \
                             and the text-only iteration is a pure completion acknowledgement \
                             (no forward-looking intent phrase)"
                        );
                    }
                }
                let stuck_loop_now =
                    phantom_eligible && super::phantom::is_stuck_in_intent_loop(&iteration_text);
                if stuck_loop_now {
                    let reps = super::phantom::max_repeated_intent_line(&iteration_text);
                    tracing::warn!(
                        "Phantom intent-loop detected (same intent line repeated {}x) — escalating \
                         self-heal (nudge + fast-escalate to sticky fallback if budget half-burned).",
                        reps
                    );
                    self.record_provider_feedback(
                        session_id,
                        "phantom_intent_loop",
                        "self_heal",
                        Some(&format!(
                            "{} line-start repetitions in a single iteration",
                            reps
                        )),
                    );
                }

                // Fast-escalate to sticky fallback when the budget is
                // exhausted, or when the stuck-loop signal fires after
                // we've already burned at least half the budget. Either
                // condition means the current provider can't reach its
                // tool-call channel for this prompt and another nudge
                // won't help.
                let should_force_fallback = phantom_eligible
                    && phantom_swaps_done < MAX_PHANTOM_SWAPS
                    && super::phantom::has_phantom_tool_intent_no_tools(&iteration_text)
                    && (phantom_retries_used >= MAX_PHANTOM_RETRIES
                        || (stuck_loop_now && phantom_retries_used >= MAX_PHANTOM_RETRIES / 2));
                if should_force_fallback {
                    let fb_provider = self.provider_for_session(session_id);
                    if fb_provider.force_next_fallback(
                        "phantom_intent_loop_or_exhausted",
                        &self.provider_model_for_session(session_id),
                    ) {
                        phantom_swaps_done += 1;
                        phantom_retries_used = 0;
                        // The rolls are what stand between a stuck provider and
                        // the give-up path. Handing the turn to a different
                        // provider is a fresh attempt, not a continuation of the
                        // old one's failure, so it gets the budget back rather
                        // than inheriting a counter the previous provider spent.
                        phantom_rolls = 0;
                        // A swap replays this turn against another provider,
                        // which re-emits the calls already counted. Without
                        // this the replayed copies stack onto the run and trip
                        // the repeat threshold on calls the model made once
                        // (#1030).
                        tool_repeat.reset();
                        let new_name = fb_provider
                            .active_subprovider_name()
                            .unwrap_or_else(|| fb_provider.name().to_string());
                        let new_model = fb_provider
                            .active_subprovider_model()
                            .unwrap_or_else(|| fb_provider.default_model().to_string());
                        tracing::warn!(
                            "Self-heal escalation: swapping from '{}' to '{}/{}' (stuck={}, retries={}).",
                            self.provider_name_for_session(session_id),
                            new_name,
                            new_model,
                            stuck_loop_now,
                            phantom_retries_used
                        );
                        self.record_provider_feedback(
                            session_id,
                            "phantom_sticky_swap",
                            "self_heal",
                            Some(&format!("→ {}/{}", new_name, new_model)),
                        );
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "Self-heal switching to {}/{} (current provider can't reach \
                                         its tool channel)",
                                        new_name, new_model
                                    ),
                                },
                            );
                        }
                        self.persist_sticky_pair(session_id, new_name.clone(), new_model.clone());
                        model_name = new_model;
                        context.add_message(Message::user(
                            "[System: A different provider is now handling this turn. Invoke the \
                             correct tool through the structured tool-call API now. Do not \
                             narrate.]"
                                .to_string(),
                        ));
                        continue;
                    }
                }
                // #1506: the detectors hoist into a NAMED list of fired
                // branches. The old anonymous || chain made the WARN
                // unattributable — the 2026-09-10 kills could not be
                // diagnosed from logs alone. Eager by design: attribution
                // wants ALL fired branches, and each scan is a cheap
                // string pass at turn end.
                let fired_branches: Vec<&str> = {
                    let candidates = [
                        (
                            "intent_no_tools",
                            super::phantom::has_phantom_tool_intent_no_tools(&iteration_text),
                        ),
                        (
                            "intent_full_text",
                            super::phantom::has_phantom_tool_intent(&iteration_text),
                        ),
                        (
                            "tool_name_narrated",
                            tool_calls_completed_this_turn == 0
                                && super::phantom::mentions_registered_tool(
                                    &iteration_text,
                                    &phantom_tool_names,
                                ),
                        ),
                        (
                            "shell_fence_narrated",
                            tool_calls_completed_this_turn == 0
                                && super::fenced_command::narrates_unrun_shell_block(
                                    &iteration_text,
                                ),
                        ),
                        (
                            "unbacked_side_effects",
                            tool_calls_completed_this_turn == 0
                                && super::phantom::claims_unbacked_side_effects(&iteration_text),
                        ),
                        (
                            "bare_completion_delivery",
                            tool_calls_completed_this_turn == 0
                                && super::phantom::is_bare_completion_only(&iteration_text)
                                && super::phantom::is_delivery_intent(
                                    display_text_override.as_deref().unwrap_or(&user_message),
                                ),
                        ),
                        (
                            "media_claim_no_marker",
                            tool_calls_completed_this_turn == 0
                                && super::phantom::claims_unbacked_media_result(&iteration_text),
                        ),
                        ("uncalled_commands", !uncalled_commands.is_empty()),
                        (
                            "unbacked_evidence",
                            super::phantom::claims_unbacked_evidence(
                                &iteration_text,
                                &turn_tool_output,
                            ),
                        ),
                        ("unbacked_facts", !unbacked_facts.is_empty()),
                    ];
                    candidates
                        .into_iter()
                        .filter(|(_, fired)| *fired)
                        .map(|(branch, _)| branch)
                        .collect()
                };
                // #1506: the summary genre. Every detector matches verbatim
                // strings and a distilled completion report paraphrases by
                // construction — four legitimate 2KB batch reports with
                // verdict tables died on 2026-09-10. Owner directive: a
                // report with tables and structured data is NEVER discarded.
                // It ships untouched, spends no retry budget, and drains no
                // streamed buffer; the would-be branches land in the WARN
                // for forensics, so #1423's fabricated-table shape trades a
                // silent discard for a visible delivery with a trail.
                //
                // Branch-class gate (#2039). The report exempts the phrase
                // matchers only. A fired fact check (unbacked facts, uncalled
                // commands, unbacked evidence) kills the report whatever its
                // shape, because a fabricated table is still fabricated.
                //
                // Sign-off after an option-surface halt (#31). The halt
                // already handed the turn back with the options rendered, so
                // the trailing text-only iteration is the model closing the
                // turn, not narrating work it skipped. Phrase matchers read
                // that prose as a promise and the retry they bought posted a
                // second answer to a channel whose first answer had already
                // landed (2026-10-17 Discord). Same branch class as the
                // report exemption: a fired fact check still kills it.
                let structured_report = super::phantom::is_structured_report(&iteration_text);
                let kill = phantom_retries_used < MAX_PHANTOM_RETRIES
                    && phantom_detections_total < MAX_PHANTOM_DETECTIONS_TOTAL
                    && phantom_eligible
                    && !fired_branches.is_empty();
                if kill
                    && option_surface_halt_seen
                    && super::phantom::option_halt_exempts(&fired_branches)
                {
                    tracing::warn!(
                        branches = ?fired_branches,
                        text_len = iteration_text.len(),
                        "phantom suppressed: sign-off after an option-surface halt delivered \
                         despite fired detectors (#31)",
                    );
                } else if kill
                    && super::phantom::structured_report_exempts(structured_report, &fired_branches)
                {
                    tracing::warn!(
                        branches = ?fired_branches,
                        text_len = iteration_text.len(),
                        "phantom suppressed: structured completion report delivered despite fired detectors",
                    );
                } else if kill {
                    phantom_detections_total += 1;
                    phantom_retries_used += 1;
                    tracing::warn!(
                        "Phantom tool call detected (local={}, branches={:?}) — model described \
                         actions without executing tools. Injecting retry prompt.",
                        is_local_provider,
                        fired_branches
                    );
                    // Analytics (#897): record the phantom detection. Tagged with
                    // the active provider/model so Mission Control can break
                    // detection rates down per-model. Fire-and-forget.
                    {
                        let prov = self.provider_name_for_session(session_id);
                        let mdl = Some(self.provider_model_for_session(session_id));
                        let sid = session_id.to_string();
                        crate::db::repository::AnalyticsEventRepository::emit_phantom(
                            &sid,
                            Some(&prov),
                            mdl.as_deref(),
                        );
                    }
                    phantom_pending = Some(phantom_retries_used);
                    self.record_provider_feedback(
                        session_id,
                        "phantom_tool_call",
                        "self_heal",
                        Some(&iteration_text.chars().take(300).collect::<String>()),
                    );
                    if let Some(ref cb) = progress_callback {
                        // Wipe the phantom narration from the live TUI buffer.
                        // The iteration is discarded server-side (skipped for DB
                        // persist), but it was already streamed to the screen and
                        // would otherwise pile up across every self-heal retry
                        // (#745). Nothing in a phantom iteration is committed, so
                        // the whole streamed buffer is discardable — usize::MAX
                        // drains it all (the TUI clamps to the buffer length).
                        cb(
                            session_id,
                            ProgressEvent::StripStreamedContent {
                                bytes: usize::MAX,
                                reason: format!(
                                    "phantom self-heal narration discarded ({} bytes)",
                                    iteration_text.len()
                                ),
                            },
                        );
                        cb(
                            session_id,
                            ProgressEvent::SelfHealingAlert {
                                message: PHANTOM_RETRY_ENFORCEMENT_ALERT.into(),
                            },
                        );
                    }
                    // Inject a system correction nudge. Local
                    // models respond better to Unsloth's blunter wording;
                    // cloud models get our existing, more-specific nudge.
                    // Do NOT add the phantom text as assistant message — it
                    // pollutes context and causes the model to hallucinate
                    // new responses from the correction feedback itself.
                    // Naming the fabricated command outranks the generic
                    // wording: it cites a fact instead of a category, so the
                    // model cannot rationalise it (#797). An invented sha or
                    // tally is the same argument one level down (#1423), and
                    // the generic correction covers the triggers that identify
                    // nothing specific.
                    let nudge = if !uncalled_commands.is_empty() {
                        super::nudge::uncalled_commands_nudge(&uncalled_commands)
                    } else if !unbacked_facts.is_empty() {
                        super::nudge::unbacked_facts_nudge(&unbacked_facts)
                    } else {
                        super::nudge::no_tool_calls_nudge(is_local_provider)
                    };
                    context.add_message(Message::user(nudge));
                    continue;
                }

                // Cap hit and the fast-escalate block above couldn't swap (no
                // fallback left, or already swapped once). Roll the budget a
                // BOUNDED number of times, then give up — a model that keeps
                // narrating instead of calling tools must not loop forever
                // (#746).
                if phantom_eligible
                    && (phantom_retries_used >= MAX_PHANTOM_RETRIES
                        || phantom_detections_total >= MAX_PHANTOM_DETECTIONS_TOTAL)
                    && super::phantom::has_phantom_tool_intent_no_tools(&iteration_text)
                {
                    // #1172: the global ceiling may trip this gate while rolls
                    // remain. A roll resets the retry counter and re-nudges,
                    // which would defeat the ceiling entirely — so no roll
                    // once total detections are spent.
                    if phantom_rolls < MAX_PHANTOM_ROLLS
                        && phantom_detections_total < MAX_PHANTOM_DETECTIONS_TOTAL
                    {
                        phantom_rolls += 1;
                        tracing::warn!(
                            "Phantom retry cap rolling ({}/{} rolls, swaps_done={}) — \
                             resetting counter and re-nudging the active provider.",
                            phantom_rolls,
                            MAX_PHANTOM_ROLLS,
                            phantom_swaps_done
                        );
                        self.record_provider_feedback(
                            session_id,
                            "phantom_retry_rolling",
                            "self_heal",
                            Some(&iteration_text.chars().take(300).collect::<String>()),
                        );
                        if let Some(ref cb) = progress_callback {
                            // Wipe the narration we are discarding on this roll,
                            // same as the per-retry strip (#745), so rolls don't
                            // pile up on screen.
                            cb(
                                session_id,
                                ProgressEvent::StripStreamedContent {
                                    bytes: usize::MAX,
                                    reason: "phantom self-heal roll — narration discarded"
                                        .to_string(),
                                },
                            );
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: SELF_HEAL_BUDGET_ROLLED_ALERT.to_string(),
                                },
                            );
                        }
                        phantom_retries_used = 0;
                        context.add_message(Message::user(
                            "[System: You have repeatedly described actions without invoking any \
                             tool. STOP narrating. Pick the correct tool and call it now through the \
                             structured tool-call API. No JSON, no markdown code blocks, only a real \
                             tool_use block. If the task is already completed and you've reported the \
                             results, respond with a short confirmation (e.g., 'Done.', 'Fixed.', \
                             'Committed.') and stop — do not run additional tool calls to verify work \
                             you already did.]"
                                .to_string(),
                        ));
                        continue;
                    }
                    // Rolls exhausted: give up. The model won't call tools, so
                    // take its narration as the final answer and END the turn
                    // instead of looping forever. Commit it (phantom iterations
                    // stash rather than accumulate) so it survives reload.
                    tracing::warn!(
                        "Phantom self-heal exhausted after {} rolls — ending turn with the \
                         narration as the answer.",
                        phantom_rolls
                    );
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::SelfHealingAlert {
                                message: SELF_HEAL_EXHAUSTED_ALERT.to_string(),
                            },
                        );
                    }
                    // The narration describes work that was never done. Every
                    // provider in the chain has now been asked and none called a
                    // tool, so delivering it would hand over a description of
                    // actions as though they had happened — the #751 image case
                    // generalised: there the claim was an image, here it is
                    // whatever was narrated. Say what is true instead, and wipe
                    // the narration already streamed to the surface.
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::StripStreamedContent {
                                bytes: usize::MAX,
                                reason: "unexecuted narration discarded".to_string(),
                            },
                        );
                    }
                    let give_up_text =
                        if super::phantom::claims_unbacked_media_result(&iteration_text) {
                            "I did not actually generate or edit an image — no image tool ran this \
                         turn, so there is nothing to show. If you want an image, ask me to \
                         generate one and I will call the image tool."
                                .to_string()
                        } else {
                            "I could not complete this. I described the steps but never invoked a \
                         tool, and retrying against every provider available did not change \
                         that, so nothing was actually done. Nothing here was carried out — \
                         please ask again, and narrow the request if it was a broad one."
                                .to_string()
                        };
                    if !give_up_text.is_empty()
                        && !is_duplicate_iteration_text(&accumulated_text, &give_up_text)
                    {
                        if !accumulated_text.is_empty() {
                            accumulated_text.push_str("\n\n");
                        }
                        accumulated_text.push_str(&give_up_text);
                        if let Err(e) = message_service
                            .append_content(assistant_db_msg.id, &give_up_text)
                            .await
                        {
                            tracing::warn!("failed to persist give-up message: {e}");
                        }
                    }
                    break;
                }

                // ── Rotation continuation ──────────────────────────────
                // When Qwen OAuth rotation happens mid-task, the new account
                // gets the same request but may respond with text-only (0 tools)
                // because it's a cold start on a fresh account. Inject a
                // continuation prompt so it picks up where the previous account
                // left off. Only retry once to avoid infinite loops.
                if !rotation_retry_used && rotated_this_iteration.is_some() && iteration > 1 {
                    rotation_retry_used = true;
                    tracing::warn!(
                        "Rotation yielded 0 tool calls after {} iterations — injecting continuation prompt",
                        iteration
                    );
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::SelfHealingAlert {
                                message: ACCOUNT_ROTATION_ALERT.into(),
                            },
                        );
                    }
                    // Add the text-only response as assistant context, then nudge
                    context.add_message(Message::assistant(iteration_text));
                    context.add_message(Message::user(
                        "[System: Provider account rotation just occurred mid-task. You were \
                         actively executing tools in previous iterations but your last response \
                         contained zero tool calls. This is a continuation — review the conversation \
                         above and resume executing tools from where you left off. Do NOT summarize \
                         or re-explain. Execute the next tool call immediately.]"
                            .to_string(),
                    ));
                    continue;
                }

                // ── Empty-response + reasoning retry ─────────────────────
                // Some reasoning runtimes (local MLX Qwen3, and cloud
                // thinking models like alibaba-qwen `qwen-latest-series-
                // invite-beta-v34`) finish a turn with only
                // `reasoning_content` chunks — zero visible text, zero
                // tool calls. The user sees a tool card / their own
                // message and then nothing, which reads as a dropped
                // request with no self-heal.
                //
                // Escalation:
                //   1. Nudge up to EMPTY_REASONING_MAX_NUDGES (5) times,
                //      sharpening the system instruction each round.
                //   2. If still empty after the budget, walk the fallback
                //      chain (sticky swap + persist) so the next turn
                //      runs on a model that will actually answer.
                //   3. If no fallback succeeds, emit a final visible
                //      SelfHealingAlert so the user knows to switch
                //      providers manually — never a silent drop.
                //
                // The trigger is an empty ANSWER, full stop. It used to also
                // require 40+ chars of reasoning, which let the worst case
                // escape: a model that answered nothing AND reasoned nothing
                // failed the check, so it was never nudged and the turn ended
                // delivering nothing. In practice the first empty-with-
                // reasoning reply nudged (1/5), the retry came back completely
                // silent, and the turn was dropped — so the counter never
                // reached 2/5, the budget was never exhausted, and the fallback
                // chain below was never reached (#978). Going quieter must not
                // be a way out of the guard.
                if super::helpers::should_nudge_empty_answer(
                    iteration,
                    is_cli_provider,
                    &iteration_text,
                ) {
                    if empty_reasoning_retries < EMPTY_REASONING_MAX_NUDGES {
                        empty_reasoning_retries += 1;
                        let attempt = empty_reasoning_retries;
                        tracing::warn!(
                            "Model ended turn with reasoning but no visible response \
                             (reasoning_len={}, iteration={}, nudge {}/{}) — nudging \
                             for the actual answer.",
                            reasoning_text.as_deref().map(|r| r.len()).unwrap_or(0),
                            iteration,
                            attempt,
                            EMPTY_REASONING_MAX_NUDGES,
                        );
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "Model reasoned without answering — nudge {}/{}",
                                        attempt, EMPTY_REASONING_MAX_NUDGES,
                                    ),
                                },
                            );
                        }
                        // Each round gets a sharper system message so the
                        // model can't keep replying with more silent
                        // thinking. The last two attempts are explicit
                        // commands to stop reasoning entirely.
                        // When no tool has executed this turn the model reasoned
                        // but never acted, so it most likely still needs to CALL a
                        // tool — the nudge must encourage the structured tool call,
                        // not suppress it (the old "tool results above are
                        // sufficient — do not call more tools" text sabotaged the
                        // very tool call the agent needed and left it narrating).
                        let no_tools_yet = tool_calls_completed_this_turn == 0;
                        let nudge = super::helpers::empty_reasoning_nudge(no_tools_yet, attempt);
                        // Preserve the reasoning on the assistant turn as a
                        // Thinking block (encoded back as reasoning_content), so
                        // preserve_thinking models — qwen3.8-max-preview: thinking
                        // is always on and the COMPLETE reasoning_content MUST be
                        // echoed back — build on it instead of re-reasoning from
                        // scratch on every nudge. Dropping it (an empty assistant
                        // message) made qwen re-derive ~20k tokens per nudge, up to
                        // 5 nudges, i.e. the 200s runaway reasoning loop (#692).
                        // Nothing is appended when there is no reasoning to keep:
                        // an empty assistant message carries no information and
                        // corrupts the conversation. That is what left five
                        // `[empty assistant] [nudge]` pairs on the context and
                        // made every fallback answer nothing (#979).
                        //
                        // Remember where the conversation stood BEFORE the first
                        // nudge so a fallback gets the user's actual request
                        // instead of the scaffolding. Captured once.
                        if pre_nudge_len.is_none() {
                            pre_nudge_len = Some(context.messages.len());
                        }
                        if let Some(stub) =
                            super::helpers::assistant_reasoning_stub(reasoning_text.as_deref())
                        {
                            context.add_message(stub);
                        }
                        context.add_message(Message::user(nudge.to_string()));
                        continue;
                    }

                    // Budget exhausted — walk the fallback chain. Sticky
                    // swap so the next user turn also lands on the new
                    // provider; the original is gone for this session.
                    tracing::warn!(
                        "Empty-reasoning nudge budget exhausted ({}/{}) — walking \
                         fallback chain",
                        empty_reasoning_retries,
                        EMPTY_REASONING_MAX_NUDGES,
                    );
                    let active_name = self.provider_name_for_session(session_id);
                    let chain = self.fallback_chain_snapshot();
                    let candidates: Vec<_> =
                        chain.iter().filter(|p| p.name() != active_name).collect();

                    if candidates.is_empty() {
                        // No escape hatch configured. Surface a visible
                        // alert so the user knows to swap manually — do
                        // NOT exit silently.
                        if let Some(ref cb) = progress_callback {
                            let message = format!(
                                "Model '{}/{}' refused to answer after {} nudges \
                                 and no fallback provider is configured. Use \
                                 /models to switch.",
                                active_name, model_name, EMPTY_REASONING_MAX_NUDGES,
                            );
                            cb(session_id, ProgressEvent::SelfHealingAlert { message });
                        }
                        // Fall through: final_response = Some(response); break
                        // happens below (existing path). The user sees the
                        // alert instead of silence.
                    } else {
                        let mut fb_succeeded = None;
                        'candidates: for fallback in &candidates {
                            let fb_name = fallback.name().to_string();
                            let fb_model = fallback.default_model().to_string();
                            if let Some(ref cb) = progress_callback {
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message: format!(
                                            "Trying fallback '{}/{}' for empty-reasoning \
                                             recovery...",
                                            fb_name, fb_model,
                                        ),
                                    },
                                );
                            }

                            // A fallback is a fresh attempt at what the user asked, not a
                            // continuation of the failed nudge dialogue. Handing it the
                            // scaffolding meant every provider answered a conversation full
                            // of "you did not answer" turns (#979).
                            let mut fb_messages =
                                super::helpers::fallback_messages(pre_nudge_len, &context.messages);
                            // Each fallback gets the same budget the primary had
                            // (#981). One bare shot meant a provider that would
                            // have answered after a single nudge was discarded on
                            // its first silent reply, which is the situation the
                            // rescue exists for.
                            let mut fb_attempt: u32 = 0;
                            loop {
                                fb_attempt += 1;
                                let mut fb_req =
                                    LLMRequest::new(fb_model.clone(), fb_messages.clone())
                                        .with_max_tokens(
                                            self.request_max_tokens_for_session(session_id),
                                        );
                                fb_req.working_directory = Some(
                                    self.get_working_directory_for_session(session_id)
                                        .to_string_lossy()
                                        .to_string(),
                                );
                                fb_req.session_id = Some(session_id);
                                if let Some(system) = &context.system_brain {
                                    fb_req = fb_req.with_system(system.clone());
                                }
                                if self.tool_registry.count() > 0 {
                                    fb_req = fb_req
                                        .with_tools(self.tool_schemas_for_session(session_id));
                                }

                                let original_provider = self.provider_for_session(session_id);
                                self.swap_provider_for_session(
                                    session_id,
                                    (*fallback).clone(),
                                    (*fallback)
                                        .active_subprovider_model()
                                        .unwrap_or_else(|| (*fallback).default_model().to_string()),
                                );
                                let mut restore_guard = FallbackProviderGuard {
                                    service: self,
                                    session_id,
                                    original: Some(original_provider),
                                };
                                let fb_result = self
                                    .stream_complete(
                                        session_id,
                                        fb_req,
                                        cancel_token.as_ref(),
                                        progress_callback.as_ref(),
                                        None,
                                        None,
                                        false,
                                    )
                                    .await;
                                match fb_result {
                                    Ok((fb_resp, _fb_reasoning)) => {
                                        let has_visible_text = fb_resp.content.iter().any(|b| {
                                            matches!(
                                                b,
                                                crate::brain::provider::ContentBlock::Text {
                                                    text,
                                                } if !text.trim().is_empty()
                                            )
                                        });
                                        if has_visible_text {
                                            // Swap sticks for the rest of the session.
                                            restore_guard.original = None;
                                            drop(restore_guard);
                                            if let Some(ref cb) = progress_callback {
                                                let from_name =
                                                    self.provider_name_for_session(session_id);
                                                cb(
                                                    session_id,
                                                    ProgressEvent::SelfHealingAlert {
                                                        message: format!(
                                                            "Empty-reasoning recovery → switched \
                                                         to {}/{}",
                                                            fb_name, fb_model,
                                                        ),
                                                    },
                                                );
                                                cb(
                                                    session_id,
                                                    ProgressEvent::ProviderSwitched {
                                                        from_name,
                                                        from_model: self
                                                            .provider_model_for_session(session_id),
                                                        to_name: fb_name.clone(),
                                                        to_model: fb_model.clone(),
                                                        reason: "empty_reasoning".to_string(),
                                                    },
                                                );
                                            }
                                            self.persist_sticky_pair(
                                                session_id,
                                                fb_name.clone(),
                                                fb_model.clone(),
                                            );
                                            fb_succeeded = Some(fb_resp);
                                            break 'candidates;
                                        } else {
                                            drop(restore_guard);
                                            if fb_attempt < EMPTY_REASONING_MAX_NUDGES {
                                                tracing::warn!(
                                                    "Empty-reasoning fallback '{}' returned empty \
                                                 (attempt {}/{}) — nudging",
                                                    fb_name,
                                                    fb_attempt,
                                                    EMPTY_REASONING_MAX_NUDGES,
                                                );
                                                if let Some(ref cb) = progress_callback {
                                                    cb(
                                                        session_id,
                                                        ProgressEvent::SelfHealingAlert {
                                                            message: format!(
                                                                "Trying fallback '{}/{}' for \
                                                             empty-reasoning recovery... \
                                                             (attempt {}/{})",
                                                                fb_name,
                                                                fb_model,
                                                                fb_attempt + 1,
                                                                EMPTY_REASONING_MAX_NUDGES,
                                                            ),
                                                        },
                                                    );
                                                }
                                                fb_messages.push(Message::user(
                                                    super::helpers::empty_reasoning_nudge(
                                                        false, fb_attempt,
                                                    )
                                                    .to_string(),
                                                ));
                                                continue;
                                            }
                                            tracing::warn!(
                                                "Empty-reasoning fallback '{}' still empty after \
                                             {} attempts — trying next",
                                                fb_name,
                                                fb_attempt,
                                            );
                                            break;
                                        }
                                    }
                                    Err(fb_err) => {
                                        drop(restore_guard);
                                        tracing::warn!(
                                            "Empty-reasoning fallback '{}' failed: {} — \
                                         trying next",
                                            fb_name,
                                            fb_err,
                                        );
                                        break;
                                    }
                                }
                            }
                        }

                        if let Some(fb_resp) = fb_succeeded {
                            // Replace the empty-reasoning response with the
                            // fallback's response and let the outer loop
                            // tail handle final_response + IntermediateText.
                            response = fb_resp;
                            // Re-derive iteration_text from the new response
                            // so the IntermediateText emit below has the
                            // actual reply text.
                            iteration_text = response
                                .content
                                .iter()
                                .filter_map(|b| match b {
                                    crate::brain::provider::ContentBlock::Text { text } => {
                                        Some(text.as_str())
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("");
                        } else if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "All {} fallback providers also returned empty \
                                         reasoning. Use /models to pick a different model.",
                                        candidates.len(),
                                    ),
                                },
                            );
                        }
                    }
                }

                // ── Tool-text leak: corrective retry, then fail clean ───
                // (fork #66, ex-upstream opencrabs/opencrabs#1260) The
                // provider flagged unrecoverable tool-call JSON as text.
                // With tools available, give the model ONE structured-calls
                // nudge; if it leaks again, fail the turn clean — never
                // deliver raw internals as the final answer. Compaction-style
                // requests without tools never reach this loop, and the
                // parse layer already stripped the JSON from content.
                if response.tool_text_leak && self.tool_registry.count() > 0 {
                    if !tool_text_leak_retry_used {
                        tool_text_leak_retry_used = true;
                        tracing::warn!(
                            "Tool-call JSON leaked as text — one corrective retry with \
                             structured-calls nudge (iteration {})",
                            iteration,
                        );
                        self.record_provider_feedback(
                            session_id,
                            "tool_text_leak_retry",
                            "provider-integrity",
                            Some("model returned tool requests as text; corrective retry"),
                        );
                        context.add_message(Message::user(
                            "[System: Your previous response contained tool-call JSON as plain \
                             text instead of executable calls. You have access to tools — invoke \
                             them ONLY through the structured function-call API, never as text \
                             or JSON in your reply. Re-issue your response using real tool_use \
                             calls; if no tool is needed, answer in plain prose without any \
                             JSON.]"
                                .to_string(),
                        ));
                        continue;
                    }
                    let msg = "Model returned tool requests as text and recovery failed \
                               after one retry — try a model with native function calling \
                               (e.g. Claude, GPT-4, Gemini)."
                        .to_string();
                    tracing::warn!("Tool-text leak retry exhausted — failing clean: {}", msg);
                    self.record_provider_feedback(
                        session_id,
                        "tool_text_leak_fail",
                        "provider-integrity",
                        Some("leak persisted after corrective retry; failing clean"),
                    );
                    return Err(AgentError::Provider(
                        crate::brain::provider::ProviderError::StreamError(msg),
                    ));
                }

                // ── Mid-sentence truncation retry ────────────────────────
                // Local reasoning models sometimes hit an internal EOS mid-
                // sentence. The response stream closes cleanly (finish_reason
                // =stop + usage chunk), but the visible text ends in the
                // middle of a word or clause: "Standard Get I", "Changelog
                // automation, duplicate CI fix, 1,890", etc. Detect the
                // truncation by looking at the last non-whitespace character
                // — if it's not a terminal token (punctuation, close-tag,
                // table pipe, code fence) we ask the model to continue once.
                if truncation_continue_attempts < super::truncation::MAX_CONTINUATION_ATTEMPTS
                    && iteration > 0
                    && !is_cli_provider
                    && matches!(
                        response.stop_reason,
                        Some(crate::brain::provider::StopReason::EndTurn)
                    )
                    && super::truncation::try_emit_truncation_continue(
                        &iteration_text,
                        reasoning_text.as_ref(),
                        &response.usage,
                        &mut context,
                        session_id,
                        truncation_continue_attempts + 1,
                        &progress_callback,
                    )
                {
                    // Keep the partial: final_text is built from the LAST
                    // response only, so without this the continuation replaces
                    // the answer instead of extending it (#859).
                    truncation_partial = Some(iteration_text.clone());
                    truncation_continue_attempts += 1;
                    // Mark the next iteration so the stream-error path skips
                    // cross-provider fallback for the continuation request.
                    current_iter_is_truncation_continue = true;
                    self.record_provider_feedback(
                        session_id,
                        "truncation_retry",
                        "stream-integrity",
                        Some(&format!(
                            "mid-sentence cut (tail {:?}); corrective continuation attempted (#36)",
                            iteration_text
                                .chars()
                                .rev()
                                .take(60)
                                .collect::<String>()
                                .chars()
                                .rev()
                                .collect::<String>()
                        )),
                    );
                    continue;
                }

                // Mermaid regen nudge (#37): if any fence in the reply fails
                // ── Degenerate continuation retry (#1737) ─────────────────
                // The previous iteration WAS a truncation continuation and
                // its result added nothing: join_continuation classifies it
                // as Echoed (returned only the tail, or just closed dangling
                // markup — the 2026-09-25 qwen case answered an unclosed code
                // span with a single backtick, 774 tokens billed). One
                // anchored retry, bounded by MAX_CONTINUATION_ATTEMPTS; a
                // second degenerate result falls through to the normal close
                // and the post-loop join appends the incomplete marker,
                // unchanged.
                if iter_is_truncation_continue
                    && truncation_continue_attempts < super::truncation::MAX_CONTINUATION_ATTEMPTS
                    && let Some(partial) = truncation_partial.as_deref()
                    && matches!(
                        super::truncation::join_continuation(partial, &iteration_text),
                        super::truncation::Continuation::Echoed(_)
                    )
                {
                    truncation_continue_attempts += 1;
                    let attempt = truncation_continue_attempts;
                    tracing::warn!(
                        "[TRUNCATION] verdict=retry-again attempt={}/{}: {} char continuation \
                         added nothing to the {} char partial (echoed tail / closed markup \
                         only) — retrying with anchored prompt",
                        attempt,
                        super::truncation::MAX_CONTINUATION_ATTEMPTS,
                        iteration_text.trim().chars().count(),
                        partial.trim_end().chars().count(),
                    );
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::SelfHealingAlert {
                                message: CONTINUATION_EMPTY_ALERT.into(),
                            },
                        );
                    }
                    // The degenerate response itself stays in context (the
                    // model must see what it already returned and had
                    // rejected), then the anchored nudge replaces the vague
                    // original.
                    context.add_message(Message::assistant(iteration_text.clone()));
                    context.add_message(Message::user(super::truncation::continuation_nudge(
                        partial,
                        Some(&iteration_text),
                    )));
                    current_iter_is_truncation_continue = true;
                    continue;
                }

                // to parse DETERMINISTICALLY, hand the model the renderer's
                // own error text before the reply goes final — same shape as
                // the empty-answer ladder: echo the broken text as an
                // assistant message, inject the correction as a user-role
                // [System: ...] nudge, re-run the iteration. Transient
                // renderer failures stay silent here (preflight reports parse
                // errors only) and keep the delivery path's degrade-to-block
                // behaviour. Gated to channel sessions — the CLI has no
                // mermaid delivery, so there is nothing to regenerate.
                #[cfg(feature = "telegram")]
                if mermaid_regen_retries < MERMAID_REGEN_MAX_NUDGES
                    && !is_cli_provider
                    && progress_callback.is_some()
                    && crate::channels::telegram::rich::mermaid::should_render_mermaid(
                        &iteration_text,
                    )
                {
                    let parse_errors =
                        crate::channels::telegram::rich::mermaid::preflight_parse_errors(
                            &iteration_text,
                        )
                        .await;
                    if !parse_errors.is_empty() {
                        mermaid_regen_retries += 1;
                        let attempt = mermaid_regen_retries;
                        tracing::warn!(
                            fences_broken = parse_errors.len(),
                            attempt,
                            budget = MERMAID_REGEN_MAX_NUDGES,
                            "mermaid preflight parse errors; nudging regen"
                        );
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "Mermaid render failed — regen \
                                         {attempt}/{MERMAID_REGEN_MAX_NUDGES}"
                                    ),
                                },
                            );
                        }
                        let nudge = crate::brain::agent::service::nudge::mermaid_regen_nudge(
                            &parse_errors,
                            attempt,
                            MERMAID_REGEN_MAX_NUDGES,
                        );
                        context.add_message(Message::assistant(iteration_text));
                        context.add_message(Message::user(nudge));
                        continue;
                    }
                }

                if iteration > 0 {
                    // Empty-analysis nudge: the model ran successful
                    // tool calls but produced ZERO text on the final
                    // iteration. For side-effect tasks (commit / push /
                    // edit) this is the intended outcome of the
                    // FINISHING A TURN directive — the tool result IS
                    // the deliverable. For analysis tasks ("audit X",
                    // "compare A and B", "what does Y do") the fetched
                    // data was meant to be INPUT to a text answer, and
                    // empty text means the user got nothing. One-shot
                    // nudge wakes the model up. If even after the
                    // nudge it still emits nothing, fall through and
                    // let the empty-text close stand — better than
                    // looping. Detection uses the clean user message
                    // when a channel handler supplied one (the
                    // `[Channel: ...]` prefix would otherwise pin
                    // every match to the wrapper, not the body).
                    // Reaction-replaced-completion nudge (#439): the final
                    // text is ONLY a <<react:emoji>> directive while tool
                    // calls ran this turn. The reaction survives; the model
                    // is asked once to also write the completion it owes.
                    // Unlike the analysis nudge below this is intent-blind:
                    // ANY work turn must report what it did.
                    if !reaction_only_nudge_used
                        && tool_calls_completed_this_turn > 0
                        && !iteration_text.trim().is_empty()
                        && {
                            let (rest, emoji) = crate::utils::extract_react_marker(&iteration_text);
                            emoji.is_some() && rest.trim().is_empty()
                        }
                    {
                        reaction_only_nudge_used = true;
                        tracing::warn!(
                            target: "reaction_only_close",
                            tools_completed = tool_calls_completed_this_turn,
                            iteration,
                            "work turn closed with a bare react directive — nudging once \
                             for the completion summary (#439)"
                        );
                        self.record_provider_feedback(
                            session_id,
                            "reaction_only_close",
                            "self_heal",
                            Some(&format!(
                                "iteration={iteration}, tools_completed={tool_calls_completed_this_turn}"
                            )),
                        );
                        context.add_message(Message::user(
                            "[System: You executed tool calls this turn but your final reply \
                             was ONLY a reaction directive. A reaction never replaces the \
                             completion for work you performed. Keep the reaction, and now \
                             write the short completion message reporting what you did \
                             (what ran, what changed, results). Do NOT run more tool calls.]"
                                .to_string(),
                        ));
                        continue;
                    }

                    let user_text_for_intent =
                        display_text_override.as_deref().unwrap_or(&user_message);
                    if !analysis_nudge_used
                        && iteration_text.trim().is_empty()
                        && tool_calls_completed_this_turn > 0
                        && super::phantom::is_analysis_intent(user_text_for_intent)
                    {
                        analysis_nudge_used = true;
                        tracing::warn!(
                            target: "analysis_empty_close",
                            tools_completed = tool_calls_completed_this_turn,
                            iteration,
                            "model ended turn with zero text after analysis-intent request — \
                             nudging once to produce the answer the user asked for"
                        );
                        self.record_provider_feedback(
                            session_id,
                            "analysis_empty_close",
                            "self_heal",
                            Some(&format!(
                                "iteration={iteration}, tools_completed={tool_calls_completed_this_turn}"
                            )),
                        );
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: EMPTY_ANSWER_ANALYSIS_ALERT.to_string(),
                                },
                            );
                        }
                        context.add_message(Message::user(
                            "[System: You fetched data via tool calls but ended the turn with NO \
                             text response. The user's request was an analysis task (audit / \
                             review / explain / compare / summarise) where the tool result is \
                             INPUT to your answer, not the answer itself. Write the actual \
                             analysis now — cite specific fields, line numbers, or values from \
                             what you fetched. Do NOT run more tool calls; you already have the \
                             data. Do NOT reply with 'Done.' or 'Got it.' — those are for \
                             side-effect tasks, this is data interpretation. End once the \
                             analysis is written.]"
                                .to_string(),
                        ));
                        continue;
                    }

                    tracing::info!("Agent completed after {} tool iterations", iteration);
                    // Emit final text so TUI persists it as a permanent message.
                    // CLI providers: helpers.rs already flushed cli_unflushed_text
                    // as IntermediateText at stream end — skip to avoid duplication.
                    if !is_cli_provider
                        && !iteration_text.is_empty()
                        && let Some(ref cb) = progress_callback
                    {
                        cb(
                            session_id,
                            ProgressEvent::IntermediateText {
                                text: iteration_text,
                                reasoning: reasoning_text,
                            },
                        );
                    }
                } else {
                    tracing::info!("Agent responded with text only (no tool calls)");
                }
                // #458's turn-close flush was removed by #1172: blocked phantom
                // iterations now persist immediately under a phantom_blocked=1
                // flag at detection time, so there is nothing left to withhold
                // and nothing to flush here.

                final_response = Some(response);

                // --- GOAL POST-TURN HOOK ---
                // If a goal is active for this session, evaluate whether
                // the goal is satisfied by the last response. If the
                // judge says CONTINUE and the turn budget has room,
                // inject a continuation prompt and re-enter the loop.
                {
                    use crate::brain::goal::GoalManager;
                    let goal_mgr = GoalManager::new(self.context.clone());
                    match goal_mgr
                        .evaluate_after_turn(
                            self.provider_for_session(session_id).as_ref(),
                            &model_name,
                            session_id,
                            &accumulated_text,
                        )
                        .await
                    {
                        crate::brain::goal::GoalDecision::Continue {
                            continuation_prompt,
                            ..
                        } => {
                            tracing::info!("Goal not yet satisfied, injecting continuation prompt");
                            if let Some(ref cb) = progress_callback {
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message: "Goal continues, re-entering tool loop"
                                            .to_string(),
                                    },
                                );
                            }
                            context.add_message(Message::user(continuation_prompt));
                            final_response = None;
                            continue;
                        }
                        crate::brain::goal::GoalDecision::Done { ref reason } => {
                            tracing::info!("Goal satisfied: {}", reason);
                        }
                        crate::brain::goal::GoalDecision::Paused { ref reason } => {
                            tracing::warn!("Goal paused: {}", reason);
                            if let Some(ref cb) = progress_callback {
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message: format!("⚠️ {reason}"),
                                    },
                                );
                            }
                        }
                    }
                }
                break;
            }

            // ── Mixed-iteration fact check (#1693) ──────────────────────────
            // The 1,341-line block above runs only when `tool_uses` is empty, so
            // one real call bought immunity for whatever sentence rode in with
            // it: at 19:14:10 on 2026-09-22 a response asserted `Gate died on
            // 2× E0603 "this item is private"` while the same response's tool
            // call was LAUNCHING that gate. Nothing checked the claim, and the
            // clippy log it refers to had exited 0.
            //
            // These three checks are content-vs-evidence, so they do not need
            // the zero-tool precondition. The shape-based tells stay inside the
            // block above on purpose — after a call, "Running fmt, then clippy"
            // is a legitimate recap (#1506, #1172).
            //
            // Carried, not acted on: the calls are real work and still run, so
            // this cannot discard the iteration the way the phantom path above
            // does. It lands as a correction after the results, the same shape
            // as the repeat verdict (#1030).
            let mixed_fact_verdict =
                if is_cli_provider || tool_uses.is_empty() || iteration_text.trim().is_empty() {
                    None
                } else {
                    // This iteration's own calls count as executed for the command
                    // check, or "Running `gh pr list`" plus a `gh pr list` call
                    // would flag the work it is about to do. The OUTPUTS stay
                    // prior-only: an in-flight call has produced nothing and so
                    // cannot vouch for a past-tense claim.
                    let mut executed = turn_tool_input.clone();
                    executed.extend(tool_uses.iter().map(|(_, _, input)| input.to_string()));
                    let evidence = Self::conversation_evidence(&context, &turn_tool_output);
                    super::phantom::mixed_iteration_facts(
                        &iteration_text,
                        &executed,
                        &turn_tool_output,
                        &evidence,
                    )
                };

            // Emit intermediate text to TUI so it appears before the tool calls.
            //
            // Also emit when the iteration produced ONLY reasoning (no visible
            // text) but is about to execute tool calls. Without this, a local
            // reasoning model like MLX Qwen that emits
            // `reasoning_content` + structured `tool_calls` never persists its
            // per-iteration thinking — everything accumulates in
            // `streaming_reasoning` until the FINAL turn bundles all four
            // iterations' thoughts into one giant Thinking block at the
            // bottom of the chat (screenshot 2026-04-17 04:17). Firing per
            // iteration splits the thinking into its proper chronological
            // slots: iter-1-thinking → tools → iter-2-thinking → tools …
            let has_reasoning_to_persist = reasoning_text
                .as_deref()
                .map(|r| !r.trim().is_empty())
                .unwrap_or(false);
            if (!iteration_text.is_empty() || has_reasoning_to_persist)
                && let Some(ref cb) = progress_callback
            {
                // Kimi Code endpoint streams reasoning inline as content with no
                // reasoning_content field (#616). This is PRE-tool text (the
                // turn continues), so on that endpoint it is reasoning, not a
                // chat message — route it to the reasoning channel instead of
                // relaying it as standalone Telegram walls. The final answer is
                // emitted on turn completion elsewhere and is untouched.
                let reasoning_inline =
                    crate::brain::provider::kimi_reasoning::streams_reasoning_inline(
                        self.provider_for_session(session_id).base_url(),
                    );
                let event = if reasoning_inline && !iteration_text.is_empty() {
                    let combined = match reasoning_text.as_deref() {
                        Some(r) if !r.trim().is_empty() => {
                            format!("{r}\n{iteration_text}")
                        }
                        _ => iteration_text.clone(),
                    };
                    ProgressEvent::IntermediateText {
                        text: String::new(),
                        reasoning: Some(combined),
                    }
                } else {
                    ProgressEvent::IntermediateText {
                        text: iteration_text.clone(),
                        // Clone: reasoning_text is still needed downstream to
                        // seed the assistant message's ContentBlock::Thinking
                        // so follow-up turns (notably kimi/Moonshot) have the
                        // required `reasoning_content` to echo back.
                        reasoning: reasoning_text.clone(),
                    }
                };
                cb(session_id, event);
            }

            // Detect tool loops: hash the full input for every tool.
            // Different arguments = different hash = no false loop detection.
            let current_call_signature = tool_uses
                .iter()
                .map(|(_, name, input)| {
                    let input_str = serde_json::to_string(input).unwrap_or_default();
                    let hash: u64 = input_str
                        .bytes()
                        .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u64));
                    format!("{}:{:x}", name, hash)
                })
                .collect::<Vec<_>>()
                .join(",");

            recent_tool_calls.push(current_call_signature.clone());

            // Keep last 50 iterations for loop detection.
            // Modern agents legitimately make dozens of tool calls with different args.
            // Signatures include arguments, so only truly identical calls match.
            if recent_tool_calls.len() > 50 {
                recent_tool_calls.remove(0);
            }

            // Check for repeated patterns with tool-specific thresholds.
            // Only triggers for truly identical calls (same tool + same arguments).

            let is_modification_tool = current_call_signature.starts_with("write:")
                || current_call_signature.starts_with("edit:")
                || current_call_signature.starts_with("bash:");

            // Modification tools are dangerous to loop (a bad write/edit/bash
            // must not repeat), so they keep the strict strictly-consecutive
            // hard-break with no nudge: 4 identical calls in a row and we stop.
            const MOD_CONSECUTIVE_BREAK: usize = 4;
            if is_modification_tool && recent_tool_calls.len() >= MOD_CONSECUTIVE_BREAK {
                let last_n = &recent_tool_calls[recent_tool_calls.len() - MOD_CONSECUTIVE_BREAK..];
                if last_n.iter().all(|call| call == &current_call_signature) {
                    // Hand the turn to the next provider instead of ending
                    // it (#1397): the pending call is named so the log and
                    // the user can see what did not run.
                    let pending = super::loop_break::describe_pending_calls(&tool_uses);
                    tracing::warn!(
                        "⚠️ Modification tool loop: '{}' repeated {} times with identical \
                         arguments, dropping the pending call and rotating the chain: {}",
                        current_call_signature,
                        MOD_CONSECUTIVE_BREAK,
                        pending,
                    );
                    return Err(super::loop_break::loop_break_error(
                        "modification-tool loop",
                        current_call_signature.split(':').next().unwrap_or("tool"),
                        MOD_CONSECUTIVE_BREAK,
                        MOD_CONSECUTIVE_BREAK,
                        &pending,
                    ));
                }
            }

            // Near-match loop detection (#957, generalized in #961): the
            // exact-match guards above only fire when the SAME name+args
            // repeat. Loops that re-issue near-identical calls differing
            // only in a counter, incrementing number, or whitespace slip
            // past every exact guard. That is exactly the tool_search
            // re-activation loop DeepSeek v4 flash falls into after a
            // rate-limit fallback (#961). Normalize every call's name+args,
            // count how many recent calls collide with the current one;
            // nudge once, then break if the model keeps re-issuing
            // near-duplicates. Every tool participates: numeric argument
            // values (read_file start_line, plan task_order, timeouts) are
            // preserved exactly by the signature (#82), so chunked reads
            // and checklist progression no longer collide — the exclusion
            // read_file once needed is gone, and its loops are guarded
            // again.
            {
                const NEAR_WINDOW: usize = 8;
                const NEAR_NUDGE_AT: usize = 3;
                const NEAR_BREAK_AT: usize = 4;
                let normalized_call = tool_uses
                    .iter()
                    .map(|(_, name, input)| {
                        crate::brain::agent::service::helpers::normalized_call_signature(
                            name.as_str(),
                            input,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                if !normalized_call.is_empty() {
                    recent_normalized_calls.push(normalized_call.clone());
                    if recent_normalized_calls.len() > 50 {
                        recent_normalized_calls.remove(0);
                    }
                    let near_in_window = {
                        let start = recent_normalized_calls.len().saturating_sub(NEAR_WINDOW);
                        recent_normalized_calls[start..]
                            .iter()
                            .filter(|c| *c == &normalized_call)
                            .count()
                    };
                    match repeat_loop_action(
                        &recent_normalized_calls,
                        &normalized_call,
                        NEAR_WINDOW,
                        NEAR_NUDGE_AT,
                        NEAR_BREAK_AT,
                        near_match_nudged,
                    ) {
                        RepeatLoopAction::Break => {
                            // Not the end of the turn (#1397): the raw pending
                            // call goes in the log and the error, and the
                            // rotation wrapper replays the turn on the next
                            // provider. Only an exhausted chain reaches the user.
                            let pending = super::loop_break::describe_pending_calls(&tool_uses);
                            tracing::warn!(
                                "⚠️ Near-identical tool-call loop persisted after nudge: '{}' \
                                 recurred {}x in last {} iterations, dropping the pending call \
                                 and rotating the chain: {}",
                                normalized_call,
                                near_in_window,
                                NEAR_WINDOW,
                                pending,
                            );
                            return Err(super::loop_break::loop_break_error(
                                "near-identical tool-call loop",
                                normalized_call.split(':').next().unwrap_or("tool"),
                                near_in_window,
                                NEAR_WINDOW,
                                &pending,
                            ));
                        }
                        RepeatLoopAction::Nudge => {
                            tracing::warn!(
                                "Near-identical tool-call loop: '{}' recurred {}x in last {} \
                                 iterations, nudging agent.",
                                normalized_call,
                                near_in_window,
                                NEAR_WINDOW,
                            );
                            if let Some(ref cb) = progress_callback {
                                cb(
                                    session_id,
                                    ProgressEvent::SelfHealingAlert {
                                        message: format!(
                                            "Stuck re-issuing near-identical tool calls ({}x), \
                                             nudging the agent to stop repeating and act",
                                            near_in_window,
                                        ),
                                    },
                                );
                            }
                            context.add_message(Message::user(format!(
                                "[System: You have issued nearly identical tool calls {} times \
                                 in the last {} steps. They differ only in numbers, counters, \
                                 or whitespace, so they return the same kind of result. \
                                 Repeating near-duplicate calls will not move you forward. {}]",
                                near_in_window,
                                NEAR_WINDOW,
                                crate::brain::agent::service::nudge::variation_directive(),
                            )));
                            near_match_nudged = true;
                            continue;
                        }
                        RepeatLoopAction::Continue => {}
                    }
                }
            }

            // Non-modification tools: an identical call (same name+args) that
            // DOMINATES the recent window is a stuck loop even when interleaved
            // with a few other calls — the strictly-consecutive check above
            // missed that (e.g. a model that re-issues the same grep every
            // other round). Nudge once (consistent with the browser-loop
            // nudge, so a stuck read/grep/list loop is never cut silently),
            // then break if the model ignores the nudge and keeps repeating.
            if !is_modification_tool {
                const REPEAT_WINDOW: usize = 8;
                // Nudge/break earlier so a single turn poisons the history with
                // far fewer identical rounds before the loop is broken (#740).
                const REPEAT_NUDGE_AT: usize = 3;
                const REPEAT_BREAK_AT: usize = 4;
                // Label the loop by the first tool name in the signature
                // ("grep:ab,read:cd" → "grep") for the user-facing message.
                let tool_label = current_call_signature
                    .split(':')
                    .next()
                    .unwrap_or("tool")
                    .to_string();
                let repeat_in_window = {
                    let start = recent_tool_calls.len().saturating_sub(REPEAT_WINDOW);
                    recent_tool_calls[start..]
                        .iter()
                        .filter(|c| *c == &current_call_signature)
                        .count()
                };

                match repeat_loop_action(
                    &recent_tool_calls,
                    &current_call_signature,
                    REPEAT_WINDOW,
                    REPEAT_NUDGE_AT,
                    REPEAT_BREAK_AT,
                    identical_call_loop_nudged,
                ) {
                    RepeatLoopAction::Break => {
                        // Same shape as the near-match break (#1397): name the
                        // pending call, rotate the chain, end the turn only
                        // when no provider is left.
                        let pending = super::loop_break::describe_pending_calls(&tool_uses);
                        tracing::warn!(
                            "⚠️ Identical-call loop persisted after nudge: '{}' x{} in last {} \
                             iterations, dropping the pending call and rotating the chain: {}",
                            current_call_signature,
                            repeat_in_window,
                            REPEAT_WINDOW,
                            pending,
                        );
                        return Err(super::loop_break::loop_break_error(
                            "identical-call loop",
                            &tool_label,
                            repeat_in_window,
                            REPEAT_WINDOW,
                            &pending,
                        ));
                    }
                    RepeatLoopAction::Nudge => {
                        tracing::warn!(
                            "Identical-call loop: '{}' x{} in last {} iterations — nudging agent.",
                            current_call_signature,
                            repeat_in_window,
                            REPEAT_WINDOW,
                        );
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: format!(
                                        "Stuck repeating `{}` ({}x) — nudging the agent to use \
                                         the results it already has",
                                        tool_label, repeat_in_window,
                                    ),
                                },
                            );
                        }
                        context.add_message(Message::user(format!(
                            "[System: You have called `{}` with identical arguments {} times in \
                             the last {} steps and it keeps returning the same result. Repeating \
                             the same call will not produce anything new. Use the results you \
                             already have to answer the user, or take a DIFFERENT action. Do not \
                             issue this identical call again.]",
                            tool_label, repeat_in_window, REPEAT_WINDOW,
                        )));
                        identical_call_loop_nudged = true;
                        continue;
                    }
                    RepeatLoopAction::Continue => {}
                }
            }

            // Semantic-loop detection for browser navigation cycles.
            //
            // Exact-loop detection above only fires when the SAME tool +
            // SAME args repeat. The browser navigate→wait→screenshot
            // rotation uses three different tools with varying args, so
            // it slips through every check despite producing zero progress.
            // 2026-05-23 09:04 logs show 32+ iterations of this rotation
            // before the user gave up.
            //
            // Heuristic: if the last 8 iterations contain `browser_screenshot`
            // 4+ times AND zero progress-making interactions (click/type),
            // inject a nudge once per turn telling the agent to interact
            // instead of screenshot again.
            if !browser_screenshot_loop_nudged && recent_tool_calls.len() >= 8 {
                let last8 = &recent_tool_calls[recent_tool_calls.len() - 8..];
                let screenshot_count = last8
                    .iter()
                    .filter(|sig| sig.starts_with("browser_screenshot:"))
                    .count();
                let progress_count = last8
                    .iter()
                    .filter(|sig| {
                        sig.starts_with("browser_click:")
                            || sig.starts_with("browser_type:")
                            || sig.starts_with("browser_eval:")
                    })
                    .count();
                if screenshot_count >= 4 && progress_count == 0 {
                    tracing::warn!(
                        "Browser semantic loop: {} screenshots in last 8 iterations, \
                         0 interactions — injecting nudge.",
                        screenshot_count,
                    );
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::SelfHealingAlert {
                                message: format!(
                                    "Stuck in screenshot loop ({}/8 iterations) — \
                                     nudging agent to interact with the page",
                                    screenshot_count,
                                ),
                            },
                        );
                    }
                    context.add_message(Message::user(
                        "[System: You have taken multiple screenshots of the same page \
                         without interacting. The screenshots already show what's on \
                         screen — STOP screenshotting. To move forward you must either: \
                         (1) `browser_click` an element (use `text=Label`, `xpath=...`, \
                         or a CSS selector), (2) `browser_type` into an input field, or \
                         (3) `browser_navigate` to a new URL. If you can't find the \
                         element you need, call `browser_find` with mode=\"text\" or \
                         mode=\"aria\" to enumerate candidates and get back stable \
                         `[data-opencrabs-match=\"N\"]` selectors. Do not screenshot \
                         again on this turn.]"
                            .to_string(),
                    ));
                    browser_screenshot_loop_nudged = true;
                    continue;
                }
            }

            // Identity of this whole round, so parallel batches compare as a
            // unit rather than per call (#1030).
            let round_signature = tool_uses
                .iter()
                .map(|(_, name, input)| super::tool_repeat::signature(name, input))
                .collect::<Vec<_>>()
                .join("\n");
            let repeat_verdict = super::tool_repeat::observe_round(
                &mut tool_repeat,
                &round_signature,
                tool_uses.first().map(|(_, name, _)| name.clone()),
            );

            // Execute tools and build response message
            let mut tool_results = Vec::new();
            let mut tool_descriptions: Vec<String> = Vec::new(); // For DB persistence
            let mut tool_outputs: Vec<(bool, String)> = Vec::new(); // (success, output) parallel to descriptions

            // ── Concurrent fast path (#361) ──
            // A multi-tool batch where nothing needs interactive approval
            // runs concurrently, capped by [agent] max_concurrent. Results
            // come back in original order; the sequential loop below then
            // sees an empty batch. Approval-gated or single-tool batches
            // fall through unchanged.
            let tool_uses = if self.batch_is_parallel_eligible(
                &tool_uses,
                &tool_context,
                has_override_approval,
            ) {
                let search_call_mask: Vec<bool> = tool_uses
                    .iter()
                    .map(|(_, name, _)| crate::brain::tools::sources_footer::is_search_tool(name))
                    .collect();
                let batch = self
                    .execute_tools_parallel(
                        session_id,
                        tool_uses,
                        &tool_context,
                        cancel_token.as_ref(),
                        progress_callback.as_ref(),
                        assistant_db_msg.id,
                    )
                    .await;
                if batch.successes > 0 {
                    phantom_retries_used = 0;
                    tool_calls_completed_this_turn += batch.successes;
                    turn_tool_output.extend(batch.outputs.iter().map(|(_, out)| out.clone()));
                    // Deterministic footer feed (#1883): batch outputs come
                    // back in tool_uses order, so the positional mask names
                    // the search calls in the same order the engines ran.
                    for (is_search, (ok, out)) in search_call_mask.iter().zip(batch.outputs.iter())
                    {
                        if *is_search && *ok {
                            turn_search_outputs.push(out.clone());
                        }
                    }
                }
                tool_results = batch.results;
                tool_descriptions = batch.descriptions;
                tool_outputs = batch.outputs;
                if batch.cancelled {
                    tracing::warn!("🛑 Tool execution cancelled mid-batch (parallel path)");
                }
                Vec::new()
            } else {
                tool_uses
            };

            for (tool_id, tool_name, tool_input) in tool_uses {
                // Check for cancellation before each tool
                if let Some(ref token) = cancel_token
                    && token.is_cancelled()
                {
                    tracing::warn!(
                        "🛑 Tool execution cancelled before '{}' at iteration {}",
                        tool_name,
                        iteration,
                    );
                    break;
                }

                tracing::info!("Executing tool '{}' (iteration {})", tool_name, iteration,);

                // Save tool input for progress reporting (before it's moved to execute)
                let tool_input_for_progress = tool_input.clone();
                turn_tool_input.push(tool_input.to_string());

                // Build short description for DB persistence
                tool_descriptions.push(Self::format_tool_summary(&tool_name, &tool_input));

                // Emit tool started progress
                if let Some(ref cb) = progress_callback {
                    cb(
                        session_id,
                        ProgressEvent::ToolStarted {
                            tool_name: tool_name.clone(),
                            tool_input: tool_input_for_progress.clone(),
                            tool_call_id: tool_id.clone(),
                        },
                    );
                }

                // Check if approval is needed.
                // Each channel's make_approval_callback() already checks
                // check_approval_policy() from config — the tool loop only
                // respects the auto_approve_tools flag and tool-level policy.
                let needs_approval = if let Some(tool) = self.tool_registry.get(&tool_name) {
                    tool.requires_approval_for_input(&tool_input)
                        && (!self.auto_approve_tools || has_override_approval)
                        && !tool_context.auto_approve
                } else {
                    false
                };

                // Request approval if needed
                if needs_approval {
                    if let Some(ref approval_cb) = approval_callback {
                        // Get tool details for approval request
                        let tool_info = if let Some(tool) = self.tool_registry.get(&tool_name) {
                            ToolApprovalInfo {
                                session_id,
                                tool_name: tool_name.clone(),
                                tool_description: tool.description().to_string(),
                                tool_input: tool_input.clone(),
                                capabilities: tool
                                    .capabilities()
                                    .iter()
                                    .map(|c| format!("{:?}", c))
                                    .collect(),
                            }
                        } else {
                            // Tool not found, skip approval. Inject brain-file
                            // guidance so the model learns the canonical name or
                            // routing inline instead of guessing again (#767).
                            let err = format!("Tool not found: {}", tool_name);
                            tool_outputs.push((false, err.clone()));
                            let mut content = err;
                            if let Some(hints) = crate::brain::hints::hints_for(&format!(
                                "{tool_name} tool not found"
                            ))
                            .await
                            {
                                content.push_str(&hints);
                            }
                            tool_results.push(ContentBlock::ToolResult {
                                tool_use_id: tool_id,
                                content,
                                is_error: Some(true),
                            });
                            continue;
                        };

                        // Call approval callback
                        tracing::info!("Requesting user approval for tool '{}'", tool_name);
                        match approval_cb(tool_info).await {
                            Ok((approved, always_approve)) => {
                                if !approved {
                                    tracing::warn!("User denied approval for tool '{}'", tool_name);
                                    self.record_tool_feedback(
                                        session_id,
                                        &tool_name,
                                        Some(&tool_input_for_progress),
                                        false,
                                        Some("user_denied_approval"),
                                    );
                                    tool_outputs
                                        .push((false, "User denied permission".to_string()));
                                    tool_results.push(ContentBlock::ToolResult {
                                        tool_use_id: tool_id,
                                        content: "User denied permission to execute this tool"
                                            .to_string(),
                                        is_error: Some(true),
                                    });
                                    continue;
                                }
                                // Propagate "always approve" to skip callbacks for remaining tools
                                if always_approve {
                                    tool_context.auto_approve = true;
                                    tracing::info!(
                                        "User selected 'Always' — auto-approving remaining tools in this loop"
                                    );
                                }
                                tracing::info!("User approved tool '{}'", tool_name);
                                // Create approved context for this tool execution
                                let mut approved_tool_context = tool_context.clone();
                                approved_tool_context.auto_approve = true; // User approved this execution

                                // Execute the tool with approved context, racing against cancel
                                // #1178 M1: set inside the Ok arm below when the tool ends the turn
                                let mut halt_turn_requested = false;
                                let exec_result = tokio::select! {
                                    biased;
                                    _ = async {
                                        if let Some(ref t) = cancel_token { t.cancelled().await } else { std::future::pending().await }
                                    } => {
                                        tracing::warn!("🛑 Tool '{}' cancelled mid-execution", tool_name);
                                        break;

                                    }
                                    r = self.tool_registry.execute(&tool_name, tool_input, &approved_tool_context) => r,
                                };
                                match exec_result {
                                    Ok(result) => {
                                        // Halt policy lives on the tool via
                                        // Tool::halts_turn, consulted through
                                        // the registry; only a SUCCESSFUL run
                                        // ends the turn (a failed options call
                                        // must not kill it) — audit fix.
                                        if self.tool_registry.halts_turn(&tool_name)
                                            && result.success
                                        {
                                            halt_turn_requested = true;
                                        }
                                        let success = result.success;
                                        let images = result.images;
                                        let content = build_tool_result_content(
                                            result.success,
                                            result.error,
                                            &result.output,
                                        );

                                        // GRANULAR LOG: Tool execution result
                                        if success {
                                            tracing::info!(
                                                "[TOOL_EXEC] ✅ Tool '{}' executed successfully, output_len={}",
                                                tool_name,
                                                content.len()
                                            );
                                            // Mirror of the non-approval branch:
                                            // a successful tool run wipes the
                                            // phantom retry counter so a later
                                            // isolated phantom burst is judged
                                            // on its own merits, not on debt
                                            // accumulated earlier in the turn.
                                            phantom_retries_used = 0;
                                            tool_calls_completed_this_turn += 1;
                                            // Deterministic footer feed
                                            // (#1883): a search call that was
                                            // approval-gated still carries its
                                            // links. On success `content` is
                                            // the raw tool output verbatim.
                                            if crate::brain::tools::sources_footer::is_search_tool(
                                                &tool_name,
                                            ) {
                                                turn_search_outputs.push(content.clone());
                                            }
                                            // Persist the touched path so a later
                                            // session on this project can re-anchor
                                            // on real paths instead of guessing.
                                            if let Some(p) = extract_path_for_recent_buffer(
                                                &tool_name,
                                                &tool_input_for_progress,
                                                &approved_tool_context.working_directory,
                                            ) {
                                                self.record_recent_path(
                                                    &approved_tool_context.working_directory,
                                                    &p,
                                                );
                                            }
                                        } else {
                                            tracing::error!(
                                                "[TOOL_EXEC] ❌ Tool '{}' failed: {}",
                                                tool_name,
                                                content.chars().take(200).collect::<String>()
                                            );
                                        }

                                        // Auto-record to feedback ledger (fire-and-forget)
                                        self.record_tool_feedback(
                                            session_id,
                                            &tool_name,
                                            Some(&tool_input_for_progress),
                                            success,
                                            if success { None } else { Some(&content) },
                                        );

                                        // Record tool execution for usage dashboard
                                        if let Some(pool) = crate::db::global_pool() {
                                            let tool_repo =
                                                crate::db::repository::ToolExecutionRepository::new(
                                                    pool.clone(),
                                                );
                                            let exec_id = uuid::Uuid::new_v4().to_string();
                                            let mid = assistant_db_msg.id.to_string();
                                            let sid = session_id.to_string();
                                            let tname = tool_name.clone();
                                            let status = if success { "success" } else { "error" };
                                            let prov = self.provider_name_for_session(session_id);
                                            let mdl =
                                                Some(self.provider_model_for_session(session_id));
                                            tokio::spawn(async move {
                                                if let Err(e) = tool_repo
                                                    .record(
                                                        &exec_id,
                                                        &mid,
                                                        &sid,
                                                        &tname,
                                                        status,
                                                        Some(&prov),
                                                        mdl.as_deref(),
                                                        None,
                                                    )
                                                    .await
                                                {
                                                    tracing::error!(
                                                        "[TOOL_EXEC] Failed to record tool execution: {}",
                                                        e
                                                    );
                                                }
                                            });

                                            // Audit READ row (#1705): only when the
                                            // operator enabled recording AND this tool
                                            // actually pulls external content into
                                            // context. Gated BEFORE any repo work, so a
                                            // disabled flag costs a bool check per call.
                                            if self.audit_recording
                                                && let Some(kind) = crate::db::repository::turn_retrieval::retrieval_kind(&tool_name)
                                            {
                                                let repo = crate::db::repository::TurnRetrievalRepository::new(pool.clone());
                                                let rid = uuid::Uuid::new_v4().to_string();
                                                let sid = session_id.to_string();
                                                let mid = assistant_db_msg.id.to_string();
                                                let tname = tool_name.clone();
                                                let target = crate::db::repository::turn_retrieval::audit_target(
                                                    &tool_name,
                                                    &tool_input_for_progress,
                                                )
                                                .unwrap_or_else(|| "unknown".to_string());
                                                let hash = {
                                                    use sha2::{Digest, Sha256};
                                                    let mut h = Sha256::new();
                                                    h.update(content.as_bytes());
                                                    format!("{:x}", h.finalize())
                                                };
                                                let preview: String =
                                                    content.chars().take(128).collect();
                                                tokio::spawn(async move {
                                                    if let Err(e) = repo
                                                        .record_retrieval(
                                                            &rid, &sid, &mid, &tname, kind,
                                                            &target, Some(&hash), Some(&preview),
                                                        )
                                                        .await
                                                    {
                                                        tracing::error!(
                                                            "[AUDIT] Failed to record turn retrieval: {e}"
                                                        );
                                                    }
                                                });
                                            }
                                        }

                                        let output_summary: String = strip_ansi_output(&content)
                                            .chars()
                                            .take(2000)
                                            .collect();
                                        tool_outputs.push((success, output_summary.clone()));
                                        if let Some(ref cb) = progress_callback {
                                            cb(
                                                session_id,
                                                ProgressEvent::ToolCompleted {
                                                    tool_name: tool_name.clone(),
                                                    tool_input: tool_input_for_progress.clone(),
                                                    success,
                                                    summary: output_summary,
                                                    tool_call_id: tool_id.clone(),
                                                },
                                            );
                                        }
                                        tool_results.push(ContentBlock::ToolResult {
                                            tool_use_id: tool_id,
                                            content,
                                            is_error: Some(!success),
                                        });
                                        // Append images (e.g. browser auto-screenshots) so the model sees them
                                        for (media_type, data) in images {
                                            tool_results.push(ContentBlock::Image {
                                                source:
                                                    crate::brain::provider::ImageSource::Base64 {
                                                        media_type,
                                                        data,
                                                    },
                                            });
                                        }
                                    }
                                    Err(e) => {
                                        let err_msg = format!("Tool execution error: {}", e);
                                        // GRANULAR LOG: Tool execution error
                                        tracing::error!(
                                            "[TOOL_EXEC] 💥 Tool '{}' error: {}",
                                            tool_name,
                                            err_msg
                                        );
                                        // #214: a name that didn't resolve or
                                        // args that failed validation means the
                                        // tool never ran, a model tool-use miss
                                        // rather than the tool failing. Bucket
                                        // it as discovery_miss so it stays out
                                        // of the tool's success rate.
                                        if e.is_pre_execution_miss() {
                                            self.record_tool_discovery_miss(
                                                session_id,
                                                &tool_name,
                                                Some(&tool_input_for_progress),
                                                Some(&err_msg),
                                            );
                                        } else {
                                            self.record_tool_feedback(
                                                session_id,
                                                &tool_name,
                                                Some(&tool_input_for_progress),
                                                false,
                                                Some(&err_msg),
                                            );
                                        }
                                        // Record tool execution for usage dashboard.
                                        // #687: skip pre-execution misses (unknown tool /
                                        // bad args) so garbage names don't pollute stats.
                                        if !e.is_pre_execution_miss()
                                            && let Some(pool) = crate::db::global_pool()
                                        {
                                            let tool_repo =
                                                crate::db::repository::ToolExecutionRepository::new(
                                                    pool.clone(),
                                                );
                                            let exec_id = uuid::Uuid::new_v4().to_string();
                                            let mid = assistant_db_msg.id.to_string();
                                            let sid = session_id.to_string();
                                            let tname = tool_name.clone();
                                            let prov = self.provider_name_for_session(session_id);
                                            let mdl =
                                                Some(self.provider_model_for_session(session_id));
                                            tokio::spawn(async move {
                                                if let Err(e) = tool_repo
                                                    .record(
                                                        &exec_id,
                                                        &mid,
                                                        &sid,
                                                        &tname,
                                                        "error",
                                                        Some(&prov),
                                                        mdl.as_deref(),
                                                        None,
                                                    )
                                                    .await
                                                {
                                                    tracing::error!(
                                                        "[TOOL_EXEC] Failed to record tool execution: {}",
                                                        e
                                                    );
                                                }
                                            });
                                        }
                                        let output_summary: String = strip_ansi_output(&err_msg)
                                            .chars()
                                            .take(2000)
                                            .collect();
                                        tool_outputs.push((false, output_summary.clone()));
                                        if let Some(ref cb) = progress_callback {
                                            cb(
                                                session_id,
                                                ProgressEvent::ToolCompleted {
                                                    tool_name: tool_name.clone(),
                                                    tool_input: tool_input_for_progress.clone(),
                                                    success: false,
                                                    summary: output_summary,
                                                    tool_call_id: tool_id.clone(),
                                                },
                                            );
                                        }
                                        // Inject brain-file guidance inline so a
                                        // tool miss carries its own remediation
                                        // hint (#767). err_msg stays pristine above
                                        // for logging/feedback; only the model-facing
                                        // content gets the hints.
                                        let mut content = err_msg;
                                        if let Some(hints) = crate::brain::hints::hints_for(
                                            &format!("{tool_name} {content}"),
                                        )
                                        .await
                                        {
                                            content.push_str(&hints);
                                        }
                                        tool_results.push(ContentBlock::ToolResult {
                                            tool_use_id: tool_id,
                                            content,
                                            is_error: Some(true),
                                        });
                                    }
                                }

                                // #1178 M1 turn-halt: suggest_options ends the turn once its result is
                                // flushed - the user picks an option and the next turn resumes from it.
                                // Policy routes through ToolRegistry::halts_turn (Tool::halts_turn).
                                if halt_turn_requested {
                                    tracing::info!(
                                        "🛑 Turn halted by option-surface tool (suggest_options)"
                                    );
                                    // #31: the following text-only iteration is the
                                    // sign-off — tag it for the phantom-gate exemption.
                                    option_surface_halt_seen = true;
                                    option_halt_iteration_has_text =
                                        !iteration_text.trim().is_empty();
                                    break;
                                }

                                continue; // Skip the normal execution path below
                            }
                            Err(e) => {
                                tracing::error!("Approval callback error: {}", e);
                                tool_outputs.push((false, format!("Approval failed: {}", e)));
                                tool_results.push(ContentBlock::ToolResult {
                                    tool_use_id: tool_id,
                                    content: format!("Approval request failed: {}", e),
                                    is_error: Some(true),
                                });
                                continue;
                            }
                        }
                    } else {
                        // Approval is required, the policy does not grant it, and
                        // this surface has no way to ask. Reaching here means the
                        // policy really is a gating one: `auto_approve_tools` is
                        // resolved from `approval_policy` at construction, so an
                        // auto policy would have cleared `needs_approval` above.
                        //
                        // The old text said only "no approval mechanism
                        // configured", which read as missing plumbing and sent
                        // people looking for a wiring bug when the answer was a
                        // setting (#769). Name the policy and the two ways out.
                        tracing::warn!(
                            "Tool '{}' requires approval: policy does not auto-approve and this \
                             surface has no interactive approval",
                            tool_name
                        );
                        let denial = format!(
                            "Tool '{tool_name}' requires approval. The current \
                             `agent.approval_policy` does not auto-approve, and this surface has \
                             no interactive approval available. Set `agent.approval_policy` to \
                             \"auto-always\", or re-run with `--auto-approve`."
                        );
                        tool_outputs.push((false, denial.clone()));
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id: tool_id,
                            content: denial,
                            is_error: Some(true),
                        });
                        continue;
                    }
                }

                // Execute the tool (no approval needed — mark context as approved
                // so the registry's own approval check doesn't block it)
                let mut approved_context = tool_context.clone();
                approved_context.auto_approve = true;
                let tool_start = std::time::Instant::now();
                // #1178 M1: set inside the Ok arm below when the tool ends the turn
                let mut halt_turn_requested = false;
                let exec_result = tokio::select! {
                    biased;
                    _ = async {

                        if let Some(ref t) = cancel_token { t.cancelled().await } else { std::future::pending().await }
                    } => {
                        tracing::warn!("🛑 Tool '{}' cancelled mid-execution", tool_name);
                        break;
                    }
                    r = self.tool_registry.execute(&tool_name, tool_input, &approved_context) => r,
                };
                match exec_result {
                    Ok(result) => {
                        // Registry-routed halt policy, success-gated (audit
                        // fix) — mirrors the approval-path site above.
                        if self.tool_registry.halts_turn(&tool_name) && result.success {
                            halt_turn_requested = true;
                        }
                        let success = result.success;
                        let images = result.images;
                        let result_output_for_evidence = result.output.clone();
                        let content =
                            build_tool_result_content(result.success, result.error, &result.output);

                        // GRANULAR LOG: Direct tool execution result
                        if success {
                            tracing::info!(
                                "[TOOL_EXEC] ✅ Tool '{}' executed successfully, output_len={}",
                                tool_name,
                                content.len()
                            );
                            // Reset the phantom retry counter so accumulated
                            // debt from earlier in the turn doesn't push a
                            // later, isolated phantom burst over the cap. The
                            // counter is now "consecutive phantoms since the
                            // last real tool execution" rather than "phantom
                            // count across the entire turn".
                            phantom_retries_used = 0;
                            // Mark the turn as having produced real work so
                            // the subsequent text-only wrap-up iteration is
                            // exempt from phantom-tool-call detection. See
                            // the comment on the `tool_calls_completed_this_turn`
                            // declaration at the top of `run_tool_loop_inner`.
                            tool_calls_completed_this_turn += 1;
                            // Keep the real output so a later iteration's
                            // quoted "evidence" can be checked against it.
                            turn_tool_output.push(result_output_for_evidence.clone());
                            // Deterministic footer feed (#1883): keep the full
                            // raw output of every search-engine call this turn.
                            if crate::brain::tools::sources_footer::is_search_tool(&tool_name) {
                                turn_search_outputs.push(result_output_for_evidence.clone());
                            }
                            // Persist the touched path (same rationale as the
                            // approval-path branch above).
                            if let Some(p) = extract_path_for_recent_buffer(
                                &tool_name,
                                &tool_input_for_progress,
                                &approved_context.working_directory,
                            ) {
                                self.record_recent_path(&approved_context.working_directory, &p);
                            }
                        } else {
                            tracing::error!(
                                "[TOOL_EXEC] ❌ Tool '{}' failed: {}",
                                tool_name,
                                content.chars().take(200).collect::<String>()
                            );
                        }

                        // Auto-record to feedback ledger (fire-and-forget)
                        self.record_tool_feedback(
                            session_id,
                            &tool_name,
                            Some(&tool_input_for_progress),
                            success,
                            if success { None } else { Some(&content) },
                        );

                        // Record tool execution for usage dashboard
                        if let Some(pool) = crate::db::global_pool() {
                            let tool_repo =
                                crate::db::repository::ToolExecutionRepository::new(pool.clone());
                            let exec_id = uuid::Uuid::new_v4().to_string();
                            let mid = assistant_db_msg.id.to_string();
                            let sid = session_id.to_string();
                            let tname = tool_name.clone();
                            let status = if success { "success" } else { "error" };
                            let prov = self.provider_name_for_session(session_id);
                            let mdl = Some(self.provider_model_for_session(session_id));
                            let dur_ms = tool_start.elapsed().as_millis() as i64;
                            tokio::spawn(async move {
                                if let Err(e) = tool_repo
                                    .record(
                                        &exec_id,
                                        &mid,
                                        &sid,
                                        &tname,
                                        status,
                                        Some(&prov),
                                        mdl.as_deref(),
                                        Some(dur_ms),
                                    )
                                    .await
                                {
                                    tracing::error!(
                                        "[TOOL_EXEC] Failed to record tool execution: {}",
                                        e
                                    );
                                }
                            });
                        }

                        let output_summary: String =
                            strip_ansi_output(&content).chars().take(2000).collect();
                        tool_outputs.push((success, output_summary.clone()));
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::ToolCompleted {
                                    tool_name: tool_name.clone(),
                                    tool_input: tool_input_for_progress.clone(),
                                    success,
                                    summary: output_summary,
                                    tool_call_id: tool_id.clone(),
                                },
                            );
                        }
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id: tool_id,
                            content,
                            is_error: Some(!success),
                        });
                        // Append images (e.g. browser auto-screenshots) so the model sees them
                        for (media_type, data) in images {
                            tool_results.push(ContentBlock::Image {
                                source: crate::brain::provider::ImageSource::Base64 {
                                    media_type,
                                    data,
                                },
                            });
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Tool execution error: {}", e);
                        // GRANULAR LOG: Direct tool execution error
                        tracing::error!("[TOOL_EXEC] 💥 Tool '{}' error: {}", tool_name, err_msg);
                        // #214: a name that didn't resolve or args that failed
                        // validation means the tool never ran, a model tool-use
                        // miss rather than the tool failing. Bucket it as
                        // discovery_miss so it stays out of the success rate.
                        if e.is_pre_execution_miss() {
                            self.record_tool_discovery_miss(
                                session_id,
                                &tool_name,
                                Some(&tool_input_for_progress),
                                Some(&err_msg),
                            );
                        } else {
                            self.record_tool_feedback(
                                session_id,
                                &tool_name,
                                Some(&tool_input_for_progress),
                                false,
                                Some(&err_msg),
                            );
                        }
                        // Record tool execution for usage dashboard.
                        // #687: skip pre-execution misses (unknown tool /
                        // bad args) so garbage names don't pollute stats.
                        if !e.is_pre_execution_miss()
                            && let Some(pool) = crate::db::global_pool()
                        {
                            let tool_repo =
                                crate::db::repository::ToolExecutionRepository::new(pool.clone());
                            let exec_id = uuid::Uuid::new_v4().to_string();
                            let mid = assistant_db_msg.id.to_string();
                            let sid = session_id.to_string();
                            let tname = tool_name.clone();
                            let prov = self.provider_name_for_session(session_id);
                            let mdl = Some(self.provider_model_for_session(session_id));
                            let dur_ms = tool_start.elapsed().as_millis() as i64;
                            tokio::spawn(async move {
                                if let Err(e) = tool_repo
                                    .record(
                                        &exec_id,
                                        &mid,
                                        &sid,
                                        &tname,
                                        "error",
                                        Some(&prov),
                                        mdl.as_deref(),
                                        Some(dur_ms),
                                    )
                                    .await
                                {
                                    tracing::error!(
                                        "[TOOL_EXEC] Failed to record tool execution: {}",
                                        e
                                    );
                                }
                            });
                        }
                        let output_summary: String = err_msg.chars().take(2000).collect();
                        tool_outputs.push((false, output_summary.clone()));
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::ToolCompleted {
                                    tool_name: tool_name.clone(),
                                    tool_input: tool_input_for_progress.clone(),
                                    success: false,
                                    summary: output_summary,
                                    tool_call_id: tool_id.clone(),
                                },
                            );
                        }
                        // Inject brain-file guidance inline so a tool miss
                        // carries its own remediation hint (#767). err_msg stays
                        // pristine above for logging/feedback; only the
                        // model-facing content gets the hints.
                        let mut content = err_msg;
                        if let Some(hints) =
                            crate::brain::hints::hints_for(&format!("{tool_name} {content}")).await
                        {
                            content.push_str(&hints);
                        }
                        tool_results.push(ContentBlock::ToolResult {
                            tool_use_id: tool_id,
                            content,
                            is_error: Some(true),
                        });
                    }
                }

                // #1178 M1 turn-halt: suggest_options ends the turn once its result is
                // flushed - the user picks an option and the next turn resumes from it.
                // Policy routes through ToolRegistry::halts_turn (Tool::halts_turn).
                if halt_turn_requested {
                    tracing::info!("🛑 Turn halted by option-surface tool (suggest_options)");
                    // #31: the following text-only iteration is the sign-off —
                    // tag it for the phantom-gate exemption.
                    option_surface_halt_seen = true;
                    option_halt_iteration_has_text = !iteration_text.trim().is_empty();
                    break;
                }
            }

            // Append tool call data to accumulated text for DB persistence.
            // v2 format: <!-- tools-v2: [{"d":"desc","s":true,"o":"output..."}] -->
            // Includes tool output so Ctrl+O expansion works after session reload.
            if !tool_descriptions.is_empty() {
                if !accumulated_text.is_empty() {
                    accumulated_text.push('\n');
                }
                let entries: Vec<serde_json::Value> = tool_descriptions.iter()
                    .zip(tool_outputs.iter())
                    .map(|(desc, (success, output))| {
                        serde_json::json!({"d": desc, "s": success, "o": output})
                    })
                    .collect();
                accumulated_text.push_str(&format!(
                    "<!-- tools-v2: {} -->",
                    serde_json::to_string(&entries).unwrap_or_default()
                ));

                // REAL-TIME PERSISTENCE: Save tool results to DB immediately
                let tool_block = format!(
                    "\n<!-- tools-v2: {} -->\n",
                    serde_json::to_string(&entries).unwrap_or_default()
                );
                let _ = message_service
                    .append_content(assistant_db_msg.id, &tool_block)
                    .await;

                // Notify TUI after each tool iteration so it refreshes in real-time,
                // even during long-running channel sessions (Telegram, WhatsApp, etc.)
                if let Some(ref tx) = self.session_updated_tx {
                    let _ = tx.send(crate::brain::agent::ChannelSessionEvent::Updated(
                        session_id,
                    ));
                }

                tool_descriptions.clear();
                tool_outputs.clear();
            }

            // Add assistant message with tool use to context (filter empty text blocks).
            // Preserve the live reasoning text as a ContentBlock::Thinking so
            // downstream providers that require it — notably Moonshot kimi
            // via opencode.ai/zen/go, which rejects any follow-up turn whose
            // assistant tool_call messages omit `reasoning_content` — can
            // echo the real reasoning instead of a placeholder. Other
            // providers either use it natively (Anthropic) or ignore
            // unknown blocks (OpenAI, Zhipu, Minimax).
            let mut clean_content: Vec<ContentBlock> = response
                .content
                .iter()
                .filter(|b| !matches!(b, ContentBlock::Text { text } if text.is_empty()))
                .cloned()
                .collect();
            if let Some(ref reasoning) = reasoning_text
                && !reasoning.trim().is_empty()
                && !clean_content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Thinking { .. }))
            {
                clean_content.insert(
                    0,
                    ContentBlock::Thinking {
                        thinking: reasoning.clone(),
                        signature: None,
                    },
                );
            }
            // Within-turn announcement loop guard (#961) — the
            // intermediate-text half. The #957 ring only saw turn-FINAL
            // text, but the DeepSeek v4 flash zip-send pattern
            // re-announces "sending now" between tool calls INSIDE one
            // turn: every announcement rides a clean iteration, so no
            // guard ever saw a repeat. Feed each iteration's outgoing
            // text through the same per-session ring — first trip nudges
            // (system message the next iteration sees), second trip ends
            // the turn through the repetition path. Compute the text
            // BEFORE `clean_content` moves into the message below.
            let outgoing_iteration_text: String = clean_content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let assistant_msg = Message {
                role: crate::brain::provider::Role::Assistant,
                content: clean_content,
            };
            context.add_message(assistant_msg);
            if !outgoing_iteration_text.trim().is_empty() {
                let action = {
                    let mut rings = self.session_outgoing_text_ring.write().unwrap();
                    rings
                        .entry(session_id)
                        .or_default()
                        .record_and_check(&outgoing_iteration_text)
                };
                match action {
                    super::announcement_loop::TextLoopAction::Abort => {
                        tracing::warn!(
                            "⚠️ Within-turn announcement loop persisted after nudge — ending \
                             turn (#961)"
                        );
                        // Provider-attributable so it can reach the fallback
                        // walk (#1023): the nudge already failed against this
                        // model, and a different one usually emits the call.
                        let model = self.provider_model_for_session(session_id);
                        return Err(AgentError::Provider(
                            crate::brain::provider::ProviderError::AnnouncementLoop(format!(
                                "near-identical announcements repeated within the turn \
                                     [diagnostics:model={model},retries={phantom_retries_used},swaps={phantom_swaps_done},rolls={phantom_rolls}]"
                            )),
                        ));
                    }
                    super::announcement_loop::TextLoopAction::Nudge => {
                        tracing::warn!(
                            "Within-turn announcement loop: near-identical intermediate text \
                             recurred — nudging agent (#961)"
                        );
                        if let Some(ref cb) = progress_callback {
                            cb(
                                session_id,
                                ProgressEvent::SelfHealingAlert {
                                    message: "Agent is re-announcing the same pending action \
                                              between tool calls — nudging it to act or report"
                                        .into(),
                                },
                            );
                        }
                        context.add_message(Message::user(
                            "[System: You have announced essentially the same pending action \
                             repeatedly within this turn without completing it or reporting a \
                             failure. Do NOT announce it again. Either execute it now and \
                             report the concrete result, or state plainly why it cannot be \
                             done.]",
                        ));
                    }
                    super::announcement_loop::TextLoopAction::Continue => {}
                }
            }

            // Cap oversized tool_result bodies BEFORE they enter context.
            // A single 1 MB read_file output (e.g., an HTML file with an
            // embedded base64 PNG) dumps ~256k tokens into context in one
            // push — exceeding the model's window AND the compaction
            // summarizer's window, triggering a hard-truncate-to-zero
            // cascade observed today on session 5ed9ff25 (read of
            // opencrabs-retro-release.html, 1,025,562 bytes → ctx jumps
            // 8k → 738k → 0 messages after truncate). Truncate generously
            // (50 KB chars ≈ 12k tokens, ~6% of a 200k window) and
            // instruct the agent to re-call with offsets / grep / line
            // ranges for the part it actually needs.
            const MAX_TOOL_RESULT_CHARS: usize = 50_000;
            for block in tool_results.iter_mut() {
                if let ContentBlock::ToolResult { content, .. } = block
                    && content.len() > MAX_TOOL_RESULT_CHARS
                {
                    let original_len = content.len();
                    let mut cut = MAX_TOOL_RESULT_CHARS;
                    while cut > 0 && !content.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    content.truncate(cut);
                    content.push_str(&format!(
                        "\n\n[Output truncated: {} → {} bytes. Re-call with \
                         start_line/line_count (read_file), head/tail, \
                         grep --max-count, or similar to fetch specific \
                         portions instead of the whole blob.]",
                        original_len, cut,
                    ));
                    tracing::warn!(
                        "Tool result content capped: {} → {} bytes (max {} chars)",
                        original_len,
                        cut,
                        MAX_TOOL_RESULT_CHARS,
                    );
                }
            }

            // Add user message with tool results to context
            let tool_result_msg = Message {
                role: crate::brain::provider::Role::User,
                content: tool_results,
            };
            context.add_message(tool_result_msg);

            // A fabrication that rode in the same response as a real tool call
            // (#1693). The calls above were real work and ran; only the claim
            // attached to them was unsupported, so this suppresses nothing and
            // ends nothing — it cites the unsupported assertion back and the
            // loop continues, exactly like the repeat correction beside it.
            //
            // #1506 still governs: a structured completion report is never
            // nagged into a retry, so its branches go to the WARN and stop.
            if let Some(violation) = mixed_fact_verdict {
                if violation.structured_report {
                    tracing::warn!(
                        target: "phantom",
                        branches = ?violation.branches,
                        "mixed-iteration fabrication inside a structured report — delivered, not nagged (#1506)",
                    );
                } else {
                    tracing::warn!(
                        target: "phantom",
                        branches = ?violation.branches,
                        commands = ?violation.uncalled_commands,
                        facts = ?violation.unbacked_facts,
                        "Fabricated claim rode a real tool call — correcting in-turn (#1693)",
                    );
                    let prov = self.provider_name_for_session(session_id);
                    let mdl = Some(self.provider_model_for_session(session_id));
                    crate::db::repository::AnalyticsEventRepository::emit_phantom(
                        &session_id.to_string(),
                        Some(&prov),
                        mdl.as_deref(),
                    );
                    // Naming the fabricated command or invented fact outranks
                    // the generic wording: it cites a fact instead of a
                    // category, so the model cannot rationalise it (#797, #1423).
                    let nudge = if !violation.uncalled_commands.is_empty() {
                        super::nudge::uncalled_commands_nudge(&violation.uncalled_commands)
                    } else if !violation.unbacked_facts.is_empty() {
                        super::nudge::unbacked_facts_nudge(&violation.unbacked_facts)
                    } else {
                        super::nudge::no_tool_calls_nudge(is_local_provider)
                    };
                    context.add_message(Message::user(nudge));
                }
            }

            // The repeat correction goes AFTER the results, so the model sees
            // the identical output it just got and then why repeating it
            // cannot help. Never suppresses the call and never ends the turn:
            // it adds one message and the loop continues (#1030).
            if let Some(nudge) = repeat_verdict {
                tracing::warn!(
                    target: "tool_repeat",
                    "Identical tool round repeated {} times; injecting a correction",
                    tool_repeat.consecutive()
                );
                context.add_message(Message::user(nudge));
            }

            // Fire token count update after tool results are added — keeps TUI in sync.
            if let Some(ref cb) = progress_callback {
                cb(session_id, ProgressEvent::TokenCount(context.token_count));
            }
            if has_progress_override && let Some(ref cb) = self.progress_callback {
                cb(session_id, ProgressEvent::TokenCount(context.token_count));
            }

            // Intra-turn periodic time marker (#153): inject current wall-clock
            // notice strictly AFTER tool results and repeat verdict, but BEFORE
            // compaction checks or next assistant generation, if >= interval elapsed.
            if let Some((notice, updated_time)) = check_intra_turn_time_marker(
                last_time_marker,
                chrono::Utc::now(),
                time_marker_interval_secs,
                brain_dir.as_deref(),
            ) {
                tracing::info!("Injecting mid-turn time notice into context: {notice}");
                context.add_message(Message::user(notice));
                last_time_marker = updated_time;
            }

            // Enforce 65% budget after tool results. Skip ONLY when the CLI
            // owns its session (claude-cli with --resume). Qwen is spawned
            // cold every turn so we MUST compact for it.
            if let Some(ref outcome) = if cli_owns_context {
                None
            } else {
                self.enforce_context_budget(
                    session_id,
                    &mut context,
                    &model_name,
                    cancel_token.as_ref(),
                    &progress_callback,
                    super::compaction::BudgetPhase::MidLoop,
                )
                .await
            } {
                // Persist compaction marker to DB so restarts load from this point
                if let Err(e) = message_service
                    .create_message(session_id, "user".to_string(), outcome.marker(""))
                    .await
                {
                    tracing::error!("Failed to persist post-tool compaction marker to DB: {}", e);
                }

                let cont_text = self
                    .continuation_prompt(
                        session_id,
                        super::compaction_prompts::CompactionKind::PostTool,
                    )
                    .await;
                context.add_message(Message::user(cont_text));
            }

            // Check for queued user messages to inject between tool iterations.
            // This lets the user provide follow-up feedback mid-execution (like Claude Code).
            if let Some(ref queue_cb) = self.message_queue_callback
                && let Some(queued_msg) = queue_cb(session_id).await
            {
                tracing::info!("Injecting queued user message between tool iterations");

                // Depth-3 notify receipts (fork #50): the drain consumes the
                // session's whole queue, so every notify receipt queued for
                // this target is now provably consumed — stamp them injected
                // before the sender can ask.
                let stamped = super::notify_receipts::mark_injected_for_target(session_id);
                if stamped > 0 {
                    tracing::info!(
                        "Stamped {stamped} notify receipt(s) injected for session {session_id}"
                    );
                }

                // Notify TUI so the user message appears inline in the chat flow
                if let Some(ref cb) = progress_callback {
                    cb(
                        session_id,
                        ProgressEvent::QueuedUserMessage {
                            text: queued_msg.display_text.clone(),
                        },
                    );
                }

                let injected = Message::user(queued_msg.context_text.clone());
                context.add_message(injected);

                // Save to database so conversation history stays consistent
                if let Err(e) = message_service
                    .create_message(session_id, "user".to_string(), queued_msg.display_text)
                    .await
                {
                    tracing::error!("Failed to persist queued user message: {e}");
                }
                // Create a NEW assistant placeholder so the next response
                // gets a sequence number AFTER the queued user message.
                assistant_db_msg = message_service
                    .create_message(session_id, "assistant".to_string(), String::new())
                    .await
                    .map_err(AgentError::db)?;
            }
        }

        // === GRACEFUL SAVE ON CANCEL/LOOP-BREAK ===
        // If we broke out of the loop without a final_response (cancellation, error, etc.)
        // but we have accumulated text/tool results, they're already in the DB from real-time persistence.
        // Usage update is handled below in the unified path after response synthesis —
        // doing it here too would double-count because the synthesized response (line below)
        // still flows through the final update_session_usage call.
        if final_response.is_none() && !accumulated_text.is_empty() {
            tracing::info!(
                "Loop broken without final response but accumulated text ({} chars) already persisted in real-time",
                accumulated_text.len()
            );
        }

        // If the loop broke without a final_response but we have accumulated text,
        // synthesize a partial response instead of erroring — the user already saw the
        // text streamed in real-time, so returning it keeps the TUI consistent.
        let response = match final_response {
            Some(resp) => resp,
            None if !accumulated_text.is_empty() => {
                tracing::warn!(
                    "Synthesizing partial response from {} chars of accumulated text \
                     (loop broke without final LLM response)",
                    accumulated_text.len()
                );
                LLMResponse {
                    id: String::new(),
                    content: vec![ContentBlock::Text {
                        text: accumulated_text.clone(),
                    }],
                    model: model_name.clone(),
                    usage: crate::brain::provider::TokenUsage {
                        input_tokens: total_input_tokens,
                        output_tokens: total_output_tokens,
                        cache_creation_tokens: total_cache_creation,
                        cache_read_tokens: total_cache_read,
                        ..Default::default()
                    },
                    stop_reason: Some(crate::brain::provider::StopReason::EndTurn),
                    // Synthesised from accumulated state — the real
                    // streaming windows already contributed via the
                    // per-iteration LLMResponses; this top-level
                    // synthesis is for content/usage handoff only.
                    streaming_active_secs: None,
                    tool_text_leak: false,
                }
            }
            None => {
                // If the cancel token is set and was triggered, this is a user-initiated
                // cancellation — return Cancelled instead of a noisy Internal error.
                if let Some(ref token) = cancel_token
                    && token.is_cancelled()
                {
                    return Err(AgentError::Cancelled);
                }
                return Err(AgentError::Internal(
                    "Tool loop ended without final response".to_string(),
                ));
            }
        };

        // Extract text from the final response only (for TUI display).
        // Intermediate text was already shown in real-time via IntermediateText events.
        let mut final_text = Self::extract_text_from_response(&response);

        // A continuation EXTENDS the partial, it does not replace it. The
        // comment above is true for the TUI, where the partial already reached
        // the user as IntermediateText, and false for channels that gate
        // intermediates (#838): there the partial is dropped and only the
        // continuation is delivered (#859).
        if let Some(partial) = truncation_partial.as_deref() {
            use super::truncation::Continuation;
            match super::truncation::join_continuation(partial, &final_text) {
                Continuation::Extended(joined) => {
                    tracing::info!(
                        "[TRUNCATION] verdict=recovered: joined {} char partial with {} char \
                         continuation into {} chars",
                        partial.chars().count(),
                        final_text.chars().count(),
                        joined.chars().count()
                    );
                    final_text = joined;
                }
                Continuation::Echoed(still_partial) => {
                    // The continuation recovered nothing — the model echoed the
                    // tail it was asked to continue from. The attempt budget is
                    // spent (`truncation_continue_attempts`), so this answer
                    // is as complete as it will get. Delivering it unmarked told
                    // the user a sentence ending at a colon was finished (#956).
                    tracing::warn!(
                        "[TRUNCATION] verdict=unrecovered: {} char continuation added nothing \
                         to the {} char partial (model echoed the tail) — delivering it marked \
                         incomplete",
                        final_text.chars().count(),
                        partial.chars().count(),
                    );
                    self.record_provider_feedback(
                        session_id,
                        "truncation_unrecovered",
                        "stream-integrity",
                        Some(
                            "continuation echoed the tail; answer delivered with incomplete \
                             marker (#36)",
                        ),
                    );
                    final_text = format!("{still_partial}{}", super::truncation::INCOMPLETE_MARKER);
                }
            }
        }

        // Cross-turn announcement loop guard (#957) — the text layer. The
        // bash-echo half of the Luna pattern is caught by the near-match
        // check in the tool layer above; this catches the reworded
        // announcements that each land as a separate, internally clean
        // turn. Ring of the last 8 outgoing texts per session (same ring
        // the within-turn hook above feeds, #961); 3 near-duplicates trip
        // a system nudge (the text still delivers — detect and surface,
        // #954 philosophy), a second trip aborts through the repetition
        // -> loop-message path. Checked BEFORE the #752 phantom
        // replacement so the ring judges the model's own words and the
        // canned phantom string can never trip the guard.
        if !final_text.trim().is_empty() {
            let action = {
                let mut rings = self.session_outgoing_text_ring.write().unwrap();
                let ring = rings.entry(session_id).or_default();
                // Dedupe (#961): the within-turn hook may have already recorded
                // this exact text; re-recording it would double-count one
                // outgoing text toward the trip threshold.
                match ring.last_recorded() {
                    Some(last) if last == final_text => None,
                    _ => Some(ring.record_and_check(&final_text)),
                }
            };
            match action {
                Some(super::announcement_loop::TextLoopAction::Abort) => {
                    tracing::warn!(
                        "⚠️ Cross-turn announcement loop persisted after nudge — aborting turn \
                         (#957)"
                    );
                    let model = self.provider_model_for_session(session_id);
                    return Err(AgentError::Provider(
                        crate::brain::provider::ProviderError::AnnouncementLoop(format!(
                            "near-identical announcements repeated across turns \
                                 [diagnostics:model={model},retries={phantom_retries_used},swaps={phantom_swaps_done},rolls={phantom_rolls}]"
                        )),
                    ));
                }
                Some(super::announcement_loop::TextLoopAction::Nudge) => {
                    tracing::warn!(
                        "Cross-turn announcement loop: near-identical outgoing text recurred — \
                         nudging agent (#957)"
                    );
                    if let Some(ref cb) = progress_callback {
                        cb(
                            session_id,
                            ProgressEvent::SelfHealingAlert {
                                message: "Agent is re-announcing the same pending action across \
                                          turns — nudging it to act or report"
                                    .into(),
                            },
                        );
                    }
                    context.add_message(Message::user(
                        "[System: You have now announced essentially the same pending action \
                         three times across recent turns without completing it or reporting a \
                         failure. Do NOT announce it again. Either execute it now and report \
                         the concrete result, or state plainly why it cannot be done.]",
                    ));
                }
                Some(super::announcement_loop::TextLoopAction::Continue) | None => {}
            }
        }

        // Turn-end phantom verdict (#752): a turn that ran ZERO tools and ends
        // with a narration promising or claiming action ("On it, filing the
        // issue... Let me check the repo first", unbacked side effects, or a
        // fabricated image) did nothing. The per-iteration checks can miss it
        // when the answer arrives via a recovery path (e.g. the empty-reasoning
        // fallback produces text AFTER the iteration's phantom check already ran
        // on empty text), so guard the delivered answer here. Multilingual by
        // construction — reuses the phantom_lang detectors. Replace the lie with
        // a truthful message instead of telling the user it was done.
        // Branch-class rule at turn end (#2039), mirroring the in-loop gate. A
        // structured report is exempt from the phrase matchers, never from a
        // fact check: a zero-tool answer that states facts the conversation
        // cannot back is fabricated whatever its shape.
        let turn_end_asserted = super::phantom::asserted_facts(&final_text);
        let turn_end_fabricated = tool_calls_completed_this_turn == 0
            && !turn_end_asserted.is_empty()
            && !super::phantom::unbacked_facts(
                &turn_end_asserted,
                &Self::conversation_evidence(&context, &turn_tool_output),
            )
            .is_empty();
        if tool_calls_completed_this_turn == 0
            && !final_text.trim().is_empty()
            && (turn_end_fabricated
                || (!super::phantom::is_structured_report(&final_text)
                    && (super::phantom::has_phantom_tool_intent_no_tools(&final_text)
                        || super::phantom::claims_unbacked_side_effects(&final_text)
                        || super::phantom::claims_unbacked_media_result(&final_text)
                        // Structural tell (#1194): the answer IS a shell command in a
                        // shell-tagged fence, and no tool ran. Carries no phrase list,
                        // so it holds where the phrase detectors above have their next
                        // gap, and it is what the reported turn actually was.
                        || super::fenced_command::narrates_unrun_shell_block(&final_text))))
        {
            tracing::warn!(
                "Turn-end phantom verdict: 0 tools ran but the answer narrates action — \
                 replacing with a truthful note (#752). preview={:?}",
                final_text.chars().take(120).collect::<String>()
            );
            if let Some(ref cb) = progress_callback {
                cb(
                    session_id,
                    ProgressEvent::StripStreamedContent {
                        bytes: usize::MAX,
                        reason: "turn-end phantom narration discarded".to_string(),
                    },
                );
            }
            final_text = "I described actions but did not actually execute any tool this turn, \
                          so nothing was done. Tell me to proceed and I will run the tools."
                .to_string();
        }

        // The assistant message was already created and updated in real-time.
        // Now update with final token usage.

        // Calculate total cost with full cache breakdown for accurate pricing.
        // input_tokens = non-cached, cache_creation/read tracked separately.
        let billable_input = total_input_tokens + total_cache_creation + total_cache_read;
        let total_tokens = billable_input + total_output_tokens;
        let table_cost = self
            .provider_for_session(session_id)
            .calculate_cost_with_cache(
                &response.model,
                total_input_tokens,
                total_output_tokens,
                total_cache_creation,
                total_cache_read,
            );
        // The provider's own dollar figure is the invoice; the pricing table
        // is only a guess about it. Use the reported sum only when every
        // counted iteration reported one — a partial sum would under-bill the
        // rest (#1707).
        let cost = crate::brain::provider::types::authoritative_cost(
            reported_cost_sum,
            cost_missing_iters,
            table_cost,
        );
        if cost != table_cost {
            tracing::info!(
                "ledger uses provider-reported cost {:.6} over table estimate {:.6}",
                cost,
                table_cost
            );
        }

        // Update message with usage info. The stashed prompt-token count
        // drives the UI ctx meter, which must show the LAST iteration's
        // prompt size — i.e. the actual context window the model just
        // saw. Summing across iterations (`billable_input`) inflates by
        // factor N: a turn with 5 tool rounds against a 22K final prompt
        // displayed as ~150K (2026-04-17 05:55 logs). Cost calculation
        // still uses the cumulative billing fields above; only the
        // displayed ctx number uses the last-iter value here.
        let stored_input_tokens: i64 = if last_iter_input_tokens > 0 {
            last_iter_input_tokens as i64
        } else {
            let overhead = self.base_context_tokens();
            (context.token_count.saturating_add(overhead as usize)) as i64
        };
        message_service
            .update_message_usage(
                assistant_db_msg.id,
                crate::services::message::MessageUsage {
                    token_count: total_tokens as i64,
                    cost,
                    input_tokens: Some(stored_input_tokens),
                    cache_creation_tokens: Some(total_cache_creation as i64),
                    cache_read_tokens: Some(total_cache_read as i64),
                    duration_secs: Some(turn_started_at.elapsed().as_secs() as i64),
                },
            )
            .await
            .map_err(AgentError::db)?;

        // Update session token usage. The pair is resolved here, not read back
        // off the session row, so a fallback's spend is attributed to the
        // provider that actually served it (#807). The cache-read count rides
        // along for the By Model cache share (#2027).
        session_service
            .update_session_usage(
                session_id,
                total_tokens as i64,
                cost,
                &self.provider_name_for_session(session_id),
                &self.provider_model_for_session(session_id),
                Some(total_cache_read as i64),
            )
            .await
            .map_err(AgentError::db)?;

        // Notify the TUI that this session was updated (enables live refresh when
        // a remote channel — Telegram, WhatsApp, Discord, Slack — processes a message).
        if let Some(ref tx) = self.session_updated_tx {
            let _ = tx.send(crate::brain::agent::ChannelSessionEvent::Updated(
                session_id,
            ));
        }

        // Calculate tokens per second for channel footer display.
        //
        // Numerator: provider-reported output_tokens summed across
        // every iteration of the tool loop. This is the authoritative
        // count from the provider's billing pipeline — never the
        // tiktoken approximation the TUI uses during live streaming
        // (cl100k_base over-counts Qwen/Kimi/GLM bytes by ~1.5-2×,
        // which combined with sub-second burst windows showed users
        // 700-800 tok/s for providers that genuinely run ~100 tok/s).
        //
        // Denominator: sum of per-iteration active-streaming windows
        // populated by `stream_complete` in helpers.rs. Each window
        // is the wall time between the first and last content delta
        // of that iteration, with idle gaps >1s excluded — so tool
        // execution, approval prompts, DB persistence, and the gap
        // between iterations are all left out. The result is the
        // model's actual sustained generation rate during streaming,
        // not output-divided-by-full-turn-wall-clock (which silently
        // halved the rate on tool-heavy turns).
        //
        // Guarded against burst-delivery artifacts: a near-zero active
        // window (provider dumped the response in one sub-second chunk)
        // or an implausibly high rate yields None instead of a fantasy
        // number like the 37203 tok/s observed on a glm-5.1 short reply.
        let tokens_per_second =
            compute_streaming_tok_per_sec(total_output_tokens, total_streaming_active_secs);

        // The turn is complete and the response is built — NOW (never before)
        // honor a manual provider/model switch the user made mid-turn, so an
        // automatic fallback this turn took doesn't stick over their pick.
        self.finalize_manual_switch(session_id, start_switch_epoch, &session_service)
            .await;

        // Audit OUTCOME row (#1705): the settled turn's mechanical verdict,
        // classified from the tool outputs only (test receipts / rustc
        // errors). Recording off → no write at all, the /audit viewer stays
        // ACTION-only. A turn that died on an error path never reaches this
        // settle point and honestly has no OUTCOME row: the viewer renders
        // those with a dash, not a guess.
        if self.audit_recording
            && let Some(pool) = crate::db::global_pool()
        {
            let (outcome, evidence) =
                crate::db::repository::turn_retrieval::turn_outcome_from_outputs(&turn_tool_output);
            let repo = crate::db::repository::TurnRetrievalRepository::new(pool.clone());
            let rid = uuid::Uuid::new_v4().to_string();
            let sid = session_id.to_string();
            let mid = assistant_db_msg.id.to_string();
            tokio::spawn(async move {
                if let Err(e) = repo
                    .record_outcome(&rid, &sid, &mid, outcome, evidence.as_deref())
                    .await
                {
                    tracing::error!("[AUDIT] Failed to record turn outcome: {e}");
                }
            });
        }

        // Plan archive at turn settle (ADR 0005 Decision 9): the completing
        // turn keeps its live plan and full all-☑ checklist through delivery;
        // once the turn settles here the finished plan archives and the session
        // returns to NoPlan, so the next turn carries no plan chrome. This is
        // the surface-agnostic settle hook every surface's turn ends through, so
        // TUI and Telegram both archive without a channel-specific path.
        // `is_complete` requires non-empty, all-resolved tasks, so Editing and
        // in-progress plans are untouched; `load_plan` is a fast no-op when the
        // session has no live plan.
        if let Some(mut finished) = crate::utils::plan_files::load_plan(session_id).await {
            // A plan whose only remaining task is the trailing delivery step
            // never reaches is_complete() on its own — delivering the answer is
            // what leaves that box unchecked, so the card lingers with 1/N done
            // (#737). When THIS turn delivered a final response, complete that
            // trailing task and persist, so the archive below fires.
            if !final_text.trim().is_empty()
                && finished.complete_trailing_delivery_task()
                && let Err(e) = crate::utils::plan_files::save_plan(&finished).await
            {
                tracing::warn!("Failed to persist auto-completed trailing plan task: {e}");
            }
            if finished.is_complete()
                && let Err(e) = crate::utils::plan_files::archive_plan(session_id).await
            {
                tracing::warn!("Failed to archive completed plan at turn settle: {e}");
            }
        }

        // Channel-safe suggest_options recovery (#1774): a model that fails
        // to emit the structured call sometimes writes it as TEXT —
        // <<suggest_options>> markers around a JSON options array (see
        // utils::directives). Recover at the settle point: strip the block
        // from the delivered text and fire the real SuggestedOptions event
        // so every surface renders native buttons. An unparseable block
        // still loses its markers: raw markers and raw JSON never ship.
        if let Some(cb) = self.progress_callback.as_ref() {
            let (cleaned, recovered) =
                crate::utils::directives::extract_leaked_suggestions(&final_text);
            if cleaned != final_text {
                final_text = cleaned;
            }
            if let Some(items) = recovered {
                cb(session_id, ProgressEvent::SuggestedOptions(items));
            }
        }

        // Deterministic Sources footer (#1883): links harvested from THIS
        // turn's search-tool outputs, appended to the delivered answer so a
        // search-backed reply never depends on the model feeling like citing.
        // No search this turn, or every source already linked in the answer,
        // leaves final_text byte-for-byte unchanged. The same footer goes to
        // the DB row so a resumed session shows what was actually delivered.
        if let Some(footer) =
            crate::brain::tools::sources_footer::footer_for(&final_text, &turn_search_outputs)
        {
            final_text = format!("{}\n\n{}", final_text.trim_end(), footer);
            if let Err(e) = message_service
                .append_content(assistant_db_msg.id, &format!("\n\n{footer}"))
                .await
            {
                tracing::warn!("Failed to persist Sources footer to DB: {}", e);
            }
        }

        Ok(AgentResponse {
            message_id: assistant_db_msg.id,
            content: final_text,
            stop_reason: response.stop_reason,
            usage: crate::brain::provider::TokenUsage {
                input_tokens: total_input_tokens,
                output_tokens: total_output_tokens,
                cache_creation_tokens: total_cache_creation,
                cache_read_tokens: total_cache_read,
                ..Default::default()
            },
            context_tokens: context.token_count as u32,
            tokens_per_second,
            cost,
            model: response.model,
            provider_name: self.provider_name_for_session(session_id),
            started_on_session_provider,
        })
    }
}
