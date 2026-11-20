//! Client half of the instance rail: talk to another crab's A2A gateway.
//!
//! Three questions, three calls:
//!
//! - is it alive? `GET /a2a/health` (public, so a keyless remote still answers)
//! - what is it working on? `session/list` (this build) or `tasks/list` (a
//!   gateway that has tasks but no ledger listing yet)
//! - what does that conversation say? `session/transcript`
//!
//! Everything here is a thin shell over one JSON-RPC POST. The parsers are
//! pure and take a `serde_json::Value`, so the wire shapes are testable
//! without a socket, and a shape change fails a test instead of painting an
//! empty rail.
//!
//! Failure is typed on purpose: a remote that does not know `session/list`
//! must be distinguishable from one that is down, because the first falls back
//! to `tasks/list` and the second shows an error card. `anyhow` alone would
//! collapse both into a string nobody can branch on.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// How long a health probe waits. The strip polls every few seconds; a dead
/// LAN address (laptop moved networks) must not hold the sweep.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a data call waits. Listing sessions is a SQLite read on the other
/// side; a few seconds is generous and still bounded.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(8);

/// Where one remote instance lives and how to authenticate to it.
#[derive(Debug, Clone)]
pub struct Target<'a> {
    pub base_url: &'a str,
    pub token: Option<&'a str>,
}

impl<'a> Target<'a> {
    pub fn new(base_url: &'a str, token: Option<&'a str>) -> Self {
        Self { base_url, token }
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

/// Why a call failed, in the three shapes a caller branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The remote answered `-32601`: it runs a build without this method.
    MethodNotFound,
    /// The credential was refused (expired, revoked, wrong scopes).
    Unauthorized,
    /// Anything else: transport, 5xx, a malformed body.
    Other,
}

/// A typed call failure. Downcast with `anyhow::Error::downcast_ref`.
#[derive(Debug, Clone)]
pub struct CallError {
    pub failure: Failure,
    pub message: String,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CallError {}

/// The failure kind behind an error, when it is one of ours.
pub fn failure_of(err: &anyhow::Error) -> Option<Failure> {
    err.downcast_ref::<CallError>().map(|e| e.failure)
}

fn call_error(failure: Failure, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CallError {
        failure,
        message: message.into(),
    })
}

/// One remote conversation, as the rail paints it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteSession {
    pub id: String,
    pub title: String,
    /// Unix seconds, the same clock the local rail sorts by.
    pub updated_at: i64,
    pub created_at: i64,
    pub model: Option<String>,
    pub cost: f64,
    pub preview: String,
}

/// One row of a remote conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
}

/// One A2A task the remote is holding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteTask {
    pub id: String,
    pub state: String,
    pub context_id: Option<String>,
    pub updated_at: i64,
}

/// A task's current answer, for the send flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSnapshot {
    pub id: String,
    pub state: String,
    /// Every text part in the task's artifacts plus its status message,
    /// joined. Empty while the task is still working.
    pub text: String,
}

impl TaskSnapshot {
    /// The task has stopped moving: no more polling.
    pub fn settled(&self) -> bool {
        matches!(
            self.state.as_str(),
            "completed" | "failed" | "canceled" | "rejected"
        )
    }
}

/// The JSON-RPC envelope. One shape, every method.
pub fn rpc_body(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    })
}

/// `GET /a2a/health`, timed. `Ok(ms)` when it answered 2xx.
pub async fn probe(target: &Target<'_>) -> Result<u64> {
    let client = reqwest::Client::new();
    let mut req = client
        .get(target.url("/a2a/health"))
        .timeout(PROBE_TIMEOUT);
    if let Some(token) = target.token {
        req = req.bearer_auth(token);
    }
    let started = Instant::now();
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("health answered HTTP {}", resp.status());
    }
    Ok(started.elapsed().as_millis() as u64)
}

/// One JSON-RPC call, result unwrapped. Error codes become [`Failure`]s.
pub async fn call(
    target: &Target<'_>,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let client = reqwest::Client::new();
    let mut req = client
        .post(target.url("/a2a/v1"))
        .timeout(CALL_TIMEOUT)
        .json(&rpc_body(method, params));
    if let Some(token) = target.token {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(call_error(
            Failure::Unauthorized,
            format!("credential refused (HTTP {})", status.as_u16()),
        ));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| call_error(Failure::Other, format!("malformed response: {e}")))?;
    if let Some(err) = body.get("error") {
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        let message = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("remote error");
        // -32601 is JSON-RPC MethodNotFound; the gateway's own table agrees
        // (`error_codes::METHOD_NOT_FOUND`).
        let failure = if code == -32601 {
            Failure::MethodNotFound
        } else {
            Failure::Other
        };
        return Err(call_error(failure, message.to_string()));
    }
    body.get("result")
        .cloned()
        .ok_or_else(|| call_error(Failure::Other, "response carried no result"))
}

/// The remote's conversations, newest first, as it reports them.
pub async fn list_sessions(target: &Target<'_>, limit: usize) -> Result<Vec<RemoteSession>> {
    let result = call(target, "session/list", serde_json::json!({ "limit": limit })).await?;
    parse_sessions(&result)
}

/// One remote conversation's rows.
pub async fn transcript(
    target: &Target<'_>,
    session_id: &str,
    limit: usize,
) -> Result<(String, Vec<RemoteMessage>)> {
    let result = call(
        target,
        "session/transcript",
        serde_json::json!({ "sessionId": session_id, "limit": limit }),
    )
    .await?;
    parse_transcript(&result)
}

/// The remote's A2A tasks: the level-1 view of a crab whose build has no
/// `session/list`.
pub async fn list_tasks(target: &Target<'_>, limit: usize) -> Result<Vec<RemoteTask>> {
    let result = call(target, "tasks/list", serde_json::json!({ "limit": limit })).await?;
    parse_tasks(&result)
}

/// Start a task on the remote. Returns its id; the caller polls [`task`].
pub async fn send(target: &Target<'_>, text: &str) -> Result<String> {
    let result = call(
        target,
        "message/send",
        serde_json::json!({
            "message": {
                "role": "user",
                "parts": [{ "text": text }],
            }
        }),
    )
    .await?;
    result
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| call_error(Failure::Other, "send returned no task id"))
}

/// Poll one task's state and answer.
pub async fn task(target: &Target<'_>, task_id: &str) -> Result<TaskSnapshot> {
    let result = call(target, "tasks/get", serde_json::json!({ "id": task_id })).await?;
    Ok(parse_task(&result))
}

// ─── Pure parsers: the wire shapes, tested without a socket ──────────────

pub fn parse_sessions(value: &serde_json::Value) -> Result<Vec<RemoteSession>> {
    let list = value
        .get("sessions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| call_error(Failure::Other, "session/list carried no sessions array"))?;
    Ok(list
        .iter()
        .map(|s| RemoteSession {
            id: s
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            title: s
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("untitled")
                .to_string(),
            updated_at: s.get("updatedAt").and_then(|v| v.as_i64()).unwrap_or(0),
            created_at: s.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0),
            model: s.get("model").and_then(|v| v.as_str()).map(str::to_string),
            cost: s.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0),
            preview: s
                .get("preview")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
        .collect())
}

pub fn parse_transcript(value: &serde_json::Value) -> Result<(String, Vec<RemoteMessage>)> {
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("remote conversation")
        .to_string();
    let list = value
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| call_error(Failure::Other, "session/transcript carried no messages array"))?;
    let messages = list
        .iter()
        .map(|m| RemoteMessage {
            id: m
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            role: m
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("assistant")
                .to_string(),
            content: m
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            created_at: m.get("createdAt").and_then(|v| v.as_i64()).unwrap_or(0),
        })
        .collect();
    Ok((title, messages))
}

pub fn parse_tasks(value: &serde_json::Value) -> Result<Vec<RemoteTask>> {
    let list = value
        .get("tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| call_error(Failure::Other, "tasks/list carried no tasks array"))?;
    Ok(list
        .iter()
        .map(|t| RemoteTask {
            id: t
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            state: t
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            context_id: t
                .get("contextId")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            updated_at: t.get("updatedAt").and_then(|v| v.as_i64()).unwrap_or(0),
        })
        .collect())
}

pub fn parse_task(value: &serde_json::Value) -> TaskSnapshot {
    let state = value
        .get("status")
        .and_then(|s| s.get("state"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut text = String::new();
    if let Some(message) = value
        .get("status")
        .and_then(|s| s.get("message"))
        .and_then(|m| m.get("parts"))
        .and_then(|p| p.as_array())
    {
        for part in message {
            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
        }
    }
    if let Some(artifacts) = value.get("artifacts").and_then(|a| a.as_array()) {
        for artifact in artifacts {
            if let Some(parts) = artifact.get("parts").and_then(|p| p.as_array()) {
                for part in parts {
                    if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
            }
        }
    }
    TaskSnapshot {
        id: value
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        state,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_is_jsonrpc_2() {
        let body = rpc_body("session/list", serde_json::json!({"limit": 5}));
        assert_eq!(body["jsonrpc"], "2.0");
        assert_eq!(body["method"], "session/list");
        assert_eq!(body["params"]["limit"], 5);
    }

    #[test]
    fn sessions_parse_with_defaults_never_panic() {
        let value = serde_json::json!({
            "sessions": [
                {"id": "a", "title": "plan the rail", "updatedAt": 10, "createdAt": 5,
                 "model": "deepseek-v4.1-flash", "cost": 1.5, "preview": "hello"},
                {"id": "b"}
            ]
        });
        let rows = parse_sessions(&value).expect("array present");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "plan the rail");
        assert_eq!(rows[0].cost, 1.5);
        assert_eq!(rows[1].title, "untitled", "a missing title has a name");
        assert_eq!(rows[1].updated_at, 0);
    }

    #[test]
    fn a_missing_sessions_array_is_an_error_not_an_empty_rail() {
        let err = parse_sessions(&serde_json::json!({"nope": 1})).unwrap_err();
        assert_eq!(failure_of(&err), Some(Failure::Other));
    }

    #[test]
    fn transcript_keeps_order_and_roles() {
        let value = serde_json::json!({
            "title": "remote chat",
            "messages": [
                {"id": "1", "role": "user", "content": "oi", "createdAt": 1},
                {"id": "2", "role": "assistant", "content": "ola", "createdAt": 2}
            ]
        });
        let (title, rows) = parse_transcript(&value).unwrap();
        assert_eq!(title, "remote chat");
        assert_eq!(rows[0].role, "user");
        assert_eq!(rows[1].content, "ola");
    }

    #[test]
    fn tasks_parse_with_the_camel_case_context() {
        let value = serde_json::json!({
            "tasks": [{"id": "t1", "state": "working", "contextId": "c1", "updatedAt": 7}]
        });
        let tasks = parse_tasks(&value).unwrap();
        assert_eq!(tasks[0].context_id.as_deref(), Some("c1"));
        assert_eq!(tasks[0].state, "working");
    }

    #[test]
    fn a_task_answer_joins_status_message_and_artifacts() {
        let value = serde_json::json!({
            "id": "t1",
            "status": {
                "state": "completed",
                "message": {"parts": [{"text": "done"}]}
            },
            "artifacts": [{"parts": [{"text": "the answer"}]}]
        });
        let snap = parse_task(&value);
        assert_eq!(snap.state, "completed");
        assert!(snap.settled());
        assert_eq!(snap.text, "done\nthe answer");
    }

    #[test]
    fn a_working_task_is_not_settled() {
        let snap = parse_task(&serde_json::json!({
            "id": "t1",
            "status": {"state": "working"}
        }));
        assert!(!snap.settled());
        assert!(snap.text.is_empty());
    }

    #[test]
    fn targets_join_urls_without_double_slashes() {
        let t = Target::new("http://192.168.1.7:8080/", None);
        assert_eq!(t.url("/a2a/health"), "http://192.168.1.7:8080/a2a/health");
        assert_eq!(t.url("a2a/v1"), "http://192.168.1.7:8080/a2a/v1");
    }
}
