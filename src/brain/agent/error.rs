//! Agent error types

use crate::brain::provider::ProviderError;
use thiserror::Error;

/// Agent error types
#[derive(Debug, Error)]
pub enum AgentError {
    /// Provider error
    #[error("Provider error: {0}")]
    Provider(#[from] ProviderError),

    /// Database error
    #[error("Database error: {0}")]
    Database(String),

    /// Session not found
    #[error("Session not found: {0}")]
    SessionNotFound(uuid::Uuid),

    /// Invalid request
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    /// Context too large
    #[error("Context too large: {current} tokens exceeds limit of {limit}")]
    ContextTooLarge { current: usize, limit: usize },

    /// Tool execution error
    #[error("Tool execution error: {0}")]
    ToolError(String),

    /// Tool not found
    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    /// Maximum tool iterations exceeded
    #[error("Maximum tool iterations exceeded: {0}")]
    MaxIterationsExceeded(usize),

    /// Operation cancelled by user (e.g. /stop)
    #[error("Cancelled")]
    Cancelled,

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(String),
}

impl AgentError {
    /// Build a `Database` error that PRESERVES the full anyhow cause
    /// chain ("outer context: inner cause: ...").
    ///
    /// The old inline `e.to_string()` at every call site kept only the
    /// outermost anyhow context, so the underlying SQLite error code
    /// (BUSY, FULL, IOERR, LOCKED) vanished from both the user-facing
    /// message and the logs, making field diagnosis of persistence
    /// failures impossible (#974).
    pub fn db(e: anyhow::Error) -> Self {
        AgentError::Database(format!("{:#}", e))
    }
}

/// Result type for agent operations
pub type Result<T> = std::result::Result<T, AgentError>;

/// Translate a raw `AgentError` into a user-readable failure message
/// that explains what self-heal already tried and what the user can do.
///
/// Used by both the TUI (`messaging.rs::agent_task` Err path → shown
/// as a permanent chat bubble) and channel handlers (Telegram /
/// Discord / Slack / WhatsApp post a message with this text). Without
/// this helper, both surfaces leaked the raw `Provider error: API
/// error (502): HTTP 502: error code: 502` to users — completely
/// uninformative, and on the TUI it auto-dismissed after 2.5s as a
/// transient toast so users often missed it entirely. Result: turn
/// looks like the agent silently dropped the request.
///
/// Patterns translated:
///   - HTTP 5xx (502 / 503 / 504) → explain that fallback chain
///     exhausted, likely shared upstream gateway outage, retry hint
///   - HTTP 429 → rate limit, wait or switch provider
///   - HTTP 4xx (other) → quote status, recommend provider switch
///   - Stream decode / `error decoding response body` → connection
///     dropped mid-response, try again or switch model
///   - Repetition guard fired → model stuck in loop, terminated
///   - Context too large → quote token counts, suggest /compact
///   - Anything else → fall back to the raw `to_string` so the user
///     at least sees something diagnostic
pub fn format_user_error(err: &AgentError) -> String {
    let raw = err.to_string();
    if raw.contains("error decoding response body") {
        return "Provider stream broke mid-response (connection dropped). \
                Self-heal already retried with no luck. Try again or \
                switch to a different model via `/models`."
            .to_string();
    }
    if raw.contains("Repetition detected") || raw.contains("Tool loop detected") {
        // Name what actually ended the turn (#1023). This is OUR loop
        // detector, not the provider: the model kept announcing an action
        // without emitting the call, or repeated near-identical tool calls,
        // and the turn was stopped rather than left to spin (#2465). Blaming
        // the provider sent users looking for provider-side causes, and
        // `/models` "worked" often enough to teach the wrong mental model --
        // a different model usually DOES emit the call, which is why the
        // suggestion stays, now with the real reason attached.
        //
        // When diagnostic context is embedded in the AnnouncementLoop or
        // ToolLoop message (model name, retry/swap/roll counts), surface it so
        // the user sees how many recovery attempts were exhausted before the
        // turn was killed. Reported by @loonix.
        let diag = extract_loop_diagnostics(&raw);
        let counts = match &diag {
            Some(d) => format!(
                " Recovery attempted: {} retries, {} provider swaps, \
                 {} roll(s) on model `{}`.",
                d.retries, d.swaps, d.rolls, d.model
            ),
            None => String::new(),
        };
        let what = if raw.contains("Tool loop detected") {
            "near-identical tool calls recurred"
        } else {
            "I kept announcing the same action without actually running it"
        };
        return format!(
            "I stopped the turn: {what}, and the loop detector ended \
             it rather than let it spin. This is usually the model \
             struggling to emit a tool call it has described -- \
             rephrasing the request, or switching models via `/models`, \
             normally clears it.{counts}"
        );
    }
    if let AgentError::ContextTooLarge { current, limit } = err {
        return format!(
            "Context too large: {current} tokens exceeds the {limit}-token \
             limit. Run `/compact` to shrink the conversation, or start \
             a fresh session."
        );
    }
    // HTTP status pattern matchers — look for `API error (XXX)` shape.
    if let Some(status) = extract_http_status(&raw) {
        match status {
            502..=504 => {
                return format!(
                    "All fallback providers returned {status} within the \
                     retry window (likely shared upstream gateway outage). \
                     Self-heal already tried 4 fallbacks. Wait a minute \
                     and retry, or switch provider via `/models`."
                );
            }
            429 => {
                return "Rate limit hit on the active provider. Wait a \
                        minute or switch provider via `/models`."
                    .to_string();
            }
            401 | 403 => {
                return format!(
                    "Authentication failed on the active provider \
                     ({status}). Check your API key in `/models` \
                     or `keys.toml`."
                );
            }
            _ => {
                // #1007: if a fallback chain walked and died, the ledger is
                // appended to the raw error — lead with it instead of
                // burying it inside a truncated details blob.
                if let Some(idx) = raw.find("All providers in the fallback chain failed") {
                    let summary: String = raw[idx..].chars().take(600).collect();
                    return format!("Provider returned HTTP {status}. {summary}");
                }
                // Truncate raw error to avoid dumping hundreds of bytes
                // of HTML from provider error pages into the TUI. Char-based
                // so a multibyte boundary can't panic the slice.
                let display_raw: String = if raw.chars().count() > 200 {
                    format!("{}...", raw.chars().take(200).collect::<String>())
                } else {
                    raw.clone()
                };
                let advice = if (400u16..500).contains(&status) {
                    // #1007: retrying a 4xx repeats the same invalid request
                    // — point at failover setup instead of "try again".
                    "Retrying won't help — the request is invalid for this \
                     provider. Switch provider via `/models`, or set up a \
                     fallback chain ([providers.fallback] in config.toml) so \
                     OpenCrabs fails over automatically."
                } else {
                    "Try again, or switch provider via `/models`."
                };
                return format!("Provider returned HTTP {status}. {advice} Details: {display_raw}");
            }
        }
    }
    raw
}

/// Extract an HTTP status code from a string like
/// `Provider error: API error (502): HTTP 502: error code: 502`.
fn extract_http_status(s: &str) -> Option<u16> {
    let lower = s.to_lowercase();
    for prefix in &["api error (", "http "] {
        if let Some(idx) = lower.find(prefix) {
            let tail = &s[idx + prefix.len()..];
            let num: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !num.is_empty()
                && let Ok(n) = num.parse::<u16>()
                && (100..=599).contains(&n)
            {
                return Some(n);
            }
        }
    }
    None
}

/// Parsed diagnostic context from the `[diagnostics:model=X,retries=N,swaps=M,rolls=K]`
/// suffix embedded in `AnnouncementLoop` error messages by `tool_loop.rs`.
struct LoopDiagnostics {
    model: String,
    retries: u32,
    swaps: u32,
    rolls: u32,
}

/// Extract the `[diagnostics:model=X,retries=N,swaps=M,rolls=K]` suffix
/// from an `AnnouncementLoop` error string. Returns `None` if the suffix
/// is absent or malformed (backward-compatible with older errors that
/// predate the diagnostic embedding).
fn extract_loop_diagnostics(raw: &str) -> Option<LoopDiagnostics> {
    let start = raw.find("[diagnostics:")?;
    let end = raw[start..].find(']')?;
    let payload = &raw[start + "[diagnostics:".len()..start + end];
    let mut model = String::new();
    let mut retries = 0u32;
    let mut swaps = 0u32;
    let mut rolls = 0u32;
    for part in payload.split(',') {
        let (key, val) = part.split_once('=')?;
        match key.trim() {
            "model" => model = val.trim().to_string(),
            "retries" => retries = val.trim().parse().ok()?,
            "swaps" => swaps = val.trim().parse().ok()?,
            "rolls" => rolls = val.trim().parse().ok()?,
            _ => {}
        }
    }
    if model.is_empty() {
        return None;
    }
    Some(LoopDiagnostics {
        model,
        retries,
        swaps,
        rolls,
    })
}
