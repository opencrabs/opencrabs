//! The window's blob cache (`blob_cache.rs`, `blob_cache_store.rs`) and the
//! icon a remote crab keeps from an upload (`Backend::keep_icon_image`).
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
use crate::backend_glue::{Backend, Kept};
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

#[test]
fn a_local_icon_is_written_here_at_once() {
    let backend = Backend::local(PathBuf::from("unused.db"));
    let kept = backend.keep_icon_image("a.png", b"png", |filename, bytes| {
        assert_eq!(bytes, b"png");
        Ok(format!("img:{filename}"))
    });
    match kept {
        Kept::Now(stored) => assert_eq!(stored, Ok("img:a.png".to_string())),
        Kept::Later(_) => panic!("the local crab's icon store is on this disk"),
    }
}

#[test]
fn a_remote_icon_goes_up_as_an_upload_and_comes_back_stored() {
    let served = serve();
    let cache = tempfile::tempdir().expect("cache");
    let backend = Backend::Remote(crab_at(served.url.clone(), cache.path()));

    let kept = backend.keep_icon_image(
        "pick.png",
        b"png",
        |_: &str, _: &[u8]| -> Result<String, String> {
            panic!("a remote crab's icon is never written on this disk")
        },
    );
    let Kept::Later(reply) = kept else {
        panic!("a remote crab keeps the icon over the wire");
    };
    let stored = futures::executor::block_on(reply).expect("kept");

    let name = stored.strip_prefix("img:").expect("an img: value");
    assert!(
        !name.is_empty() && !name.contains('/') && !name.contains(".."),
        "{stored}"
    );
    let icon = served.home.path().join("gui-icons").join(name);
    assert_eq!(std::fs::read(&icon).expect("the icon on the crab"), b"png");
}

/// A remote crab at the discard port with `names` already kept in `cache`,
/// under `/home/crab/`: answers from the cache only, never the network.
fn remote_with_kept(cache: &Path, names: &[&str]) -> (Backend, Arc<RemoteCrab>) {
    let crab = crab_at("http://127.0.0.1:9".into(), cache);
    let store = BlobStore::at(cache.to_path_buf());
    for name in names {
        store
            .keep(&Path::new("/home/crab").join(name), b"#!/bin/sh\n")
            .expect("keep");
    }
    (Backend::Remote(crab.clone()), crab)
}

#[test]
fn a_remote_crab_s_launcher_or_script_never_reaches_the_opener() {
    let cache = tempfile::tempdir().expect("cache");
    let names = [
        "x.desktop",
        "x.sh",
        "x.svg",
        "x.command",
        "x.exe",
        "x.bat",
        "x.jar",
        "x.ps1",
        "x",
    ];
    let (backend, crab) = remote_with_kept(cache.path(), &names);

    for name in names {
        let on_crab = Path::new("/home/crab").join(name);
        let kept = backend.cached_blob(&on_crab).expect("the copy is kept");
        assert_eq!(
            kept.extension().and_then(|e| e.to_str()),
            Some("bin"),
            "{name} is kept as .bin"
        );
        assert_eq!(
            backend.openable(&on_crab),
            Err(crate::blob_cache_store::NOT_OPENED_HERE),
            "{name} must not reach the system opener"
        );
        assert!(
            backend.revealable(&on_crab).starts_with(cache.path()),
            "{name}: reveal shows the cache, not a place the crab named"
        );
    }
    assert_eq!(crab.blob_fetches(), 0, "every answer came from the cache");
}

#[test]
fn a_remote_crab_s_picture_opens_from_its_copy() {
    let cache = tempfile::tempdir().expect("cache");
    let (backend, _crab) = remote_with_kept(cache.path(), &["x.png", "y.PDF"]);

    let png = backend
        .openable(Path::new("/home/crab/x.png"))
        .expect("a png opens");
    assert!(png.starts_with(cache.path()), "{}", png.display());
    assert_eq!(png.extension().and_then(|e| e.to_str()), Some("png"));
    let pdf = backend
        .openable(Path::new("/home/crab/y.PDF"))
        .expect("a pdf opens");
    assert_eq!(pdf.extension().and_then(|e| e.to_str()), Some("pdf"));
}

#[test]
fn a_remote_file_not_fetched_yet_is_not_opened() {
    let cache = tempfile::tempdir().expect("cache");
    let (backend, crab) = remote_with_kept(cache.path(), &[]);

    assert_eq!(
        backend.openable(Path::new("/home/crab/x.png")),
        Err(crate::blob_cache_store::NOT_OPENED_HERE),
        "never the crab's own path on this disk"
    );
    assert_eq!(crab.blob_fetches(), 0, "asking to open fetches nothing");
}

#[test]
fn a_remote_icon_outside_the_allowlist_is_neither_fetched_nor_read() {
    let cache = tempfile::tempdir().expect("cache");
    let (backend, crab) = remote_with_kept(cache.path(), &["a.png"]);

    assert_eq!(
        backend.localize_allowed(Path::new("/home/crab/a.png")),
        Some(BlobStore::at(cache.path().to_path_buf()).slot(Path::new("/home/crab/a.png")))
    );
    for name in ["a.svg", "a.sh", "a.desktop"] {
        assert_eq!(
            backend.localize_allowed(&Path::new("/home/crab").join(name)),
            None,
            "{name}"
        );
    }
    assert_eq!(crab.blob_fetches(), 0);
}

#[test]
fn the_local_crab_opens_and_reveals_its_own_path_as_before() {
    let dir = tempfile::tempdir().expect("dir");
    let backend = Backend::local(dir.path().join("opencrabs.db"));
    for name in ["x.sh", "x.desktop", "x.svg", "x.png"] {
        let file = dir.path().join(name);
        assert_eq!(backend.openable(&file), Ok(file.clone()), "{name}");
        assert_eq!(backend.revealable(&file), file, "{name}");
        assert_eq!(
            backend.localize_allowed(&file),
            Some(file.clone()),
            "{name}"
        );
    }
}

#[test]
fn a_slot_keeps_only_an_allowed_extension() {
    let store = BlobStore::at(PathBuf::from("/cache/crab"));
    let ext = |name: &str| {
        store
            .slot(&Path::new("/home/crab").join(name))
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_string)
    };
    assert_eq!(ext("a.PNG").as_deref(), Some("png"));
    assert_eq!(ext("a.mp4").as_deref(), Some("mp4"));
    assert_eq!(ext("a.txt").as_deref(), Some("txt"));
    for name in ["a.desktop", "a.sh", "a.svg", "a.lnk", "a.app", "a"] {
        assert_eq!(ext(name).as_deref(), Some("bin"), "{name}");
    }
}

#[test]
fn the_preview_hands_the_opener_only_what_openable_answers() {
    const SESSIONS: &str = include_str!("../dashboard_sessions.rs");
    let (_, body) = SESSIONS
        .split_once("fn preview_body(")
        .expect("preview_body");
    let body = body.split("\n    pub(crate) fn ").next().unwrap_or(body);
    assert!(body.contains("self.backend.openable(&open.path)"));
    assert!(body.contains("self.backend.revealable(&open.path)"));
    assert_eq!(
        body.matches("preview::open_command(").count(),
        1,
        "one opener call, fed by openable"
    );
    assert!(!body.contains("open_command(&open.path)"));
    assert!(!body.contains("reveal_command(&open.path)"));
}

/// The directory `BlobStore::for_crab(id)` keeps its copies in. Pure: a
/// slot is built, nothing is written.
fn crab_dir(id: &str) -> PathBuf {
    BlobStore::for_crab(id)
        .slot(Path::new("/home/crab/a.png"))
        .parent()
        .expect("a slot has a directory")
        .to_path_buf()
}

#[test]
fn a_crab_id_of_64_hex_never_shares_another_crab_s_cache_dir() {
    use sha2::{Digest, Sha256};

    let long = "x".repeat(80);
    for id in ["home crab", "lab/box", "", "plain-id_1", long.as_str()] {
        let hex = |text: String| format!("{:x}", Sha256::digest(text.as_bytes()));
        // The directory names an id could be hashed into, each tried as a
        // second crab's own (64 hex digits) id.
        for twin in [hex(id.to_string()), hex(format!("crab:{id}"))] {
            assert_eq!(twin.len(), 64);
            assert_ne!(
                crab_dir(id),
                crab_dir(&twin),
                "{id:?} and the 64-hex id {twin} share a directory"
            );
        }
    }
    assert_ne!(crab_dir("a"), crab_dir("b"));
    assert_eq!(crab_dir("a"), crab_dir("a"), "one id, one directory");
    for id in ["plain-id_1", "..", "../../etc", "a/b"] {
        let name = crab_dir(id);
        let name = name.file_name().and_then(|n| n.to_str()).expect("a name");
        assert_ne!(name, id, "an id is never the directory's name");
        assert_eq!(name.len(), 64, "{name}");
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()), "{name}");
    }
}

#[cfg(unix)]
#[test]
fn a_slot_that_resolves_out_of_the_cache_is_neither_opened_nor_answered() {
    let cache = tempfile::tempdir().expect("cache");
    let outside = tempfile::tempdir().expect("outside");
    let elsewhere = outside.path().join("secret.png");
    std::fs::write(&elsewhere, b"png").expect("a file outside the cache");
    let on_crab = Path::new("/home/crab/x.png");
    let slot = BlobStore::at(cache.path().to_path_buf()).slot(on_crab);
    std::os::unix::fs::symlink(&elsewhere, &slot).expect("symlink in the cache");
    let (backend, crab) = remote_with_kept(cache.path(), &[]);

    assert_eq!(
        backend.cached_blob(on_crab),
        None,
        "not a copy in the cache"
    );
    assert_eq!(
        backend.openable(on_crab),
        Err(crate::blob_cache_store::NOT_OPENED_HERE),
        "a file outside the crab's cache never reaches the opener"
    );
    assert_eq!(crab.blob_fetches(), 0, "asking to open fetches nothing");
}

#[cfg(unix)]
#[test]
fn a_cache_reached_through_a_symlink_still_opens_its_own_copy() {
    let real = tempfile::tempdir().expect("cache");
    let links = tempfile::tempdir().expect("links");
    let cache = links.path().join("cache");
    std::os::unix::fs::symlink(real.path(), &cache).expect("symlinked cache");
    let (backend, _crab) = remote_with_kept(&cache, &["x.png"]);

    let png = backend
        .openable(Path::new("/home/crab/x.png"))
        .expect("the copy is in the cache, however it is reached");
    assert!(png.starts_with(&cache), "{}", png.display());
}

#[test]
fn a_remote_crab_s_link_reaches_the_opener_only_for_web_and_mail() {
    let cache = tempfile::tempdir().expect("cache");
    let (backend, crab) = remote_with_kept(cache.path(), &[]);

    for url in [
        "https://example.com/a?b=c",
        "http://example.com",
        "HTTPS://Example.com",
        "mailto:crab@example.com",
        "mailto:crab@example.com?subject=hi&body=a%20b&cc=x@example.com",
    ] {
        assert_eq!(backend.openable_url(url), Ok(url), "{url}");
    }
    for url in [
        "file:///etc/passwd",
        "FILE:///home/me/x.desktop",
        "vscode://file/home/me/x",
        "slack://open",
        "smb://host/share",
        "javascript:alert(1)",
        "/home/me/x.sh",
        "~/x.desktop",
        "x.desktop",
        "https://",
        "mailto:",
        "",
        " https://example.com",
        "https://example.com/a b",
        "https://example.com/\n",
        "mailto:a@example.com?attach=/etc/passwd",
        "mailto:a@example.com?subject=x&ATTACHMENT=~/.ssh/id_rsa",
        "mailto:a@example.com?att%61ch=/etc/passwd",
        "mailto:a@example.com?subject=x&attach",
    ] {
        assert_eq!(
            backend.openable_url(url),
            Err(crate::preview::REMOTE_URL_NOT_OPENED),
            "{url:?} must not reach the system opener"
        );
    }
    assert_eq!(crab.blob_fetches(), 0, "a link fetches nothing");
}

#[test]
fn the_local_crab_s_links_open_as_before_whatever_their_scheme() {
    let dir = tempfile::tempdir().expect("dir");
    let backend = Backend::local(dir.path().join("opencrabs.db"));
    for url in [
        "https://example.com",
        "mailto:me@example.com",
        "vscode://file/x",
        "slack://open",
    ] {
        assert_eq!(backend.openable_url(url), Ok(url), "{url}");
    }
}

/// The body of `fn <name>(` in `dashboard_sessions.rs`, up to the next
/// method.
fn sessions_fn(name: &str) -> &'static str {
    const SESSIONS: &str = include_str!("../dashboard_sessions.rs");
    let (_, body) = SESSIONS
        .split_once(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("{name}"));
    body.split("\n    pub(crate) fn ").next().unwrap_or(body)
}

#[test]
fn a_transcript_link_reaches_the_opener_only_through_openable_url() {
    let body = sessions_fn("open_transcript_target");
    assert!(body.contains("self.backend.openable_url(&url)"));
    assert_eq!(
        body.matches("preview::url_command(").count(),
        1,
        "one opener call, fed by openable_url"
    );
    let gate = body
        .find("match self.backend.openable_url(&url)")
        .expect("the link is matched on openable_url");
    let opened = body
        .find("Ok(_) => preview::launch(&preview::url_command(&url))")
        .expect("the opener runs only in the Ok arm");
    assert!(gate < opened, "openable_url is asked before the opener");
}

#[test]
fn a_previewed_document_is_read_where_localize_says() {
    let body = sessions_fn("open_preview");
    let localized = body
        .find("let load_path = self.backend.localize(&path);")
        .expect("the document's path goes through localize");
    let read = body
        .find("preview_document::parse(&load_path")
        .expect("the document is read from load_path");
    assert!(localized < read, "localized before it is read");
    assert!(!body.contains("let load_path = path.clone();"));
}
