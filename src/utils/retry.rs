//! Generic retry utilities with exponential backoff
//!
//! Provides a unified retry mechanism for both database and API operations.

use std::future::Future;
use std::time::Duration;
use tokio::time::sleep;

/// Trait for errors that can be classified as retryable
pub trait RetryableError: std::fmt::Display {
    /// Check if this error should be retried
    fn is_retryable(&self) -> bool;

    /// Optional: Extract Retry-After duration if available
    fn retry_after(&self) -> Option<Duration> {
        None
    }

    /// True when the error is a handshake timeout: a wall elapsed because the
    /// endpoint never answered at all, as opposed to an answer that refused the
    /// request (a 5xx, a 429, a dropped connection). Only this kind can trip
    /// [`RetryConfig::abort_on_consecutive_timeouts`], because repeating a
    /// send to an endpoint that has not answered once is just more waiting.
    fn is_handshake_timeout(&self) -> bool {
        false
    }
}

/// Universal retry configuration
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (0 means no retries)
    pub max_attempts: u32,
    /// Initial delay before first retry
    pub initial_delay: Duration,
    /// Maximum delay between retries
    pub max_delay: Duration,
    /// Backoff multiplier (typically 2.0 for exponential)
    pub backoff_multiplier: f64,
    /// Add random jitter to delays (0.0 = none, 0.1+ = recommended for distributed systems)
    pub jitter: f64,
    /// Stop the ladder after this many CONSECUTIVE handshake timeouts, even
    /// with attempts left. `None` (the default) always burns the full attempt
    /// budget.
    ///
    /// Only timeouts count, and only consecutive ones: a timeout followed by a
    /// 5xx resets the streak, because the endpoint did answer. This exists for
    /// endpoints where a timeout means "no answer at all", so the remaining
    /// attempts would repeat the same silence (#2232).
    pub abort_on_consecutive_timeouts: Option<u32>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            // Network/API default: 4 retries over ~15s (1s → 2s → 4s → 8s).
            // Kept in lockstep with `brain::provider::retry::RetryConfig`
            // default — the old 100ms initial was too aggressive to ride
            // out a transient blip and hammered rate-limited endpoints.
            // DB lock-contention retries use the `database()` preset
            // (50ms) instead, which is correct for local SQLite.
            max_attempts: 4,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            jitter: 0.1,
            abort_on_consecutive_timeouts: None,
        }
    }
}

/// In-place attempts a provider call gets before its caller walks the fallback
/// chain: the initial send plus `max_attempts` retries of the default ladder
/// (5 sends today).
///
/// User-facing retry counts derive from this instead of hardcoding a number: an
/// alert claimed "retried 3x in-place" long after the budget had moved on, and on
/// a path that retried zero times (#2270).
pub fn in_place_attempt_budget() -> u32 {
    RetryConfig::default().max_attempts + 1
}

impl RetryConfig {
    /// Create database-optimized config (high frequency, low latency, deterministic)
    pub fn database() -> Self {
        Self {
            max_attempts: 5,
            initial_delay: Duration::from_millis(50),
            max_delay: Duration::from_secs(5),
            backoff_multiplier: 2.0,
            jitter: 0.0, // Deterministic for database locks
            abort_on_consecutive_timeouts: None,
        }
    }

    /// Create aggressive database retry config
    pub fn database_aggressive() -> Self {
        Self {
            max_attempts: 10,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            backoff_multiplier: 1.5,
            jitter: 0.0,
            abort_on_consecutive_timeouts: None,
        }
    }

    /// Create API-optimized config (distributed backoff with jitter)
    pub fn api() -> Self {
        Self::default()
    }

    /// Create aggressive API retry config
    pub fn api_aggressive() -> Self {
        Self {
            max_attempts: 5,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff_multiplier: 2.0,
            jitter: 0.2,
            abort_on_consecutive_timeouts: None,
        }
    }

    /// The built-in OpenCrabs endpoint (#2232).
    ///
    /// Same patient budget as the default, with one exception: the ladder
    /// stops after two consecutive handshake timeouts. The endpoint is a proxy
    /// in front of upstream models, so a timeout there means it never answered
    /// at all. Five sends of the 120s endpoint budget would hold the user for
    /// ten minutes before the fallback chain even starts, and the remaining
    /// attempts would only repeat the same silence. A 5xx, a 429 or a dropped
    /// connection still gets every attempt, because those are answers.
    pub fn opencrabs_endpoint() -> Self {
        Self {
            abort_on_consecutive_timeouts: Some(2),
            ..Default::default()
        }
    }

    /// Create no-retry config
    pub fn no_retry() -> Self {
        Self {
            max_attempts: 0,
            ..Default::default()
        }
    }

    /// Qwen / OpenRouter-style tight-rate-limit-window retry: a 3s initial
    /// delay so we don't burn quota while the rate-limit window is closed.
    /// Backoff 3s → 6s → 12s → 24s. Used by the custom OpenAI-compatible
    /// provider for qwen-family models, which rate-limit on tight windows.
    pub fn qwen_cli_match() -> Self {
        Self {
            max_attempts: 4,
            initial_delay: Duration::from_secs(3),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            jitter: 0.2,
            abort_on_consecutive_timeouts: None,
        }
    }

    /// Calculate delay for a given attempt with optional jitter
    pub fn calculate_delay(&self, attempt: u32) -> Duration {
        let base_delay_ms = self.initial_delay.as_millis() as f64;
        let exponential = base_delay_ms * self.backoff_multiplier.powi(attempt as i32);
        let capped = exponential.min(self.max_delay.as_millis() as f64);

        let final_delay = if self.jitter > 0.0 {
            use rand::Rng;
            let mut rng = rand::rng();
            let jitter_factor = 1.0 + rng.random_range(-self.jitter..self.jitter);
            (capped * jitter_factor).max(0.0)
        } else {
            capped
        };

        Duration::from_millis(final_delay as u64)
    }
}

/// Generic retry function that works with any retryable error type
///
/// # Example
/// ```ignore
/// let config = RetryConfig::api();
/// let result = retry(|| async { make_api_call().await }, &config).await;
/// ```
pub async fn retry<F, Fut, T, E>(
    mut operation: F,
    config: &RetryConfig,
) -> std::result::Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, E>>,
    E: RetryableError,
{
    let mut attempt = 0;
    let mut last_error: Option<E> = None;
    let mut consecutive_timeouts: u32 = 0;

    loop {
        match operation().await {
            Ok(result) => {
                if attempt > 0 {
                    tracing::info!("Operation succeeded after {} retries", attempt);
                }
                return Ok(result);
            }
            Err(err) => {
                if config.max_attempts == 0 || !err.is_retryable() {
                    tracing::debug!("Error is not retryable: {}", err);
                    return Err(err);
                }

                // Every retryable error gets the full patient backoff. We do
                // NOT fast-fail DNS/connection errors: real providers recover
                // within the retry window (e.g. dialagram, ~98.8% uptime), and
                // bailing after one retry abandoned the user's chosen provider
                // for a transient blip. A genuinely dead host is bounded by the
                // fallback chain + sticky-fallback threshold instead — not by
                // giving up on the very first request.
                if attempt >= config.max_attempts {
                    tracing::warn!("Max retry attempts ({}) exceeded", config.max_attempts);
                    return Err(last_error.unwrap_or(err));
                }

                // A handshake timeout means the endpoint never answered at
                // all, so the attempts left would repeat the same silence.
                // Stop once the configured streak is reached (#2232).
                if err.is_handshake_timeout() {
                    consecutive_timeouts += 1;
                } else {
                    consecutive_timeouts = 0;
                }
                if let Some(limit) = config.abort_on_consecutive_timeouts
                    && consecutive_timeouts >= limit
                {
                    tracing::warn!(
                        "Stopping after {} consecutive handshake timeouts, {} of {} requests used: {}",
                        consecutive_timeouts,
                        attempt + 1,
                        config.max_attempts + 1,
                        err
                    );
                    return Err(err);
                }

                // Check for Retry-After hint from the error
                let delay = if let Some(retry_after) = err.retry_after() {
                    tracing::info!(
                        "Error provided retry_after hint: {}ms",
                        retry_after.as_millis()
                    );
                    retry_after
                } else {
                    config.calculate_delay(attempt)
                };

                tracing::info!(
                    "Retry attempt {}/{} after {}ms for error: {}",
                    attempt + 1,
                    config.max_attempts,
                    delay.as_millis(),
                    err
                );

                last_error = Some(err);
                sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

/// Like [`retry`], but invokes `on_retry(attempt, max, &err)` immediately
/// before each backoff sleep. The callback lets a caller surface retry
/// progress to a UI (e.g. the agent's `RetryAttempt` event) without the
/// retry loop knowing anything about UI plumbing.
///
/// `attempt` is 1-based (the upcoming retry number); `max` is
/// `config.max_attempts`. The callback fires only for retryable errors that
/// have attempts remaining — not on the final give-up or on non-retryable
/// errors.
pub async fn retry_with_notify<F, Fut, T, E, N>(
    mut operation: F,
    config: &RetryConfig,
    mut on_retry: N,
) -> std::result::Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, E>>,
    E: RetryableError,
    N: FnMut(u32, u32, &E),
{
    let mut attempt = 0;
    let mut last_error: Option<E> = None;
    let mut consecutive_timeouts: u32 = 0;

    loop {
        match operation().await {
            Ok(result) => {
                if attempt > 0 {
                    tracing::info!("Operation succeeded after {} retries", attempt);
                }
                return Ok(result);
            }
            Err(err) => {
                if config.max_attempts == 0 || !err.is_retryable() {
                    tracing::debug!("Error is not retryable: {}", err);
                    return Err(err);
                }
                // Every retryable error gets the full patient backoff. We do
                // NOT fast-fail DNS/connection errors: real providers recover
                // within the retry window (e.g. dialagram, ~98.8% uptime), and
                // bailing after one retry abandoned the user's chosen provider
                // for a transient blip. A genuinely dead host is bounded by the
                // fallback chain + sticky-fallback threshold instead — not by
                // giving up on the very first request.
                if attempt >= config.max_attempts {
                    tracing::warn!("Max retry attempts ({}) exceeded", config.max_attempts);
                    return Err(last_error.unwrap_or(err));
                }

                // A handshake timeout means the endpoint never answered at
                // all, so the attempts left would repeat the same silence.
                // Stop once the configured streak is reached (#2232).
                if err.is_handshake_timeout() {
                    consecutive_timeouts += 1;
                } else {
                    consecutive_timeouts = 0;
                }
                if let Some(limit) = config.abort_on_consecutive_timeouts
                    && consecutive_timeouts >= limit
                {
                    tracing::warn!(
                        "Stopping after {} consecutive handshake timeouts, {} of {} requests used: {}",
                        consecutive_timeouts,
                        attempt + 1,
                        config.max_attempts + 1,
                        err
                    );
                    return Err(err);
                }

                let delay = err
                    .retry_after()
                    .unwrap_or_else(|| config.calculate_delay(attempt));

                tracing::info!(
                    "Retry attempt {}/{} after {}ms for error: {}",
                    attempt + 1,
                    config.max_attempts,
                    delay.as_millis(),
                    err
                );
                // Surface the upcoming retry to the caller (UI, metrics, …).
                on_retry(attempt + 1, config.max_attempts, &err);

                last_error = Some(err);
                sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

/// Retry with a simple error display (for errors that don't implement RetryableError)
///
/// Uses a custom retryable check function.
pub async fn retry_with_check<F, Fut, T, E, C>(
    mut operation: F,
    config: &RetryConfig,
    is_retryable: C,
) -> std::result::Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, E>>,
    E: std::fmt::Display,
    C: Fn(&E) -> bool,
{
    let mut attempt = 0;
    let mut last_error: Option<E> = None;

    loop {
        match operation().await {
            Ok(result) => {
                if attempt > 0 {
                    tracing::info!("Operation succeeded after {} retries", attempt);
                }
                return Ok(result);
            }
            Err(err) => {
                if config.max_attempts == 0 || !is_retryable(&err) {
                    tracing::debug!("Error is not retryable: {}", err);
                    return Err(err);
                }

                if attempt >= config.max_attempts {
                    tracing::warn!("Max retry attempts ({}) exceeded", config.max_attempts);
                    return Err(last_error.unwrap_or(err));
                }

                let delay = config.calculate_delay(attempt);

                tracing::info!(
                    "Retry attempt {}/{} after {}ms for error: {}",
                    attempt + 1,
                    config.max_attempts,
                    delay.as_millis(),
                    err
                );

                last_error = Some(err);
                sleep(delay).await;
                attempt += 1;
            }
        }
    }
}
