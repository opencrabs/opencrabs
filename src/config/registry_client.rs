//! Provider-registry HTTP client.
//!
//! Two GETs and two plain data types: `GET /providers` returns the registry's
//! providers and their models, `GET /health` reports whether it is reachable.
//!
//! This replaced a third-party crate that shipped the registry SERVER in the
//! same package as its client with no feature gate, so importing it for these
//! two calls compiled a whole web stack into every build and carried two
//! advisories with it. The client was a `reqwest::Client` with three lines of
//! glue, and `reqwest` is already a direct dependency, so it lives here now.
//! See the git history for the full accounting.
//!
//! The wire format is unchanged from that crate: field names, renames and
//! serde defaults match exactly, so an existing registry keeps working.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// An AI inference provider and the models it serves.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    /// Display name, e.g. `"Anthropic"`.
    pub name: String,
    /// Stable identifier, e.g. `"anthropic"`.
    pub id: String,
    /// Provider category. Named `type` on the wire.
    #[serde(rename = "type")]
    pub provider_type: String,
    /// API key placeholder, e.g. `"$ANTHROPIC_API_KEY"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Base URL for the provider's API.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_endpoint: Option<String>,
    /// Default model for large or complex work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_large_model_id: Option<String>,
    /// Default model for small or fast work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_small_model_id: Option<String>,
    /// Extra HTTP headers the provider requires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_headers: Option<HashMap<String, String>>,
    /// Models available from this provider. Absent means none, not an error.
    #[serde(default)]
    pub models: Vec<Model>,
}

/// A model's identity, limits and pricing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Model {
    /// Stable identifier, e.g. `"claude-sonnet-4-5-20250929"`.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// USD per 1M input tokens.
    pub cost_per_1m_in: f64,
    /// USD per 1M output tokens.
    pub cost_per_1m_out: f64,
    /// USD per 1M cached input tokens, when the provider prices caching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_1m_in_cached: Option<f64>,
    /// USD per 1M cached output tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_per_1m_out_cached: Option<f64>,
    /// Context window in tokens.
    pub context_window: u64,
    /// Default output-token ceiling.
    pub default_max_tokens: u64,
    /// Whether extended thinking is supported.
    #[serde(default)]
    pub can_reason: bool,
    /// Whether a `reasoning_effort` parameter is accepted.
    #[serde(default)]
    pub has_reasoning_efforts: bool,
    /// Default reasoning effort, when the model has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<String>,
    /// Whether image or file inputs are supported.
    #[serde(default)]
    pub supports_attachments: bool,
}

/// Reads providers from a registry over HTTP.
#[derive(Debug, Clone)]
pub struct RegistryClient {
    base_url: String,
    http: reqwest::Client,
}

impl RegistryClient {
    /// Point a client at a registry's base URL, e.g. `https://registry.example`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: crate::utils::http::shared().clone(),
        }
    }

    /// Build the URL for `path`, tolerating a base URL with or without a
    /// trailing slash so `http://host/` and `http://host` behave the same.
    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }

    /// Every provider the registry knows about.
    ///
    /// Errors on transport failure, a non-success status, or a body that does
    /// not parse. A non-2xx is reported with its status rather than being
    /// parsed, so an HTML error page cannot masquerade as an empty registry.
    pub async fn get_providers(&self) -> Result<Vec<Provider>> {
        let response = self.http.get(self.url("providers")).send().await?;
        let status = response.status();
        if !status.is_success() {
            bail!("Failed to get providers: HTTP {status}");
        }
        Ok(response.json().await?)
    }

    /// Whether the registry answers its health endpoint.
    ///
    /// `false` rather than an error when the request fails: callers use this to
    /// decide whether to bother with the registry at all, and an unreachable
    /// server is the answer, not an exception.
    pub async fn health_check(&self) -> Result<bool> {
        match self.http.get(self.url("health")).send().await {
            Ok(response) => Ok(response.status().is_success()),
            Err(_) => Ok(false),
        }
    }
}
