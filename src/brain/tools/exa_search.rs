//! EXA Search Tool
//!
//! Perform real-time internet searches using the EXA AI search API.
//! Supports two modes:
//! - **MCP mode (default):** Free, no API key — uses hosted MCP endpoint at `mcp.exa.ai`
//! - **Direct API mode:** When `EXA_API_KEY` is set — higher rate limits

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

const MCP_ENDPOINT: &str = "https://mcp.exa.ai/mcp";
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

/// EXA search tool — works out of the box via free MCP endpoint.
/// Set `EXA_API_KEY` for direct API access with higher rate limits.
pub struct ExaSearchTool {
    pub(crate) api_key: Option<String>,
    mcp_session_id: Arc<RwLock<Option<String>>>,
}

impl ExaSearchTool {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key,
            mcp_session_id: Arc::new(RwLock::new(None)),
        }
    }

    pub(crate) fn use_mcp(&self) -> bool {
        self.api_key.as_ref().is_none_or(|k| k.is_empty())
    }

    /// Initialize an MCP session and return the session ID if the server
    /// issued one. Returns `None` for stateless servers.
    ///
    /// Every exa_search failure in the 2026-04-16/17 logs (5/5 = 100%)
    /// was `"MCP server did not return session ID"`. The original code
    /// treated a missing `Mcp-Session-Id` response header as a terminal
    /// error. MCP Streamable HTTP transport (protocol rev 2025-03-26+)
    /// lets servers operate in **stateless** mode — they process each
    /// JSON-RPC request standalone without tracking sessions, and skip
    /// setting the header. EXA's hosted endpoint apparently migrated to
    /// that mode.
    ///
    /// Behaviour now:
    ///   - header present → store it, send the `initialized` notification
    ///     against the session, return `Some(id)`
    ///   - header absent  → log a debug note, skip the notification
    ///     (there's nothing to target), return `None`. Subsequent
    ///     tool calls omit the `Mcp-Session-Id` header entirely.
    ///
    /// On any non-2xx init response we still fail loudly with the
    /// server's status + body so a real breakage (auth, 500, etc.)
    /// stays diagnosable instead of getting silently swallowed.
    async fn init_mcp_session(&self, client: &reqwest::Client) -> Result<Option<String>> {
        self.init_mcp_session_at(client, MCP_ENDPOINT).await
    }

    /// Inner initialize — endpoint parameterised so tests can point it
    /// at a mockito server. Production callers go through the wrapper
    /// above which pins `MCP_ENDPOINT`.
    pub(crate) async fn init_mcp_session_at(
        &self,
        client: &reqwest::Client,
        endpoint: &str,
    ) -> Result<Option<String>> {
        let init_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "opencrabs",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        });

        let response = client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&init_request)
            .send()
            .await
            .map_err(|e| ToolError::Execution(format!("MCP initialize failed: {}", e)))?;

        let status = response.status();
        let session_id = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(ToolError::Execution(format!(
                "MCP initialize returned {}: {}",
                status,
                body.chars().take(500).collect::<String>()
            )));
        }

        if let Some(ref id) = session_id {
            tracing::debug!("MCP: using session {}", id);
            // Send initialized notification — required by spec for
            // session-ful transports, skipped entirely in stateless
            // mode below.
            let notification = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            });
            // Fire-and-forget notification: we only care that it was sent
            // (errors propagate via `?`); the response body is unused.
            client
                .post(endpoint)
                .header("Content-Type", "application/json")
                .header("Mcp-Session-Id", id)
                .json(&notification)
                .send()
                .await
                .map_err(|e| {
                    ToolError::Execution(format!("MCP initialized notification failed: {}", e))
                })?;
            *self.mcp_session_id.write().await = Some(id.clone());
        } else {
            tracing::debug!("MCP: server did not set Mcp-Session-Id header — using stateless mode");
            *self.mcp_session_id.write().await = None;
        }

        Ok(session_id)
    }

    /// Get or create an MCP session. Returns `None` in stateless mode
    /// (the server didn't issue a session on initialize).
    async fn ensure_mcp_session(&self, client: &reqwest::Client) -> Result<Option<String>> {
        if let Some(ref id) = *self.mcp_session_id.read().await {
            return Ok(Some(id.clone()));
        }
        self.init_mcp_session(client).await
    }

    /// Execute search via free hosted MCP endpoint.
    async fn execute_via_mcp(
        &self,
        query: &str,
        num_results: usize,
        search_type: &str,
    ) -> Result<ToolResult> {
        // The MCP walk makes several calls with one client, and the ceiling is
        // part of that client, so it lives in a slot and is built once.
        static MCP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        let client = crate::utils::http::cached_client(&MCP_CLIENT, |b| {
            b.timeout(std::time::Duration::from_secs(30))
        });

        // Try with existing session, re-init on 404
        let result = self
            .try_mcp_tool_call(client, query, num_results, search_type)
            .await;

        match result {
            Ok(tool_result) => Ok(tool_result),
            Err(ToolError::Execution(msg)) if msg.contains("404") || msg.contains("session") => {
                // Session expired — re-initialize
                tracing::info!("MCP session expired, re-initializing");
                *self.mcp_session_id.write().await = None;
                self.try_mcp_tool_call(client, query, num_results, search_type)
                    .await
            }
            Err(e) => Err(e),
        }
    }

    /// Perform a single MCP tools/call request.
    async fn try_mcp_tool_call(
        &self,
        client: &reqwest::Client,
        query: &str,
        num_results: usize,
        search_type: &str,
    ) -> Result<ToolResult> {
        let session_id = self.ensure_mcp_session(client).await?;

        let tool_call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": query,
                    "numResults": num_results,
                    "type": search_type
                }
            }
        });

        // In stateless mode we omit the Mcp-Session-Id header entirely
        // — sending a bogus/empty value would break some servers.
        let mut req = client
            .post(MCP_ENDPOINT)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&tool_call);
        if let Some(ref id) = session_id {
            req = req.header("Mcp-Session-Id", id);
        }
        let response = req
            .send()
            .await
            .map_err(|e| ToolError::Execution(format!("MCP tool call failed: {}", e)))?;

        let status = response.status();
        if status.as_u16() == 404 {
            return Err(ToolError::Execution(
                "MCP session expired (404)".to_string(),
            ));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!(
                "EXA MCP search failed with status {}: {}",
                status, body
            )));
        }

        // Parse response — handle both JSON and SSE
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body_text = response.text().await.map_err(|e| {
            ToolError::Execution(format!("Failed to read MCP response body: {}", e))
        })?;

        let json_body = if content_type.contains("text/event-stream") {
            // Parse SSE: extract last "data: " line with a JSON-RPC response
            Self::parse_sse_response(&body_text)?
        } else {
            serde_json::from_str::<Value>(&body_text).map_err(|e| {
                ToolError::Execution(format!("Failed to parse MCP JSON response: {}", e))
            })?
        };

        // Extract result text from JSON-RPC response
        Self::extract_mcp_result(&json_body, query)
    }

    /// Parse SSE response body into the last JSON-RPC message.
    pub(crate) fn parse_sse_response(body: &str) -> Result<Value> {
        let mut last_json = None;
        for line in body.lines() {
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data: ")
                && let Ok(parsed) = serde_json::from_str::<Value>(data)
                && parsed.get("id").is_some()
            {
                last_json = Some(parsed);
            }
        }
        last_json.ok_or_else(|| {
            ToolError::Execution("No JSON-RPC response found in SSE stream".to_string())
        })
    }

    /// Extract the text result from a JSON-RPC tools/call response.
    pub(crate) fn extract_mcp_result(json: &Value, query: &str) -> Result<ToolResult> {
        // Check for JSON-RPC error
        if let Some(error) = json.get("error") {
            let msg = error
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown MCP error");
            return Ok(ToolResult::error(format!("EXA MCP error: {}", msg)));
        }

        // Check for tool execution error
        let result = json.get("result").ok_or_else(|| {
            ToolError::Execution("MCP response missing 'result' field".to_string())
        })?;

        if result.get("isError") == Some(&Value::Bool(true)) {
            let error_text = result
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("Unknown error");
            return Ok(ToolResult::error(format!(
                "EXA search error: {}",
                error_text
            )));
        }

        // Extract content text
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("No results returned");

        let mut output = format!("Search results for: \"{}\"\n\n{}", query, text);
        if output.ends_with('\n') {
            // Already has trailing newline
        } else {
            output.push('\n');
        }

        Ok(ToolResult::success(output))
    }

    /// Execute search via direct EXA API (requires API key).
    async fn execute_via_api(&self, input: &ExaSearchInput) -> Result<ToolResult> {
        let api_key = self.api_key.as_deref().ok_or_else(|| {
            ToolError::Execution("Direct API mode requires EXA_API_KEY".to_string())
        })?;

        let client = crate::utils::http::shared();

        let body = serde_json::json!({
            "query": input.query,
            "num_results": input.max_results,
            "type": input.search_type,
            "contents": {
                "text": true
            }
        });

        let response = client
            .post("https://api.exa.ai/search")
            .header("x-api-key", api_key)
            .header("Content-Type", "application/json")
            .header("x-exa-integration", "opencrabs")
            .json(&body)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| ToolError::Execution(format!("EXA search request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Ok(ToolResult::error(format!(
                "EXA search failed with status {}: {}",
                status, body
            )));
        }

        let exa_response: ExaResponse = response
            .json()
            .await
            .map_err(|e| ToolError::Execution(format!("Failed to parse EXA response: {}", e)))?;

        let mut output = format!("Search results for: \"{}\"\n\n", input.query);

        if exa_response.results.is_empty() {
            output.push_str("No results found. Try rephrasing your query.\n");
        } else {
            for (i, result) in exa_response.results.iter().enumerate() {
                let title = result.title.as_deref().unwrap_or("Untitled");
                output.push_str(&format!("{}. {}\n", i + 1, title));
                output.push_str(&format!("   URL: {}\n", result.url));
                if let Some(text) = &result.text {
                    let snippet: String = text.chars().take(300).collect();
                    output.push_str(&format!("   {}\n", snippet));
                }
                output.push('\n');
            }
        }

        Ok(ToolResult::success(output))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ExaSearchInput {
    /// Search query
    pub(crate) query: String,

    /// Maximum number of results to return
    #[serde(default = "default_max_results")]
    pub(crate) max_results: usize,

    /// Search type: "auto", "neural", "fast", "deep-lite", "deep", "deep-reasoning", or "instant"
    #[serde(default = "default_search_type")]
    pub(crate) search_type: String,
}

fn default_max_results() -> usize {
    5
}

fn default_search_type() -> String {
    "auto".to_string()
}

// Direct API response structures (used only in API mode)
#[derive(Debug, Deserialize)]
struct ExaResponse {
    results: Vec<ExaResult>,
}

#[derive(Debug, Deserialize)]
struct ExaResult {
    title: Option<String>,
    url: String,
    text: Option<String>,
}

#[async_trait]
impl Tool for ExaSearchTool {
    fn name(&self) -> &str {
        "exa_search"
    }

    fn description(&self) -> &str {
        "Search the internet using EXA AI — neural / semantic ranking \
         that beats keyword search for technical documentation, API \
         references, code examples, research papers, and conceptual \
         queries. \
         \n\nPREFERRED over `web_search` when both are available. Use \
         for any research / docs / \"how does X work\" question. Pick \
         `brave_search` instead for current events / news / time-sensitive \
         queries. Pick the `gh` CLI via `bash` for GitHub-specific \
         lookups. Use `browser_navigate` only when the user explicitly \
         asks for browser interaction or every search route has been \
         exhausted."
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
                "search_type": {
                    "type": "string",
                    "description": "Search type: 'auto', 'neural', 'fast', 'deep-lite', 'deep', 'deep-reasoning', or 'instant' (default: 'auto')",
                    "enum": ["auto", "neural", "fast", "deep-lite", "deep", "deep-reasoning", "instant"],
                    "default": "auto"
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
        let input: ExaSearchInput = serde_json::from_value(input.clone())
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
        let parsed: ExaSearchInput = serde_json::from_value(input)?;

        if self.use_mcp() {
            self.execute_via_mcp(&parsed.query, parsed.max_results, &parsed.search_type)
                .await
        } else {
            self.execute_via_api(&parsed).await
        }
    }
}
