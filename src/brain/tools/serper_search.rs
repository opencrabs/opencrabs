//! Serper Search Tool (#1731)
//!
//! Google SERP results via the Serper.dev API. Optional engine: registered
//! only when `[providers.web_search.serper]` is enabled with a key
//! (key itself lives in keys.toml). Supports `gl` (country) and `hl`
//! (language) locale targeting, the capability #1661 asked for.

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Serper (Google SERP) search tool, requires an API key from serper.dev
pub struct SerperSearchTool {
    api_key: String,
}

impl SerperSearchTool {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SerperSearchInput {
    /// Search query
    pub(crate) query: String,

    /// Maximum number of results to return
    #[serde(default = "default_max_results")]
    pub(crate) max_results: usize,

    /// Country code for locale targeting (e.g. "us", "pt", "gb")
    #[serde(default)]
    pub(crate) gl: Option<String>,

    /// Language code for locale targeting (e.g. "en", "pt")
    #[serde(default)]
    pub(crate) hl: Option<String>,
}

fn default_max_results() -> usize {
    5
}

/// Request body. `gl`/`hl` are omitted when unset so the API defaults apply.
#[derive(Debug, Serialize)]
pub(crate) struct SerperRequest<'a> {
    pub(crate) q: &'a str,
    pub(crate) num: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gl: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) hl: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SerperResponse {
    #[serde(default)]
    pub(crate) organic: Vec<SerperOrganic>,
}

/// Only the fields we consume are declared; serde ignores the rest
/// (`position`, `date`, `sitelinks`, `attributes`, `knowledgeGraph`, ...).
#[derive(Debug, Deserialize)]
pub(crate) struct SerperOrganic {
    pub(crate) title: String,
    pub(crate) link: String,
    pub(crate) snippet: Option<String>,
}

#[async_trait]
impl Tool for SerperSearchTool {
    fn name(&self) -> &str {
        "serper_search"
    }

    fn description(&self) -> &str {
        "Search Google via the Serper.dev API — classic Google SERP results \
         with `gl` (country) and `hl` (language) locale targeting. \
         \n\nOptional engine: only available when configured. Use `web_search` \
         as the default (it fans out across every configured engine, including \
         this one when enabled); call `serper_search` directly when you \
         specifically need Google-ranked results or locale-scoped results for \
         a region/language. Technical documentation and conceptual queries \
         still belong in `exa_search`."
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
                },
                "gl": {
                    "type": "string",
                    "description": "Country code for locale targeting (e.g. \"us\", \"pt\", \"gb\")",
                    "maxLength": 8
                },
                "hl": {
                    "type": "string",
                    "description": "Language code for locale targeting (e.g. \"en\", \"pt\")",
                    "maxLength": 8
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
        let input: SerperSearchInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;

        if input.query.trim().is_empty() {
            return Err(ToolError::InvalidInput("Query cannot be empty".to_string()));
        }

        if input.max_results == 0 || input.max_results > 10 {
            return Err(ToolError::InvalidInput(
                "max_results must be between 1 and 10".to_string(),
            ));
        }

        for (name, code) in [("gl", input.gl), ("hl", input.hl)] {
            if let Some(code) = code
                && code.trim().is_empty()
            {
                return Err(ToolError::InvalidInput(format!(
                    "{name} cannot be empty when provided"
                )));
            }
        }

        Ok(())
    }

    async fn execute(&self, input: Value, _context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: SerperSearchInput = serde_json::from_value(input)?;

        let client = crate::utils::http::shared();

        let body = SerperRequest {
            q: &input.query,
            num: input.max_results,
            gl: input.gl.as_deref().map(str::trim).filter(|g| !g.is_empty()),
            hl: input.hl.as_deref().map(str::trim).filter(|h| !h.is_empty()),
        };

        let response = client
            .post("https://google.serper.dev/search")
            .header("X-API-KEY", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| ToolError::Execution(format!("Serper search request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!(
                "Serper search failed with status {}: {}",
                status, body
            )));
        }

        let serper_response: SerperResponse = response
            .json()
            .await
            .map_err(|e| ToolError::Execution(format!("Failed to parse Serper response: {}", e)))?;

        let mut output = format!("Search results for: \"{}\"\n\n", input.query);

        if serper_response.organic.is_empty() {
            output.push_str("No results found. Try rephrasing your query.\n");
        } else {
            for (i, result) in serper_response.organic.iter().enumerate() {
                output.push_str(&format!("{}. {}\n", i + 1, result.title));
                output.push_str(&format!("   URL: {}\n", result.link));
                if let Some(desc) = &result.snippet {
                    output.push_str(&format!("   {}\n", desc));
                }
                output.push('\n');
            }
        }

        Ok(ToolResult::success(output))
    }
}
