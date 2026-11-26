//! Brave Search Tool
//!
//! Perform real-time internet searches using the Brave Search API.

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Brave search tool (requires BRAVE_API_KEY)
pub struct BraveSearchTool {
    api_key: String,
}

impl BraveSearchTool {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct BraveSearchInput {
    /// Search query
    pub(crate) query: String,

    /// Maximum number of results to return
    #[serde(default = "default_max_results")]
    pub(crate) max_results: usize,
}

fn default_max_results() -> usize {
    5
}

#[derive(Debug, Deserialize)]
pub(crate) struct BraveResponse {
    pub(crate) web: Option<BraveWebResults>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BraveWebResults {
    pub(crate) results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BraveResult {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) description: Option<String>,
}

#[async_trait]
impl Tool for BraveSearchTool {
    fn name(&self) -> &str {
        "brave_search"
    }

    fn description(&self) -> &str {
        "Search the internet using Brave Search — privacy-focused, \
         independently-indexed, strong on current events, news, and \
         time-sensitive queries (\"what happened today\", recent \
         releases, breaking changes). \
         \n\nPREFERRED over `web_search` when both are available. Pick \
         `exa_search` instead for technical documentation / API \
         references / conceptual queries (neural ranking wins there). \
         Pick the `gh` CLI via `bash` for GitHub-specific lookups. Use \
         `browser_navigate` only when the user explicitly asks for \
         browser interaction or every search route has been exhausted."
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
        let input: BraveSearchInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;

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
        let input: BraveSearchInput = serde_json::from_value(input)?;

        let client = crate::utils::http::shared();

        let url = format!(
            "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
            urlencoding::encode(&input.query),
            input.max_results
        );

        let response = client
            .get(&url)
            .header("X-Subscription-Token", &self.api_key)
            .header("Accept", "application/json")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| ToolError::Execution(format!("Brave search request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!(
                "Brave search failed with status {}: {}",
                status, body
            )));
        }

        let brave_response: BraveResponse = response
            .json()
            .await
            .map_err(|e| ToolError::Execution(format!("Failed to parse Brave response: {}", e)))?;

        let mut output = format!("Search results for: \"{}\"\n\n", input.query);

        let results = brave_response.web.map(|w| w.results).unwrap_or_default();

        if results.is_empty() {
            output.push_str("No results found. Try rephrasing your query.\n");
        } else {
            for (i, result) in results.iter().enumerate() {
                output.push_str(&format!("{}. {}\n", i + 1, result.title));
                output.push_str(&format!("   URL: {}\n", result.url));
                if let Some(desc) = &result.description {
                    output.push_str(&format!("   {}\n", desc));
                }
                output.push('\n');
            }
        }

        Ok(ToolResult::success(output))
    }
}
