//! Process-wide tokio runtime construction.
//!
//! The binary builds its runtime here instead of using `#[tokio::main]` so the
//! worker threads get a stack deep enough to carry an agent turn (#2080).

use std::io;

/// Worker-thread stack size for the process runtime.
///
/// `#[tokio::main]` spawns workers at the 2 MiB default, and the agent's first
/// turn nests deeper than that in debug builds: the process aborts with
/// `thread 'tokio-rt-worker' has overflowed its stack` as soon as the turn
/// starts, before any reply renders (#2080). 16 MiB matches what
/// `src/tests/auto_title_e2e_test.rs` already proves sufficient for the same
/// future, so the setting is a measured value and not a guess.
pub const WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

/// Build the multi-thread runtime the binary drives the app on.
///
/// Every worker thread gets [`WORKER_STACK_SIZE`], so a turn that runs on any
/// of them is independent of the caller's environment and of the platform's
/// default thread stack.
pub fn build() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(WORKER_STACK_SIZE)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The depth the agent future is proven to need, recorded in
    /// `src/tests/auto_title_e2e_test.rs`: "verified empirically: SIGABRT
    /// unset, pass at any RUST_MIN_STACK >= 8MB".
    const PROVEN_MINIMUM: usize = 8 * 1024 * 1024;

    #[test]
    fn worker_stack_covers_the_depth_the_agent_future_needs() {
        // Both sides are constants, so the comparison is decided at compile
        // time: a stack lowered below the proven minimum fails the build, not
        // just this test.
        const {
            assert!(
                WORKER_STACK_SIZE >= PROVEN_MINIMUM,
                "worker stack must cover the depth the agent future is proven to need (#2080)"
            )
        };
    }

    #[test]
    fn the_binary_builds_its_runtime_through_this_builder() {
        // A structural guard: the behavioural half of #2080 lives in the
        // first-run harness, because only the real first turn reaches the
        // depth. What this catches is the regression that actually happened,
        // a future edit putting the binary back on the 2 MiB default.
        let main_rs = include_str!("main.rs");
        // Match the attribute as an attribute line, not as a substring: a bare
        // `contains` also hits the explanatory comment in main.rs that names
        // the macro, which made this guard fail on a correctly converted file.
        let on_tokio_main_macro = main_rs
            .lines()
            .any(|line| line.trim().starts_with("#[tokio::main"));
        assert!(
            !on_tokio_main_macro,
            "src/main.rs is back on #[tokio::main], which spawns workers at the 2 MiB default and aborts the first agent turn (#2080)"
        );
        assert!(
            main_rs.contains("runtime::build()"),
            "src/main.rs no longer builds its runtime through runtime::build(), so the worker stack is unset (#2080)"
        );
    }

    #[test]
    fn an_agent_turn_completes_on_the_production_runtime() {
        // Smoke test on the production configuration: the runtime the binary
        // ships must carry a full turn to completion. The mock provider keeps
        // the turn shallow, so this proves the configuration is usable rather
        // than proving the stack depth; depth is covered by the harness.
        let rt = build().expect("build the production runtime");
        rt.block_on(async {
            use crate::brain::agent::service::AgentService;
            use crate::db::Database;
            use crate::services::{ServiceContext, SessionService};
            use crate::tests::agent_service_mocks::MockProvider;
            use std::sync::Arc;

            let db = Database::connect_in_memory().await.unwrap();
            db.run_migrations().await.unwrap();
            let context = ServiceContext::new(db.pool().clone());

            let agent = AgentService::new_for_test(Arc::new(MockProvider), context.clone()).await;
            let sessions = SessionService::new(context);
            let session = sessions
                .create_session(Some("Runtime smoke".to_string()))
                .await
                .unwrap();

            let reply = agent
                .send_message(session.id, "hello".to_string(), None)
                .await
                .unwrap();
            assert!(
                !reply.content.is_empty(),
                "the production runtime must carry a turn to completion"
            );
        });
    }
}
