//! Pins the per-failure-class retry wait introduced for the prefill cut
//! (#3213).
//!
//! A request that was accepted but never started answering is a different
//! failure from a dropped stream or a rate limit: the body is queued and the
//! upstream is cold, so re-sending one second later pays the warm-up again
//! (receipt: 2026-11-22 session 00d3619d, 120s prefill wait, resend 1099ms
//! later). `stream_retry_backoff` therefore scales the wait to the budget
//! that expired for this class while every other class keeps the unchanged
//! transport ladder and any server `Retry-After` hint wins over both.
//!
//! These tests guard the policy numbers, not the plumbing: the retry arm in
//! the tool loop calls this function, so a silent change here changes how a
//! live turn waits.

use crate::brain::provider::error::{ProviderError, short_error_reason, stream_retry_backoff};
use std::time::Duration;

#[test]
fn prefill_cut_waits_a_quarter_of_the_budget_at_the_floor() {
    // 120s budget → a quarter is 30s, which is both the floor floor result
    // and the per-attempt cap.
    assert_eq!(
        stream_retry_backoff(&ProviderError::PrefillTimeout { budget_secs: 120 }, 1),
        Duration::from_secs(30)
    );
}

#[test]
fn prefill_cut_never_waits_less_than_ten_seconds() {
    // A short prefill budget (a cloud endpoint's 60s) would give 15s, but a
    // deliberately tiny budget must still not collapse to a sub-second retry.
    let wait = stream_retry_backoff(&ProviderError::PrefillTimeout { budget_secs: 4 }, 1);
    assert_eq!(wait, Duration::from_secs(10));
}

#[test]
fn prefill_cut_doubles_per_attempt_and_caps_at_sixty() {
    let err = ProviderError::PrefillTimeout { budget_secs: 120 };
    // 30s base → 30s, 60s, 60s (total cap), 60s, 60s.
    assert_eq!(stream_retry_backoff(&err, 1), Duration::from_secs(30));
    assert_eq!(stream_retry_backoff(&err, 2), Duration::from_secs(60));
    assert_eq!(stream_retry_backoff(&err, 3), Duration::from_secs(60));
    assert_eq!(stream_retry_backoff(&err, 5), Duration::from_secs(60));
}

#[test]
fn prefill_cut_per_attempt_never_exceeds_thirty_seconds() {
    // A very long endpoint budget (CLI's 600s) must not turn into a 150s
    // single sleep: the quarter is clamped to the per-attempt cap first.
    assert_eq!(
        stream_retry_backoff(&ProviderError::PrefillTimeout { budget_secs: 600 }, 1),
        Duration::from_secs(30)
    );
}

#[test]
fn transport_ladder_is_unchanged_for_other_classes() {
    // Dropped streams and 5xx keep 500ms → 1s → 2s → 4s → 8s, capped from
    // there so a long retry run cannot overflow the shift.
    let dropped = ProviderError::StreamError("connection closed".to_string());
    assert_eq!(
        stream_retry_backoff(&dropped, 1),
        Duration::from_millis(500)
    );
    assert_eq!(stream_retry_backoff(&dropped, 2), Duration::from_secs(1));
    assert_eq!(stream_retry_backoff(&dropped, 3), Duration::from_secs(2));
    assert_eq!(stream_retry_backoff(&dropped, 4), Duration::from_secs(4));
    assert_eq!(stream_retry_backoff(&dropped, 5), Duration::from_secs(8));
    assert_eq!(stream_retry_backoff(&dropped, 9), Duration::from_secs(8));

    // A handshake timeout stays on the same ladder rather than borrowing the
    // prefill scaling.
    assert_eq!(
        stream_retry_backoff(&ProviderError::Timeout(90), 1),
        Duration::from_millis(500)
    );
}

#[test]
fn declared_retry_after_wins_over_the_prefill_scaling() {
    // A server-declared wait short-circuits every ladder, per class. The
    // rate-limit hint is what the prefill scaling must never overwrite.
    let rate_limited = ProviderError::RateLimitExceeded("quota; retry after 7 seconds".to_string());
    assert_eq!(
        stream_retry_backoff(&rate_limited, 1),
        Duration::from_secs(7)
    );
}

#[test]
fn prefill_timeout_is_retryable_and_reasoned() {
    use crate::utils::retry::RetryableError;

    let err = ProviderError::PrefillTimeout { budget_secs: 120 };
    assert!(
        RetryableError::is_retryable(&err),
        "a prefill cut must reach the retry arm"
    );
    assert_eq!(short_error_reason(&err), "no first token after 120s");
    // No declared cooldown: the hint path must stay empty so the scaled
    // policy above is what the wait comes from.
    assert_eq!(RetryableError::retry_after(&err), None);
}
