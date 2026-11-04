//! Loop guard (#957): normalization, near-duplicate matching, and the
//! cross-turn outgoing-text ring.
//!
//! The Luna incident (18+ near-identical announcements, bash echo loop)
//! slipped past every existing guard because they all match EXACT text or
//! EXACT tool signatures within ONE turn. Normalization collapses
//! counters, punctuation and whitespace so near-identical repeats become
//! exact matches (tool layer); Jaccard near-duplicate matching over the
//! normalized word sets plus a per-session ring buffer catches the
//! reworded announcements that span turns (text layer).

use crate::brain::agent::service::announcement_loop::{
    OutgoingTextRing, TextLoopAction, near_duplicate,
};
use crate::brain::agent::service::helpers::{
    normalize_loop_arg, normalize_loop_text, normalized_call_signature,
};
use serde_json::json;

// ---- normalize_loop_text ----

#[test]
fn normalize_strips_digits_punctuation_and_lowercases() {
    assert_eq!(
        normalize_loop_text("Echo \"Hello, World!\" 42 times -- really?!"),
        "echo hello world times really"
    );
}

#[test]
fn normalize_collapses_whitespace_and_caps_length() {
    assert_eq!(normalize_loop_text("a   b\n\nc"), "a b c");
    let long = "word ".repeat(500);
    assert!(normalize_loop_text(&long).chars().count() <= 400);
}

#[test]
fn normalize_keeps_cyrillic_letters() {
    // The reported log was Russian; the guard must normalize it cleanly.
    assert_eq!(
        normalize_loop_text("Отправляю 6 подтверждений в ДДС!"),
        "отправляю подтверждений в ддс"
    );
}

#[test]
fn counter_variations_collide_after_normalization() {
    // The Luna pattern: same bash echo, only the counter moves.
    // Normalization must make them EXACTLY equal — that is what the
    // tool-loop bash near-match relies on.
    let a = "bash: echo \"Отправляю 1 подтверждение в ДДС\"";
    let b = "bash: echo \"Отправляю 2 подтверждение в ДДС\"";
    assert_eq!(normalize_loop_text(a), normalize_loop_text(b));

    let c = "bash: echo \"Sending confirmation 3 of 6 to DDS with the PDF attachment\"";
    let d = "bash: echo \"Sending confirmation 4 of 6 to DDS with the PDF attachment\"";
    assert_eq!(normalize_loop_text(c), normalize_loop_text(d));
}

#[test]
fn different_commands_stay_apart_after_normalization() {
    assert_ne!(
        normalize_loop_text("cargo build --release"),
        normalize_loop_text("cargo test --all-features")
    );
    assert_ne!(
        normalize_loop_text("git status"),
        normalize_loop_text("ls -la")
    );
}

// ---- normalized_call_signature (#961) ----

#[test]
fn counter_variant_tool_search_queries_collide() {
    // The #961 DeepSeek v4 flash pattern: tool_search re-issued with only a
    // trailing counter moving between attempts must collapse to the SAME
    // near-match signature so the generalized tool-layer guard can count it.
    let a = normalized_call_signature(
        "tool_search",
        &json!({"query": "send a document file to telegram"}),
    );
    let b = normalized_call_signature(
        "tool_search",
        &json!({"query": "send a document file to telegram 2"}),
    );
    assert_eq!(a, b);
}

#[test]
fn signature_collapses_counters_punctuation_and_case() {
    // Regression guard for the #957 bash path under the generalized helper.
    let a = normalized_call_signature("bash", &json!({"command": "echo \"Attempt 1 of 6\""}));
    let b = normalized_call_signature("bash", &json!({"command": "echo attempt 2 of 6"}));
    assert_eq!(a, b);
}

#[test]
fn different_tool_search_queries_stay_apart() {
    let a = normalized_call_signature(
        "tool_search",
        &json!({"query": "send a document file to telegram"}),
    );
    let b = normalized_call_signature("tool_search", &json!({"query": "schedule a cron job"}));
    assert_ne!(a, b);
}

#[test]
fn signature_is_namespaced_by_tool_name() {
    // Same args under different tool names must not collide.
    let a = normalized_call_signature("tool_search", &json!({"query": "send"}));
    let b = normalized_call_signature("telegram_send", &json!({"query": "send"}));
    assert_ne!(a, b);
}

#[test]
fn read_file_chunk_offsets_stay_apart() {
    // #82: numeric argument values are preserved exactly, so chunked reads
    // differing only in start_line no longer collapse — the tool loop no
    // longer needs its read_file exclusion, and genuine read loops (same
    // path, same range) still collide.
    let a = normalized_call_signature(
        "read_file",
        &json!({"path": "src/main.rs", "start_line": 100, "line_count": 50}),
    );
    let b = normalized_call_signature(
        "read_file",
        &json!({"path": "src/main.rs", "start_line": 150, "line_count": 50}),
    );
    assert_ne!(a, b);

    // Identical chunked reads (the stuck-loop shape) still collide.
    let c = normalized_call_signature(
        "read_file",
        &json!({"path": "src/main.rs", "start_line": 100, "line_count": 50}),
    );
    assert_eq!(a, c);
}

#[test]
fn plan_checklist_progression_stays_apart() {
    // #82 real-world false positive (2026-09-02): completing plan checklist
    // tasks differs ONLY in task_order — digit-stripping collapsed the
    // progression into one signature, nudging and then breaking a session
    // mid-checklist. Numeric fields must keep the calls distinct, while a
    // literally identical stuck plan call still collides.
    let done = |n: i64| {
        normalized_call_signature("plan", &json!({"operation": "complete", "task_order": n}))
    };
    assert_ne!(done(1), done(2));
    assert_ne!(done(2), done(3));
    assert_eq!(done(3), done(3));
}

#[test]
fn normalize_loop_arg_drops_lone_digits_and_keeps_identifiers() {
    // Lone digits are counters; digits inside a longer token are part of
    // an identifier and survive (#1397).
    assert_eq!(normalize_loop_arg("attempt 1 of 6"), "attempt of");
    assert_eq!(
        normalize_loop_arg("gh pr merge 1394 --squash"),
        "gh pr merge 1394 squash"
    );
    assert_eq!(
        normalize_loop_arg("sed -n '490,570p' x.rs"),
        "sed n 490 570p x rs"
    );
    assert_eq!(normalize_loop_arg("git show b3b615ee"), "git show b3b615ee");
    assert_eq!(normalize_loop_arg("curl :8080/v1"), "curl 8080 v1");
}

#[test]
fn bash_pr_merges_with_different_numbers_stay_apart() {
    // The 2026-09-05 16:08 trip: merging PRs one by one was counted as one
    // call recurring four times and the pending merge was dropped (#1397).
    let merge = |n: u32| {
        normalized_call_signature(
            "bash",
            &json!({"command": format!("cd ~/srv/rs/opencrabs && gh pr merge {n} --squash --admin --delete-branch")}),
        )
    };
    assert_ne!(merge(1394), merge(1391));
    assert_ne!(merge(1391), merge(1390));
    // A literally re-issued merge (the call the nudge intercepted) still
    // collides with itself, so a genuine stuck merge is still counted.
    assert_eq!(merge(1390), merge(1390));
}

#[test]
fn bash_file_paging_ranges_stay_apart() {
    // The 16:40 and 16:52 trips: reading one file at different line ranges
    // while resolving a merge conflict (#1397).
    let page = |range: &str| {
        normalized_call_signature(
            "bash",
            &json!({"command": format!("sed -n '{range}' src/channels/telegram/agent.rs")}),
        )
    };
    assert_ne!(page("490,570p"), page("495,580p"));
    assert_ne!(page("100,150p"), page("200,250p"));
    assert_eq!(page("490,570p"), page("490,570p"));
}

#[test]
fn bash_commands_with_different_shas_stay_apart() {
    let show = |sha: &str| {
        normalized_call_signature(
            "bash",
            &json!({"command": format!("git show {sha} --stat")}),
        )
    };
    assert_ne!(show("b3b615ee"), show("d7e40ceb"));
    assert_eq!(show("b3b615ee"), show("b3b615ee"));
}

#[test]
fn bash_counter_loops_still_collide() {
    // The #957 contract survives the identifier change: a lone-digit
    // counter is still erased, so the Luna echo loop and "attempt N of 6"
    // keep collapsing to one signature.
    let echo = |n: u32| {
        normalized_call_signature(
            "bash",
            &json!({"command": format!("echo \"Отправляю {n} подтверждение в ДДС\"")}),
        )
    };
    assert_eq!(echo(1), echo(2));
    assert_eq!(echo(2), echo(9));
    let attempt = |n: u32| {
        normalized_call_signature(
            "bash",
            &json!({"command": format!("echo attempt {n} of 6")}),
        )
    };
    assert_eq!(attempt(1), attempt(5));
}

#[test]
fn identifiers_in_non_bash_string_args_stay_apart_too() {
    // The rule is per string argument, not per tool: an issue number in a
    // grep pattern or a numbered file name is an identifier everywhere.
    let g = |pat: &str| normalized_call_signature("grep", &json!({"pattern": pat, "path": "src"}));
    assert_ne!(g("#1394"), g("#1390"));
    let r = |f: &str| normalized_call_signature("read_file", &json!({"path": f}));
    assert_ne!(
        r("migrations/0042_users.sql"),
        r("migrations/0043_users.sql")
    );
}

#[test]
fn numeric_and_bool_fields_are_order_independent_and_exact() {
    // Numeric/bool params are kept exactly and sorted by key, so argument
    // insertion order never changes the signature (#82).
    let a = normalized_call_signature("bash", &json!({"command": "sleep 5", "timeout_secs": 120}));
    let b = normalized_call_signature("bash", &json!({"timeout_secs": 120, "command": "sleep 5"}));
    assert_eq!(a, b);
    let c = normalized_call_signature("bash", &json!({"command": "sleep 5", "timeout_secs": 60}));
    assert_ne!(a, c);
}

// ---- near_duplicate ----

#[test]
fn counter_variations_collide() {
    // The Luna pattern: same bash echo, only the counter moves.
    let a = "echo \"Отправляю 1 подтверждение в ДДС\"";
    let b = "echo \"Отправляю 2 подтверждение в ДДС\"";
    assert!(near_duplicate(a, b));

    let c = "Sending confirmation 3 of 6 to DDS with the PDF attachment";
    let d = "Sending confirmation 4 of 6 to DDS with the PDF attachment";
    assert!(near_duplicate(c, d));
}

#[test]
fn different_commands_do_not_collide() {
    assert!(!near_duplicate(
        "cargo build --release",
        "cargo test --all-features"
    ));
    assert!(!near_duplicate("git status", "ls -la"));
}

#[test]
fn short_texts_require_exact_normalized_match() {
    // Below 3 normalized words, only equality counts (Jaccard too coarse).
    assert!(near_duplicate("echo hi", "echo hi!"));
    assert!(!near_duplicate("echo hi", "echo yo"));
}

#[test]
fn similar_but_different_reports_do_not_collide() {
    // Legitimate templated status reports that differ where it matters.
    let a = "Deployed backend v1.2 to prod, all checks green";
    let b = "Deployed frontend v3.4 to staging, all checks green";
    assert!(!near_duplicate(a, b));
}

#[test]
fn empty_or_digit_only_text_is_not_a_duplicate() {
    assert!(!near_duplicate("", ""));
    assert!(!near_duplicate("12345", "12345"));
}

// ---- OutgoingTextRing (text layer) ----

#[test]
fn trip_sequence_nudges_then_aborts() {
    // The approved #957 acceptance sequence: clean -> clean -> trip-nudge
    // -> trip-abort. Counter-only variants count as near-duplicates.
    //
    // #2129 changed when the abort lands: one nudge is a retry, not a
    // verdict, so the ring nudges for five tripping turns and only cuts the
    // sixth off. Each text here is its own turn.
    let mut ring = OutgoingTextRing::default();
    let text = |n: u32| {
        format!("Отправляю {n} подтверждение по документу в ДДС и продолжаю обработку, ожидайте")
    };
    assert_eq!(ring.record_and_check(&text(1)), TextLoopAction::Continue);
    assert_eq!(ring.record_and_check(&text(2)), TextLoopAction::Continue);
    // Five tripping turns, five nudges. Each text is one turn.
    for n in 3..=7 {
        assert_eq!(
            ring.record_and_check(&text(n)),
            TextLoopAction::Nudge,
            "turn {n} must nudge, not abort"
        );
        ring.end_turn();
    }
    assert_eq!(ring.record_and_check(&text(8)), TextLoopAction::Abort);
}

#[test]
fn varied_genuine_texts_never_trip() {
    // No-false-positive: legitimately different turn outputs, including
    // templated status reports that differ where it matters.
    let mut ring = OutgoingTextRing::default();
    let texts = [
        "Deployed backend v1.2 to prod, all 42 checks green",
        "Deployed frontend v3.4 to staging, all 51 checks green",
        "Ran cargo clippy across the workspace, zero warnings",
        "Merged the session-store refactor after review",
        "Scheduled the nightly backup rotation job",
        "Summarized the standup notes and posted them",
    ];
    for t in texts {
        assert_eq!(
            ring.record_and_check(t),
            TextLoopAction::Continue,
            "text: {t}"
        );
    }
}

#[test]
fn ring_rotation_still_trips_after_old_entries_drop() {
    // The ring caps at 8 (#961): fill it with distinct texts first, then
    // start the loop. Old distinct entries fall out and the
    // near-duplicates still reach the trip threshold.
    let mut ring = OutgoingTextRing::default();
    for t in [
        "First unrelated answer about the config audit",
        "Second unrelated answer with the benchmark numbers",
        "Third unrelated answer summarizing the migration",
        "Fourth unrelated answer about the flaky test fix",
        "Fifth unrelated answer closing out the review",
        "Sixth unrelated answer about the onboarding flow",
        "Seventh unrelated answer covering the rate limits",
        "Eighth unrelated answer wrapping up the retrospective",
    ] {
        assert_eq!(ring.record_and_check(t), TextLoopAction::Continue);
    }
    let loopy = "Отправляю подтверждение по документу в ДДС и продолжаю обработку, ожидайте ";
    let first = format!("{loopy}1");
    let second = format!("{loopy}2");
    let third = format!("{loopy}3");
    assert_eq!(ring.record_and_check(&first), TextLoopAction::Continue);
    assert_eq!(ring.record_and_check(&second), TextLoopAction::Continue);
    assert_eq!(ring.record_and_check(&third), TextLoopAction::Nudge);
}

// ---- Luna fixture regression (#957) ----

const LUNA_FIXTURE: &str = include_str!("fixtures/luna_echo_loop.txt");

fn fixture_lines(prefix: &str) -> Vec<String> {
    LUNA_FIXTURE
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.starts_with(prefix))
        .map(|l| l[prefix.len()..].to_string())
        .collect()
}

#[test]
fn luna_bash_echoes_all_normalize_identically() {
    // Tool layer: every echo differs only by its counter, so all
    // normalized commands must collide — that is exactly what the
    // bash near-match in the tool loop counts.
    let cmds = fixture_lines("bash|");
    assert!(cmds.len() >= 6, "fixture must carry the full echo loop");
    let mut normalized = cmds.iter().map(|c| normalize_loop_text(c));
    let first = normalized.next().unwrap();
    assert!(!first.is_empty());
    for n in normalized {
        assert_eq!(n, first, "counter variant escaped the tool-layer net");
    }
}

#[test]
fn luna_announcements_trip_the_ring() {
    // Text layer: feeding Luna's reworded announcements through the ring
    // must nudge and, once the five retries are spent, fall back: the sequence
    // that ended the real incident with 18+ undelivered repeats.
    //
    // #2129: each fixture line is its own turn. A nudge buys the model the
    // next turn, so a clean line after a nudge is the nudge WORKING, not a
    // violation. What must hold is that an unbroken run of repeats ends in
    // the abort.
    let texts = fixture_lines("text|");
    assert!(texts.len() >= 6, "fixture must carry the announcement loop");
    let mut ring = OutgoingTextRing::default();
    let mut nudged_turns = 0;
    let mut aborted = false;
    // Each pass over the loop is one turn. A turn is nudged at most once, so
    // the count is per turn, not per record: every repeat inside a tripping
    // turn answers Nudge by design.
    for _ in 0..8 {
        let mut this_turn = TextLoopAction::Continue;
        for t in &texts {
            match ring.record_and_check(t) {
                TextLoopAction::Abort => {
                    this_turn = TextLoopAction::Abort;
                    break;
                }
                TextLoopAction::Nudge => this_turn = TextLoopAction::Nudge,
                TextLoopAction::Continue => {}
            }
        }
        if this_turn == TextLoopAction::Abort {
            aborted = true;
            break;
        }
        if this_turn == TextLoopAction::Nudge {
            nudged_turns += 1;
        }
        ring.end_turn();
    }
    assert_eq!(
        nudged_turns, 5,
        "the run must be nudged five times, not cut off"
    );
    assert!(
        aborted,
        "the run must end in the fallback after the retries"
    );
}

#[test]
fn one_noisy_turn_does_not_reach_the_abort() {
    // #2129's second half at the text layer: a single turn that repeats must
    // not walk the fallback chain before the model has read a single nudge.
    let texts = fixture_lines("text|");
    let mut ring = OutgoingTextRing::default();
    for t in &texts {
        assert_ne!(
            ring.record_and_check(t),
            TextLoopAction::Abort,
            "one turn must never abort: {t}"
        );
    }
}

// ---- #961 regression: DeepSeek v4 flash zip-send loop ----

#[test]
fn subset_announcement_counts_as_near_duplicate() {
    // The overlap-coefficient clause (#961): a short reworded
    // announcement whose words are almost all contained in a longer one
    // counts as a near-duplicate even though Jaccard is dragged down by
    // the length difference.
    assert!(near_duplicate(
        "Sending the zip to this thread now:",
        "Sending the zip:"
    ));
}

#[test]
fn overlap_clause_spares_unrelated_texts() {
    assert!(!near_duplicate(
        "Sending the zip to this thread now:",
        "Running the test suite again"
    ));
}

const ZIP_SEND_FIXTURE: &str = include_str!("fixtures/zip_send_announcement_loop.txt");

fn zip_send_fixture_lines(prefix: &str) -> Vec<String> {
    ZIP_SEND_FIXTURE
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.starts_with(prefix))
        .map(|l| l[prefix.len()..].to_string())
        .collect()
}

#[test]
fn deepseek_zip_send_announcements_nudge_then_abort() {
    // The text layer against the real #961 shape: the eight reworded
    // "sending now" announcements from the DeepSeek v4 flash zip-send
    // loop. With the overlap coefficient and the cap-8 ring the first
    // trip lands on the fourth announcement.
    //
    // #2129 moved the abort: one nudge is a retry now, so a single pass
    // through the eight announcements nudges and stops there. The cut-off
    // still exists, it just needs five ignored nudges first.
    let texts = zip_send_fixture_lines("text|");
    assert_eq!(texts.len(), 8, "fixture must carry all eight announcements");
    let mut ring = OutgoingTextRing::default();
    let expected = [
        TextLoopAction::Continue,
        TextLoopAction::Continue,
        TextLoopAction::Continue,
        TextLoopAction::Nudge,
        TextLoopAction::Continue,
        TextLoopAction::Continue,
        TextLoopAction::Continue,
        TextLoopAction::Nudge,
    ];
    for (t, want) in texts.iter().zip(&expected) {
        assert_eq!(ring.record_and_check(t), *want, "text: {t}");
    }
    // Same turn, so the whole pass spent exactly one retry.
    ring.end_turn();
    // Four more ignored nudges, then the fallback on the sixth turn.
    for turn in 2..=5 {
        for t in &texts {
            assert_ne!(
                ring.record_and_check(t),
                TextLoopAction::Abort,
                "turn {turn} aborted before the retries were spent: {t}"
            );
        }
        ring.end_turn();
    }
    assert_eq!(
        ring.record_and_check(&texts[0]),
        TextLoopAction::Abort,
        "the sixth nudged turn must reach the fallback"
    );
}

#[test]
fn deepseek_zip_send_tool_queries_are_reworded_not_identical() {
    // Documents the tool-layer gap from #961: the re-activation queries
    // were reworded every attempt, so none are identical even after
    // normalization — the exact-match guards never saw a repeat at all.
    // (The generalized near-match catches counter/punctuation variants;
    // these deep rewordings are the text layer's job.)
    let queries = zip_send_fixture_lines("tool_search|");
    assert!(
        queries.len() >= 7,
        "fixture must carry the re-activation spiral"
    );
    let mut seen = std::collections::HashSet::new();
    for q in &queries {
        let n = normalize_loop_text(q);
        assert!(
            !seen.contains(&n),
            "query repeat would invalidate the #961 shape: {n}"
        );
        seen.insert(n);
    }
}

// ---- pagination arguments (#199) ----

#[test]
fn pagination_arguments_in_strings_stay_apart() {
    // String cursors, page tokens, and page numbers must not collapse to
    // identical signatures during legitimate sequential pagination loops.
    let cursor_call = |c: &str| {
        normalized_call_signature(
            "github_api",
            &json!({"endpoint": "/repos/org/repo/issues", "cursor": c}),
        )
    };
    assert_ne!(cursor_call("cur-1"), cursor_call("cur-2"));
    assert_ne!(cursor_call("page_token_abc"), cursor_call("page_token_xyz"));
    assert_eq!(cursor_call("cur-1"), cursor_call("cur-1"));

    let page_str =
        |p: &str| normalized_call_signature("api_query", &json!({"page": p, "per_page": "50"}));
    assert_ne!(page_str("1"), page_str("2"));
    assert_ne!(page_str("2"), page_str("3"));
    assert_eq!(page_str("1"), page_str("1"));

    let offset_str = |off: &str| {
        normalized_call_signature(
            "db_query",
            &json!({"query": "SELECT * FROM t", "offset": off}),
        )
    };
    assert_ne!(offset_str("0"), offset_str("50"));
    assert_ne!(offset_str("50"), offset_str("100"));
    assert_eq!(offset_str("0"), offset_str("0"));

    let next_page_token =
        |tok: &str| normalized_call_signature("fetch_data", &json!({"next_page_token": tok}));
    assert_ne!(next_page_token("tok-1"), next_page_token("tok-2"));
    assert_eq!(next_page_token("tok-1"), next_page_token("tok-1"));

    let custom_suffix =
        |val: &str| normalized_call_signature("custom_tool", &json!({"user_cursor": val}));
    assert_ne!(custom_suffix("c1"), custom_suffix("c2"));
    assert_eq!(custom_suffix("c1"), custom_suffix("c1"));
}

#[test]
fn is_pagination_key_detection() {
    use crate::brain::agent::service::helpers::is_pagination_key;
    assert!(is_pagination_key("offset"));
    assert!(is_pagination_key("OFFSET"));
    assert!(is_pagination_key("page"));
    assert!(is_pagination_key("page_token"));
    assert!(is_pagination_key("next_page_token"));
    assert!(is_pagination_key("cursor"));
    assert!(is_pagination_key("next_cursor"));
    assert!(is_pagination_key("starting_after"));
    assert!(is_pagination_key("start_line"));
    assert!(is_pagination_key("token"));
    assert!(is_pagination_key("pagination_token"));
    assert!(is_pagination_key("after"));
    assert!(is_pagination_key("before"));
    assert!(is_pagination_key("skip"));
    assert!(is_pagination_key("marker"));
    assert!(is_pagination_key("continuation_token"));
    assert!(is_pagination_key("page_number"));
    assert!(is_pagination_key("page_idx"));
    assert!(is_pagination_key("page_index"));
    assert!(is_pagination_key("items_offset"));
    assert!(is_pagination_key("user_cursor"));
    assert!(is_pagination_key("feed_page"));
    assert!(is_pagination_key("continuation_page_token"));

    // Non-pagination keys must NOT match
    assert!(!is_pagination_key("command"));
    assert!(!is_pagination_key("query"));
    assert!(!is_pagination_key("attempt"));
    assert!(!is_pagination_key("path"));
}
