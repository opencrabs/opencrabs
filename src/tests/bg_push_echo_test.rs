//! #1221/#1225: the background-task echo bubble renderer + the
//! session_notify label plumbing. Pure assertions — no bot, no runtime:
//! framing strips (System + notify header), bubble assembly (rich markdown +
//! HTML fallback), truncation discipline (raw text cut BEFORE conversion,
//! wrapper tags stay intact).

use crate::brain::agent::BgTaskMeta;
use crate::channels::telegram::resume::{
    BubbleWire, NotifySender, background_task_title, build_bg_echo_bubble, build_bg_receipt_card,
    build_notify_receipt_card, split_bg_echo_parts, split_notify_header, strip_system_framing,
};
use uuid::Uuid;

const SENDER: &str = "6c1c9cb9-8243-4def-abe5-d926d0ca8bed";

#[test]
fn strips_notify_header_and_returns_sender() {
    let ctx = format!("[session-notify from={SENDER}]\n\nhello from the other topic");
    let (sender, body) = split_bg_echo_parts(&ctx);
    assert_eq!(
        sender,
        Some(NotifySender::Session(Uuid::parse_str(SENDER).unwrap()))
    );
    assert_eq!(body, "hello from the other topic");
}

#[test]
fn cli_sender_label_is_carried_verbatim() {
    // #23 (owner amendment "Overridable"): the CLI lane stamps
    // `from=cli:<label>` — no sender session exists, so the label rides the
    // header verbatim and the echo renders it without a session lookup.
    let ctx = "[session-notify from=cli:oc-deploy]\n\nbuild green";
    let (sender, body) = split_bg_echo_parts(ctx);
    assert_eq!(sender, Some(NotifySender::CliTooling("oc-deploy")));
    assert_eq!(body, "build green");
}

#[test]
fn cli_label_survives_surrounding_whitespace() {
    let ctx = "[session-notify from=cli: CI runner ]\n\nbody";
    let (sender, body) = split_notify_header(ctx);
    assert_eq!(sender, Some(NotifySender::CliTooling("CI runner")));
    assert_eq!(body, "body");
}

#[test]
fn empty_cli_label_is_malformed_framing() {
    let ctx = "[session-notify from=cli:]\n\nbody";
    let (sender, body) = split_notify_header(ctx);
    assert_eq!(sender, None, "an empty cli: label is not a sender");
    assert_eq!(body, ctx, "malformed header passes whole text through");
}

#[test]
fn split_notify_header_rejects_malformed_framing() {
    let bad_uuid = "[session-notify from=not-a-uuid]\n\nbody";
    let (sender, body) = split_notify_header(bad_uuid);
    assert_eq!(sender, None);
    assert_eq!(
        body, bad_uuid,
        "malformed header must pass whole text through"
    );
    let no_close = "[session-notify from=6c1c9cb9-8243-4def-abe5-d926d0ca8bed";
    let (sender, body) = split_notify_header(no_close);
    assert_eq!(sender, None);
    assert_eq!(body, no_close);
}

#[test]
fn strips_terminated_system_framing() {
    // Real framing shape (background_tasks.rs): block ends with ']'.
    let ctx = "[System: the background task you started has finished.\nStatus: exit 0]\nreal tail";
    let (sender, body) = split_bg_echo_parts(ctx);
    assert_eq!(sender, None, "background tasks carry no sender");
    assert!(!body.contains("[System:"), "scaffolding must not render");
    assert!(body.contains("Status: exit 0"), "inner content survives");
    assert!(body.contains("real tail"));
}

#[test]
fn system_framing_without_closing_brace_passes_through() {
    let ctx = "[System: task finished]\nsome output";
    let inner = strip_system_framing(ctx);
    assert_eq!(
        inner, ctx,
        "the ']' must terminate the block to be stripped"
    );
}

#[test]
fn strips_system_framing_even_after_notify_header() {
    let ctx =
        format!("[session-notify from={SENDER}]\n\n[System: the push you asked for]\npushed body");
    let (sender, body) = split_bg_echo_parts(&ctx);
    assert!(sender.is_some());
    assert!(
        !body.contains("[System:"),
        "System framing stripped after header"
    );
    assert!(body.contains("pushed body"));
}

#[test]
fn absent_framing_passes_through_untouched() {
    let (sender, body) = split_bg_echo_parts("plain text, no framing");
    assert_eq!(sender, None);
    assert_eq!(body, "plain text, no framing");
}

#[test]
fn bubble_wraps_in_blockquote_with_bold_header() {
    let (wire, html) = build_bg_echo_bubble("some output", "📨 Ops / Push to session");
    let BubbleWire::Markdown(markdown) = &wire else {
        panic!("plain echo bubble rides the markdown outbox wire");
    };
    assert!(markdown.contains("📨 Ops / Push to session"));
    assert!(markdown.contains("some output"));
    assert!(html.starts_with("<blockquote expandable>"));
    assert!(html.ends_with("</blockquote>"));
    assert!(html.contains("<b>📨 Ops / Push to session</b>"));
    assert!(html.contains("some output"));
}

#[test]
fn background_task_title_is_preserved() {
    let (_, html) = build_bg_echo_bubble("finished ok", "⚙️ background task result");
    assert!(html.contains("<b>⚙️ background task result</b>"));
}

#[test]
fn rich_markdown_keeps_fences() {
    let ctx = "# Heading\n```rust\nfn main() {}\n```";
    let (wire, _) = build_bg_echo_bubble(ctx, "📨 Team");
    let BubbleWire::Markdown(markdown) = wire else {
        panic!("plain echo bubble rides the markdown outbox wire");
    };
    assert!(markdown.contains("```rust"));
    assert!(markdown.contains("# Heading"));
}

#[test]
fn long_output_is_truncated_before_conversion_and_stays_wellformed() {
    let big = format!("{{}}\n{}", "y".repeat(10_000));
    let (wire, html) = build_bg_echo_bubble(&big, "⚙️ background task result");
    let BubbleWire::Markdown(markdown) = &wire else {
        panic!("plain echo bubble rides the markdown outbox wire");
    };
    assert!(markdown.contains("(truncated)"));
    assert!(html.contains("(truncated)"));
    // Truncating raw text first means the wrapper tags can never be cut:
    assert!(html.starts_with("<blockquote expandable>"));
    assert!(html.ends_with("</blockquote>"));
}

#[test]
fn background_title_names_the_task_from_display_text() {
    assert_eq!(
        background_task_title("🔧 background task finished: grep-errors"),
        "🔧 background task finished: grep-errors"
    );
    assert_eq!(
        background_task_title("🔧 background task failed: cleanup-unified"),
        "🔧 background task failed: cleanup-unified"
    );
}

#[test]
fn blank_display_text_falls_back_to_generic_title() {
    assert_eq!(background_task_title("   "), "⚙️ background task result");
    assert_eq!(background_task_title(""), "⚙️ background task result");
}

#[test]
fn overlong_task_label_is_capped_in_title() {
    let long = format!("🔧 background task finished: {}", "x".repeat(500));
    let t = background_task_title(&long);
    assert!(t.chars().count() <= 120, "header must stay readable");
    assert!(t.starts_with("🔧 background task finished:"));
}

#[test]
fn html_fallback_escapes_dynamic_title() {
    let (_, html) = build_bg_echo_bubble("body", "📨 Ops <script> / Push");
    assert!(html.contains("<b>📨 Ops &lt;script&gt; / Push</b>"));
    assert!(!html.contains("<script>"), "title must not inject raw HTML");
}

#[test]
fn bubble_md_and_classic_stay_separate() {
    let (wire, classic) = build_bg_echo_bubble("body", "T");
    let BubbleWire::Markdown(md) = wire else {
        panic!("plain echo bubble rides the markdown outbox wire");
    };
    assert!(
        classic.contains("blockquote expandable"),
        "fallback stays classic-dialect"
    );
    assert!(
        !md.contains('<'),
        "markdown leg stays tag-free for the rich parser"
    );
}

// ---- #15 receipt cards (owner-locked shapes P3f / N4) ----

fn meta(success: bool, label: &str, secs: f32, tail: &str) -> BgTaskMeta {
    BgTaskMeta {
        task_id: "t-1".to_string(),
        success,
        label: label.to_string(),
        elapsed_secs: secs,
        tail: tail.to_string(),
    }
}

#[test]
fn bg_receipt_card_matches_the_locked_p3f_shape() {
    let (wire, classic) = build_bg_receipt_card(&meta(
        true,
        "gh run watch 33117665576",
        1646.0,
        "line one\nline two",
    ));
    let BubbleWire::Html(rich) = &wire else {
        panic!("chrome card rides the HTML rich wire (#85)");
    };
    assert!(
        rich.starts_with(
            "<details><summary><sub>✅ <code>gh run watch 33117665576</code> 🕒 27m 26s</sub></summary>"
        ),
        "summary = icon + monospace roster label + clock + duration, whole line subbed: {rich}"
    );
    assert!(
        rich.contains("<pre>line one\nline two</pre>"),
        "body is ONE pre block with the tail verbatim: {rich}"
    );
    assert!(rich.ends_with("</details>"));
    assert!(
        !rich.contains("exit"),
        "no exit code / wording in the bubble"
    );
    // Collapsed by default — never emit <details open> (#85 locked rule).
    assert!(
        !rich.contains("<details open"),
        "collapsible ships collapsed"
    );
    // Degraded path stays a classic blockquote carrying the same content.
    assert!(classic.starts_with("<blockquote expandable>"));
    assert!(classic.contains("gh run watch 33117665576"));
    assert!(classic.contains("line one"));
}

#[test]
fn bg_receipt_card_failure_uses_the_cross_icon() {
    let (wire, _) = build_bg_receipt_card(&meta(false, "cargo test", 3.0, "boom"));
    let BubbleWire::Html(rich) = wire else {
        panic!("chrome card rides the HTML rich wire (#85)");
    };
    assert!(
        rich.starts_with("<details><summary><sub>❌ <code>cargo test</code> 🕒 3s</sub></summary>")
    );
}

#[test]
fn bg_receipt_card_strips_backticks_from_the_label() {
    let (wire, _) = build_bg_receipt_card(&meta(true, "cat `file`.md", 1.0, "ok"));
    let BubbleWire::Html(rich) = wire else {
        panic!("chrome card rides the HTML rich wire (#85)");
    };
    assert!(
        rich.contains("<code>cat file.md</code>"),
        "label backticks stripped so the code span stays intact: {rich}"
    );
}

#[test]
fn bg_receipt_card_fence_outgrows_backtick_runs_in_the_tail() {
    // On the HTML rich leg containment comes from the <pre> tag, so the
    // tail ships verbatim with no fence at all.
    // On the classic leg, receipt_fence's backtick arms-race survives markdown_to_html
    // because the parser length-matches fences (#94).
    let tail = "look:\n```\nnested fence\n```\ndone";
    let (wire, classic) = build_bg_receipt_card(&meta(true, "cat README.md", 2.0, tail));
    let BubbleWire::Html(rich) = wire else {
        panic!("chrome card rides the HTML rich wire (#85)");
    };
    assert!(
        rich.contains("<pre>look:\n```\nnested fence\n```\ndone</pre>"),
        "pre containment ships the tail verbatim, fence-free: {rich}"
    );
    assert!(
        classic.contains("<pre><code>look:\n```\nnested fence\n```\ndone</code></pre>"),
        "classic leg: the compose-time arms-race fence survives markdown_to_html \
         now that parser has fence length-matching (#94): {classic}"
    );
}

#[test]
fn bg_receipt_card_rich_leg_escapes_the_tail() {
    // #85: the tail is raw process output — a '<' in it must not open a
    // tag inside the <details> fold.
    let (wire, _) = build_bg_receipt_card(&meta(true, "render", 1.0, "<b>&raw</b>"));
    let BubbleWire::Html(rich) = wire else {
        panic!("chrome card rides the HTML rich wire (#85)");
    };
    assert!(
        rich.contains("<pre>&lt;b&gt;&amp;raw&lt;/b&gt;</pre>"),
        "pre content is escaped: {rich}"
    );
    assert!(
        !rich.contains("<b>&raw"),
        "tail cannot open a tag inside the fold"
    );
}

#[test]
fn bg_receipt_card_empty_tail_is_a_flat_one_liner() {
    // A whitespace-only tail leaves nothing inside the <details> wrapper —
    // the rich API rejects the card with RICH_MESSAGE_EMPTY and the outbox
    // fallback escapes the literal wrapper tags into the chat. The guard
    // must drop the card to the flat markdown wire with no wrapper on
    // either leg.
    for tail in ["", "   ", "\n\t"] {
        let (wire, classic) = build_bg_receipt_card(&meta(true, "gh run watch 1", 61.0, tail));
        let BubbleWire::Markdown(md) = &wire else {
            panic!("empty-tail card must ride the flat markdown wire (#38 guard)");
        };
        assert!(!md.contains("<details>"), "no wrapper to reject: {md}");
        assert!(
            !classic.contains("<details>"),
            "no wrapper to leak: {classic}"
        );
        assert!(
            md.starts_with("✅ `gh run watch 1` 🕒 "),
            "flat markdown card: {md}"
        );
        assert!(!md.contains('\n'), "one line only: {md}");
        assert!(
            classic.starts_with("<b>✅ gh run watch 1 🕒 "),
            "flat classic card: {classic}"
        );
    }
}

#[test]
fn bg_receipt_card_empty_tail_escapes_the_label_on_the_classic_leg() {
    let (wire, classic) = build_bg_receipt_card(&meta(false, "grep <b>", 1.0, ""));
    let BubbleWire::Markdown(md) = wire else {
        panic!("empty-tail card must ride the flat markdown wire (#38 guard)");
    };
    assert!(
        classic.contains("&lt;b&gt;"),
        "classic leg escapes the label: {classic}"
    );
    assert!(
        !classic.contains("<b>grep"),
        "label cannot open a tag: {classic}"
    );
    assert!(
        md.starts_with("❌ `grep <b>` 🕒 "),
        "markdown leg keeps the label verbatim: {md}"
    );
}

#[tokio::test]
async fn notify_receipt_card_matches_the_locked_n4_shape() {
    let body = "RECEIPT CONTRACT DELIVERED — swap verified, all three clauses journal-anchored.\n\n\
                | Clause | Anchor |\n|---|---|\n| Build | run 1 |";
    let (wire, classic) = build_notify_receipt_card("Compiler", body).await;
    let BubbleWire::Html(rich) = &wire else {
        panic!("notify card rides the HTML rich wire (#85)");
    };
    assert!(
        rich.starts_with(
            "<details><summary><sub>📨 From <b>Compiler</b>: RECEIPT CONTRACT DELIVERED — swap \
             verified, a…</sub></summary>"
        ),
        "summary = 📨 + From + bold sender + colon + 45-char first-line preview, whole line subbed: {rich}"
    );
    assert!(
        rich.contains("<pre>"),
        "markdown body renders through the HTML dialect (table as grid/pre): {rich}"
    );
    assert!(!rich.contains("```"), "notify body is never fenced");
    assert!(rich.ends_with("</details>"));
    // Collapsed by default — never emit <details open> (#85 locked rule).
    assert!(
        !rich.contains("<details open"),
        "collapsible ships collapsed"
    );
    assert!(classic.starts_with("<blockquote expandable>"));
    assert!(classic.contains("Compiler"));
}

#[tokio::test]
async fn notify_receipt_card_sanitizes_angle_brackets_in_sender() {
    let (wire, _) = build_notify_receipt_card("Ops <script>", "body line").await;
    let BubbleWire::Html(rich) = wire else {
        panic!("notify card rides the HTML rich wire (#85)");
    };
    assert!(
        rich.contains("<b>Ops ‹script›</b>: body line"),
        "angle brackets neutralized so the sender can't open a tag: {rich}"
    );
    assert!(!rich.contains("<script>"));
}

#[tokio::test]
async fn notify_receipt_card_empty_body_is_a_flat_one_liner() {
    // Same defect class as the bg card: a whitespace-only body leaves an
    // empty card inside the <details> wrapper (RICH_MESSAGE_EMPTY). The
    // guard must drop the card to the flat markdown wire with no wrapper
    // on either leg.
    for body in ["", "  \n\t "] {
        let (wire, classic) = build_notify_receipt_card("Compiler", body).await;
        let BubbleWire::Markdown(md) = &wire else {
            panic!("empty-body card must ride the flat markdown wire (#38 guard)");
        };
        assert!(!md.contains("<details>"), "no wrapper to reject: {md}");
        assert!(
            !classic.contains("<details>"),
            "no wrapper to leak: {classic}"
        );
        assert_eq!(md, "📨 From **Compiler**", "flat markdown card");
        assert_eq!(classic, "📨 From <b>Compiler</b>", "flat classic card");
    }
}

#[tokio::test]
async fn notify_preview_truncates_the_first_line_only() {
    let body = format!("{}\nsecond line stays in the body", "x".repeat(80));
    let (wire, _) = build_notify_receipt_card("Worker", &body).await;
    let BubbleWire::Html(rich) = wire else {
        panic!("notify card rides the HTML rich wire (#85)");
    };
    let preview = format!("{}…", "x".repeat(45));
    assert!(
        rich.contains(&format!(": {preview}</sub>")),
        "preview = first line truncated to 45 chars + ellipsis: {rich}"
    );
    assert!(
        rich.contains("second line stays in the body"),
        "the full body survives inside the fold"
    );
}

/// #85: the notify card rides the HTML rich wire — its body renders through
/// markdown_to_html_mermaid_p, so pipe tables ship as rendered grid markup
/// inside the collapsible (Telegram's HTML dialect has no <table> tag; the
/// markdown-ladder parse test this replaced rode the retired outbox route).
#[tokio::test]
async fn notify_receipt_card_keeps_tables_native_inside_the_fold() {
    let body = "| a | b |\n|---|---|\n| 1 | 2 |";
    let (wire, _) = build_notify_receipt_card("Compiler", body).await;
    let BubbleWire::Html(rich) = wire else {
        panic!("notify card rides the HTML rich wire (#85)");
    };
    assert!(
        rich.contains("<details><summary>"),
        "wrapper intact: {rich}"
    );
    assert!(
        rich.contains("<pre>a | b</pre>") || rich.contains("a | b"),
        "body keeps the rendered table grid: {rich}"
    );
    assert!(rich.contains("</details>"), "wrapper closes");
}

/// #490: a notify past the classic 3200 budget used to lose its TAIL — which
/// is exactly where the Disclosures and What-now/next blocks live, so the owner
/// lost the part written for him while the sending lane still read
/// `Delivered`. The rich Html wire carries ~32K, so it takes
/// `ECHO_BODY_CAP_CHARS_RICH`; only the classic fallback is cut, and it is
/// cut exactly ONCE (the old re-cap fed an already-cut body back through the
/// same guard, which could print a second `(truncated)` marker).
///
/// Three sentinels make the leg split observable without parsing markup:
/// `INSIDE-CAP` at ~2000 (inside the classic budget), `OUTSIDE-CAP!` at ~3300
/// (outside it), and the tail sentinel in the last 35 chars.
#[tokio::test]
async fn notify_receipt_card_keeps_a_4719_char_body_whole_on_the_rich_leg() {
    // 4719 chars is the issue's own measured instance length (#490).
    const TOTAL: usize = 4719;
    let head = "a".repeat(2000);
    let inside = "INSIDE-CAP";
    let outside = "OUTSIDE-CAP!";
    let tail = "TAIL-SENTINEL-#490-DISCLOSURES-HERE";
    // Place the sentinels so `inside` sits above the classic budget and
    // `outside` below it, then fill exactly to the measured length. The
    // arithmetic is recomputed here and asserted below, so a drift in any
    // piece fails the test loudly instead of silently weakening it.
    let outside_at = 3300usize;
    let tail_at = TOTAL - tail.len();
    let gap1 = "b".repeat(outside_at - (head.len() + inside.len()));
    let gap2 = "c".repeat(tail_at - (outside_at + outside.len()));
    let body = format!("{head}{inside}{gap1}{outside}{gap2}{tail}");
    assert_eq!(
        body.chars().count(),
        TOTAL,
        "fixture is the measured length"
    );

    let (wire, classic) = build_notify_receipt_card("Compiler", &body).await;
    let BubbleWire::Html(rich) = &wire else {
        panic!("notify card rides the HTML rich wire (#85)");
    };

    // RICH LEG — 4719 is under the 30000 rich budget, so no cut fires at all:
    // the whole body survives, its last 40 characters included.
    let last40: String = body.chars().skip(TOTAL - 40).collect();
    assert!(
        rich.contains(&last40),
        "rich payload must carry the body's LAST 40 chars: {last40:?}"
    );
    assert!(
        rich.contains(tail),
        "the tail sentinel survives on the rich leg — this is the #490 defect"
    );
    assert!(
        !rich.contains("(truncated)"),
        "no cut fires under the rich budget, so the rich leg carries no marker"
    );

    // CLASSIC FALLBACK — its own budget still governs, and it is cut ONCE.
    assert!(
        classic.contains(inside),
        "content inside the classic budget survives on the fallback"
    );
    assert!(
        !classic.contains(outside),
        "content past the classic budget is dropped on the fallback, proving the cap still binds at ~3200"
    );
    // Task 3 CHANGED THIS ASSERTION. It first read `!classic.contains(tail)`,
    // pinning the old head-only cut; the tail-preserving helper now keeps both
    // ends on BOTH legs, so the fallback retains the ending too. Asserting the
    // new behaviour is the point — the old assertion would have locked the
    // #490 defect in place on the leg with the smaller budget.
    assert!(
        classic.contains(tail),
        "the classic leg now keeps its tail too; only the MIDDLE is dropped"
    );
    assert!(
        classic.contains("(truncated 1559 chars)"),
        "the classic leg states the count it dropped (4719 in, 3160 kept as head+tail: 1559 went)"
    );
    assert_eq!(
        classic.matches("(truncated)").count(),
        1,
        "the classic fallback carries EXACTLY one truncation marker (the #490 re-cap printed a second)"
    );
}

/// #490: when a cap DOES fire, it keeps the tail. The head-only cut dropped the
/// ending, which is where the Disclosures and What-now/next blocks live — the
/// part written for the reader. Both legs have budgets, so both must preserve.
#[test]
fn a_firing_cap_keeps_the_tail_and_states_the_dropped_count_on_both_legs() {
    const TOTAL: usize = 5000; // over the classic 3200, under the rich 30000
    let head = "HEAD-SENTINEL";
    let tail = "TAIL-SENTINEL-NEEDS-FROM-YOU";
    let body = format!(
        "{head}{}{tail}",
        "m".repeat(TOTAL - head.len() - tail.len())
    );
    assert_eq!(body.chars().count(), TOTAL);

    let (wire, html) = build_bg_echo_bubble(&body, "⚙️ background task result");
    let BubbleWire::Markdown(markdown) = &wire else {
        panic!("plain echo bubble rides the markdown outbox wire");
    };

    // 5000 in, 3160 kept as head+tail, so 1840 chars went — stated once.
    for (leg, payload) in [("markdown", markdown), ("classic html", &html)] {
        assert!(
            payload.contains(head),
            "{leg}: the opening survives: {}",
            &payload[..payload.len().min(120)]
        );
        assert!(
            payload.contains(tail),
            "{leg}: the TAIL survives a firing cap — the #490 defect was this being dropped"
        );
        assert!(
            payload.contains("(truncated 1840 chars)"),
            "{leg}: the marker states the dropped COUNT"
        );
    }
    // Wrapper safety: cutting RAW text before conversion is what keeps the
    // classic wrapper well-formed (the plan_card lesson).
    assert!(html.starts_with("<blockquote expandable>"));
    assert!(html.ends_with("</blockquote>"));
}

/// #490, second case: a body ABOVE the rich budget (30000) is cut — and its
/// tail STILL survives, which is the behaviour the head-only cut lost at every
/// budget, not just the classic one.
#[tokio::test]
async fn above_the_rich_budget_the_tail_still_survives() {
    const TOTAL: usize = 40000;
    let head = "HEAD-SENTINEL";
    let tail = "TAIL-SENTINEL-DISCLOSURES-BLOCK";
    let body = format!(
        "{head}{}{tail}",
        "m".repeat(TOTAL - head.len() - tail.len())
    );
    assert_eq!(body.chars().count(), TOTAL);

    let (wire, classic) = build_notify_receipt_card("Compiler", &body).await;
    let BubbleWire::Html(rich) = &wire else {
        panic!("notify card rides the HTML rich wire (#85)");
    };

    assert!(rich.contains(head), "the opening survives the rich cut");
    assert!(
        rich.contains(tail),
        "the tail survives even above the rich budget — 40000 in, 29960 kept, 10040 dropped"
    );
    assert!(
        rich.contains("(truncated 10040 chars)"),
        "the rich leg states its own dropped count"
    );
    assert!(
        rich.contains("(truncated)"),
        "the card's headline suffix also flags the cut"
    );
    // The suffix states no count, so the number appears exactly ONCE.
    assert_eq!(
        rich.matches("(truncated ").count(),
        1,
        "one count, adjacent to the material it describes — not repeated as chrome"
    );
    // The classic fallback is cut harder, and still keeps its tail.
    assert!(
        classic.contains(tail),
        "classic fallback keeps its tail too"
    );
}

/// #490 task 4 — the delivery verdict must TELL the sender its payload was
/// shortened. Before this, a notify whose tail had been cut still answered a
/// bare `Delivered`, so the sending lane could not learn that the blocks
/// written FOR the receiving lane (Disclosures, What-now/next) never arrived.
///
/// The verdict names BOTH legs with their own budgets, because the leg is not
/// known at verdict time: the card takes the rich wire and falls back to the
/// classic one only if that call fails.
#[tokio::test]
async fn notify_verdict_reports_what_each_leg_would_drop() {
    use crate::utils::echo_budget::{
        ECHO_BODY_CAP_CHARS, ECHO_BODY_CAP_CHARS_RICH, sender_over_cap_signal,
    };

    // 4719 chars is the issue's own measured instance length (#490).
    const TOTAL: usize = 4719;
    // 4719 through the classic cap: 3200 budget less the 40-char marker
    // reserve = 3160 kept as head+tail (head 2106 / tail 1054), so 1559 go.
    // Pinned here so a drift in the reserve, the split or the budget fails
    // loudly instead of quietly weakening the assertion.
    const DROPPED_ON_CLASSIC: usize = 1559;

    let body: String = "x".repeat(TOTAL);

    // Drive it through the render path exactly as a notify push does.
    let (wire, classic) = build_notify_receipt_card("Compiler", &body).await;
    assert!(
        matches!(wire, BubbleWire::Html(_)),
        "the notify card rides the rich HTML wire — the leg whose budget #490 split"
    );

    // THE VERDICT: zero dropped on the rich leg, the real count on the classic
    // fallback, and each leg named with the budget that governs it.
    let signal = sender_over_cap_signal(&body);
    let expected = format!(
        " Payload {TOTAL} chars \u{2014} rich leg ({ECHO_BODY_CAP_CHARS_RICH} budget) drops 0; \
         classic fallback ({ECHO_BODY_CAP_CHARS} budget) drops {DROPPED_ON_CLASSIC}."
    );
    assert_eq!(
        signal, expected,
        "the verdict must expose 0 dropped on the rich leg and the classic count, naming both legs"
    );

    // The warning must AGREE with the bytes the reader received — a warning
    // that disagrees with the delivered payload is worse than no warning.
    assert!(
        classic.contains(&format!("(truncated {DROPPED_ON_CLASSIC} chars)")),
        "the verdict's classic count must match the marker the fallback renders"
    );

    // NO CUT, NO NOISE. A body inside both budgets keeps the verdict it always
    // had, so the warning stays a signal rather than chrome on every send.
    assert_eq!(sender_over_cap_signal("short body"), "");
    assert_eq!(
        sender_over_cap_signal(&"y".repeat(ECHO_BODY_CAP_CHARS)),
        "",
        "a body exactly AT the classic budget is not over it"
    );
}
