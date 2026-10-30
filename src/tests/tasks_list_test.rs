//! #1160 tasks_list + detached status files + prompt rot-guard.

use crate::brain::agent::service::work_status::{self, test_override};
use crate::brain::tools::Tool;
use crate::brain::tools::tasks_list::{
    DetachedRow, SubagentRow, TasksListTool, render_tasks, subagent_status_file,
};
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn schema_has_no_params_and_tool_is_read_only() {
    let tool = TasksListTool::new();
    assert_eq!(tool.name(), "tasks_list");
    assert_eq!(
        tool.input_schema()["properties"].as_object().unwrap().len(),
        0
    );
    assert!(tool.hints().read_only);
}

#[test]
fn render_empty_roster_says_so_explicitly() {
    let out = render_tasks(&[], &[]);
    assert_eq!(out, "No background tasks.");
}

#[test]
fn render_lists_both_systems_with_states_and_pointers() {
    // Fixture path is deliberately NEUTRAL: it must not be the retired
    // pre-#26 layout (see `work_status::legacy_dir()`), which this test used
    // to enshrine as correct (#165). The real advertised path is pinned by
    // `advertised_status_file_is_the_file_the_writer_creates` below.
    let subs = vec![SubagentRow {
        id: "agt-1".into(),
        label: "research".into(),
        state: "running".into(),
        status_file: Some("/tmp/detached/agt-1.json".into()),
    }];
    let det = vec![DetachedRow {
        label: "cargo test".into(),
        elapsed_secs: 42,
    }];
    let out = render_tasks(&subs, &det);
    assert!(out.contains("Sub-agents (1)"), "was: {out}");
    assert!(out.contains("- agt-1 [research] running"), "was: {out}");
    assert!(out.contains("status file: /tmp/detached/agt-1.json"));
    assert!(out.contains("Detached commands (1)"), "was: {out}");
    assert!(out.contains("- cargo test (elapsed 42s)"), "was: {out}");
}

/// The path `tasks_list` hands the model must be a file a writer really
/// created.
///
/// Deliberately crosses the writer boundary instead of comparing
/// `subagent_status_file()` to the resolver it delegates to: that comparison
/// is a tautology, green on any build, including one whose resolver points at
/// a directory nothing creates. Here a real `WorkStatus::new_agent` persists
/// the record and the advertised STRING is what opens it, so the test fails
/// on any drift between the two sides, retired `legacy_dir()` included.
///
/// An empty read from a wrong path is indistinguishable from "this sub-agent
/// never existed", which is why this is pinned rather than assumed.
#[test]
fn advertised_status_file_is_the_file_the_writer_creates() {
    use crate::brain::agent::service::work_status;

    let dir = TempDir::new().unwrap();
    test_override::set(dir.path().join("detached"));

    let id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    work_status::WorkStatus::new_agent(
        &id.to_string(),
        "research",
        &session_id.to_string(),
        "probe the advertised path",
        None,
    )
    .unwrap();

    let advertised = subagent_status_file(&id.to_string());
    let raw = std::fs::read_to_string(&advertised)
        .unwrap_or_else(|e| panic!("advertised path {advertised} is not readable: {e}"));
    assert!(raw.contains("\"kind\": \"agent\""), "was: {raw}");
    assert!(raw.contains(&session_id.to_string()), "was: {raw}");

    // The retired pre-#26 sibling is a directory nothing creates; advertising
    // through it is the failure mode this pins against.
    let retired = work_status::legacy_dir().display().to_string();
    assert!(
        !advertised.starts_with(&retired),
        "advertised path {advertised} must not live under the retired {retired}"
    );

    test_override::clear();
}

/// Gap 2: a detached command's status file exists mid-run with spawn data,
/// and gains exit info on completion. Since #26 the record lives in the
/// unified `work_status` schema (`kind: command`, state machine, `finish`).
#[test]
fn detached_status_file_written_then_finished() {
    let dir = TempDir::new().unwrap();
    test_override::set(dir.path().join("detached"));

    let task_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    work_status::WorkStatus::new_command(
        &task_id.to_string(),
        &session_id.to_string(),
        "cargo test",
        "cargo test --lib",
    )
    .unwrap();
    let path = dir.path().join("detached").join(format!("{task_id}.json"));
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"cargo test\""), "was: {raw}");
    assert!(raw.contains(&session_id.to_string()), "was: {raw}");
    assert!(raw.contains("\"kind\": \"command\""), "was: {raw}");
    assert!(!raw.contains("\"finish\""), "mid-run must be unfinished");

    work_status::WorkStatus::finish_command(
        &task_id.to_string(),
        &session_id.to_string(),
        "cargo test",
        "cargo test --lib",
        work_status::CommandExit {
            success: true,
            code: 0,
            elapsed_secs: 99.5,
            output_bytes: 2048,
        },
    )
    .unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("\"success\": true"), "was: {raw}");
    assert!(raw.contains("\"output_bytes\": 2048"), "was: {raw}");
    assert!(raw.contains("\"state\": \"Completed\""), "was: {raw}");
}

/// Gap 3 rot-guard: the LONG TASKS paragraph must keep covering sub-agents,
/// not only bash — it regressed to bash-only once already (#762).
#[test]
fn prompt_builder_keeps_subagent_background_contract() {
    let dir = TempDir::new().unwrap();
    let prompt = crate::brain::prompt_builder::BrainLoader::new(dir.path().to_path_buf())
        .build_system_brain(None);
    assert!(
        prompt.contains("spawned agents run in the background"),
        "subagent background contract missing from system prompt"
    );
}

/// Rot-guard: the LONG TASKS paragraph must keep forbidding hand-rolled
/// polling of detached tasks (#2101). Only the sub-agent sentence was pinned,
/// so the anti-polling clauses could be deleted with the suite still green.
#[test]
fn prompt_builder_keeps_anti_polling_contract() {
    let dir = TempDir::new().unwrap();
    let prompt = crate::brain::prompt_builder::BrainLoader::new(dir.path().to_path_buf())
        .build_system_brain(None);
    for clause in [
        "do NOT sit in a wait loop",
        "do NOT re-run it to \"check\"",
        "do NOT hand-roll a poll loop",
    ] {
        assert!(
            prompt.contains(clause),
            "anti-polling clause missing from system prompt: {clause}"
        );
    }
}

/// #191 regression pin: `execute` reports only the CALLER's sub-agents.
///
/// The manager is process-global (one instance per channel factory), so
/// pre-fix this iterated every session's children. That is not merely noise:
/// the tool's framing tells the model these are *its* in-flight sub-agents
/// ("do not spawn duplicates"), so a foreign row makes a lane silently skip
/// work it believes is already running.
///
/// Pins the call site rather than the renderer — `render_tasks` takes rows the
/// caller built, so it cannot see a scope regression.
#[tokio::test]
async fn execute_lists_only_the_callers_subagents() {
    use crate::brain::tools::ToolExecutionContext;
    use crate::brain::tools::subagent::{SubAgent, SubAgentManager};
    use std::sync::Arc;

    fn child(id: &str, label: &str, parent: Uuid) -> SubAgent {
        SubAgent::new(id.to_string(), label.to_string(), Uuid::new_v4(), parent)
    }

    let me = Uuid::from_u128(0x191);
    let other = Uuid::from_u128(0x192);

    let mgr = Arc::new(SubAgentManager::new());
    mgr.insert(child("mine0001", "my-research", me));
    mgr.insert(child("theirs01", "their-research", other));

    let mut ctx = ToolExecutionContext::new(me);
    ctx.subagent_manager = Some(mgr);

    let out = TasksListTool::new()
        .execute(serde_json::json!({}), &ctx)
        .await
        .unwrap();
    assert!(out.success, "tasks_list failed: {:?}", out.error);
    assert!(
        out.output.contains("mine0001"),
        "own child missing: {}",
        out.output
    );
    assert!(
        !out.output.contains("theirs01"),
        "another session's child leaked into the roster: {}",
        out.output
    );
}
