//! The window's blob cache (`blob_cache.rs`, `blob_cache_store.rs`).
//!
//! `localize` runs inside a frame, so its promises are about time and
//! network: a kept copy answers with no fetch, a missing one is asked for
//! once and off the frame, and the local crab never touches the cache.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::response::Json;
use axum::routing::post;
use opencrabs::backend::Ctx;
use serde_json::Value;

use crate::agent_bridge::TurnEvent;
use crate::backend_glue::Backend;
use crate::backend_remote::RemoteCrab;
use crate::blob_cache_store::BlobStore;

/// A remote crab at `url` whose blob copies go to `cache`.
fn crab_at(url: String, cache: &Path) -> Arc<RemoteCrab> {
    Arc::new(
        RemoteCrab::new("cached", url, None, PathBuf::from("unused.db"))
            .with_blob_dir(cache.to_path_buf()),
    )
}

/// `gui/call` answered by running the operation on the crab's `Ctx`, every
/// scope granted: the gateway's happy path, nothing else.
async fn rpc(State(ctx): State<Arc<Ctx>>, Json(req): Json<Value>) -> Json<Value> {
    let name = req["params"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let params = req["params"]["params"].clone();
    let result = opencrabs::backend::dispatch(&ctx, &name, params)
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    Json(serde_json::json!({ "jsonrpc": "2.0", "result": result, "id": req["id"].clone() }))
}

/// A crab on a temp home, served over `gui/call`. The runtime must outlive
/// the test's calls.
struct Served {
    _runtime: tokio::runtime::Runtime,
    home: tempfile::TempDir,
    url: String,
}

fn serve() -> Served {
    let home = tempfile::tempdir().expect("crab home");
    let ctx = Ctx::new(home.path().join("opencrabs.db")).with_home(home.path().to_path_buf());
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let app = Router::new()
        .route("/a2a/v1", post(rpc))
        .with_state(Arc::new(ctx));
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    runtime.spawn(async move { axum::serve(listener, app).await });
    Served {
        _runtime: runtime,
        home,
        url: format!("http://{addr}/"),
    }
}

#[test]
fn a_cached_blob_is_answered_without_the_network() {
    let cache = tempfile::tempdir().expect("cache");
    // Nothing listens on the discard port: a fetch could only fail, and
    // none may even start.
    let crab = crab_at("http://127.0.0.1:9".into(), cache.path());
    let on_crab = Path::new("/home/crab/.opencrabs/gui-icons/crab.png");
    let kept = BlobStore::at(cache.path().to_path_buf())
        .keep(on_crab, b"png")
        .expect("keep");
    let backend = Backend::Remote(crab.clone());

    let here = backend.localize(on_crab);

    assert_eq!(here, kept);
    assert_eq!(std::fs::read(&here).expect("the copy"), b"png");
    assert_eq!(crab.blob_fetches(), 0, "a hit asks the crab nothing");
}

#[test]
fn a_miss_does_not_block_the_frame_and_is_asked_for_once() {
    let cache = tempfile::tempdir().expect("cache");
    // A gateway that takes the connection and never answers: a fetch made
    // inside the frame would hang the frame here.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/", silent.local_addr().expect("addr"));
    let crab = crab_at(url, cache.path());
    let backend = Backend::Remote(crab.clone());
    let on_crab = Path::new("/home/crab/.opencrabs/tmp/gui-uploads/clip.mp4");

    let started = Instant::now();
    let here = backend.localize(on_crab);
    let again = backend.localize(on_crab);
    let took = started.elapsed();

    assert!(
        took < Duration::from_secs(1),
        "two frames' localize took {took:?}"
    );
    assert_eq!(here, again, "the answer is where the copy will land");
    assert!(here.starts_with(cache.path()), "{}", here.display());
    assert_eq!(here.extension().and_then(|e| e.to_str()), Some("mp4"));
    assert!(!here.exists(), "nothing landed: the file reads as missing");
    assert_eq!(
        crab.blob_fetches(),
        1,
        "one fetch for one path, however many frames ask"
    );
    drop(silent);
}

#[test]
fn a_fetched_blob_lands_in_the_cache_and_repaints_the_window() {
    let served = serve();
    let icons = served.home.path().join("gui-icons");
    std::fs::create_dir_all(&icons).expect("icons");
    let on_crab = icons.join("crab.png");
    std::fs::write(&on_crab, b"png").expect("icon");
    let cache = tempfile::tempdir().expect("cache");
    let crab = crab_at(served.url.clone(), cache.path());
    let (sink, events) = std::sync::mpsc::channel();
    crab.set_blob_sink(sink);
    let backend = Backend::Remote(crab.clone());

    let slot = backend.localize(&on_crab);
    let event = events
        .recv_timeout(Duration::from_secs(10))
        .expect("a landed copy repaints the window");

    assert!(matches!(event, TurnEvent::Refresh));
    assert_eq!(backend.cached_blob(&on_crab), Some(slot.clone()));
    assert_eq!(backend.localize(&on_crab), slot);
    assert_eq!(std::fs::read(&slot).expect("the copy"), b"png");
    assert_eq!(crab.blob_fetches(), 1);
}

#[test]
fn the_local_crab_never_writes_the_cache() {
    let dir = tempfile::tempdir().expect("dir");
    let backend = Backend::local(dir.path().join("opencrabs.db"));
    let file = dir.path().join("plan.md");

    assert_eq!(
        backend.localize(&file),
        file,
        "the local crab's path is its own"
    );
    assert_eq!(backend.cached_blob(&file), Some(file.clone()));
    assert_eq!(backend.request_blob(&file), file);
    let written: Vec<_> = std::fs::read_dir(dir.path())
        .expect("dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert!(written.is_empty(), "the local crab wrote {written:?}");
}

#[test]
fn one_crab_path_has_one_slot_and_keeps_its_extension() {
    let store = BlobStore::at(PathBuf::from("/cache/crab"));
    let png = Path::new("/home/crab/.opencrabs/gui-icons/a.png");
    assert_eq!(store.slot(png), store.slot(png));
    assert_ne!(store.slot(png), store.slot(Path::new("/home/crab/b.png")));
    assert_eq!(
        store.slot(png).extension().and_then(|e| e.to_str()),
        Some("png")
    );
    assert!(store.slot(png).starts_with("/cache/crab"));
    let odd = store.slot(Path::new("/home/crab/x.p/n g"));
    assert_eq!(odd.parent(), Some(Path::new("/cache/crab")));
}
