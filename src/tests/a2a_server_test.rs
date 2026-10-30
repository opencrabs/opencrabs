use crate::a2a::handler;
use crate::a2a::server::*;
use axum::body::Body;
use axum::http::Request;
use axum::http::StatusCode;
use tower::ServiceExt;

async fn test_state() -> A2aState {
    use crate::a2a::test_helpers::helpers;

    A2aState {
        task_store: handler::new_task_store(),
        cancel_store: handler::new_cancel_store(),
        host: "127.0.0.1".to_string(),
        port: 18790,
        agent_service: helpers::placeholder_agent_service().await,
        service_context: helpers::placeholder_service_context().await,
        api_key: None,
        pairing: crate::remote::pairing::PairingStore::new(
            std::env::temp_dir().join(format!("oc-a2a-test-pairing-{}", std::process::id())),
        ),
        pair_hits: crate::a2a::pairing_http::new_pair_hits(),
        remote_state_dir: std::env::temp_dir()
            .join(format!("oc-a2a-test-remote-{}", std::process::id())),
    }
}

#[tokio::test]
async fn test_health_endpoint() {
    let app = build_router(test_state().await, &[]);
    let req = Request::builder()
        .uri("/a2a/health")
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_agent_card_endpoint() {
    let app = build_router(test_state().await, &[]);
    let req = Request::builder()
        .uri("/.well-known/agent.json")
        .body(Body::empty())
        .expect("request");

    let resp = app.oneshot(req).await.expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn start_server_signals_readiness_once_bound() {
    use crate::a2a::test_helpers::helpers;
    use crate::config::A2aConfig;

    let config = A2aConfig {
        enabled: true,
        bind: "127.0.0.1".into(),
        port: 0,
        allowed_origins: vec![],
        advertise_url: None,
        api_key: None,
    };
    let agent_service = helpers::placeholder_agent_service().await;
    let service_context = helpers::placeholder_service_context().await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let handle = tokio::spawn(async move {
        let _ = start_server(&config, agent_service, service_context, Some(ready_tx)).await;
    });

    let res = tokio::time::timeout(std::time::Duration::from_secs(5), ready_rx).await;
    handle.abort();

    assert!(res.is_ok(), "Timed out waiting for readiness signal");
    assert!(
        res.unwrap().expect("oneshot channel open").is_ok(),
        "Expected Ok readiness"
    );
}

#[tokio::test]
async fn start_server_signals_refusal_on_gate_failure() {
    use crate::a2a::test_helpers::helpers;
    use crate::config::A2aConfig;

    let config = A2aConfig {
        enabled: true,
        bind: "0.0.0.0".into(),
        port: 0,
        allowed_origins: vec![],
        advertise_url: None,
        api_key: None,
    };
    let agent_service = helpers::placeholder_agent_service().await;
    let service_context = helpers::placeholder_service_context().await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let res = start_server(&config, agent_service, service_context, Some(ready_tx)).await;
    assert!(res.is_err(), "Expected gate failure to error");

    let ready_res = tokio::time::timeout(std::time::Duration::from_secs(5), ready_rx).await;
    assert!(ready_res.is_ok(), "Timed out waiting for readiness signal");
    assert!(
        ready_res.unwrap().expect("oneshot open").is_err(),
        "Expected Err readiness signal on gate failure"
    );
}

#[tokio::test]
async fn start_server_signals_ready_when_disabled() {
    use crate::a2a::test_helpers::helpers;
    use crate::config::A2aConfig;

    let config = A2aConfig {
        enabled: false,
        bind: "127.0.0.1".into(),
        port: 0,
        allowed_origins: vec![],
        advertise_url: None,
        api_key: None,
    };
    let agent_service = helpers::placeholder_agent_service().await;
    let service_context = helpers::placeholder_service_context().await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let res = start_server(&config, agent_service, service_context, Some(ready_tx)).await;
    assert!(res.is_ok(), "Expected disabled start_server to succeed");

    let ready_res = tokio::time::timeout(std::time::Duration::from_secs(5), ready_rx).await;
    assert!(ready_res.is_ok(), "Timed out waiting for readiness signal");
    assert!(
        ready_res.unwrap().expect("oneshot open").is_ok(),
        "Expected Ok readiness signal when disabled"
    );
}
