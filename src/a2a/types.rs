//! A2A Protocol data types — RC v1.0 MVP subset.
//!
//! Reference: <https://a2a-protocol.org/latest/specification/>
//! Implements Section 4 (Protocol Data Model) core objects.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Task Lifecycle ──────────────────────────────────────────

/// Task states per §4.1.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskState {
    Submitted,
    Working,
    Completed,
    Failed,
    Canceled,
    InputRequired,
    Rejected,
    AuthRequired,
}

/// Task status per §4.1.2.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    pub state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// Task — the core unit of action per §4.1.1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

// ─── Message & Part ──────────────────────────────────────────

/// Role per §4.1.5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Agent,
}

/// Part — content container per §4.1.6.
/// A Part MUST contain exactly one of: text, data, url, raw.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

impl Part {
    /// Create a text part.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            text: Some(s.into()),
            data: None,
            url: None,
            media_type: None,
            filename: None,
            metadata: None,
        }
    }
}

/// Message per §4.1.4.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub role: Role,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

// ─── Artifact ────────────────────────────────────────────────

/// Artifact — task output per §4.1.7.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

// ─── Agent Card (Discovery) ─────────────────────────────────

/// Agent Card per §8 — published at `.well-known/agent.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub supported_interfaces: Vec<SupportedInterface>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<AgentProvider>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<AgentCapabilities>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<AgentSkill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_input_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_output_modes: Vec<String>,
}

/// Protocol interface supported by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedInterface {
    pub url: String,
    pub protocol_binding: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
}

/// Agent provider metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProvider {
    pub organization: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Agent capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub push_notifications: bool,
    #[serde(default)]
    pub state_transition_history: bool,
}

/// Agent skill — a capability the agent offers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
}

// ─── JSON-RPC 2.0 ───────────────────────────────────────────

/// JSON-RPC 2.0 request per §9.3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
    pub id: serde_json::Value,
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: serde_json::Value,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    /// Create a success response.
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Create an error response.
    pub fn error(id: serde_json::Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
            id,
        }
    }
}

// ─── Streaming Events (§4.2) ─────────────────────────────────

/// TaskStatusUpdateEvent per §4.2.1 -- sent during message/stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusUpdateEvent {
    pub kind: String, // "status-update"
    pub task_id: String,
    pub context_id: String,
    pub status: TaskStatus,
    #[serde(rename = "final")]
    pub is_final: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// TaskArtifactUpdateEvent per §4.2.2 -- sent during message/stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskArtifactUpdateEvent {
    pub kind: String, // "artifact-update"
    pub task_id: String,
    pub context_id: String,
    pub artifact: Artifact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_chunk: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// Union type for SSE event payloads in message/stream responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StreamEvent {
    Task(Task),
    StatusUpdate(TaskStatusUpdateEvent),
    ArtifactUpdate(TaskArtifactUpdateEvent),
}

// ─── Request Parameters ──────────────────────────────────────

/// SendMessageRequest params per §3.2.1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageParams {
    pub message: Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, serde_json::Value>>,
}

/// GetTask params.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskParams {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_length: Option<u32>,
}

/// CancelTask params.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelTaskParams {
    pub id: String,
}

// ─── A2A Error Codes (§9.5) ─────────────────────────────────

/// Standard A2A error codes.
pub mod error_codes {
    /// JSON-RPC parse error.
    pub const PARSE_ERROR: i64 = -32700;
    /// Invalid JSON-RPC request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Method not found.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid params.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Task not found.
    pub const TASK_NOT_FOUND: i64 = -32001;
    /// Content type not supported.
    pub const CONTENT_TYPE_NOT_SUPPORTED: i64 = -32002;
    /// Unsupported operation.
    pub const UNSUPPORTED_OPERATION: i64 = -32003;
    /// Session token lacks the scope this method requires (paired remote
    /// access; see `crate::remote::scopes`). Distinct from -32001/-32003.
    pub const INSUFFICIENT_SCOPE: i64 = -32004;
}
