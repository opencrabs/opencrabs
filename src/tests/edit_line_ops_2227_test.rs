//! Regression (#2227): `edit_file`'s line operations must change only the
//! lines they were asked to change.
//!
//! `replace_lines`, `insert_line` and `delete_lines` used to rebuild the whole
//! file from `content.lines()` and `join("\n")`. `lines()` drops every
//! terminator and a trailing `\r`, so the join could only put back the ending
//! it was told to: a CRLF file came out LF, and any file lost its final
//! newline. The tool result showed only the changed lines, so nothing said so.
//!
//! The fix splices byte ranges out of the original, the way the fuzzy fallback
//! already does since #2212. Untouched bytes are never rewritten.
//!
//! Every assertion compares bytes, not lines: a line-level check cannot see a
//! lost `\r` or a missing final newline.

use crate::brain::tools::Tool;
use crate::brain::tools::ToolExecutionContext;
use crate::brain::tools::edit::EditTool;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

/// The fixture from the report: five CRLF lines, 60 bytes.
const CRLF_CONFIG: &str = "[app]\r\nname = demo\r\nmode = debug\r\nport = 8080\r\nworkers = 4\r\n";

fn context(dir: &TempDir) -> ToolExecutionContext {
    ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(dir.path().to_path_buf())
        .with_auto_approve(true)
}

/// Run one edit and return the file exactly as it now sits on disk.
async fn edit(name: &str, original: &str, input: serde_json::Value) -> String {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, original).unwrap();

    let result = EditTool.execute(input, &context(&dir)).await.unwrap();
    assert!(result.success, "edit reported failure: {:?}", result.error);

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
async fn deleting_a_line_from_a_crlf_file_changes_only_that_line() {
    let after = edit(
        "settings.ini",
        CRLF_CONFIG,
        json!({"path": "settings.ini", "operation": "delete_lines", "start_line": 2, "end_line": 2}),
    )
    .await;

    assert_bytes(
        &after,
        "[app]\r\nname = demo\r\nport = 8080\r\nworkers = 4\r\n",
    );
    assert_eq!(after.matches('\r').count(), 4, "one CR per remaining line");
    assert!(after.ends_with("\r\n"), "final newline survives");
}

#[tokio::test]
async fn deleting_a_line_from_an_lf_file_keeps_the_final_newline() {
    let after = edit(
        "notes.txt",
        "a\nb\nc\nd\ne\nf\n",
        json!({"path": "notes.txt", "operation": "delete_lines", "start_line": 1, "end_line": 1}),
    )
    .await;

    assert_bytes(&after, "a\nc\nd\ne\nf\n");
}

#[tokio::test]
async fn replacing_a_line_on_a_crlf_file_keeps_every_other_byte() {
    let after = edit(
        "settings.ini",
        CRLF_CONFIG,
        json!({"path": "settings.ini", "operation": "replace_lines", "start_line": 2, "end_line": 2, "new_text": "mode = release"}),
    )
    .await;

    assert_bytes(
        &after,
        "[app]\r\nname = demo\r\nmode = release\r\nport = 8080\r\nworkers = 4\r\n",
    );
}

#[tokio::test]
async fn replacement_text_adopts_the_file_ending() {
    // `read_file` hands the model CR-stripped text, so a multi-line
    // replacement arrives with bare LF even on a Windows file. The inserted
    // lines have to take the file's ending, or the fix would preserve the
    // old bytes and still write new LF lines into a CRLF file.
    let after = edit(
        "settings.ini",
        CRLF_CONFIG,
        json!({"path": "settings.ini", "operation": "replace_lines", "start_line": 2, "end_line": 2, "new_text": "mode = release\nverified = true"}),
    )
    .await;

    assert_bytes(
        &after,
        "[app]\r\nname = demo\r\nmode = release\r\nverified = true\r\nport = 8080\r\nworkers = 4\r\n",
    );
    assert_eq!(after.matches('\r').count(), 6);
}

#[tokio::test]
async fn inserting_a_line_on_a_crlf_file_keeps_the_ending() {
    let after = edit(
        "settings.ini",
        "[app]\r\nname = demo\r\nport = 8080\r\n",
        json!({"path": "settings.ini", "operation": "insert_line", "line": 2, "text": "mode = debug"}),
    )
    .await;

    assert_bytes(
        &after,
        "[app]\r\nname = demo\r\nmode = debug\r\nport = 8080\r\n",
    );
}

#[tokio::test]
async fn a_file_without_a_final_newline_does_not_gain_one() {
    let after = edit(
        "tail.txt",
        "a\nb\nc",
        json!({"path": "tail.txt", "operation": "delete_lines", "start_line": 1, "end_line": 1}),
    )
    .await;

    assert_bytes(&after, "a\nc");
}

#[tokio::test]
async fn deleting_the_last_line_leaves_the_preceding_terminator() {
    let after = edit(
        "tail.txt",
        "a\nb\n",
        json!({"path": "tail.txt", "operation": "delete_lines", "start_line": 1, "end_line": 1}),
    )
    .await;

    assert_bytes(&after, "a\n");
}

#[tokio::test]
async fn inserting_at_eof_into_an_unterminated_file_terminates_the_old_last_line() {
    let after = edit(
        "tail.txt",
        "a\nb",
        json!({"path": "tail.txt", "operation": "insert_line", "line": 2, "text": "c"}),
    )
    .await;

    assert_bytes(&after, "a\nb\nc");
}

#[tokio::test]
async fn inserting_at_the_top_adds_no_leading_blank_line() {
    let after = edit(
        "top.txt",
        "a\r\nb\r\n",
        json!({"path": "top.txt", "operation": "insert_line", "line": 0, "text": "header"}),
    )
    .await;

    assert_bytes(&after, "header\r\na\r\nb\r\n");
}

#[tokio::test]
async fn a_mixed_ending_file_stays_mixed() {
    // Line 0 is CRLF, line 1 is LF, line 2 is CRLF. Deleting the middle line
    // must not normalise the two survivors to a common ending.
    let after = edit(
        "mixed.txt",
        "a\r\nb\nc\r\n",
        json!({"path": "mixed.txt", "operation": "delete_lines", "start_line": 1, "end_line": 1}),
    )
    .await;

    assert_bytes(&after, "a\r\nc\r\n");
}

#[tokio::test]
async fn replacing_a_range_with_several_lines_keeps_the_ending() {
    let after = edit(
        "range.txt",
        "a\r\nb\r\nc\r\nd\r\n",
        json!({"path": "range.txt", "operation": "replace_lines", "start_line": 1, "end_line": 2, "new_text": "x\ny"}),
    )
    .await;

    assert_bytes(&after, "a\r\nx\r\ny\r\nd\r\n");
}

#[tokio::test]
async fn a_lone_carriage_return_is_not_a_line_break() {
    // `lines()` treats only `\n` as a separator, and so does the splice. An
    // old-Mac `\r` inside a line must survive a delete elsewhere in the file.
    let after = edit(
        "old.txt",
        "a\rb\nc\n",
        json!({"path": "old.txt", "operation": "delete_lines", "start_line": 1, "end_line": 1}),
    )
    .await;

    assert_bytes(&after, "a\rb\n");
}

#[tokio::test]
async fn the_reported_line_count_still_matches_the_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.ini");
    std::fs::write(&path, CRLF_CONFIG).unwrap();

    let result = EditTool
        .execute(
            json!({"path": "settings.ini", "operation": "delete_lines", "start_line": 2, "end_line": 2}),
            &context(&dir),
        )
        .await
        .unwrap();

    assert!(result.success);
    assert!(
        result.output.contains("Lines: 5 \u{2192} 4"),
        "unexpected output: {}",
        result.output
    );
    assert!(
        result.output.contains("- mode = debug"),
        "the diff should still name the removed line: {}",
        result.output
    );
}
