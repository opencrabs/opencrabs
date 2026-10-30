//! Error types for LLM providers

use thiserror::Error;

/// Provider error types
#[derive(Debug, Error)]
pub enum ProviderError {
    /// HTTP request failed. The message carries reqwest's source chain, so
    /// the retry log names the real cause (for example a DNS failure) and
    /// not only "error sending request for url (...)".
    #[error("HTTP request failed: {}", reqwest_error_chain(.0))]
    HttpError(#[from] reqwest::Error),

    /// API returned an error
    #[error(
        "API error ({status}){}: {message}",
        error_type
            .as_ref()
            .filter(|t| !t.is_empty())
            .map(|t| format!(" [{}]", t))
            .unwrap_or_default()
    )]
    ApiError {
        status: u16,
        message: String,
        error_type: Option<String>,
    },

    /// Invalid API key
    #[error("Invalid API key")]
    InvalidApiKey,

    /// Rate limit exceeded
    #[error("Rate limit exceeded: {0}")]
    RateLimitExceeded(String),

    /// Invalid request
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    /// Model not found
    #[error("Model not found: {0}")]
    ModelNotFound(String),

    /// Context length exceeded
    #[error("Context length exceeded: {0} tokens")]
    ContextLengthExceeded(u32),

    /// Streaming not supported
    #[error("Streaming not supported by this provider")]
    StreamingNotSupported,

    /// Tools not supported
    #[error("Tools not supported by this provider")]
    ToolsNotSupported,

    /// JSON parsing error
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// Streaming error
    #[error("Streaming error: {0}")]
    StreamError(String),

    /// Timeout
    #[error("Request timed out after {0}s")]
    Timeout(u64),

    /// Thinking-loop timeout (#890): model streamed for N seconds without
    /// emitting a tool call AND without delivering any content. Retryable with
    /// phantom enforcement.
    ///
    /// The second half is #1690: this used to fire on a stream that was happily
    /// mid-answer, which threw the delivered text away and replayed the whole
    /// conversation. A stream that is delivering now stands the guard down and
    /// finishes, so reaching this error means there was nothing to lose.
    #[error("Thinking loop timeout: {0}s with no tool call emitted")]
    ThinkingLoopTimeout(u64),

    /// The model kept announcing an action without emitting the call, and the
    /// announcement-loop detector ended the turn (#1023).
    ///
    /// Ours by origin, the model's by nature — which is what matters for
    /// routing. It was raised as `AgentError::Internal`, a type the fallback
    /// walk does not match on, so a turn killed this way could never try
    /// another provider even though a different model usually emits the call
    /// immediately.
    #[error("Repetition detected: {0}")]
    AnnouncementLoop(String),

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Whether an error/message text is the provider's server-side repetitive-tool-
/// call guardrail (#740). Matched loosely so variants across providers
/// (alibaba/qwen, opencode-go/deepseek) all classify. Lives here (provider
/// layer) so `is_retryable` and the agent's recovery share one matcher.
pub fn is_repetitive_tool_guardrail(msg: &str) -> bool {
    let l = msg.to_lowercase();
    l.contains("repetitive tool call")
        || l.contains("identical name and arguments")
        || (l.contains("repeated") && l.contains("consecutive rounds"))
}

/// Phrase vocabulary for HARD quota / billing limits (monthly cap, free
/// tier exhausted, no credit) as opposed to a transient per-minute
/// throttle. A hard quota will not lift inside any retry window, so
/// retrying it only burns backoff (#952). Deliberately conservative: an
/// unknown 429 wording stays retryable, because transient throttling is
/// the common case and a false "hard quota" classification would skip
/// retries that would have succeeded.
const QUOTA_EXHAUSTION_PHRASES: &[&str] = &[
    "exceeded your current quota",
    "insufficient_quota",
    "insufficient balance",
    "no credit balance",
    "credit balance is insufficient",
    "out of credits",
    "no credits left",
    "quota exceeded",
    "quota_exceeded",
    "quota exhausted",
    "quota_exhausted",
    "exhausted your quota",
    "monthly limit",
    "monthly quota",
    "free tier quota",
    "free allocated quota",
    "allocated quota",
    "allocationquota",
    "reached your monthly",
    "billing_hard_limit",
    "hard limit reached",
    // Modelscope's monthly-dead wording (#1084).
    "month's quota",
    "try again next month",
];

/// The user-facing text for a 429, with advice chosen from the upstream
/// message rather than from the status code alone (#1254).
///
/// A 429 carrying `Insufficient balance … Please recharge` is a billing
/// state, not a throttle: it will not clear on any retry timescale, so
/// "please retry later" is advice that contradicts the sentence it is
/// appended to. This text is not internal, the self-healing banner embeds
/// it verbatim into the chat, so a whole group gets told to wait for
/// something that only money or a different provider fixes.
///
/// An unrecognised 429 keeps the retry advice: the phrase list is
/// deliberately conservative, and treating an unknown throttle as terminal
/// is the more expensive mistake.
pub fn rate_limit_message(upstream: &str, retry_after: Option<u64>) -> String {
    if is_quota_exhausted_message(upstream) {
        // A Retry-After header on a balance error is noise, so it is
        // dropped here rather than offered as a countdown to nothing.
        return format!(
            "{} (quota or balance exhausted, not a temporary throttle: \
             recharge the provider or use /models to switch)",
            upstream
        );
    }
    match retry_after {
        Some(secs) => format!("{} (retry after {} seconds)", upstream, secs),
        None => format!("{} (rate limited, please retry later)", upstream),
    }
}

/// True when an error/message body describes a HARD quota or billing
/// limit rather than a transient throttle (#952).
pub fn is_quota_exhausted_message(msg: &str) -> bool {
    let l = msg
        .to_lowercase()
        // Normalize curly apostrophes so "month\u{2019}s" matches "month's" (#1084).
        .replace(['\u{2018}', '\u{2019}'], "'");
    QUOTA_EXHAUSTION_PHRASES.iter().any(|p| l.contains(p))
}

/// A declared wait longer than this is not retried in place: the chain is
/// consulted instead and the provider is parked for the declared window.
pub const INLINE_RETRY_CEILING: std::time::Duration = std::time::Duration::from_secs(60);

impl ProviderError {
    /// The wait the server itself declared, UNCAPPED. `retry_after()` is the
    /// in-place retry hint (clamped so a turn cannot stall); this is the
    /// cooldown window: how long the provider said not to come back.
    pub fn declared_cooldown(&self) -> Option<std::time::Duration> {
        let msg = match self {
            ProviderError::RateLimitExceeded(m) => m.as_str(),
            ProviderError::ApiError {
                status, message, ..
            } if *status == 429 => message.as_str(),
            _ => return None,
        };
        parse_retry_seconds(msg).map(std::time::Duration::from_secs)
    }

    /// Check if error is retryable
    pub fn is_retryable(&self) -> bool {
        // Hard quota / billing limits never lift inside a retry window —
        // bail straight to the fallback chain instead of burning the whole
        // backoff budget against a wall (#952).
        if self.is_quota_exhausted() {
            return false;
        }
        // A declared wait past the inline ceiling is not worth retrying in
        // place: park and fall back, do not burn 4 x 30 s of silence.
        if self
            .declared_cooldown()
            .is_some_and(|d| d > INLINE_RETRY_CEILING)
        {
            return false;
        }
        match self {
            ProviderError::HttpError(_)
            | ProviderError::RateLimitExceeded(_)
            | ProviderError::Timeout(_)
            | ProviderError::ThinkingLoopTimeout(_)
            // A stream that broke mid-flight ("connection closed before message
            // completed", the SSE socket dropping, a partial body) is a
            // transport hiccup, not a client mistake — re-issuing the request
            // usually succeeds. Retry it like the other transport errors instead
            // of bouncing straight to the fallback chain. A genuinely fatal
            // cause (bad model, auth, invalid content) surfaces as a typed
            // ApiError with its own status, which is handled below — it never
            // reaches here as a StreamError.
            | ProviderError::StreamError(_) => true,
            // The provider's repetitive-tool-call guardrail (a 500) will 500
            // again on retry with the same poisoned history — retrying just
            // burns attempts. Mark it NON-retryable so it surfaces immediately
            // to the tool loop, which prunes the duplicate rounds and retries
            // once with a healed history (#740).
            ProviderError::ApiError { status: 500, message, .. }
                if is_repetitive_tool_guardrail(message) =>
            {
                false
            }
            ProviderError::ApiError { status, .. } if *status >= 500 => true,
            // A 4xx whose body is an HTML page is an infrastructure / CDN /
            // load-balancer error page, NOT a real JSON API client error.
            // These are transient (the next request usually hits a healthy
            // node) and must be retried, not bounced straight to the
            // fallback chain. Canonical case: modelscope intermittently
            // returns HTTP 405 with a Chinese HTML error page for a valid
            // POST to /chat/completions; retrying succeeds, but the old code
            // treated 405 as a hard client error and fell back instantly
            // with zero retries (2026-06-07). Real client errors return
            // JSON, never HTML, so this never masks an invalid_model /
            // validation / auth problem.
            ProviderError::ApiError {
                status, message, ..
            } if (400..500).contains(status) && is_html_error_body(message) => true,
            // HTTP 400 with a generic proxy-style body (empty error_type
            // AND a message that doesn't describe an actionable client
            // problem) is almost always a transient upstream failure
            // forwarded by the proxy. opencode.ai's "Provider returned
            // error" is the canonical case — the user's payload is fine,
            // their upstream is having a moment. Retry before falling
            // back. Real client-side 400s (invalid_model, validation
            // errors, bad JSON) carry specific error_type or message
            // strings and stay non-retryable.
            ProviderError::ApiError {
                status: 400,
                message,
                error_type,
            } if is_transient_proxy_400(message, error_type.as_deref()) => true,
            // A 4xx JSON body that describes a TEMPORARY server-side
            // unavailability — the model/provider is overloaded, at capacity,
            // or asking to try again. Some providers return this instead of
            // 429/5xx, so without classifying it the transient error surfaces
            // (or bounces to fallback with zero retries) instead of getting
            // the retry budget. `is_temporarily_unavailable` excludes
            // permanent model-unsupported and auth errors, so this never masks
            // an actionable configuration problem.
            ProviderError::ApiError { status, .. }
                if (400..500).contains(status) && self.is_temporarily_unavailable() =>
            {
                true
            }
            // A 404 that is NOT a "model not found" is almost always a flaky
            // provider / proxy dropping a valid endpoint mid-run (transient),
            // not a permanent client error — retry it (bounded by the retry
            // budget) instead of bailing the whole run. Genuine model-not-found
            // 404s are caught by `is_model_unsupported` and route to the
            // model-mismatch UX, staying permanent (#748).
            ProviderError::ApiError { status: 404, .. } if !self.is_model_unsupported() => true,
            _ => false,
        }
    }

    /// Get HTTP status code if available
    pub fn status_code(&self) -> Option<u16> {
        match self {
            ProviderError::ApiError { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// True when the server rejected the REQUEST's model id (not the
    /// credential). Some OpenAI-compatible proxies — notably
    /// `opencode.ai/zen` — return HTTP 401 with
    /// `{"error":{"type":"ModelError","message":"Model X not supported"}}`
    /// for "this key can't use that model", which collides with real
    /// auth failures. Downstream code uses this to keep the actual
    /// "invalid key" classification meaningful and route model-mismatch
    /// errors to a different UX path.
    pub fn is_model_unsupported(&self) -> bool {
        match self {
            ProviderError::ModelNotFound(_) => true,
            ProviderError::ApiError {
                error_type,
                message,
                ..
            } => {
                let type_hit = error_type.as_ref().is_some_and(|t| {
                    let t = t.to_ascii_lowercase();
                    t == "modelerror"
                        || t == "model_error"
                        || t == "model_not_found"
                        || t == "invalid_model"
                });
                let msg = message.to_ascii_lowercase();
                let msg_hit = msg.contains("model")
                    && (msg.contains("not supported")
                        || msg.contains("not found")
                        || msg.contains("unsupported"));
                type_hit || msg_hit
            }
            _ => false,
        }
    }

    /// True when the error describes a TEMPORARY server-side unavailability —
    /// the model or provider is overloaded, at capacity, or explicitly asking
    /// to try again — rather than a permanent client error (bad model, auth,
    /// validation). Some OpenAI-compatible providers return this as a 4xx JSON
    /// body instead of 429/5xx, so `is_retryable` and (through it) the fallback
    /// chain consult this to give the request its retry budget and, failing
    /// that, roll to the next provider. Deliberately excludes permanent
    /// model-unsupported and auth/validation errors so it never masks an
    /// actionable configuration problem.
    pub fn is_temporarily_unavailable(&self) -> bool {
        match self {
            // 429 and 5xx already route through RateLimitExceeded / the 5xx
            // arm of is_retryable; classify only the ambiguous 4xx JSON case.
            ProviderError::ApiError {
                status,
                message,
                error_type,
            } if (400..500).contains(status) && *status != 429 => {
                // A permanent "model not found / not supported" must stay
                // permanent (routes to the model-mismatch UX, not a retry).
                if self.is_model_unsupported() {
                    return false;
                }
                is_temporary_unavailable_signal(message, error_type.as_deref())
            }
            _ => false,
        }
    }

    /// True when this error signals a HARD quota or billing limit that
    /// will not lift inside any retry window (monthly cap, free tier
    /// exhausted, no credit, billing cap). Distinct from a transient
    /// per-minute throttle, which stays retryable (#952).
    pub fn is_quota_exhausted(&self) -> bool {
        match self {
            ProviderError::RateLimitExceeded(msg) => is_quota_exhausted_message(msg),
            ProviderError::ApiError {
                status, message, ..
            } if *status == 429 => is_quota_exhausted_message(message),
            // 402 Payment Required: billing cap / out of credit. Hard stop.
            ProviderError::ApiError { status: 402, .. } => true,
            ProviderError::StreamError(msg) => is_quota_exhausted_message(msg),
            _ => false,
        }
    }
}

/// Does this failure justify handing the request to the NEXT provider in a
/// chain? The single policy shared by `FallbackProvider`, the tool loop's
/// in-place walk, and compaction, so a request that would fail over during a
/// chat turn fails over everywhere else too (#1247).
///
/// Note the deliberate split from [`ProviderError::is_retryable`]: the two
/// answer different questions and a hard quota is exactly where they diverge.
/// `is_retryable` says "retry the SAME provider", which for a monthly cap or
/// an empty balance is pointless — so it returns false, as #952 intended.
/// Chain traversal then read that false as "this error is fatal, stop" and
/// aborted on the one class of error a chain exists to survive: the next
/// provider bills a different account and would have answered fine.
pub fn should_try_next_provider(err: &ProviderError) -> bool {
    // Transient (rate limit, 5xx, timeout) — the next provider may be healthy.
    if err.is_retryable() {
        return true;
    }
    // Hard quota / billing cap. Never retried in place, always worth handing
    // to a provider with its own budget.
    if err.is_quota_exhausted() {
        return true;
    }
    // A 429 naming a long wait: not retryable in place, but the next provider
    // bills a different account and should answer now.
    if err
        .declared_cooldown()
        .is_some_and(|d| d > INLINE_RETRY_CEILING)
    {
        return true;
    }
    match err {
        // Model not supported here — a fallback may carry it, and the caller
        // remaps to the fallback's default model anyway.
        ProviderError::ModelNotFound(_) => true,
        // 400 invalid parameter/model, 401/403 dead credentials, 402 billing.
        // All unrecoverable on THIS provider, all fine on the next one.
        ProviderError::ApiError {
            status: 400..=403, ..
        }
        | ProviderError::InvalidApiKey => true,
        // Parsed model/param rejection.
        ProviderError::InvalidRequest(_) => true,
        // The payload is too big for THIS provider's window. Windows differ
        // by an order of magnitude across one chain (128K to 1M in a real
        // config), so which provider is asked changes the answer, and the
        // caller rebuilds the request for whoever receives it (#1379). When
        // the whole chain refuses, `with_chain_summary` keeps this variant
        // intact so the tool loop's emergency compaction still fires.
        ProviderError::ContextLengthExceeded(_) => true,
        _ => false,
    }
}

/// One-line, user-facing classification of a provider failure for retry
/// notices and fallback-chain summaries (#952): "quota exhausted",
/// "rate limited", "auth error", "server error 502", "timeout", ...
pub fn short_error_reason(err: &ProviderError) -> String {
    if err.is_quota_exhausted() {
        return "quota exhausted".to_string();
    }
    match err {
        ProviderError::RateLimitExceeded(_) => "rate limited".to_string(),
        ProviderError::ApiError { status, .. } if *status == 429 => "rate limited".to_string(),
        ProviderError::ApiError { status: 401, .. }
        | ProviderError::ApiError { status: 403, .. } => "auth error".to_string(),
        ProviderError::ApiError { status, .. } if *status >= 500 => {
            format!("server error {status}")
        }
        ProviderError::ApiError { status, .. } => format!("HTTP {status}"),
        ProviderError::InvalidApiKey => "missing/invalid API key".to_string(),
        ProviderError::ModelNotFound(_) => "model not found".to_string(),
        ProviderError::Timeout(_) => "timeout".to_string(),
        ProviderError::StreamError(_) => "stream error".to_string(),
        ProviderError::HttpError(_) => "connection error".to_string(),
        _ => "transient error".to_string(),
    }
}

/// User-facing summary for "every provider in the chain failed" (#952).
/// Names the primary and each fallback attempted with its failure reason,
/// and ends with an
/// actionable hint — instead of leaking the bare error string of whichever
/// provider happened to die last.
pub fn chain_exhausted_summary(primary: &str, primary_reason: &str, tried: &[String]) -> String {
    let mut lines = vec![format!(
        "All providers in the fallback chain failed. {primary}: {primary_reason}."
    )];
    for t in tried {
        lines.push(format!("Fallback {t}"));
    }
    lines.push(
        "Switch to a working provider via /models, or wait for the quota window to reset."
            .to_string(),
    );
    lines.join("\n")
}

/// Actionable setup guidance shown when self-healing has nothing to fall
/// back to because no fallback chain is configured (#1006, #1007). The
/// runtime never pointed users at `[providers.fallback]` before, so most
/// recoverable failures died raw instead of failing over.
pub fn no_chain_setup_guidance() -> &'static str {
    "no fallback chain configured. To set one up: add [providers.fallback] \
     to config.toml with enabled = true and providers = [\"first\", \
     \"second\"] (tried in order, at least two recommended; each provider \
     must already have its keys in keys.toml), then /restart. OpenCrabs \
     will then fail over automatically when a provider fails."
}

/// Attach a chain-exhaustion summary to the last provider error without
/// losing its variant, so upstream classification keeps working while the
/// message carries the full tried-provider ledger (#1007). Variants without
/// a message slot pass through unchanged.
pub fn with_chain_summary(err: ProviderError, summary: String) -> ProviderError {
    match err {
        ProviderError::RateLimitExceeded(m) => {
            ProviderError::RateLimitExceeded(format!("{m}\n{summary}"))
        }
        ProviderError::InvalidRequest(m) => {
            ProviderError::InvalidRequest(format!("{m}\n{summary}"))
        }
        ProviderError::ModelNotFound(m) => ProviderError::ModelNotFound(format!("{m}\n{summary}")),
        ProviderError::StreamError(m) => ProviderError::StreamError(format!("{m}\n{summary}")),
        ProviderError::Internal(m) => ProviderError::Internal(format!("{m}\n{summary}")),
        ProviderError::ApiError {
            status,
            message,
            error_type,
        } => ProviderError::ApiError {
            status,
            message: format!("{message}\n{summary}"),
            error_type,
        },
        other => other,
    }
}

/// True when a provider error body describes a transient overload/capacity
/// condition. Matches on overload-ish error types and on a bounded vocabulary
/// of "temporarily unavailable / at capacity / try again" phrases, while
/// rejecting permanent auth/model phrases outright so an auth or
/// model-not-found body is never mistaken for a transient blip.
pub(crate) fn is_temporary_unavailable_signal(message: &str, error_type: Option<&str>) -> bool {
    let ty = error_type.unwrap_or("").trim().to_ascii_lowercase();
    // Error types some providers set explicitly for an overloaded backend.
    const TRANSIENT_TYPES: &[&str] = &[
        "overloaded_error",
        "overloaded",
        "server_error",
        "service_unavailable",
        "capacity_error",
        "capacity",
    ];
    if TRANSIENT_TYPES.iter().any(|t| ty == *t) {
        return true;
    }
    let m = message.to_ascii_lowercase();
    // Guard: permanent auth/config/model errors must never read as transient,
    // even if some other word in the body happens to look transient.
    const PERMANENT_HINTS: &[&str] = &[
        "invalid api key",
        "unauthorized",
        "authentication",
        "permission denied",
        "not found",
        "not supported",
        "unsupported",
        "does not exist",
        "invalid model",
        "no such model",
    ];
    if PERMANENT_HINTS.iter().any(|h| m.contains(h)) {
        return false;
    }
    // Positive vocabulary for a temporary server-side unavailability. Kept
    // specific to overload/capacity/try-again so unrelated 4xx bodies do not
    // match. Add new strings here when a provider invents a different phrase.
    const TRANSIENT_HINTS: &[&str] = &[
        "overloaded",
        "over capacity",
        "at capacity",
        "no capacity",
        "temporarily unavailable",
        "temporarily overloaded",
        "currently unavailable",
        "currently overloaded",
        "service unavailable",
        "server is busy",
        "servers are busy",
        "too busy",
        "high demand",
        "try again",
        "please retry",
    ];
    TRANSIENT_HINTS.iter().any(|h| m.contains(h))
}

/// True when an inline stream error is the proxy's CAPACITY SATURATION signal
/// rather than a real rate limit (#2270).
///
/// The built-in endpoint answers 200 OK and then reports a full upstream pool
/// inside the SSE body: `type=capacity_saturated`, "... out of quota or cooling
/// down ...; retry shortly". Retrying in place is the right response because the
/// proxy rotates upstream providers on every attempt, so the next send can land
/// on a healthy one. Classifying it as a rate limit instead walked the fallback
/// chain 0.5 ms later with zero in-place attempts (2026-10-30 log).
///
/// Hard quota phrases stay out of this matcher on purpose: a monthly cap or an
/// exhausted credit balance does not lift inside any retry window, so those keep
/// falling back immediately (#952).
pub(crate) fn is_capacity_saturation_signal(message: &str, error_type: Option<&str>) -> bool {
    if is_quota_exhausted_message(message) {
        return false;
    }
    if error_type
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("capacity_saturated")
    {
        return true;
    }
    let m = message.to_ascii_lowercase();
    const CAPACITY_HINTS: &[&str] = &[
        "capacity_saturated",
        "cooling down",
        "retry shortly",
        "no capacity",
    ];
    CAPACITY_HINTS.iter().any(|h| m.contains(h))
}

/// True when an error body is an HTML page rather than a JSON API error.
/// A 4xx that returns HTML came from a CDN / load balancer / reverse proxy
/// (an infrastructure error page), not the API itself — these are
/// transient and worth retrying. Real API client errors are always JSON,
/// so this never matches a genuine invalid_model / validation / auth error.
pub(crate) fn is_html_error_body(message: &str) -> bool {
    // Scan a bounded prefix so a huge HTML page isn't lowercased in full on
    // every error. `chars().take()` is char-boundary-safe — a byte slice
    // would panic mid-UTF8 (the modelscope body has Chinese characters).
    let head: String = message
        .trim_start()
        .chars()
        .take(256)
        .collect::<String>()
        .to_ascii_lowercase();
    head.contains("<!doctype")
        || head.contains("<html")
        || head.contains("<head")
        || head.contains("<body")
}

/// True when an HTTP 400 response body looks like a proxy passthrough of
/// an upstream hiccup rather than a real client-side error. Used by
/// `is_retryable` so opencode.ai-style "Provider returned error" 400s
/// go through the 4-attempt backoff (`RetryConfig::default`) instead of
/// bailing to fallback on the first try.
pub(crate) fn is_transient_proxy_400(message: &str, error_type: Option<&str>) -> bool {
    // Real client errors always carry an error_type (OpenAI: "invalid_request_error",
    // "model_not_found", "validation_error", etc.). Treat any non-empty type as
    // non-transient so we don't retry bad payloads.
    if error_type.is_some_and(|t| !t.is_empty()) {
        return false;
    }
    let m = message.trim().to_ascii_lowercase();
    if m.is_empty() {
        return true;
    }
    // Known proxy-passthrough phrases. Add new strings here when a proxy
    // invents a different one.
    const TRANSIENT_HINTS: &[&str] = &[
        "provider returned error",
        "upstream error",
        "internal error",
        "temporary",
        "try again",
        "bad gateway",
    ];
    TRANSIENT_HINTS.iter().any(|h| m.contains(h))
}

/// Result type for provider operations
pub type Result<T> = std::result::Result<T, ProviderError>;

impl crate::utils::retry::RetryableError for ProviderError {
    fn is_retryable(&self) -> bool {
        // Delegate to the inherent classifier. `Self::is_retryable` would
        // be ambiguous (inherent vs this trait method), so go through a
        // free helper that names the inherent unambiguously.
        provider_error_is_retryable(self)
    }

    fn retry_after(&self) -> Option<std::time::Duration> {
        // Parse a server Retry-After hint from rate-limit errors, clamped
        // to 30s so a pathological "retry after 300s" can't stall a turn.
        // Other error kinds have no hint — the caller falls back to the
        // exponential schedule.
        self.declared_cooldown()
            .map(|d| d.min(std::time::Duration::from_secs(30)))
    }
}

/// Free wrapper so the `RetryableError` impl can call the inherent
/// `ProviderError::is_retryable` without method-resolution ambiguity.
fn provider_error_is_retryable(e: &ProviderError) -> bool {
    e.is_retryable()
}

/// Render a concise, SPECIFIC reason for a provider error, for the user-facing
/// TUI warnings ("⏳ Retry…", "🔧 Switched to…"). For HTTP errors this digs
/// through reqwest's source chain to the real cause — DNS lookup failure,
/// connection refused, TLS error, timeout — instead of the opaque top-level
/// "error sending request for url (…)" that hides what actually happened.
pub fn user_facing_reason(err: &ProviderError) -> String {
    match err {
        ProviderError::HttpError(e) => describe_reqwest_error(e),
        other => other.to_string(),
    }
}

/// Render a reqwest error with its whole source chain, joined by ": ". The
/// top level says only "error sending request for url (...)"; the deeper
/// entries carry the OS or resolver cause ("dns error: failed to lookup
/// address information", "Connection refused").
pub(crate) fn reqwest_error_chain(e: &reqwest::Error) -> String {
    let mut out = e.to_string();
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

/// Classify a reqwest error into a short, specific phrase by walking its source
/// chain to the deepest OS/resolver cause. Appends the host when known, e.g.
/// "DNS lookup failed (www.dialagram.me)" or "connection refused (api.x.com)".
/// A timeout has no usable chain, so it is split by phase instead: see
/// [`describe_timeout`].
pub(crate) fn describe_reqwest_error(e: &reqwest::Error) -> String {
    let host = e.url().and_then(|u| u.host_str()).map(str::to_owned);
    let host_suffix = host
        .as_deref()
        .map(|h| format!(" ({h})"))
        .unwrap_or_default();

    if e.is_timeout() {
        // #2204: one deadline covers DNS, TCP and TLS together, and on expiry
        // hyper reports "tcp connect error" whichever phase actually hung. A
        // resolver that never answered therefore reads as a slow server. The
        // message cannot carry the phase, so re-probe the name once and let
        // that decide. Only a request that already failed pays for this.
        let probe = match host.as_deref() {
            Some(h) if e.is_connect() => probe_dns(h, DNS_PROBE_TIMEOUT),
            _ => DnsProbe::Skipped,
        };
        return describe_timeout(e.is_connect(), probe, &host_suffix);
    }

    // The deepest source-chain entry carries the real OS/resolver error; the
    // reqwest top-level Display ("error sending request for url …") does not.
    let mut deepest: Option<String> = None;
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = src {
        deepest = Some(s.to_string());
        src = s.source();
    }
    let detail = deepest.unwrap_or_else(|| e.to_string());
    let low = detail.to_ascii_lowercase();

    let label = if low.contains("dns")
        || low.contains("lookup address")
        || low.contains("nodename nor servname")
        || low.contains("name or service not known")
        || low.contains("no such host")
        || low.contains("failed to resolve")
        || low.contains("could not resolve")
    {
        "DNS lookup failed"
    } else if low.contains("connection refused") {
        "connection refused"
    } else if low.contains("connection reset") {
        "connection reset by peer"
    } else if low.contains("network is unreachable") {
        "network unreachable"
    } else if low.contains("no route to host") {
        "no route to host"
    } else if low.contains("timed out") || low.contains("timeout") {
        "timed out"
    } else if low.contains("certificate")
        || low.contains("tls")
        || low.contains("ssl")
        || low.contains("handshake")
    {
        "TLS/certificate error"
    } else {
        // Unknown shape — surface the real deepest cause itself, trimmed, so
        // we never hide what happened behind a generic label.
        let trimmed: String = detail.chars().take(140).collect();
        return format!("{trimmed}{host_suffix}");
    };
    format!("{label}{host_suffix}")
}

/// How long the error path waits for a name to resolve before it gives up on
/// naming the cause. A healthy resolver answers in milliseconds; a broken one
/// hits its own multi-second timeout (the reported machine saw `Resolving
/// timed out after 10000 milliseconds`), so this only has to separate the two.
/// Kept at one second because the probe runs while the turn is being handed
/// back, so a failed request's diagnosis must not add more than a second.
const DNS_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Outcome of re-resolving the host after a connect-phase timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DnsProbe {
    /// Nothing to probe: the URL carried no host, or the request did not time
    /// out in the connect phase.
    Skipped,
    /// The name resolved, so the wait was the connection itself.
    Resolved,
    /// The resolver answered with an error.
    Failed,
    /// The resolver did not answer inside the probe window.
    TimedOut,
}

/// Label a timeout by the phase it fell in. `is_connect` comes from reqwest and
/// is true only when the connection was never established, so a timeout after
/// the socket is up keeps the plain request label.
pub(crate) fn describe_timeout(is_connect: bool, probe: DnsProbe, host_suffix: &str) -> String {
    if !is_connect {
        return format!("request timed out{host_suffix}");
    }
    match probe {
        DnsProbe::Failed | DnsProbe::TimedOut => format!("DNS lookup failed{host_suffix}"),
        DnsProbe::Resolved | DnsProbe::Skipped => format!("connect timed out{host_suffix}"),
    }
}

/// Resolve `host` once, bounded, off the caller's thread. Called only to name
/// the cause of a request that already failed, so it never sits on a healthy
/// request's path. A resolver that outlives the window leaves one parked
/// thread behind; that is cheaper than reporting the wrong cause.
pub(crate) fn probe_dns(host: &str, timeout: std::time::Duration) -> DnsProbe {
    if host.parse::<std::net::IpAddr>().is_ok() {
        // An address cannot fail to resolve, so the wait was the connection.
        return DnsProbe::Resolved;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let name = host.to_owned();
    let spawned = std::thread::Builder::new()
        .name("dns-probe".into())
        .spawn(move || {
            use std::net::ToSocketAddrs;
            let outcome = match (name.as_str(), 0u16).to_socket_addrs() {
                Ok(mut addrs) => {
                    if addrs.next().is_some() {
                        DnsProbe::Resolved
                    } else {
                        DnsProbe::Failed
                    }
                }
                Err(_) => DnsProbe::Failed,
            };
            let _ = tx.send(outcome);
        });
    if spawned.is_err() {
        return DnsProbe::Skipped;
    }
    match rx.recv_timeout(timeout) {
        Ok(outcome) => outcome,
        Err(_) => DnsProbe::TimedOut,
    }
}

/// Parse a retry-delay (seconds) out of a rate-limit error message.
/// Recognizes "60 seconds", "60s", "retry in 60", "wait 60". Moved here
/// from the former `brain::provider::retry` module when retry logic was
/// consolidated onto `utils::retry`.
fn parse_retry_seconds(msg: &str) -> Option<u64> {
    use regex::Regex;
    let patterns = [
        r"(\d+)\s*seconds?",
        r"(\d+)\s*s\b",
        r"retry in (\d+)",
        r"wait (\d+)",
    ];
    for pattern in patterns {
        if let Ok(re) = Regex::new(pattern)
            && let Some(captures) = re.captures(msg)
            && let Some(num_str) = captures.get(1)
            && let Ok(secs) = num_str.as_str().parse::<u64>()
        {
            return Some(secs);
        }
    }
    None
}
