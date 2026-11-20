//! The voice note row: player first, words behind a fold (#3111).
//!
//! The GUI is a separate binary crate, so `voice_note` is out of reach from
//! here. Its four behavioural cases run beside it in
//! `src/gui/tests/voice_note_row_test.rs`; this file pins the wiring the
//! transcript depends on, which is what the library can actually see.

const TRANSCRIPT: &str = include_str!("../gui/transcript.rs");
const COMPOSER: &str = include_str!("../gui/composer_orb.rs");

/// One named region of a source file, from `start` up to `end`.
fn region(source: &'static str, start: &str, end: &str) -> &'static str {
    source
        .split(start)
        .nth(1)
        .unwrap_or_else(|| panic!("{start} is defined"))
        .split(end)
        .next()
        .unwrap_or_else(|| panic!("{end} follows {start}"))
}

/// The helper itself, so every pin below reads the shipped rule.
fn helper_body() -> &'static str {
    region(
        TRANSCRIPT,
        "/// The clip a message carries when the message is a voice note (#3111).",
        "\nthread_local! {",
    )
}

/// A voice note is the `AUD` marker whose path the capture path wrote. The
/// recording rule is borrowed, not restated: a second copy of it here would
/// drift from the one the send path uses.
#[test]
fn voice_note_reads_the_marker_kind_and_the_recording_rule() {
    let body = helper_body();
    assert!(
        body.contains("kind == \"AUD\""),
        "only an audio marker is a clip"
    );
    assert!(
        body.contains("crate::voice_input::is_recording(Path::new(path))"),
        "and only a recording is a voice note, not a picked audio file"
    );
    assert!(
        body.contains("&& !path.is_empty()"),
        "an empty path is not a clip"
    );
}

/// A recording is not always the first marker in the body: a take sent beside a
/// screenshot carries the image first, and the row still has to find the clip.
#[test]
fn voice_note_walks_every_marker_in_the_body() {
    let body = helper_body();
    assert!(
        body.contains("rest = after.get(end + 2..).unwrap_or_default()"),
        "a marker that is not the clip advances the scan instead of ending it"
    );
    assert!(
        body.trim_end().ends_with("None\n}"),
        "and a body with no recording ends as no voice note"
    );
}

/// A stored row is only a voice note when the reader sent it. An answer that
/// carries a spoken reply keeps its own shape.
#[test]
fn voice_note_is_asked_only_about_a_user_row() {
    let bubble = region(TRANSCRIPT, "fn bubble(", "fn folded_document_cell(");
    assert!(
        bubble.contains("let voice = if role == \"user\" {\n        voice_note(&content)"),
        "the rule is asked about the reader's own rows"
    );
}

/// The helper answers one question: is there a clip. Whether there are words to
/// fold away is a question about the row's text, which the row already has with
/// every marker stripped, so a take with nothing transcribed gets no toggle.
#[test]
fn voice_note_returns_only_the_clip() {
    assert!(
        helper_body().contains("-> Option<PathBuf>"),
        "the clip is the whole answer"
    );
    let bubble = region(TRANSCRIPT, "fn bubble(", "fn folded_document_cell(");
    assert!(
        bubble.contains("let transcript = (voice.is_some() && !text.trim().is_empty())"),
        "a fold exists only when there is text to fold"
    );
}

/// The row draws the clip first, because the clip is what the message is. The
/// attachments are drawn once, in one place or the other, never twice.
#[test]
fn the_player_is_drawn_above_the_words() {
    let bubble = region(TRANSCRIPT, "fn bubble(", "fn folded_document_cell(");
    let player = bubble
        .find("if voice.is_some() && !paths.is_empty() {")
        .expect("a voice row draws its clip above its words");
    let prose = bubble
        .find("if !text.trim().is_empty() && (voice.is_none() || transcript_open) {")
        .expect("the words are drawn behind the fold");
    assert!(player < prose, "the player comes first");
    assert_eq!(
        bubble.matches("history_attachments(&paths").count(),
        2,
        "one call above the words and one below, never both on the same row"
    );
    assert!(
        bubble.contains("if voice.is_none() && !paths.is_empty() {"),
        "every other row keeps its files under its text"
    );
}

/// The fold is a control of its own, and it is the only thing that opens the
/// words: the quote and the folded document card follow the same guard, because
/// both of them are text.
#[test]
fn the_fold_hides_the_quote_and_the_document_card_too() {
    let bubble = region(TRANSCRIPT, "fn bubble(", "fn folded_document_cell(");
    assert!(
        bubble.contains("if (voice.is_none() || transcript_open) && !quote.is_empty() {"),
        "a quoted reply folds with the words it belongs to"
    );
    let card_guard = concat!(
        "if let Some(fold) = &folded\n",
        "        && (voice.is_none() || transcript_open)",
    );
    assert!(
        bubble.contains(card_guard),
        "and so does the card that stands in for a long one"
    );
}

/// Opening a fold is paint-time state: the toggle writes the set and reads the
/// same set back, so a repaint does not close what the reader opened.
#[test]
fn the_toggle_writes_the_set_it_reads() {
    let toggle = region(TRANSCRIPT, "fn transcript_toggle(", "fn history_attachments(");
    assert!(
        toggle.contains("OPEN_TRANSCRIPTS.with(|open| {"),
        "the press flips the set"
    );
    let flip = concat!(
        "if !open.remove(&pressed) {\n",
        "                    open.insert(pressed.clone());",
    );
    assert!(
        toggle.contains(flip),
        "and the same key is what it removes and inserts"
    );
    assert!(
        toggle.contains("window.refresh()"),
        "the row repaints once the set changed"
    );
    let read = region(TRANSCRIPT, "fn transcript_is_open(", "fn transcript_toggle(");
    assert!(
        read.contains("OPEN_TRANSCRIPTS.with(|open| open.borrow().contains(key))"),
        "and the read is the same set"
    );
    assert!(
        TRANSCRIPT.contains("static OPEN_TRANSCRIPTS: RefCell<HashSet<String>>"),
        "collapsed is the default every launch, so nothing is persisted"
    );
}

/// The block the row plays is twice the strip it replaced, and it gives way to a
/// narrow bubble instead of painting past it.
#[test]
fn the_player_is_twice_the_strip_it_replaced() {
    assert!(
        COMPOSER.contains("const PLAYER_POINTS: (f32, f32) = (276.0, 60.0);"),
        "the old block was 138 by 30, and the owner asked for double"
    );
    assert!(
        COMPOSER.contains("const MIN_PLAYER_WIDTH: f32 = 120.0;"),
        "the block has a floor, so it stays a target in a narrow pane"
    );
    assert!(
        COMPOSER.contains("PLAYER_POINTS.0.min(max_width).max(MIN_PLAYER_WIDTH)"),
        "the width is the block's preference, clamped to the room it has"
    );
    let player = region(
        COMPOSER,
        "fn player_into(",
        "/// The block while its clip is still being made",
    );
    assert!(
        player.contains("let width = player_width(max_width);"),
        "every raster is built at the width the row could give it"
    );
}
