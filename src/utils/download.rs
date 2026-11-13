//! Inbound media downloads, done once instead of eleven times (#2261).
//!
//! Every channel used to call `reqwest::get(url)` inline, which has three
//! problems the copies shared: `Ok` means the request completed, not that it
//! succeeded, so a 403 or an expired `file_path` body was handed downstream as
//! if it were the user's file; the client carries no connect or read timeout,
//! so a stalled body hangs the task forever; and nothing retried, although the
//! platform URLs are generated per request and a retry is exactly the fix for
//! a transient one.
//!
//! Wrong content is worse than a failure here: a truncated or HTML error body
//! reaching a vision or document path produces a confident wrong answer. So the
//! status is checked before a single byte is read, and a permanent failure (a
//! 4xx other than a rate limit) is never retried.

use std::fmt;
use std::time::Duration;

use reqwest::StatusCode;

use super::retry::{RetryConfig, retry_with_check};

/// Connect timeout: short, because a dead host should fail fast and let the
/// retry loop try again rather than sit in a half-open socket.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole-request timeout including the body read. Generous: a 20 MB file on a
/// slow link is legitimate, an infinite hang is not.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// One shared client, built once. A client per download throws away the
/// connection pool and re-resolves DNS every time.
fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            // A TLS backend failure here would break the whole process, and the
            // default client still beats no client: keep going with the error
            // visible in the logs.
            .unwrap_or_else(|e| {
                tracing::error!("inbound download: client build failed ({e:#}), using default");
                reqwest::Client::new()
            })
    })
}

/// Why a download failed, kept structured so the caller can decide whether to
/// retry and what to tell the user.
#[derive(Debug)]
pub enum DownloadError {
    /// Transport level: DNS, TLS, connection reset, timeout. Always retryable.
    Network(String),
    /// A non-success status. Retryable only for 408, 429 and 5xx.
    Status(StatusCode),
}

impl DownloadError {
    /// A transient failure is worth another attempt. A 403 on an expired
    /// `file_path` is not: the URL is dead and hammering it costs bandwidth for
    /// a certainty.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Network(_) => true,
            Self::Status(status) => {
                status.is_server_error()
                    || *status == StatusCode::REQUEST_TIMEOUT
                    || *status == StatusCode::TOO_MANY_REQUESTS
            }
        }
    }

    /// Human readable, and true: says the network or the platform failed, never
    /// that the feature is broken.
    pub fn user_message(&self, what: &str) -> String {
        let head = format!("Could not download the {what}.");
        match self {
            Self::Status(status) => format!(
                "{head} The server answered HTTP {status}. Please resend it in a moment."
            ),
            Self::Network(_) => format!("{head} Network error, please resend it in a moment."),
        }
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(detail) => write!(f, "network error: {detail}"),
            Self::Status(status) => write!(f, "HTTP {status}"),
        }
    }
}

impl std::error::Error for DownloadError {}

/// Download a URL's body, checking the status first and retrying the transient
/// failures. The retry wraps send AND body read, so a reset mid-body is retried
/// as a whole request instead of surfacing as a truncated file.
pub async fn fetch_bytes(url: &str) -> Result<Vec<u8>, DownloadError> {
    let config = RetryConfig::api_aggressive();
    let client = client();
    retry_with_check(
        || async {
            let response = client.get(url).send().await.map_err(|e| {
                // `{e:#}` keeps the cause chain: a bare `{e}` renders as
                // "error sending request" and drops the reason.
                DownloadError::Network(format!("{e:#}"))
            })?;
            let status = response.status();
            if !status.is_success() {
                return Err(DownloadError::Status(status));
            }
            response
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| DownloadError::Network(format!("{e:#}")))
        },
        &config,
        |e| e.is_retryable(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_errors_are_retryable() {
        assert!(DownloadError::Network("connection reset".into()).is_retryable());
    }

    #[test]
    fn a_dead_url_is_not_retried() {
        // An expired file_path is a permanent 400/403/404: retrying it five
        // times just costs bandwidth.
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
        ] {
            assert!(
                !DownloadError::Status(status).is_retryable(),
                "{status} must not be retried"
            );
        }
    }

    #[test]
    fn rate_limits_and_server_errors_are_retried() {
        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert!(
                DownloadError::Status(status).is_retryable(),
                "{status} must be retried"
            );
        }
    }

    #[test]
    fn the_user_is_told_the_truth() {
        let message = DownloadError::Network("dns failure".into()).user_message("voice note");
        assert!(message.contains("voice note"), "{message}");
        assert!(message.contains("Network error"), "{message}");
        // "check your network" reads as the user's fault; this path is not.
        assert!(!message.contains("Failed to download"), "{message}");

        let message = DownloadError::Status(StatusCode::FORBIDDEN).user_message("file");
        assert!(message.contains("403"), "{message}");
    }
}