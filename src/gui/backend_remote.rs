//! The window's handle on another crab: [`Backend::Remote`](crate::backend_glue::Backend).
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
//!
//! What the window reads while it renders (the [`Facts`] and the running
//! tasks) is never a call: [`RemoteCrab::facts`] and
//! [`RemoteCrab::tasks_snapshot`] answer from a [`FactsCache`] on this side.
//! The cache is read again from the crab when a session's events open
//! ([`watch`]), when the stream comes back after a drop, and when an event
//! says the values moved (`RailChanged`, `TranscriptChanged`,
//! `TasksChanged`). A read that changed what the window would draw sends it
//! [`TurnEvent::Refresh`], so the new values show without waiting for a
//! frame painted for another reason.
//!
//! A file of the crab's is never fetched inside a frame either:
//! [`RemoteCrab::cached_blob`] answers the copy kept on this machine
//! ([`BlobStore`]), and [`request_blob`] fetches a missing one by `blob.get`
//! on a thread of its own, then sends the same [`TurnEvent::Refresh`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use opencrabs::backend::blob::{BlobGet, MAX_BLOB_BYTES};
use opencrabs::backend::facts::{FactsCache, LoadFacts};
use opencrabs::backend::tasks::{SessionTasks, TasksSnapshot};
use opencrabs::backend::{BackendError, Event, Facts, Op, Scope};
use opencrabs::remote::instance_client::{self, CALL_TIMEOUT};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent_bridge::{EventSink, TurnEvent};
use crate::blob_cache_store::BlobStore;

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
    /// This window's own ledger, kept across hops so what belongs to the
    /// window stays on this machine whichever crab it looks at.
    window_ledger: PathBuf,
    /// The per-frame values last read from the crab.
    cache: FactsCache,
    /// What the window asked the cache about, so a refresh knows what to read.
    wanted: Mutex<Wanted>,
    /// One read of the facts in flight at a time.
    facts_lane: Lane,
    /// One read of the running tasks in flight at a time.
    tasks_lane: Lane,
    /// Its stable id (the `host_id` it announced when paired), the name of
    /// its directory in the window's blob cache.
    id: String,
    /// The copies of its files kept on this machine.
    blobs: BlobStore,
    /// The crab paths already asked for by `blob.get`: one fetch each, so a
    /// missing file is not asked again on every frame.
    blobs_asked: Mutex<HashSet<PathBuf>>,
    /// How many `blob.get` fetches started.
    blob_fetches: AtomicUsize,
    /// Where a fetched blob tells the window to paint again: the sink of
    /// the last event reader opened ([`watch`]), numbered, held only while
    /// that reader runs so the crab never keeps a window's channel open.
    blob_sink: Mutex<Option<(usize, EventSink)>>,
    /// The number of the last event reader that lent its sink.
    blob_readers: AtomicUsize,
}

/// The keys the window read from the cache or opened events for.
#[derive(Default)]
struct Wanted {
    /// Sessions whose facts the window reads; `None` is the crab-wide entry.
    facts: HashSet<Option<Uuid>>,
    /// Sessions whose running tasks the window reads.
    tasks: HashSet<Uuid>,
}

/// One kind of cache read: at most one in flight, and a refresh asked for
/// while it runs folds into a single read right after it.
#[derive(Default)]
struct Lane(Mutex<LaneState>);

#[derive(Default)]
struct LaneState {
    running: bool,
    again: bool,
}

impl Lane {
    /// `true` when the caller starts the read; `false` when one is in flight
    /// and will read once more when it ends.
    fn start(&self) -> bool {
        let Ok(mut state) = self.0.lock() else {
            return false;
        };
        if state.running {
            state.again = true;
            return false;
        }
        state.running = true;
        state.again = false;
        true
    }

    /// The read ended: `true` when it has to run once more.
    fn finish(&self) -> bool {
        let Ok(mut state) = self.0.lock() else {
            return false;
        };
        if state.again {
            state.again = false;
            return true;
        }
        state.running = false;
        false
    }

    /// The read never started (its thread did not spawn).
    fn abandon(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.running = false;
            state.again = false;
        }
    }
}

/// Which part of the cache a refresh reads.
#[derive(Clone, Copy)]
enum Part {
    Facts,
    Tasks,
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
        let base_url = base_url.into();
        // Until the pairing record names it (`with_id`), the gateway's URL is
        // what tells this crab from another.
        let id = base_url.trim_end_matches('/').to_string();
        Self {
            name: name.into(),
            blobs: BlobStore::for_crab(&id),
            id,
            base_url,
            token,
            window_ledger,
            cache: FactsCache::default(),
            wanted: Mutex::new(Wanted::default()),
            facts_lane: Lane::default(),
            tasks_lane: Lane::default(),
            blobs_asked: Mutex::new(HashSet::new()),
            blob_fetches: AtomicUsize::new(0),
            blob_sink: Mutex::new(None),
            blob_readers: AtomicUsize::new(0),
        }
    }

    /// The same crab, known by the stable id its pairing recorded, which
    /// names its directory in the blob cache.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        if !id.is_empty() {
            self.blobs = BlobStore::for_crab(&id);
            self.id = id;
        }
        self
    }

    /// The same crab, its blob copies kept in `dir` instead of the home's
    /// cache: a test never writes the real one.
    #[cfg(test)]
    pub fn with_blob_dir(mut self, dir: PathBuf) -> Self {
        self.blobs = BlobStore::at(dir);
        self
    }

    /// The stable id this crab is known by.
    #[allow(dead_code)] // read by the tests and by whoever keys a crab's window state
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The copy on this machine of the crab's file at `path`, when one was
    /// fetched. Synchronous and local: one `stat`, never the network.
    pub(crate) fn cached_blob(&self, path: &Path) -> Option<PathBuf> {
        self.blobs.cached(path)
    }

    /// Send landed blobs' repaints to `sink`, as an opened event reader
    /// does, without opening one.
    #[cfg(test)]
    pub(crate) fn set_blob_sink(&self, sink: EventSink) {
        if let Ok(mut blob_sink) = self.blob_sink.lock() {
            *blob_sink = Some((0, sink));
        }
    }

    /// How many `blob.get` fetches this crab started.
    #[cfg(test)]
    pub(crate) fn blob_fetches(&self) -> usize {
        self.blob_fetches.load(Ordering::SeqCst)
    }

    /// The facts for `session` as last read from the crab, or `None` when
    /// they were never read, which the window takes as "still booting".
    ///
    /// Synchronous and local: the window reads it while it renders, so it
    /// never reaches the network. A miss is remembered, and the next refresh
    /// reads it.
    pub(crate) fn facts(&self, session: Option<Uuid>) -> Option<Facts> {
        if let Ok(mut wanted) = self.wanted.lock() {
            wanted.facts.insert(session);
        }
        self.cache.facts(session)
    }

    /// The running tasks of `sessions` as last read from the crab, in the
    /// shape `TasksSnapshot` answers. Synchronous and local, like
    /// [`facts`](Self::facts); the sessions asked about are read on the next
    /// refresh.
    pub(crate) fn tasks_snapshot(&self, sessions: &[Uuid]) -> Vec<SessionTasks> {
        if let Ok(mut wanted) = self.wanted.lock() {
            wanted.tasks.extend(sessions.iter().copied());
        }
        self.cache.tasks(sessions)
    }

    /// The lane that reads `part`.
    fn lane(&self, part: Part) -> &Lane {
        match part {
            Part::Facts => &self.facts_lane,
            Part::Tasks => &self.tasks_lane,
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
pub(crate) async fn call<O: Op>(crab: &RemoteCrab, op: O) -> Result<O::Reply, BackendError> {
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
pub(crate) async fn call_json(
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
pub(crate) fn decode_reply(
    crab: &str,
    name: &str,
    status: u16,
    body: &str,
) -> Result<Value, BackendError> {
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
pub(crate) fn spawn<O: Op>(
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

/// Fetch the crab's file at `path` into the blob cache, off the window's
/// thread, and answer where the copy lands.
///
/// Returns at once: the window calls it while it renders. Each path is asked
/// for once; a path already asked (in flight, kept, or refused) starts
/// nothing. A copy that lands sends the window [`TurnEvent::Refresh`] through
/// the sink of its event reader, so the next frame finds it. A fetch that
/// fails is logged and not tried again: the file shows as missing.
pub(crate) fn request_blob(crab: &Arc<RemoteCrab>, path: &Path) -> PathBuf {
    let slot = crab.blobs.slot(path);
    let first = crab
        .blobs_asked
        .lock()
        .map(|mut asked| asked.insert(path.to_path_buf()))
        .unwrap_or(false);
    if !first {
        return slot;
    }
    crab.blob_fetches.fetch_add(1, Ordering::SeqCst);
    let fetcher = crab.clone();
    let wanted = path.to_path_buf();
    let spawned = std::thread::Builder::new()
        .name("remote-blob".into())
        .spawn(move || {
            let got = instance_client::block_on(async {
                anyhow::Ok(fetch_blob(&fetcher, &wanted).await)
            })
            .unwrap_or_else(|e| Err(format!("{e:#}")));
            match got {
                Ok(()) => {
                    let sink = fetcher
                        .blob_sink
                        .lock()
                        .ok()
                        .and_then(|lent| lent.as_ref().map(|(_, sink)| sink.clone()));
                    // A closed sink is a window gone: the copy is kept anyway.
                    if let Some(sink) = sink {
                        let _ = sink.send(TurnEvent::Refresh);
                    }
                }
                Err(e) => {
                    tracing::debug!("{}: blob.get {}: {e}", fetcher.name, wanted.display());
                }
            }
        });
    if let Err(e) = spawned {
        if let Ok(mut asked) = crab.blobs_asked.lock() {
            asked.remove(path);
        }
        tracing::warn!("{}: remote-blob thread: {e}", crab.name);
    }
    slot
}

/// One `blob.get` of `path`, kept in `crab`'s blob cache.
async fn fetch_blob(crab: &RemoteCrab, path: &Path) -> Result<(), String> {
    let reply = call(
        crab,
        BlobGet {
            path: path.to_string_lossy().into_owned(),
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    // The crab serves at most MAX_BLOB_BYTES; a reply past it is refused
    // before it is decoded, so a wrong crab cannot fill the cache.
    let encoded = reply.base64.len() as u64;
    if reply.size > MAX_BLOB_BYTES || encoded / 4 * 3 > MAX_BLOB_BYTES + 3 {
        return Err(format!(
            "{} bytes, over the blob ceiling",
            reply.size.max(encoded / 4 * 3)
        ));
    }
    let bytes = BASE64
        .decode(reply.base64.as_bytes())
        .map_err(|e| e.to_string())?;
    crab.blobs
        .keep(path, &bytes)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Read `part` of `crab`'s cache again, off the window's thread.
///
/// Returns at once. A refresh asked for while one of the same part runs is
/// folded into one more read after it, so a burst of events costs two reads,
/// not one per event. A read that fails keeps what the cache had. A read
/// that changed the cache tells the window through `sink`.
fn refresh(crab: &Arc<RemoteCrab>, part: Part, sink: &EventSink) {
    if !crab.lane(part).start() {
        return;
    }
    let reader = crab.clone();
    let sink = sink.clone();
    let name = match part {
        Part::Facts => "remote-facts",
        Part::Tasks => "remote-tasks",
    };
    let spawned = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            loop {
                let moved = instance_client::block_on(async {
                    anyhow::Ok(match part {
                        Part::Facts => read_facts(&reader).await,
                        Part::Tasks => read_tasks(&reader).await,
                    })
                })
                .unwrap_or(false);
                // A closed sink is a window gone: the read still ends its lane.
                if moved {
                    let _ = sink.send(TurnEvent::Refresh);
                }
                if !reader.lane(part).finish() {
                    break;
                }
            }
        });
    if let Err(e) = spawned {
        crab.lane(part).abandon();
        tracing::warn!("{}: {name} thread: {e}", crab.name);
    }
}

/// Read the facts of every session the window wants into the cache.
/// `true` when one of them changed.
async fn read_facts(crab: &RemoteCrab) -> bool {
    let sessions: Vec<Option<Uuid>> = crab
        .wanted
        .lock()
        .map(|wanted| wanted.facts.iter().copied().collect())
        .unwrap_or_default();
    let mut moved = false;
    for session in sessions {
        match call(crab, LoadFacts { session }).await {
            Ok(facts) => moved |= crab.cache.store_facts(session, facts),
            Err(e) => tracing::debug!("{}: facts.load: {e}", crab.name),
        }
    }
    moved
}

/// Read the running tasks of every session the window wants into the cache.
/// `true` when the rows changed.
async fn read_tasks(crab: &RemoteCrab) -> bool {
    let sessions: Vec<Uuid> = crab
        .wanted
        .lock()
        .map(|wanted| wanted.tasks.iter().copied().collect())
        .unwrap_or_default();
    if sessions.is_empty() {
        return false;
    }
    let op = TasksSnapshot {
        sessions: sessions.clone(),
    };
    match call(crab, op).await {
        Ok(reply) => crab.cache.store_tasks(&sessions, reply),
        Err(e) => {
            tracing::debug!("{}: tasks.snapshot: {e}", crab.name);
            false
        }
    }
}

/// The parts of the cache `event` says moved.
fn moved_by(event: &Event) -> &'static [Part] {
    match event {
        // A turn's progress and its settle move nothing the cache holds; the
        // transcript event that follows a turn does.
        Event::Turn { .. }
        | Event::TurnDone { .. }
        | Event::TurnFailed { .. }
        | Event::TurnCancelled { .. } => &[],
        Event::RailChanged | Event::TranscriptChanged { .. } => &[Part::Facts],
        Event::TasksChanged => &[Part::Tasks],
        // A question to the user, not a value the cache holds.
        Event::ApprovalRequested { .. } => &[],
    }
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
///
/// Opening it is opening the session: the cache reads the session's facts,
/// the crab-wide ones and its running tasks now, and again whenever the
/// stream comes back after a drop, since events may have been missed.
pub(crate) fn watch(
    crab: Arc<RemoteCrab>,
    session: Uuid,
    sink: EventSink,
) -> Result<Watch, String> {
    if let Ok(mut wanted) = crab.wanted.lock() {
        wanted.facts.insert(None);
        wanted.facts.insert(Some(session));
        wanted.tasks.insert(session);
    }
    let reader = crab.blob_readers.fetch_add(1, Ordering::SeqCst) + 1;
    if let Ok(mut blob_sink) = crab.blob_sink.lock() {
        *blob_sink = Some((reader, sink.clone()));
    }
    refresh(&crab, Part::Facts, &sink);
    refresh(&crab, Part::Tasks, &sink);
    let stop = CancellationToken::new();
    let reader_stop = stop.clone();
    let lender = crab.clone();
    std::thread::Builder::new()
        .name("remote-events".into())
        .spawn(move || {
            let _ = instance_client::block_on(async {
                read_events(&crab, session, &sink, &reader_stop).await;
                anyhow::Ok(())
            });
            // The reader is gone: its sink goes with it, unless a newer
            // reader already lent its own.
            take_blob_sink(&crab, reader);
        })
        .map_err(|e| {
            take_blob_sink(&lender, reader);
            format!("event reader thread: {e}")
        })?;
    Ok(Watch { stop })
}

/// Give back the sink event reader `reader` lent for landed blobs.
fn take_blob_sink(crab: &RemoteCrab, reader: usize) {
    if let Ok(mut blob_sink) = crab.blob_sink.lock()
        && blob_sink.as_ref().is_some_and(|(lent, _)| *lent == reader)
    {
        *blob_sink = None;
    }
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
async fn read_events(
    crab: &Arc<RemoteCrab>,
    session: Uuid,
    sink: &EventSink,
    stop: &CancellationToken,
) {
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
async fn stream_once(
    crab: &Arc<RemoteCrab>,
    session: Uuid,
    sink: &EventSink,
    down: &mut bool,
) -> Ended {
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
        refresh(crab, Part::Facts, sink);
        refresh(crab, Part::Tasks, sink);
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
                    for part in moved_by(&event) {
                        refresh(crab, *part, sink);
                    }
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
pub(crate) fn to_turn_event(event: Event) -> Option<TurnEvent> {
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
        // Reload hints: the reader refreshes the remote cache itself
        // (`moved_by`) and sends `TurnEvent::Refresh` when the read changed
        // it, so the hint alone repaints nothing.
        Event::RailChanged | Event::TranscriptChanged { .. } | Event::TasksChanged => None,
        // The window has no approval card for a remote turn yet: the request
        // is not shown, so the crab's own approval timeout denies the tool,
        // which is what a turn with no callback did before rb/s3b.
        Event::ApprovalRequested { .. } => None,
    }
}

/// The SSE framing of `GET /gui/events`: `data:` lines up to a blank line
/// make one event, one [`Event`] as JSON. Comment lines (the keep-alive) and
/// the other fields are skipped. Chunks split anywhere, a line included.
#[derive(Default)]
pub(crate) struct Frames {
    /// The bytes of the line not yet ended.
    line: Vec<u8>,
    /// The `data:` lines of the event not yet ended.
    data: Option<String>,
}

impl Frames {
    /// Feed one chunk; the events it completed, in order.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
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
