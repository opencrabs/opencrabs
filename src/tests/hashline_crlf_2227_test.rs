//! Regression (#2227): `hashline_edit` must not rewrite line endings it was
//! not asked to touch.
//!
//! The sibling defect to the `edit_file` line operations. `hashline_edit`
//! rebuilds the whole document from a line vector and joined it with a bare
//! `\n`, so editing one line of a CRLF file converted every line in the file
//! to LF. The tool result showed only the changed lines, so nothing said so.
//!
//! Unlike `edit_file`, this tool applies edits bottom-up over a line vector, so
//! a mixed-ending file cannot come out byte-identical. The file's dominant
//! ending wins, and that trade-off is pinned by a test here rather than left
//! implicit.

use crate::brain::tools::Tool;
use crate::brain::tools::ToolExecutionContext;
use crate::brain::tools::hashline::edit::HashlineEditTool;
use crate::brain::tools::hashline::hash::hash_line;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

const CRLF_CONFIG: &str = "[app]\r\nname = demo\r\nport = 8080\r\n";

fn context(dir: &TempDir) -> ToolExecutionContext {
    ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(dir.path().to_path_buf())
        .with_auto_approve(true)
}

async fn run(name: &str, original: &str, input: serde_json::Value) -> String {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, original).unwrap();

    let result = HashlineEditTool
        .execute(input, &context(&dir))
        .await
        .unwrap();
    assert!(result.success, "edit failed: {:?}", result.error);

    std::fs::read_to_string(&path).unwrap()
}

fn assert_bytes(actual: &str, expected: &str) {
    assert_eq!(
        actual.as_bytes(),
        expected.as_bytes(),
        "\nactual   {actual:?}\nexpected {expected:?}"
    );
}

#[tokio::test]
async fn replacing_a_line_on_a_crlf_file_keeps_crlf() {
    let after = run(
        "settings.ini",
        CRLF_CONFIG,
        json!({
            "path": "settings.ini",
            "edits": [{
                "op": "replace",
                "pos": format!("2#{}", hash_line("name = demo")),
                "lines": "name = prod"
            }]
        }),
    )
    .await;

    assert_bytes(&after, "[app]\r\nname = prod\r\nport = 8080\r\n");
    assert_eq!(after.matches('\r').count(), 3, "one CR per line");
}

#[tokio::test]
async fn appending_to_a_crlf_file_keeps_crlf() {
    let after = run(
        "settings.ini",
        CRLF_CONFIG,
        json!({
            "path": "settings.ini",
            "edits": [{
                "op": "append",
                "pos": format!("3#{}", hash_line("port = 8080")),
                "lines": "workers = 4"
            }]
        }),
    )
    .await;

    assert_bytes(
        &after,
        "[app]\r\nname = demo\r\nport = 8080\r\nworkers = 4\r\n",
    );
}

#[tokio::test]
async fn prepending_to_a_crlf_file_keeps_crlf() {
    let after = run(
        "settings.ini",
        CRLF_CONFIG,
        json!({
            "path": "settings.ini",
            "edits": [{
                "op": "prepend",
                "pos": format!("1#{}", hash_line("[app]")),
                "lines": "; managed"
            }]
        }),
    )
    .await;

    assert_bytes(
        &after,
        "; managed\r\n[app]\r\nname = demo\r\nport = 8080\r\n",
    );
}

#[tokio::test]
async fn a_crlf_file_without_a_final_newline_does_not_gain_one() {
    let after = run(
        "tail.ini",
        "a\r\nb",
        json!({
            "path": "tail.ini",
            "edits": [{
                "op": "replace",
                "pos": format!("1#{}", hash_line("a")),
                "lines": "A"
            }]
        }),
    )
    .await;

    assert_bytes(&after, "A\r\nb");
}

#[tokio::test]
async fn an_lf_file_stays_lf() {
    // The guard against over-correcting: a plain Unix file must not pick up
    // carriage returns because the fix learned about CRLF.
    let after = run(
        "notes.txt",
        "a\nb\nc\n",
        json!({
            "path": "notes.txt",
            "edits": [{
                "op": "replace",
                "pos": format!("2#{}", hash_line("b")),
                "lines": "B"
            }]
        }),
    )
    .await;

    assert_bytes(&after, "a\nB\nc\n");
    assert_eq!(after.matches('\r').count(), 0);
}

#[tokio::test]
async fn a_mixed_ending_file_takes_its_dominant_ending() {
    // Documented trade-off: the bottom-up line vector cannot carry per-line
    // endings, so two CRLF lines outvote one LF line.
    let after = run(
        "mixed.txt",
        "a\r\nb\nc\r\n",
        json!({
            "path": "mixed.txt",
            "edits": [{
                "op": "replace",
                "pos": format!("3#{}", hash_line("c")),
                "lines": "C"
            }]
        }),
    )
    .await;

    assert_bytes(&after, "a\r\nb\r\nC\r\n");
}
