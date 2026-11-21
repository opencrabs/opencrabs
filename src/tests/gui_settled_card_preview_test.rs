//! A folded turn paints its head and its footer, never a preview band.
//!
//! #2984: the settled card painted a third row quoting the last call, which
//! wrapped to two or three lines per turn. The row, the `Turn::preview` field
//! and the single paint site that fed it are gone, and the assertion that
//! covered the field went with them in the same commit.
//!
//! The window has no headless harness, so the shape is read from source, the
//! way the other gui tests do it.

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");

/// The body of one top-level item, from its anchor to the next one. The cut
/// has to include the `pub(crate)` and `pub` forms: the file's own test module
/// is indented, so a bare `\nfn ` would run past the end of the item and read
/// assertions out of unrelated tests.
fn body_after<'a>(source: &'a str, anchor: &str) -> &'a str {
    let rest = source
        .split_once(anchor)
        .unwrap_or_else(|| panic!("{anchor} is gone from the source"))
        .1;
    let mut end = rest.len();
    for stop in ["\nfn ", "\npub fn ", "\npub(crate) fn ", "\n}"] {
        if let Some(at) = rest.find(stop) {
            end = end.min(at);
        }
    }
    rest.split_at(end).0
}

/// The field was what made the row possible, so it stays out of the turn.
#[test]
fn the_turn_carries_no_preview_field() {
    let turn = body_after(TRANSCRIPT, "pub struct Turn {");
    assert!(
        !turn.contains("preview"),
        "Turn must not carry a preview field again (#2984), got {turn:?}"
    );
}

/// The card paints the head, then the body only when the turn is open. A third
/// band under the head is the row #2984 removed.
#[test]
fn the_settled_card_paints_no_preview_band() {
    let card = body_after(TRANSCRIPT, "fn settled_card(");
    assert!(
        !card.contains("preview"),
        "the settled card must not read a preview again (#2984), got {card:?}"
    );
    assert!(
        !card.contains("\\u{2514}"),
        "the corner glyph that led the preview row must stay out of the card, got {card:?}"
    );
}

/// Removing the row must not remove the rows behind the chevron: the expanded
/// body is what still says what the turn did.
#[test]
fn the_settled_card_still_paints_the_expanded_body() {
    let card = body_after(TRANSCRIPT, "fn settled_card(");
    assert!(
        card.contains("let open = !body.is_empty();"),
        "the card must still ask whether the body is open, got {card:?}"
    );
    assert!(
        card.contains(".children(body),"),
        "the expanded body must still reach the card, got {card:?}"
    );
}
