//! How fast a turn streamed, and when a measurement is too coarse to trust.
//!
//! Both surfaces print a tokens-per-second figure: the terminal footer and the
//! window footer. They used to compute it in two places, so a rule learned on
//! one side never reached the other (#2703). This module holds the rule once.
//!
//! The arithmetic is trivial; the guard is the point. Burst-delivering
//! providers dump a whole response in one chunk, and a UI that stamps chunks
//! when it drains them rather than when they arrived measures a window of a
//! few milliseconds, which divides into a rate no model produces. The service
//! has refused those numbers since 2026-06-06; the surfaces now refuse them
//! with the same floor and ceiling instead of each inventing their own.

/// Below this, the timing is too coarse to represent a generation rate.
///
/// A burst-delivered response can land in a single sub-second chunk, making the
/// active window ~8ms and the computed rate physically impossible (37203 tok/s
/// observed 2026-06-06). Nothing is reported in that case.
pub const MIN_ACTIVE_SECS: f64 = 0.3;

/// Upper bound on a believable streaming rate.
///
/// Frontier models stream at tens to low hundreds of tok/s; the fastest
/// specialized inference tops out around 1 to 2k. A computed rate above this
/// is a network-burst artifact, so it is dropped rather than shown.
pub const MAX_PLAUSIBLE_TOK_S: f64 = 2000.0;

/// The rate for a turn, or `None` when the measurement is not credible.
///
/// `tokens` is the turn's output tokens (reasoning included: it is model
/// output). `active_secs` is the summed streaming time, with idle gaps and tool
/// waits already excluded, so a long tool run cannot drag the number down.
///
/// Returns `None` when there are no tokens, when the window is under
/// [`MIN_ACTIVE_SECS`], or when the result is over [`MAX_PLAUSIBLE_TOK_S`].
/// A caller that gets `None` keeps whatever it was already showing; a fresh
/// zero would be a worse lie than a stale truth.
pub fn credible_rate(tokens: u32, active_secs: f64) -> Option<f64> {
    if tokens == 0 || !active_secs.is_finite() || active_secs < MIN_ACTIVE_SECS {
        return None;
    }
    let rate = f64::from(tokens) / active_secs;
    if rate.is_finite() && rate <= MAX_PLAUSIBLE_TOK_S {
        Some(rate)
    } else {
        None
    }
}

/// Live tok/s accumulator, shared by both surfaces (#2703).
///
/// Tracks ACTIVE streaming time (windows where token deltas arrive
/// continuously) and stashes the last finalized rate, so a footer keeps
/// showing the previous turn's tok/s during idle until the next turn
/// produces its first token. Tool execution waits, approval prompts and
/// between-stream network round-trips are excluded: anything more than a
/// second without a token closes the current window.
///
/// 2026-05-28 user report: before this tracker the footer divided streaming
/// tokens by full wall-clock elapsed, so a 30s tool exec turned a 200 tok/s
/// burst into "8 tok/s" mid-turn.
///
/// This used to exist twice, once in `tui::app::state` and once inside the
/// window's live turn. Two copies of a timing rule drift, and the window's
/// copy read the clock when it drained a batch rather than when the chunks
/// arrived, so a burst measured a window of milliseconds and 4640 tok/s
/// reached the status bar (#2703).
#[derive(Debug, Clone, Default)]
pub struct StreamingTpsTracker {
    /// Accumulated active streaming seconds for the current turn.
    accumulated_secs: f64,
    /// Instant the current open window started.
    window_start: Option<std::time::Instant>,
    /// Instant of the most recent advance call, which is where the open
    /// window ends.
    last_token_at: Option<std::time::Instant>,
    /// Last finalized tok/s value. Persists across idle until the next
    /// finalize call, so an idle footer keeps a number instead of blanking.
    pub last_tps: Option<f64>,
}

impl StreamingTpsTracker {
    /// A gap longer than this between consecutive token events closes the
    /// current active window. Tool execution, approval waits and
    /// between-response network round-trips all exceed it, so they are
    /// correctly excluded from the rate denominator.
    const IDLE_GAP_SECS: f64 = 1.0;

    /// Call on every token event after incrementing the counter.
    ///
    /// `now` is the instant the chunk ARRIVED, not the instant the UI got
    /// round to processing it. A UI that folds a queued batch must pass one
    /// stamp per chunk: stamping the whole batch with a single clock read
    /// collapses the window and invents a rate (#2703). The clock is a
    /// parameter so tests can drive it deterministically.
    pub fn advance(&mut self, now: std::time::Instant) {
        match (self.window_start, self.last_token_at) {
            (None, _) => {
                // First token this turn: open a window starting now.
                self.window_start = Some(now);
            }
            (Some(start), Some(last)) => {
                if (now - last).as_secs_f64() > Self::IDLE_GAP_SECS {
                    // Idle gap: close the prior window and open a new one.
                    self.accumulated_secs += (last - start).as_secs_f64();
                    self.window_start = Some(now);
                }
                // else: still inside the active window.
            }
            (Some(_), None) => {
                // A start without a last token should not happen, but reset
                // rather than measure from a stale window.
                self.window_start = Some(now);
            }
        }
        self.last_token_at = Some(now);
    }

    /// Total active seconds so far, including the currently open window.
    ///
    /// Window time is always measured between window start and the LAST
    /// token event, never extended to now, so idle ticks past the last token
    /// cannot inflate the rate.
    pub fn active_secs(&self) -> f64 {
        let in_flight = match (self.window_start, self.last_token_at) {
            (Some(start), Some(last)) => (last - start).as_secs_f64().max(0.0),
            _ => 0.0,
        };
        self.accumulated_secs + in_flight
    }

    /// The same number, with the clock as an explicit argument for callers
    /// that already hold one.
    pub fn active_secs_now(&self, _now: std::time::Instant) -> f64 {
        self.active_secs()
    }

    /// Stash the just-finished turn's rate as `last_tps` and reset the
    /// accumulator for the next turn. A turn with zero tokens leaves
    /// `last_tps` untouched, preserving the rate already on screen.
    ///
    /// `authoritative` is the agent service's own `tokens_per_second`:
    /// provider-reported output tokens over summed active streaming time.
    /// When present it overrides the local estimate, because tiktoken's
    /// cl100k_base over-counts Qwen/Kimi/GLM bytes by roughly 1.5 to 2x and
    /// the local window only covers the visible final message. When absent
    /// (CLI providers, or a turn the streaming layer could not measure) the
    /// local estimate is used, and it is refused by [`credible_rate`] if the
    /// window is too coarse to mean anything.
    pub fn finalize(&mut self, total_tokens: u32, authoritative: Option<f64>) {
        let active = self.active_secs();
        if let Some(tps) = authoritative.filter(|t| t.is_finite() && *t > 0.0) {
            self.last_tps = Some(tps);
        } else if let Some(rate) = credible_rate(total_tokens, active) {
            self.last_tps = Some(rate);
        }
        self.reset_window();
    }

    /// Drop the current turn's window and keep the last finalized rate, so a
    /// fresh turn starts measuring from zero without blanking the footer.
    pub fn reset_window(&mut self) {
        self.accumulated_secs = 0.0;
        self.window_start = None;
        self.last_token_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_normal_turn_reports_its_rate() {
        // 1800 tokens over 9s is 200 tok/s, a plausible frontier rate.
        let rate = credible_rate(1800, 9.0).expect("a plain turn must measure");
        assert!((rate - 200.0).abs() < 0.001, "got {rate}");
    }

    #[test]
    fn a_collapsed_window_is_refused() {
        // The #2703 receipt: 18382 tokens beside a 3.96s window is 4640 tok/s,
        // which is over the ceiling, so no surface may print it.
        assert_eq!(credible_rate(18382, 3.96), None);
        // A burst that lands in milliseconds is under the floor instead.
        assert_eq!(credible_rate(120, 0.008), None);
    }

    #[test]
    fn a_turn_with_no_tokens_measures_nothing() {
        assert_eq!(credible_rate(0, 30.0), None);
        assert_eq!(credible_rate(500, 0.0), None);
        assert_eq!(credible_rate(500, f64::NAN), None);
    }

    #[test]
    fn the_boundaries_are_inclusive_at_the_ceiling_and_exclusive_at_the_floor() {
        // Exactly at the ceiling is believable; the floor is a strict minimum.
        assert_eq!(credible_rate(2000, 1.0), Some(2000.0));
        assert_eq!(credible_rate(2001, 1.0), None);
        assert_eq!(credible_rate(300, MIN_ACTIVE_SECS), Some(1000.0));
        assert_eq!(credible_rate(300, MIN_ACTIVE_SECS - 0.001), None);
    }

    /// The terminal footer runs on this rule, not on a copy of it (#2703).
    ///
    /// The terminal reads its clock per event, so its window is usually real.
    /// What it never had was a ceiling: a burst provider could still divide a
    /// whole response into a handful of milliseconds and print the result.
    #[test]
    fn the_shared_tracker_refuses_a_collapsed_window() {
        let base = std::time::Instant::now();

        // A turn that streamed: one token event every 100ms for four seconds.
        let mut tracker = StreamingTpsTracker::default();
        for i in 0..40u64 {
            tracker.advance(base + std::time::Duration::from_millis(i * 100));
        }
        tracker.finalize(240, None);
        let credible = tracker.last_tps.expect("a turn that streamed must measure");
        assert!((credible - 240.0 / 3.9).abs() < 0.01, "got {credible}");

        // The next turn arrives as one burst: 2400 tokens inside 100ms.
        for i in 0..400u64 {
            tracker.advance(
                base + std::time::Duration::from_secs(30)
                    + std::time::Duration::from_micros(i * 250),
            );
        }
        tracker.finalize(2400, None);
        assert_eq!(
            tracker.last_tps,
            Some(credible),
            "a collapsed window must keep the rate already on the footer"
        );
    }
}
