//! Client half of the instance rail: the transport to another crab's A2A
//! gateway.
//!
//! Two doors:
//!
//! - is it alive? [`probe`], `GET /a2a/health` (public, so a keyless remote
//!   still answers)
//! - anything else: [`call`], one JSON-RPC POST to `/a2a/v1`, result unwrapped
//!
//! What a method's answer means is the caller's business; this module only
//! moves the envelope and types the failure.
//!
//! Failure is typed on purpose: a remote that does not know a method must be
//! distinguishable from one that is down, because the first falls back to an
//! older method and the second shows an error card. `anyhow` alone would
//! collapse both into a string nobody can branch on.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use anyhow::Result;

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

#[cfg(test)]
#[path = "instance_client_sharing_test.rs"]
mod sharing_test;

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
    fn targets_join_urls_without_double_slashes() {
        let t = Target::new("http://192.168.1.7:8080/", None);
        assert_eq!(t.url("/a2a/health"), "http://192.168.1.7:8080/a2a/health");
        assert_eq!(t.url("a2a/v1"), "http://192.168.1.7:8080/a2a/v1");
    }
}
