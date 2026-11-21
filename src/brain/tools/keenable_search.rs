//! Keenable Search Tool (#2939)
//!
//! Optional engine in the `web_search` fan-out. Registered only when
//! `[providers.web_search.keenable]` is enabled. A key is optional: with
//! none, requests go to the keyless public endpoint and send
//! `X-Keenable-Title: opencrabs`. With a key, they use `/v1/search` and
//! `X-API-Key`.

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const DEFAULT_API_ROOT: &str = "https://api.keenable.ai";
const PUBLIC_TITLE: &str = "opencrabs";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

/// Keenable web search tool. A key is optional; keyless mode uses the
/// public endpoint and always sends the app title header.
pub struct KeenableSearchTool {
    api_key: Option<String>,
    /// Test seam so mockito can stand in for api.keenable.ai.
    api_root: String,
    timeout: Duration,
}

impl KeenableSearchTool {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key: api_key.filter(|k| !k.is_empty()),
            api_root: DEFAULT_API_ROOT.to_string(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_api_root(mut self, api_root: impl Into<String>) -> Self {
        self.api_root = api_root.into();
        self
    }

    #[cfg(test)]
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub(crate) fn uses_key(&self) -> bool {
        self.api_key.is_some()
    }

    pub(crate) fn search_url(&self) -> String {
        let root = self.api_root.trim_end_matches('/');
        if self.uses_key() {
            format!("{root}/v1/search")
        } else {
            format!("{root}/v1/search/public")
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct KeenableSearchInput {
    pub(crate) query: String,
    #[serde(default = "default_max_results")]
    pub(crate) max_results: usize,
}

fn default_max_results() -> usize {
    5
}

#[derive(Debug, Serialize)]
pub(crate) struct KeenableRequest<'a> {
    pub(crate) query: &'a str,
    pub(crate) max_results: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KeenableResponse {
    #[serde(default)]
    pub(crate) results: Vec<KeenableResult>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KeenableResult {
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) snippet: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
}

impl KeenableResult {
    fn evidence(&self) -> Option<&str> {
        self.snippet
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                self.description
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
    }
}

#[async_trait]
impl Tool for KeenableSearchTool {
    fn name(&self) -> &str {
        "keenable_search"
    }

    fn description(&self) -> &str {
        "Search the web with Keenable. Optional engine: only available when \
         configured. Use `web_search` as the default (it fans out across \
         every configured engine, including this one when enabled); call \
         `keenable_search` directly when you specifically want Keenable results."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results (default: 5)",
                    "default": 5,
                    "minimum": 1,
                    "maximum": 10
                }
            },
            "required": ["query"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    fn requires_approval(&self) -> bool {
        false
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let input: KeenableSearchInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {e}")))?;

        if input.query.trim().is_empty() {
            return Err(ToolError::InvalidInput("Query cannot be empty".to_string()));
        }

        if input.max_results == 0 || input.max_results > 10 {
            return Err(ToolError::InvalidInput(
                "max_results must be between 1 and 10".to_string(),
            ));
        }

        Ok(())
    }

    async fn execute(&self, input: Value, _context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: KeenableSearchInput = serde_json::from_value(input)?;

        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| ToolError::Execution(format!("Failed to create HTTP client: {e}")))?;

        let body = KeenableRequest {
            query: &input.query,
            max_results: input.max_results,
        };

        let mut request = client
            .post(self.search_url())
            .header("Content-Type", "application/json")
            .header("X-Keenable-Title", PUBLIC_TITLE)
            .json(&body);

        if let Some(key) = &self.api_key {
            request = request.header("X-API-Key", key);
        }

        let response = request
            .send()
            .await
            .map_err(|e| ToolError::Execution(format!("Keenable search request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!(
                "Keenable search failed with status {status}: {body}"
            )));
        }

        let keenable_response: KeenableResponse = response
            .json()
            .await
            .map_err(|e| ToolError::Execution(format!("Failed to parse Keenable response: {e}")))?;

        let mut output = format!("Search results for: \"{}\"\n\n", input.query);

        let mut written = 0usize;
        for result in &keenable_response.results {
            let Some(url) = result
                .url
                .as_deref()
                .map(str::trim)
                .filter(|u| !u.is_empty())
            else {
                continue;
            };
            written += 1;
            let title = result
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(url);
            output.push_str(&format!("{written}. {title}\n"));
            output.push_str(&format!("   URL: {url}\n"));
            if let Some(desc) = result.evidence() {
                output.push_str(&format!("   {desc}\n"));
            }
            output.push('\n');
        }

        if written == 0 {
            output.push_str("No results found. Try rephrasing your query.\n");
        }

        Ok(ToolResult::success(output))
    }
}
