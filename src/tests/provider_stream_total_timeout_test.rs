//! Regression guards for #1635.
//!
//! Two defects shipped together and reinforced each other:
//!
//! 1. Every OpenAI-compatible provider built its HTTP client with a 60s total
//!    request timeout. reqwest's `.timeout()` covers the response body read, so
//!    it is a hard wall-clock ceiling on an SSE stream no matter how many chunks
//!    are still arriving. Anthropic and Gemini already used 300s, which is why
//!    only the compat providers (every custom endpoint, i.e. every cheap
//!    thinking-heavy fallback model) died at exactly 60.0s mid-stream.
//!    #1635 raised the number to 300s, which moved the wall. #1687 took it off
//!    the stream path altogether: the total now bounds non-streaming calls only,
//!    and `the_stream_clients_carry_no_total_timeout` below pins that it never
//!    comes back.
//! 2. The stream-retry log lines hardcoded `3` while `MAX_STREAM_RETRIES` is 5,
//!    emitting `Stream retry 5/3 failed` and `All 3 stream retries failed`. That
//!    made the retry budget look smaller than it is and sent the first reading
//!    of the incident down the wrong path entirely.

use std::path::Path;

use crate::brain::provider::anthropic::DEFAULT_TIMEOUT as ANTHROPIC_TOTAL_TIMEOUT;
use crate::brain::provider::custom_openai_compatible::DEFAULT_TIMEOUT as COMPAT_TOTAL_TIMEOUT;
use crate::brain::provider::gemini::DEFAULT_TIMEOUT as GEMINI_TOTAL_TIMEOUT;

#[test]
fn compat_total_timeout_is_not_below_the_native_stream_providers() {
    assert!(
        COMPAT_TOTAL_TIMEOUT >= ANTHROPIC_TOTAL_TIMEOUT,
        "OpenAI-compatible total request timeout ({:?}) is below the Anthropic one ({:?}). \
         reqwest's client timeout covers the streamed body, so a lower value guillotines \
         healthy SSE streams on every custom provider while Anthropic keeps working (#1635).",
        COMPAT_TOTAL_TIMEOUT,
        ANTHROPIC_TOTAL_TIMEOUT,
    );
    assert!(
        COMPAT_TOTAL_TIMEOUT >= GEMINI_TOTAL_TIMEOUT,
        "OpenAI-compatible total request timeout ({:?}) is below the Gemini one ({:?}) (#1635).",
        COMPAT_TOTAL_TIMEOUT,
        GEMINI_TOTAL_TIMEOUT,
    );
}

/// #1687: the total wall clock must never sit on the stream path again.
///
/// reqwest's `.timeout()` bounds the whole exchange INCLUDING the body read, so
/// a client that carries one puts a ceiling on every stream it serves, no matter
/// how healthily chunks arrive. The inter-chunk idle guard in `helpers.rs` is
/// the correct detector for a dead stream, and it is the only one a stream may
/// meet. This scans the source of all three provider families rather than
/// asserting a relationship between two constants, because the defect was never
/// the VALUE of the number: #1635 raised it from 60s to 300s and the mechanism
/// survived intact.
#[test]
fn the_stream_clients_carry_no_total_timeout() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));

    // 1. The one stream transport every provider shares carries no ceiling,
    //    and neither does the tuning it is built from. Both moved into the
    //    shared HTTP module when the per-provider builders were retired
    //    (#2843), so the scan follows the seam rather than the file it used
    //    to live in.
    let http = std::fs::read_to_string(manifest.join("src/utils/http.rs"))
        .unwrap_or_else(|e| panic!("cannot read src/utils/http.rs: {e}"));
    for (anchor, label) in [
        ("fn streaming_transport_ref()", "the stream transport"),
        ("fn provider_tuning(", "the provider transport tuning"),
    ] {
        let start = http.find(anchor).unwrap_or_else(|| {
            panic!(
                "{anchor} is gone from src/utils/http.rs, and that is the seam \
                 that carries no total ceiling (#1687)"
            )
        });
        let rest = http
            .get(start..)
            .expect("find returns a char boundary, so get cannot miss");
        let body = rest
            .get(..rest.find("\n}").unwrap_or(rest.len()))
            .expect("a search offset is a char boundary");
        assert!(
            !body.contains(".timeout("),
            "{label} sets a total request timeout again, which is a wall clock on \
             every stream it serves (#1687).\n{body}"
        );
    }

    // 2. Each family's `stream()` posts through that transport, never through
    //    the buffered client that carries a total timeout.
    for rel in [
        "src/brain/provider/custom_openai_compatible.rs",
        "src/brain/provider/anthropic.rs",
        "src/brain/provider/gemini.rs",
    ] {
        let src = std::fs::read_to_string(manifest.join(rel))
            .unwrap_or_else(|e| panic!("cannot read {rel}: {e}"));
        let send = method_body(&src, "async fn stream(");
        assert!(
            send.contains(".stream_client"),
            "{rel}: `stream()` no longer posts on the total-timeout-free client (#1687)"
        );
        for (idx, line) in send.lines().enumerate() {
            let trimmed = line.trim();
            let timed = trimmed == ".client"
                || trimmed.starts_with(".client.")
                || trimmed.starts_with("self.client.");
            assert!(
                !timed,
                "{rel}: `stream()` still reaches for the timed `client` at relative line \
                 {idx}, which puts the #1687 ceiling back on the stream path"
            );
        }
    }
}

/// The body of one `impl` method: from its signature to the closing brace at
/// the impl indentation. Brace counting would be fooled by the `{}` inside the
/// tracing format strings these bodies are full of.
fn method_body<'a>(src: &'a str, signature: &str) -> &'a str {
    let start = src.find(signature).unwrap_or_else(|| {
        panic!("`{signature}` is gone, so this guard cannot scan what it cannot find")
    });
    let rest = src.get(start..).expect("find returns a char boundary");
    rest.get(..rest.find("\n    }").unwrap_or(rest.len()))
        .expect("a search offset is a char boundary")
}

#[test]
fn stream_retry_log_lines_never_hardcode_the_retry_budget() {
    let tool_loop =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/brain/agent/service/tool_loop.rs");
    let content = std::fs::read_to_string(&tool_loop)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", tool_loop.display()));

    // Anchors: if the retry log lines are ever reshaped, these fail loudly
    // instead of letting the scan below pass vacuously.
    for anchor in [
        "\"Stream retry {}/{} failed: {}\"",
        "\"All {} stream retries failed",
        "\"5xx retry {}/{} failed: {}\"",
        "\"All {} 5xx retries failed",
    ] {
        assert!(
            content.contains(anchor),
            "expected retry log format {anchor} in tool_loop.rs; the scan below cannot \
             guard what it can no longer find (#1635)"
        );
    }

    let mut violations = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        if !lower.contains("retry") && !lower.contains("retries") {
            continue;
        }
        // `retry {}/3 failed` — the attempt counter with a literal budget.
        if digit_follows(line, "retry {}/") {
            violations.push((idx + 1, line.trim().to_string()));
            continue;
        }
        // `All 3 stream retries failed` — the exhausted-budget line.
        if digit_follows(line, "All ") {
            violations.push((idx + 1, line.trim().to_string()));
        }
    }

    assert!(
        violations.is_empty(),
        "stream retry log lines must interpolate MAX_STREAM_RETRIES, not a literal count \
         (#1635). Offending lines in src/brain/agent/service/tool_loop.rs:\n{}",
        violations
            .iter()
            .map(|(line_no, text)| format!("  {line_no}: {text}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// True when `marker` occurs in `line` immediately followed by an ASCII digit.
fn digit_follows(line: &str, marker: &str) -> bool {
    line.match_indices(marker).any(|(at, _)| {
        line[at + marker.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
    })
}
