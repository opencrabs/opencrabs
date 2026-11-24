//! `GET /gui/events?session=<id>`: the crab's events for one session, as SSE.
//!
//! Operations answer through `gui/call`; events arrive here. A surface that
//! has a session open subscribes to it and sees every turn in that session,
//! whoever started it: the local window, a channel, or another remote
//! surface. That is the fan-out: one [`broadcast`] channel per session, kept
//! in a [`Hub`], and every subscriber of the session holds a receiver on it.
//!
//! A producer hands an [`Event`] to [`publish`]. An event that names a
//! session goes to that session's subscribers only, so a subscriber of A
//! never sees B's turn. An event that names none (`RailChanged`,
//! `TasksChanged`) is news for every surface and goes to all of them.
//!
//! A subscription cleans up after itself: when the stream drops (the client
//! went away), the last receiver of a session removes the session's channel.
//! There is no reaper.
//!
//! The route is a GET rather than a JSON-RPC method because SSE is a GET in
//! every client that speaks it (`EventSource` reconnects by itself), and
//! because the scope gate then lives here, next to the stream, instead of in
//! the A2A method table. It sits behind `require_bearer` like every other
//! protected route and needs `a2a:read`: watching a session changes nothing.
//!
//! Events carry what a surface shows, a turn's text included, and never a
//! key or a token: [`Event`] has no field that could hold one.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::Extension;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response, Sse, sse};
use futures::stream;
use serde::Deserialize;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::a2a::server::{A2aState, GrantedScopes};
use crate::a2a::types::{JsonRpcError, JsonRpcResponse, error_codes};
use crate::backend::{BackendError, Event, Scope};
use crate::remote::scopes::{SCOPE_READ, allows};

/// How many events a slow subscriber may fall behind before it skips ahead.
/// A turn streams a chunk per token, so this is sized for a burst of those.
const CAPACITY: usize = 1024;

/// The session channels: one sender per session somebody is watching.
#[derive(Default)]
pub struct Hub {
    channels: Mutex<HashMap<Uuid, broadcast::Sender<Event>>>,
}

impl Hub {
    /// Watch `session`. The first subscriber opens the session's channel,
    /// later ones share it.
    pub fn subscribe(self: &Arc<Self>, session: Uuid) -> Subscription {
        let rx = lock(&self.channels)
            .entry(session)
            .or_insert_with(|| broadcast::channel(CAPACITY).0)
            .subscribe();
        Subscription {
            hub: Arc::clone(self),
            session,
            rx,
        }
    }

    /// Hand `event` to everyone watching. Returns how many subscribers it
    /// reached: zero when nobody has the session open, which is not an error.
    pub fn publish(&self, event: &Event) -> usize {
        let channels = lock(&self.channels);
        match session_of(event) {
            Some(session) => channels
                .get(&session)
                .map_or(0, |tx| tx.send(event.clone()).unwrap_or(0)),
            None => channels
                .values()
                .map(|tx| tx.send(event.clone()).unwrap_or(0))
                .sum(),
        }
    }

    /// Whether anybody is watching `session` right now.
    pub fn is_watched(&self, session: Uuid) -> bool {
        lock(&self.channels).contains_key(&session)
    }
}

/// One subscriber's hold on a session. Dropping the last one for a session
/// closes the session's channel.
pub struct Subscription {
    hub: Arc<Hub>,
    session: Uuid,
    rx: broadcast::Receiver<Event>,
}

impl Subscription {
    /// The next event for this session, or `None` once the channel is gone.
    ///
    /// A subscriber that fell more than [`CAPACITY`] events behind lost some.
    /// It gets `TranscriptChanged` in their place, the event that already
    /// tells a surface to reload the session rather than patch it.
    pub async fn next(&mut self) -> Option<Event> {
        match self.rx.recv().await {
            Ok(event) => Some(event),
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(
                    "gui/events: subscriber of {} missed {missed} events",
                    self.session
                );
                Some(Event::TranscriptChanged {
                    session_id: self.session,
                })
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut channels = lock(&self.hub.channels);
        // Our own receiver is still alive here, so one left means only us.
        // While it lives nobody else can remove the entry, so the sender in
        // the map is the one we subscribed to.
        let last = channels
            .get(&self.session)
            .is_some_and(|tx| tx.receiver_count() <= 1);
        if last {
            channels.remove(&self.session);
        }
    }
}

/// The session an event belongs to, or `None` for news every surface gets.
/// No wildcard: a new event kind has to say where it goes.
fn session_of(event: &Event) -> Option<Uuid> {
    match event {
        Event::Turn { session_id, .. }
        | Event::TurnDone { session_id }
        | Event::TurnFailed { session_id, .. }
        | Event::TurnCancelled { session_id }
        | Event::TranscriptChanged { session_id } => Some(*session_id),
        Event::RailChanged | Event::TasksChanged => None,
    }
}

/// The channel map. A subscriber that panicked while holding the lock left
/// the map itself intact, so a poisoned lock is read through.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// This process's hub: every surface and every producer meet here.
static HUB: LazyLock<Arc<Hub>> = LazyLock::new(|| Arc::new(Hub::default()));

/// This process's hub.
pub fn hub() -> &'static Arc<Hub> {
    &HUB
}

/// Hand `event` to every surface watching on this process. See
/// [`Hub::publish`].
pub fn publish(event: &Event) -> usize {
    hub().publish(event)
}

/// The query of `GET /gui/events`.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// The session to watch.
    pub session: Uuid,
}

/// How often an open stream asks again whether its credential still holds.
const RECHECK: Duration = Duration::from_secs(30);

/// `GET /gui/events?session=<id>`: the session's events as SSE, one
/// [`Event`] as JSON per `data:` line.
///
/// The stream can stay open for hours, longer than the pairing behind it.
/// Every [`RECHECK`] it presents the same bearer again, the way
/// `require_bearer` did at connect, and ends once that no longer carries
/// `a2a:read`: a revoked or expired token stops seeing events.
pub async fn handle_events(
    State(state): State<A2aState>,
    Extension(granted): Extension<GrantedScopes>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Response {
    crate::remote::tunnel::note_traffic(&state.remote_state_dir);
    if !allows(&granted.0, SCOPE_READ) {
        return forbidden();
    }
    let token = super::bearer_from_headers(&headers).map(str::to_string);
    let still_allowed = move || {
        super::credential_scopes(&state, token.as_deref())
            .is_some_and(|scopes| allows(&scopes, SCOPE_READ))
    };
    stream_events(hub().subscribe(query.session), RECHECK, still_allowed)
}

/// The SSE body for one subscription. The subscription lives in the stream,
/// so it drops, and cleans up, when the client goes away or when
/// `still_allowed`, asked every `recheck`, says no.
fn stream_events(
    subscription: Subscription,
    recheck: Duration,
    still_allowed: impl Fn() -> bool + Send + 'static,
) -> Response {
    let timer = tokio::time::interval_at(tokio::time::Instant::now() + recheck, recheck);
    let events = stream::unfold(
        (subscription, timer, still_allowed),
        |(mut subscription, mut timer, still_allowed)| async move {
            loop {
                tokio::select! {
                    event = subscription.next() => {
                        let frame = sse::Event::default().json_data(&event?);
                        return Some((frame, (subscription, timer, still_allowed)));
                    }
                    _ = timer.tick() => {
                        if !still_allowed() {
                            return None;
                        }
                    }
                }
            }
        },
    );
    Sse::new(events)
        .keep_alive(sse::KeepAlive::default())
        .into_response()
}

/// The 403 a caller without `a2a:read` gets, shaped like a refused
/// `gui/call`: the typed backend error rides in `data`.
fn forbidden() -> Response {
    let err = BackendError::Forbidden { scope: Scope::Read };
    let body = JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        result: None,
        error: Some(JsonRpcError {
            code: error_codes::INSUFFICIENT_SCOPE,
            message: err.to_string(),
            data: serde_json::to_value(&err).ok(),
        }),
        id: serde_json::Value::Null,
    };
    (StatusCode::FORBIDDEN, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::handler;
    use crate::a2a::server::build_router;
    use crate::brain::agent::ProgressEvent;
    use axum::body::Body;
    use axum::http::Request;
    use futures::StreamExt;
    use tower::ServiceExt;

    fn turn(session_id: Uuid, text: &str) -> Event {
        Event::Turn {
            session_id,
            progress: ProgressEvent::StreamingChunk { text: text.into() },
        }
    }

    /// The events already waiting for `sub`, without waiting for more.
    fn drain(sub: &mut Subscription) -> Vec<Event> {
        std::iter::from_fn(|| sub.rx.try_recv().ok()).collect()
    }

    #[test]
    fn a_session_event_reaches_only_that_sessions_subscribers() {
        let hub = Arc::new(Hub::default());
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut on_a = hub.subscribe(a);
        let mut on_b = hub.subscribe(b);
        let event = turn(a, "for a");
        assert_eq!(hub.publish(&event), 1);
        assert_eq!(drain(&mut on_a), vec![event]);
        assert_eq!(drain(&mut on_b), Vec::<Event>::new(), "b saw a's turn");
    }

    #[test]
    fn every_subscriber_of_a_session_gets_the_same_event() {
        let hub = Arc::new(Hub::default());
        let session = Uuid::new_v4();
        let mut first = hub.subscribe(session);
        let mut second = hub.subscribe(session);
        let event = Event::TurnFailed {
            session_id: session,
            message: "provider: timeout".into(),
        };
        assert_eq!(hub.publish(&event), 2);
        assert_eq!(drain(&mut first), vec![event.clone()]);
        assert_eq!(drain(&mut second), vec![event]);
    }

    #[test]
    fn news_without_a_session_reaches_every_subscriber() {
        let hub = Arc::new(Hub::default());
        let mut on_a = hub.subscribe(Uuid::new_v4());
        let mut on_b = hub.subscribe(Uuid::new_v4());
        for event in [Event::RailChanged, Event::TasksChanged] {
            assert_eq!(hub.publish(&event), 2);
            assert_eq!(drain(&mut on_a), vec![event.clone()]);
            assert_eq!(drain(&mut on_b), vec![event]);
        }
    }

    #[test]
    fn an_unwatched_session_reaches_nobody() {
        let hub = Arc::new(Hub::default());
        let session = Uuid::new_v4();
        assert_eq!(hub.publish(&turn(session, "nobody")), 0);
        assert!(!hub.is_watched(session), "publishing opened a channel");
    }

    #[test]
    fn the_last_subscriber_to_leave_closes_the_session() {
        let hub = Arc::new(Hub::default());
        let session = Uuid::new_v4();
        let first = hub.subscribe(session);
        let second = hub.subscribe(session);
        drop(first);
        assert!(hub.is_watched(session), "one subscriber is still there");
        assert_eq!(hub.publish(&turn(session, "still")), 1);
        drop(second);
        assert!(
            !hub.is_watched(session),
            "the channel outlived its subscribers"
        );
        let mut again = hub.subscribe(session);
        let event = turn(session, "reopened");
        assert_eq!(hub.publish(&event), 1);
        assert_eq!(drain(&mut again), vec![event]);
    }

    #[tokio::test]
    async fn a_subscriber_that_fell_behind_is_told_to_reload() {
        let hub = Arc::new(Hub::default());
        let session = Uuid::new_v4();
        let mut sub = hub.subscribe(session);
        for i in 0..=CAPACITY {
            hub.publish(&turn(session, &i.to_string()));
        }
        assert_eq!(
            sub.next().await,
            Some(Event::TranscriptChanged {
                session_id: session
            })
        );
        assert_eq!(sub.next().await, Some(turn(session, "1")));
    }

    async fn router_state(dir: &std::path::Path, api_key: Option<&str>) -> A2aState {
        use crate::a2a::test_helpers::helpers;
        A2aState {
            task_store: handler::new_task_store(),
            cancel_store: handler::new_cancel_store(),
            host: "127.0.0.1".to_string(),
            port: 18790,
            agent_service: helpers::placeholder_agent_service().await,
            service_context: helpers::placeholder_service_context().await,
            api_key: api_key.map(str::to_string),
            pairing: crate::remote::pairing::PairingStore::new(dir.join("pairing")),
            pair_hits: crate::a2a::pairing_http::new_pair_hits(),
            remote_state_dir: dir.join("remote"),
        }
    }

    fn events_request(session: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(format!("/gui/events?session={session}"))
            .body(Body::empty())
            .expect("request")
    }

    /// Read `n` events off an SSE body, skipping keep-alive comments.
    async fn read_events(body: Body, n: usize) -> Vec<Event> {
        let mut frames = body.into_data_stream();
        let mut text = String::new();
        let mut events = Vec::new();
        while events.len() < n {
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), frames.next())
                .await
                .expect("an event within 5s")
                .expect("the stream stays open")
                .expect("a body chunk");
            text.push_str(std::str::from_utf8(&chunk).expect("SSE is UTF-8"));
            while let Some(end) = text.find("\n\n") {
                let block: String = text.drain(..end + 2).collect();
                for line in block.lines() {
                    if let Some(data) = line.strip_prefix("data:") {
                        let event = serde_json::from_str(data.trim_start()).expect("an Event");
                        events.push(event);
                    }
                }
            }
        }
        events
    }

    /// Through the real router: a turn's events come off the SSE stream as
    /// the same values, in the same order, as they were published.
    #[tokio::test]
    async fn a_turn_crosses_the_stream_unchanged_and_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = build_router(router_state(dir.path(), None).await, &[]);
        let session = Uuid::new_v4();
        let resp = app
            .oneshot(events_request(&session.to_string()))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(hub().is_watched(session), "the GET subscribed");

        let other = Uuid::new_v4();
        let turn_events = vec![
            Event::Turn {
                session_id: session,
                progress: ProgressEvent::Thinking,
            },
            turn(session, "hel"),
            turn(session, "lo"),
            Event::TurnFailed {
                session_id: session,
                message: "provider: no provider configured".into(),
            },
        ];
        for event in &turn_events {
            publish(&turn(other, "not yours"));
            assert_eq!(publish(event), 1);
        }
        assert_eq!(
            read_events(resp.into_body(), turn_events.len()).await,
            turn_events
        );
        assert!(
            !hub().is_watched(session),
            "dropping the stream closed the session"
        );
    }

    #[tokio::test]
    async fn a_caller_without_read_is_refused_and_subscribes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = router_state(dir.path(), None).await;
        let session = Uuid::new_v4();
        let resp = handle_events(
            State(state),
            Extension(GrantedScopes(vec![
                crate::remote::scopes::SCOPE_SEND.to_string(),
            ])),
            HeaderMap::new(),
            Query(EventsQuery { session }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(!hub().is_watched(session));
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: JsonRpcResponse = serde_json::from_slice(&bytes).expect("JSON-RPC");
        let error = body.error.expect("an error");
        assert_eq!(error.code, error_codes::INSUFFICIENT_SCOPE);
        let data: BackendError = serde_json::from_value(error.data.expect("data")).expect("typed");
        assert_eq!(data, BackendError::Forbidden { scope: Scope::Read });
    }

    /// A stream whose credential stops holding ends, and its subscription
    /// goes with it.
    #[tokio::test]
    async fn the_stream_ends_once_its_credential_no_longer_holds() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let session = Uuid::new_v4();
        let allowed = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&allowed);
        let resp = stream_events(
            hub().subscribe(session),
            std::time::Duration::from_millis(10),
            move || flag.load(Ordering::SeqCst),
        );
        let mut frames = resp.into_body().into_data_stream();
        let event = turn(session, "while allowed");
        assert_eq!(publish(&event), 1);
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), frames.next())
            .await
            .expect("the event within 5s")
            .expect("the stream is open")
            .expect("a body chunk");
        assert!(
            std::str::from_utf8(&chunk)
                .expect("UTF-8")
                .contains("while allowed")
        );
        allowed.store(false, Ordering::SeqCst);
        let end = tokio::time::timeout(std::time::Duration::from_secs(5), frames.next())
            .await
            .expect("the stream ends within 5s");
        assert!(end.is_none(), "the stream outlived its credential");
        drop(frames);
        assert!(!hub().is_watched(session));
    }

    #[tokio::test]
    async fn the_stream_is_behind_the_bearer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = build_router(router_state(dir.path(), Some("secret")).await, &[]);
        let session = Uuid::new_v4();
        let resp = app
            .oneshot(events_request(&session.to_string()))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(!hub().is_watched(session));
    }

    #[tokio::test]
    async fn a_session_that_is_no_uuid_is_a_bad_request() {
        let dir = tempfile::tempdir().expect("tempdir");
        let app = build_router(router_state(dir.path(), None).await, &[]);
        let resp = app
            .oneshot(events_request("not-a-session"))
            .await
            .expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
