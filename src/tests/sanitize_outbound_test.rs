//! Regression tests for #2102: one outbound sanitize chokepoint.
//!
//! Seven channel paths called `strip_llm_artifacts` alone, so a model reply
//! carrying a credential reached Trello comments, Discord/Slack reactions and
//! intermediate streamed text with the key intact. The fix adds
//! [`sanitize_outbound`] (artifacts then secrets) and routes those paths
//! through it.
//!
//! The source-scan guard keeps the pair from drifting apart again: any direct
//! `strip_llm_artifacts(` call left under `src/channels/` must have a
//! `redact_secrets` call within 20 lines either side. Measured pairing distance
//! at the time of the fix is at most 14 lines forward and 4 back.

use crate::utils::sanitize::{redact_secrets, sanitize_outbound, strip_llm_artifacts};

const KEY_SHAPED: &str = "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789";

#[test]
fn outbound_redaction_redacts_a_credential_shaped_string() {
    let text = format!("here is the key {KEY_SHAPED} use it");
    let out = sanitize_outbound(&text);
    assert!(!out.contains(KEY_SHAPED), "credential leaked: {out}");
    assert!(out.contains("[REDACTED]"), "no redaction marker: {out}");
}

#[test]
fn outbound_sanitize_strips_artifacts_and_redacts_in_one_call() {
    let text = format!("answer <!-- tools-v2: [{{\"name\":\"bash\"}}] --> {KEY_SHAPED}");
    let out = sanitize_outbound(&text);
    assert!(!out.contains("tools-v2"), "artifact leaked: {out}");
    assert!(!out.contains(KEY_SHAPED), "credential leaked: {out}");
    assert!(out.contains("answer"), "prose dropped: {out}");
}

#[test]
fn outbound_sanitize_leaves_ordinary_prose_intact() {
    let text = "The build finished in 42 seconds and the tests passed.";
    assert_eq!(sanitize_outbound(text), text);
}

/// Every direct `strip_llm_artifacts(` call under `src/channels/` must be
/// paired with a `redact_secrets` call within 20 lines either side.
#[test]
fn every_channel_artifact_strip_is_paired_with_a_secret_redaction() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels");
    let mut files = Vec::new();
    collect_rust_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "source scan found no files under {}",
        root.display()
    );

    let mut unpaired = Vec::new();
    for path in files {
        let source = std::fs::read_to_string(&path).expect("read channel source");
        let lines: Vec<&str> = source.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if !line.contains("strip_llm_artifacts(") || line.trim_start().starts_with("//") {
                continue;
            }
            let start = idx.saturating_sub(20);
            let end = (idx + 20).min(lines.len().saturating_sub(1));
            let paired = lines[start..=end].iter().any(|l| {
                (l.contains("redact_secrets") || l.contains("sanitize_outbound("))
                    && !l.trim_start().starts_with("//")
            });
            if !paired {
                unpaired.push(format!("{}:{}", path.display(), idx + 1));
            }
        }
    }

    assert!(
        unpaired.is_empty(),
        "strip_llm_artifacts without a nearby redact_secrets (use sanitize_outbound): {unpaired:?}"
    );
}

/// The seven paths fixed for #2102 keep going through the chokepoint.
#[test]
fn the_seven_outbound_paths_use_the_chokepoint() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels");
    let paths = [
        "trello/client.rs",
        "discord/reactions.rs",
        "discord/interactions.rs",
        "slack/reactions.rs",
        "slack/flow_progress.rs",
        "slack/handler.rs",
        "telegram/agent.rs",
    ];
    for rel in paths {
        let source = std::fs::read_to_string(root.join(rel)).expect("read channel source");
        assert!(
            source.contains("sanitize_outbound("),
            "{rel} lost its sanitize_outbound call"
        );
    }
}

fn collect_rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

// Keep the two building blocks referenced so an unused-import warning cannot
// hide a future rename of the helpers this guard depends on.
#[test]
fn the_building_blocks_still_exist() {
    let text = format!("x {KEY_SHAPED}");
    assert!(!redact_secrets(&text).contains(KEY_SHAPED));
    assert_eq!(strip_llm_artifacts("plain"), "plain");
}
