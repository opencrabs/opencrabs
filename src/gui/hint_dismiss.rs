//! The window's door to the hint-dismissal sidecar (#3120, #3121).
//!
//! The engine in `ui_core::hints` decides which line a screen may print; it
//! never reads a database, because it also runs in the TUI, in tests and on
//! surfaces with no ledger at all. So the set of hidden ids is read here, once
//! at startup and again after each dismissal, and handed to the engine as
//! plain state.
//!
//! Both calls open the ledger through the same 4 MB, no-migration door the
//! rail uses. A hint is not worth a stack the size of a thread's, and it is
//! never worth failing the window over: a read that errors leaves the hints
//! showing rather than blanking them.

use opencrabs::db::Database;
use opencrabs::db::repository::HintDismissalRepository;
use opencrabs::ui_core::hints::DISMISS_DAYS;

/// The ids hidden right now, for a first paint that already respects them.
///
/// Blocking on purpose: the window has no frame to draw yet, and a hint that
/// flashes in for one frame and disappears is worse than one that never shows.
pub(crate) fn load(backend: crate::backend_glue::Backend) -> Vec<String> {
    let db_path = backend.local_db_path();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<String>>();
    let spawned = std::thread::Builder::new()
        .name("hint-dismissals".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let ids = match runtime {
                Ok(rt) => rt.block_on(async {
                    let db = Database::connect(&db_path).await.ok()?;
                    let repo = HintDismissalRepository::new(db.pool().clone());
                    repo.active().await.ok()
                }),
                Err(_) => None,
            };
            let _ = tx.send(ids.unwrap_or_default());
        });
    if spawned.is_err() {
        return Vec::new();
    }
    // The sender drops when the thread ends, so a dead thread reads as "no
    // dismissals" and every hint shows again instead of the window hanging.
    rx.recv().unwrap_or_default()
}

/// Hide one hint for [`DISMISS_DAYS`], on a thread of its own.
///
/// The window has already stopped drawing the line by the time this runs, so
/// the write is fire and forget: a failed write costs the reader the line
/// coming back on the next launch, which is the safe direction. Doing it
/// inline would block the click on a database open.
pub(crate) fn dismiss(backend: crate::backend_glue::Backend, hint_id: String) {
    let db_path = backend.local_db_path();
    let spawned = std::thread::Builder::new()
        .name("hint-dismiss".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(rt) = runtime else {
                return;
            };
            rt.block_on(async {
                let Ok(db) = Database::connect(&db_path).await else {
                    return;
                };
                let repo = HintDismissalRepository::new(db.pool().clone());
                if let Err(e) = repo.dismiss(&hint_id, DISMISS_DAYS).await {
                    crate::logging::error(&format!("hint dismissal write: {e}"));
                }
            });
        });
    if let Err(e) = spawned {
        crate::logging::error(&format!("hint dismissal thread failed to spawn: {e}"));
    }
}
