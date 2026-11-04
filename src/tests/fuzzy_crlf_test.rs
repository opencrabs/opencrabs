//! Regression (#2212): the fuzzy edit fallback must not rewrite line endings
//! it was not asked to touch.
//!
//! `read_file` shows the model CR-stripped text, so on a CRLF file the model
//! composes `old_text` with bare LF. The exact substring match then misses and
//! `fuzzy_replace_once` takes over. It used to rebuild the whole file from
//! `lines()` and `join("\n")`, which dropped every `\r` in the file, not just
//! the matched block.
//!
//! The fix splices the matched byte range out of the original, so untouched
//! bytes are never rewritten.

use crate::brain::tools::Tool;
use crate::brain::tools::ToolExecutionContext;
use crate::brain::tools::edit::EditTool;
use crate::brain::tools::file_versions::forget_session;
use crate::brain::tools::fuzzy::fuzzy_replace_once;
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

/// The fixture from the report: five CRLF lines, 61 bytes.
const CRLF_CONFIG: &str = "name=demo\r\nhost=localhost\r\nport=8080\r\ndebug=false\r\nlog=info\r\n";

/// Run the fallback, asserting the input really does miss the exact path.
/// An `old_str` that is a literal substring takes the fast path and proves
/// nothing about the code this file exists to pin.
fn fuzzy_only(content: &str, old_str: &str, new_str: &str) -> String {
    assert!(
        !content.contains(old_str),
        "test is worthless: {old_str:?} hits the exact-substring path"
    );
    fuzzy_replace_once(content, old_str, new_str).expect("fuzzy replace")
}

#[test]
fn a_crlf_file_keeps_crlf_on_every_untouched_line() {
    let replaced = fuzzy_only(
        CRLF_CONFIG,
        "host=localhost\nport=8080",
        "host=0.0.0.0\nport=9090",
    );

    assert_eq!(
        replaced,
        "name=demo\r\nhost=0.0.0.0\r\nport=9090\r\ndebug=false\r\nlog=info\r\n"
    );
    assert_eq!(
        replaced.matches("\r\n").count(),
        5,
        "every line keeps its CRLF"
    );
    assert_eq!(
        CRLF_CONFIG.len(),
        61,
        "the reported fixture is 61 bytes, not 54"
    );
}

#[test]
fn the_replacement_lines_adopt_the_file_ending() {
    // new_str arrives with LF because that is what the model saw, but the file
    // is CRLF, so the inserted lines must be CRLF too.
    let replaced = fuzzy_only("a=1\r\nb=2\r\n", "a=1\nb=2", "a=1\nb=3");
    assert_eq!(replaced, "a=1\r\nb=3\r\n");
}

#[test]
fn an_lf_file_stays_lf() {
    // Trailing space in old_str: the line matches fuzzily, the string does
    // not, so the fallback runs on an LF file.
    let replaced = fuzzy_only("alpha\nbeta\ngamma\n", "beta ", "BETA");
    assert_eq!(replaced, "alpha\nBETA\ngamma\n");
}

#[test]
fn a_mixed_ending_file_is_not_normalised() {
    // Only the matched block is rewritten. The untouched neighbours keep the
    // ending they already had, so the LF lines stay LF in a mostly-CRLF file.
    let content = "first\nsecond\nthird\r\nfourth\n";
    let replaced = fuzzy_only(content, "second \nthird", "SECOND\nTHIRD");
    assert_eq!(replaced, "first\nSECOND\r\nTHIRD\r\nfourth\n");
}

#[test]
fn a_file_without_a_trailing_newline_does_not_gain_one() {
    let replaced = fuzzy_only("alpha\r\nbeta", "alpha\nbeta", "alpha\nBETA");
    assert_eq!(replaced, "alpha\r\nBETA");
}

#[test]
fn deleting_a_matched_block_removes_its_terminator_too() {
    let replaced = fuzzy_only("alpha\r\nbeta\r\ngamma\r\n", "beta ", "");
    assert_eq!(replaced, "alpha\r\ngamma\r\n");
}

#[test]
fn a_multiline_replacement_keeps_the_block_position() {
    let replaced = fuzzy_only("a\r\nb\r\nc\r\nd\r\n", "b\nc", "B\nC\nD");
    assert_eq!(replaced, "a\r\nB\r\nC\r\nD\r\nd\r\n");
}

/// The reported path end to end: `edit_file` on a CRLF file with LF `old_text`.
#[tokio::test]
async fn edit_file_preserves_crlf_on_a_file_it_only_partly_touches() {
    let dir = TempDir::new().unwrap();
    let session = Uuid::new_v4();
    let path = dir.path().join("app.ini");
    std::fs::write(&path, CRLF_CONFIG).unwrap();

    let context = ToolExecutionContext::new(session)
        .with_working_directory(dir.path().to_path_buf())
        .with_auto_approve(true);

    let result = EditTool
        .execute(
            json!({
                "path": path.to_str().unwrap(),
                "operation": "replace",
                "old_text": "host=localhost\nport=8080",
                "new_text": "host=0.0.0.0\nport=9090",
            }),
            &context,
        )
        .await
        .expect("edit_file");
    assert!(result.success, "edit failed: {:?}", result.output);

    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        after,
        "name=demo\r\nhost=0.0.0.0\r\nport=9090\r\ndebug=false\r\nlog=info\r\n"
    );

    forget_session(session);
}
