//! Reply with the picked rows quoted into the same session (#2827).
//!
//! A forward refuses the session it came from, and rightly: a copy of a message
//! next to the message is noise. A reply is the other shape, the picks become
//! context for something new the reader is about to say, so the quote has to
//! live in the outgoing text itself. The window has no headless harness, so the
//! wiring is read from source, the way the other gui tests do it.

use crate::tests::gui_dashboard_source::DASHBOARD;

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");

/// The quote is written into the field, not held in a slot beside it. What the
/// user sees before pressing Enter is what the model reads, and the row keeps
/// it without a second column to carry it.
#[test]
fn the_quote_goes_into_the_composer_as_text() {
    let reply = DASHBOARD
        .split("fn reply_rows(")
        .nth(1)
        .expect("both Reply entry points quote through one function")
        .split("/// The rows the current picks stand for")
        .next()
        .unwrap_or_default();
    assert!(
        reply.contains("transcript::quote_prefix(&rows)"),
        "the picks are turned into a quote"
    );
    assert!(
        reply.contains("editor.restore_draft(text, cx)"),
        "and the quote lands in the composer as its text"
    );
    assert!(
        reply.contains("self.msg_select.cancel()"),
        "the picks are spent once they are quoted"
    );
    assert!(
        !reply.contains("composer.quote"),
        "there is no second slot holding a quote the field cannot show"
    );
    // The field's text is the turn's text, so the quote reaches the send path
    // with no argument added anywhere along it.
    assert!(
        DASHBOARD.contains("self.send_from_pane(session, text, display, SendOrigin::Window, cx)"),
        "the composer's own text is what send_from_pane is handed"
    );
}

/// A quote is a blockquote: every line carries the marker, which is what lets
/// the bubble tell the quote from the reply under it.
#[test]
fn every_quoted_line_carries_the_marker() {
    let builder = TRANSCRIPT
        .split("pub fn quote_prefix(")
        .nth(1)
        .expect("one builder writes the quote")
        .split("fn split_quote(")
        .next()
        .unwrap_or_default();
    assert!(
        builder.contains("out.push_str(\"> \")"),
        "each line is marked as quoted"
    );
    assert!(
        builder.contains("row.content.trim_end().split('\\n')"),
        "a multi-line row is quoted line by line, so the block stays a block"
    );
}

/// The bar offers it next to Forward, which is where a reader who has just
/// picked rows looks for what to do with them.
#[test]
fn the_bar_offers_reply_next_to_forward() {
    let bar = DASHBOARD
        .split("pub(crate) fn select_bar(")
        .nth(1)
        .expect("the selection bar is one function")
        .split("pub(crate) fn forward_picker(")
        .next()
        .unwrap_or_default();
    let reply = bar
        .find("\"message-select-reply\"")
        .expect("the bar draws a Reply button");
    let forward = bar
        .find("\"message-select-forward\"")
        .expect("the bar still draws Forward");
    assert!(reply < forward, "Reply sits beside Forward, not behind it");
    assert!(
        bar.contains("this.msg_reply_begin(cx)"),
        "and the button is wired to it"
    );
}

/// Only a user row reads its opening blockquote as a quote. An answer's own
/// markdown blockquote already renders through the markdown path, and lifting
/// it out would change every answer that quotes anything.
#[test]
fn only_a_user_row_lifts_the_quote_out() {
    let bubble = TRANSCRIPT
        .split("fn bubble(")
        .nth(1)
        .expect("one bubble renderer")
        .split("fn forward_block(")
        .next()
        .unwrap_or_default();
    assert!(
        bubble.contains("let (quote, text) = if role == \"user\""),
        "the quote is lifted for the user role only"
    );
    assert!(
        bubble.contains("col = col.child(quote_block(&quote))"),
        "and it is drawn above the reply"
    );
    let split = TRANSCRIPT
        .split("fn split_quote(")
        .nth(1)
        .expect("one splitter reads the markers")
        .split("fn quote_block(")
        .next()
        .unwrap_or_default();
    assert!(
        split.contains("return (Vec::new(), text)"),
        "a row with no quote opens with its own text, exactly as before"
    );
}
