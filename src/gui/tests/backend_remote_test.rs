//! The remote door (`backend_remote.rs`): operations over `gui/call`, events
//! over `/gui/events`, and the refusals of both, against a mock gateway.
//!
//! These lived in an inline `mod tests` at the bottom of `backend_remote.rs`.
//! The layout guard (#3062) freezes inline modules under `gui/` and says
//! where their tests go: beside the module they exercise, reached through
//! `crate::` paths. The tests moved whole.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response, Sse, sse};
use axum::routing::{get, post};
use opencrabs::backend::sessions::LoadSessions;
use opencrabs::backend::{BackendError, Ctx, Event, Scope};
use opencrabs::brain::agent::ProgressEvent;
use opencrabs::remote::instance_client;
use serde_json::Value;
use uuid::Uuid;

use crate::agent_bridge::TurnEvent;
use crate::backend_glue::Backend;
use crate::backend_remote::{Frames, RemoteCrab, call_json, decode_reply, to_turn_event, watch};

/// A stand-in for a crab's gateway. `POST /a2a/v1` answers `gui/call`
/// in the gateway's shape (`src/a2a/handler/gui.rs`): the operation runs
/// through `backend::dispatch_as` for a caller holding `granted`, a
/// failure carries the typed error in `data`, and a refused scope is an
/// HTTP 403. `GET /gui/events` serves `events` once per connection and
/// closes, or answers `events_status` when that is not 200.
struct Mock {
    ctx: Ctx,
    granted: Vec<Scope>,
    events: Vec<Event>,
    events_status: StatusCode,
    dials: AtomicUsize,
    /// Every `POST /a2a/v1` it answered: one per round trip.
    calls: AtomicUsize,
}

async fn rpc(State(mock): State<Arc<Mock>>, Json(req): Json<Value>) -> Response {
    mock.calls.fetch_add(1, Ordering::SeqCst);
    let id = req["id"].clone();
    let name = req["params"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let params = req["params"]["params"].clone();
    match opencrabs::backend::dispatch_as(&mock.ctx, &mock.granted, &name, params).await {
        Ok(result) => Json(serde_json::json!({ "jsonrpc": "2.0", "result": result, "id": id }))
            .into_response(),
        Err(err) => {
            let (status, code) = match &err {
                BackendError::UnknownOp(_) => (StatusCode::OK, -32601),
                BackendError::Forbidden { .. } => (StatusCode::FORBIDDEN, -32003),
                BackendError::BadParams(_) => (StatusCode::OK, -32602),
                _ => (StatusCode::OK, -32603),
            };
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "error": { "code": code, "message": err.to_string(), "data": err },
                "id": id,
            });
            (status, Json(body)).into_response()
        }
    }
}

async fn events(State(mock): State<Arc<Mock>>) -> Response {
    mock.dials.fetch_add(1, Ordering::SeqCst);
    if mock.events_status != StatusCode::OK {
        return mock.events_status.into_response();
    }
    let frames: Vec<_> = mock
        .events
        .iter()
        .map(|event| sse::Event::default().json_data(event))
        .collect();
    Sse::new(futures::stream::iter(frames)).into_response()
}

/// A served mock and the runtime it runs on; the runtime must outlive
/// the test's calls.
struct Served {
    runtime: tokio::runtime::Runtime,
    mock: Arc<Mock>,
    crab: Arc<RemoteCrab>,
}

fn serve(mock: Mock) -> Served {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let mock = Arc::new(mock);
    let app = Router::new()
        .route("/a2a/v1", post(rpc))
        .route("/gui/events", get(events))
        .with_state(mock.clone());
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    runtime.spawn(async move { axum::serve(listener, app).await });
    let crab = Arc::new(RemoteCrab::new(
        "mock",
        format!("http://{addr}/"),
        Some("t0ken".into()),
        PathBuf::from("unused.db"),
    ));
    Served {
        runtime,
        mock,
        crab,
    }
}

fn mock(ctx: Ctx, granted: &[Scope]) -> Mock {
    Mock {
        ctx,
        granted: granted.to_vec(),
        events: Vec::new(),
        events_status: StatusCode::OK,
        dials: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
    }
}

/// Each operation's fixture request, by wire name.
fn fixture_requests() -> std::collections::HashMap<String, Value> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gui_backend");
    let mut out = std::collections::HashMap::new();
    for entry in std::fs::read_dir(&dir).expect("fixtures dir") {
        let path = entry.expect("fixture entry").path();
        let text = std::fs::read_to_string(&path).expect("fixture");
        let fixture: Value = serde_json::from_str(&text).expect("fixture json");
        if let Some(op) = fixture["op"].as_str() {
            out.insert(op.to_string(), fixture["request"].clone());
        }
    }
    out
}

fn by_remote(
    served: &Served,
    name: &str,
    scope: Scope,
    params: Value,
) -> Result<Value, BackendError> {
    instance_client::block_on(async {
        anyhow::Ok(call_json(&served.crab, name, Some(scope), params).await)
    })
    .expect("the shared runtime ran the call")
}

#[test]
fn the_registry_has_operations_to_carry() {
    assert!(
        !opencrabs::backend::registry().is_empty(),
        "the per-operation tests below would pass vacuously"
    );
}

/// Every registered operation, with its own fixture request, crosses the
/// remote door and comes back refused exactly as the local door refuses
/// a caller without its scope. Nothing runs: the gate is first.
#[test]
fn every_operation_is_refused_alike_by_both_doors() {
    let served = serve(mock(Ctx::new(PathBuf::from("unused.db")), &[]));
    let requests = fixture_requests();
    for (name, scope) in opencrabs::backend::registry() {
        let params = requests.get(name).cloned().unwrap_or(Value::Null);
        let local = served.runtime.block_on(opencrabs::backend::dispatch_as(
            &served.mock.ctx,
            &[],
            name,
            params.clone(),
        ));
        let remote = by_remote(&served, name, scope, params);
        assert_eq!(remote, local, "{name}");
        assert_eq!(remote, Err(BackendError::Forbidden { scope }), "{name}");
    }
}

/// Every registered operation, holding every scope, reaches its params by
/// the remote door and is answered as the local door answers it. The
/// params are a JSON string, which no operation decodes, so nothing runs.
#[test]
fn every_operation_reaches_its_params_alike_by_both_doors() {
    let served = serve(mock(Ctx::new(PathBuf::from("unused.db")), Scope::ALL));
    let probe = Value::String("remote door probe".into());
    for (name, scope) in opencrabs::backend::registry() {
        let local = served.runtime.block_on(opencrabs::backend::dispatch_as(
            &served.mock.ctx,
            Scope::ALL,
            name,
            probe.clone(),
        ));
        let remote = by_remote(&served, name, scope, probe.clone());
        assert_eq!(remote, local, "{name}");
        assert!(
            matches!(remote, Err(BackendError::BadParams(_))),
            "{name}: {remote:?}"
        );
    }
}

/// A real reply crosses typed: `Backend::Remote` and `Backend::Local` on
/// the same ledger list the same sessions.
#[test]
fn a_reply_crosses_the_remote_door_typed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("remote-door.db");
    let served = serve(mock(Ctx::new(db_path.clone()), Scope::ALL));
    served.runtime.block_on(async {
        let db = opencrabs::db::Database::connect(&db_path)
            .await
            .expect("connect");
        db.run_migrations().await.expect("migrate");
        let repo = opencrabs::db::repository::SessionRepository::new(db.pool().clone());
        for title in ["one", "two"] {
            repo.create(&opencrabs::db::models::Session::new(
                Some(title.into()),
                None,
                None,
            ))
            .await
            .expect("create");
        }
    });
    let local = Backend::Local(Arc::new(Ctx::new(db_path)));
    let remote = Backend::Remote(served.crab.clone());
    let local = futures::executor::block_on(local.call(LoadSessions {})).expect("local");
    let remote = futures::executor::block_on(remote.call(LoadSessions {})).expect("remote");
    assert_eq!(remote.len(), 2);
    assert_eq!(
        serde_json::to_value(&remote).expect("encode"),
        serde_json::to_value(&local).expect("encode")
    );
}

#[test]
fn a_crab_that_is_not_there_is_a_failure_naming_it() {
    // Port 9 on loopback: nothing listens, the connect is refused.
    let crab = Arc::new(RemoteCrab::new(
        "gone",
        "http://127.0.0.1:9",
        None,
        PathBuf::from("unused.db"),
    ));
    let err = futures::executor::block_on(Backend::Remote(crab).call(LoadSessions {}))
        .expect_err("nothing listens");
    match err {
        BackendError::Failed(text) => {
            assert!(text.starts_with("gone: sessions.load: "), "{text}")
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_typed_error_in_data_comes_back_as_itself() {
    let refused = BackendError::Forbidden {
        scope: Scope::Admin,
    };
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": -32003, "message": "forbidden", "data": refused },
        "id": 1,
    });
    assert_eq!(
        decode_reply("c", "x.y", 403, &body.to_string()),
        Err(BackendError::Forbidden {
            scope: Scope::Admin
        })
    );
}

#[test]
fn a_crab_without_gui_call_does_not_know_the_operation() {
    let body = r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#;
    assert_eq!(
        decode_reply("c", "sessions.load", 200, body),
        Err(BackendError::UnknownOp("sessions.load".into()))
    );
}

#[test]
fn a_refused_bearer_is_a_failure_with_its_status() {
    assert_eq!(
        decode_reply("c", "sessions.load", 401, "unauthorized"),
        Err(BackendError::Failed(
            "c: sessions.load: credential refused (HTTP 401)".into()
        ))
    );
}

#[test]
fn a_null_result_is_a_reply() {
    let body = r#"{"jsonrpc":"2.0","result":null,"id":1}"#;
    assert_eq!(decode_reply("c", "x.y", 200, body), Ok(Value::Null));
}

#[test]
fn frames_survive_any_split() {
    let session = Uuid::new_v4();
    let first = Event::TurnDone {
        session_id: session,
    };
    let second = Event::RailChanged;
    let wire = format!(
        ":keep-alive\r\n\r\ndata: {}\r\n\r\nevent: x\ndata:{}\n\n",
        serde_json::to_string(&first).expect("encode"),
        serde_json::to_string(&second).expect("encode"),
    );
    let wire = wire.as_bytes();
    for cut in 0..wire.len() {
        let mut frames = Frames::default();
        let mut got = frames.push(&wire[..cut]);
        got.extend(frames.push(&wire[cut..]));
        assert_eq!(got, vec![first.clone(), second.clone()], "cut at {cut}");
    }
}

#[test]
fn a_data_field_may_span_lines() {
    let mut frames = Frames::default();
    let got = frames.push(b"data: {\"kind\":\ndata: \"rail_changed\"}\n\n");
    assert_eq!(got, vec![Event::RailChanged]);
}

#[test]
fn a_frame_that_does_not_decode_is_skipped() {
    let mut frames = Frames::default();
    let got =
        frames.push(b"data: {\"kind\":\"no_such\"}\n\ndata: {\"kind\":\"tasks_changed\"}\n\n");
    assert_eq!(got, vec![Event::TasksChanged]);
}

#[test]
fn turn_progress_reaches_the_window_and_settles_do_not() {
    let session = Uuid::new_v4();
    let progress = ProgressEvent::StreamingChunk { text: "wor".into() };
    match to_turn_event(Event::Turn {
        session_id: session,
        progress: progress.clone(),
    }) {
        Some(TurnEvent::ProgressFor(id, got, _)) => {
            assert_eq!(id, session);
            assert_eq!(got, progress);
        }
        _ => panic!("turn progress must reach the window"),
    }
    assert!(
        to_turn_event(Event::TurnDone {
            session_id: session
        })
        .is_none()
    );
    assert!(
        to_turn_event(Event::TurnFailed {
            session_id: session,
            message: "x".into()
        })
        .is_none()
    );
    assert!(
        to_turn_event(Event::TurnCancelled {
            session_id: session
        })
        .is_none()
    );
}

fn next(rx: &std::sync::mpsc::Receiver<TurnEvent>) -> TurnEvent {
    rx.recv_timeout(Duration::from_secs(20))
        .expect("the reader delivered in time")
}

/// The reader delivers the stream's events, says once that it dropped,
/// dials again, and says it is back.
#[test]
fn the_reader_delivers_and_reconnects() {
    let session = Uuid::new_v4();
    let mut m = mock(Ctx::new(PathBuf::from("unused.db")), Scope::ALL);
    m.events = vec![Event::Turn {
        session_id: session,
        progress: ProgressEvent::Thinking,
    }];
    let served = serve(m);
    let (sink, rx) = std::sync::mpsc::channel();
    let backend = Backend::Remote(served.crab.clone());
    let watch = backend
        .watch(session, sink)
        .expect("reader started")
        .expect("a remote backend has a reader");

    assert!(
        matches!(next(&rx), TurnEvent::ProgressFor(id, ProgressEvent::Thinking, _) if id == session)
    );
    match next(&rx) {
        TurnEvent::CommandReply {
            session: None,
            text,
        } => {
            assert!(text.contains("reconnecting"), "{text}")
        }
        _ => panic!("the drop is told to the notices"),
    }
    match next(&rx) {
        TurnEvent::CommandReply {
            session: None,
            text,
        } => {
            assert!(text.contains("back"), "{text}")
        }
        _ => panic!("the reconnect is told to the notices"),
    }
    assert!(
        matches!(next(&rx), TurnEvent::ProgressFor(id, ProgressEvent::Thinking, _) if id == session)
    );
    assert!(served.mock.dials.load(Ordering::SeqCst) >= 2);
    drop(watch);
}

/// A refused credential ends the reader with one notice.
#[test]
fn a_refused_reader_says_so_and_stops() {
    let mut m = mock(Ctx::new(PathBuf::from("unused.db")), &[]);
    m.events_status = StatusCode::FORBIDDEN;
    let served = serve(m);
    let (sink, rx) = std::sync::mpsc::channel();
    let _watch = watch(served.crab.clone(), Uuid::new_v4(), sink).expect("reader started");
    match next(&rx) {
        TurnEvent::CommandReply {
            session: None,
            text,
        } => {
            assert!(text.contains("HTTP 403"), "{text}")
        }
        _ => panic!("the refusal is told to the notices"),
    }
    // The reader ended and dropped its sink: the channel disconnects.
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(20)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
    ));
    assert_eq!(served.mock.dials.load(Ordering::SeqCst), 1);
}

#[test]
fn a_local_backend_has_no_reader() {
    let (sink, _rx) = std::sync::mpsc::channel();
    let local = Backend::local(PathBuf::from("unused.db"));
    assert!(
        local
            .watch(Uuid::new_v4(), sink)
            .expect("no thread")
            .is_none()
    );
}

/// The acceptance of C4: what the window reads while it renders (the facts
/// and the running tasks) never reaches the network. A thousand frames of
/// reads cost no `gui/call`; opening the session's events is what reads the
/// crab, off the window's thread.
#[test]
fn render_reads_cost_zero_network_calls() {
    let served = serve(mock(Ctx::new(PathBuf::from("unused.db")), Scope::ALL));
    let backend = Backend::Remote(served.crab.clone());
    let session = Uuid::new_v4();

    for _ in 0..1000 {
        assert_eq!(backend.facts(Some(session)), None);
        assert_eq!(backend.facts(None), None);
        assert!(backend.tasks_snapshot(vec![session]).is_empty());
    }
    assert_eq!(
        served.mock.calls.load(Ordering::SeqCst),
        0,
        "a read inside a frame went over the wire"
    );

    // Opening the events reads what the frames asked about, off this thread.
    let (sink, _rx) = std::sync::mpsc::channel();
    let watch = backend
        .watch(session, sink)
        .expect("reader started")
        .expect("a remote backend has a reader");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while served.mock.calls.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        served.mock.calls.load(Ordering::SeqCst) > 0,
        "the cache was never read"
    );

    drop(watch);
}
