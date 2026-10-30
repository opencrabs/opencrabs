//! Guard: em dashes stay off the SelfHealingAlert display path (#1745
//! follow-up, 2026-09-29 audit). `normalize_dashes` only sanitizes model
//! prose through `strip_llm_artifacts`; alert text composed in-repo used to
//! reach Slack/Discord/Telegram/WhatsApp/CLI posts with em dashes intact.
//! Two invariants are pinned here:
//!
//! 1. The alert text constants carry no typographic dashes.
//! 2. Every file that renders `ProgressEvent::SelfHealingAlert` calls
//!    `normalize_dashes`, so any future producer stays covered. A renderer
//!    written as `{ message: msg }` instead of `{ message }` defeats this
//!    scan; the pattern is the shape all six current arms use.

use crate::brain::agent::service::tool_loop::{
    ACCOUNT_ROTATION_ALERT, CONTINUATION_EMPTY_ALERT, EMPTY_ANSWER_ANALYSIS_ALERT,
    PHANTOM_RETRY_ENFORCEMENT_ALERT, SELF_HEAL_BUDGET_ROLLED_ALERT, SELF_HEAL_EXHAUSTED_ALERT,
};
use crate::brain::agent::service::truncation::{CONTINUATION_NUDGE_ALERT, INCOMPLETE_MARKER};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn alert_constants_carry_no_typographic_dashes() {
    let consts: [(&str, &str); 8] = [
        (
            "PHANTOM_RETRY_ENFORCEMENT_ALERT",
            PHANTOM_RETRY_ENFORCEMENT_ALERT,
        ),
        (
            "SELF_HEAL_BUDGET_ROLLED_ALERT",
            SELF_HEAL_BUDGET_ROLLED_ALERT,
        ),
        ("SELF_HEAL_EXHAUSTED_ALERT", SELF_HEAL_EXHAUSTED_ALERT),
        ("ACCOUNT_ROTATION_ALERT", ACCOUNT_ROTATION_ALERT),
        ("CONTINUATION_EMPTY_ALERT", CONTINUATION_EMPTY_ALERT),
        ("EMPTY_ANSWER_ANALYSIS_ALERT", EMPTY_ANSWER_ANALYSIS_ALERT),
        ("CONTINUATION_NUDGE_ALERT", CONTINUATION_NUDGE_ALERT),
        ("INCOMPLETE_MARKER", INCOMPLETE_MARKER),
    ];
    for (name, text) in consts {
        assert!(
            !text.contains('\u{2014}') && !text.contains('\u{2013}'),
            "{name} carries a typographic dash: {text:?}"
        );
    }
}

#[test]
fn every_self_healing_renderer_normalizes_dashes() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files);
    assert!(
        files.len() > 100,
        "source walk found nothing under {:?}; cwd={:?}",
        src,
        std::env::current_dir()
    );
    let mut violations = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&src)
            .expect("walk root is src")
            .to_string_lossy()
            .replace('\\', "/");
        if rel.starts_with("tests/") {
            continue;
        }
        let Ok(content) = fs::read_to_string(file) else {
            continue;
        };
        if content.contains("ProgressEvent::SelfHealingAlert { message } =>")
            && !content.contains("normalize_dashes")
        {
            violations.push(rel);
        }
    }
    assert!(
        violations.is_empty(),
        "files rendering SelfHealingAlert without normalize_dashes: {violations:?}"
    );
}

#[test]
fn trello_outbound_funnels_through_the_boundary() {
    // #1970: the #1745 audit missed Trello, leaving card comments as the
    // only channel shipping completely raw model prose (no dash
    // normalization, no artifact stripping, no ZERO WIDTH cleanup). Pin the
    // client chokepoint so both the reply echo and the proactive
    // add_comment action stay inside the boundary, and so a future
    // refactor that drops the call fails here instead of shipping raw.
    let client = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/channels/trello/client.rs");
    let content = fs::read_to_string(&client)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", client.display()));
    assert!(
        content.contains("async fn add_comment_to_card("),
        "add_comment_to_card moved or renamed; re-verify the Trello boundary (#1970)"
    );
    assert!(
        content.contains("sanitize_outbound"),
        "Trello client no longer routes outbound comments through \
         sanitize_outbound (#1970 regression; #2102 made the boundary \
         stricter by redacting secrets after stripping artifacts)"
    );
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
