//! #2975: the working-directory tools must not ask the caller's runtime what
//! shape it is.
//!
//! The window runs every turn on a `current_thread` runtime
//! (`src/gui/agent_bridge.rs`), while the terminal runs the binary on a
//! `multi_thread` one. `tokio::task::block_in_place` panics on the first and
//! is legal on the second, so the same tool worked in the TUI and killed every
//! GUI turn that changed directory.
//!
//! Each test drives the tool from a `current_thread` runtime, which is the
//! shape the window uses. On the old tree the call panics inside
//! `block_in_place` and the test fails with that panic instead of an
//! assertion.

use crate::brain::tools::Tool;
use crate::brain::tools::ToolExecutionContext;
use crate::brain::tools::config_tool::ConfigTool;
use crate::brain::tools::slash_command::SlashCommandTool;
use crate::db::Database;
use crate::db::models::Session;
use crate::db::repository::SessionRepository;
use crate::services::ServiceContext;
use serde_json::json;

/// An in-memory ledger with one session, so the persist step actually runs.
/// Without a service context the tools never reach the blocking call, and the
/// test would pass on the broken tree.
async fn context() -> ToolExecutionContext {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let pool = db.pool().clone();

    let session = Session::new(Some("chat".into()), Some("m".into()), None);
    SessionRepository::new(pool.clone())
        .create(&session)
        .await
        .unwrap();

    let mut ctx = ToolExecutionContext::new(session.id);
    ctx.service_context = Some(ServiceContext::new(pool));
    ctx
}

/// The window's shape: one thread, one reactor, no blocking pool.
fn current_thread() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn config_manager_set_working_directory_returns_on_a_current_thread_runtime() {
    let rt = current_thread();
    rt.block_on(async {
        let ctx = context().await;
        let dir = std::env::temp_dir();

        let out = ConfigTool
            .execute(
                json!({
                    "operation": "set_working_directory",
                    "path": dir.to_string_lossy(),
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(out.success, "tool failed: {:?}", out.error);
        assert!(
            out.output.contains("Working directory changed to"),
            "unexpected output: {}",
            out.output
        );
    });
}

#[test]
fn slash_command_cd_returns_on_a_current_thread_runtime() {
    let rt = current_thread();
    rt.block_on(async {
        let ctx = context().await;
        let dir = std::env::temp_dir();

        let out = SlashCommandTool
            .execute(
                json!({
                    "command": "/cd",
                    "args": dir.to_string_lossy(),
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(out.success, "tool failed: {:?}", out.error);
        assert!(
            out.output.contains("Working directory changed to"),
            "unexpected output: {}",
            out.output
        );
    });
}
