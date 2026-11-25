//! `gui/call`: one backend operation, by name, for a remote surface.
//!
//! Params are `{name, params}`. The operation's own scope is checked against
//! what the caller's credential carries before its params are read, then the
//! operation runs through `backend::dispatch_as`, the same executor the local
//! window uses. Every failure carries the typed [`BackendError`] in the
//! JSON-RPC error's `data`, so a client decodes the same value a local call
//! would have returned.
//!
//! A refused scope is the one answer that is not HTTP 200: it is 403, so a
//! client tells "not allowed" from "failed" without reading the body.

use axum::http::StatusCode;
use serde::Deserialize;

use crate::a2a::types::{JsonRpcError, JsonRpcResponse, error_codes};
use crate::backend::{self, BackendError, Ctx, Scope};

/// The params of a `gui/call` request.
#[derive(Debug, Deserialize)]
struct CallParams {
    /// The operation's wire name, `<domain>.<verb>`.
    name: String,
    /// The operation itself, as JSON. Absent reads as `null`.
    #[serde(default)]
    params: serde_json::Value,
}

/// Handle one `gui/call` request.
///
/// `ctx` is the crab the operation runs against: the process's primary, or
/// `None` while it is still booting. `granted` is the scope list the bearer
/// carried. The scope gate does not depend on boot: a caller without the
/// scope is refused with 403 whether or not the crab is ready.
pub(crate) async fn handle_call(
    ctx: Option<&Ctx>,
    granted: &[String],
    id: serde_json::Value,
    params: serde_json::Value,
) -> (StatusCode, JsonRpcResponse) {
    let call: CallParams = match serde_json::from_value(params) {
        Ok(call) => call,
        Err(e) => {
            return error_response(id, BackendError::BadParams(format!("gui/call: {e}")));
        }
    };
    let granted = granted_scopes(granted);
    let Some(ctx) = ctx else {
        let refused = match backend::scope_of(&call.name) {
            None => BackendError::UnknownOp(call.name),
            Some(scope) if !granted.contains(&scope) => BackendError::Forbidden { scope },
            Some(_) => BackendError::NotReady,
        };
        return error_response(id, refused);
    };
    // Marked as the remote door: a turn it starts asks this surface before
    // a gated tool runs, where the window's own turn never asks.
    let dispatched =
        backend::door::remote(backend::dispatch_as(ctx, &granted, &call.name, call.params)).await;
    match dispatched {
        Ok(reply) => (StatusCode::OK, JsonRpcResponse::success(id, reply)),
        Err(e) => error_response(id, e),
    }
}

/// The backend scopes among the credential's wire strings. A string that is
/// not a backend scope grants nothing here.
fn granted_scopes(granted: &[String]) -> Vec<Scope> {
    granted.iter().filter_map(|s| Scope::parse(s)).collect()
}

/// The HTTP status and JSON-RPC error for a backend error, with the typed
/// error in `data`.
fn error_response(id: serde_json::Value, err: BackendError) -> (StatusCode, JsonRpcResponse) {
    let (status, code) = match &err {
        BackendError::UnknownOp(_) => (StatusCode::OK, error_codes::METHOD_NOT_FOUND),
        BackendError::Forbidden { .. } => (StatusCode::FORBIDDEN, error_codes::INSUFFICIENT_SCOPE),
        BackendError::BadParams(_) => (StatusCode::OK, error_codes::INVALID_PARAMS),
        BackendError::NotReady | BackendError::Busy | BackendError::Failed(_) => {
            (StatusCode::OK, error_codes::INTERNAL_ERROR)
        }
    };
    let response = JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: None,
        error: Some(JsonRpcError {
            code,
            message: err.to_string(),
            data: serde_json::to_value(&err).ok(),
        }),
        id,
    };
    (status, response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::handler;
    use crate::a2a::server::{A2aState, build_router};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// Params no operation decodes from: every op is a braced struct, which
    /// serde never reads out of a JSON string. A call that gets past the
    /// scope gate therefore stops at `BadParams` and runs nothing.
    const PROBE: &str = "gui/call scope probe";

    fn ctx() -> Ctx {
        Ctx::new(std::path::PathBuf::from("unused.db"))
    }

    fn call(name: &str, params: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "name": name, "params": params })
    }

    /// The typed error a response carries in `data`.
    fn backend_error(response: &JsonRpcResponse) -> BackendError {
        let data = response
            .error
            .as_ref()
            .and_then(|e| e.data.clone())
            .expect("an error response carries the backend error");
        serde_json::from_value(data).expect("data decodes as BackendError")
    }

    #[test]
    fn the_registry_has_operations_to_gate() {
        assert!(
            !backend::registry().is_empty(),
            "the per-operation scope tests below would pass vacuously"
        );
    }

    #[tokio::test]
    async fn every_operation_passes_the_gate_with_its_scope() {
        let ctx = ctx();
        for (name, scope) in backend::registry() {
            let granted = vec![scope.as_str().to_string()];
            let (status, response) = handle_call(
                Some(&ctx),
                &granted,
                serde_json::json!(1),
                call(name, serde_json::json!(PROBE)),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{name} with {scope}");
            assert!(
                matches!(backend_error(&response), BackendError::BadParams(_)),
                "{name} with {scope} reached its params: {response:?}"
            );
        }
    }

    #[tokio::test]
    async fn every_operation_is_refused_without_its_scope() {
        let ctx = ctx();
        for (name, scope) in backend::registry() {
            let granted: Vec<String> = Scope::ALL
                .iter()
                .filter(|other| **other != scope)
                .map(|other| other.as_str().to_string())
                .collect();
            let (status, response) = handle_call(
                Some(&ctx),
                &granted,
                serde_json::json!(1),
                call(name, serde_json::json!(PROBE)),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{name} without {scope}");
            assert_eq!(backend_error(&response), BackendError::Forbidden { scope });
            let code = response.error.as_ref().map(|e| e.code);
            assert_eq!(code, Some(error_codes::INSUFFICIENT_SCOPE));
        }
    }

    #[tokio::test]
    async fn every_operation_is_refused_before_boot_without_its_scope() {
        for (name, scope) in backend::registry() {
            let (status, response) = handle_call(
                None,
                &[],
                serde_json::json!(1),
                call(name, serde_json::json!(PROBE)),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{name} before boot");
            assert_eq!(backend_error(&response), BackendError::Forbidden { scope });
        }
    }

    #[tokio::test]
    async fn an_operation_with_its_scope_before_boot_is_not_ready() {
        for (name, scope) in backend::registry() {
            let granted = vec![scope.as_str().to_string()];
            let (status, response) = handle_call(
                None,
                &granted,
                serde_json::json!(1),
                call(name, serde_json::json!(PROBE)),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{name} before boot");
            assert_eq!(backend_error(&response), BackendError::NotReady);
        }
    }

    #[tokio::test]
    async fn an_unknown_operation_is_refused_by_name() {
        let granted: Vec<String> = Scope::ALL.iter().map(|s| s.as_str().to_string()).collect();
        for ctx in [Some(ctx()), None] {
            let (status, response) = handle_call(
                ctx.as_ref(),
                &granted,
                serde_json::json!(1),
                call("no.such.op", serde_json::Value::Null),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                backend_error(&response),
                BackendError::UnknownOp("no.such.op".to_string())
            );
            let code = response.error.as_ref().map(|e| e.code);
            assert_eq!(code, Some(error_codes::METHOD_NOT_FOUND));
        }
    }

    #[tokio::test]
    async fn a_call_without_a_name_is_bad_params() {
        let (status, response) = handle_call(
            Some(&ctx()),
            &[],
            serde_json::json!(1),
            serde_json::json!({ "params": {} }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(matches!(
            backend_error(&response),
            BackendError::BadParams(_)
        ));
        let code = response.error.as_ref().map(|e| e.code);
        assert_eq!(code, Some(error_codes::INVALID_PARAMS));
    }

    #[test]
    fn a_credential_string_that_is_no_backend_scope_grants_nothing() {
        let granted = vec![
            "a2a:read".to_string(),
            "a2a:READ".to_string(),
            "admin".to_string(),
        ];
        assert_eq!(granted_scopes(&granted), vec![Scope::Read]);
    }

    async fn router_state(dir: &std::path::Path) -> A2aState {
        use crate::a2a::test_helpers::helpers;
        A2aState {
            task_store: handler::new_task_store(),
            cancel_store: handler::new_cancel_store(),
            host: "127.0.0.1".to_string(),
            port: 18790,
            agent_service: helpers::placeholder_agent_service().await,
            service_context: helpers::placeholder_service_context().await,
            api_key: None,
            pairing: crate::remote::pairing::PairingStore::new(dir.join("pairing")),
            pair_hits: crate::a2a::pairing_http::new_pair_hits(),
            remote_state_dir: dir.join("remote"),
        }
    }

    /// Through the real router: the open loopback posture carries
    /// `a2a:read` and `a2a:send` only, so an operation needing anything else
    /// comes back as an HTTP 403 and one needing those does not.
    #[tokio::test]
    async fn the_gateway_answers_403_for_a_scope_the_credential_lacks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = build_router(router_state(dir.path()).await, &[]);
        let carried = [Scope::Read, Scope::Send];
        for (name, scope) in backend::registry() {
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "gui/call",
                "params": call(name, serde_json::json!(PROBE)),
                "id": 1,
            });
            let req = Request::builder()
                .method("POST")
                .uri("/a2a/v1")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request");
            let resp = app.clone().oneshot(req).await.expect("response");
            let expected = if carried.contains(&scope) {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            };
            assert_eq!(resp.status(), expected, "{name} needs {scope}");
        }
    }
}
