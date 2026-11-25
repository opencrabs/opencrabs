//! The window's handle on another crab: [`Backend::Remote`](super::Backend).
//!
//! Three doors, all over that crab's A2A gateway and the transport of
//! `opencrabs::remote::instance_client` (C1):
//!
//! - an operation: [`call`], one `gui/call` JSON-RPC POST carrying the same
//!   [`Op`] value the local backend runs. The other crab decodes it and runs
//!   it through `backend::dispatch_as`, so the reply is the one a local call
//!   would have returned.
//! - its events: [`watch`], a reader on `GET /gui/events?session=<id>` that
//!   pushes into the window's own [`EventSink`]. The window's poll does not
//!   know where an event came from.
//! - a refusal: every failure is a [`BackendError`]. A typed one rides in the
//!   JSON-RPC error's `data` and comes back as itself; a failure of the wire
//!   becomes `Failed` with the crab's name, the text the notices print.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use opencrabs::backend::{BackendError, Event, Op, Scope};
use opencrabs::remote::instance_client::{self, CALL_TIMEOUT};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent_bridge::{EventSink, TurnEvent};

/// The first wait before the event reader dials again after a drop.
const RETRY_FIRST: Duration = Duration::from_secs(1);
/// The longest wait between two dials: a crab that is down for an hour is
/// asked twice a minute, not once a second.
const RETRY_MAX: Duration = Duration::from_secs(30);

/// Another crab: where its gateway answers and the credential it was paired
/// with. Deliberately no `Debug`: the token must not reach a log line.
pub struct RemoteCrab {
    /// What the notices call it.
    name: String,
    /// The gateway's base URL, `http://host:port`.
    base_url: String,
    /// The bearer the pairing handed out, when the gateway wants one.
    token: Option<String>,
    /// This window's own ledger, for the bridge callers that still open a
    /// ledger by path (see [`super::Backend::local_db_path`]).
    window_ledger: PathBuf,
}

impl RemoteCrab {
    /// A crab at `base_url`. `window_ledger` is this machine's ledger, never
    /// the remote one: a remote crab's ledger is only reached by operations.
    #[allow(dead_code)] // the instance rail (C3) builds it when a crab is picked
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        token: Option<String>,
        window_ledger: PathBuf,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            token,
            window_ledger,
        }
    }

    /// This window's own ledger.
    pub fn window_ledger(&self) -> &Path {
        &self.window_ledger
    }

    /// `path` on this crab's gateway, without a doubled slash.
    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// A request with this crab's bearer on it, when it has one.
    fn authed(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => req.bearer_auth(token),
            None => req,
        }
    }
}

/// Run `op` on `crab` and decode its reply.
///
/// The params are the op itself as JSON, exactly what `backend::run_json`
/// decodes on the other side. A reply that does not decode is answered the
/// way the parity harness answers it, so a remote mismatch reads the same in
/// a test and in the window.
pub(super) async fn call<O: Op>(crab: &RemoteCrab, op: O) -> Result<O::Reply, BackendError> {
    let params = serde_json::to_value(&op)
        .map_err(|e| BackendError::BadParams(format!("{}: {e}", O::NAME)))?;
    let reply = call_json(crab, O::NAME, Some(O::SCOPE), params).await?;
    serde_json::from_value(reply)
        .map_err(|e| BackendError::BadParams(format!("{}: reply decode: {e}", O::NAME)))
}

/// One `gui/call`: the operation called `name` with JSON `params`.
///
/// A read is bounded by the transport's data deadline ([`CALL_TIMEOUT`]): it
/// is a ledger read on the other side. Anything else (a turn, a shell
/// command, a brain generation) runs for as long as the crab works on it, as
/// it does locally, so only the connect is bounded (the shared client's).
/// `scope` is `None` when the caller does not know it, which is read as
/// "not a read".
pub(super) async fn call_json(
    crab: &RemoteCrab,
    name: &str,
    scope: Option<Scope>,
    params: Value,
) -> Result<Value, BackendError> {
    let body = instance_client::rpc_body(
        "gui/call",
        serde_json::json!({ "name": name, "params": params }),
    );
    let mut req = opencrabs::utils::http::shared()
        .post(crab.url("/a2a/v1"))
        .json(&body);
    if scope == Some(Scope::Read) {
        req = req.timeout(CALL_TIMEOUT);
    }
    let resp = crab
        .authed(req)
        .send()
        .await
        .map_err(|e| BackendError::Failed(format!("{}: {name}: {e}", crab.name)))?;
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .await
        .map_err(|e| BackendError::Failed(format!("{}: {name}: {e}", crab.name)))?;
    decode_reply(&crab.name, name, status, &text)
}

/// The reply of one `gui/call`, from its HTTP status and body.
///
/// In order: a typed [`BackendError`] in the error's `data` is the answer
/// itself (a 403 carries `Forbidden` there); a refused bearer is a 401 with
/// no typed error; `-32601` without one is a crab built before `gui/call`
/// existed, which does not know the operation either; any other error is a
/// failure with the crab's own message.
fn decode_reply(crab: &str, name: &str, status: u16, body: &str) -> Result<Value, BackendError> {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    if let Some(err) = parsed.as_ref().and_then(|b| b.get("error")) {
        if let Some(typed) = err
            .get("data")
            .and_then(|d| serde_json::from_value::<BackendError>(d.clone()).ok())
        {
            return Err(typed);
        }
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("remote error");
        // -32601 is JSON-RPC MethodNotFound, the code the gateway gives an
        // unknown method (`error_codes::METHOD_NOT_FOUND`).
        if err.get("code").and_then(Value::as_i64) == Some(-32601) {
            return Err(BackendError::UnknownOp(name.to_string()));
        }
        return Err(BackendError::Failed(format!("{crab}: {name}: {message}")));
    }
    if status == 401 || status == 403 {
        return Err(BackendError::Failed(format!(
            "{crab}: {name}: credential refused (HTTP {status})"
        )));
    }
    match parsed.and_then(|b| b.get("result").cloned()) {
        Some(result) => Ok(result),
        None => Err(BackendError::Failed(format!(
            "{crab}: {name}: the reply carried no result (HTTP {status})"
        ))),
    }
}

/// Run `op` on `crab` off the window's thread and hand its reply to `done`.
///
/// The call runs on the transport's shared runtime (`instance_client`), the
/// reactor its pooled connections belong to, from a thread of its own: a
/// remote reply is a round trip, and the window never waits on one. `Err`
/// means the thread never started, so `done` will not run.
pub(super) fn spawn<O: Op>(
    crab: Arc<RemoteCrab>,
    op: O,
    done: impl FnOnce(Result<O::Reply, BackendError>) + Send + 'static,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name(format!("remote-{}", O::NAME))
        .spawn(move || {
            let reply = instance_client::block_on(async { anyhow::Ok(call(&crab, op).await) })
                .unwrap_or_else(|e| Err(BackendError::Failed(format!("{}: {e:#}", crab.name))));
            done(reply);
        })
        .map(|_| ())
        .map_err(|e| format!("{} thread: {e}", O::NAME))
}

/// An open event reader. Dropping it stops the reader and closes the stream.
pub struct Watch {
    stop: CancellationToken,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Read `session`'s events on `crab` into `sink` until the [`Watch`] drops.
///
/// The reader dials again after every drop, waiting [`RETRY_FIRST`] and
/// doubling up to [`RETRY_MAX`]; a stream that delivered something starts
/// the wait over. The notices hear once that the stream is gone and once
/// that it is back, never once per dial. A refused credential ends the
/// reader with a notice: dialling again would only be refused again. So does
/// a window that went away (the sink is closed).
pub(super) fn watch(
    crab: Arc<RemoteCrab>,
    session: Uuid,
    sink: EventSink,
) -> Result<Watch, String> {
    let stop = CancellationToken::new();
    let reader_stop = stop.clone();
    std::thread::Builder::new()
        .name("remote-events".into())
        .spawn(move || {
            let _ = instance_client::block_on(async move {
                read_events(&crab, session, &sink, &reader_stop).await;
                anyhow::Ok(())
            });
        })
        .map_err(|e| format!("event reader thread: {e}"))?;
    Ok(Watch { stop })
}

/// How one connection of the event reader ended.
enum Ended {
    /// The window's sink is closed: nobody is left to read.
    SinkGone,
    /// The crab refused the credential; carries the notice.
    Refused(String),
    /// The stream dropped or never opened; carries why, and whether it
    /// delivered an event first.
    Dropped { reason: String, delivered: bool },
}

/// The reconnect loop around [`stream_once`].
async fn read_events(crab: &RemoteCrab, session: Uuid, sink: &EventSink, stop: &CancellationToken) {
    let mut delay = RETRY_FIRST;
    let mut down = false;
    loop {
        let ended = tokio::select! {
            _ = stop.cancelled() => return,
            ended = stream_once(crab, session, sink, &mut down) => ended,
        };
        match ended {
            Ended::SinkGone => return,
            Ended::Refused(text) => {
                notice(sink, text);
                return;
            }
            Ended::Dropped { reason, delivered } => {
                if delivered {
                    delay = RETRY_FIRST;
                }
                if !down {
                    down = true;
                    let text = format!("{}: event stream lost ({reason}); reconnecting", crab.name);
                    if !notice(sink, text) {
                        return;
                    }
                }
            }
        }
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(delay) => {}
        }
        delay = (delay * 2).min(RETRY_MAX);
    }
}

/// One connection to `GET /gui/events`, read until it ends. `down` says the
/// notices were told the stream is gone; an open stream tells them it is
/// back and clears it.
async fn stream_once(crab: &RemoteCrab, session: Uuid, sink: &EventSink, down: &mut bool) -> Ended {
    let req =
        opencrabs::utils::http::shared().get(crab.url(&format!("/gui/events?session={session}")));
    let mut resp = match crab.authed(req).send().await {
        Ok(resp) => resp,
        Err(e) => {
            return Ended::Dropped {
                reason: e.to_string(),
                delivered: false,
            };
        }
    };
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Ended::Refused(format!(
            "{}: the crab refused its events to this credential (HTTP {status})",
            crab.name
        ));
    }
    if !resp.status().is_success() {
        return Ended::Dropped {
            reason: format!("HTTP {status}"),
            delivered: false,
        };
    }
    if *down {
        *down = false;
        if !notice(sink, format!("{}: event stream back", crab.name)) {
            return Ended::SinkGone;
        }
    }
    let mut frames = Frames::default();
    let mut delivered = false;
    loop {
        match resp.chunk().await {
            Ok(Some(bytes)) => {
                for event in frames.push(&bytes) {
                    delivered = true;
                    if let Some(turn_event) = to_turn_event(event)
                        && sink.send(turn_event).is_err()
                    {
                        return Ended::SinkGone;
                    }
                }
            }
            Ok(None) => {
                return Ended::Dropped {
                    reason: "closed by the crab".into(),
                    delivered,
                };
            }
            Err(e) => {
                return Ended::Dropped {
                    reason: e.to_string(),
                    delivered,
                };
            }
        }
    }
}

/// Put `text` in the window's notices. `false` when the window is gone.
fn notice(sink: &EventSink, text: String) -> bool {
    sink.send(TurnEvent::CommandReply {
        session: None,
        text,
    })
    .is_ok()
}

/// The window event a crab event becomes, if any.
///
/// No wildcard: a new event kind has to say what the window does with it.
fn to_turn_event(event: Event) -> Option<TurnEvent> {
    match event {
        // The same progress the local service forwarder hands the window.
        Event::Turn {
            session_id,
            progress,
        } => Some(TurnEvent::ProgressFor(session_id, progress, Instant::now())),
        // A turn this window started settles on the reply of the call that
        // started it, exactly as a local one does; a second settle from the
        // stream would close the card twice.
        Event::TurnDone { .. } | Event::TurnFailed { .. } | Event::TurnCancelled { .. } => None,
        // Reload hints for the remote cache, which the window has no event
        // for yet (C4 keeps that cache).
        Event::RailChanged | Event::TranscriptChanged { .. } | Event::TasksChanged => None,
    }
}

/// The SSE framing of `GET /gui/events`: `data:` lines up to a blank line
/// make one event, one [`Event`] as JSON. Comment lines (the keep-alive) and
/// the other fields are skipped. Chunks split anywhere, a line included.
#[derive(Default)]
struct Frames {
    /// The bytes of the line not yet ended.
    line: Vec<u8>,
    /// The `data:` lines of the event not yet ended.
    data: Option<String>,
}

impl Frames {
    /// Feed one chunk; the events it completed, in order.
    fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut out = Vec::new();
        for &byte in bytes {
            if byte != b'\n' {
                self.line.push(byte);
                continue;
            }
            let mut line = std::mem::take(&mut self.line);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(event) = self.end_line(&String::from_utf8_lossy(&line)) {
                out.push(event);
            }
        }
        out
    }

    /// One whole line; the event it completed, if it was the blank one.
    fn end_line(&mut self, line: &str) -> Option<Event> {
        if line.is_empty() {
            let data = self.data.take()?;
            return match serde_json::from_str(&data) {
                Ok(event) => Some(event),
                Err(e) => {
                    tracing::warn!("gui/events: skipped an event that does not decode: {e}");
                    None
                }
            };
        }
        if let Some(value) = line.strip_prefix("data:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::Backend;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::Router;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Json, Response, Sse, sse};
    use axum::routing::{get, post};
    use opencrabs::backend::Ctx;
    use opencrabs::backend::sessions::LoadSessions;
    use opencrabs::brain::agent::ProgressEvent;

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
    }

    async fn rpc(State(mock): State<Arc<Mock>>, Json(req): Json<Value>) -> Response {
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
        let body =
            r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#;
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
}
