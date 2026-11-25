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

/// The first `BangOutput` the sink receives, or a failure naming what came.
fn bang_output(rx: &std::sync::mpsc::Receiver<TurnEvent>) -> String {
    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(TurnEvent::BangOutput(text)) => text,
        Ok(_) => panic!("the sink got another event first"),
        Err(e) => panic!("no bang output: {e}"),
    }
}

/// `!ls` with a remote crab is the crab's to run. A token without admin is
/// refused at its gateway: the sink gets the 403 in `cap::notice` words,
/// which a run on this machine could never produce.
#[test]
fn a_bang_command_goes_to_the_remote_crab_and_its_refusal_comes_back() {
    let served = serve(mock(
        Ctx::new(PathBuf::from("unused.db")),
        &[Scope::Read, Scope::Send, Scope::Manage],
    ));
    let backend = Backend::Remote(served.crab.clone());
    let (sink, rx) = std::sync::mpsc::channel();
    crate::bang::spawn(&backend, "ls".to_string(), sink);
    let text = bang_output(&rx);
    assert_eq!(
        text,
        "$ ls\nshell.run: needs a2a:admin: this crab's token does not grant it"
    );
    assert_eq!(
        served.mock.calls.load(Ordering::SeqCst),
        1,
        "the command crossed the wire once"
    );
}

/// An editor needs a terminal on this machine: with a remote crab the sink
/// gets the reason, and nothing is asked of the crab or opened here.
#[test]
fn an_editor_with_a_remote_crab_answers_with_the_reason() {
    let served = serve(mock(Ctx::new(PathBuf::from("unused.db")), Scope::ALL));
    let backend = Backend::Remote(served.crab.clone());
    let (sink, rx) = std::sync::mpsc::channel();
    crate::bang::spawn(&backend, "vim notes.txt".to_string(), sink);
    assert_eq!(
        bang_output(&rx),
        "$ vim notes.txt\nonly on a crab on this machine"
    );
    assert_eq!(served.mock.calls.load(Ordering::SeqCst), 0);
}

/// The /cd picker's rows come from the crab's `ListDir`. A folder that does
/// exist on this machine is refused by a remote crab without admin: the
/// answer is the crab's 403, never this disk's listing.
#[test]
fn the_cd_listing_is_asked_of_the_remote_crab() {
    use opencrabs::backend::fs_list::ListDir;

    let here = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(here.path().join("only-here")).expect("mkdir");
    let served = serve(mock(Ctx::new(PathBuf::from("unused.db")), &[Scope::Read]));
    let backend = Backend::Remote(served.crab.clone());
    let reply = futures::executor::block_on(backend.call(ListDir {
        dir: here.path().to_path_buf(),
        show_hidden: false,
    }));
    let err = reply.expect_err("the crab refuses a reader");
    assert_eq!(
        err,
        BackendError::Forbidden {
            scope: Scope::Admin
        }
    );
    assert_eq!(
        opencrabs::backend::cap::notice("fs_list.list_dir", &err),
        "fs_list.list_dir: needs a2a:admin: this crab's token does not grant it"
    );
    assert_eq!(served.mock.calls.load(Ordering::SeqCst), 1);
}

/// `/cd /nao/existe` is checked on the crab: the remote door answers the
/// line the window used to print after looking at its own disk.
#[test]
fn a_cd_to_a_missing_folder_is_refused_by_the_crab_in_the_windows_words() {
    use opencrabs::backend::session_ops::ApplyCd;

    let served = serve(mock(Ctx::new(PathBuf::from("unused.db")), Scope::ALL));
    let backend = Backend::Remote(served.crab.clone());
    let reply = futures::executor::block_on(backend.call(ApplyCd {
        session_id: Uuid::new_v4(),
        dir: PathBuf::from("/nao/existe"),
    }));
    assert_eq!(
        reply,
        Err(BackendError::Failed(
            opencrabs::backend::session_ops::cd_not_a_directory("/nao/existe")
        ))
    );
}

/// The window's halves of the three doors, read off the source: `/cd` with
/// a path never asks this disk, the picker never lists it or falls back to
/// this process's folder, and the footer's branch is never read through the
/// synchronous local door.
#[test]
fn the_cd_and_footer_doors_never_read_this_machine() {
    let src = crate::dashboard_source::DASHBOARD;
    let cd = src
        .split("C::Cd => {")
        .nth(1)
        .and_then(|rest| rest.split("\n            other => {").next())
        .expect("the /cd arm");
    assert!(
        !cd.contains(".is_dir()"),
        "the /cd arm probes no folder here"
    );

    let picker = src
        .split("pub(crate) fn open_cd_picker(")
        .nth(1)
        .and_then(|rest| rest.split("pub(crate) fn toggle_help(").next())
        .expect("the picker's functions");
    for local in ["cd_entries_for(", "current_dir(", "ask_crab("] {
        assert!(!picker.contains(local), "the picker calls {local}");
    }
    assert!(
        picker.contains("backend.call(op)"),
        "the picker asks the crab the window holds"
    );

    let footer = src
        .split("fn footer_place(")
        .nth(1)
        .and_then(|rest| rest.split("\n    pub(crate) fn ").next())
        .expect("the footer's place");
    assert!(!footer.contains("ask_crab("));
    let status = include_str!("../status.rs");
    let branch = status
        .split("pub(crate) fn git_branch(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("git_branch");
    assert!(!branch.contains("ask_crab("), "git_branch waits on no crab");
}
