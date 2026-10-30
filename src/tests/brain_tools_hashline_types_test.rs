use crate::brain::tools::hashline::hash::HASH_LEN;
use crate::brain::tools::hashline::types::*;

/// A tag of the current width built from a readable base. Written this way so
/// the parse tests survive a HASH_LEN change instead of hardcoding a length
/// that goes stale the next time the tag is widened (#2202).
fn tag(base: &str) -> String {
    let mut s = base.to_string();
    while s.len() < HASH_LEN {
        s.push('Z');
    }
    s.truncate(HASH_LEN);
    s
}

#[test]
fn test_parse_hashref_valid() {
    let expected = tag("VKQM");
    let hr = HashRef::parse(&format!("12#{expected}")).unwrap();
    assert_eq!(hr.hash, expected);
}

#[test]
fn test_parse_hashref_bare_hash() {
    // The current read format references lines by hash alone.
    let expected = tag("VKQM");
    let hr = HashRef::parse(&expected).unwrap();
    assert_eq!(hr.hash, expected);
    let hr = HashRef::parse(&format!("#{expected}")).unwrap();
    assert_eq!(hr.hash, expected);
}

#[test]
fn test_parse_hashref_single_digit() {
    let expected = tag("ZPMQ");
    let hr = HashRef::parse(&format!("1#{expected}")).unwrap();
    assert_eq!(hr.hash, expected);
}

#[test]
fn test_parse_hashref_large_line() {
    let expected = tag("RWSN");
    let hr = HashRef::parse(&format!("1234#{expected}")).unwrap();
    assert_eq!(hr.hash, expected);
}

#[test]
fn test_parse_hashref_with_pipe_content() {
    // Model might include the content after the pipe
    let expected = tag("TXJB");
    let hr = HashRef::parse(&format!("5#{expected}|some code here")).unwrap();
    assert_eq!(hr.hash, expected);
}

#[test]
fn test_parse_hashref_lowercase_uppercased() {
    let expected = tag("VKQM");
    let hr = HashRef::parse(&format!("3#{}", expected.to_lowercase())).unwrap();
    assert_eq!(hr.hash, expected);
}

#[test]
fn test_parse_hashref_missing_separator() {
    // A line number glued to a hash with no `#` is the wrong length.
    assert!(HashRef::parse(&format!("12{}", tag("VKQM"))).is_err());
}

#[test]
fn test_parse_hashref_invalid_line_ignored() {
    // Legacy format: line number is ignored, only hash matters
    let expected = tag("VKQM");
    let hr = HashRef::parse(&format!("abc#{expected}")).unwrap();
    assert_eq!(hr.hash, expected);
}

#[test]
fn test_parse_hashref_zero_line_ignored() {
    // Legacy format: line number is ignored, only hash matters
    let expected = tag("VKQM");
    let hr = HashRef::parse(&format!("0#{expected}")).unwrap();
    assert_eq!(hr.hash, expected);
}

#[test]
fn test_parse_hashref_wrong_hash_length() {
    // Too short and too long both rejected (hash is exactly HASH_LEN chars).
    // Derived from `tag`, never written out, so a width change cannot make
    // the "too short" case accidentally valid (#2202).
    let too_short = &tag("VKQM")[..HASH_LEN - 1];
    assert!(
        HashRef::parse(&format!("5#{too_short}")).is_err(),
        "a {}-char tag must be rejected",
        HASH_LEN - 1
    );
    let too_long = format!("{}Z", tag("VKQM"));
    assert!(HashRef::parse(&format!("5#{too_long}")).is_err());
}

#[test]
fn test_parse_hashref_error_names_the_source_of_the_tag() {
    // The dominant hashline_edit failure is a source line pasted where a tag
    // belongs. The error has to say where tags come from, not just a length.
    let err = HashRef::parse("const MAX_BODY_BYTES: usize = 10_000;").unwrap_err();
    assert!(
        err.contains("hashline=true"),
        "error must name read_file hashline=true: {err}"
    );
    assert!(
        err.contains("before the `|`"),
        "error must name where the tag sits on the line: {err}"
    );
}

#[test]
fn test_deserialize_replace_op() {
    let json = serde_json::json!({
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
fn test_deserialize_replace_range() {
    let json = serde_json::json!({
        "op": "replace",
        "pos": "5#VKQMZP",
        "end": "8#MBTXJZ",
        "lines": "replacement"
    });
    let op: HashlineEditOp = serde_json::from_value(json).unwrap();
    match op {
        HashlineEditOp::Replace { pos, end, lines } => {
            assert_eq!(pos, "5#VKQMZP");
            assert_eq!(end.unwrap(), "8#MBTXJZ");
            assert_eq!(lines, "replacement");
        }
        _ => panic!("Expected Replace"),
    }
}

#[test]
fn test_deserialize_append() {
    let json = serde_json::json!({
        "op": "append",
        "pos": "10#XYWQZP",
        "lines": "inserted line"
    });
    let op: HashlineEditOp = serde_json::from_value(json).unwrap();
    match op {
        HashlineEditOp::Append { pos, lines } => {
            assert_eq!(pos.unwrap(), "10#XYWQZP");
            assert_eq!(lines, "inserted line");
        }
        _ => panic!("Expected Append"),
    }
}

#[test]
fn test_deserialize_prepend() {
    let json = serde_json::json!({
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
fn test_deserialize_full_input() {
    let json = serde_json::json!({
        "path": "src/main.rs",
        "edits": [
            { "op": "replace", "pos": "1#VKQMZP", "lines": "new line 1" },
            { "op": "append", "pos": "5#MBTXJZ", "lines": "inserted" }
        ]
    });
    let input: HashlineEditInput = serde_json::from_value(json).unwrap();
    assert_eq!(input.path, "src/main.rs");
    assert_eq!(input.edits.len(), 2);
}
