//! Pure routing decisions for the Responses wire dialect (#2059).
//!
//! Dialect selection is deliberately config-first, with a narrow
//! error-based fallback, never a model-name list: routing reasoning models
//! by name already broke twice here (a new model id silently bypasses the
//! list and the request goes to the wrong endpoint). The rule this module
//! implements is:
//!
//! 1. An explicit `wire_dialect` in the provider section wins outright.
//! 2. Otherwise the official OpenAI host resolves to `Auto`, which starts on
//!    Chat Completions and moves to Responses only when the endpoint refuses
//!    the request there (see [`is_dialect_rejection`]).
//! 3. Every other endpoint stays on Chat, which is what all existing
//!    OpenAI-compatible providers expect.

use super::error::ProviderError;

/// Base path of the Chat Completions endpoint this provider posts to.
const CHAT_SUFFIX: &str = "/chat/completions";

/// Derive the Responses endpoint from the URL this provider posts to.
///
/// Handles the shapes a configured `base_url` actually takes:
/// - a full Chat Completions URL (`.../v1/chat/completions`),
/// - a legacy completions URL (`.../v1/completions`),
/// - a version-root base (`.../v1`).
///
/// Returns `None` for any other shape, so an unrecognised endpoint stays on
/// Chat instead of being rewritten into a guess.
pub(crate) fn responses_url(chat_url: &str) -> Option<String> {
    let trimmed = chat_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    // A trailing slash would defeat suffix matching.
    let url = trimmed.strip_suffix('/').unwrap_or(trimmed);
    if let Some(base) = url.strip_suffix(CHAT_SUFFIX) {
        return non_empty_base(base).map(|b| format!("{b}/responses"));
    }
    if let Some(base) = url.strip_suffix("/completions") {
        return non_empty_base(base).map(|b| format!("{b}/responses"));
    }
    if let Some(base) = url.strip_suffix("/v1") {
        return non_empty_base(base).map(|b| format!("{b}/v1/responses"));
    }
    None
}

fn non_empty_base(base: &str) -> Option<&str> {
    if base.is_empty() { None } else { Some(base) }
}

/// Endpoint-shaped phrases that mean "this URL does not speak that dialect".
///
/// Deliberately narrow: a plain validation error on a supported endpoint must
/// NOT trigger a dialect switch, or every bad request would be retried twice.
const DIALECT_REJECTION_NEEDLES: &[&str] = &[
    "unknown url",
    "unknown endpoint",
    "no such endpoint",
    "unrecognized request url",
    "unknown request url",
    "endpoint not found",
    "unsupported endpoint",
    "not a chat model",
    "does not support the chat completions",
    "does not support the responses",
    "only supported in v1/responses",
    "please use v1/responses",
];

/// True when a failure means the endpoint does not accept this dialect, so
/// exactly one attempt on the other dialect is worth making.
///
/// A 404 is the unambiguous signal (the route does not exist there); 400/422
/// need an endpoint-shaped message because a plain validation error means the
/// route exists and the request was wrong.
pub(crate) fn is_dialect_rejection(status: u16, message: &str) -> bool {
    if status == 404 {
        return true;
    }
    if status != 400 && status != 422 {
        return false;
    }
    let lower = message.to_ascii_lowercase();
    DIALECT_REJECTION_NEEDLES.iter().any(|n| lower.contains(n))
}

/// [`is_dialect_rejection`] over a provider error, if it carries an HTTP
/// status at all. Transport errors, timeouts and rate limits never count:
/// they say nothing about which dialect the endpoint speaks.
pub(crate) fn is_dialect_rejection_error(err: &ProviderError) -> bool {
    match err {
        ProviderError::ApiError {
            status, message, ..
        } => is_dialect_rejection(*status, message),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_url_rewrites_a_chat_completions_url() {
        assert_eq!(
            responses_url("https://api.openai.com/v1/chat/completions").as_deref(),
            Some("https://api.openai.com/v1/responses")
        );
        assert_eq!(
            responses_url("http://localhost:1234/v1/chat/completions").as_deref(),
            Some("http://localhost:1234/v1/responses")
        );
    }

    #[test]
    fn responses_url_accepts_a_trailing_slash() {
        assert_eq!(
            responses_url("https://api.openai.com/v1/chat/completions/").as_deref(),
            Some("https://api.openai.com/v1/responses")
        );
    }

    #[test]
    fn responses_url_handles_version_root_and_legacy_completions() {
        assert_eq!(
            responses_url("https://api.openai.com/v1").as_deref(),
            Some("https://api.openai.com/v1/responses")
        );
        assert_eq!(
            responses_url("https://gateway.example/v1/completions").as_deref(),
            Some("https://gateway.example/v1/responses")
        );
    }

    #[test]
    fn responses_url_refuses_unknown_shapes() {
        assert_eq!(responses_url(""), None);
        assert_eq!(responses_url("   "), None);
        assert_eq!(responses_url("https://api.openai.com"), None);
        assert_eq!(responses_url("/chat/completions"), None);
        assert_eq!(
            responses_url("https://host/some/other/path"),
            None,
            "an unrecognised endpoint must stay on chat, never be guessed"
        );
    }

    #[test]
    fn a_404_is_always_a_dialect_rejection() {
        assert!(is_dialect_rejection(404, ""));
        assert!(is_dialect_rejection(
            404,
            "The model `gpt-6` does not exist or you do not have access to it."
        ));
    }

    #[test]
    fn a_400_rejects_only_on_endpoint_shaped_messages() {
        assert!(is_dialect_rejection(
            400,
            "Unknown request URL: POST /v1/chat/completions"
        ));
        assert!(is_dialect_rejection(
            400,
            "This model does not support the chat completions endpoint, please use v1/responses"
        ));
        assert!(
            !is_dialect_rejection(400, "Invalid value for 'temperature': expected a number"),
            "a plain validation error must not trigger a dialect switch"
        );
        assert!(
            !is_dialect_rejection(422, "Schema validation failed (2 field(s)): tools: extra"),
            "a 422 schema error is about the request, not the dialect"
        );
    }

    #[test]
    fn non_http_statuses_never_trigger_a_switch() {
        assert!(!is_dialect_rejection(500, "internal error"));
        assert!(!is_dialect_rejection(401, "invalid api key"));
        assert!(!is_dialect_rejection(429, "rate limited"));
    }

    #[test]
    fn error_adapter_only_matches_api_errors() {
        assert!(is_dialect_rejection_error(&ProviderError::ApiError {
            status: 404,
            message: "no such endpoint".to_string(),
            error_type: None,
        }));
        assert!(!is_dialect_rejection_error(&ProviderError::Timeout(30)));
        assert!(!is_dialect_rejection_error(
            &ProviderError::RateLimitExceeded("slow down".to_string())
        ));
        assert!(!is_dialect_rejection_error(&ProviderError::InvalidApiKey));
    }
}
