//! Regression tests for `handshake_timeout_for`.
//!
//! Locks in the 2026-04-25 fix where NVIDIA's wedged
//! `integrate.api.nvidia.com` ate ~3 minutes (3 × 90s) of retry budget
//! before falling back. Cloud HTTP now bails after 60s (bumped from 30s
//! on 2026-05-07 because routing proxies like dialagram legitimately
//! need 20-45s when upstream is slow), while local HTTP keeps the longer
//! 90s window for cold-loading models.

use crate::brain::agent::service::helpers::{
    cli_startup_timeout_for, handshake_timeout_for, send_handshake_timeout_for,
};
use std::time::Duration;

#[test]
fn cli_providers_get_ten_minutes() {
    // Subprocess startup + auth refresh dominate; the URL — if any —
    // doesn't matter.
    assert_eq!(handshake_timeout_for(true, None), Duration::from_secs(600));
    assert_eq!(
        handshake_timeout_for(true, Some("http://localhost:1234/v1/chat/completions")),
        Duration::from_secs(600),
    );
    assert_eq!(
        handshake_timeout_for(true, Some("https://api.openai.com/v1/chat/completions")),
        Duration::from_secs(600),
    );
}

#[test]
fn local_http_gets_ninety_seconds() {
    // Cold-loading a large gguf or MLX checkpoint can take 30+ seconds.
    for url in [
        "http://localhost:1234/v1/chat/completions",
        "http://127.0.0.1:8080/v1/chat/completions",
        "http://[::1]:11434/api/chat",
        "http://0.0.0.0:8000/v1/chat/completions",
        "http://192.168.1.42:11434/api/chat",
        "http://10.0.0.5:8080/v1",
        "http://172.20.0.10:8000/v1",
        "http://my-rig.local:1234/v1/chat/completions",
    ] {
        assert_eq!(
            handshake_timeout_for(false, Some(url)),
            Duration::from_secs(90),
            "expected 90s for local URL: {}",
            url,
        );
    }
}

#[test]
fn cloud_http_gets_sixty_seconds() {
    // Routing proxies (dialagram, openrouter) can take 20-45s when
    // upstream is slow. Wedged servers are >120s, so 60s still catches
    // real hangs in under 3 min via the retry chain. Previous 30s killed
    // legitimate requests to slower-but-healthy providers.
    for url in [
        "https://integrate.api.nvidia.com/v1/chat/completions",
        "https://api.openai.com/v1/chat/completions",
        "https://api.anthropic.com/v1/messages",
        "https://api.z.ai/api/coding/paas/v4/chat/completions",
        "https://opencode.ai/zen/go/v1/chat/completions",
        "https://openrouter.ai/api/v1/chat/completions",
        "https://api.minimax.io/v1/chat/completions",
        "https://api.moonshot.ai/v1/chat/completions",
    ] {
        assert_eq!(
            handshake_timeout_for(false, Some(url)),
            Duration::from_secs(60),
            "expected 60s for cloud URL: {}",
            url,
        );
    }
}

#[test]
fn opencrabs_endpoint_gets_two_minutes() {
    // #2232: the built-in endpoint is a proxy in front of upstream models,
    // so a cold upstream can legitimately need longer than the 60s cloud
    // budget to produce response headers. A handshake that lands at 70s or
    // 100s is slow, not broken.
    assert_eq!(
        handshake_timeout_for(false, Some("https://inference.opencrabs.com/v1")),
        Duration::from_secs(120),
    );
    // The neighbours in the class table must not move: another cloud host
    // keeps 60s, a local server keeps 90s.
    assert_eq!(
        handshake_timeout_for(false, Some("https://api.openai.com/v1/chat/completions")),
        Duration::from_secs(60),
    );
    assert_eq!(
        handshake_timeout_for(false, Some("http://127.0.0.1:8080/v1/chat/completions")),
        Duration::from_secs(90),
    );
    // CLI still wins over the endpoint class: a subprocess has no send to
    // bound, so its startup wall stays at 10 min.
    assert_eq!(
        handshake_timeout_for(true, Some("https://inference.opencrabs.com/v1")),
        Duration::from_secs(600),
    );
    // The per-send budget each HTTP provider applies follows the same class.
    assert_eq!(
        send_handshake_timeout_for(false, Some("https://inference.opencrabs.com/v1")),
        Some(Duration::from_secs(120)),
    );
}

#[test]
fn missing_base_url_defaults_to_cloud_timeout() {
    // Providers without a base_url (built-in Anthropic/Gemini that
    // hardcode their endpoints internally) are always cloud — they
    // can't be a local LM server.
    assert_eq!(handshake_timeout_for(false, None), Duration::from_secs(60));
}

// ---------------------------------------------------------------------------
// The PER-SEND budget (#680/#682).
//
// `handshake_timeout_for` is the class table. `send_handshake_timeout_for` is
// what each HTTP provider applies at its own `.send()`, and it must NOT return
// a budget for a CLI provider: those spawn a subprocess rather than sending
// HTTP, so a send budget is meaningless.
// ---------------------------------------------------------------------------

#[test]
fn cli_providers_get_no_per_send_budget() {
    assert_eq!(send_handshake_timeout_for(true, None), None);
    assert_eq!(
        send_handshake_timeout_for(true, Some("https://api.openai.com/v1/chat/completions")),
        None,
    );
}

#[test]
fn http_providers_get_the_class_budget_per_send() {
    assert_eq!(
        send_handshake_timeout_for(false, Some("https://api.openai.com/v1/chat/completions")),
        Some(Duration::from_secs(60)),
    );
    assert_eq!(
        send_handshake_timeout_for(false, Some("http://127.0.0.1:8080/v1/chat/completions")),
        Some(Duration::from_secs(90)),
    );
    // No base_url: the built-in Anthropic/Gemini providers hardcode cloud
    // endpoints, so they take the cloud budget.
    assert_eq!(
        send_handshake_timeout_for(false, None),
        Some(Duration::from_secs(60)),
    );
}

// ---------------------------------------------------------------------------
// The CALLER-LEVEL wall (#680). This is the leg that keeps the two helpers from
// being swapped: an HTTP provider must get NO wall around `provider.stream()`
// (its budget lives at the send), while a CLI provider — a subprocess, with no
// send to bound — must keep its process-startup wall, and this level is the
// only place that can carry it.
// ---------------------------------------------------------------------------

#[test]
fn caller_wall_is_cli_only() {
    assert_eq!(
        cli_startup_timeout_for(true, None),
        Some(Duration::from_secs(600)),
    );
    assert_eq!(
        cli_startup_timeout_for(true, Some("https://api.openai.com/v1/chat/completions")),
        Some(Duration::from_secs(600)),
    );
    assert_eq!(cli_startup_timeout_for(false, None), None);
    assert_eq!(
        cli_startup_timeout_for(false, Some("https://api.openai.com/v1/chat/completions")),
        None,
    );
    assert_eq!(
        cli_startup_timeout_for(false, Some("http://127.0.0.1:8080/v1/chat/completions")),
        None,
    );
}

/// The two helpers partition the provider space: exactly one of them yields a
/// wall for any given provider, never both and never neither.
#[test]
fn send_and_caller_walls_never_both_apply() {
    let bases = [
        None,
        Some("https://api.openai.com/v1/chat/completions"),
        Some("http://127.0.0.1:8080/v1/chat/completions"),
    ];
    for cli in [true, false] {
        for base in bases {
            let send = send_handshake_timeout_for(cli, base);
            let caller = cli_startup_timeout_for(cli, base);
            assert!(
                send.is_some() ^ caller.is_some(),
                "exactly one wall must apply (cli={}, send={:?}, caller={:?})",
                cli,
                send,
                caller
            );
        }
    }
}

/// A URL that merely carries the built-in host keeps the 60s cloud budget.
///
/// The endpoint class is a host check (#2234). Before the host comparison was
/// fixed, a substring match handed any such URL the 120s endpoint budget.
#[test]
fn a_lookalike_url_does_not_get_the_endpoint_budget() {
    for url in [
        "https://evil.example/?x=inference.opencrabs.com",
        "https://proxy.example/inference.opencrabs.com/v1",
        "https://inference.opencrabs.com.attacker.example/v1",
        "https://notinference.opencrabs.com/v1",
    ] {
        assert_eq!(
            handshake_timeout_for(false, Some(url)),
            Duration::from_secs(60),
            "{url} must keep the 60s cloud budget"
        );
    }
    // The real host still gets the endpoint budget.
    assert_eq!(
        handshake_timeout_for(false, Some("https://inference.opencrabs.com/v1")),
        Duration::from_secs(120),
    );
}
