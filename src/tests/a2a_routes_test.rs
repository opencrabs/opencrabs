//! Integration tests for route announcement: the auth-gated `routes` block
//! on health and the canonical `GET /a2a/routes` endpoint. The privacy
//! invariant under test: LAN topology is never handed to anonymous callers,
//! and a keyless gateway suppresses announcements entirely.

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
        remote_state_dir: dir.to_path_buf(),
    }
}

async fn get(app: Router, uri: &str, bearer: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut b = Request::builder().method("GET").uri(uri);
    if let Some(t) = bearer {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let resp = app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

async fn pair_for(app: &Router, store: &PairingStore, scopes: &[String]) -> String {
    let code = store.mint_code(scopes).unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/a2a/pair")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({"code": code, "label": "routes-test"}))
                .unwrap(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    body["token"].as_str().unwrap().to_string()
}

// ── health: the auth-gated routes block ────────────────────────────────────

#[tokio::test]
async fn health_anonymous_gets_no_routes() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state_with(Some("k"), dir.path()).await, &[]);
    let (status, body) = get(app, "/a2a/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok", "health itself stays public");
    assert!(body.get("routes").is_none(), "no routes for anonymous");
    assert!(body.get("host_id").is_none());
}

#[tokio::test]
async fn health_bad_bearer_gets_no_routes() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state_with(Some("k"), dir.path()).await, &[]);
    let (status, body) = get(app, "/a2a/health", Some("wrong-token")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("routes").is_none());
}

#[tokio::test]
async fn health_api_key_bearer_includes_routes() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state_with(Some("k"), dir.path()).await, &[]);
    let (status, body) = get(app, "/a2a/health", Some("k")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["host_id"].as_str().unwrap().starts_with("h-"));
    assert!(body["routes"].is_array(), "routes array present for authed");
    // host_id must be persisted and stable across calls
    let id1 = body["host_id"].as_str().unwrap().to_string();
    let dir_check = dir.path().join("host_id");
    assert!(
        dir_check.exists(),
        "host_id persisted under remote_state_dir"
    );
    assert!(std::fs::read_to_string(&dir_check).unwrap().contains(&id1));
}

#[tokio::test]
async fn health_read_scoped_session_token_includes_routes() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("k"), dir.path()).await;
    let store = PairingStore::new(dir.path().join("pairing.json"));
    let app = build_router(state, &[]);
    let token = pair_for(&app, &store, &[SCOPE_READ.to_string()]).await;
    let (status, body) = get(app, "/a2a/health", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["routes"].is_array());
}

#[tokio::test]
async fn health_keyless_never_includes_routes() {
    // Keyless = anyone reaching the port counts as authed. With a tunnel up
    // that "anyone" is the internet, so announcement stays off entirely.
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state_with(None, dir.path()).await, &[]);
    let (status, body) = get(app, "/a2a/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("routes").is_none());
    assert!(body.get("host_id").is_none());
}

// ── GET /a2a/routes: the canonical endpoint ────────────────────────────────

#[tokio::test]
async fn routes_endpoint_requires_bearer_when_key_set() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state_with(Some("k"), dir.path()).await, &[]);
    let (status, _) = get(app, "/a2a/routes", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn routes_endpoint_shape_for_api_key() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state_with(Some("k"), dir.path()).await, &[]);
    let (status, body) = get(app, "/a2a/routes", Some("k")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["host_id"].as_str().unwrap().starts_with("h-"));
    assert!(body["routes"].is_array());
    assert!(body["announced_at"].as_u64().unwrap() > 0);
    // every announced route is well-formed
    for r in body["routes"].as_array().unwrap() {
        let url = r["url"].as_str().unwrap();
        assert!(
            url.starts_with("http://") || url.starts_with("https://"),
            "{url}"
        );
        assert!(["lan", "tailnet", "tunnel"].contains(&r["kind"].as_str().unwrap()));
    }
}

#[tokio::test]
async fn routes_endpoint_keyless_is_suppressed_with_note() {
    let dir = tempfile::tempdir().unwrap();
    let app = build_router(state_with(None, dir.path()).await, &[]);
    let (status, body) = get(app, "/a2a/routes", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "suppression is policy, not an error"
    );
    assert_eq!(body["routes"].as_array().unwrap().len(), 0);
    assert!(body["note"].as_str().unwrap().contains("api_key"));
}

#[tokio::test]
async fn routes_endpoint_needs_read_scope() {
    let dir = tempfile::tempdir().unwrap();
    let state = state_with(Some("k"), dir.path()).await;
    let store = PairingStore::new(dir.path().join("pairing.json"));
    let app = build_router(state, &[]);

    let send_only = pair_for(&app, &store, &[SCOPE_SEND.to_string()]).await;
    let (status, body) = get(app.clone(), "/a2a/routes", Some(&send_only)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"].as_str().unwrap().contains(SCOPE_READ));

    let with_read = pair_for(
        &app,
        &store,
        &[SCOPE_READ.to_string(), SCOPE_SEND.to_string()],
    )
    .await;
    let (status, _) = get(app, "/a2a/routes", Some(&with_read)).await;
    assert_eq!(status, StatusCode::OK);
}
