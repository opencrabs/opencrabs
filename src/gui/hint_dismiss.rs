//! The window's door to the hint-dismissal sidecar (#3120, #3121).
//!
//! The engine in `ui_core::hints` decides which line a screen may print; it
//! never reads a database, because it also runs in the TUI, in tests and on
//! surfaces with no ledger at all. So the set of hidden ids is read here, once
//! at startup and again after each dismissal, and handed to the engine as
//! plain state.
//!
//! Both calls are the crab's (`HintDismissals`, `DismissHint`), run on the
//! backend's 4 MB worker lane through the same no-migration door the rail
//! uses. A hint is never worth failing the window over: a read that errors
//! leaves the hints showing rather than blanking them.

/// The ids hidden right now, for a first paint that already respects them.
///
/// Blocking on purpose: the window has no frame to draw yet, and a hint that
/// flashes in for one frame and disappears is worse than one that never shows.
/// The read is the crab's (`HintDismissals`), on its own worker lane; this
/// thread only parks on the answer.
pub(crate) fn load(backend: crate::backend_glue::Backend) -> Vec<String> {
    // Any failure, a dead worker included, reads as "no dismissals" and every
    // hint shows again instead of the window hanging.
    futures::executor::block_on(backend.call(opencrabs::backend::settings::HintDismissals {}))
        .unwrap_or_default()
}

/// Hide one hint for `DISMISS_DAYS`, off the window's thread.
///
/// The window has already stopped drawing the line by the time this runs, so
/// the write is fire and forget: a failed write costs the reader the line
/// coming back on the next launch, which is the safe direction. Doing it
/// inline would block the click on a database open.
pub(crate) fn dismiss(backend: crate::backend_glue::Backend, hint_id: String) {
    let spawned = backend.spawn(
        opencrabs::backend::settings::DismissHint { hint_id },
        |result| {
            // A ledger that did not open stays silent, as it always did; only
            // the write's own failure is worth a log line.
            if let Ok(Some(line)) = result {
                crate::logging::error(&line);
            }
        },
    );
    if let Err(e) = spawned {
        crate::logging::error(&format!("hint dismissal thread failed to spawn: {e}"));
    }
}
