//! A2A Send Tool
//!
//! Agent-callable tool for sending tasks to remote A2A agents.
//! Supports discovery, message/send, tasks/get, and tasks/cancel.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolHints, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

use crate::remote::{route_client, route_store::RouteStore};

/// Tool for communicating with remote A2A agents.
#[derive(Default)]
pub struct A2aSendTool;

impl A2aSendTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for A2aSendTool {
    fn name(&self) -> &str {
        "a2a_send"
    }

    fn description(&self) -> &str {
        "Communicate with remote A2A (Agent-to-Agent) agents. \
         Actions: 'discover' to fetch an agent's capabilities, \
         'send' to send a task message, 'get' to check task status, \
         'cancel' to cancel a running task. \
         Target the remote agent with EITHER its base URL or the 'profile' \
         parameter naming a local profile (see profile_list); exactly one is \
         required."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["discover", "send", "get", "cancel"],
                    "description": "'discover' to fetch agent card, 'send' to send a task, 'get' to check task status, 'cancel' to cancel a task"
                },
                "url": {
                    "type": "string",
                    "description": "Base URL of the remote A2A agent (e.g. http://127.0.0.1:18790). Mutually exclusive with 'profile'"
                },
                "profile": {
                    "type": "string",
                    "description": "Name of a local profile to target (see profile_list). Resolves via that profile's advertise_url, else http://bind:port. Mutually exclusive with 'url'"
                },
                "message": {
                    "type": "string",
                    "description": "Text message to send to the remote agent (required for 'send')"
                },
                "task_id": {
                    "type": "string",
                    "description": "Task ID (required for 'get' and 'cancel')"
                },
                "context_id": {
                    "type": "string",
                    "description": "Optional context ID for continuing a conversation"
                },
                "api_key": {
                    "type": "string",
                    "description": "Optional Bearer token for authenticated A2A endpoints"
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    fn hints(&self) -> ToolHints {
        ToolHints {
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        }
    }

    fn requires_approval(&self) -> bool {
        true
    }

    fn requires_approval_for_input(&self, input: &Value) -> bool {
        // Discovery is read-only, no approval needed
        let action = input.get("action").and_then(|v| v.as_str()).unwrap_or("");
        action != "discover"
    }

    async fn execute(&self, input: Value, _context: &ToolExecutionContext) -> Result<ToolResult> {
        let action = input.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let base_url: String = match (
            input
                .get("url")
                .and_then(|v| v.as_str())
                .filter(|u| !u.is_empty()),
            input
                .get("profile")
                .and_then(|v| v.as_str())
                .filter(|p| !p.is_empty()),
        ) {
            (Some(_), Some(_)) => {
                return Ok(ToolResult::error(
                    "Parameters 'url' and 'profile' are mutually exclusive - provide only one."
                        .to_string(),
                ));
            }
            (Some(u), None) => u.trim_end_matches('/').to_string(),
            (None, Some(p)) => match resolve_profile_target(p) {
                Ok(url) => url,
                Err(msg) => return Ok(ToolResult::error(msg)),
            },
            (None, None) => {
                return Ok(ToolResult::error(
                    "Missing target - provide either 'url' or 'profile'.".to_string(),
                ));
            }
        };
        let api_key = input.get("api_key").and_then(|v| v.as_str());

        // Route-aware resolution: race when the store knows 2+ ways to
        // reach this host; otherwise pass through untouched (no store
        // entry = exactly the pre-Phase-2 behavior).
        let route_store = RouteStore::new(RouteStore::default_path());
        let base_url = route_client::resolve_best_url(&route_store, &base_url).await;
        let base_url: &str = &base_url;

        match action {
            "discover" => {
                let result = discover(base_url, api_key).await;
                // Harvest announced routes while we're here (auth-gated on
                // the server; silently no-ops for keyless/anonymous remotes).
                if let Ok(Some((host_id, n))) =
                    route_client::harvest(&route_store, base_url, api_key).await
                {
                    tracing::debug!(host = %host_id, routes = n, "learned routes via discover");
                }
                result
            }
            "send" => {
                let message = match input.get("message").and_then(|v| v.as_str()) {
                    Some(m) if !m.is_empty() => m,
                    _ => {
                        return Ok(ToolResult::error(
                            "Missing required parameter 'message' for 'send' action.".to_string(),
                        ));
                    }
                };
                let context_id = input.get("context_id").and_then(|v| v.as_str());
                send_message(base_url, api_key, message, context_id).await
            }
            "get" => {
                let task_id = match input.get("task_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        return Ok(ToolResult::error(
                            "Missing required parameter 'task_id' for 'get' action.".to_string(),
                        ));
                    }
                };
                get_task(base_url, api_key, task_id).await
            }
            "cancel" => {
                let task_id = match input.get("task_id").and_then(|v| v.as_str()) {
                    Some(id) if !id.is_empty() => id,
                    _ => {
                        return Ok(ToolResult::error(
                            "Missing required parameter 'task_id' for 'cancel' action.".to_string(),
                        ));
                    }
                };
                cancel_task(base_url, api_key, task_id).await
            }
            other => Ok(ToolResult::error(format!(
                "Unknown action '{other}'. Valid: discover, send, get, cancel"
            ))),
        }
    }
}

pub(crate) fn auth_headers(api_key: Option<&str>) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(key) = api_key
        && let Ok(val) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", key))
    {
        headers.insert(reqwest::header::AUTHORIZATION, val);
    }
    headers
}

/// Discover a remote agent's capabilities via their Agent Card.
async fn discover(base_url: &str, api_key: Option<&str>) -> Result<ToolResult> {
    let url = format!("{}/.well-known/agent.json", base_url);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .headers(auth_headers(api_key))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| super::error::ToolError::Execution(format!("HTTP error: {}", e)))?;

    if !resp.status().is_success() {
        return Ok(ToolResult::error(format!(
            "Agent discovery failed: HTTP {}",
            resp.status()
        )));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| super::error::ToolError::Execution(format!("JSON parse error: {}", e)))?;

    let pretty = serde_json::to_string_pretty(&body).unwrap_or_default();
    Ok(ToolResult::success(format!(
        "Agent Card from {}:\n{}",
        base_url, pretty
    )))
}

/// Send a message/task to a remote A2A agent.
async fn send_message(
    base_url: &str,
    api_key: Option<&str>,
    message: &str,
    context_id: Option<&str>,
) -> Result<ToolResult> {
    let url = format!("{}/a2a/v1", base_url);

    let mut msg = serde_json::json!({
        "role": "user",
        "parts": [{"text": message}]
    });
    if let Some(ctx) = context_id {
        msg["contextId"] = serde_json::json!(ctx);
    }

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "message/send",
        "params": {
            "message": msg
        },
        "id": 1
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .headers(auth_headers(api_key))
        .json(&body)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| super::error::ToolError::Execution(format!("HTTP error: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Ok(ToolResult::error(format!(
            "A2A send failed: HTTP {} — {}",
            status, text
        )));
    }

    let rpc_response: Value = resp
        .json()
        .await
        .map_err(|e| super::error::ToolError::Execution(format!("JSON parse error: {}", e)))?;

    // Check for JSON-RPC error
    if let Some(err) = rpc_response.get("error") {
        let msg = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        return Ok(ToolResult::error(format!("A2A error: {}", msg)));
    }

    // Extract task result
    let result = &rpc_response["result"];
    let task_id = result["id"].as_str().unwrap_or("unknown");
    let state = result["status"]["state"].as_str().unwrap_or("unknown");

    // Extract response text from artifacts or status message
    let response_text = extract_response_text(result);

    let mut output = format!("Task: {} (state: {})", task_id, state);
    if let Some(ctx) = result["contextId"].as_str() {
        output.push_str(&format!("\nContext: {}", ctx));
    }
    if !response_text.is_empty() {
        output.push_str(&format!("\n\nResponse:\n{}", response_text));
    }

    Ok(ToolResult::success(output))
}

/// Get the status of a task on a remote A2A agent.
async fn get_task(base_url: &str, api_key: Option<&str>, task_id: &str) -> Result<ToolResult> {
    let url = format!("{}/a2a/v1", base_url);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "tasks/get",
        "params": {
            "id": task_id
        },
        "id": 1
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .headers(auth_headers(api_key))
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| super::error::ToolError::Execution(format!("HTTP error: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Ok(ToolResult::error(format!(
            "A2A get failed: HTTP {} — {}",
            status, text
        )));
    }

    let rpc_response: Value = resp
        .json()
        .await
        .map_err(|e| super::error::ToolError::Execution(format!("JSON parse error: {}", e)))?;

    if let Some(err) = rpc_response.get("error") {
        let msg = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        return Ok(ToolResult::error(format!("A2A error: {}", msg)));
    }

    let result = &rpc_response["result"];
    let state = result["status"]["state"].as_str().unwrap_or("unknown");
    let response_text = extract_response_text(result);

    let mut output = format!("Task {} — state: {}", task_id, state);
    if !response_text.is_empty() {
        output.push_str(&format!("\n\nResponse:\n{}", response_text));
    }

    Ok(ToolResult::success(output))
}

/// Cancel a task on a remote A2A agent.
async fn cancel_task(base_url: &str, api_key: Option<&str>, task_id: &str) -> Result<ToolResult> {
    let url = format!("{}/a2a/v1", base_url);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "tasks/cancel",
        "params": {
            "id": task_id
        },
        "id": 1
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .headers(auth_headers(api_key))
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| super::error::ToolError::Execution(format!("HTTP error: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Ok(ToolResult::error(format!(
            "A2A cancel failed: HTTP {} — {}",
            status, text
        )));
    }

    let rpc_response: Value = resp
        .json()
        .await
        .map_err(|e| super::error::ToolError::Execution(format!("JSON parse error: {}", e)))?;

    if let Some(err) = rpc_response.get("error") {
        let msg = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        return Ok(ToolResult::error(format!("A2A error: {}", msg)));
    }

    let state = rpc_response["result"]["status"]["state"]
        .as_str()
        .unwrap_or("unknown");
    Ok(ToolResult::success(format!(
        "Task {} — cancelled (state: {})",
        task_id, state
    )))
}

/// Extract readable text from a task's artifacts and status message.
pub(crate) fn extract_response_text(task: &Value) -> String {
    let mut texts = Vec::new();

    // Check status message
    if let Some(msg) = task.get("status").and_then(|s| s.get("message"))
        && let Some(parts) = msg.get("parts").and_then(|p| p.as_array())
    {
        for part in parts {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                texts.push(text.to_string());
            }
        }
    }

    // Check artifacts
    if let Some(artifacts) = task.get("artifacts").and_then(|a| a.as_array()) {
        for artifact in artifacts {
            if let Some(parts) = artifact.get("parts").and_then(|p| p.as_array()) {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        texts.push(text.to_string());
                    }
                }
            }
        }
    }

    texts.join("\n")
}

/// Error text for targeting an a2a-disabled profile (#1161 wording).
pub(crate) fn profile_disabled_error(name: &str) -> String {
    format!("profile '{name}' has a2a disabled - set [a2a] enabled=true in its config")
}

/// Resolve a local profile name to its reachable A2A URL (#1161):
/// `advertise_url` when set, else `http://{bind}:{port}`. Unknown names get
/// the valid roster listed so the model can self-correct.
fn resolve_profile_target(name: &str) -> std::result::Result<String, String> {
    let profiles = crate::config::profile::list_profiles()
        .map_err(|e| format!("failed to list profiles: {e}"))?;
    if !profiles.iter().any(|p| p.name == name) {
        let valid: Vec<&str> = profiles.iter().map(|p| p.name.as_str()).collect();
        return Err(format!(
            "Unknown profile '{}'. Valid profiles: {}",
            name,
            valid.join(", ")
        ));
    }
    let (cfg, _found) = super::profile_list::load_profile_a2a(name).map_err(|e| e.to_string())?;
    if !cfg.enabled {
        return Err(profile_disabled_error(name));
    }
    Ok(super::profile_list::effective_a2a_url(&cfg))
}
