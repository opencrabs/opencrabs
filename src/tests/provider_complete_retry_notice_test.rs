//! Regression for #1520: the non-streaming `complete()` retry records its
//! attempts into `retry_notices` like the streaming path does.
//!
//! A manual compaction retried a gateway 504 twice and the TUI showed
//! nothing, while the same retries on a chat turn show the usual warning.
//! `stream()` wrapped its send in `retry_with_notify` and pushed a notice per
//! retry; `complete()` used the plain `retry` and its attempts reached the
//! log file only. Compaction is the main caller of `complete()`.

use crate::brain::provider::{LLMRequest, Message, OpenAIProvider, Provider};
use crate::utils::retry::RetryConfig;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

const GATEWAY_TIMEOUT: &str = "<html><body><h1>504 Gateway Time-out</h1></body></html>";

fn completion_json() -> String {
    r#"{"id":"chatcmpl-1520","object":"chat.completion","model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"summary text"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#.to_string()
}

/// Serve one request per entry, in order: `(status line, content type, body)`.
async fn serve_sequence(
    listener: TcpListener,
    responses: Vec<(&'static str, &'static str, String)>,
) {
    for (status, ctype, body) in responses {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 16384];
        let read = timeout(Duration::from_secs(5), sock.read(&mut buf)).await;
        assert!(read.is_ok(), "request head never arrived");
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.expect("write");
        sock.flush().await.ok();
    }
}

fn fast_retries(max_attempts: u32) -> RetryConfig {
    RetryConfig {
        max_attempts,
        initial_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(10),
        backoff_multiplier: 1.0,
        jitter: 0.0,
    }
}

#[tokio::test]
async fn a_retried_504_on_complete_leaves_a_retry_notice() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(serve_sequence(
        listener,
        vec![
            (
                "504 Gateway Time-out",
                "text/html",
                GATEWAY_TIMEOUT.to_string(),
            ),
            ("200 OK", "application/json", completion_json()),
        ],
    ));

    let provider = OpenAIProvider::local(format!("http://127.0.0.1:{port}/chat/completions"))
        .with_retry_config(fast_retries(3));
    let req = LLMRequest::new("test-model", vec![Message::user("summarise")]);
    let response = provider
        .complete(req)
        .await
        .expect("second attempt succeeds");
    assert_eq!(response.id, "chatcmpl-1520");

    let notices = provider.take_retry_notices();
    assert_eq!(notices.len(), 1, "one retry, one notice: {notices:?}");
    let (attempt, max, reason) = &notices[0];
    assert_eq!(*attempt, 1);
    assert_eq!(*max, 3);
    assert!(
        reason.contains("test-model") && reason.contains("server error 504"),
        "notice names the model and the cause: {reason}"
    );
    assert!(
        provider.take_retry_notices().is_empty(),
        "notices drain on take, they never leak into a later turn"
    );
}

#[tokio::test]
async fn a_first_try_success_on_complete_leaves_no_notice() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(serve_sequence(
        listener,
        vec![("200 OK", "application/json", completion_json())],
    ));

    let provider = OpenAIProvider::local(format!("http://127.0.0.1:{port}/chat/completions"))
        .with_retry_config(fast_retries(3));
    let req = LLMRequest::new("test-model", vec![Message::user("summarise")]);
    provider
        .complete(req)
        .await
        .expect("first attempt succeeds");
    assert!(provider.take_retry_notices().is_empty());
}

/// The tool loop drains the notices after the manual-compaction call so the
/// attempts reach the UI on the turn that made them, success or failure.
#[test]
fn manual_compaction_drains_retry_notices() {
    const SRC: &str = include_str!("../brain/agent/service/tool_loop.rs");
    let manual = SRC
        .find("if is_manual_compact {")
        .expect("manual compact branch present");
    let window = &SRC[manual..manual + 900];
    assert!(
        window.contains("emit_retry_notices("),
        "manual /compact must drain the summariser's retry notices"
    );
}
