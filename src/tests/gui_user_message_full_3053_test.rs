//! A message the reader typed is never cut silently, and a long one opens in
//! the reader (#3053, refined by #3064).
//!
//! The 20-line bound was measured on generated working-out (#2805). Applied to
//! the reader's own prompt it hid the rest of what he wrote behind a control
//! that only expands the card, and a turn with no card had no control at all.
//! #3064 keeps that promise and names the exception: past a much higher bound
//! the row keeps its opening lines and the rest becomes a document card, so the
//! cut is announced and one click from everything it left out.
//!
//! These checks read source, the way the other gui tests do: the window has no
//! headless harness, and the drawing lives in the gui binary.

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const MAIN: &str = concat!(
    include_str!("../gui/main.rs"),
    include_str!("../gui/app.rs"),
    include_str!("../gui/dashboard_render.rs"),
    include_str!("../gui/dashboard_state.rs"),
    include_str!("../gui/dashboard_sessions.rs"),
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

/// A prompt that reads in the column is drawn whole, and one that does not is
/// folded behind a card instead of cut with nothing to press.
#[test]
fn a_prompt_is_never_cut_without_a_control() {
    let bubble = region("fn bubble(", "/// A forwarded copy");
    assert!(
        bubble.contains("message_reader::fold(content)"),
        "the fold decision is the row's, made against the message reader's rules"
    );
    assert!(
        bubble.contains("None if full || role == \"user\""),
        "a body under the fold bound is drawn in full, whatever the turn is doing"
    );
    assert!(
        bubble.contains("excerpt_lines(content, MSG_EXCERPT_LINES)"),
        "working-out bodies still take the bound they were measured on"
    );
    assert!(
        bubble.contains("folded_document_cell(fold, open)"),
        "a folded prompt draws the card that reopens the rest"
    );
    assert!(
        bubble.contains("marker_paths(content)"),
        "the attachments are read from the whole body, before any fold hides the tail"
    );

    let card = region("fn folded_document_cell(", "/// A forwarded copy");
    assert!(
        card.contains("open: Option<&ClickHandler>"),
        "the card takes the click that opens the message in the reader"
    );

    let blocks = region(
        "fn block_view(",
        "Block::Thinking { content } => thinking_block",
    );
    assert!(
        blocks.contains("open,"),
        "block_view hands the open handler to the bubble it draws"
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
    assert!(
        view.contains("message_click.get(id)"),
        "the row and its card open the same reader, so they cannot disagree"
    );
}

/// A live row has no stored id, so it has no reader to open: it draws whole
/// rather than folding behind a card nobody can press.
#[test]
fn a_live_row_never_folds() {
    let live = region("fn live_block(", "/// How much of one narration");
    assert!(
        live.contains("None,"),
        "the live row passes no open handler"
    );
    assert!(
        live.contains("draws whole until it settles"),
        "so the fold is declined and the live text is drawn in full (#3064)"
    );
}

/// The text with no file behind it is still readable, and once the window has
/// written the file the reader opens that file (#3064).
#[test]
fn the_reader_prefers_the_written_file_and_falls_back_to_the_row() {
    assert!(
        PREVIEW_DOCUMENT.contains("pub fn parse_text(text: &str, flow: markdown::MdFlow)"),
        "the document body parses text, whether it came from a file or a row"
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
        MAIN.contains("self.prompt_docs.get(&id)"),
        "the reader asks where the row's document was written"
    );
    assert!(
        MAIN.contains("unwrap_or(content)"),
        "a file that cannot be read falls back to the text the row holds"
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
