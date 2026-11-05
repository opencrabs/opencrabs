//! Tests for `brain::agent::format_user_error` — the shared helper that
//! turns raw `AgentError` strings into something the user can act on.
//!
//! Before this helper landed, every surface (TUI toast, Telegram chat,
//! Discord chat, Slack chat, WhatsApp chat) printed the raw
//! `Provider error: API error (502): HTTP 502: error code: 502` and
//! the TUI version auto-dismissed after 2.5s. Result: a user whose
//! turn died because the 5xx fallback chain exhausted saw nothing
//! actionable and concluded the agent silently dropped their request.
//!
//! These tests pin the wording per failure shape so a future refactor
//! of the helper can't accidentally regress to leaking raw errors.

use crate::brain::agent::{AgentError, format_user_error};
use crate::brain::provider::ProviderError;

// Build a synthetic `AgentError::Provider(ProviderError::ApiError)` that
// stringifies as `Provider error: API error (NNN) [type]: msg` — the
// exact shape `format_user_error` matches against to pull out the
// status code. We don't construct real network errors here; the
// helper looks at the text representation, so a synthetic ApiError
// with the right status is enough.
fn provider_err(msg: &str) -> AgentError {
    // Pull `(NNN)` out of the synthetic message to populate `status`,
    // so the helper's `to_string()` sees the canonical format.
    let status = msg
        .find('(')
        .and_then(|i| {
            let tail = &msg[i + 1..];
            let n: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            n.parse::<u16>().ok()
        })
        .unwrap_or(500);
    AgentError::Provider(ProviderError::ApiError {
        status,
        message: msg.to_string(),
        error_type: None,
    })
}

#[test]
fn maps_502_to_fallback_exhausted_message() {
    let err = provider_err("API error (502): HTTP 502: error code: 502");
    let msg = format_user_error(&err);
    assert!(
        msg.contains("502"),
        "must keep the status code visible: {msg}"
    );
    assert!(
        msg.contains("fallback"),
        "must mention the fallback chain so the user knows self-heal already tried: {msg}"
    );
    assert!(
        msg.contains("/models") || msg.contains("switch provider"),
        "must point the user at the recovery action: {msg}"
    );
}

#[test]
fn maps_503_and_504_same_way_as_502() {
    for code in [503, 504] {
        let err = provider_err(&format!("API error ({code}): HTTP {code}"));
        let msg = format_user_error(&err);
        assert!(
            msg.contains("fallback"),
            "5xx code {code} must trigger the fallback-exhausted message: {msg}"
        );
    }
}

#[test]
fn maps_429_to_rate_limit_message() {
    let err = provider_err("API error (429): too many requests");
    let msg = format_user_error(&err);
    assert!(
        msg.contains("Rate limit") || msg.contains("rate limit"),
        "429 must produce a rate-limit message: {msg}"
    );
    assert!(
        !msg.contains("HTTP 429"),
        "the user doesn't need the HTTP shape leaked into the readable message: {msg}"
    );
}

#[test]
fn maps_401_and_403_to_auth_failure() {
    for code in [401, 403] {
        let err = provider_err(&format!("API error ({code}): forbidden"));
        let msg = format_user_error(&err);
        assert!(
            msg.contains("Authentication") || msg.contains("API key"),
            "{code} must mention auth / API key: {msg}"
        );
        assert!(
            msg.contains("/models") || msg.contains("keys.toml"),
            "{code} must point the user at where to fix the key: {msg}"
        );
    }
}

#[test]
fn maps_stream_broken_to_specific_message() {
    let err = AgentError::Internal("error decoding response body: ...".to_string());
    let msg = format_user_error(&err);
    assert!(
        msg.contains("stream") || msg.contains("Stream"),
        "stream-broken signature must produce a stream-specific message: {msg}"
    );
    assert!(
        msg.contains("Try again") || msg.contains("switch"),
        "must include a recovery hint: {msg}"
    );
}

#[test]
fn maps_repetition_loop_to_loop_message() {
    let err = AgentError::Internal("Repetition detected after 8000 bytes".to_string());
    let msg = format_user_error(&err);
    assert!(
        msg.contains("stuck") || msg.contains("loop"),
        "repetition-guard signature must produce a loop message: {msg}"
    );
}

#[test]
fn repetition_loop_surfaces_diagnostic_counts_when_present() {
    let err = AgentError::Provider(ProviderError::AnnouncementLoop(
        "near-identical announcements repeated within the turn \
         [diagnostics:model=qwen-3.7-max,retries=5,swaps=3,rolls=2]"
            .to_string(),
    ));
    let msg = format_user_error(&err);
    assert!(
        msg.contains("qwen-3.7-max"),
        "must surface the model name from the diagnostic suffix: {msg}"
    );
    assert!(
        msg.contains("5 retries"),
        "must surface the retry count: {msg}"
    );
    assert!(
        msg.contains("3 provider swaps"),
        "must surface the swap count: {msg}"
    );
    assert!(
        msg.contains("2 roll(s)"),
        "must surface the roll count: {msg}"
    );
}

#[test]
fn tool_loop_produces_loop_message_with_models_remedy() {
    let err = AgentError::Provider(ProviderError::ToolLoop(
        "near-identical tool-call loop: 'bash' recurred 4x".to_string(),
    ));
    let msg = format_user_error(&err);
    assert!(
        msg.contains("loop") || msg.contains("tool"),
        "must describe the tool loop: {msg}"
    );
    assert!(
        msg.contains("/models"),
        "must still suggest /models: {msg}"
    );
}

fn repetition_loop_works_without_diagnostic_suffix() {
    let err = AgentError::Provider(ProviderError::AnnouncementLoop(
        "near-identical announcements repeated within the turn".to_string(),
    ));
    let msg = format_user_error(&err);
    assert!(
        msg.contains("stuck") || msg.contains("loop"),
        "must still produce a loop message without diagnostics: {msg}"
    );
    assert!(
        !msg.contains("Recovery attempted"),
        "must NOT include recovery counts when diagnostics are absent: {msg}"
    );
}

#[test]
fn maps_context_too_large_to_compact_hint() {
    let err = AgentError::ContextTooLarge {
        current: 250_000,
        limit: 200_000,
    };
    let msg = format_user_error(&err);
    assert!(
        msg.contains("250000") || msg.contains("250,000"),
        "must quote the current token count: {msg}"
    );
    assert!(
        msg.contains("200000") || msg.contains("200,000"),
        "must quote the limit: {msg}"
    );
    assert!(
        msg.contains("/compact"),
        "must point the user at /compact: {msg}"
    );
}

#[test]
fn unknown_error_falls_back_to_raw_string() {
    let err = AgentError::Internal("some weird new failure mode".to_string());
    let msg = format_user_error(&err);
    // We don't want to swallow unknown errors — better to leak the
    // raw string than to print "An error occurred" with no detail.
    assert!(
        msg.contains("some weird new failure mode"),
        "unknown errors must surface the raw text so they're diagnosable: {msg}"
    );
}

#[test]
fn unknown_4xx_includes_status_and_raw_for_debuggability() {
    let err = provider_err("API error (418): I'm a teapot");
    let msg = format_user_error(&err);
    assert!(msg.contains("418"));
    assert!(
        msg.contains("teapot") || msg.contains("self-heal"),
        "either the raw body or the self-heal context must appear: {msg}"
    );
}

// ------------------------------------------------------------------
// AgentError::db — anyhow chain preservation (#974)
// ------------------------------------------------------------------

#[test]
fn db_error_preserves_full_anyhow_chain() {
    let cause = anyhow::anyhow!("database is locked").context("Failed to create message");
    let msg = AgentError::db(cause).to_string();
    assert_eq!(
        msg, "Database error: Failed to create message: database is locked",
        "the SQLite cause must survive the AgentError boundary (#974)"
    );
}

#[test]
fn db_error_single_level_unchanged() {
    let cause = anyhow::anyhow!("disk I/O error");
    assert_eq!(
        AgentError::db(cause).to_string(),
        "Database error: disk I/O error",
        "single-level errors must render exactly as before (#974)"
    );
}

#[test]
fn format_user_error_surfaces_sqlite_cause() {
    let cause = anyhow::anyhow!("database or disk is full").context("Failed to create message");
    let msg = format_user_error(&AgentError::db(cause));
    assert!(
        msg.contains("database or disk is full"),
        "the underlying SQLite cause must reach the user, not just the \
         outer context (#974): {msg}"
    );
    assert!(
        msg.contains("Failed to create message"),
        "the outer context must stay visible too: {msg}"
    );
}
