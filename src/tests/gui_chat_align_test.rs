//! Every row starts at the left margin, and the pane chrome clears its ring.
//!
//! The window has no headless harness, so the shape is read from source, the
//! way the other gui tests do it. Three defects are guarded here:
//!
//! - a row pushed to its end lost the start of its own line when it was wider
//!   than the pane, so the body that wraps the bubble is a block box that hands
//!   it the whole column (#2981);
//! - the split-pane title carried no weight and both chrome rows sat inside
//!   the 24 px corner arc;
//! - the chat surface had no corner, so inside a pane its square fill painted
//!   over the ring.
//!
//! The bubble stays narrower than the cards around it: `MESSAGE_RADIUS` is
//! asserted against `CARD_RADIUS` in `transcript.rs`, and nothing here widens
//! it.

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const PANE_VIEW: &str = include_str!("../gui/pane_view.rs");
const PANE_CHROME: &str = include_str!("../gui/pane_chrome.rs");

/// The body of one top-level function, from its anchor to the next top-level
/// item. The cut has to include the `pub(crate)` and `pub` forms: the file's
/// own test module is indented, so a bare `\nfn ` would run past the end of the
/// function and read assertions out of unrelated tests.
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
    // Every candidate is a byte offset returned by `find`, or `rest.len()`, so
    // `end` always lands on a char boundary. `split_at` says that once instead
    // of an index expression the byte-slice ratchet has to re-litigate.
    rest.split_at(end).0
}

/// Every stored row starts at the left margin. The body that wraps the bubble
/// is a block box, so it hands the bubble the whole column: a flex row pushed
/// to its end instead put the excess out of the left edge and left the reader
/// scrolling past blank height before the text (#2981). The row is not told
/// whose message it draws, because both voices take the same shape.
#[test]
fn every_stored_row_starts_at_the_left_margin() {
    let builder = body_after(TRANSCRIPT, "fn message_row(");
    assert!(
        !builder.contains("mine: bool"),
        "the row is not told whose message it draws, because both take one shape"
    );
    assert!(
        !builder.contains("justify_end()"),
        "nothing is pushed to the row's end, so the body keeps the left edge"
    );
    assert!(
        builder.contains(".flex_1()") && builder.contains(".min_w_0()"),
        "the body hands the bubble the whole column and still shrinks"
    );
}

/// A live turn is drawn by a second builder. If only the stored row changed
/// shape, the bubble would jump sideways the moment the turn persisted, so the
/// live body takes the same block box the stored row does (#2981).
#[test]
fn the_live_turn_sits_where_the_stored_row_will() {
    let builder = body_after(TRANSCRIPT, "fn live_user_row(\n    slot: LiveSlot,");
    assert_eq!(
        builder.matches(".justify_end()").count(),
        0,
        "the live body is a block box, so the sent turn keeps the left margin"
    );
    assert!(
        builder.contains(".flex_1()") && builder.contains(".min_w_0()"),
        "and it takes the whole column, the same body the stored row uses"
    );
}

/// The user bubble is a horizontal flex item, so it must shrink to the real
/// row width. Attachment height is protected by the strip's own shrink guard.
/// The column declares no width of its own (#2981): the wrapper above it is
/// what carries the definite width the text wraps at.
#[test]
fn the_bubble_shrinks_horizontally_without_shrinking_attachments() {
    let column = body_after(TRANSCRIPT, "fn bubble(\n    role: &str,")
        .split("if bubble_has_surface")
        .next()
        .unwrap_or_default();
    assert!(
        !column.contains(".flex_shrink_0()"),
        "the bubble can shrink below a planned width that exceeds its row"
    );
    assert!(
        !column.contains(".w_full()"),
        "the column declares no width, so the wrapper's definite one wins"
    );
    assert!(
        column.contains(".min_w_0()"),
        "and it still shrinks instead of running past the pane"
    );
    let strip = TRANSCRIPT
        .split("let mut strip = div()")
        .nth(1)
        .and_then(|rest| rest.split(".children(tiles)").next())
        .expect("the attachment strip is present");
    assert!(
        strip.contains(".flex_shrink_0()"),
        "the attachment strip keeps its planned height"
    );
}

/// GPUI wraps text only with a known or definite width. Every wrapper between
/// the transcript and a text line must preserve that width, including the
/// otherwise invisible drag-selection row (#2947).
#[test]
fn the_text_width_stays_definite_through_the_wrappers() {
    // `md_block` is a delegate now: the width lives on the block list it
    // builds, so the wrapper that carries it is `md_parsed_blocks` (#3019).
    for body in [
        body_after(TRANSCRIPT, "fn selectable_row<E: gpui::IntoElement>("),
        body_after(TRANSCRIPT, "fn message_row("),
        body_after(TRANSCRIPT, "fn md_parsed_blocks("),
        body_after(TRANSCRIPT, "fn plain_block("),
    ] {
        assert!(
            body.contains(".w_full()"),
            "the wrapper takes the row width"
        );
        assert!(body.contains(".min_w_0()"), "the wrapper can shrink");
    }
    let line = body_after(TRANSCRIPT, "fn md_line(")
        .split("let base = div()")
        .nth(1)
        .and_then(|rest| rest.split("if l.heading").next())
        .expect("the markdown line has a content wrapper");
    assert!(line.contains(".w_full()"));
    assert!(line.contains(".min_w_0()"));
}

/// The chat surface carries the card radius. It must not clip: the element is
/// already the scroller, and gpui's style setters are last-write-wins, so a
/// later clip would replace the scroll and freeze the transcript.
#[test]
fn the_chat_surface_is_rounded_and_still_scrolls() {
    let column = body_after(TRANSCRIPT, ".id(\"transcript\")")
        .split("if let Some(wheel)")
        .next()
        .unwrap_or_default();
    assert!(
        column.contains(".rounded(px(CARD_RADIUS))"),
        "the chat surface carries the same corner as the cards"
    );
    assert!(
        column.contains(".overflow_y_scroll()"),
        "the transcript still scrolls"
    );
    assert!(
        !column.contains(".overflow_hidden()"),
        "a clip here would replace the scroll, because the setters are last-write-wins"
    );
}

/// The pane paints its own fill and clips its children, so no child can paint
/// a square corner over the ring's arc.
#[test]
fn the_pane_clips_its_children_to_the_ring() {
    assert_eq!(
        PANE_VIEW.matches(".rounded(px(CARD_RADIUS))").count(),
        1,
        "one pane, one ring"
    );
    let ring = PANE_VIEW
        .find(".rounded(px(CARD_RADIUS))")
        .expect("the pane keeps its ring");
    let fill = PANE_VIEW
        .find(".bg(rgb(palette::current().bg))")
        .expect("the pane paints its own fill");
    assert!(fill < ring, "the fill sits on the rounded box");
    // The chrome box above the ring clips its own strips (#2980), so the first
    // `.overflow_hidden()` in the file is no longer the pane's. The clip that
    // rounds the corner is the one that follows the ring.
    let tail = PANE_VIEW.split_at(ring).1;
    assert!(
        tail.contains(".overflow_hidden()"),
        "the clip comes after the ring in the chain"
    );
}

/// The pane title is the pane's name, so it carries a heading's weight instead
/// of reading as one more line of body text.
#[test]
fn the_pane_title_carries_weight() {
    let header = body_after(PANE_CHROME, "pub(crate) fn pane_header(");
    assert!(
        header.contains(".font_weight(gpui::FontWeight::BOLD)"),
        "the title is bold"
    );
}

/// Both chrome rows step in from the corner: at 8 px of padding the 24 px arc
/// reaches the first character. The close glyph keeps its own padding, so the
/// button does not grow with the row.
#[test]
fn both_chrome_rows_clear_the_corner() {
    assert_eq!(
        PANE_CHROME.matches(".px_3()").count(),
        2,
        "the title row and the footer row both step in"
    );
    assert_eq!(
        PANE_CHROME.matches(".py_2()").count(),
        2,
        "and both step down"
    );
    assert_eq!(
        PANE_CHROME.matches(".px_2()").count(),
        1,
        "the close glyph keeps its own padding"
    );
}

/// A percentage cap is ignored while the row's width is indefinite, which is
/// exactly the narrow split pane: the bubble then sized to max-content and
/// `justify_end` pushed its beginning out of the left edge (#2937). The column
/// therefore declares no width of its own, the wrapper above it supplies the
/// definite one, and the only cap is a pixel one the caller passes (#2976,
/// #2978). A full-width declaration on the column was tried in #2979 and
/// reverted in #2981: a percentage width resolves against a parent whose width
/// is still indefinite while the row is measured, so the row wrapped far taller
/// than its text.
#[test]
fn the_bubble_is_capped_in_pixels() {
    let bubble = body_after(TRANSCRIPT, "fn bubble(");
    assert!(
        !bubble.contains(".w_full()"),
        "the bubble column takes its content width, never the row's"
    );
    assert!(
        bubble.contains(".min_w_0()"),
        "and it can still shrink below a planned width that exceeds its row"
    );
    assert!(
        bubble.contains(".when(row_cap > 0.0, |d| d.max_w(px(row_cap)))"),
        "the only cap is the pixel one the caller passes"
    );
    let code = bubble
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("max_w_full"),
        "a percentage max would overwrite the pixel cap on the same field"
    );
}

/// Every row fills the transcript (#2979). The two thirds cap that #2969 added
/// is gone: in a single pane it measured the bubble against a width the row
/// never had, so the column collapsed to a few pixels and the text wrapped one
/// glyph per line, while the same row in a split, which took no cap, drew
/// correctly (#2978). One rule now, and it is the box the row is painted into.
#[test]
fn every_row_fills_the_transcript() {
    assert!(
        !TRANSCRIPT.contains("MESSAGE_MAX_FRACTION"),
        "the two thirds cap is gone, not retuned"
    );
    let helper = body_after(TRANSCRIPT, "fn message_max_width(");
    assert!(
        helper.contains("content_width.max(0.0)"),
        "a row takes the whole column it is painted into"
    );
    let view = body_after(TRANSCRIPT, "pub fn view(");
    assert!(
        view.contains("let row_cap = message_max_width(content_width);"),
        "the rows are handed the column itself, with no flag deciding otherwise"
    );
    for (name, anchor) in [
        ("the bubble", "fn bubble("),
        ("the forwarded row", "fn forward_block("),
        ("the settled card", "fn settled_card("),
        ("the live card", "fn live_card("),
    ] {
        let body = body_after(TRANSCRIPT, anchor);
        assert!(
            body.contains(".when(row_cap > 0.0, |d| d.max_w(px(row_cap)))")
                || body.contains(".when(max_width > 0.0, |d| d.max_w(px(max_width)))"),
            "{name} takes the width it is given, and no max when there is none"
        );
    }
    let plan = body_after(TRANSCRIPT, "fn block_view(");
    assert!(
        plan.contains(".when(row_cap > 0.0, |d| d.max_w(px(row_cap)))"),
        "the finished plan card takes the same column as the rows beside it"
    );
}

/// The planned width is the transcript's outer measure, but its root spends a
/// half rem on each horizontal side. The bubble must inherit the remaining
/// inner width or right alignment pushes that excess off the left (#2947,
/// #3114).
#[test]
fn the_width_cap_excludes_the_transcript_padding() {
    let view = body_after(TRANSCRIPT, "pub fn view(");
    assert!(
        view.contains("let content_width = transcript_content_width(content_width, rem_size);"),
        "the rows use the inner width, not the transcript's outer measure"
    );
    assert!(
        view.contains(".px_2()"),
        "this subtraction belongs to the root's actual horizontal padding"
    );
    let helper = body_after(TRANSCRIPT, "fn transcript_content_width(");
    assert!(
        helper.contains("(outer_width - rem_size).max(0.0)"),
        "one rem of gutter, split across both sides, clamped at zero"
    );
}

/// gpui clips children to a rectangle: `ContentMask` carries bounds and no
/// corner radii, so a clip on the pane can never round a child's own fill. The
/// header paints the pane's top edge, so it carries the radius itself, and the
/// radius is set before the fill it shapes (#2936).
#[test]
fn the_header_fill_follows_the_ring() {
    let header = body_after(PANE_CHROME, "pub(crate) fn pane_header(");
    let round = header
        .find(".rounded_t(px(CARD_RADIUS))")
        .expect("the header rounds its own top corners");
    let fill = header
        .find(".bg(rgb(header_bg))")
        .expect("the header paints its own fill");
    assert!(round < fill, "the radius is set before the fill it shapes");
}
