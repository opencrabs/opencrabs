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

use std::sync::LazyLock;
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
///
/// Every field here is a field `session/list` sends; see
/// `a2a::handler::sessions::handle_list_sessions` for the emitter. A field
/// added on one side and not the other is how the rail ends up painting 1970.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteSession {
    pub id: String,
    pub title: String,
    /// Unix seconds, the same clock the local rail sorts by.
    pub updated_at: i64,
    pub created_at: i64,
    pub model: Option<String>,
}

/// One row of a remote conversation.
///
/// These are the same columns a local row carries, because the panel paints a
/// remote conversation with the local transcript painter: it decides what a
/// turn is, how reasoning folds, and what the card's footer prints, all from
/// the rows themselves. A row that arrives without them is a row the painter
/// can only draw as bare text, which is why the first remote panel read like a
/// different product (Daniel, 21 Nov 2026).
///
/// Every field past `created_at` is optional and defaults to absent: a host on
/// an older build sends none of them, and `None` here means exactly what it
/// means on a local row, nothing measured, nothing to draw.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RemoteMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    /// The reasoning behind the answer, when the host kept it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// How many calls the turn made, counted the way the live card counts them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<i64>,
    /// How long the turn took, for the card's clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<i64>,
    /// The conversation's context window at the time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_window: Option<i64>,
    /// The turn's own output rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_sec: Option<f64>,
    /// Prompt tokens the model saw on the turn's final call. `TurnMetrics`
    /// reads this as the turn's ctx, so without it the footer has no budget to
    /// print even when the host measured one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    /// Whether the agent took this user row between tool calls instead of it
    /// starting a turn. The transcript uses it to keep one busy period in one
    /// card; without it a remote answer that continued after a tool call is
    /// drawn as a separate turn, and the conversation reads differently from
    /// the same conversation locally.
    #[serde(default)]
    pub continuation: bool,
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

/// The one runtime every remote call shares.
///
/// `reqwest` needs a Tokio reactor, and the window is on gpui's executor, so a
/// runtime has to exist for these calls. The window used to build one per
/// call; measured 21 Nov, the switch to another crab paid that setup on every
/// probe and every fetch, and it paid it again for the client each time.
///
/// The client itself is the process-wide one (`utils::http::shared()`, #2843):
/// one CA load, one pool. What is shared here is the reactor that pool belongs
/// to, because sockets cannot outlive the runtime that made them. Two worker
/// threads is enough: these are a health probe and a JSON-RPC POST, and a
/// deeper pool would only queue against SQLite reads on the other side.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("remote-client")
            .build()
            .expect("a runtime for remote calls")
    });
    &RUNTIME
}

/// Run one remote future to completion on the shared runtime.
///
/// This is the door a non-async caller uses (the window's background tasks).
/// It must not be called from inside another Tokio runtime: that panics, and
/// the panic names it. Everything in the window is on gpui's executor, so the
/// rule holds there by construction.
pub fn block_on<T, F>(call: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    runtime().block_on(call)
}

/// `GET /a2a/health`, timed. `Ok(ms)` when it answered 2xx.
pub async fn probe(target: &Target<'_>) -> Result<u64> {
    let client = crate::utils::http::shared();
    let mut req = client.get(target.url("/a2a/health")).timeout(PROBE_TIMEOUT);
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
    let client = crate::utils::http::shared();
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
    let result = call(
        target,
        "session/list",
        serde_json::json!({ "limit": limit }),
    )
    .await?;
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
//
// These read what `a2a::handler::sessions` actually writes, not a sketch of it.
// The host emits snake_case keys and RFC 3339 timestamps, which is the same
// shape the rest of the A2A surface uses; a camelCase fixture here would pass
// while every stamp in the window read 1970, and did.

/// A wire timestamp as unix seconds.
///
/// RFC 3339 is what the host writes. A bare integer is accepted as well so a
/// fixture, or an older peer, cannot silently become the epoch.
fn unix_seconds(value: Option<&serde_json::Value>) -> i64 {
    let Some(value) = value else {
        return 0;
    };
    if let Some(n) = value.as_i64() {
        return n;
    }
    value
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.timestamp())
        .unwrap_or(0)
}

/// A number the host may or may not have measured. `None` covers both "the
/// host is older and does not send this" and "the row measured nothing", which
/// the painter draws the same way: no segment.
fn opt_i64(value: Option<&serde_json::Value>) -> Option<i64> {
    value.and_then(|v| v.as_i64())
}

/// The same for a rate, which `tokens_per_sec` stores as a float.
fn opt_f64(value: Option<&serde_json::Value>) -> Option<f64> {
    value.and_then(|v| v.as_f64())
}

/// A text column the host may not send. An empty string is not "measured and
/// empty" for reasoning: a host that kept no reasoning sends nothing, so `""`
/// is treated as absent rather than as a fold header with no body.
fn opt_text(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

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
            updated_at: unix_seconds(s.get("updatedAt")),
            created_at: unix_seconds(s.get("createdAt")),
            model: s.get("model").and_then(|v| v.as_str()).map(str::to_string),
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
        .ok_or_else(|| {
            call_error(
                Failure::Other,
                "session/transcript carried no messages array",
            )
        })?;
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
            created_at: unix_seconds(m.get("createdAt")),
            thinking: opt_text(m.get("thinking")),
            tool_calls: opt_i64(m.get("toolCalls")),
            duration_secs: opt_i64(m.get("durationSecs")),
            ctx_window: opt_i64(m.get("ctxWindow")),
            tokens_per_sec: opt_f64(m.get("tokensPerSec")),
            input_tokens: opt_i64(m.get("inputTokens")),
            continuation: m
                .get("continuation")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
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
            updated_at: unix_seconds(t.get("timestamp")),
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

    /// The fixture is the host's own output, copied from
    /// `handle_list_sessions`: snake_case keys, RFC 3339 stamps. A camelCase
    /// fixture here is what let the rail paint 1970 for a whole afternoon
    /// while every test stayed green.
    #[test]
    fn sessions_parse_the_shape_the_host_writes() {
        let value = serde_json::json!({
            "sessions": [
                {
                    "id": "a",
                    "title": "plan the rail",
                    "model": "deepseek-v4.1-flash",
                    "provider": "opencrabs",
                    "updatedAt": "2026-11-20T14:03:11.512+00:00",
                    "createdAt": "2026-11-20T12:00:00+00:00",
                    "archived": false,
                    "workingDirectory": "/home/loonix/x",
                    "tokenCount": 42
                },
                {"id": "b"}
            ]
        });
        let rows = parse_sessions(&value).expect("array present");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "plan the rail");
        assert_eq!(
            rows[0].updated_at, 1_795_183_391,
            "an RFC 3339 stamp is unix seconds, not the epoch"
        );
        assert_eq!(rows[0].created_at, 1_795_176_000);
        assert_eq!(rows[1].title, "untitled", "a missing title has a name");
        assert_eq!(rows[1].updated_at, 0);
    }

    /// The parsers above read what the handler writes. Nothing in the type
    /// system ties the two together, and the last time they drifted the window
    /// painted every stamp as 1970 with every test green. This reads the
    /// handler's source and fails the moment a key it emits is renamed.
    #[test]
    fn the_host_and_the_parser_agree_on_the_keys() {
        let host = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/a2a/handler/sessions.rs"
        ))
        .expect("the handler's source is in the tree");
        for key in [
            "\"updatedAt\"",
            "\"createdAt\"",
            "\"contextId\"",
            "\"timestamp\"",
            "\"title\"",
        ] {
            assert!(
                host.contains(key),
                "session/list, session/transcript or tasks/list stopped emitting {key}; \
                 the parser in this file reads it, so one of the two has to move"
            );
        }
    }

    #[test]
    fn a_bare_integer_stamp_still_reads_as_seconds() {
        let value = serde_json::json!({
            "sessions": [{"id": "a", "updatedAt": 1_700_000_000}]
        });
        let rows = parse_sessions(&value).unwrap();
        assert_eq!(rows[0].updated_at, 1_700_000_000);
    }

    #[test]
    fn a_missing_sessions_array_is_an_error_not_an_empty_rail() {
        let err = parse_sessions(&serde_json::json!({"nope": 1})).unwrap_err();
        assert_eq!(failure_of(&err), Some(Failure::Other));
    }

    #[test]
    fn transcript_keeps_order_and_roles() {
        // `handle_transcript`'s payload, verbatim: snake_case, RFC 3339, and a
        // `sequence` the panel does not draw but the host does send.
        let value = serde_json::json!({
            "sessionId": "3f1a2b3c-0000-4000-8000-000000000000",
            "title": "remote chat",
            "messages": [
                {"id": "1", "role": "user", "content": "oi",
                 "sequence": 1, "createdAt": "2026-11-20T14:00:00+00:00"},
                {"id": "2", "role": "assistant", "content": "ola",
                 "sequence": 2, "createdAt": "2026-11-20T14:01:00+00:00"}
            ]
        });
        let (title, rows) = parse_transcript(&value).unwrap();
        assert_eq!(title, "remote chat");
        assert_eq!(rows[0].role, "user");
        assert_eq!(rows[1].content, "ola");
        assert_eq!(rows[1].created_at, 1_795_183_260);
    }

    /// The panel paints with the local transcript painter, and that painter
    /// reads a turn's reasoning, its tool count, its clock and its ctx budget
    /// off the row. A payload that carries only role+content is a conversation
    /// the painter can only draw as flat text, which is exactly how a remote
    /// session came to look like a different product (Daniel, 21 Nov 2026).
    /// This fixture is `handle_transcript`'s current output, key for key.
    #[test]
    fn a_transcript_row_carries_what_the_painter_reads() {
        let value = serde_json::json!({
            "sessionId": "3f1a2b3c-0000-4000-8000-000000000000",
            "title": "remote chat",
            "messages": [{
                "id": "2", "role": "assistant", "content": "ola",
                "sequence": 2, "createdAt": "2026-11-20T14:01:00+00:00",
                "thinking": "pensei nisto",
                "toolCalls": 3,
                "durationSecs": 7,
                "ctxWindow": 200000,
                "tokensPerSec": 42.5,
                "inputTokens": 74000,
                "continuation": true
            }]
        });
        let (_, rows) = parse_transcript(&value).unwrap();
        let row = &rows[0];
        assert_eq!(row.thinking.as_deref(), Some("pensei nisto"));
        assert_eq!(row.tool_calls, Some(3));
        assert_eq!(row.duration_secs, Some(7));
        assert_eq!(row.ctx_window, Some(200_000));
        assert_eq!(row.tokens_per_sec, Some(42.5));
        assert_eq!(row.input_tokens, Some(74_000));
        assert!(row.continuation);
    }

    /// A host on an older build sends none of those keys. That is not an error:
    /// the row simply measured nothing this client can draw, and every field
    /// defaults to absent so the conversation still opens.
    #[test]
    fn a_host_without_those_columns_still_parses() {
        let value = serde_json::json!({
            "sessionId": "3f1a2b3c-0000-4000-8000-000000000000",
            "title": "old host",
            "messages": [{"id": "1", "role": "user", "content": "oi",
                          "sequence": 1, "createdAt": "2026-11-20T14:00:00+00:00"}]
        });
        let (_, rows) = parse_transcript(&value).unwrap();
        let row = &rows[0];
        assert_eq!(row.thinking, None);
        assert_eq!(row.tool_calls, None);
        assert_eq!(row.ctx_window, None);
        assert_eq!(row.tokens_per_sec, None);
        assert_eq!(row.input_tokens, None);
        assert!(!row.continuation);
    }

    #[test]
    fn tasks_parse_the_shape_the_host_writes() {
        // `handle_list_tasks` emits snake_case and a `timestamp` string. The
        // camelCase version this test used to assert is the A2A *task* schema
        // from `tasks/get`, which is a different method with a different shape.
        let value = serde_json::json!({
            "tasks": [{
                "id": "t1",
                "contextId": "c1",
                "state": "Working",
                "timestamp": "2026-11-20T14:00:00+00:00"
            }]
        });
        let tasks = parse_tasks(&value).unwrap();
        assert_eq!(tasks[0].context_id.as_deref(), Some("c1"));
        assert_eq!(tasks[0].state, "Working");
        assert_eq!(tasks[0].updated_at, 1_795_183_200);
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
