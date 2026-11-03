//! Context-budget enforcement (Tier 1 + Tier 2 compaction).
//!
//! Extracted from `tool_loop.rs` (was lines 134-330) as part of the
//! 2026-05-04 Linor-flagged refactor: `tool_loop.rs` was 4,047 lines.
//! Compaction logic is cohesive — one async method that decides between
//! the soft 65 % LLM-summarisation tier, the 90 % hard-truncate floor,
//! and the safety-net truncation when all attempts fail. Lives next to
//! the rest of `impl AgentService` in the same crate; callers still
//! invoke it as `self.enforce_context_budget(...)` exactly as before.
//!
//! Behaviour is unchanged from the pre-extraction version. The
//! exhaustive comments inside the function were preserved verbatim
//! because they document the regression history (pre-0f052250 shape,
//! cancellation race details, the failed async-spawn variant).

use super::builder::AgentService;
use super::compaction_notice::CompactionNotifier;
use super::types::{ProgressCallback, ProgressEvent};
use crate::brain::agent::context::{AgentContext, CompactionScope};
use std::sync::atomic::Ordering;
use uuid::Uuid;

/// What `enforce_context_budget` did to the context on this visit.
///
/// Both variants MUST be persisted as a DB marker row. The summarised case is
/// obvious. The truncated case is the one that bit us: dropping the oldest
/// messages without writing a marker leaves `messages_from_last_compaction`
/// pointing at the same old anchor, so the next restart reloads the history
/// that just overflowed and overflows again on the first turn. Two sessions
/// died that way on 2026-05-05 (397% and 372% context, 793k tokens still in
/// the DB, a fresh loop on every user message). A marker with no summary is
/// lossy; no marker at all is unrecoverable.
pub(crate) enum CompactionOutcome {
    /// A summary was produced and swapped into the live context. The String is
    /// the exact marker text the apply step welded into the context — banner
    /// plus summary — so the persisted row carries the same bytes the live
    /// context does (#1928).
    Summarised(String),
    /// Every summariser attempt failed, so the oldest messages were dropped
    /// to fit the window instead. Nothing before this point survives.
    Truncated,
}

impl CompactionOutcome {
    /// The marker row to persist. `trigger` names what prompted this
    /// compaction (empty for the ordinary paths), so each call site keeps its
    /// own wording without copying the marker text five times.
    pub(crate) fn marker(&self, trigger: &str) -> String {
        match self {
            // #1928: the payload IS the applied marker (banner + summary).
            // Persist those bytes; rebuilding the banner here is exactly how
            // the in-memory marker and the DB row drifted apart — a delta
            // segment's sentinel never reached the DB, so the loader counted
            // every segment as a fresh boundary and kept only the newest.
            // The trigger decoration rides the full-window banner's
            // "compacted." sentence; the delta and consolidation banners carry
            // no such sentence and are persisted unchanged.
            Self::Summarised(applied_marker) if trigger.is_empty() => applied_marker.clone(),
            Self::Summarised(applied_marker) => applied_marker.replacen(
                "automatically compacted.",
                &format!("automatically compacted{trigger}."),
                1,
            ),
            Self::Truncated => format!(
                "[CONTEXT COMPACTION — The conversation was automatically compacted{trigger}. \
                 No summary is available: every summariser attempt failed, so the oldest \
                 messages were dropped to fit the window. Nothing before this point \
                 survives. Ask the user to restate anything you need rather than guessing \
                 at what was lost.]"
            ),
        }
    }
}

/// A summariser running against a snapshot of a session's context while that
/// session keeps taking turns.
///
/// There is deliberately no cancel handle. Cancelling an in-flight summary to
/// make room is what produced the 2026-05-05 loop: the context was truncated,
/// the summary that would have justified the truncation was thrown away, and
/// no marker was written, so every restart replayed the overflow. When the
/// context outgrows the headroom the turn WAITS for this task instead.
pub(crate) struct PendingCompaction {
    handle: tokio::task::JoinHandle<crate::brain::agent::error::Result<String>>,
    /// Length of the message vector when the snapshot was taken. Everything
    /// after this index arrived while the summariser was thinking and is not
    /// described by the summary, so the swap re-appends it.
    snapshot_len: usize,
    /// What the summariser was pointed at (#1649): the whole window, the
    /// delta since the last marker, or the frozen segments. The apply path
    /// dispatches on this — a delta lands as a new frozen segment, a
    /// consolidation replaces the segments, a full window swaps everything.
    scope: CompactionScope,
    /// Fill level at spawn time, reported as the "before" on the receipt so
    /// the number reflects the context that was actually summarised.
    snapshot_usage_pct: f64,
    started: std::time::Instant,
}

impl PendingCompaction {
    /// Stop a summariser whose subject no longer exists.
    ///
    /// Not the same move as cancelling to reclaim headroom, which is what
    /// looped two sessions in May: there the summary was still the thing that
    /// would have made the truncation recoverable. Here the context it
    /// describes has already been replaced by a synchronous compaction, so
    /// the result could only overwrite fresher state. Letting it run would
    /// spend a provider call on a summary nothing may apply.
    pub(super) fn abort(self) {
        self.handle.abort();
    }
}

/// Where in a turn a budget check is happening.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetPhase {
    /// About to answer a user. A summary in flight is waited for here: the
    /// reply would otherwise be composed against a context that is one swap
    /// away from being replaced, and the user is already waiting on a reply
    /// anyway, so the wait costs them nothing extra.
    TurnStart,
    /// Between provider calls inside the tool loop. A summary in flight keeps
    /// running unless the context has climbed to the ceiling.
    MidLoop,
}

/// What a visit found in the session's pending slot.
pub(crate) enum PendingState {
    /// Nothing in flight.
    Empty,
    /// Still thinking, and this visit is not obliged to wait for it.
    StillRunning,
    /// A summary landed and was swapped into the live context. Carries the
    /// exact marker text the apply welded in (#1928).
    Applied(String),
    /// The task finished without a summary. Spawning another would just
    /// repeat the same failing call on every visit, so the caller falls back
    /// to the blocking path and its attempt budget.
    Failed,
}

/// What the 65% gate owes a session that has just crossed it (#1686).
///
/// True only when this visit starts or performs a summariser. The gate is
/// re-entered on every tool iteration while a background summariser is in
/// flight, and the announcement used to sit above that dispatch, so session
/// `a58b8714` logged 128 gate WARNs. Across 2026-09-22 and 23 the ratio was
/// 241 gate hits against 27 compactions that actually applied: roughly nine
/// announcements, spawns and ledger rows per summariser.
/// Each repeat paid three times: a WARN line, a `tokio::spawn`, and a
/// feedback-ledger row asserting a compaction had started.
///
/// A `Failed` visit does announce. It falls back to the blocking path, which
/// is a second summariser rather than a repeat of the first.
pub(crate) fn gate_announces_compaction(state: &PendingState) -> bool {
    matches!(state, PendingState::Empty | PendingState::Failed)
}

/// The body of the gate WARN, extracted so tests can assert what actually
/// prints. The value is shown with one decimal and the trigger is stated as a
/// label: the integer format once turned a 65.4% fill into
/// "Context at 65% (>65%)", a sentence contradicting its own claim (#1733).
pub(crate) fn gate_warn_message(usage_pct: f64) -> String {
    format!(
        "Context at {:.1}% (threshold 65%) — triggering LLM compaction",
        usage_pct
    )
}

/// Fill level at which a turn stops running ahead of the summariser and waits
/// for it. Above this there is not enough headroom left to be confident the
/// next provider call fits, and blocking here is exactly what the code did
/// before compaction went to the background.
const BACKPRESSURE_CEILING_PCT: f64 = 80.0;

/// Whether this visit has to stop and wait for an in-flight summariser.
///
/// Waiting, never cancelling. Two reasons to stop running ahead of it:
/// a reply is about to be composed and must not be composed against a context
/// one swap away from replacement, or the context has climbed past the point
/// where the next provider call comfortably fits.
pub(crate) fn must_wait_for_compaction(phase: BudgetPhase, usage_pct: f64) -> bool {
    phase == BudgetPhase::TurnStart || usage_pct >= BACKPRESSURE_CEILING_PCT
}

impl AgentService {
    /// Enforce context budget with non-blocking compaction.
    ///
    /// Tier 1 — soft trigger at 65%: spawns an async LLM compaction task in
    /// the background and returns immediately. The agent keeps processing
    /// turns. Subsequent visits to this function check whether the spawned
    /// task has finished and atomically swap the summary in when it has.
    ///
    /// Tier 2 — hard floor at 90%: if context grows past 90% (because growth
    /// outran compaction or compaction failed), emergency truncation cuts
    /// older messages back to 80%. This path NEVER fails. It also cancels
    /// any in-flight async compaction so a stale snapshot summary cannot
    /// later overwrite the now-truncated context.
    ///
    /// NOTE: 65% (~130k of 200k) is chosen because MiniMax (and likely other
    /// providers) start returning `400 Prompt exceeds max length` well below
    /// the documented limit, around 75-80% in practice. 65% gives enough
    /// headroom to summarise without bumping into the actual ceiling.
    ///
    /// Returns `Some(outcome)` on any visit that changed the context, and the
    /// caller MUST persist `outcome.marker(..)`. That includes the truncation
    /// path: a context that shrank without a marker cannot be reloaded.
    pub(super) async fn enforce_context_budget(
        &self,
        session_id: Uuid,
        context: &mut AgentContext,
        model_name: &str,
        cancel_token: Option<&tokio_util::sync::CancellationToken>,
        progress_callback: &Option<ProgressCallback>,
        phase: BudgetPhase,
    ) -> Option<CompactionOutcome> {
        // Set by every branch that drops messages. Read at each of the three
        // exits: dropping messages without telling the DB is what turns a
        // recoverable overflow into a restart loop.
        let mut truncated = false;
        // Restored to the pre-0f052250 shape (the version that ran fine for
        // months before the async-compaction refactor). Logic, in order:
        //
        //   Tier 2 (90% hard floor): truncate to 80% first, then FALL THROUGH
        //     to Tier 1. Doing the truncation first means the compaction
        //     summarizer below sees ≤80% of the window — well within tokenizer
        //     headroom — so it doesn't hit `400 Prompt exceeds max length`
        //     and there's no failed-summarizer-then-truncate cascade.
        //
        //   Tier 1 (65% soft trigger): up to 3 sync compact_context attempts.
        //     If any succeed, summary lands and the marker gets persisted by
        //     the caller. If still over 65% target after success, re-compact
        //     once more with the now-tighter budget.
        //
        //   Safety net: only if all 3 attempts totally failed AND we're still
        //     above 80%, hard-truncate to 80%. This is the LAST RESORT — it
        //     drops messages without a summary marker, but only fires when
        //     the LLM compaction path is entirely unavailable.
        //
        // No async spawn/swap, no cancel-pending-on-90%, no per-call hard-
        // truncate fallback in the error arm — those were the additions that
        // produced the cascade-and-loop behaviour.
        let effective_max = context.max_tokens;
        // The provider's own count when we have one. Measuring against the
        // local estimate is what let a request sail past a 1M limit while
        // being reported as 66% full.
        let effective_tokens = context.effective_token_count();
        let usage_pct = if effective_max > 0 {
            (effective_tokens as f64 / effective_max as f64) * 100.0
        } else {
            100.0
        };

        tracing::trace!(
            "Context budget: {} tokens / {} max = {:.1}%",
            context.token_count,
            effective_max,
            usage_pct,
        );

        // ── Tier 2: 90% hard floor — truncate to 80%, then fall through to Tier 1 compaction ──
        if usage_pct >= 90.0 {
            tracing::warn!(
                "Context at {:.0}% ({} tokens) — hard truncating to 80%",
                usage_pct,
                context.token_count,
            );

            let target = (effective_max as f64 * 0.80) as usize;
            let before_len = context.messages.len();
            context.hard_truncate_to(target);
            context.trim_to_fit(0);
            truncated |= context.messages.len() < before_len;

            if let Some(cb) = progress_callback {
                cb(session_id, ProgressEvent::TokenCount(context.token_count));
            }

            tracing::info!(
                "Hard truncation complete: {} messages, {} tokens ({:.0}%)",
                context.messages.len(),
                context.token_count,
                effective_tokens as f64 / effective_max as f64 * 100.0,
            );

            let usage_pct_now = if effective_max > 0 {
                (context.effective_token_count() as f64 / effective_max as f64) * 100.0
            } else {
                100.0
            };
            tracing::debug!(
                "Post-truncation: {:.0}% — falling through to auto-compaction",
                usage_pct_now,
            );
        }

        // ── An in-flight summariser gets first refusal ──
        // Ahead of the 65% gate: a summary that finished on a previous visit
        // is free to apply, and a turn about to answer must not answer from a
        // context queued for replacement.
        let usage_pct = if effective_max > 0 {
            (context.effective_token_count() as f64 / effective_max as f64) * 100.0
        } else {
            100.0
        };
        let pending_state = self
            .resolve_pending_compaction(session_id, context, phase, usage_pct, progress_callback)
            .await;
        if let PendingState::Applied(applied_marker) = pending_state {
            return Some(CompactionOutcome::Summarised(applied_marker));
        }

        // ── Tier 1: soft trigger at 65% - LLM compaction ──
        let usage_pct = if effective_max > 0 {
            (context.effective_token_count() as f64 / effective_max as f64) * 100.0
        } else {
            100.0
        };
        if usage_pct <= 65.0 {
            // Below the compaction trigger. Try to emit the pre-compaction
            // pressure warning if usage is in the 55-64% band (#909), and
            // re-arm the throttle when usage has dropped below the floor.
            self.maybe_emit_pressure_warning(session_id, context, usage_pct);
            // Tier 2 may have already dropped messages to get us here. The
            // marker still has to land even though no summariser ran.
            return truncated.then_some(CompactionOutcome::Truncated);
        }

        // Announce only on a visit that actually summarises. This used to sit
        // above the dispatch, so every tool iteration that re-entered the gate
        // while a background summariser was in flight re-announced the same
        // compaction (#1686).
        if !gate_announces_compaction(&pending_state) {
            // Work is already under way against this exact conversation, or a
            // summary already landed. A second summariser would burn a provider
            // call describing a context the first one is about to replace.
            return truncated.then_some(CompactionOutcome::Truncated);
        }

        tracing::warn!("{}", gate_warn_message(usage_pct));
        self.record_provider_feedback(
            session_id,
            "context_compaction",
            model_name,
            Some(&format!("proactive_65pct tokens={}", context.token_count)),
        );

        match pending_state {
            // Nothing in flight and backgrounding is on: start the summariser
            // and let the turn carry on. This is the whole point — the user
            // sees the receipt afterwards instead of a minute of nothing.
            PendingState::Empty if self.background_compaction => {
                self.spawn_background_compaction(session_id, context, model_name, usage_pct);
                return truncated.then_some(CompactionOutcome::Truncated);
            }
            // Backgrounding off, or the background attempt already failed:
            // block the turn, exactly as every compaction did before.
            PendingState::Empty | PendingState::Failed => {}
            PendingState::StillRunning | PendingState::Applied(_) => {
                unreachable!("gate_announces_compaction returns false for both")
            }
        }

        // Signal channels that the next 10-60s will produce zero
        // streaming chunks so their typing-indicator pingers can keep
        // firing. Carries the fill level so channels can render a visible
        // "compacting" line (#29); the percentage is a LEVEL, not progress.
        // The Instant anchors the elapsed duration reported on the
        // CompactionSummary emit below.
        let compact_started = std::time::Instant::now();
        let before_pct = usage_pct;
        // E2 (#29): the ETA hint rides the event — the duration this
        // session's LAST successful compaction actually took. `None` on the
        // first compaction: no history, no prediction (a static guess is
        // never right — owner 2026-08-29).
        let predicted = self
            .last_compaction_elapsed
            .read()
            .ok()
            .and_then(|map| map.get(&session_id).copied());
        if let Some(cb) = progress_callback {
            cb(
                session_id,
                ProgressEvent::Compacting {
                    usage_pct,
                    predicted,
                },
            );
        }

        // Up to 3 attempts — transient summarizer errors (network blip,
        // tokenizer-edge 400) usually self-resolve on retry.
        let mut summary_result: Option<(String, String)> = None;
        const MAX_ATTEMPTS: u32 = 3;
        for attempt in 1..=MAX_ATTEMPTS {
            match self
                .compact_context(
                    session_id,
                    context,
                    model_name,
                    cancel_token,
                    CompactionNotifier::auto_from(session_id, progress_callback.as_ref()),
                )
                .await
            {
                Ok((summary, applied_marker)) => {
                    summary_result = Some((summary, applied_marker));
                    break;
                }
                Err(e) => {
                    tracing::error!(
                        "LLM compaction failed (attempt {}/{}): {}",
                        attempt,
                        MAX_ATTEMPTS,
                        e
                    );
                }
            }
        }

        // If still over the 65% target after a successful compaction, run one
        // more pass with the tighter post-summary budget.
        let target_tokens = (effective_max as f64 * 0.65) as usize;
        if context.token_count > target_tokens && summary_result.is_some() {
            tracing::warn!(
                "Still at {} tokens after compaction (target {}), re-compacting",
                context.token_count,
                target_tokens,
            );
            if let Ok((summary, applied_marker)) = self
                .compact_context(
                    session_id,
                    context,
                    model_name,
                    cancel_token,
                    CompactionNotifier::auto_from(session_id, progress_callback.as_ref()),
                )
                .await
            {
                summary_result = Some((summary, applied_marker));
            }
        }

        // Last resort: every compaction attempt failed AND we're still over
        // 80%. Truncate to keep the next request from going out at 200%+. No
        // marker is persisted in this branch; the caller sees None back.
        if summary_result.is_none() {
            let safety_target = (effective_max as f64 * 0.80) as usize;
            if context.token_count > safety_target {
                tracing::warn!(
                    "Compaction exhausted, context at {} tokens (>{:.0}%) — safety truncation to 80%",
                    context.token_count,
                    usage_pct,
                );
                let before_len = context.messages.len();
                context.hard_truncate_to(safety_target);
                context.trim_to_fit(0);
                truncated |= context.messages.len() < before_len;
            }
        }

        // Success: surface the outcome to channels (first-ever user-visible
        // compaction receipt, #29) and clear the #909 pressure throttle so
        // the settled ctx footer never wears the ❕ until usage climbs the
        // 55% floor again.
        if let Some((ref summary, _)) = summary_result {
            self.note_compaction_success(
                session_id,
                context,
                summary,
                before_pct,
                compact_started.elapsed(),
                progress_callback,
            );
        }

        // Emit the token count the NEXT request will start with.
        if let Some(cb) = progress_callback {
            if let Some((ref summary, _)) = summary_result {
                let marker_tokens = AgentContext::estimate_tokens(summary) + 100;
                let brain_tokens = self
                    .default_system_brain
                    .as_deref()
                    .map(AgentContext::estimate_tokens)
                    .unwrap_or(0);
                cb(
                    session_id,
                    ProgressEvent::TokenCount(marker_tokens + brain_tokens),
                );
            } else {
                cb(session_id, ProgressEvent::TokenCount(context.token_count));
            }
        }

        match summary_result {
            Some((_, applied_marker)) => Some(CompactionOutcome::Summarised(applied_marker)),
            None if truncated => Some(CompactionOutcome::Truncated),
            None => None,
        }
    }

    /// The two fill levels the waiting path reports (#1686).
    ///
    /// A background summariser is announced while the live context is still
    /// drifting down from Tier-2 trimming, so the live percentage and the
    /// percentage the gate requested are two different measurements of one
    /// event. Session `bee04b00` rendered a progress line reading 54% beside a
    /// receipt reading 66% -> 17%, and `cb1c7a07` rendered 21% beside 66% ->
    /// 16%: in both cases the progress figure sits under the 65% gate, so the
    /// line announced a compaction at a level that cannot have requested one.
    ///
    /// The progress line therefore always carries the requested level, which is
    /// the same `snapshot_usage_pct` the summary already reports as its
    /// "before". The live level stays in the log, where showing how far the
    /// context moved while the summariser ran is the whole point.
    pub(crate) fn waiting_report_levels(requested: f64, live: f64) -> (f64, f64) {
        (requested, live)
    }

    /// Bookkeeping every successful compaction owes, whichever path produced
    /// it: clear the #909 pressure throttle so the ctx footer drops its
    /// marker, remember how long this took so the next compaction can quote a
    /// real ETA instead of a guess, and hand channels the receipt (#29).
    fn note_compaction_success(
        &self,
        session_id: Uuid,
        context: &AgentContext,
        summary: &str,
        before_pct: f64,
        elapsed: std::time::Duration,
        progress_callback: &Option<ProgressCallback>,
    ) {
        if let Ok(mut map) = self.session_pressure_warned.write() {
            map.insert(session_id, false);
        }
        if let Ok(mut map) = self.last_compaction_elapsed.write() {
            map.insert(session_id, elapsed);
        }
        if let Some(cb) = progress_callback {
            // The same basis the gate measures on and the meter logs at
            // service/context.rs:1011. Dividing the raw `token_count` by
            // `max_tokens` here dropped the provider anchor, so a context the
            // log recorded at 17% was receipted at 19% (session bee04b00,
            // #1686): one compaction, two different after-numbers.
            let after_pct = context.usage_percentage();
            cb(
                session_id,
                ProgressEvent::CompactionSummary {
                    summary: summary.to_string(),
                    before_pct,
                    after_pct,
                    elapsed,
                },
            );
        }
    }

    /// The ETA hint for this session, grounded in its last observed
    /// compaction. `None` on the first one: no history, no prediction.
    fn predicted_compaction_elapsed(&self, session_id: Uuid) -> Option<std::time::Duration> {
        self.last_compaction_elapsed
            .read()
            .ok()
            .and_then(|map| map.get(&session_id).copied())
    }

    /// How long one summariser attempt gets before the chain walks on.
    ///
    /// Scaled from what this session's compactions actually cost, tripled, so
    /// a conversation that habitually takes four minutes is not declared hung
    /// at five. A session with no history has nothing to scale from and gets
    /// the floor, which is the per-request bound every HTTP provider already
    /// enforces.
    pub(super) fn compaction_attempt_deadline(&self, session_id: Uuid) -> std::time::Duration {
        self.predicted_compaction_elapsed(session_id)
            .map(|observed| observed * 3)
            .unwrap_or(AgentService::COMPACTION_ATTEMPT_FLOOR)
            .max(AgentService::COMPACTION_ATTEMPT_FLOOR)
    }

    /// Take the session's in-flight summariser out of the map.
    ///
    /// Taken rather than borrowed because awaiting it needs ownership, and
    /// holding a `std::sync::Mutex` across an await would poison every other
    /// session's budget check.
    pub(super) fn take_pending_compaction(&self, session_id: Uuid) -> Option<PendingCompaction> {
        self.pending_compactions
            .lock()
            .ok()
            .and_then(|mut map| map.remove(&session_id))
    }

    /// Decide what to do with an in-flight summariser, and swap it in if it is
    /// this visit's job to.
    async fn resolve_pending_compaction(
        &self,
        session_id: Uuid,
        context: &mut AgentContext,
        phase: BudgetPhase,
        usage_pct: f64,
        progress_callback: &Option<ProgressCallback>,
    ) -> PendingState {
        let Some(pending) = self.take_pending_compaction(session_id) else {
            return PendingState::Empty;
        };

        // Two reasons to stop running ahead of the summariser: the turn is
        // about to answer a user, or the context has climbed past the point
        // where the next provider call comfortably fits. Neither cancels it.
        if !pending.handle.is_finished() && !must_wait_for_compaction(phase, usage_pct) {
            if let Ok(mut map) = self.pending_compactions.lock() {
                map.insert(session_id, pending);
            }
            return PendingState::StillRunning;
        }

        if !pending.handle.is_finished() {
            // One event, two levels (#1686): the notice carries the fill the
            // compaction was requested at, the diagnostic keeps the live one
            // so the drift stays visible in the log where it belongs.
            let (notice_pct, log_pct) =
                Self::waiting_report_levels(pending.snapshot_usage_pct, usage_pct);
            tracing::warn!(
                "Waiting on background compaction at {:.0}% ({:?} elapsed) — {}",
                log_pct,
                pending.started.elapsed(),
                if phase == BudgetPhase::TurnStart {
                    "a reply must not be composed against a context queued for replacement"
                } else {
                    "context reached the back-pressure ceiling"
                },
            );
            // The only place backgrounded compaction is announced: here the
            // user really is waiting, and silence reads as a hang.
            if let Some(cb) = progress_callback {
                cb(
                    session_id,
                    ProgressEvent::Compacting {
                        usage_pct: notice_pct,
                        predicted: self.predicted_compaction_elapsed(session_id),
                    },
                );
            }
        }

        let PendingCompaction {
            handle,
            snapshot_len,
            scope,
            snapshot_usage_pct,
            started,
        } = pending;

        match handle.await {
            Ok(Ok(summary)) => {
                let applied_marker =
                    Self::apply_compaction_summary_after(context, scope, &summary, snapshot_len);
                self.note_compaction_success(
                    session_id,
                    context,
                    &summary,
                    snapshot_usage_pct,
                    started.elapsed(),
                    progress_callback,
                );
                if let Some(cb) = progress_callback {
                    cb(session_id, ProgressEvent::TokenCount(context.token_count));
                }
                PendingState::Applied(applied_marker)
            }
            Ok(Err(e)) => {
                tracing::error!(
                    "Background compaction failed after {:?}: {e}",
                    started.elapsed()
                );
                // A background summariser that dies used to die in the log
                // only; the user saw the context climb and nothing else
                // (#1521).
                if let Some(cb) = progress_callback {
                    cb(
                        session_id,
                        ProgressEvent::SelfHealingAlert {
                            message: format!("Background compaction failed: {e}"),
                        },
                    );
                }
                PendingState::Failed
            }
            Err(e) => {
                tracing::error!("Background compaction task did not finish: {e}");
                PendingState::Failed
            }
        }
    }

    /// Start a summariser against a snapshot and let the turn carry on.
    ///
    /// Nothing about the live context is touched here. The task reads a clone
    /// and the swap happens on a later visit, so the turn keeps streaming
    /// while the summary is written.
    fn spawn_background_compaction(
        &self,
        session_id: Uuid,
        context: &AgentContext,
        model_name: &str,
        usage_pct: f64,
    ) {
        let snapshot_len = context.messages.len();
        // #1649: scope-aware input. A session's first compaction summarises
        // the whole window; every later one summarises only the delta since
        // the last marker (frozen segments never re-enter the input), or the
        // segments themselves when they crowd the window past half.
        let (scope, messages, token_count) = context.compaction_input();
        tracing::info!(
            "Spawning background compaction at {:.0}% ({snapshot_len} messages, {scope:?} scope)",
            usage_pct,
        );

        let provider = self.provider_for_session(session_id);
        let fallbacks = self.fallback_chain_snapshot();
        let max_tokens = context.max_tokens;
        let model = model_name.to_string();
        let max_output = super::request_budget::compaction_summary_request_allowance();
        let working_dir = self.get_working_directory_for_session(session_id);
        let auto_approve = self.auto_approve_tools.load(Ordering::Relaxed);
        let subagents = self.subagent_manager.clone();
        let attempt_deadline = self.compaction_attempt_deadline(session_id);
        // #1960: read the live skill inventory BEFORE the spawn, so the
        // summariser is told which skills the session actually holds instead of
        // guessing from the transcript.
        let active_skills = self.skill_inventory_for_session(session_id);
        // Its own token: this task answers to session teardown, never to a
        // context that grew impatient.
        let cancel = tokio_util::sync::CancellationToken::new();

        let handle = tokio::spawn(async move {
            let summary = Self::compute_compaction_summary(
                provider,
                fallbacks,
                session_id,
                scope,
                messages,
                token_count,
                max_tokens,
                usage_pct,
                model,
                max_output,
                working_dir,
                auto_approve,
                active_skills,
                cancel,
                attempt_deadline,
                None,
            )
            .await?;
            Ok(Self::decorate_compaction_summary(summary, session_id, subagents).await)
        });

        if let Ok(mut map) = self.pending_compactions.lock() {
            map.insert(
                session_id,
                PendingCompaction {
                    handle,
                    snapshot_len,
                    scope,
                    snapshot_usage_pct: usage_pct,
                    started: std::time::Instant::now(),
                },
            );
        }
    }

    /// Pre-compaction pressure-warning gate (#909).
    ///
    /// Called from `enforce_context_budget` when usage is at or below 65% (i.e.
    /// compaction is NOT firing this turn). Decides whether to append the
    /// behavioural "persist your state" nudge to the system brain:
    ///
    /// - usage **below** the 55% floor: clear the per-session throttle flag so
    ///   the next entry into the band warns again. No nudge.
    /// - usage **in** the 55-64% band AND not yet emitted this entry: append the
    ///   nudge to `system_brain`, count its tokens, set the throttle flag.
    /// - usage **in** the band but already emitted: no-op (once-per-entry).
    ///
    /// The nudge is transient: `system_brain` is rebuilt from disk every turn
    /// (`live_system_brain_for_session`), so the suffix never persists to the
    /// DB and cannot compound across turns.
    pub(super) fn maybe_emit_pressure_warning(
        &self,
        session_id: Uuid,
        context: &mut AgentContext,
        usage_pct: f64,
    ) {
        use super::nudge::should_emit_pressure_warning;

        // Re-arm the throttle once usage drops below the band floor so the
        // next climb back into the band warns again.
        if usage_pct < super::nudge::PRESSURE_WARN_FLOOR {
            if let Ok(mut map) = self.session_pressure_warned.write() {
                map.insert(session_id, false);
            }
            return;
        }

        let already_emitted = self
            .session_pressure_warned
            .read()
            .ok()
            .and_then(|map| map.get(&session_id).copied())
            .unwrap_or(false);

        if let Some(warning) = should_emit_pressure_warning(usage_pct, already_emitted) {
            if let Some(ref mut brain) = context.system_brain {
                tracing::info!(
                    "Context at {:.0}% - emitting pre-compaction pressure warning",
                    usage_pct
                );
                brain.push_str(warning);
                context.token_count += AgentContext::estimate_tokens(warning);
            }
            // Mark emitted regardless of whether a brain existed, so a
            // missing brain doesn't retry every turn until the band ends.
            if let Ok(mut map) = self.session_pressure_warned.write() {
                map.insert(session_id, true);
            }
        }
        // else: in-band but already emitted -> no-op (once-per-entry).
    }

    /// True while the pre-compaction pressure nudge (#909, 55–64% band) is
    /// active for this session. The settled-flow ctx segment wears the quiet
    /// ❕ marker exactly when the hint is in the prompt (#29); the flag is
    /// cleared on compaction success and re-arms below the 55% floor.
    pub fn pressure_warning_active(&self, session_id: Uuid) -> bool {
        self.session_pressure_warned
            .read()
            .ok()
            .and_then(|map| map.get(&session_id).copied())
            .unwrap_or(false)
    }
}
