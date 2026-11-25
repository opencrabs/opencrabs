//! Mission Control on a crab that is not this machine: Shift+L
//! (`dashboard_screens::open_logs`) and the refusals of `decide` and `load`.
//!
//! The log viewer reads this machine's log folder, so a remote crab answers
//! with a note and no viewer. A 403 from the crab reads as the reason `cap`
//! gives, not as the raw text of the error.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use opencrabs::backend::{BackendError, Scope};
use opencrabs::brain::mission_control::{McInboxKind, TimeWindow};

use crate::backend_glue::{Backend, RemoteCrab};
use crate::dashboard_screens::open_logs;
use crate::mission_control::{Decision, decide, load};

fn remote() -> Backend {
    Backend::Remote(Arc::new(RemoteCrab::new(
        "elsewhere",
        "http://127.0.0.1:9",
        None,
        PathBuf::from("unused.db"),
    )))
}

fn local() -> Backend {
    Backend::local(PathBuf::from("unused.db"))
}

/// A gateway that refuses every `gui/call` with a 403 for `a2a:admin`, in
/// the shape the crab's own refusal takes (`src/a2a/handler/gui.rs`).
async fn refuse_admin(Json(req): Json<serde_json::Value>) -> Response {
    let refused = BackendError::Forbidden {
        scope: Scope::Admin,
    };
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": -32003, "message": refused.to_string(), "data": refused },
        "id": req["id"].clone(),
    });
    (StatusCode::FORBIDDEN, Json(body)).into_response()
}

/// The refusing gateway and the runtime it runs on; the runtime must
/// outlive the test's calls.
fn refusing_crab() -> (tokio::runtime::Runtime, Backend) {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let app = Router::new().route("/a2a/v1", post(refuse_admin));
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    runtime.spawn(async move { axum::serve(listener, app).await });
    let crab = RemoteCrab::new(
        "refusing",
        format!("http://{addr}/"),
        Some("t0ken".into()),
        PathBuf::from("unused.db"),
    );
    (runtime, Backend::Remote(Arc::new(crab)))
}

#[test]
fn shift_l_on_a_remote_crab_leaves_a_note_and_opens_no_viewer() {
    // Nothing dials the crab: the capability is the backend's own answer.
    let notice = match open_logs(&remote()) {
        Ok(_) => panic!("a remote crab opened this machine's logs"),
        Err(notice) => notice,
    };
    assert!(!notice.ok, "{notice:?}");
    assert!(notice.text.contains("this machine"), "{}", notice.text);
}

#[test]
fn shift_l_on_a_local_crab_opens_the_viewer_as_it_always_has() {
    assert!(open_logs(&local()).is_ok());
}

#[test]
fn an_apply_refused_for_admin_reads_as_the_missing_scope() {
    let (_runtime, backend) = refusing_crab();
    let line = futures::executor::block_on(decide(
        backend,
        "prop_tool_x".to_string(),
        McInboxKind::ProposedTool,
        Decision::Apply,
    ))
    .expect_err("refused");
    assert!(line.starts_with("mission.decide: "), "{line}");
    assert!(line.contains("a2a:admin"), "{line}");
    assert_eq!(
        line,
        opencrabs::backend::cap::notice(
            "mission.decide",
            &BackendError::Forbidden {
                scope: Scope::Admin
            }
        )
    );
}

#[test]
fn a_refused_load_reads_as_the_missing_scope() {
    let (_runtime, backend) = refusing_crab();
    let line = match futures::executor::block_on(load(backend, TimeWindow::Day)) {
        Ok(_) => panic!("a refused read came back with a snapshot"),
        Err(line) => line,
    };
    assert!(line.starts_with("mission.load: "), "{line}");
    assert!(line.contains("a2a:admin"), "{line}");
}

#[test]
fn a_failure_that_is_not_about_access_keeps_its_text() {
    // Nothing listens on port 9: the dial fails, and the text names the crab
    // and the operation as it always did, with no scope in it.
    let line = futures::executor::block_on(decide(
        remote(),
        "prop_tool_x".to_string(),
        McInboxKind::ProposedTool,
        Decision::Reject,
    ))
    .expect_err("nothing listens");
    assert!(line.starts_with("elsewhere: mission.decide: "), "{line}");
    assert!(!line.contains("a2a:admin"), "{line}");
}
