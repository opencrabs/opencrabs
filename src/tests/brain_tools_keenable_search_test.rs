//! Tests for the optional Keenable search engine (#2939).
//!
//! Off by default. When enabled, keyless mode hits `/v1/search/public` with
//! `X-Keenable-Title: opencrabs`; a key switches to `/v1/search` with
//! `X-API-Key`. Response parsing is pinned to `results[]` with `title`,
//! `url`, `snippet`/`description`, and `acquired_at` (ignored extra).

use crate::brain::tools::Tool;
use crate::brain::tools::ToolCapability;
use crate::brain::tools::ToolExecutionContext;
use crate::brain::tools::keenable_search::*;
use crate::brain::tools::web_search::WebSearchTool;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

fn ctx() -> ToolExecutionContext {
    ToolExecutionContext::new(Uuid::new_v4()).with_auto_approve(true)
}

fn make_tool() -> KeenableSearchTool {
    KeenableSearchTool::new(None)
}

fn recorded_results() -> serde_json::Value {
    json!({
        "results": [
            {
                "title": "Keenable bridge",
                "url": "https://keenable.example/bridge",
                "snippet": "It cost 40M according to Keenable.",
                "acquired_at": "2026-11-16T12:00:00Z"
            },
            {
                "title": "Description only",
                "url": "https://keenable.example/desc",
                "description": "Uses the description field."
            }
        ]
    })
}

fn ddg_html(title: &str, url: &str) -> String {
    format!(r#"<a class="result-link" href="{url}">{title}</a>"#)
}

#[test]
fn test_tool_name() {
    assert_eq!(make_tool().name(), "keenable_search");
}

#[test]
fn test_tool_capabilities() {
    let caps = make_tool().capabilities();
    assert_eq!(caps.len(), 1);
    assert!(matches!(caps[0], ToolCapability::Network));
}

#[test]
fn test_tool_no_approval_required() {
    assert!(!make_tool().requires_approval());
}

#[test]
fn test_validate_valid_and_invalid_input() {
    let tool = make_tool();
    assert!(tool.validate_input(&json!({ "query": "rust" })).is_ok());
    assert!(tool.validate_input(&json!({ "query": "" })).is_err());
    assert!(tool.validate_input(&json!({ "max_results": 5 })).is_err());
    assert!(
        tool.validate_input(&json!({ "query": "q", "max_results": 0 }))
            .is_err()
    );
    assert!(
        tool.validate_input(&json!({ "query": "q", "max_results": 11 }))
            .is_err()
    );
}

#[test]
fn test_default_deserialization() {
    let input: KeenableSearchInput = serde_json::from_value(json!({ "query": "hello" })).unwrap();
    assert_eq!(input.query, "hello");
    assert_eq!(input.max_results, 5);
}

#[test]
fn test_keyless_uses_public_endpoint_keyed_uses_search() {
    let keyless = KeenableSearchTool::new(None);
    assert!(!keyless.uses_key());
    assert_eq!(
        keyless.search_url(),
        "https://api.keenable.ai/v1/search/public"
    );

    let empty = KeenableSearchTool::new(Some(String::new()));
    assert!(!empty.uses_key());
    assert_eq!(
        empty.search_url(),
        "https://api.keenable.ai/v1/search/public"
    );

    let keyed = KeenableSearchTool::new(Some("test-key".to_string()));
    assert!(keyed.uses_key());
    assert_eq!(keyed.search_url(), "https://api.keenable.ai/v1/search");
}

/// Parser keeps title/url/snippet, falls back to description, and ignores
/// extras such as acquired_at.
#[test]
fn test_response_parsing() {
    let response: KeenableResponse = serde_json::from_value(recorded_results()).unwrap();
    assert_eq!(response.results.len(), 2);
    assert_eq!(
        response.results[0].title.as_deref(),
        Some("Keenable bridge")
    );
    assert_eq!(
        response.results[0].url.as_deref(),
        Some("https://keenable.example/bridge")
    );
    assert_eq!(
        response.results[0].snippet.as_deref(),
        Some("It cost 40M according to Keenable.")
    );
    assert_eq!(
        response.results[1].description.as_deref(),
        Some("Uses the description field.")
    );
}

#[test]
fn test_response_without_results_is_empty() {
    let response: KeenableResponse = serde_json::from_value(json!({})).unwrap();
    assert!(response.results.is_empty());
}

async fn mock_keenable(
    server: &mut mockito::Server,
    path: &str,
    status: usize,
    body: &str,
) -> mockito::Mock {
    server
        .mock("POST", path)
        .match_header("x-keenable-title", "opencrabs")
        .with_status(status)
        .with_header("content-type", "application/json")
        .with_body(body)
        .create_async()
        .await
}

#[tokio::test]
async fn keyless_request_sends_title_and_no_api_key() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/search/public")
        .match_header("x-keenable-title", "opencrabs")
        .match_header("content-type", "application/json")
        .match_body(mockito::Matcher::Json(json!({
            "query": "bridge",
            "max_results": 5
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(recorded_results().to_string())
        .create_async()
        .await;

    let tool = KeenableSearchTool::new(None).with_api_root(server.url());
    let result = tool
        .execute(json!({ "query": "bridge" }), &ctx())
        .await
        .expect("execute");
    assert!(result.success, "got: {}", result.output);
    assert!(result.output.contains("Keenable bridge"));
    assert!(
        result
            .output
            .contains("URL: https://keenable.example/bridge")
    );
    assert!(result.output.contains("Uses the description field."));
    mock.assert_async().await;
}

#[tokio::test]
async fn keyed_request_sends_api_key_and_title() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1/search")
        .match_header("x-keenable-title", "opencrabs")
        .match_header("x-api-key", "test-key")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(recorded_results().to_string())
        .create_async()
        .await;

    let tool = KeenableSearchTool::new(Some("test-key".to_string())).with_api_root(server.url());
    let result = tool
        .execute(json!({ "query": "bridge" }), &ctx())
        .await
        .expect("execute");
    assert!(result.success, "got: {}", result.output);
    mock.assert_async().await;
}

#[tokio::test]
async fn non_2xx_is_a_failed_result() {
    let mut server = mockito::Server::new_async().await;
    let _mock = mock_keenable(
        &mut server,
        "/v1/search/public",
        429,
        r#"{"error":"rate_limited"}"#,
    )
    .await;
    let tool = KeenableSearchTool::new(None).with_api_root(server.url());
    let result = tool
        .execute(json!({ "query": "bridge" }), &ctx())
        .await
        .expect("execute");
    assert!(!result.success);
    let err = result.error.as_deref().unwrap_or("");
    assert!(
        err.contains("429"),
        "got output={} error={err}",
        result.output
    );
}

#[tokio::test]
async fn missing_title_error_is_a_failed_result() {
    let mut server = mockito::Server::new_async().await;
    let _mock = mock_keenable(
        &mut server,
        "/v1/search/public",
        400,
        r#"{"error":"missing_app_title"}"#,
    )
    .await;
    let tool = KeenableSearchTool::new(None).with_api_root(server.url());
    let result = tool
        .execute(json!({ "query": "bridge" }), &ctx())
        .await
        .expect("execute");
    assert!(!result.success);
    let err = result.error.as_deref().unwrap_or("");
    assert!(
        err.contains("missing_app_title"),
        "got output={} error={err}",
        result.output
    );
}

#[tokio::test]
async fn malformed_json_is_an_execution_error() {
    let mut server = mockito::Server::new_async().await;
    let _mock = mock_keenable(&mut server, "/v1/search/public", 200, "{not-json").await;
    let tool = KeenableSearchTool::new(None).with_api_root(server.url());
    let err = tool
        .execute(json!({ "query": "bridge" }), &ctx())
        .await
        .expect_err("malformed JSON should fail parse");
    let msg = err.to_string();
    assert!(
        msg.contains("Failed to parse Keenable response"),
        "got: {msg}"
    );
}

#[tokio::test]
async fn timeout_is_an_execution_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    // Keep the listener (and its backlog) alive, but never accept, so the
    // HTTP handshake cannot complete before the short client timeout.
    let tool = KeenableSearchTool::new(None)
        .with_api_root(format!("http://{addr}"))
        .with_timeout(Duration::from_millis(80));
    let err = tool
        .execute(json!({ "query": "bridge" }), &ctx())
        .await
        .expect_err("timeout should fail the request");
    let msg = err.to_string();
    assert!(msg.contains("Keenable search request failed"), "got: {msg}");
    drop(listener);
}

fn fanout_tool(keenable: Option<KeenableSearchTool>, ddg_base: &str) -> WebSearchTool {
    WebSearchTool::new(None, None, None, keenable.map(Arc::new)).with_ddg_base_url(ddg_base)
}

#[tokio::test]
async fn fan_out_unconfigured_does_not_call_keenable() {
    let mut ddg = mockito::Server::new_async().await;
    let ddg_mock = ddg
        .mock("GET", mockito::Matcher::Regex(r"^/lite/\?q=".to_string()))
        .with_status(200)
        .with_body(ddg_html("DDG bridge", "https://ddg.example/bridge"))
        .create_async()
        .await;
    let mut keenable = mockito::Server::new_async().await;
    let keenable_mock = keenable
        .mock("POST", mockito::Matcher::Any)
        .expect(0)
        .create_async()
        .await;

    let tool = fanout_tool(None, &ddg.url());
    let result = tool
        .execute(json!({ "query": "bridge" }), &ctx())
        .await
        .expect("execute");
    assert!(result.success, "got: {}", result.output);
    assert!(result.output.contains("DDG bridge"));
    assert!(!result.output.contains("keenable.example"));
    ddg_mock.assert_async().await;
    keenable_mock.assert_async().await;
}

#[tokio::test]
async fn fan_out_configured_merges_recorded_keenable_results() {
    let mut ddg = mockito::Server::new_async().await;
    let _ddg_mock = ddg
        .mock("GET", mockito::Matcher::Regex(r"^/lite/\?q=".to_string()))
        .with_status(200)
        .with_body(ddg_html("DDG bridge", "https://ddg.example/bridge"))
        .create_async()
        .await;
    let mut keenable = mockito::Server::new_async().await;
    let keenable_mock = mock_keenable(
        &mut keenable,
        "/v1/search/public",
        200,
        &recorded_results().to_string(),
    )
    .await;

    let tool = fanout_tool(
        Some(KeenableSearchTool::new(None).with_api_root(keenable.url())),
        &ddg.url(),
    );
    let result = tool
        .execute(json!({ "query": "bridge" }), &ctx())
        .await
        .expect("execute");
    assert!(result.success, "got: {}", result.output);
    assert!(result.output.contains("DDG bridge"));
    assert!(result.output.contains("Keenable bridge"));
    assert!(result.output.contains("https://keenable.example/bridge"));
    keenable_mock.assert_async().await;
}

#[tokio::test]
async fn fan_out_still_answers_when_keenable_returns_429() {
    let mut ddg = mockito::Server::new_async().await;
    let _ddg_mock = ddg
        .mock("GET", mockito::Matcher::Regex(r"^/lite/\?q=".to_string()))
        .with_status(200)
        .with_body(ddg_html("DDG bridge", "https://ddg.example/bridge"))
        .create_async()
        .await;
    let mut keenable = mockito::Server::new_async().await;
    let _keenable_mock = mock_keenable(
        &mut keenable,
        "/v1/search/public",
        429,
        r#"{"error":"rate_limited"}"#,
    )
    .await;

    let tool = fanout_tool(
        Some(KeenableSearchTool::new(None).with_api_root(keenable.url())),
        &ddg.url(),
    );
    let result = tool
        .execute(json!({ "query": "bridge" }), &ctx())
        .await
        .expect("execute");
    assert!(result.success, "got: {}", result.output);
    assert!(result.output.contains("DDG bridge"));
    assert!(!result.output.contains("rate_limited"));
}

#[tokio::test]
async fn fan_out_still_answers_when_keenable_times_out() {
    let mut ddg = mockito::Server::new_async().await;
    let _ddg_mock = ddg
        .mock("GET", mockito::Matcher::Regex(r"^/lite/\?q=".to_string()))
        .with_status(200)
        .with_body(ddg_html("DDG bridge", "https://ddg.example/bridge"))
        .create_async()
        .await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let tool = fanout_tool(
        Some(
            KeenableSearchTool::new(None)
                .with_api_root(format!("http://{addr}"))
                .with_timeout(Duration::from_millis(80)),
        ),
        &ddg.url(),
    );
    let result = tool
        .execute(json!({ "query": "bridge" }), &ctx())
        .await
        .expect("execute");
    assert!(result.success, "got: {}", result.output);
    assert!(result.output.contains("DDG bridge"));
    drop(listener);
}

// ---- Registration gating: off unless enabled, key optional ----

use crate::brain::tools::registry::ToolRegistry;
use crate::cli::tool_setup::register_core_agent_tools;
use crate::config::{Config, ProviderConfig, WebSearchProviders};
use crate::db::Database;

fn keenable_config(section: Option<(bool, Option<&str>)>) -> Config {
    let mut config = Config::default();
    config.providers.web_search = Some(WebSearchProviders {
        keenable: section.map(|(enabled, api_key)| ProviderConfig {
            enabled,
            api_key: api_key.map(str::to_string),
            ..Default::default()
        }),
        ..Default::default()
    });
    config
}

async fn gated_registry(config: &Config) -> Arc<ToolRegistry> {
    let db = Database::connect_in_memory().await.expect("in-memory db");
    db.run_migrations().await.expect("migrations");
    let registry = Arc::new(ToolRegistry::new());
    let _subagent_manager = register_core_agent_tools(&registry, &db, config, false);
    registry
}

#[tokio::test]
async fn keenable_registers_only_when_enabled() {
    let registry = gated_registry(&keenable_config(Some((true, None)))).await;
    assert!(registry.has_tool("keenable_search"));
    assert!(registry.has_tool("web_search"));

    let registry = gated_registry(&keenable_config(Some((true, Some("k"))))).await;
    assert!(registry.has_tool("keenable_search"));

    let registry = gated_registry(&keenable_config(Some((true, Some(""))))).await;
    assert!(registry.has_tool("keenable_search"));

    let registry = gated_registry(&keenable_config(Some((false, Some("k"))))).await;
    assert!(!registry.has_tool("keenable_search"));

    let registry = gated_registry(&keenable_config(None)).await;
    assert!(!registry.has_tool("keenable_search"));
    assert!(registry.has_tool("web_search"));
}
