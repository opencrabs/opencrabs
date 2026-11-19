//! What a long prompt does in the row (#3064).
//!
//! Three decisions, all pure: whether a body is long enough to fold, what the
//! row keeps of it, and what the card that holds the rest is called. The
//! drawing is in the GUI binary; these are the rules behind it.
//!
//! The promise #3053 made still holds: no line is lost. The body is stored
//! whole, copy and edit read all of it, and the card opens all of it in the
//! document reader. Only the row's drawing is bounded.

use crate::ui_core::message_reader::{
    FOLD_AFTER_LINES, FOLD_BYTES, FOLD_KEEP_BYTES, FOLD_KEEP_LINES, NAME_CHARS, TITLE_CHARS, fold,
    opens_in_reader, title,
};

#[test]
fn a_short_message_keeps_its_click_free() {
    assert!(!opens_in_reader("one line"));
    assert!(!opens_in_reader(""));
    // A message that is long in characters but short in lines still reads in
    // the column: wrapping is the column's job, and this one stays under the
    // byte bound that catches a single unbroken wall.
    let one_wrapped_line = "word ".repeat(500);
    assert!(
        !opens_in_reader(&one_wrapped_line),
        "2.5 KB on one line still reads in the column"
    );
    assert!(fold("one line").is_none());
}

#[test]
fn the_threshold_is_the_first_body_that_stops_reading_well() {
    let at_limit = vec!["line"; FOLD_AFTER_LINES].join("\n");
    assert!(
        !opens_in_reader(&at_limit),
        "{FOLD_AFTER_LINES} lines still read in the column"
    );
    let past_limit = vec!["line"; FOLD_AFTER_LINES + 1].join("\n");
    assert!(
        opens_in_reader(&past_limit),
        "{} lines fold",
        FOLD_AFTER_LINES + 1
    );
}

#[test]
fn one_unbroken_line_past_the_byte_bound_folds_too() {
    // The line count alone leaves this case untouched: one 5 KB line is one
    // line, and it costs the frame exactly what a thousand short ones cost.
    let wall = "word ".repeat(FOLD_BYTES / 4);
    assert!(wall.len() > FOLD_BYTES, "the fixture is past the bound");
    assert_eq!(wall.lines().count(), 1, "the fixture is one line");
    assert!(opens_in_reader(&wall), "a single wall folds on bytes");
    let folded = fold(&wall).expect("it folds");
    assert!(
        folded.head.len() <= FOLD_KEEP_BYTES + 4,
        "the head is bounded"
    );
    assert!(
        folded.head.chars().count() > 100,
        "the head is a real opening sentence, not one word"
    );
}

#[test]
fn a_long_prompt_folds_to_its_opening_lines() {
    // The whole contract in one place: a body past the bound keeps a bounded
    // head, names a document, and says how much sits behind the card; a body
    // under the bound takes no fold at all and keeps its click free.
    let body = (0..200)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let folded = fold(&body).expect("200 lines fold");
    assert_eq!(folded.head.lines().count(), FOLD_KEEP_LINES);
    assert!(folded.name.ends_with(".md"), "the card names a markdown file");
    assert!(
        folded.summary.contains("200 lines"),
        "the card says how much the fold hides"
    );
    assert!(fold("three\nshort\nlines").is_none());
}

#[test]
fn the_head_keeps_the_opening_lines() {
    let body = (0..200)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let folded = fold(&body).expect("200 lines fold");
    let head: Vec<&str> = folded.head.lines().collect();
    assert_eq!(head.len(), FOLD_KEEP_LINES, "the head keeps its 20 lines");
    assert_eq!(head[0], "line 0", "and keeps them from the start");
    assert_eq!(head[FOLD_KEEP_LINES - 1], "line 19");
    assert!(
        !folded.head.ends_with('\n'),
        "the head never ends on a blank line"
    );
}

#[test]
fn the_head_cuts_on_a_character_boundary() {
    // Two-byte characters across the byte bound: a byte slice here is #2271,
    // and the panic would land on the first accented line of a pasted log.
    // Three times the byte bound, so the fixture is unambiguously past it:
    // two times lands exactly on `FOLD_BYTES`, which does not fold.
    let body = "\u{e1}".repeat(FOLD_KEEP_BYTES * 3);
    let folded = fold(&body).expect("the body folds");
    assert!(
        folded.head.len() <= FOLD_KEEP_BYTES + 4,
        "the head stays within the byte bound, got {}",
        folded.head.len()
    );
    assert!(
        folded.head.chars().all(|c| c == '\u{e1}'),
        "every character survived the cut"
    );
}

#[test]
fn the_card_names_the_document_after_its_opening_words() {
    let body = vec!["still logging all this shit"; 40].join("\n");
    let folded = fold(&body).expect("the body folds");
    assert_eq!(folded.name, "still-logging-all-this-shit.md");

    // Leading blank lines are how a pasted block starts; the name skips them.
    let padded = format!("\n\n  \n{}", vec!["deploy log"; 40].join("\n"));
    assert_eq!(fold(&padded).expect("the body folds").name, "deploy-log.md");

    // Punctuation and separators become one dash, never a run of them.
    let punctuated = format!("\"quoted\" (paren) / slash\n{}", vec!["x"; 40].join("\n"));
    assert_eq!(
        fold(&punctuated).expect("the body folds").name,
        "quoted-paren-slash.md"
    );
}

#[test]
fn a_document_name_stays_short() {
    let body = format!("{}\n{}", "a".repeat(120), vec!["x"; 40].join("\n"));
    let folded = fold(&body).expect("the body folds");
    assert_eq!(folded.name, format!("{}.md", "a".repeat(NAME_CHARS)));
}

#[test]
fn the_card_says_how_much_sits_behind_it() {
    let body = vec!["x"; 2000].join("\n");
    let folded = fold(&body).expect("the body folds");
    assert_eq!(
        folded.summary, "2000 lines \u{00b7} 3 KB",
        "the card prints the line count and the size"
    );
}

#[test]
fn the_title_is_the_first_line_that_carries_text() {
    assert_eq!(title("first line\nsecond line"), "first line");
    // Leading blank lines are what a pasted block starts with; the title skips
    // them instead of printing an empty header.
    assert_eq!(
        title("\n\n   \n  the real opening\nthen more"),
        "the real opening"
    );
    assert_eq!(title(""), "message");
    assert_eq!(title("   \n\t\n"), "message");
}

#[test]
fn the_title_is_cut_on_a_character_boundary() {
    let long = "\u{e1}".repeat(TITLE_CHARS + 20);
    let fitted = title(&long);
    assert!(
        fitted.chars().count() <= TITLE_CHARS,
        "the title stays within {TITLE_CHARS} characters, got {}",
        fitted.chars().count()
    );
    assert!(fitted.ends_with('\u{2026}'), "a cut title says it was cut");
    // A body that fits is printed whole, with no ellipsis to imply otherwise.
    let short = "curto mas completo";
    assert_eq!(title(short), short);
}
