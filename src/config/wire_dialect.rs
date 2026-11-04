//! Wire dialect selection for [OI]-compatible providers (#2059).
//!
//! An [OI]-compatible endpoint can serve a request two ways: the classic
//! Chat Completions route (`POST {base}/chat/completions`) or the newer
//! Responses route (`POST {base}/responses`). The two are not
//! interchangeable. They take a different request body, return a different
//! response shape, and stream a different event vocabulary, so the route a
//! provider uses is a property of the endpoint, not of the caller.
//!
//! `[providers.<name>] wire_dialect` selects it: `auto`, `chat`, or
//! `responses`. Unset means `auto`, which resolves by provider kind: the
//! official host is `auto` (Chat first, Responses when the endpoint refuses
//! the model), and every other compatible endpoint is `chat`, because a
//! third-party gateway almost never implements `/responses` at all.
//!
//! Pure module: no config, no client, no IO, so the resolution rules are
//! unit tested directly.

use serde::{Deserialize, Serialize};

/// How a request reaches an [OI]-compatible endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WireDialect {
    /// Chat Completions first, Responses only when the endpoint refuses the
    /// model on the chat route. The default for the official host.
    #[default]
    Auto,
    /// Always `POST {base}/chat/completions`.
    Chat,
    /// Always `POST {base}/responses`.
    Responses,
}

impl WireDialect {
    /// The config spelling, for logs and docs.
    pub fn as_str(self) -> &'static str {
        match self {
            WireDialect::Auto => "auto",
            WireDialect::Chat => "chat",
            WireDialect::Responses => "responses",
        }
    }

    /// The route this provider actually uses.
    ///
    /// An explicit `chat` or `responses` is returned unchanged. `auto` (and
    /// an unset key) resolves by provider kind: the official host keeps
    /// `auto`, every other compatible endpoint is pinned to `chat` so a
    /// gateway that has no `/responses` route is never sent there.
    pub fn resolve(configured: Option<Self>, is_official_host: bool) -> Self {
        match configured {
            Some(explicit) => explicit,
            None if is_official_host => WireDialect::Auto,
            None => WireDialect::Chat,
        }
    }
}

/// True when `base_url` points at the official [OI] host, which is the only
/// endpoint assumed to implement the Responses route.
///
/// Deliberately a host check and not a model-name check: routing by model
/// name is the pattern that already failed twice in this tree, because the
/// model list is a moving target no compile-time table keeps up with.
pub(crate) fn is_official_host(base_url: Option<&str>) -> bool {
    base_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some_and(|url| url.to_ascii_lowercase().contains("api.openai.com"))
}

impl<'de> Deserialize<'de> for WireDialect {
    /// Accepts `auto`, `chat`, and `responses`, case-insensitively and
    /// trimmed. Anything else is a hard parse error naming the valid values:
    /// a typo here would otherwise silently pin the provider to Chat and
    /// look like the feature never landed.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(WireDialect::Auto),
            "chat" => Ok(WireDialect::Chat),
            "responses" => Ok(WireDialect::Responses),
            other => Err(serde::de::Error::custom(format!(
                "unknown wire_dialect \"{other}\" (expected one of: auto, chat, responses)"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A bare string is not a valid TOML document (a TOML file is always
    // `key = value` tables), so bare-value parsing is exercised through
    // serde_json, where a JSON string IS a document. The hand-written
    // Deserialize is deserializer-agnostic. The real TOML surface is
    // covered through a ProviderConfig table below.
    use crate::config::ProviderConfig;

    #[test]
    fn each_valid_value_parses() {
        for (raw, expected) in [
            ("\"auto\"", WireDialect::Auto),
            ("\"chat\"", WireDialect::Chat),
            ("\"responses\"", WireDialect::Responses),
        ] {
            let parsed: WireDialect = serde_json::from_str(raw).expect(raw);
            assert_eq!(parsed, expected, "{raw} should parse");
        }
    }

    #[test]
    fn values_parse_case_insensitively_and_trimmed() {
        let parsed: WireDialect =
            serde_json::from_str("\" Responses \"").expect("trimmed + mixed case");
        assert_eq!(parsed, WireDialect::Responses);
    }

    #[test]
    fn garbage_is_rejected_with_the_valid_values_named() {
        let err = serde_json::from_str::<WireDialect>("\"chatgpt\"")
            .expect_err("an unknown dialect must not parse");
        let msg = err.to_string();
        assert!(
            msg.contains("chatgpt") && msg.contains("auto") && msg.contains("responses"),
            "error should name the bad value and the valid ones, got: {msg}"
        );
        assert!(
            toml::from_str::<ProviderConfig>("wire_dialect = \"chatgpt\"").is_err(),
            "the same garbage must also fail inside a provider table"
        );
    }

    #[test]
    fn parses_inside_a_provider_config_table() {
        let parsed: ProviderConfig =
            toml::from_str("wire_dialect = \"Responses\"").expect("a real provider table");
        assert_eq!(parsed.wire_dialect, Some(WireDialect::Responses));

        let chat: ProviderConfig =
            toml::from_str("wire_dialect = \"chat\"").expect("chat in table");
        assert_eq!(chat.wire_dialect, Some(WireDialect::Chat));

        let unset: ProviderConfig = toml::from_str("").expect("an empty table");
        assert_eq!(unset.wire_dialect, None, "unset means resolve() decides");

        let text = toml::to_string(&parsed).expect("serialize a provider table");
        assert!(
            text.contains("wire_dialect = \"responses\""),
            "serializes in the lowercase config spelling, got: {text}"
        );
        assert!(
            !toml::to_string(&unset)
                .expect("serialize an unset table")
                .contains("wire_dialect"),
            "unset must not appear in the serialized table"
        );
    }

    #[test]
    fn defaults_resolve_by_provider_kind() {
        assert_eq!(
            WireDialect::resolve(None, true),
            WireDialect::Auto,
            "official host defaults to auto"
        );
        assert_eq!(
            WireDialect::resolve(None, false),
            WireDialect::Chat,
            "a compatible endpoint defaults to chat"
        );
    }

    #[test]
    fn a_configured_dialect_wins_over_the_kind_default() {
        assert_eq!(
            WireDialect::resolve(Some(WireDialect::Chat), true),
            WireDialect::Chat
        );
        assert_eq!(
            WireDialect::resolve(Some(WireDialect::Responses), false),
            WireDialect::Responses
        );
        assert_eq!(
            WireDialect::resolve(Some(WireDialect::Auto), false),
            WireDialect::Auto
        );
    }

    #[test]
    fn only_the_official_host_is_official() {
        assert!(is_official_host(Some(
            "https://api.openai.com/v1/chat/completions"
        )));
        assert!(is_official_host(Some("https://API.OPENAI.COM/v1")));
        assert!(!is_official_host(Some("https://openrouter.ai/api/v1")));
        assert!(!is_official_host(Some("http://localhost:11434/v1")));
        assert!(!is_official_host(None));
        assert!(!is_official_host(Some("   ")));
    }

    #[test]
    fn round_trips_through_serialization() {
        for dialect in [WireDialect::Auto, WireDialect::Chat, WireDialect::Responses] {
            let text = serde_json::to_string(&dialect).expect("serialize");
            assert_eq!(text, format!("\"{}\"", dialect.as_str()));
            let back: WireDialect = serde_json::from_str(&text).expect("parse");
            assert_eq!(back, dialect);
        }
    }
}
