use crate::utils::string::*;

#[test]
fn test_strip_ctx_footer() {
    assert_eq!(
        strip_ctx_footer("Hello\n\nctx: 84K/200K 42% | 406 tok/s"),
        "Hello"
    );
    assert_eq!(strip_ctx_footer("ctx: 8K/200K 4%"), "");
    assert_eq!(
        strip_ctx_footer("Some text\nctx: 1M/2M 50% | 123 tok/s\nMore text"),
        "Some text\nMore text"
    );
    // Don't strip lines that just happen to contain a colon
    assert_eq!(
        strip_ctx_footer("Note: something here"),
        "Note: something here"
    );
    // Multi-line without footer is unchanged
    let multi = "line1\nline2\nline3";
    assert_eq!(strip_ctx_footer(multi), multi);
}

#[test]
fn test_truncate_str_ascii() {
    assert_eq!(truncate_str("hello world", 5), "hello");
    assert_eq!(truncate_str("hello", 10), "hello");
    assert_eq!(truncate_str("hello", 5), "hello");
}

#[test]
fn test_truncate_str_multibyte_boundary() {
    // █ is U+2588, 3 bytes in UTF-8
    let s = "abc█def";
    // "abc" = 3 bytes, "█" = bytes 3..6, "def" = bytes 6..9
    assert_eq!(truncate_str(s, 3), "abc"); // exact boundary before █
    assert_eq!(truncate_str(s, 4), "abc"); // inside █, backs up to 3
    assert_eq!(truncate_str(s, 5), "abc"); // inside █, backs up to 3
    assert_eq!(truncate_str(s, 6), "abc█"); // exact boundary after █
}

#[test]
fn test_truncate_str_emoji() {
    // 🦀 is U+1F980, 4 bytes in UTF-8
    let s = "hi🦀bye";
    // "hi" = 2 bytes, "🦀" = bytes 2..6, "bye" = bytes 6..9
    assert_eq!(truncate_str(s, 2), "hi");
    assert_eq!(truncate_str(s, 3), "hi"); // inside 🦀
    assert_eq!(truncate_str(s, 5), "hi"); // inside 🦀
    assert_eq!(truncate_str(s, 6), "hi🦀");
}

#[test]
fn test_truncate_str_zero() {
    assert_eq!(truncate_str("hello", 0), "");
    assert_eq!(truncate_str("🦀", 0), "");
}

#[test]
fn test_truncate_str_empty() {
    assert_eq!(truncate_str("", 5), "");
    assert_eq!(truncate_str("", 0), "");
}

#[test]
fn test_truncate_str_all_multibyte() {
    // Each char is 3 bytes
    let s = "███"; // 9 bytes
    assert_eq!(truncate_str(s, 1), ""); // inside first █
    assert_eq!(truncate_str(s, 3), "█");
    assert_eq!(truncate_str(s, 7), "██"); // inside third █
    assert_eq!(truncate_str(s, 9), "███");
}

#[test]
fn test_truncate_middle_preserves_tail() {
    let s = "/Users/alice/srv/myapp/lib/presentation/pages/some_really_long_widget_name.dart";
    let out = truncate_middle(s, 40);
    // The filename must survive — that's the whole point of middle-elide.
    assert!(
        out.ends_with("some_really_long_widget_name.dart")
            || out.ends_with("_name.dart")
            || out.ends_with(".dart"),
        "expected filename tail preserved, got: {}",
        out
    );
    assert!(out.contains('…'), "expected ellipsis marker in middle");
    assert!(out.len() <= 40, "expected <= 40 bytes, got {}", out.len());
}

#[test]
fn test_truncate_middle_short_string_unchanged() {
    assert_eq!(truncate_middle("short.rs", 80), "short.rs");
}

#[test]
fn test_truncate_middle_tiny_budget_falls_back() {
    let out = truncate_middle("hello world", 4);
    // Budget too small for head+…+tail — falls back to head-only truncation.
    assert!(out.len() <= 4);
}

#[test]
fn test_tilde_home_collapses_prefix() {
    if let Some(h) = dirs::home_dir() {
        let home_str = h.to_string_lossy();
        let full = format!("{}/srv/project/file.rs", home_str);
        assert_eq!(tilde_home(&full), "~/srv/project/file.rs");
        // Non-home paths untouched.
        assert_eq!(tilde_home("/tmp/foo"), "/tmp/foo");
        // Bash commands with multiple occurrences all collapsed.
        let cmd = format!("cp {}/a {}/b", home_str, home_str);
        assert_eq!(tilde_home(&cmd), "cp ~/a ~/b");
    }
}

// ── looks_like_file_path ───────────────────────────────────────────────

#[test]
fn test_looks_like_file_path_absolute_with_segments() {
    assert!(looks_like_file_path("/Users/alice/Downloads/report.pdf"));
    assert!(looks_like_file_path("/tmp/foo.txt"));
    assert!(looks_like_file_path("/etc/hosts"));
}

#[test]
fn test_looks_like_file_path_with_extension_and_text() {
    // Drag-and-drop pattern: path followed by a message
    assert!(looks_like_file_path("/report.pdf check this"));
    assert!(looks_like_file_path("/data.csv analyze"));
}

#[test]
fn test_looks_like_file_path_slash_commands_are_not_paths() {
    assert!(!looks_like_file_path("/help"));
    assert!(!looks_like_file_path("/models"));
    assert!(!looks_like_file_path("/deploy staging"));
    assert!(!looks_like_file_path("/credits"));
}

#[test]
fn test_looks_like_file_path_no_slash_prefix() {
    assert!(!looks_like_file_path("hello world"));
    assert!(!looks_like_file_path("report.pdf"));
}

// ── format_ctx_footer ───────────────────────────────────────────────────

#[test]
fn test_format_ctx_footer_k_values() {
    assert_eq!(format_ctx_footer(8000, 200000, None), "ctx: 8K/200K 4%");
}

#[test]
fn test_format_ctx_footer_small_values() {
    assert_eq!(format_ctx_footer(500, 200000, None), "ctx: 500/200K 0%");
}

#[test]
fn test_format_ctx_footer_m_values() {
    assert_eq!(
        format_ctx_footer(1200000, 2000000, None),
        "ctx: 1.2M/2.0M 60%"
    );
}

#[test]
fn test_format_ctx_footer_zero_used() {
    assert_eq!(format_ctx_footer(0, 200000, None), "ctx: 0/200K 0%");
}

#[test]
fn test_format_ctx_footer_zero_max() {
    assert_eq!(format_ctx_footer(5000, 0, None), "ctx: 5K/0 0%");
}

#[test]
fn test_format_ctx_footer_full() {
    assert_eq!(
        format_ctx_footer(200000, 200000, None),
        "ctx: 200K/200K 100%"
    );
}

#[test]
fn test_format_ctx_footer_with_tps() {
    assert_eq!(
        format_ctx_footer(8000, 200000, Some(45.7)),
        "ctx: 8K/200K 4% | 46 tok/s"
    );
    assert_eq!(
        format_ctx_footer(500, 200000, Some(123.4)),
        "ctx: 500/200K 0% | 123 tok/s"
    );
}

// ── thinking_excerpt (#742) ─────────────────────────────────────

#[test]
fn thinking_excerpt_returns_none_for_short_reasoning() {
    assert!(crate::utils::string::thinking_excerpt("hi").is_none());
    assert!(crate::utils::string::thinking_excerpt("   ").is_none());
}

#[test]
fn thinking_excerpt_takes_latest_sentence_stripped_and_capitalized() {
    let t = "First I looked at the config file. Let me read the template exactly now";
    let e = crate::utils::string::thinking_excerpt(t).unwrap();
    // Latest sentence, "Let me " stripped, first letter capitalized.
    assert_eq!(e, "Read the template exactly now");
}

#[test]
fn thinking_excerpt_caps_at_the_budget_with_ellipsis() {
    use crate::utils::string::{THINKING_EXCERPT_CHARS, thinking_excerpt_capped};
    let long = format!("Now I am {}", "x".repeat(600));
    let e = thinking_excerpt_capped(&long, THINKING_EXCERPT_CHARS).unwrap();
    assert!(
        e.chars().count() <= THINKING_EXCERPT_CHARS + 1,
        "len {}",
        e.chars().count()
    ); // budget + ellipsis
    assert!(e.ends_with('\u{2026}'));
}

#[test]
fn thinking_excerpt_keeps_a_multi_step_chain_intact() {
    // The reported case: 80 chars cut this off mid-thought at the last step,
    // which is the one moment the line is meant to be useful (#768).
    let chain = "Right, the plan. #766 tests running in background then wait for them \
                 then commit #766 then comment and close #766 then move on to #767 and \
                 finally re-run the full suite before tagging";
    let e = crate::utils::string::thinking_excerpt_capped(
        chain,
        crate::utils::string::THINKING_EXCERPT_CHARS,
    )
    .unwrap();
    assert!(
        e.contains("re-run the full suite"),
        "the tail of the chain must survive the cap, got: {e}"
    );
    assert!(
        !e.ends_with('\u{2026}'),
        "nothing to truncate at 300, got: {e}"
    );
}

/// #490: the tail-preserving sibling keeps BOTH ends and reports the count it
/// dropped. The head-only cut lost the ending entirely — which is where a
/// reader's own blocks (Disclosures, What-now/next) live.
#[test]
fn tail_preserving_keeps_both_ends_and_reports_the_count() {
    let head_sentinel = "HEAD-SENTINEL";
    let tail_sentinel = "TAIL-SENTINEL";
    let body = format!("{head_sentinel}{}{tail_sentinel}", "m".repeat(5000));
    let (out, dropped) = truncate_chars_tail_preserving(&body, 1000);

    assert!(out.contains(head_sentinel), "head survives: {out:.80}");
    assert!(
        out.contains(tail_sentinel),
        "the TAIL survives — this is the whole point of the #490 change"
    );
    assert!(
        out.contains("(truncated 4066 chars)"),
        "the marker states the dropped COUNT (5026 chars in, 960 kept as head+tail): {out}"
    );
    assert!(dropped > 0, "a cut happened, so the count is non-zero");
    assert!(
        out.chars().count() <= 1000,
        "the output respects its own budget, marker included: got {}",
        out.chars().count()
    );
}

/// The unchanged path: a body that already fits comes back verbatim with zero
/// dropped, which is what a caller branches on before it owes a warning.
#[test]
fn tail_preserving_leaves_a_fitting_body_untouched() {
    let body = "short body";
    let (out, dropped) = truncate_chars_tail_preserving(body, 1000);
    assert_eq!(out, body);
    assert_eq!(dropped, 0);
}

/// A budget too small for both ends AND a marker falls back to the head-only
/// cut rather than emitting a marker that IS the output.
#[test]
fn tail_preserving_degrades_to_head_only_at_a_tiny_budget() {
    let body = "x".repeat(500);
    let (out, dropped) = truncate_chars_tail_preserving(&body, 20);
    assert_eq!(out.chars().count(), 20, "output fits the budget");
    assert_eq!(dropped, 480, "20 kept of 500");
    assert!(
        !out.contains("truncated"),
        "no marker at a tiny budget: {out}"
    );
}

/// Multi-byte safety: the helper slices by CHARACTER, so a body full of
/// 4-byte emoji must not panic on a non-boundary byte index.
#[test]
fn tail_preserving_is_char_boundary_safe() {
    let body = "🎯".repeat(3000);
    let (out, dropped) = truncate_chars_tail_preserving(&body, 500);
    assert!(out.starts_with('🎯'));
    assert!(out.ends_with('🎯'));
    assert!(dropped > 0);
}

/// The head cut snaps DOWN to a boundary. Byte `max_bytes` may sit inside a
/// multi-byte character, and the raw `&s[..max_bytes]` panicked there (#2124).
#[test]
fn truncate_str_snaps_down_inside_a_multibyte_char() {
    // "é" is 2 bytes, so byte 1 lands inside it.
    assert_eq!(truncate_str("é", 1), "");
    // A 4-byte emoji: budgets 1, 2 and 3 all land inside it.
    for budget in 1..4 {
        assert_eq!(truncate_str("🎯tail", budget), "");
    }
    assert_eq!(truncate_str("🎯tail", 4), "🎯");
    // A budget at or past the end is the identity.
    assert_eq!(truncate_str("abc", 3), "abc");
    assert_eq!(truncate_str("abc", 99), "abc");
}

/// The tail cut is the shape that panicked: `&s[len - n..]` with `len - n`
/// inside a character. It now snaps UP to the next boundary (#2124).
#[test]
fn truncate_str_tail_snaps_up_inside_a_multibyte_char() {
    let body = "Report attached: 📎 Reporté";
    // A budget the string already fits keeps all of it.
    assert_eq!(truncate_str_tail(body, 80), body);
    let tail = truncate_str_tail(body, 8);
    assert!(body.ends_with(tail), "the result is a suffix: {tail:?}");
    assert!(!tail.is_empty(), "8 bytes buys at least one character");
    // No budget can panic, whatever byte it lands on.
    for n in 0..=body.len() + 2 {
        let _ = truncate_str_tail(body, n);
    }
}

/// Peeking at the final character's punctuation must not slice inside it.
#[test]
fn prefix_before_last_char_never_splits_the_final_char() {
    // "°" is 2 bytes, the shape that broke the period check (#2124).
    assert_eq!(prefix_before_last_char("5°"), "5");
    assert_eq!(prefix_before_last_char("ab."), "ab");
    assert_eq!(prefix_before_last_char("🎯"), "");
    assert_eq!(prefix_before_last_char(""), "");
}

/// On ASCII the new helpers are byte-for-byte what the old raw slices did, so
/// routing ten call sites through them changed no behaviour.
#[test]
fn the_boundary_helpers_match_the_old_ascii_slices() {
    let body = "abcdefghij";
    assert_eq!(truncate_str_tail(body, 4), "ghij");
    assert_eq!(prefix_before_last_char(body), "abcdefghi");
    assert_eq!(truncate_str(body, 4), "abcd");
}
