//! The crab's paths the plan strip and the preview header stat (C5c).
//!
//! `PlanPath` and an opened preview name a file on the crab's disk. Before
//! the strip or the header stats one, the path goes through
//! `Backend::localize`: a remote crab's file is read from the window's blob
//! cache, never looked up on this machine's disk by the crab's name, and the
//! local crab's path is its own.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use opencrabs::tui::plan::{PlanDocument, PlanStatus, PlanTask, TaskType};

use crate::backend_glue::Backend;
use crate::backend_remote::RemoteCrab;
use crate::blob_cache_store::BlobStore;
use crate::plan_strip::PlanStripState;
use crate::preview::{self, Preview};

const TURNS: &str = include_str!("../dashboard_turns.rs");
const RENDER: &str = include_str!("../dashboard_render.rs");

/// A remote crab whose copies go to `cache`. Nothing listens on the discard
/// port, so a fetch could only fail; the tests assert none starts.
fn crab_with_cache(cache: &Path) -> Arc<RemoteCrab> {
    Arc::new(
        RemoteCrab::new(
            "localized",
            "http://127.0.0.1:9".into(),
            None,
            PathBuf::from("unused.db"),
        )
        .with_blob_dir(cache.to_path_buf()),
    )
}

fn plan_json() -> Vec<u8> {
    let mut plan = PlanDocument::new(uuid::Uuid::new_v4(), "Landing".to_string());
    plan.status = PlanStatus::Active;
    plan.add_task(PlanTask::new(
        1,
        "task".to_string(),
        String::new(),
        TaskType::Edit,
    ));
    serde_json::to_vec(&plan).expect("plan serializes")
}

#[test]
fn a_remote_plan_path_reaches_the_strip_localized() {
    let cache = tempfile::tempdir().expect("cache");
    let crab = crab_with_cache(cache.path());
    let on_crab = Path::new("/home/crab/.opencrabs/plans/landing.json");
    assert!(!on_crab.exists(), "the crab's path is not on this machine");
    BlobStore::at(cache.path().to_path_buf())
        .keep(on_crab, &plan_json())
        .expect("keep");
    let backend = Backend::Remote(crab.clone());

    let mut strip = PlanStripState::default();
    strip.set_crab_path(&backend, Some(on_crab.to_path_buf()));

    let plan = strip.document().expect("the strip read the cached copy");
    assert_eq!(plan.title, "Landing");
    assert_eq!(
        crab.blob_fetches(),
        0,
        "a cached plan asks the crab nothing"
    );
}

#[test]
fn a_local_plan_path_reaches_the_strip_as_it_was() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("plan.json");
    std::fs::write(&path, plan_json()).expect("plan file");
    let backend = Backend::local(dir.path().join("opencrabs.db"));

    let mut through_crab = PlanStripState::default();
    through_crab.set_crab_path(&backend, Some(path.clone()));
    let mut direct = PlanStripState::default();
    direct.set_path(Some(path));

    assert!(through_crab.document().is_some());
    assert_eq!(through_crab.progress_label(), direct.progress_label());
    assert_eq!(through_crab.mtime(), direct.mtime());
}

#[test]
fn a_remote_document_header_states_the_cached_copy() {
    let cache = tempfile::tempdir().expect("cache");
    let crab = crab_with_cache(cache.path());
    let on_crab = Path::new("/home/crab/.opencrabs/tmp/gui-uploads/report.pdf");
    BlobStore::at(cache.path().to_path_buf())
        .keep(on_crab, b"pdf!")
        .expect("keep");
    let backend = Backend::Remote(crab.clone());
    let open = Preview::of(on_crab.to_path_buf());

    let header = preview::detail_line(&open.localized(&backend), 1.0, None);

    assert!(header.starts_with("pdf document"), "{header}");
    assert!(header.contains("4 B"), "{header}");
    assert_eq!(crab.blob_fetches(), 0);
}

#[test]
fn a_local_document_header_is_unchanged() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("report.pdf");
    std::fs::write(&path, b"pdf!").expect("file");
    let backend = Backend::local(dir.path().join("opencrabs.db"));
    let open = Preview::of(path);

    assert_eq!(
        preview::detail_line(&open.localized(&backend), 1.0, None),
        preview::detail_line(&open, 1.0, None),
    );
}

/// The wiring the tests above cannot reach without a window: the `PlanPath`
/// arm hands both strips the crab's path through `set_crab_path`, and the
/// preview header is drawn from the localized preview.
#[test]
fn the_window_localizes_before_the_strip_and_the_header() {
    let arm = TURNS
        .split("agent_bridge::TurnEvent::PlanPath { session, path } =>")
        .nth(1)
        .and_then(|rest| rest.split("agent_bridge::TurnEvent::").next())
        .expect("the PlanPath arm");
    assert_eq!(arm.matches("set_crab_path(").count(), 2, "{arm}");
    assert!(!arm.contains(".set_path("), "{arm}");

    let header = RENDER
        .split("preview::detail_line(")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("the header line");
    assert!(header.contains("open.localized(&self.backend)"), "{header}");
}
