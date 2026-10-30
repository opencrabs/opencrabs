//! Pins the `ProviderError: RetryableError` behaviour after retry
//! consolidation.
//!
//! `brain::provider::retry` was a near-duplicate of `utils::retry`. The
//! consolidation deleted the provider module and routed every provider
//! retry through `utils::retry::retry`, which is generic over
//! `RetryableError`. For that to be behaviour-preserving, `ProviderError`
//! must implement the trait so that:
//!   - `is_retryable()` matches the inherent classifier (transient HTTP /
//!     5xx / rate-limit retry; client 4xx do not), and
//!   - `retry_after()` extracts a server Retry-After hint from rate-limit
//!     errors, clamped to 30s so a pathological "retry after 300s" can't
//!     stall a turn.
//!
//! These tests guard the moved Retry-After parsing and the trait wiring so
//! a future change can't silently break provider retry/fallback timing.

use crate::brain::provider::ProviderError;
use crate::utils::retry::RetryableError;
use std::time::Duration;

#[test]
fn retryable_classification_matches_inherent() {
    // Transient kinds retry.
    assert!(RetryableError::is_retryable(&ProviderError::Timeout(10)));
    assert!(RetryableError::is_retryable(
        &ProviderError::RateLimitExceeded("slow down".to_string())
    ));
    assert!(RetryableError::is_retryable(&ProviderError::ApiError {
        status: 503,
        message: "upstream unavailable".to_string(),
        error_type: None,
    }));

    // Client errors do not retry.
    assert!(!RetryableError::is_retryable(&ProviderError::InvalidApiKey));
    assert!(!RetryableError::is_retryable(&ProviderError::ApiError {
        status: 400,
        message: "Invalid model id: foo".to_string(),
        error_type: Some("invalid_request_error".to_string()),
    }));

    // Trait and inherent classifiers must agree.
    let e = ProviderError::Timeout(5);
    assert_eq!(RetryableError::is_retryable(&e), e.is_retryable());
}

#[test]
fn retry_after_parses_rate_limit_hint() {
    let e = ProviderError::RateLimitExceeded("retry in 12 seconds".to_string());
    assert_eq!(e.retry_after(), Some(Duration::from_secs(12)));

    let e = ProviderError::ApiError {
        status: 429,
        message: "Too many requests, wait 5s".to_string(),
        error_type: Some("rate_limit".to_string()),
    };
    assert_eq!(e.retry_after(), Some(Duration::from_secs(5)));
}

#[test]
fn retry_after_clamps_to_30s() {
    // A provider asking for an absurd wait must not stall the turn.
    let e = ProviderError::RateLimitExceeded("retry in 300 seconds".to_string());
    assert_eq!(
        e.retry_after(),
        Some(Duration::from_secs(30)),
        "Retry-After hints must be clamped to 30s"
    );
}

#[test]
fn retry_after_none_for_non_rate_limit() {
    // No hint on non-rate-limit errors — caller uses the exponential schedule.
    assert_eq!(ProviderError::Timeout(10).retry_after(), None);
    assert_eq!(ProviderError::InvalidApiKey.retry_after(), None);
    assert_eq!(
        ProviderError::ApiError {
            status: 500,
            message: "boom".to_string(),
            error_type: None,
        }
        .retry_after(),
        None
    );
}

#[test]
fn retry_after_none_when_no_parseable_number() {
    // Rate-limit error with no parseable duration → None (fall back to backoff).
    let e = ProviderError::RateLimitExceeded("you are being rate limited".to_string());
    assert_eq!(e.retry_after(), None);
}

// A minimal retryable test error. The hard-down fast-fail was removed
// (2026-06-08): EVERY retryable error — DNS/connection failures included —
// now gets the full patient budget. Providers like dialagram (~98.8%
// uptime) recover within the retry window, so abandoning them after one
// retry was wrong; a genuinely dead host is bounded by the fallback chain +
// sticky-fallback threshold instead, not by giving up on the first request.
#[derive(Debug)]
struct ClassError;
impl std::fmt::Display for ClassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "class error")
    }
}
impl RetryableError for ClassError {
    fn is_retryable(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn every_retryable_error_uses_the_full_patient_budget() {
    // No error kind is fast-failed any more — DNS/connection blips included.
    use crate::utils::retry::{RetryConfig, retry};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let cfg = RetryConfig {
        max_attempts: 4,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
        backoff_multiplier: 2.0,
        jitter: 0.0,
    };

    let calls = Arc::new(AtomicU32::new(0));
    let c2 = calls.clone();
    let out: Result<i32, ClassError> = retry(
        move || {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(ClassError)
            }
        },
        &cfg,
    )
    .await;

    assert!(out.is_err());
    // 1 initial try + 4 retries = full patient budget; nothing is capped.
    assert_eq!(
        calls.load(Ordering::SeqCst),
        5,
        "all retryable errors must use the full 4-retry patient budget"
    );
}

#[tokio::test]
async fn retry_with_notify_fires_per_attempt_for_surfacing() {
    // The retry-visibility feature depends on retry_with_notify calling the
    // notifier once per retry with the right (attempt, max). This is what
    // feeds the TUI "⏳ Retry N/M" — pin it so a refactor can't silently
    // stop surfacing retries (the exact bug the user reported).
    use crate::utils::retry::{RetryConfig, retry_with_notify};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    let cfg = RetryConfig {
        max_attempts: 4,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
        backoff_multiplier: 2.0,
        jitter: 0.0,
    };

    // Always-failing transient error → exhausts all 4 retries.
    let notices: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let n2 = notices.clone();
    let calls = Arc::new(AtomicU32::new(0));
    let c2 = calls.clone();
    let out: Result<i32, ProviderError> = retry_with_notify(
        move || {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(ProviderError::Timeout(1))
            }
        },
        &cfg,
        |attempt, max, _err| {
            n2.lock().unwrap().push((attempt, max));
        },
    )
    .await;

    assert!(out.is_err());
    // 4 retries notified (the final give-up is not a retry), attempts 1..=4.
    let recorded = notices.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![(1, 4), (2, 4), (3, 4), (4, 4)],
        "notifier must fire once per retry with 1-based attempt and the max"
    );
    // 1 initial + 4 retries = 5 operation calls.
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn retry_with_notify_does_not_fire_on_success_or_non_retryable() {
    use crate::utils::retry::{RetryConfig, retry_with_notify};
    use std::sync::{Arc, Mutex};

    let cfg = RetryConfig {
        max_attempts: 3,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
        backoff_multiplier: 2.0,
        jitter: 0.0,
    };

    // Immediate success → no notices.
    let fired = Arc::new(Mutex::new(0u32));
    let f2 = fired.clone();
    let _: Result<i32, ProviderError> =
        retry_with_notify(|| async { Ok(1) }, &cfg, |_, _, _| *f2.lock().unwrap() += 1).await;
    assert_eq!(*fired.lock().unwrap(), 0, "no retries on success");

    // Non-retryable → no notices.
    let fired = Arc::new(Mutex::new(0u32));
    let f2 = fired.clone();
    let _: Result<i32, ProviderError> = retry_with_notify(
        || async { Err(ProviderError::InvalidApiKey) },
        &cfg,
        |_, _, _| *f2.lock().unwrap() += 1,
    )
    .await;
    assert_eq!(
        *fired.lock().unwrap(),
        0,
        "non-retryable errors must not notify"
    );
}

#[tokio::test]
async fn provider_error_drives_generic_retry() {
    // End-to-end: a ProviderError flowing through utils::retry::retry must
    // retry transient errors and stop on non-retryable ones — proving the
    // trait wiring is what the consolidated provider path relies on.
    use crate::utils::retry::{RetryConfig, retry};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    // Fast config so the test doesn't wait the real 1s+ schedule.
    let cfg = RetryConfig {
        max_attempts: 3,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
        backoff_multiplier: 2.0,
        jitter: 0.0,
    };

    // Transient: fails twice then succeeds.
    let count = Arc::new(AtomicU32::new(0));
    let c2 = count.clone();
    let out: Result<i32, ProviderError> = retry(
        move || {
            let c = c2.clone();
            async move {
                if c.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err(ProviderError::Timeout(1))
                } else {
                    Ok(7)
                }
            }
        },
        &cfg,
    )
    .await;
    assert_eq!(out.unwrap(), 7);
    assert_eq!(
        count.load(Ordering::SeqCst),
        3,
        "should retry twice then succeed"
    );

    // Non-retryable: fails once, no retries.
    let count = Arc::new(AtomicU32::new(0));
    let c2 = count.clone();
    let out: Result<i32, ProviderError> = retry(
        move || {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(ProviderError::InvalidApiKey)
            }
        },
        &cfg,
    )
    .await;
    assert!(out.is_err());
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "non-retryable must not retry"
    );
}

// ---------------------------------------------------------------------------
// Full attempt budget on every endpoint (#2277).
//
// The owner directive: the primary provider always rides the full 5 sends.
// The proxy rotates upstreams between retries, so a handshake timeout is not
// evidence that the next send repeats the silence; the #2232 stop-after-two
// breaker was removed together with its config field. A timeout is still a
// retryable transient like any other: 5xx, 429, stream drop and timeout all
// burn the whole ladder before the fallback chain gets a turn.
// ---------------------------------------------------------------------------

/// The opencrabs endpoint keeps the patient default ladder.
#[test]
fn opencrabs_endpoint_profile_is_the_patient_default() {
    use crate::brain::provider::custom_openai_compatible::select_retry_config;
    use crate::utils::retry::RetryConfig;

    let default = RetryConfig::default();
    for provider_name in ["opencrabs", "my-copy"] {
        let cfg = select_retry_config(None, provider_name, false, "deepseek-v4.1-flash");
        assert_eq!(cfg.max_attempts, default.max_attempts);
        assert_eq!(cfg.initial_delay, default.initial_delay);
        assert_eq!(cfg.max_delay, default.max_delay);
        assert_eq!(cfg.backoff_multiplier, default.backoff_multiplier);
        assert_eq!(cfg.jitter, default.jitter);
    }

    // An explicit override still wins over the built-in default.
    let forced = select_retry_config(
        Some(&RetryConfig::api_aggressive()),
        "opencrabs",
        false,
        "deepseek-v4.1-flash",
    );
    assert_eq!(
        forced.max_attempts,
        RetryConfig::api_aggressive().max_attempts
    );
}

/// Two consecutive handshake timeouts do NOT stop the ladder: the proxy
/// rotates upstreams between retries, so the next send can land on a healthy
/// upstream. The regression from #2277: the ladder aborted on the second
/// consecutive timeout and walked to the fallback after 2 of 5 sends.
#[tokio::test]
async fn two_consecutive_timeouts_do_not_stop_the_ladder() {
    use crate::utils::retry::{RetryConfig, retry};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let cfg = RetryConfig {
        max_attempts: 4,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
        backoff_multiplier: 2.0,
        jitter: 0.0,
    };

    let calls = Arc::new(AtomicU32::new(0));
    let c2 = calls.clone();
    let out: Result<i32, ProviderError> = retry(
        move || {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(ProviderError::Timeout(120))
            }
        },
        &cfg,
    )
    .await;

    assert!(out.is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        5,
        "timeouts are transients like any other: the ladder must burn all 1 + 4 sends"
    );
}

/// A 5xx keeps every attempt too.
#[tokio::test]
async fn a_5xx_keeps_every_attempt() {
    use crate::utils::retry::{RetryConfig, retry};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let cfg = RetryConfig {
        max_attempts: 4,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
        backoff_multiplier: 2.0,
        jitter: 0.0,
    };

    let calls = Arc::new(AtomicU32::new(0));
    let c2 = calls.clone();
    let out: Result<i32, ProviderError> = retry(
        move || {
            let c = c2.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(ProviderError::ApiError {
                    status: 503,
                    message: "upstream unavailable".to_string(),
                    error_type: None,
                })
            }
        },
        &cfg,
    )
    .await;

    assert!(out.is_err());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        5,
        "a 5xx is an answer, so it keeps the full 1 + 4 attempts"
    );
}

// ---------------------------------------------------------------------------
// Provider-level selection.
//
// `retry_config()` is what every request path on the [OI] client reads, so
// the profile has to be selected there. The override wins over everything.
// ---------------------------------------------------------------------------

/// Every endpoint without an override gets its named profile, and the
/// override beats them all.
#[test]
fn the_retry_profile_is_selected_by_provider_name_and_override() {
    use crate::brain::provider::custom_openai_compatible::select_retry_config;
    use crate::utils::retry::RetryConfig;

    // Qwen and OpenRouter keep the tight 429-window profile.
    let qwen = select_retry_config(None, "qwen", false, "qwen-max");
    assert_eq!(
        qwen.max_attempts,
        RetryConfig::qwen_cli_match().max_attempts
    );

    // Every other cloud provider keeps the default attempt budget.
    let other = select_retry_config(None, "openai", false, "gpt-4o");
    assert_eq!(other.max_attempts, 4);

    // An explicit override wins over the named profile.
    let forced = select_retry_config(
        Some(&RetryConfig::api_aggressive()),
        "qwen",
        false,
        "qwen-max",
    );
    assert_eq!(
        forced.max_attempts,
        RetryConfig::api_aggressive().max_attempts
    );
}
