//! The crab the task doors read from inside a frame (`dashboard_turns.rs`).
//!
//! The layout guard (#3062) freezes inline modules under `gui/`, so these sit
//! beside the module they exercise and reach it through `crate::` paths.

use std::path::PathBuf;
use std::sync::Arc;

use crate::backend_glue::{Backend, RemoteCrab};
use crate::dashboard_turns::tasks_ctx;

#[test]
fn a_remote_crab_has_no_tasks_ctx() {
    let crab = RemoteCrab::new(
        "elsewhere",
        "http://127.0.0.1:9",
        None,
        PathBuf::from("unused.db"),
    );
    assert!(tasks_ctx(&Backend::Remote(Arc::new(crab))).is_none());
}

#[test]
fn a_local_crab_reads_its_own_tasks_ctx() {
    assert!(tasks_ctx(&Backend::local(PathBuf::from("unused.db"))).is_some());
}
