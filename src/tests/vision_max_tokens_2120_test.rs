//! #2120: `analyze_image` sent a hardcoded `max_tokens: 1024`, which is a
//! completion budget shared with the model's reasoning tokens. On a reasoning
//! vision model the reasoning alone ate the budget, `content` came back empty
//! or cut mid-sentence, and `finish_reason` was `length`. Live repro on
//! `deepseek-v4.1-flash`: 1024 -> `length`, 904 reasoning tokens, 516 chars;
//! 8192 -> `stop`, 586 reasoning tokens, 1089 chars and a complete answer.
//!
//! The fix has three parts, each pinned here:
//!   1. no cap is sent unless the provider config sets one
//!      (`vision_max_tokens`), so the endpoint applies its own default;
//!   2. a configured cap is honoured verbatim;
//!   3. an endpoint that *requires* `max_tokens` still works: the request is
//!      retried exactly once with `VISION_RETRY_MAX_TOKENS`, and never when a
//!      cap was already configured (a second identical 400 is not a retry
//!      case).

use crate::brain::provider::factory::VisionCandidate;
use crate::brain::tools::provider_vision::{
    VISION_RETRY_MAX_TOKENS, needs_max_tokens, try_vision_candidate, vision_request_body,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

const IMAGE: &str = "data:image/png;base64,AAAA";

fn candidate(base_url: String, max_tokens: Option<u32>) -> VisionCandidate {
    VisionCandidate {
        api_key: "test-key".to_string(),
        base_url,
        model: "vision-model".to_string(),
        max_tokens,
    }
}

/// The `400` an endpoint returns when it wants an explicit output cap.
fn max_tokens_required_body() -> String {
    r#"{"error":{"message":"This model requires max_tokens to be provided.","type":"invalid_request_error"}}"#
        .to_string()
}

fn vision_ok_body() -> String {
    r#"{"id":"chatcmpl-2120","object":"chat.completion","model":"vision-model","choices":[{"index":0,"message":{"role":"assistant","content":"a red square"},"finish_reason":"stop"}]}"#
        .to_string()
}

/// Serve one response per entry, in order, and record each request body.
async fn serve(
    listener: TcpListener,
    responses: Vec<(&'static str, String)>,
    bodies: Arc<tokio::sync::Mutex<Vec<String>>>,
) {
    for (status, body) in responses {
        let Ok(Ok((mut sock, _))) = timeout(Duration::from_secs(5), listener.accept()).await else {
            return;
        };
        let mut buf = vec![0u8; 65536];
        let read = timeout(Duration::from_secs(5), sock.read(&mut buf)).await;
        let n = match read {
            Ok(Ok(n)) => n,
            _ => return,
        };
        let raw = String::from_utf8_lossy(&buf[..n]).to_string();
        if let Some(idx) = raw.find("\r\n\r\n") {
            bodies.lock().await.push(raw[idx + 4..].to_string());
        }
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.ok();
        sock.flush().await.ok();
    }
}

fn count_bodies(bodies: &[String]) -> usize {
    bodies.len()
}

#[test]
fn no_cap_omits_max_tokens_entirely() {
    let body = vision_request_body("m", "what is this?", IMAGE, None);
    assert!(
        body.get("max_tokens").is_none(),
        "max_tokens must be absent, not null: gateways reject an explicit null (#2120), got {body}"
    );
    // The rest of the payload is unchanged.
    assert_eq!(body["model"], "m");
    assert_eq!(body["messages"][0]["content"][0]["text"], "what is this?");
    assert_eq!(body["messages"][0]["content"][1]["image_url"]["url"], IMAGE);
}

#[test]
fn configured_cap_is_honoured() {
    let body = vision_request_body("m", "q", IMAGE, Some(4096));
    assert_eq!(
        body["max_tokens"], 4096,
        "providers.<name>.vision_max_tokens must reach the request verbatim"
    );
}

#[test]
fn needs_max_tokens_is_a_single_substring_check() {
    // Anything that names the cap: the request carried none, so a 400 about it
    // is the endpoint asking for the field.
    for reason in [
        "This model requires max_tokens to be provided.",
        "max_tokens is required",
        "Missing required parameter: 'max_tokens'",
        "max_completion_tokens must be provided",
        "max_tokens must be less than or equal to 4096",
    ] {
        assert!(needs_max_tokens(reason), "should retry: {reason}");
    }
    // Failures a cap cannot fix.
    for reason in [
        "model not found",
        "invalid api key",
        "No text response from vision model",
        "content must be a string or array",
    ] {
        assert!(!needs_max_tokens(reason), "should NOT retry: {reason}");
    }
}

#[tokio::test]
async fn a_cap_demanding_endpoint_gets_exactly_one_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    tokio::spawn(serve(
        listener,
        vec![
            ("400 Bad Request", max_tokens_required_body()),
            ("200 OK", vision_ok_body()),
        ],
        bodies.clone(),
    ));

    let client = reqwest::Client::new();
    let cand = candidate(format!("http://127.0.0.1:{port}/chat/completions"), None);
    let out = try_vision_candidate(&client, &cand, "what is this?", IMAGE, None).await;

    assert_eq!(out.as_deref(), Ok("a red square"), "retry should succeed");
    let sent = bodies.lock().await;
    assert_eq!(
        count_bodies(&sent),
        2,
        "expected exactly one retry, got {} request(s)",
        sent.len()
    );
    assert!(
        !sent[0].contains("max_tokens"),
        "first attempt must carry no cap: {}",
        sent[0]
    );
    assert!(
        sent[1].contains(&format!("\"max_tokens\":{VISION_RETRY_MAX_TOKENS}")),
        "retry must carry the {} cap, got: {}",
        VISION_RETRY_MAX_TOKENS,
        sent[1]
    );
}

#[tokio::test]
async fn a_configured_cap_is_never_retried() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    tokio::spawn(serve(
        listener,
        vec![("400 Bad Request", max_tokens_required_body())],
        bodies.clone(),
    ));

    let client = reqwest::Client::new();
    let cand = candidate(
        format!("http://127.0.0.1:{port}/chat/completions"),
        Some(2048),
    );
    let out = try_vision_candidate(&client, &cand, "what is this?", IMAGE, None).await;

    assert!(out.is_err(), "a pinned cap that still 400s must fail");
    let sent = bodies.lock().await;
    assert_eq!(
        count_bodies(&sent),
        1,
        "a configured cap must not trigger the fallback retry, got {} request(s)",
        sent.len()
    );
    assert!(
        sent[0].contains("\"max_tokens\":2048"),
        "the pinned cap must be sent, got: {}",
        sent[0]
    );
}

#[tokio::test]
async fn an_unrelated_400_is_not_retried() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    let bodies = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    tokio::spawn(serve(
        listener,
        vec![(
            "400 Bad Request",
            r#"{"error":{"message":"Unsupported parameter: max_tokens"}}"#.to_string(),
        )],
        bodies.clone(),
    ));

    let client = reqwest::Client::new();
    let cand = candidate(format!("http://127.0.0.1:{port}/chat/completions"), None);
    let out = try_vision_candidate(&client, &cand, "what is this?", IMAGE, None).await;

    assert!(out.is_err());
    let sent = bodies.lock().await;
    assert_eq!(
        count_bodies(&sent),
        1,
        "only a cap DEMAND justifies a retry; an unsupported-parameter 400 must not"
    );
}
