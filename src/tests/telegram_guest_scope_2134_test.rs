//! #2134: a guest turn reaches a SCOPED tool surface, not the owner's.
//!
//! The guest allowlist (`TelegramConfig::guest_allowed`) answers who may ask.
//! It never answered what that caller may reach: an admitted guest ran the
//! owner's full registry with one restriction, `deny_all_approvals`, which only
//! refuses tools declaring `requires_approval()`. Every read-only tool stayed
//! free, so a stranger could search the owner's whole session history, read
//! `USER.md` / `MEMORY.md`, and read any absolute path on disk.
//!
//! These pin the three layers that close it:
//!
//! 1. the schema filter, so a guest is never shown what it may not call;
//! 2. the registry refusal, for a model that calls a remembered name anyway;
//! 3. `read_file` confinement, so an allowed read stays inside the tree.
//!
//! The last test is the other half of the contract: the scope is guest-only.
//! A normal group turn and a TUI turn keep the full set.

use crate::brain::tools::error::Result;
use crate::brain::tools::read::ReadTool;
use crate::brain::tools::registry::ToolRegistry;
use crate::brain::tools::scope;
use crate::brain::tools::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashSet;
use uuid::Uuid;

/// A registered tool with a fixed name, standing in for the real ones so the
/// registry's filter and refusal can be exercised without wiring 95 tools.
struct NamedTool {
    name: &'static str,
}

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "stub"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![]
    }
    async fn execute(&self, _input: Value, _ctx: &ToolExecutionContext) -> Result<ToolResult> {
        Ok(ToolResult::success("ok".to_string()))
    }
}

/// The tools the issue names, plus one allowed tool to prove the surface is not
/// simply empty.
fn registry_with_the_owner_surface() -> ToolRegistry {
    let reg = ToolRegistry::new();
    for name in [
        "web_search",
        "read_file",
        "tool_search",
        "session_search",
        "channel_search",
        "load_brain_file",
        "memory_search",
        "bash",
        "write_file",
    ] {
        reg.register(std::sync::Arc::new(NamedTool { name }));
    }
    reg
}

fn names_of(defs: &[crate::brain::provider::Tool]) -> HashSet<String> {
    defs.iter().map(|t| t.name.clone()).collect()
}

/// The message a result carries, wherever the tool put it. `ToolResult::error`
/// leaves `output` empty and fills `error`, so assertions read both rather than
/// silently comparing against an empty string.
fn message(result: &ToolResult) -> String {
    if result.output.is_empty() {
        result.error.clone().unwrap_or_default()
    } else {
        result.output.clone()
    }
}

#[test]
fn a_guest_schema_set_is_the_allowlist_and_nothing_else() {
    let reg = registry_with_the_owner_surface();
    // Lazy tools ON: the filter runs before the core/extended split.
    let guest = reg.get_tool_definitions_scoped(&HashSet::new(), true);
    let names = names_of(&guest);

    for allowed in ["web_search", "read_file", "tool_search"] {
        assert!(
            names.contains(allowed),
            "a guest must keep '{allowed}'; got {names:?}"
        );
    }
    for refused in [
        "session_search",
        "channel_search",
        "load_brain_file",
        "memory_search",
        "bash",
        "write_file",
    ] {
        assert!(
            !names.contains(refused),
            "a guest must never be shown '{refused}'; got {names:?}"
        );
    }

    // Lazy tools OFF: the normal path ships everything, so the guest path has
    // its own entry point. Missing it would hand a stranger all nine.
    let guest_unlazy = names_of(&reg.get_tool_definitions_for_guest());
    assert_eq!(
        guest_unlazy, names,
        "the lazy and unlazy guest sets must be identical"
    );
}

#[test]
fn the_owner_surface_is_untouched_by_the_scope() {
    let reg = registry_with_the_owner_surface();
    // Not a guest: every registered tool rides, both in lazy mode (core set
    // here, so read_file and friends) and unlazy mode (everything).
    let all = names_of(&reg.get_tool_definitions());
    assert_eq!(
        all.len(),
        9,
        "an owner turn keeps the full set; got {all:?}"
    );

    // Lazy mode: `false` must be a no-op. The scoped set has to match the
    // unscoped one exactly, whatever the core set happens to hold that day.
    // Asserting on specific names would break every time the catalog changes.
    let lazy = names_of(&reg.get_tool_definitions_scoped(&HashSet::new(), false));
    let lazy_unscoped = names_of(&reg.get_tool_definitions_filtered(&HashSet::new()));
    assert_eq!(
        lazy, lazy_unscoped,
        "the scope flag must not narrow an owner's lazy set; got {lazy:?}"
    );
    assert!(
        lazy.contains("load_brain_file") && lazy.contains("bash"),
        "an owner's core set must keep the personal-context tools; got {lazy:?}"
    );
}

#[tokio::test]
async fn a_guest_call_to_a_refused_tool_is_rejected_at_dispatch() {
    let reg = registry_with_the_owner_surface();
    let ctx = ToolExecutionContext::new(Uuid::new_v4()).with_guest_scope(true);

    // session_search: the worst of the set. Unfiltered it scans EVERY session,
    // the owner's DMs included.
    let out = reg
        .execute("session_search", json!({"query": "anything"}), &ctx)
        .await
        .expect("dispatch must return a refusal, not an error");
    assert!(!out.success, "a guest session_search must be refused");
    assert!(
        message(&out).contains("not available to a guest turn"),
        "the refusal must say why: {}",
        message(&out)
    );
    assert!(
        message(&out).contains("web_search"),
        "the refusal must name what IS reachable, so the model stops retrying: {}",
        message(&out)
    );

    // load_brain_file: the owner's USER.md, MEMORY.md and AGENTS.md.
    let out = reg
        .execute("load_brain_file", json!({"name": "MEMORY.md"}), &ctx)
        .await
        .expect("dispatch must return a refusal, not an error");
    assert!(!out.success, "a guest load_brain_file must be refused");

    // bash and write_file: refused even with auto-approve on, so the guest
    // scope does not depend on the approval callback being wired.
    let yolo = ToolExecutionContext::new(Uuid::new_v4())
        .with_guest_scope(true)
        .with_auto_approve(true);
    for name in ["bash", "write_file"] {
        let out = reg
            .execute(name, json!({"command": "echo hi", "path": "/tmp/x"}), &yolo)
            .await
            .expect("dispatch must return a refusal, not an error");
        assert!(!out.success, "a guest {name} must be refused even on yolo");
    }
}

#[tokio::test]
async fn a_guest_read_outside_the_working_directory_is_refused() {
    let ctx = ToolExecutionContext::new(Uuid::new_v4())
        .with_guest_scope(true)
        .with_working_directory(std::env::temp_dir());

    // An absolute path outside the tree. Before this, `validate_path_safety`
    // only required the path to EXIST, so anything readable was readable.
    let out = ReadTool
        .execute(json!({"path": "/etc/hosts"}), &ctx)
        .await
        .expect("a refusal is a result, not an error");
    assert!(
        !out.success,
        "a guest read outside the tree must be refused"
    );
    assert!(
        message(&out).contains("outside the working directory"),
        "the refusal must name the confinement: {}",
        message(&out)
    );

    // Inside the tree, the same tool still works: the scope confines, it does
    // not disable.
    let dir = tempfile::tempdir().expect("temp dir");
    let inside = dir.path().join("notes.txt");
    std::fs::write(&inside, "hello from inside\n").expect("write");
    let ctx = ToolExecutionContext::new(Uuid::new_v4())
        .with_guest_scope(true)
        .with_working_directory(dir.path().to_path_buf());
    let out = ReadTool
        .execute(json!({"path": inside.to_str().unwrap()}), &ctx)
        .await
        .expect("read");
    assert!(
        out.success,
        "a permitted read must still work: {}",
        message(&out)
    );
    assert!(message(&out).contains("hello from inside"));

    // A symlink inside the tree that points outside it is refused too: the
    // check is about where the bytes live, not how the path was spelled.
    #[cfg(unix)]
    {
        let link = dir.path().join("escape");
        std::os::unix::fs::symlink("/etc/hosts", &link).expect("symlink");
        let out = ReadTool
            .execute(json!({"path": link.to_str().unwrap()}), &ctx)
            .await
            .expect("read");
        assert!(
            !out.success,
            "a symlink out of the tree must not be a way around the scope: {}",
            message(&out)
        );
    }
}

#[test]
fn the_confinement_helper_resolves_before_it_compares() {
    let root = std::path::Path::new("/tmp/tree");
    assert!(scope::path_stays_inside(
        root,
        std::path::Path::new("/tmp/tree/a/b.rs")
    ));
    assert!(scope::path_stays_inside(
        root,
        std::path::Path::new("/tmp/tree")
    ));
    assert!(!scope::path_stays_inside(
        root,
        std::path::Path::new("/tmp/treehouse/a.rs")
    ));
    assert!(!scope::path_stays_inside(
        root,
        std::path::Path::new("/etc/hosts")
    ));
    // `..` is normalized away rather than taken literally, so an escape cannot
    // ride a path that merely STARTS with the root.
    assert!(!scope::path_stays_inside(
        root,
        std::path::Path::new("/tmp/tree/../elsewhere/x")
    ));
}

#[test]
fn the_allowlist_is_a_short_public_surface() {
    // Default-deny is the point: a tool added tomorrow is out of a guest's
    // reach until someone lists it. If this assertion ever needs a big edit,
    // that edit is the review.
    assert!(scope::guest_allows("web_search"));
    assert!(scope::guest_allows("read_file"));
    assert!(!scope::guest_allows("session_search"));
    assert!(!scope::guest_allows("channel_search"));
    assert!(!scope::guest_allows("load_brain_file"));
    assert!(!scope::guest_allows("memory_search"));
    assert!(!scope::guest_allows("bash"));
    assert!(!scope::guest_allows("write_file"));
    assert!(!scope::guest_allows("config_manager"));
    assert!(!scope::guest_allows("spawn_agent"));
    assert!(!scope::guest_allows("a_tool_invented_next_week"));
}
