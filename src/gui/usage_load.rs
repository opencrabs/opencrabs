//! Reading the usage ledger off the main thread.
//!
//! Moved out of the crate root as pure code motion: the functions, their
//! comments and their tests are unchanged, only the module boundary is new.

use super::*;

/// The period's display word for the usage chrome line: the owner asked for
/// one line reading `OpenCrabs Usage · All Time · v0.5.15`, the middle word
/// tracking the period switch (t/w/m/a).
pub(crate) fn period_word(p: Period) -> &'static str {
    match p {
        Period::Today => "Today",
        Period::Week => "Weekly",
        Period::Month => "Monthly",
        Period::AllTime => "All Time",
    }
}

/// Fetch the real view through the crab (`UsageView`) and build the view
/// model here, so the formatting stays where the drawing is.
///
/// The read runs on the backend's 4 MB worker lane, never on this future's
/// thread: gpui's background executor polls its async tasks on Apple
/// dispatch-queue worker threads whose stacks are tiny, and `Config::load`'s
/// frame chain (thread-locals, dyld TLV instantiation) blows that stack with a
/// silent SIGILL. Crash receipt 2026-10-30 08:33:39, exception "Thread stack
/// size exceeded", frames `ledger_path -> Config::load -> resolve_profile_home
/// -> _tlv_get_addr`. The future only awaits the worker's answer. The op opens
/// the ledger by the loader's no-migration door: the daemon and the TUI stay
/// the owners of the schema.
pub(crate) async fn load_view(backend: crate::backend_glue::Backend, period: Period) -> LoadState {
    match backend
        .call(opencrabs::backend::usage::UsageView { period })
        .await
    {
        Ok(data) => LoadState::Loaded(Box::new(UsageViewModel::build(&data.into(), period))),
        Err(e) => LoadState::Failed(e.into_message().into()),
    }
}
