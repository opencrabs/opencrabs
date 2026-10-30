//! Comprehensive tests for hashline editing functionality

use crate::brain::tools::hashline::edit::HashlineEditTool;
use crate::brain::tools::hashline::hash::{HASH_LEN, format_hashline, hash_line};
use crate::brain::tools::hashline::types::{HashRef, HashlineEditInput, HashlineEditOp};
use crate::brain::tools::read::ReadTool;
use crate::brain::tools::{Tool, ToolExecutionContext};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn test_hash_line_deterministic() {
    let hash1 = hash_line("test content");
    let hash2 = hash_line("test content");
    assert_eq!(hash1, hash2);
    assert_eq!(hash1.len(), HASH_LEN);
}

#[test]
fn test_hash_line_different_content() {
    let hash1 = hash_line("line one");
    let hash2 = hash_line("line two");
    assert_ne!(hash1, hash2);
}

#[test]
fn test_hash_line_same_content_same_hash() {
    // Pure content hashing: identical content always produces the same hash
    // regardless of position. Line-shift avalanche is handled at validation time.
    let hash1 = hash_line("same content");
    let hash2 = hash_line("same content");
    assert_eq!(hash1, hash2);
}

#[test]
fn test_hash_line_blank_lines_same_hash() {
    // Blank lines all produce the same hash (pure content, no line number)
    let hash1 = hash_line("");
    let hash2 = hash_line("");
    let hash3 = hash_line("");
    assert_eq!(hash1, hash2);
    assert_eq!(hash2, hash3);
    assert_eq!(hash1, hash3);
}

#[test]
fn test_format_hashline() {
    let formatted = format_hashline(12, "VKQMZP", "test code");
    assert_eq!(formatted, "VKQMZP|test code");
}

#[test]
fn test_hashref_parse_valid() {
    let href = HashRef::parse("42#RWSNZP").unwrap();
    assert_eq!(href.hash, "RWSNZP");
}

#[test]
fn test_hashref_parse_lowercase() {
    let href = HashRef::parse("10#txjbzp").unwrap();
    assert_eq!(href.hash, "TXJBZP");
}

#[test]
fn test_hashref_parse_with_content() {
    let href = HashRef::parse("5#TXJBZP|some code here").unwrap();
    assert_eq!(href.hash, "TXJBZP");
}

#[test]
fn test_hashref_parse_missing_separator() {
    // A line number glued to a hash with no `#` is the wrong length.
    let result = HashRef::parse("42RWSNZP");
    assert!(result.is_err());
}

#[test]
fn test_hashref_parse_invalid_line_ignored() {
    // Legacy format: line number is ignored, only hash matters
    let href = HashRef::parse("abc#RWSNZP").unwrap();
    assert_eq!(href.hash, "RWSNZP");
}

#[test]
fn test_hashref_parse_zero_line_ignored() {
    // Legacy format: line number is ignored, only hash matters
    let href = HashRef::parse("0#RWSNZP").unwrap();
    assert_eq!(href.hash, "RWSNZP");
}

#[test]
fn test_hashref_parse_wrong_hash_length() {
    // Built from a real tag and HASH_LEN, so a width change cannot turn one of
    // these into a valid ref by accident (#2202).
    let valid = hash_line("some line");
    let too_short = &valid[..HASH_LEN - 1];
    assert!(HashRef::parse(&format!("5#{too_short}")).is_err());
    let too_long = format!("{valid}Z");
    assert!(HashRef::parse(&format!("5#{too_long}")).is_err());
}

#[test]
fn test_hashline_edit_op_deserialize_replace() {
    let json = json!({
        "op": "replace",
        "pos": "5#VKQMZP",
        "lines": "new content"
    });
    let op: HashlineEditOp = serde_json::from_value(json).unwrap();
    match op {
        HashlineEditOp::Replace { pos, end, lines } => {
            assert_eq!(pos, "5#VKQMZP");
            assert!(end.is_none());
            assert_eq!(lines, "new content");
        }
        _ => panic!("Expected Replace"),
    }
}

#[test]
fn test_hashline_edit_op_deserialize_replace_range() {
    let json = json!({
        "op": "replace",
        "pos": "5#VKQMZP",
        "end": "8#MBTXJZ",
        "lines": "replacement"
    });
    let op: HashlineEditOp = serde_json::from_value(json).unwrap();
    match op {
        HashlineEditOp::Replace { pos, end, lines } => {
            assert_eq!(pos, "5#VKQMZP");
            assert_eq!(end, Some("8#MBTXJZ".to_string()));
            assert_eq!(lines, "replacement");
        }
        _ => panic!("Expected Replace"),
    }
}

#[test]
fn test_hashline_edit_op_deserialize_append() {
    let json = json!({
        "op": "append",
        "pos": "10#XYWQZP",
        "lines": "inserted line"
    });
    let op: HashlineEditOp = serde_json::from_value(json).unwrap();
    match op {
        HashlineEditOp::Append { pos, lines } => {
            assert_eq!(pos, Some("10#XYWQZP".to_string()));
            assert_eq!(lines, "inserted line");
        }
        _ => panic!("Expected Append"),
    }
}

#[test]
fn test_hashline_edit_op_deserialize_prepend() {
    let json = json!({
        "op": "prepend",
        "lines": "header line"
    });
    let op: HashlineEditOp = serde_json::from_value(json).unwrap();
    match op {
        HashlineEditOp::Prepend { pos, lines } => {
            assert!(pos.is_none());
            assert_eq!(lines, "header line");
        }
        _ => panic!("Expected Prepend"),
    }
}

#[test]
fn test_hashline_edit_input_deserialize() {
    let json = json!({
        "path": "test.rs",
        "edits": [
            {"op": "replace", "pos": "1#ABQWZP", "lines": "new"},
            {"op": "append", "pos": "5#CDQWZP", "lines": "added"}
        ]
    });
    let input: HashlineEditInput = serde_json::from_value(json).unwrap();
    assert_eq!(input.path, "test.rs");
    assert_eq!(input.edits.len(), 2);
}

#[tokio::test]
async fn test_hashline_edit_replace_single_line() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "line one\nline two\nline three\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    // Get hash for line 2
    let hash = hash_line("line two");
    let pos = format!("2#{}", hash);

    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": pos,
            "lines": "LINE TWO MODIFIED"
        }]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let content = std::fs::read_to_string(&file_path).unwrap();
    assert!(content.contains("LINE TWO MODIFIED"));
    assert!(!content.contains("line two"));
}

#[tokio::test]
async fn test_hashline_edit_replace_range() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "one\ntwo\nthree\nfour\nfive\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    let hash2 = hash_line("two");
    let hash4 = hash_line("four");

    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": format!("2#{}", hash2),
            "end": format!("4#{}", hash4),
            "lines": "REPLACED\nRANGE"
        }]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let content = std::fs::read_to_string(&file_path).unwrap();
    assert!(content.contains("REPLACED"));
    assert!(content.contains("RANGE"));
    assert!(!content.contains("two"));
    assert!(!content.contains("four"));
}

#[tokio::test]
async fn test_hashline_edit_append() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "line one\nline two\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    let hash = hash_line("line two");

    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [{
            "op": "append",
            "pos": format!("2#{}", hash),
            "lines": "appended line"
        }]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let content = std::fs::read_to_string(&file_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2], "appended line");
}

#[tokio::test]
async fn test_hashline_edit_prepend() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "line one\nline two\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    let hash = hash_line("line one");

    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [{
            "op": "prepend",
            "pos": format!("1#{}", hash),
            "lines": "prepended line"
        }]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let content = std::fs::read_to_string(&file_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "prepended line");
}

#[tokio::test]
async fn test_hashline_edit_hash_mismatch() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "line one\nline two\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    // Use a well-formed hash that is not present in the file.
    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": "1#ZZZZZZ",
            "lines": "new content"
        }]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(!result.success);
    assert!(result.error.unwrap().contains("not found in file"));
}

#[tokio::test]
async fn test_hashline_edit_overlapping_ranges() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "one\ntwo\nthree\nfour\nfive\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    let hash1 = hash_line("one");
    let hash3 = hash_line("three");
    let hash5 = hash_line("five");

    // Overlapping ranges: 1-3 and 3-5
    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            {
                "op": "replace",
                "pos": format!("1#{}", hash1),
                "end": format!("3#{}", hash3),
                "lines": "first"
            },
            {
                "op": "replace",
                "pos": format!("3#{}", hash3),
                "end": format!("5#{}", hash5),
                "lines": "second"
            }
        ]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(!result.success);
    assert!(result.error.unwrap().contains("Overlapping"));
}

#[tokio::test]
async fn test_hashline_edit_multiple_edits() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "one\ntwo\nthree\nfour\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    let hash1 = hash_line("one");
    let hash3 = hash_line("three");

    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [
            {
                "op": "replace",
                "pos": format!("1#{}", hash1),
                "lines": "ONE"
            },
            {
                "op": "replace",
                "pos": format!("3#{}", hash3),
                "lines": "THREE"
            }
        ]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let content = std::fs::read_to_string(&file_path).unwrap();
    assert!(content.contains("ONE"));
    assert!(content.contains("THREE"));
}

#[tokio::test]
async fn test_hashline_edit_strip_prefix_autocorrect() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "line one\nline two\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    let hash = hash_line("line one");

    // Include hashline prefix in the replacement (model mistake)
    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": format!("1#{}", hash),
            "lines": format!("1#{}|NEW CONTENT", hash)
        }]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let content = std::fs::read_to_string(&file_path).unwrap();
    // Should strip the prefix and only contain "NEW CONTENT"
    assert!(content.contains("NEW CONTENT"));
    assert!(!content.contains(format!("1#{}", hash).as_str()));
}

#[tokio::test]
async fn test_hashline_edit_strips_current_read_prefix() {
    // The real "edit mangled my file" case (#573): read_file hashline=true
    // prints lines as `HASH|content`, and models echo that exact form back
    // into the replacement. The bare `HASH|` prefix must be stripped, or it
    // lands verbatim in the file.
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "line one\nline two\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    let hash = hash_line("line one");
    let echoed = format!("{hash}|NEW CONTENT"); // exactly what the read prints

    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": hash.clone(),
            "lines": echoed,
        }]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let content = std::fs::read_to_string(&file_path).unwrap();
    assert!(content.contains("NEW CONTENT"));
    assert!(
        !content.contains(&format!("{hash}|")),
        "the echoed HASH| prefix leaked into the file: {content:?}"
    );
}

#[tokio::test]
async fn test_hashline_edit_keeps_markdown_pipe_content() {
    // A real content line that merely contains an early `|` (markdown table)
    // must NOT be treated as a hash prefix and truncated.
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.md");
    std::fs::write(&file_path, "header\nrow\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    let hash = hash_line("row");
    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": hash,
            "lines": "| a | b |",
        }]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let content = std::fs::read_to_string(&file_path).unwrap();
    assert!(
        content.contains("| a | b |"),
        "markdown table row was mangled: {content:?}"
    );
}

#[tokio::test]
async fn test_read_file_hashline_mode() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "line one\nline two\nline three\n").unwrap();

    let tool = ReadTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf());

    let input = json!({
        "path": file_path.to_str().unwrap(),
        "hashline": true
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let output = result.output;
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 3);

    // Check format: ID|content (no line numbers)
    assert!(lines[0].contains("|line one"));
    assert!(lines[1].contains("|line two"));
    assert!(lines[2].contains("|line three"));
    // Each line should start with a HASH_LEN-char hash followed by |
    for line in &lines {
        assert!(
            line.len() > HASH_LEN && line.as_bytes()[HASH_LEN] == b'|',
            "Expected format ID|content, got: {}",
            line
        );
    }
}

#[tokio::test]
async fn test_read_file_hashline_default_false() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "line one\nline two\n").unwrap();

    let tool = ReadTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf());

    let input = json!({
        "path": file_path.to_str().unwrap()
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let output = result.output;
    // Should NOT contain hashline format
    assert!(!output.contains("#"));
    assert!(!output.contains("|"));
    assert_eq!(output.trim(), "line one\nline two");
}

#[tokio::test]
async fn test_read_file_hashline_with_start_line() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    std::fs::write(&file_path, "one\ntwo\nthree\nfour\nfive\n").unwrap();

    let tool = ReadTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf());

    let input = json!({
        "path": file_path.to_str().unwrap(),
        "start_line": 2,
        "line_count": 2,
        "hashline": true
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let output = result.output;
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 2);

    // Should contain the requested lines in ID|content format
    assert!(lines[0].contains("|three"));
    assert!(lines[1].contains("|four"));
    // Each line should start with a HASH_LEN-char hash followed by |
    for line in &lines {
        assert!(
            line.len() > HASH_LEN && line.as_bytes()[HASH_LEN] == b'|',
            "Expected format ID|content, got: {}",
            line
        );
    }
}

#[tokio::test]
async fn test_hashline_edit_collision_rejected() {
    // Test that when the same hash appears on multiple lines, the tool rejects
    // and tells the LLM to use edit_file instead
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    // Two identical lines that will produce the same hash
    std::fs::write(&file_path, "same content\nsame content\n").unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    let hash = hash_line("same content");

    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": format!("1#{}", hash),
            "lines": "new content"
        }]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(!result.success);
    let error = result.error.unwrap();
    assert!(error.contains("collision"));
    assert!(error.contains("edit_file"));
}

#[tokio::test]
async fn test_read_file_hashline_collision_detection() {
    // Test that when reading with hashline mode, lines with collisions are marked
    // with #COLLISION and a warning is added
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("test.txt");
    // Two identical lines that will produce the same hash
    std::fs::write(&file_path, "same content\ndifferent\nsame content\n").unwrap();

    let tool = ReadTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf());

    let input = json!({
        "path": file_path.to_str().unwrap(),
        "hashline": true
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(result.success);

    let output = result.output;
    let lines: Vec<&str> = output.lines().collect();

    // Should have 3 content lines + 1 empty line + 1 warning line = 5 lines
    assert_eq!(lines.len(), 5);

    // First line should be marked as COLLISION
    assert!(lines[0].starts_with("COLLISION|same content"));

    // Second line should have a normal hash (tag + |content)
    assert!(!lines[1].contains("COLLISION"));
    assert!(lines[1].contains("|different"));

    // Third line should also be marked as COLLISION
    assert!(lines[2].starts_with("COLLISION|same content"));

    // Fourth line should be empty
    assert_eq!(lines[3], "");

    // Fifth line should be the warning
    assert!(lines[4].contains("WARNING"));
    assert!(lines[4].contains("collision"));
    assert!(lines[4].contains("edit_file"));
}

// --- #2201: the invalid-hash error has to show a real tag ---

#[tokio::test]
async fn test_invalid_hash_ref_error_shows_real_tags_from_the_file() {
    let temp_dir = TempDir::new().unwrap();
    let file_path = temp_dir.path().join("sample.txt");
    let body = "fn alpha() {\n    let one = 1;\n}\n\nfn beta() {\n    let two = 2;\n}\n";
    std::fs::write(&file_path, body).unwrap();

    let tool = HashlineEditTool;
    let context = ToolExecutionContext::new(Uuid::new_v4())
        .with_working_directory(temp_dir.path().to_path_buf())
        .with_auto_approve(true);

    // The logged failure shape: a whole source line pasted where a tag belongs.
    let input = json!({
        "path": file_path.to_str().unwrap(),
        "edits": [{
            "op": "replace",
            "pos": "fn alpha() {",
            "lines": "fn alpha() {}"
        }]
    });

    let result = tool.execute(input, &context).await.unwrap();
    assert!(!result.success, "a source line must not parse as a tag");

    // A failed ToolResult carries its message in `error`, not `output`
    // (ToolResult::error leaves output empty), and that is the string the
    // model is shown, so assert against it.
    let out = result
        .error
        .clone()
        .unwrap_or_else(|| result.output.clone());
    assert!(
        out.contains("read_file") && out.contains("hashline=true"),
        "error must name where a tag comes from: {out}"
    );

    // At least two real tags from this file, in the exact printed form.
    let mut real_tags = 0;
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let expected = format!("{}|", hash_line(line));
        if out.contains(&expected) {
            real_tags += 1;
        }
    }
    assert!(
        real_tags >= 2,
        "error must show at least two real tags from the file, found {real_tags}: {out}"
    );
}
