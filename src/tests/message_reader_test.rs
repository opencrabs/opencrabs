//! When a message the reader typed opens in the document reader (#3053).
//!
//! Two decisions, both pure: whether a body is long enough to bother opening,
//! and what the reader's header calls it. The drawing is in the GUI binary;
//! these are the rules that decide what it draws.

use opencrabs::ui_core::message_reader::{READER_LINES, TITLE_CHARS, opens_in_reader, title};

#[test]
fn a_short_message_keeps_its_click_free() {
    assert!(!opens_in_reader("one line"));
    assert!(!opens_in_reader(""));
    // A message that is long in characters but short in lines still reads in
    // the column: the count is lines, because wrapping is the column's job.
    let one_wrapped_line = "word ".repeat(500);
    assert!(
        !opens_in_reader(&one_wrapped_line),
        "a single long line is one line"
    );
}

#[test]
fn the_threshold_is_the_first_body_that_stops_reading_well() {
    let at_limit = vec!["line"; READER_LINES].join("\n");
    assert!(
        !opens_in_reader(&at_limit),
        "{READER_LINES} lines still read in the column"
    );
    let past_limit = vec!["line"; READER_LINES + 1].join("\n");
    assert!(
        opens_in_reader(&past_limit),
        "{} lines open in the reader",
        READER_LINES + 1
    );
}

#[test]
fn the_title_is_the_first_line_that_carries_text() {
    assert_eq!(title("first line\nsecond line"), "first line");
    // Leading blank lines are what a pasted block starts with; the title skips
    // them instead of printing an empty header.
    assert_eq!(title("\n\n   \n  the real opening\nthen more"), "the real opening");
    assert_eq!(title(""), "message");
    assert_eq!(title("   \n\t\n"), "message");
}

#[test]
fn the_title_is_cut_on_a_character_boundary() {
    let long = "á".repeat(TITLE_CHARS + 20);
    let fitted = title(&long);
    assert!(
        fitted.chars().count() <= TITLE_CHARS,
        "the title stays within {TITLE_CHARS} characters, got {}",
        fitted.chars().count()
    );
    assert!(fitted.ends_with('…'), "a cut title says it was cut");
    // A body that fits is printed whole, with no ellipsis to imply otherwise.
    let short = "curto mas completo";
    assert_eq!(title(short), short);
}
