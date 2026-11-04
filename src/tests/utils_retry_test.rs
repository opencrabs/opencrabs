use crate::utils::retry::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

#[derive(Debug)]
struct TestError {
    retryable: bool,
    message: String,
}

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl RetryableError for TestError {
    fn is_retryable(&self) -> bool {
        self.retryable
    }
}

#[tokio::test]
async fn test_successful_operation_no_retry() {
    let config = RetryConfig::default();
    let result: Result<i32, TestError> = retry(|| async { Ok(42) }, &config).await;
    assert_eq!(result.unwrap(), 42);
}

#[tokio::test]
async fn test_non_retryable_error_fails_immediately() {
    let config = RetryConfig::default();
    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    let result: Result<i32, TestError> = retry(
        || {
            let count = call_count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Err(TestError {
                    retryable: false,
                    message: "permanent error".into(),
                })
            }
        },
        &config,
    )
    .await;

    assert!(result.is_err());
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_retryable_error_retries() {
    let config = RetryConfig {
        max_attempts: 3,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(10),
        backoff_multiplier: 2.0,
        jitter: 0.0,
    };

    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    let result: Result<i32, TestError> = retry(
        || {
            let count = call_count_clone.clone();
            async move {
                let current = count.fetch_add(1, Ordering::SeqCst);
                if current < 2 {
                    Err(TestError {
                        retryable: true,
                        message: "temporary error".into(),
                    })
                } else {
                    Ok(42)
                }
            }
        },
        &config,
    )
    .await;

    assert_eq!(result.unwrap(), 42);
    assert_eq!(call_count.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_max_attempts_exceeded() {
    let config = RetryConfig {
        max_attempts: 2,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(10),
        backoff_multiplier: 2.0,
        jitter: 0.0,
    };

    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    let result: Result<i32, TestError> = retry(
        || {
            let count = call_count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Err(TestError {
                    retryable: true,
                    message: "always fails".into(),
                })
            }
        },
        &config,
    )
    .await;

    assert!(result.is_err());
    assert_eq!(call_count.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn test_no_retry_config() {
    let config = RetryConfig::no_retry();
    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = call_count.clone();

    let result: Result<i32, TestError> = retry(
        || {
            let count = call_count_clone.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Err(TestError {
                    retryable: true,
                    message: "error".into(),
                })
            }
        },
        &config,
    )
    .await;

    assert!(result.is_err());
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[test]
fn test_calculate_delay_exponential() {
    let config = RetryConfig {
        initial_delay: Duration::from_millis(100),
        max_delay: Duration::from_secs(10),
        backoff_multiplier: 2.0,
        jitter: 0.0,
        ..Default::default()
    };

    assert_eq!(config.calculate_delay(0), Duration::from_millis(100));
    assert_eq!(config.calculate_delay(1), Duration::from_millis(200));
    assert_eq!(config.calculate_delay(2), Duration::from_millis(400));
    assert_eq!(config.calculate_delay(3), Duration::from_millis(800));
}

#[test]
fn test_calculate_delay_capped() {
    let config = RetryConfig {
        initial_delay: Duration::from_millis(100),
        max_delay: Duration::from_millis(500),
        backoff_multiplier: 2.0,
        jitter: 0.0,
        ..Default::default()
    };

    assert_eq!(config.calculate_delay(0), Duration::from_millis(100));
    assert_eq!(config.calculate_delay(1), Duration::from_millis(200));
    assert_eq!(config.calculate_delay(2), Duration::from_millis(400));
    assert_eq!(config.calculate_delay(3), Duration::from_millis(500));
    assert_eq!(config.calculate_delay(10), Duration::from_millis(500));
}

#[test]
fn test_preset_configs() {
    let db = RetryConfig::database();
    assert_eq!(db.max_attempts, 5);
    assert_eq!(db.initial_delay, Duration::from_millis(50));
    assert_eq!(db.jitter, 0.0);

    let api = RetryConfig::api();
    assert_eq!(api.max_attempts, 4);
    assert_eq!(api.initial_delay, Duration::from_secs(1));
    assert_eq!(api.jitter, 0.1);

    let no_retry = RetryConfig::no_retry();
    assert_eq!(no_retry.max_attempts, 0);
}

#[test]
fn in_place_retry_attempts_track_the_retry_config() {
    // The user-facing retry count must track the ladder, never a literal
    // (#2270): the June alert said "retried 3x in-place" while the budget was
    // already 5 sends, on a path that retried zero times.
    let cfg = RetryConfig::default();
    assert_eq!(in_place_attempt_budget(), cfg.max_attempts + 1);
    assert_eq!(in_place_attempt_budget(), 5);
}
