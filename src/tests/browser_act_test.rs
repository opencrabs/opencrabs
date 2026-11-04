//! browser_act (multi_act, #1188) — pure-core tests. No Chrome needed:
//! parsing/validation, cap enforcement, prefix reporting, and pre-flight
//! JS shape are all pure functions. The Chrome-requiring login-fixture
//! e2e lives at the bottom, `#[ignore]`'d per house convention
//! (see browser_e2e_test.rs for the rationale).

#![cfg(feature = "browser")]

use crate::brain::tools::browser::{Act, build_pre_flight_js, format_prefix_report, parse_actions};
use serde_json::json;

fn actions(v: serde_json::Value) -> serde_json::Value {
    json!({ "actions": v })
}

#[test]
fn parses_all_five_action_types() {
    let input = actions(json!([
        {"type": "click",  "selector": "#submit"},
        {"type": "fill",   "selector": "input[name=user]", "text": "adolfo"},
        {"type": "press",  "key": "Enter"},
        {"type": "select", "selector": "#country", "value": "PT"},
        {"type": "wait",   "ms": 500},
    ]));
    let acts = parse_actions(&input).expect("valid batch parses");
    assert_eq!(acts.len(), 5);
    assert_eq!(
        acts[0],
        Act::Click {
            selector: "#submit".into()
        }
    );
    assert_eq!(
        acts[1],
        Act::Fill {
            selector: "input[name=user]".into(),
            text: "adolfo".into()
        }
    );
    assert_eq!(
        acts[2],
        Act::Press {
            key: "Enter".into()
        }
    );
    assert_eq!(
        acts[3],
        Act::Select {
            selector: "#country".into(),
            value: "PT".into()
        }
    );
    assert_eq!(acts[4], Act::Wait { ms: 500 });
}

#[test]
fn cap_of_ten_is_enforced_up_front() {
    let eleven: Vec<_> = (0..11)
        .map(|i| json!({"type": "press", "key": format!("Key{i}")}))
        .collect();
    let err = parse_actions(&actions(json!(eleven))).unwrap_err();
    assert!(err.contains("Too many actions: 11"), "got: {err}");
    assert!(err.contains("cap is 10"), "got: {err}");

    // Exactly ten passes — the cap is inclusive.
    let ten: Vec<_> = (0..10)
        .map(|i| json!({"type": "press", "key": format!("Key{i}")}))
        .collect();
    assert_eq!(parse_actions(&actions(json!(ten))).unwrap().len(), 10);
}

#[test]
fn unknown_type_and_missing_fields_are_rejected_with_action_numbers() {
    let err = parse_actions(&actions(json!([
        {"type": "press", "key": "Enter"},
        {"type": "drag", "selector": "#x"},
    ])))
    .unwrap_err();
    assert!(err.contains("action 2"), "got: {err}");
    assert!(err.contains("unknown type 'drag'"), "got: {err}");

    let err = parse_actions(&actions(json!([{"type": "fill", "selector": "#u"}]))).unwrap_err();
    assert!(err.contains("action 1"), "got: {err}");
    assert!(err.contains("\"text\""), "got: {err}");

    let err = parse_actions(&actions(json!([{"type": "click"}]))).unwrap_err();
    assert!(err.contains("\"selector\""), "got: {err}");
}

#[test]
fn wait_is_bounded_per_action() {
    let err = parse_actions(&actions(json!([{"type": "wait", "ms": 60000}]))).unwrap_err();
    assert!(err.contains("exceeds the per-action cap"), "got: {err}");

    let err = parse_actions(&actions(json!([{"type": "wait", "ms": 0}]))).unwrap_err();
    assert!(err.contains("positive"), "got: {err}");
}

#[test]
fn empty_or_missing_actions_array_is_rejected() {
    assert!(parse_actions(&json!({})).is_err());
    assert!(parse_actions(&actions(json!([]))).is_err());
}

// ---- #2112: the upload action ----

/// A real file on disk: the upload action validates paths at parse time, so a
/// fixture has to exist.
fn fixture_file(dir: &tempfile::TempDir, name: &str) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, b"fixture-bytes").unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn upload_parses_and_labels_its_file_count() {
    let dir = tempfile::TempDir::new().unwrap();
    let a = fixture_file(&dir, "a.mp4");
    let b = fixture_file(&dir, "b.jpg");
    let acts = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]", "files": [a, b]},
    ])))
    .expect("valid upload parses");

    assert_eq!(
        acts[0],
        Act::Upload {
            selector: "input[type=file]".into(),
            files: vec![a, b],
        }
    );
    assert_eq!(
        acts[0].label(),
        "upload input[type=file] (2 files)",
        "the label must name the selector and the file count"
    );
    // The selector takes part in pre-flight, so a stale input rejects the
    // batch with zero side effects like any other targeted action.
    assert_eq!(acts[0].selector(), Some("input[type=file]"));
}

#[test]
fn upload_rejects_an_empty_or_missing_files_array() {
    let dir = tempfile::TempDir::new().unwrap();
    let a = fixture_file(&dir, "a.mp4");

    let err = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]", "files": []},
    ])))
    .unwrap_err();
    assert!(err.contains("non-empty"), "got: {err}");

    let err = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]"},
    ])))
    .unwrap_err();
    assert!(err.contains("\"files\""), "got: {err}");

    // A non-string entry is not a path.
    let err = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]", "files": [a, 7]},
    ])))
    .unwrap_err();
    assert!(err.contains("non-empty strings"), "got: {err}");
}

#[test]
fn upload_rejects_a_relative_path_before_any_cdp_call() {
    let err = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]", "files": ["clip.mp4"]},
    ])))
    .unwrap_err();
    assert!(err.contains("not absolute"), "got: {err}");
}

#[test]
fn upload_rejects_a_missing_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let missing = dir.path().join("nope.mp4");
    let err = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]", "files": [missing.to_string_lossy()]},
    ])))
    .unwrap_err();
    assert!(err.contains("does not exist"), "got: {err}");
}

#[test]
fn upload_rejects_a_directory() {
    // DOM.setFileInputFiles takes any path the browser can read; a directory
    // is not a file to attach.
    let dir = tempfile::TempDir::new().unwrap();
    let sub = dir.path().join("folder");
    std::fs::create_dir(&sub).unwrap();
    let err = parse_actions(&actions(json!([
        {"type": "upload", "selector": "input[type=file]", "files": [sub.to_string_lossy()]},
    ])))
    .unwrap_err();
    assert!(err.contains("not a regular file"), "got: {err}");
}

#[test]
fn the_unknown_type_error_lists_upload() {
    // The rejection message is how a model learns the vocabulary; leaving
    // upload out of it hides the action from every caller that guessed wrong.
    let err = parse_actions(&actions(json!([{"type": "attach", "selector": "#x"}]))).unwrap_err();
    assert!(
        err.contains("click/fill/press/select/wait/upload"),
        "got: {err}"
    );
}

#[test]
fn prefix_report_names_completed_prefix_and_holds_remaining() {
    let failed = Act::Click {
        selector: "#gone".into(),
    };
    // Two done, third failed → "actions 1-2 OK; action 3 …"
    let r = format_prefix_report(2, &failed, "element no longer present");
    assert!(r.starts_with("actions 1-2 OK;"), "got: {r}");
    assert!(r.contains("action 3 (click #gone) failed"), "got: {r}");
    assert!(r.contains("NOT run"), "got: {r}");

    // Nothing done → whole call failed cleanly.
    let r = format_prefix_report(0, &failed, "not found");
    assert!(r.contains("action 1 (click #gone) failed"), "got: {r}");
    assert!(r.contains("No actions executed"), "got: {r}");
}

#[test]
fn pre_flight_js_embeds_all_selectors_and_all_three_shapes() {
    let js = build_pre_flight_js(&[
        "#submit".into(),
        "text=Sign in".into(),
        "xpath=//button[1]".into(),
    ]);
    // All three selectors ride in as a JSON array.
    assert!(js.contains("\"#submit\""), "js: {js}");
    assert!(js.contains(r#""text=Sign in""#), "js: {js}");
    assert!(js.contains(r#""xpath=//button[1]""#), "js: {js}");
    // All three resolution strategies present.
    // Pre-flight must resolve exactly what execution will resolve: css
    // and text pierce open shadow roots, xpath stays main-document
    // because the XPath spec has no shadow boundary.
    assert!(js.contains("text=") && js.contains("__ocWalk()"));
    assert!(js.contains("xpath=") && js.contains("document.evaluate"));
    assert!(js.contains("__ocQueryOne(s)"));
    // Visibility gating is part of resolution (zero-size rects fail).
    assert!(js.contains("getBoundingClientRect"));
    // Per-selector result objects carry index + ok + reason.
    assert!(js.contains("results.push"));
}

/// Login-form fixture acceptance (#1188): a full fill+fill+press
/// sequence completes in ONE call with a success result and screenshot.
/// `#[ignore]` — launches real Chrome (house convention, see header).
#[tokio::test]
#[ignore = "launches real Chrome — opt-in via `cargo test -- --ignored browser_act`"]
async fn login_form_completes_in_one_call() {
    use crate::brain::tools::browser::{BrowserActTool, BrowserManager};
    use crate::brain::tools::{Tool, ToolExecutionContext};
    use std::sync::Arc;
    use uuid::Uuid;

    let mgr = Arc::new(BrowserManager::new(Default::default()));
    let act = BrowserActTool::new(mgr.clone());
    let ctx = ToolExecutionContext::new(Uuid::new_v4());

    // The tried-and-true public login fixture used across browser e2e.
    let nav = crate::brain::tools::browser::BrowserNavigateTool::new(mgr.clone());
    let _ = Tool::execute(
        &nav,
        json!({"url": "https://the-internet.herokuapp.com/login"}),
        &ctx,
    )
    .await;

    let out = Tool::execute(
        &act,
        actions(json!([
            {"type": "fill", "selector": "#username", "text": "tomsmith"},
            {"type": "fill", "selector": "#password", "text": "SuperSecretPassword!"},
            {"type": "press", "key": "Enter"},
        ])),
        &ctx,
    )
    .await
    .expect("execute ok");

    assert!(out.success, "batch should succeed: {:?}", out.error);
    assert!(
        out.output.contains("Executed 3 actions"),
        "got: {}",
        out.output
    );
    let close = crate::brain::tools::browser::BrowserCloseTool::new(mgr);
    let _ = close.execute(serde_json::json!({}), &ctx).await;
}
