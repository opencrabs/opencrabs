//! A2A Gateway HTTP server powered by axum.
//!
//! Serves:
//! - `GET  /.well-known/agent.json` — Agent Card discovery
//! - `POST /a2a/v1`                 — JSON-RPC 2.0 endpoint (scoped)
//! - `POST /a2a/pair`               — One-time code → scoped session token
//! - `GET  /a2a/health`             — Health check

use crate::a2a::{agent_card, handler, pairing_http, types::*};
use crate::brain::agent::service::AgentService;
use crate::config::A2aConfig;
use crate::remote::pairing::PairingStore;
use crate::remote::scopes::{ALL_SCOPES, allows, scope_for_method};
use crate::services::ServiceContext;
use axum::{
    Extension, Router,
    extract::State,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Json, Sse, sse},
    routing::{get, post},
};
use futures::stream;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};

/// Shared state for the A2A gateway.
#[derive(Clone)]
pub struct A2aState {
    pub task_store: handler::TaskStore,
    pub cancel_store: handler::CancelStore,
    pub host: String,
    pub port: u16,
    pub agent_service: Arc<AgentService>,
    pub service_context: ServiceContext,
    pub api_key: Option<String>,
    /// Paired-session store (codes + tokens). Path-injected for tests.
    pub pairing: PairingStore,
    /// Per-client rate-limit buckets for POST /a2a/pair.
    pub pair_hits: pairing_http::PairHits,
}

/// The scopes a request's credential carries, attached by `require_bearer`
/// and enforced per method in `handle_jsonrpc`.
#[derive(Clone, Debug)]
pub struct GrantedScopes(pub Vec<String>);

fn all_scopes() -> Vec<String> {
    ALL_SCOPES.iter().map(|s| s.to_string()).collect()
}

/// Compare a presented bearer token against the configured one without a
/// timing side-channel on the secret.
///
/// A plain `token == expected` short-circuits at the first differing byte, so
/// the response time leaks how many leading bytes a guess got right and the
/// token can be recovered one byte at a time over the network. Hashing both
/// sides to a fixed-length SHA-256 digest first removes that: the digests are
/// always 32 bytes (no length leak), and an attacker cannot steer the digest
/// of a guess to walk the comparison, because that would require a SHA-256
/// preimage. This is the standard hash-then-compare mitigation and needs no
/// new dependency.
pub(crate) fn tokens_match(presented: &str, expected: &str) -> bool {
    use sha2::{Digest, Sha256};
    let a = Sha256::digest(presented.as_bytes());
    let b = Sha256::digest(expected.as_bytes());
    a == b
}

/// Bearer auth middleware. Two credential kinds:
///
/// - The config `[a2a].api_key` → all scopes (backward compatible).
/// - A paired `ocpair_` session token → the scopes its code carried.
///
/// When no api_key is configured the legacy loopback-open posture stands:
/// everything through (`check_gate_authenticated` guarantees that posture
/// can only bind loopback). Successful auth attaches [`GrantedScopes`] for
/// the per-method check in `handle_jsonrpc`.
async fn require_bearer(
    State(state): State<A2aState>,
    mut req: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> axum::response::Response {
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    let scopes = match (&state.api_key, presented) {
        // Legacy open mode: only reachable on a loopback bind (#1473 gate).
        (None, _) => all_scopes(),
        (Some(expected), Some(token)) if tokens_match(token, expected) => all_scopes(),
        (Some(_), Some(token)) if token.starts_with(crate::remote::pairing::TOKEN_PREFIX) => {
            // Paired session. Small blocking JSON read per request: fine at
            // A2A rates, and the writer's atomic rename means never partial.
            match state.pairing.verify_token(token) {
                Some(session) => session.scopes,
                None => return unauthorized_response(),
            }
        }
        _ => return unauthorized_response(),
    };

    req.extensions_mut().insert(GrantedScopes(scopes));
    next.run(req).await
}

fn unauthorized_response() -> axum::response::Response {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": -32001, "message": "Unauthorized: invalid or missing Bearer token" },
        "id": null
    });
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

/// Build the axum router for the A2A gateway.
pub fn build_router(state: A2aState, allowed_origins: &[String]) -> Router {
    let cors = if allowed_origins.is_empty() {
        CorsLayer::new()
    } else {
        let origins: Vec<_> = allowed_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        CorsLayer::new().allow_origin(AllowOrigin::list(origins))
    };

    // Auth-protected JSON-RPC endpoint
    let protected = Router::new()
        .route("/a2a/v1", post(handle_jsonrpc))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ));

    // Public endpoints (discovery + health + pairing). Pairing is
    // unauthenticated by design — the one-time code IS the credential —
    // and rate-limited in the handler.
    Router::new()
        .route("/.well-known/agent.json", get(get_agent_card))
        .route("/a2a/health", get(health_check))
        .route("/a2a/pair", post(pairing_http::handle_pair))
        .merge(protected)
        .layer(cors)
        .with_state(state)
}

/// Refuse an a2a bind that would be an open, unauthenticated gate.
///
/// The bearer token is the ONLY authorization boundary in front of the agent's
/// full tool surface: `send`/`stream` run the agent with tools for any caller
/// that clears `require_bearer`, and that middleware lets everyone through when
/// no key is set. Loopback is safe (only same-box callers reach the port); any
/// other bind with no key is an open tool-capable gate (#1473).
///
/// `Ok(())` means safe to start. A bind string that is not a bare IP never
/// parses into the `SocketAddr` the server needs either, so treating a parse
/// miss as "not loopback" only ever fails safe.
pub(crate) fn check_gate_authenticated(bind: &str, api_key: Option<&str>) -> Result<(), String> {
    if api_key.is_some() {
        return Ok(());
    }
    let loopback = bind
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false);
    if loopback {
        return Ok(());
    }
    Err(format!(
        "bind is '{bind}' (not loopback) with no api_key set, which would expose the agent's \
         full tool surface unauthenticated. Set [a2a].api_key, or bind to 127.0.0.1 (#1473)."
    ))
}

/// Start the A2A gateway server.
///
/// Runs as a background task — call from `tokio::spawn`.
///
/// If `ready` is provided, signals `Ok(())` once the socket is bound and
/// listening (or immediately if A2A is disabled), or signals `Err` if gate
/// validation, address parsing, or socket binding fails.
/// The socket is bound and listening when `ready` resolves `Ok(())`, so any
/// incoming connection is queued by the kernel accept queue and reaches the gateway.
pub async fn start_server(
    config: &A2aConfig,
    agent_service: Arc<AgentService>,
    service_context: ServiceContext,
    ready: Option<tokio::sync::oneshot::Sender<anyhow::Result<()>>>,
) -> anyhow::Result<()> {
    if !config.enabled {
        tracing::info!("A2A gateway disabled in config");
        if let Some(tx) = ready {
            let _ = tx.send(Ok(()));
        }
        return Ok(());
    }

    // The bearer token is the ONLY authorization boundary in front of the
    // agent's full tool surface here: `send`/`stream` run the agent with tools
    // for any caller that gets past this middleware, and `require_bearer` lets
    // everyone through when no key is set. That is fine on loopback, where only
    // same-box callers can reach the port. It is an open, unauthenticated,
    // tool-capable gate the moment the bind leaves loopback. Refuse rather than
    // start silently open (#1473). A hostname bind that is not a bare IP never
    // parsed into the SocketAddr below either, so treating a parse miss as
    // "not loopback" only ever fails safe.
    if let Err(reason) = check_gate_authenticated(&config.bind, config.api_key.as_deref()) {
        let msg = anyhow::anyhow!("A2A gateway refuses to start: {reason}");
        if let Some(tx) = ready {
            let _ = tx.send(Err(anyhow::anyhow!("{msg}")));
        }
        anyhow::bail!(msg);
    }

    // Restore any in-flight tasks from the database
    let task_store = handler::new_task_store();
    let persisted = super::persistence::load_active_tasks(&service_context.pool()).await;
    if !persisted.is_empty() {
        let mut store = task_store.write().await;
        for task in persisted {
            tracing::info!(
                "A2A: Restored task {} (state: {:?})",
                task.id,
                task.status.state
            );
            store.insert(task.id.clone(), task);
        }
    }

    let state = A2aState {
        task_store,
        cancel_store: handler::new_cancel_store(),
        host: config.bind.clone(),
        port: config.port,
        agent_service,
        service_context,
        api_key: config.api_key.clone(),
        pairing: PairingStore::new(PairingStore::default_path()),
        pair_hits: pairing_http::new_pair_hits(),
    };

    let app = build_router(state, &config.allowed_origins);
    let addr: SocketAddr = match format!("{}:{}", config.bind, config.port).parse() {
        Ok(addr) => addr,
        Err(e) => {
            let err = anyhow::anyhow!("Invalid A2A gateway address: {}", e);
            if let Some(tx) = ready {
                let _ = tx.send(Err(anyhow::anyhow!("{err}")));
            }
            return Err(err);
        }
    };

    tracing::info!("A2A Gateway starting on http://{}", addr);
    tracing::info!("   Agent Card: http://{}/.well-known/agent.json", addr);
    tracing::info!("   JSON-RPC:   http://{}/a2a/v1", addr);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            let err = anyhow::anyhow!("A2A bind failed: {e}");
            if let Some(tx) = ready {
                let _ = tx.send(Err(anyhow::anyhow!("{err}")));
            }
            return Err(err);
        }
    };

    if let Some(tx) = ready {
        let _ = tx.send(Ok(()));
    }

    axum::serve(listener, app).await?;

    Ok(())
}

/// GET /.well-known/agent.json — Agent Card discovery.
async fn get_agent_card(State(state): State<A2aState>) -> Json<AgentCard> {
    let registry = state.agent_service.tool_registry();
    let card = agent_card::build_agent_card(&state.host, state.port, Some(registry));
    Json(card)
}

/// POST /a2a/v1 -- JSON-RPC 2.0 endpoint.
/// Returns JSON for most methods, SSE stream for `message/stream`.
async fn handle_jsonrpc(
    State(state): State<A2aState>,
    Extension(granted): Extension<GrantedScopes>,
    Json(req): Json<JsonRpcRequest>,
) -> axum::response::Response {
    if req.jsonrpc != "2.0" {
        return (
            StatusCode::OK,
            Json(JsonRpcResponse::error(
                req.id,
                error_codes::INVALID_REQUEST,
                "Invalid JSON-RPC version, expected 2.0",
            )),
        )
            .into_response();
    }

    // Scope gate BEFORE any dispatch (and before the stream branch, so
    // message/stream is covered). Unknown methods fall through to the
    // dispatcher's MethodNotFound — credentials don't change that.
    if let Some(required) = scope_for_method(&req.method)
        && !allows(&granted.0, required)
    {
        return (
            StatusCode::OK,
            Json(JsonRpcResponse::error(
                req.id,
                error_codes::INSUFFICIENT_SCOPE,
                format!("insufficient scope: {required} required"),
            )),
        )
            .into_response();
    }

    // message/stream returns SSE instead of JSON
    if req.method == "message/stream" {
        return handle_stream(state, req).await;
    }

    let response = handler::dispatch(
        req,
        state.task_store,
        state.cancel_store,
        state.agent_service,
        state.service_context.clone(),
    )
    .await;
    (StatusCode::OK, Json(response)).into_response()
}

/// Handle `message/stream` -- returns an SSE stream of task updates.
async fn handle_stream(state: A2aState, req: JsonRpcRequest) -> axum::response::Response {
    match handler::stream::handle_stream_message(
        req.id,
        req.params,
        state.task_store,
        state.cancel_store,
        state.agent_service,
        state.service_context,
    )
    .await
    {
        Ok((id, rx)) => {
            let stream = stream::unfold((id, rx), |(id, mut rx)| async move {
                let event = rx.recv().await?;
                let result = serde_json::to_value(&event).unwrap_or_default();
                let rpc_response = JsonRpcResponse::success(id.clone(), result);
                let data = serde_json::to_string(&rpc_response).unwrap_or_default();
                let sse_event = Ok::<_, std::convert::Infallible>(sse::Event::default().data(data));
                Some((sse_event, (id, rx)))
            });
            Sse::new(stream).into_response()
        }
        Err(error_response) => (StatusCode::OK, Json(error_response)).into_response(),
    }
}

/// GET /a2a/health — Health check.
async fn health_check() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "version": crate::VERSION,
        "protocol": "A2A",
        "protocol_version": "1.0",
        "provider": "OpenCrabs Community"
    }))
}
