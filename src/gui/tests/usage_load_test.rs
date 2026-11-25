//! The usage screen's fetch (`usage_load::load_view`) goes through the crab.
//!
//! The window used to open the ledger itself by path; it now asks the
//! backend for `UsageView` and builds the view model from the reply. These
//! pin that the view it draws is still the loader's view of the same ledger,
//! and that a ledger that does not open still reads as a failure carrying
//! the loader's own text.

use std::sync::Arc;

use opencrabs::backend::Ctx;
use opencrabs::ui_core::usage::data::Period;

use crate::backend_glue::Backend;
use crate::boot::LoadState;
use crate::usage_load::load_view;

/// A fresh, migrated ledger in its own directory.
fn ledger(dir: &tempfile::TempDir) -> std::path::PathBuf {
    let db_path = dir.path().join("usage-load.db");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let db = opencrabs::db::Database::connect(&db_path)
            .await
            .expect("connect");
        db.run_migrations().await.expect("migrate");
    });
    db_path
}

#[test]
fn the_window_draws_the_loaders_view_of_the_same_ledger() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = ledger(&dir);
    let backend = Backend::Local(Arc::new(Ctx::new(db_path.clone())));
    for period in [Period::Today, Period::AllTime] {
        let drawn = match futures::executor::block_on(load_view(backend.clone(), period)) {
            LoadState::Loaded(vm) => vm,
            LoadState::Failed(e) => panic!("{period:?}: {e}"),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let loaded = runtime
            .block_on(opencrabs::ui_core::usage::loader::usage_view(
                &db_path, period,
            ))
            .expect("the loader reads the same ledger");
        assert_eq!(format!("{drawn:?}"), format!("{loaded:?}"), "{period:?}");
    }
}

#[test]
fn a_ledger_that_does_not_open_fails_with_the_loaders_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A directory where the file should be: nothing can read it as SQLite.
    let db_path = dir.path().to_path_buf();
    let backend = Backend::Local(Arc::new(Ctx::new(db_path.clone())));
    let drawn = match futures::executor::block_on(load_view(backend, Period::Month)) {
        LoadState::Failed(e) => e.to_string(),
        LoadState::Loaded(_) => panic!("a directory is not a ledger"),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let loaded = runtime
        .block_on(opencrabs::ui_core::usage::loader::usage_view(
            &db_path,
            Period::Month,
        ))
        .expect_err("the loader cannot read a directory either");
    assert_eq!(drawn, format!("{loaded:#}"));
}
