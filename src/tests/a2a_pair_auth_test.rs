//! Integration tests for the A2A pairing endpoint and scoped bearer auth:
//! code → token → authorized JSON-RPC, scope enforcement, revocation,
//! rate limiting, and lockout surfacing. Router-level (tower oneshot),
//! hermetic: temp-dir pairing stores, per-test rate-limit buckets, no
//! network, no agent runs (tasks/get + tasks/cancel probes hit the
//! dispatcher's not-found path without touching the placeholder agent).

use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::a2a::handler;
use crate::a2a::pairing_http::new_pair_hits;
use crate::a2a::server::{A2aState, build_router};
use crate::remote::pairing::PairingStore;
use crate::remote::scopes::{SCOPE_READ, SCOPE_SEND};

async fn state_with(api_key: Option<&str>, dir: &Path) -> A2aState {
    use crate::a2a::test_helpers::helpers;
    A2aState {
        task_store: handler::new_task_store(),
        cancel_store: handler::new_cancel_store(),
        host: "127.0.0.1".to_string(),
        port: 18790,
        agent_service: helpers::placeholder_agent_service().await,
        service_context: helpers::placeholder_service_context().await,
        api_key: api_key.map(str::to_string),
        pairing: PairingStore::new(dir.join("pairing.json")),
        pair_hits: new_pair_hits(),
    }
}

async fn call(
    app: Router,
    uri: &str,
    body: serde_json::Value,
    bearer: Option<&str>,
    client_ip: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = bearer {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    if let Some(ip) = client_ip {
        builder = builder.header("cf-connecting-ip", ip);
    }
    let req = builder
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, json)
}

fn rpc(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
}

async fn paired_token(app: &Router, store: &PairingStore, scopes: &[String], ip: &str) -> String {
    let code = store.mint_code(scopes).expect("mint");
    let (status, body) = call(
        app.clone(),
        "/a2a/pair",
        serde_json::json!({"code": code, "label": "test-client"}),
        None,
        Some(ip),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "pair failed: {body}");
    body["token"].as_str().expect("token").to_string()
}

#[tokio::test]
async fn pair_wrong_code_is_401() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let app = build_router(state, &[]);
    let (status, body) = call(
        app,
        "/a2a/pair",
        serde_json::json!({"code": "WRONG123", "label": "x"}),
        None,
        Some("1.1.1.1"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body["error"].as_str().unwrap().contains("invalid"));
}

#[tokio::test]
async fn pair_bad_body_is_400() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let app = build_router(state, &[]);
    let req = Request::builder()
        .method("POST")
        .uri("/a2a/pair")
        .header("content-type", "application/json")
        .body(Body::from("{\"label\": \"no code field\"}"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    // axum: well-formed JSON of the wrong shape → 422 (400 is syntax errors)
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn pair_success_returns_scoped_token() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let store = PairingStore::new(dir.path().join("pairing.json"));
    let app = build_router(state, &[]);
    let code = store
        .mint_code(&[SCOPE_READ.to_string(), SCOPE_SEND.to_string()])
        .unwrap();
    let (status, body) = call(
        app,
        "/a2a/pair",
        serde_json::json!({"code": code, "label": "phone"}),
        None,
        Some("2.2.2.2"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["token"].as_str().unwrap().starts_with("ocpair_"));
    let scopes = body["scopes"].as_array().unwrap();
    assert_eq!(scopes.len(), 2);
    assert!(body["expires_at"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn paired_token_passes_auth_and_read_scope() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let store = PairingStore::new(dir.path().join("pairing.json"));
    let app = build_router(state, &[]);
    let token = paired_token(&app, &store, &[SCOPE_READ.to_string()], "3.3.3.3").await;

    // tasks/get for a missing task: HTTP 200 + TASK_NOT_FOUND (-32001,
    // "not found" message) proves we cleared auth AND the read scope gate.
    let (status, body) = call(
        app,
        "/a2a/v1",
        rpc("tasks/get", serde_json::json!({"id": "nope"})),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"].as_i64(), Some(-32001));
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not found")
    );
}

#[tokio::test]
async fn read_scope_cannot_use_send_methods() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let store = PairingStore::new(dir.path().join("pairing.json"));
    let app = build_router(state, &[]);
    let token = paired_token(&app, &store, &[SCOPE_READ.to_string()], "4.4.4.4").await;

    let (status, body) = call(
        app.clone(),
        "/a2a/v1",
        rpc("tasks/cancel", serde_json::json!({"id": "nope"})),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["error"]["code"].as_i64(),
        Some(-32004),
        "expected INSUFFICIENT_SCOPE: {body}"
    );

    // message/stream is gated the same way (before the SSE branch)
    let (_status, body) = call(
        app,
        "/a2a/v1",
        rpc("message/stream", serde_json::json!({})),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(body["error"]["code"].as_i64(), Some(-32004));
}

#[tokio::test]
async fn send_scope_passes_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let store = PairingStore::new(dir.path().join("pairing.json"));
    let app = build_router(state, &[]);
    let token = paired_token(&app, &store, &[SCOPE_SEND.to_string()], "5.5.5.5").await;

    // tasks/cancel with a send-scoped token clears the scope gate and
    // reaches the dispatcher (task missing → -32001, NOT -32004).
    let (status, body) = call(
        app,
        "/a2a/v1",
        rpc("tasks/cancel", serde_json::json!({"id": "nope"})),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(body["error"]["code"].as_i64(), Some(-32004));
}

#[tokio::test]
async fn api_key_bearer_keeps_full_access() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let app = build_router(state, &[]);
    let (status, body) = call(
        app,
        "/a2a/v1",
        rpc("tasks/cancel", serde_json::json!({"id": "nope"})),
        Some("cfg-key"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(
        body["error"]["code"].as_i64(),
        Some(-32004),
        "api_key must never hit the scope gate"
    );
    // Full access = the request reached the dispatcher: TASK_NOT_FOUND
    // (-32001 + "not found") on HTTP 200. The unauthorized body reuses
    // -32001, but it rides HTTP 401 — the status assert above separates them.
    assert_eq!(body["error"]["code"].as_i64(), Some(-32001));
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not found")
    );
}

#[tokio::test]
async fn missing_or_bad_bearer_is_401_when_key_set() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let app = build_router(state, &[]);
    let (status, _) = call(
        app.clone(),
        "/a2a/v1",
        rpc("tasks/get", serde_json::json!({"id": "x"})),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        app,
        "/a2a/v1",
        rpc("tasks/get", serde_json::json!({"id": "x"})),
        Some("ocpair_totally-made-up"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn legacy_keyless_loopback_still_open() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(None, dir.path()).await;
    let app = build_router(state, &[]);
    let (status, body) = call(
        app,
        "/a2a/v1",
        rpc("tasks/get", serde_json::json!({"id": "nope"})),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"].as_i64(), Some(-32001)); // reached dispatch
}

#[tokio::test]
async fn revoked_token_is_401() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let store = PairingStore::new(dir.path().join("pairing.json"));
    let app = build_router(state, &[]);
    let token = paired_token(&app, &store, &[SCOPE_READ.to_string()], "6.6.6.6").await;
    assert_eq!(store.revoke("test-client").unwrap(), 1);
    let (status, _) = call(
        app,
        "/a2a/v1",
        rpc("tasks/get", serde_json::json!({"id": "x"})),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pair_rate_limit_returns_429() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let app = build_router(state, &[]);
    let mut saw_429 = false;
    for i in 0..12 {
        let (status, _body) = call(
            app.clone(),
            "/a2a/pair",
            serde_json::json!({"code": format!("BADCODE{i:02}"), "label": "x"}),
            None,
            Some("9.9.9.9"),
        )
        .await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "12 attempts from one IP must trip 429");
}

#[tokio::test]
async fn store_lockout_surfaces_as_429_across_ips() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("cfg-key"), dir.path()).await;
    let app = build_router(state, &[]);
    // Fresh client IP per attempt keeps the rate limiter out of the way,
    // so the 429 below can only come from the store's global lockout.
    for i in 0..11 {
        let (status, body) = call(
            app.clone(),
            "/a2a/pair",
            serde_json::json!({"code": format!("BADCODE{i:02}"), "label": "x"}),
            None,
            Some(&format!("10.0.0.{i}")),
        )
        .await;
        if i == 10 {
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
            assert!(body["error"].as_str().unwrap().contains("locked"));
        }
    }
}
