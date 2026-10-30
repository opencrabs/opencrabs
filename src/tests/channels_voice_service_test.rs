use crate::channels::voice::service::*;
use proptest::prelude::*;

#[test]
fn split_short_text_returns_single_chunk() {
    let text = "Hello world, this is short.";
    let chunks = split_for_tts(text, 1500);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], text);
}

#[test]
fn split_long_text_breaks_at_sentences() {
    let text = "First sentence here. Second sentence here. Third sentence is much longer and goes on for a while to push us over the limit.";
    let chunks = split_for_tts(text, 50);
    assert!(chunks.len() > 1);
    // Each chunk should be under the limit
    for chunk in &chunks {
        assert!(chunk.len() <= 50, "chunk too long: {} chars", chunk.len());
    }
    // All content should be preserved
    let rejoined: String = chunks.join(" ");
    assert!(rejoined.contains("First sentence"));
    assert!(rejoined.contains("Second sentence"));
    assert!(rejoined.contains("Third sentence"));
}

#[test]
fn split_respects_question_and_exclamation() {
    let text = "Is this working? Yes it is! Great news indeed. Another sentence.";
    let chunks = split_for_tts(text, 30);
    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(chunk.len() <= 30, "chunk too long: {} chars", chunk.len());
    }
}

#[test]
fn split_falls_back_to_word_boundary() {
    // No sentence breaks, just one long sentence
    let text = "word ".repeat(100); // 500 chars, no periods
    let chunks = split_for_tts(&text, 100);
    assert!(chunks.len() > 1);
    for chunk in &chunks {
        assert!(chunk.len() <= 100, "chunk too long: {} chars", chunk.len());
    }
}

#[test]
fn find_sentence_break_finds_last_period() {
    assert_eq!(find_sentence_break("Hello. World"), Some(7)); // ". " is 2 chars
    assert_eq!(find_sentence_break("Hello! World"), Some(7));
    assert_eq!(find_sentence_break("Hello? World"), Some(7));
    assert_eq!(find_sentence_break("No break here"), None);
}

#[test]
fn split_preserves_newlines() {
    let text =
        "Paragraph one.\n\nParagraph two.\n\nParagraph three with more text to exceed limit.";
    let chunks = split_for_tts(text, 30);
    assert!(chunks.len() > 1);
}

#[test]
fn split_empty_text() {
    let chunks = split_for_tts("", 1500);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], "");
}

#[test]
fn split_exactly_at_limit() {
    let text = "a".repeat(1500);
    let chunks = split_for_tts(&text, 1500);
    assert_eq!(chunks.len(), 1);
}

#[test]
fn split_one_over_limit() {
    let text = "a".repeat(1501);
    let chunks = split_for_tts(&text, 1500);
    assert_eq!(chunks.len(), 2);
}

#[test]
fn split_realistic_long_response() {
    // Simulate a long assistant response by repeating meaningful text
    let base = "Here is a detailed explanation of how Rust's borrow checker works. The borrow checker is a compile-time feature that ensures memory safety. It tracks the lifetime of references and ensures they don't outlive the data they point to. ";
    let text = base.repeat(10); // ~2500 chars
    let chunks = split_for_tts(&text, 1500);
    assert!(
        chunks.len() >= 2,
        "expected >= 2 chunks, got {}",
        chunks.len()
    );
    for chunk in &chunks {
        assert!(chunk.len() <= 1500, "chunk too long: {} chars", chunk.len());
        assert!(!chunk.trim().is_empty(), "empty chunk");
    }
}

// ---------------------------------------------------------------------------
// #2051: the splitter must cut on char boundaries, never at a raw byte index.
// ---------------------------------------------------------------------------

/// Whitespace is dropped at chunk edges by `trim`, so collapsed text is the
/// invariant: no non-whitespace character is ever lost or reordered.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect()
}

/// Run `f` on a worker thread so a regression that spins fails the test instead
/// of hanging the suite forever.
fn with_deadline<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("{what} never returned: the progress guard is gone (#2051)"))
}

/// The reported payload: `->` (U+2192, three bytes) straddling byte 1500. The
/// raw slice panicked here and the whole TTS output was lost.
#[test]
fn split_does_not_panic_on_a_multibyte_char_at_the_cut() {
    let mut text = "a".repeat(1499);
    text.push('\u{2192}');
    text.push_str(&"b".repeat(200));
    assert!(
        !text.is_char_boundary(1500),
        "premise: byte 1500 must land inside the arrow"
    );

    let chunks = split_for_tts(&text, 1500);

    assert!(chunks.len() >= 2, "expected a split, got {chunks:?}");
    for chunk in &chunks {
        assert!(
            chunk.len() <= 1500,
            "chunk of {} bytes exceeds the budget",
            chunk.len()
        );
    }
    assert_eq!(collapse(&chunks.concat()), collapse(&text));
}

/// Same class, second door: the last whitespace in the window is U+00A0 (two
/// bytes), so the word break at `pos + 1` landed inside the char and the
/// caller's slice panicked.
#[test]
fn split_does_not_panic_on_multibyte_whitespace_at_the_cut() {
    let mut text = "x".repeat(100);
    text.push('\u{00a0}');
    text.push_str(&"y".repeat(100));
    let nbsp = text.find('\u{00a0}').expect("nbsp is in the text");
    assert!(
        !text.is_char_boundary(nbsp + 1),
        "premise: the byte after the nbsp is inside it"
    );

    let chunks = split_for_tts(&text, 150);

    for chunk in &chunks {
        assert!(chunk.len() <= 150, "chunk too long: {} bytes", chunk.len());
    }
    assert_eq!(collapse(&chunks.concat()), collapse(&text));
}

/// A budget smaller than one char empties the rounded-down window, so the
/// progress guard is the only thing keeping the loop finite.
#[test]
fn split_makes_progress_when_a_char_is_wider_than_the_budget() {
    let crabs = with_deadline("split_for_tts with a sub-char budget", || {
        split_for_tts("\u{1f980}\u{1f980}\u{1f980}\u{1f980}", 1)
    });
    assert_eq!(crabs.len(), 4, "one chunk per crab: {crabs:?}");
    for chunk in &crabs {
        assert_eq!(chunk.chars().count(), 1);
    }

    let ascii = with_deadline("split_for_tts with a zero budget", || {
        split_for_tts("abc", 0)
    });
    assert_eq!(
        ascii.len(),
        3,
        "a zero budget must still terminate: {ascii:?}"
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Arbitrary unicode at arbitrary budgets: always returns, never panics,
    /// and never exceeds the budget unless one char alone is wider than it.
    #[test]
    fn split_survives_arbitrary_unicode(
        chars in proptest::collection::vec(any::<char>(), 0..300),
        max_bytes in 1usize..64,
    ) {
        let text: String = chars.into_iter().collect();
        let chunks = split_for_tts(&text, max_bytes);

        for chunk in &chunks {
            prop_assert!(
                chunk.len() <= max_bytes || chunk.chars().count() <= 1,
                "chunk of {} bytes over a {}-byte budget: {:?}",
                chunk.len(),
                max_bytes,
                chunk
            );
        }
        prop_assert_eq!(collapse(&chunks.concat()), collapse(&text));
    }
}

// ---------------------------------------------------------------------------
// Structural pins: the raw slice must not come back, and no chunker may lose
// the boundary-safe guard that replaces it.
// ---------------------------------------------------------------------------

/// Every text chunker in the tree, by path, with its source inlined so the scan
/// reads the file the compiler sees.
const CHUNKERS: [(&str, &str); 4] = [
    (
        "src/channels/voice/service.rs",
        include_str!("../channels/voice/service.rs"),
    ),
    (
        "src/channels/telegram/markdown.rs",
        include_str!("../channels/telegram/markdown.rs"),
    ),
    (
        "src/channels/trello/handler.rs",
        include_str!("../channels/trello/handler.rs"),
    ),
    (
        "src/cron/scheduler.rs",
        include_str!("../cron/scheduler.rs"),
    ),
];

/// The panics of #1919 and #2051 both started as `&text[..max_*]`, so the
/// pattern is the sentinel rather than the one function that carried it.
/// Comment lines are skipped: scheduler.rs documents its own #1919 fix by
/// quoting the slice it removed, and so does service.rs.
#[test]
fn no_chunker_slices_at_a_raw_byte_budget() {
    let offenders: Vec<String> = CHUNKERS
        .iter()
        .flat_map(|(path, source)| {
            source
                .lines()
                .enumerate()
                .filter(|(_, line)| !line.trim_start().starts_with("//"))
                .filter(|(_, line)| line.contains("[..max"))
                .map(move |(i, line)| format!("{path}:{}: {}", i + 1, line.trim()))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "a chunker byte-slices at a raw budget and will panic on a multibyte \
         char (#2051): {offenders:#?}"
    );
}

/// The other half of the pin: each chunker's boundary-safe guard must still be
/// there, so deleting it fails here instead of in production at 4am.
#[test]
fn every_chunker_keeps_its_char_boundary_guard() {
    let guards = [
        (
            "src/channels/voice/service.rs",
            "truncate_str(remaining, max_bytes)",
        ),
        ("src/channels/telegram/markdown.rs", ".nth(max_len)"),
        (
            "src/channels/trello/handler.rs",
            "is_char_boundary(safe_max)",
        ),
        ("src/cron/scheduler.rs", "truncate_str(content, MAX_LEN)"),
    ];
    for (path, guard) in guards {
        let source = CHUNKERS
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, s)| *s)
            .unwrap_or_else(|| panic!("{path} is missing from CHUNKERS"));
        assert!(
            source.contains(guard),
            "{path} lost its char boundary guard ({guard})"
        );
    }
}
