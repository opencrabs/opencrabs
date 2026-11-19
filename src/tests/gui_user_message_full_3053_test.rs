//! A message the reader typed is never cut, and a long one opens in the
//! reader (#3053).
//!
//! The 20-line bound was measured on generated working-out (#2805). Applied to
//! the reader's own prompt it hid the rest of what he wrote behind a control
//! that only expands the card, and a turn with no card had no control at all.
//! These checks read source, the way the other gui tests do: the window has no
//! headless harness, and the drawing lives in the gui binary.

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const MAIN: &str = concat!(
    include_str!("../gui/main.rs"),
    include_str!("../gui/app.rs"),
    include_str!("../gui/dashboard_render.rs"),
    include_str!("../gui/dashboard_state.rs"),
);
const PANE_VIEW: &str = include_str!("../gui/pane_view.rs");
const PREVIEW: &str = include_str!("../gui/preview.rs");
const PREVIEW_DOCUMENT: &str = include_str!("../gui/preview_document.rs");

/// The renderer between two of its neighbours, so an assertion cannot be
/// satisfied by an unrelated function elsewhere in the file.
fn region_in(source: &'static str, start: &str, end: &str) -> &'static str {
    source
        .split(start)
        .nth(1)
        .unwrap_or_else(|| panic!("{start} is defined"))
        .split(end)
        .next()
        .unwrap_or_else(|| panic!("{end} follows {start}"))
}

fn region(start: &str, end: &str) -> &'static str {
    region_in(TRANSCRIPT, start, end)
}

/// The cut stays, for the body it was measured on. A message the reader typed
/// takes the full path whether the turn is folded, carded, or neither.
#[test]
fn a_message_the_reader_typed_is_never_cut() {
    let bubble = region("fn bubble(", "/// A forwarded copy");
    assert!(
        bubble.contains("if full || role == \"user\""),
        "a user body skips the excerpt in the bubble"
    );
    assert!(
        bubble.contains("excerpt_lines(content, MSG_EXCERPT_LINES)"),
        "working-out bodies still take the bound"
    );

    let blocks = region(
        "fn block_view(",
        "Block::Thinking { content } => thinking_block",
    );
    assert!(
        blocks.contains("full || role == \"user\""),
        "a forwarded message the reader sent is not cut either"
    );

    let view = region("pub fn view(", "/// The owner's test feedback");
    assert!(
        view.contains("let full_body = !folded"),
        "an expanded turn still draws everything in full"
    );
    assert!(
        view.contains("|| !carded"),
        "a folded turn with no card has no header to expand, so it draws in full"
    );
    assert!(
        view.contains("role == \"assistant\" || role == \"user\""),
        "the folded exemption covers the reader's own rows, not only answers"
    );
}

/// The reader takes text that has no file behind it: nothing is written to
/// disk to read a message, and the file actions are not drawn for one.
#[test]
fn the_reader_reads_a_message_without_a_file() {
    assert!(
        PREVIEW_DOCUMENT.contains("pub fn parse_text(text: &str, flow: markdown::MdFlow)"),
        "the document body parses text that is already in memory"
    );
    assert!(
        PREVIEW_DOCUMENT.contains("pub fn text(generation: u64) -> Self"),
        "a reader with no path is a supported state"
    );
    assert!(
        PREVIEW.contains("pub fn message(title: String) -> Self"),
        "a message preview is its own kind, not a fake path"
    );
    assert!(
        PREVIEW.contains("PreviewKind::Message => message_body(document, middle_click)"),
        "the message body is the reader, with no path row"
    );
    assert!(
        !PREVIEW_DOCUMENT.contains("temp_dir()") && !PREVIEW_DOCUMENT.contains("write_all"),
        "a message is never written to a temporary file to be read"
    );
}

/// Both places that draw a conversation mint the click, so the reader opens
/// from a split leaf as well as from the window's chat.
#[test]
fn a_long_message_opens_in_the_reader_from_every_transcript() {
    for (name, source) in [("main.rs", MAIN), ("pane_view.rs", PANE_VIEW)] {
        assert!(
            source.contains("message_reader::opens_in_reader(&row.content)"),
            "{name} decides the click from the row's own length"
        );
        assert!(
            source.contains("message_reader::title(&row.content)"),
            "{name} names the reader after the message"
        );
        assert!(
            source.contains("this.open_message_preview("),
            "{name} opens the message in the reader"
        );
        assert!(
            source.contains("click.down.position).magnitude() > 2.0"),
            "{name} keeps a drag a copy gesture instead of opening the reader"
        );
    }
}
