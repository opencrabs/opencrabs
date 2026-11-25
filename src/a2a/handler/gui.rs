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
//!
//! The caller is the credential the gate resolved ([`CredentialId`]): two
//! pairings are two principals, so a turn one of them opens answers to it
//! alone. A call that reaches here with no credential named is refused with
//! 401, never run under a shared principal.

use axum::http::StatusCode;
use serde::Deserialize;

use crate::a2a::server::CredentialId;
use crate::a2a::types::{JsonRpcError, JsonRpcResponse, error_codes};
use crate::backend::door::Caller;
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

/// Handle one `gui/call` request for the credential `credential` names.
///
/// `ctx` is the crab the operation runs against: the process's primary, or
/// `None` while it is still booting. `credential` is the one the gate
/// resolved; `None` (no gate attached it) is refused with 401 before
/// anything else. `granted` is the scope list the bearer carried. The scope
/// gate does not depend on boot: a caller without the scope is refused with
/// 403 whether or not the crab is ready.
pub(crate) async fn handle_call_for(
    ctx: Option<&Ctx>,
    credential: Option<&CredentialId>,
    granted: &[String],
    id: serde_json::Value,
    params: serde_json::Value,
) -> (StatusCode, JsonRpcResponse) {
    let Some(CredentialId(principal)) = credential else {
        return unattributed_response(id);
    };
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
    // Marked as the remote door, with the caller's principal and every
    // scope it carries: a turn it starts asks this surface before a gated
    // tool runs only when the caller may approve, and answers to it alone.
    let caller = Caller {
        principal: principal.clone(),
        scopes: granted.clone(),
    };
    let dispatched = backend::door::remote_as(
        caller,
        backend::dispatch_as(ctx, &granted, &call.name, call.params),
    )
    .await;
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

/// The 401 for a call no credential names: the gate attaches one to every
/// request it lets through, so this is a wiring fault, refused rather than
/// run as anybody.
fn unattributed_response(id: serde_json::Value) -> (StatusCode, JsonRpcResponse) {
    (
        StatusCode::UNAUTHORIZED,
        JsonRpcResponse::error(
            id,
            UNAUTHORIZED,
            "Unauthorized: gui/call names no credential",
        ),
    )
}

/// The JSON-RPC code the gate answers an unauthenticated request with.
const UNAUTHORIZED: i64 = -32001;

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

    /// The calls below that predate [`CredentialId`] run as one fixed
    /// principal, the way `door::remote` names the tests' callers.
    async fn handle_call(
        ctx: Option<&Ctx>,
        granted: &[String],
        id: serde_json::Value,
        params: serde_json::Value,
    ) -> (StatusCode, JsonRpcResponse) {
        let test = CredentialId("test".to_string());
        handle_call_for(ctx, Some(&test), granted, id, params).await
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

    /// The dispatch opens the remote door: `turn.resume`, which only the
    /// local door may run, is refused with a 403 to the owner's credential
    /// (every wire scope), before the crab is even asked for its service.
    #[tokio::test]
    async fn a_gui_call_runs_by_the_remote_door() {
        let granted: Vec<String> = crate::remote::scopes::ALL_SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect();
        let params = serde_json::json!({
            "session_id": "5b3a0f0e-8a51-4c49-9d39-3f1f4a1c2b77",
            "content": "continue",
        });
        let (status, response) = handle_call(
            Some(&ctx()),
            &granted,
            serde_json::json!(1),
            call("turn.resume", params),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(
            backend_error(&response),
            BackendError::Forbidden {
                scope: Scope::Admin
            }
        );
    }

    /// A call the gate attached no credential to is refused with 401 before
    /// its name is even read, never run as a shared principal.
    #[tokio::test]
    async fn a_call_no_credential_names_is_refused() {
        let granted: Vec<String> = Scope::ALL.iter().map(|s| s.as_str().to_string()).collect();
        for ctx in [Some(ctx()), None] {
            let (status, response) = handle_call_for(
                ctx.as_ref(),
                None,
                &granted,
                serde_json::json!(1),
                call("no.such.op", serde_json::Value::Null),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            let code = response.error.as_ref().map(|e| e.code);
            assert_eq!(code, Some(UNAUTHORIZED));
        }
    }

    /// The owner's key and the open posture name fixed principals, and the
    /// key itself is never one.
    #[tokio::test]
    async fn the_api_key_and_the_open_posture_name_their_own_principals() {
        let dir = tempfile::tempdir().expect("tempdir");
        let open = router_state(dir.path()).await;
        let (CredentialId(principal), _) = credential(&open, None).expect("open posture admits");
        assert_eq!(principal, LOOPBACK_OPEN_CREDENTIAL);

        let owned = paired_state(dir.path()).await;
        let (CredentialId(principal), _) =
            credential(&owned, Some(OWNER_KEY)).expect("api_key admits");
        assert_eq!(principal, OWNER_CREDENTIAL);
        assert!(!principal.contains(OWNER_KEY));
    }

    /// Two pairings are two principals: each is its pairing's listed id, and
    /// the token that presented it appears nowhere in the caller.
    #[tokio::test]
    async fn two_pairings_are_two_principals_and_neither_holds_its_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = paired_state(dir.path()).await;
        let (alice_token, alice) = pair(&state, "alice");
        let (bob_token, bob) = pair(&state, "bob");
        assert_ne!(alice, bob);
        let listed: Vec<String> = state
            .pairing
            .list_sessions()
            .expect("list sessions")
            .into_iter()
            .map(|s| s.id)
            .collect();
        for (token, CredentialId(principal)) in [(&alice_token, &alice), (&bob_token, &bob)] {
            assert!(listed.contains(principal), "{principal} is no pairing's id");
            let caller = Caller {
                principal: principal.clone(),
                scopes: Scope::ALL.to_vec(),
            };
            let shown = format!("{caller:?}");
            assert!(
                !shown.contains(token.as_str()),
                "the caller holds its token"
            );
            assert!(!token.contains(principal.as_str()));
        }
    }

    /// B's answer to the approval A's turn asked is refused, and so is B's
    /// stop; A's own answer is the one the turn takes.
    #[tokio::test]
    async fn another_pairing_cannot_answer_a_turns_approval_its_own_does() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = paired_state(dir.path()).await;
        let (_, alice) = pair(&state, "alice");
        let (_, bob) = pair(&state, "bob");
        let crab = Crab::gated().await;
        let id = crab.session().await;
        let mut sub = gui_stream::hub().subscribe(id);
        let turn = crab.call(&alice, "turn.send", turn_send(id));
        let answers = async {
            let request_id = next_request(&mut sub).await;
            let answer = |approved: bool| {
                serde_json::json!({
                    "session_id": id,
                    "request_id": request_id,
                    "approved": approved,
                    "always": false,
                })
            };
            let (status, response) = crab.call(&bob, "turn.approval_answer", answer(true)).await;
            assert_refused_to_another_principal(status, &response);
            let (status, response) = crab.call(&bob, "turn.cancel", cancel(id)).await;
            assert_refused_to_another_principal(status, &response);
            let (status, response) = crab
                .call(&alice, "turn.approval_answer", answer(false))
                .await;
            assert_eq!(status, StatusCode::OK, "{response:?}");
            assert_eq!(response.result, Some(serde_json::json!(true)));
        };
        let ((status, response), ()) = tokio::join!(turn, answers);
        assert_eq!(status, StatusCode::OK, "{response:?}");
        assert_eq!(crab.ran(), 0, "the tool ran though its owner denied it");
    }

    /// B's stop of A's running turn is refused and the turn goes on; A's own
    /// stop drops it.
    #[tokio::test]
    async fn another_pairing_cannot_cancel_a_turn_its_own_does() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = paired_state(dir.path()).await;
        let (_, alice) = pair(&state, "alice");
        let (_, bob) = pair(&state, "bob");
        let crab = Crab::gated().await;
        let id = crab.session().await;
        let mut sub = gui_stream::hub().subscribe(id);
        let turn = crab.call(&alice, "turn.send", turn_send(id));
        let stops = async {
            let _ = next_request(&mut sub).await;
            let (status, response) = crab.call(&bob, "turn.cancel", cancel(id)).await;
            assert_refused_to_another_principal(status, &response);
            let (status, response) = crab.call(&alice, "turn.cancel", cancel(id)).await;
            assert_eq!(status, StatusCode::OK, "{response:?}");
            assert_eq!(response.result, Some(serde_json::json!(true)));
        };
        let ((status, response), ()) = tokio::join!(turn, stops);
        assert_eq!(status, StatusCode::OK, "{response:?}");
        assert_eq!(
            response.result,
            Some(serde_json::Value::Null),
            "the stop dropped the turn"
        );
        assert_eq!(crab.ran(), 0);
    }

    use crate::a2a::server::{LOOPBACK_OPEN_CREDENTIAL, OWNER_CREDENTIAL, credential, gui_stream};
    use crate::backend::Event;
    use crate::remote::scopes::ALL_SCOPES;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// The owner's key of [`paired_state`].
    const OWNER_KEY: &str = "owner-key";

    /// A gateway state with an api_key set, so `ocpair_` tokens are checked
    /// against its pairing store.
    async fn paired_state(dir: &std::path::Path) -> A2aState {
        A2aState {
            api_key: Some(OWNER_KEY.to_string()),
            ..router_state(dir).await
        }
    }

    /// Pair one device carrying every wire scope: its token, and the
    /// credential the gate resolves it to.
    fn pair(state: &A2aState, label: &str) -> (String, CredentialId) {
        let scopes: Vec<String> = ALL_SCOPES.iter().map(|s| s.to_string()).collect();
        let code = state.pairing.mint_code(&scopes).expect("mint a code");
        let (token, _) = state
            .pairing
            .verify_code(&code, label)
            .expect("redeem the code");
        let (credential, _) = credential(state, Some(token.as_str())).expect("the token admits");
        (token, credential)
    }

    /// The refusal a principal gets for another principal's turn.
    fn assert_refused_to_another_principal(status: StatusCode, response: &JsonRpcResponse) {
        assert_eq!(status, StatusCode::FORBIDDEN, "{response:?}");
        assert_eq!(
            backend_error(response),
            BackendError::Forbidden {
                scope: Scope::Admin
            }
        );
    }

    fn turn_send(session_id: uuid::Uuid) -> serde_json::Value {
        serde_json::json!({
            "session_id": session_id,
            "content": "hello",
            "display": null,
            "voice": false,
        })
    }

    fn cancel(session_id: uuid::Uuid) -> serde_json::Value {
        serde_json::json!({ "session_id": session_id })
    }

    /// The request id of the next approval request on `sub`.
    async fn next_request(sub: &mut gui_stream::Subscription) -> uuid::Uuid {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(30), sub.next())
                .await
                .expect("an approval request within 30s")
                .expect("the session channel stays open");
            if let Event::ApprovalRequested { request_id, .. } = event {
                return request_id;
            }
        }
    }

    /// A tool that needs approval and counts the times it ran.
    struct GatedTool {
        ran: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::brain::tools::Tool for GatedTool {
        fn name(&self) -> &str {
            "approval_tool"
        }

        fn description(&self) -> &str {
            "A tool that requires approval"
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }

        fn capabilities(&self) -> Vec<crate::brain::tools::ToolCapability> {
            Vec::new()
        }

        fn requires_approval(&self) -> bool {
            true
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &crate::brain::tools::ToolExecutionContext,
        ) -> crate::brain::tools::Result<crate::brain::tools::ToolResult> {
            self.ran.fetch_add(1, Ordering::SeqCst);
            Ok(crate::brain::tools::ToolResult::success("ran".to_string()))
        }
    }

    /// A crab on its own temp home and migrated ledger, whose turns call
    /// [`GatedTool`] under the "ask" policy, reached by `gui/call` alone.
    struct Crab {
        home: tempfile::TempDir,
        ctx: Ctx,
        ran: Arc<AtomicUsize>,
    }

    impl Crab {
        async fn gated() -> Self {
            use crate::tests::agent_service_mocks::MockProviderWithNamedTool;

            let home = tempfile::tempdir().expect("temp home");
            std::fs::write(
                home.path().join("config.toml"),
                "[agent]\napproval_policy = \"ask\"\n",
            )
            .expect("write the crab's config");
            let ctx =
                Ctx::new(home.path().join("opencrabs.db")).with_home(home.path().to_path_buf());
            let db = ctx.db().await.expect("db");
            db.run_migrations().await.expect("migrate the ledger");
            let ran = Arc::new(AtomicUsize::new(0));
            let registry = crate::brain::tools::ToolRegistry::new();
            registry.register(Arc::new(GatedTool {
                ran: Arc::clone(&ran),
            }));
            let provider: Arc<dyn crate::brain::provider::Provider> =
                Arc::new(MockProviderWithNamedTool::new("approval_tool"));
            let service = crate::brain::agent::AgentService::new_for_test(
                provider,
                crate::services::ServiceContext::new(db.pool().clone()),
            )
            .await
            .with_tool_registry(Arc::new(registry));
            assert!(
                !service.auto_approves_tools(),
                "the test needs a gating policy"
            );
            ctx.set_service(Arc::new(service));
            Self { home, ctx, ran }
        }

        async fn session(&self) -> uuid::Uuid {
            use crate::db::repository::SessionRepository;

            let db = self.ctx.db().await.expect("db");
            let session = crate::db::models::Session::new(Some("turn".into()), None, None);
            SessionRepository::new(db.pool().clone())
                .create(&session)
                .await
                .expect("create session");
            session.id
        }

        /// `gui/call` `name` with `params` as `credential`, carrying every
        /// wire scope, with this crab's home as the profile home.
        async fn call(
            &self,
            credential: &CredentialId,
            name: &str,
            params: serde_json::Value,
        ) -> (StatusCode, JsonRpcResponse) {
            let granted: Vec<String> = ALL_SCOPES.iter().map(|s| s.to_string()).collect();
            crate::config::profile::with_home_override_async(
                self.home.path().to_path_buf(),
                handle_call_for(
                    Some(&self.ctx),
                    Some(credential),
                    &granted,
                    serde_json::json!(1),
                    call(name, params),
                ),
            )
            .await
        }

        fn ran(&self) -> usize {
            self.ran.load(Ordering::SeqCst)
        }
    }
}
