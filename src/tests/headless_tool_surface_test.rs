//! #129 — headless tool-surface honesty (owner-ruled design, issue body).
//!
//! Pins the three layers of the headless gate:
//! (a) the headless registry lacks `session_notify` + `suggest_options`,
//! (b) the interactive registry keeps them,
//! (c) both tools hard-error when invoked with a headless context
//!     (belt-and-braces backstop),
//! (d) the headless preamble is a standalone const and the registry gate
//!     documentation matches the owner ruling C (sub-agents always headless:
//!     both tools in ALWAYS_EXCLUDED).

use crate::brain::tools::Tool;
use crate::brain::tools::catalog;
use crate::brain::tools::error::ToolError;
use crate::brain::tools::registry::ToolRegistry;
use crate::brain::tools::subagent::ALWAYS_EXCLUDED;
use crate::brain::tools::r#trait::ToolExecutionContext;
use crate::brain::tools::{subagent, suggest_options::SuggestOptionsTool};
use crate::cli::tool_setup::{HEADLESS_PREAMBLE, register_core_agent_tools};
use crate::config::Config;
use crate::db::Database;
use serde_json::json;
use std::sync::Arc;

async fn build_registry(headless: bool, notify_enabled: bool) -> (Arc<ToolRegistry>, ()) {
    let db = Database::connect_in_memory().await.expect("in-memory db");
    db.run_migrations().await.expect("migrations");
    let mut config = Config::default();
    config.agent.session_notify_enabled = notify_enabled;
    let registry = Arc::new(ToolRegistry::new());
    let _m = register_core_agent_tools(&registry, &db, &config, headless);
    (registry, ())
}

/// Same, through the ACP assembly under test.
async fn build_acp_registry(notify_enabled: bool) -> (Arc<ToolRegistry>, ()) {
    use crate::cli::tool_setup::register_acp_agent_tools;
    let db = Database::connect_in_memory().await.expect("in-memory db");
    db.run_migrations().await.expect("migrations");
    let mut config = Config::default();
    config.agent.session_notify_enabled = notify_enabled;
    let registry = Arc::new(ToolRegistry::new());
    let _m = register_acp_agent_tools(&registry, &db, &config);
    (registry, ())
}

/// (a) Headless registry: both interactive-only tools absent.
#[tokio::test]
async fn headless_registry_lacks_session_notify_and_suggest_options() {
    let (registry, _) = build_registry(true, true).await;
    assert!(
        !registry.has_tool("session_notify"),
        "session_notify must NOT be registered headless (#129): a one-shot \
         process cannot flush a parked notification"
    );
    assert!(
        !registry.has_tool("suggest_options"),
        "suggest_options must NOT be registered headless (#129): it renders \
         nowhere without a TUI/channel handler"
    );
    // Sanity: the core set is still intact on the headless path.
    assert!(
        registry.has_tool("bash") && registry.has_tool("read_file"),
        "core tools must remain registered headless"
    );
}

/// (b) Interactive registry: both tools present, but only with the kill
/// switch opted in (#1840).
#[tokio::test]
async fn interactive_registry_keeps_session_notify_and_suggest_options() {
    let (registry, _) = build_registry(false, true).await;
    assert!(
        registry.has_tool("session_notify"),
        "interactive registry must keep session_notify when the flag is opted in"
    );
    assert!(
        registry.has_tool("suggest_options"),
        "interactive registry must keep suggest_options"
    );
}

/// (b2) #1840: interactive registry with the kill switch unset (default off):
/// `session_notify` must not exist in the surface at all. The agent never
/// sees the schema, never calls it, never gets confused by a refusal.
/// `suggest_options` is unaffected by the kill switch.
#[tokio::test]
async fn interactive_registry_hides_session_notify_when_kill_switch_is_off() {
    let (registry, _) = build_registry(false, false).await;
    assert!(
        !registry.has_tool("session_notify"),
        "session_notify must NOT be registered when [agent] session_notify_enabled \
         is unset/false (#1840): a disabled tool must not exist in the surface"
    );
    assert!(
        registry.has_tool("suggest_options"),
        "suggest_options is not governed by the kill switch and must stay"
    );
    // Sanity: the core set is intact with the flag off.
    assert!(
        registry.has_tool("bash") && registry.has_tool("read_file"),
        "core tools must remain registered with the kill switch off"
    );
}

/// (e) #1540 follow-up — the ACP surface is a LIVE user surface: the
/// interactive registry with `session_notify` stripped (no turn-end relay
/// to flush a parked notification over ACP), `suggest_options` present so
/// clients render its chips from the `tool_call` rawInput. Fails on the old
/// `cmd_acp` assembly, which passed `headless = true`.
#[tokio::test]
async fn acp_surface_keeps_suggest_options_and_drops_session_notify() {
    // Kill switch ON: the assertion must hold even for an operator who
    // opted in, because the drop is structural (no relay), not config.
    let (registry, _) = build_acp_registry(true).await;
    assert!(
        registry.has_tool("suggest_options"),
        "ACP is interactive: suggest_options must be registered so clients \
         can render the option chips"
    );
    assert!(
        !registry.has_tool("session_notify"),
        "ACP has no turn-end relay: session_notify must never be registered, \
         even with [agent] session_notify_enabled = true"
    );
    // Sanity: the core set is intact on the ACP path.
    assert!(
        registry.has_tool("bash") && registry.has_tool("read_file"),
        "core tools must remain registered on the ACP surface"
    );
}

/// (c) Belt-and-braces: both tools hard-error on a headless context.
#[tokio::test]
async fn interactive_only_tools_hard_error_on_headless_context() {
    let mut ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
    ctx.headless = true;
    // #1802: this suite targets the headless guard, not the kill switch;
    // opt the flag in so the headless path is what's under test.
    ctx.session_notify_enabled = true;

    let suggest_err = <SuggestOptionsTool as Tool>::execute(
        &SuggestOptionsTool,
        json!({ "options": ["Go", "Stop"] }),
        &ctx,
    )
    .await
    .expect_err("suggest_options must hard-error headless (#129)");
    assert!(
        matches!(suggest_err, ToolError::Execution(ref m) if m.contains("not available headless")),
        "suggest_options headless error must be a loud ToolError, got: {suggest_err:?}"
    );

    let notify_err = <subagent::SessionNotifyTool as Tool>::execute(
        &subagent::SessionNotifyTool,
        json!({ "target_session": uuid::Uuid::new_v4().to_string(), "text": "hi" }),
        &ctx,
    )
    .await
    .expect_err("session_notify must hard-error headless (#129)");
    assert!(
        matches!(notify_err, ToolError::Execution(ref m) if m.contains("not available headless")),
        "session_notify headless error must be a loud ToolError, got: {notify_err:?}"
    );
}

/// (c, cont.) Interactive context: no guard trip — suggest_options proceeds
/// past the headless check (it fails later on missing progress sink or
/// returns a normal verdict, but NEVER with the headless error).
#[tokio::test]
async fn suggest_options_does_not_trip_guard_on_interactive_context() {
    let ctx = ToolExecutionContext::new(uuid::Uuid::new_v4());
    let result = <SuggestOptionsTool as Tool>::execute(
        &SuggestOptionsTool,
        json!({ "options": ["Go"] }),
        &ctx,
    )
    .await;
    match result {
        Err(ToolError::Execution(m)) if m.contains("not available headless") => {
            panic!("headless guard must not fire on an interactive context")
        }
        _ => {} // normal path (verdict or unrelated error) is fine
    }
}

/// (d) Preamble content + sub-agent surface law.
#[test]
fn headless_preamble_names_the_final_message_law() {
    assert!(
        HEADLESS_PREAMBLE.contains("HEADLESS"),
        "preamble must identify the surface"
    );
    assert!(
        HEADLESS_PREAMBLE.to_lowercase().contains("final message"),
        "preamble must carry the final-message self-containedness law"
    );
    // AGENTS.md was deliberately NOT touched (owner design): the preamble is
    // the ONLY vehicle for the headless-scoped rule.
    assert!(
        HEADLESS_PREAMBLE.contains("session_notify")
            && HEADLESS_PREAMBLE.contains("suggest_options"),
        "preamble must warn the model off the two unavailable tools"
    );
}

/// Owner ruling C: sub-agents are ALWAYS headless — both tools sit in
/// ALWAYS_EXCLUDED, so build_child_registry strips them even from an
/// interactive parent's registry.
#[test]
fn child_registry_strips_interactive_only_tools() {
    assert!(
        ALWAYS_EXCLUDED.contains(&"session_notify"),
        "session_notify must be in ALWAYS_EXCLUDED (ruling C)"
    );
    assert!(
        ALWAYS_EXCLUDED.contains(&"suggest_options"),
        "suggest_options must be in ALWAYS_EXCLUDED (ruling C)"
    );

    // Simulate an interactive parent registry carrying both tools.
    let parent = ToolRegistry::new();
    parent.register(Arc::new(subagent::SessionNotifyTool));
    parent.register(Arc::new(SuggestOptionsTool));
    let child = subagent::build_child_registry(&parent);
    assert!(
        !child.has_tool("session_notify") && !child.has_tool("suggest_options"),
        "child registry must strip interactive-only tools regardless of parent surface"
    );
}

/// (a, v0.4.98 owner ruling) The headless lazy-catalog ROSTER must not
/// advertise the interactive-only pair: the registry gate removes them, so a
/// roster that still names them promises a capability the surface lacks. The
/// interactive roster keeps both (they are real, registered tools there).
#[test]
fn headless_roster_excludes_interactive_only_tools() {
    let headless = catalog::tool_access_prompt(true);
    assert!(
        !headless.contains("session_notify"),
        "headless roster must not advertise session_notify (v0.4.98 ruling a)"
    );
    assert!(
        !headless.contains("suggest_options"),
        "headless roster must not advertise suggest_options (v0.4.98 ruling a)"
    );
    // Sanity: the filter must not gut the roster — a real headless-reachable
    // tool still shows, and the agents group survives (minus its pair).
    assert!(
        headless.contains("spawn_agent") && headless.contains("wait_agent"),
        "headless roster must keep the agents group's other members"
    );

    // The roster only prints EXTENDED groups; suggest_options is a CORE tool
    // (catalog::CORE_TOOLS) and never appears in the roster on ANY surface —
    // it is registered in every non-sub-agent registry. session_notify IS an
    // extended tool, so the interactive roster must still name it.
    let interactive = catalog::tool_access_prompt(false);
    assert!(
        interactive.contains("session_notify"),
        "interactive roster keeps session_notify verbatim"
    );
    // suggest_options lives in CORE_TOOLS, outside the printed roster — pin the
    // structural truth instead of a string the roster never contained.
    assert!(
        catalog::CORE_TOOLS.contains(&"suggest_options"),
        "suggest_options is core (registered on interactive surfaces), not rostered"
    );
}
