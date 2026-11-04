//! Tests for the Telegram rich-message markdown front-end: the schema-
//! independent AST parser ([`parse_markdown`]) and the HTML fallback renderer
//! ([`markdown_to_html`]). The rich-first `InputRichMessage` serializer is
//! finalized separately against the Bot API field schema and tested there.

use crate::channels::telegram::resume::BubbleWire;
use crate::channels::telegram::rich::ast::{Align, Block, Inline};
use crate::channels::telegram::rich::detect::has_rich_structure;
use crate::channels::telegram::rich::parse::parse_markdown;
use crate::channels::telegram::rich::{contains_table, markdown_to_html, prefers_rich_render};

fn text(s: &str) -> Inline {
    Inline::Text(s.to_string())
}

// ── inline parsing ──────────────────────────────────────────────────

#[test]
fn inline_bold_italic_code_link() {
    let blocks = parse_markdown("a **b** _c_ `d` [e](http://x)");
    let Block::Paragraph(inl) = &blocks[0] else {
        panic!("expected paragraph, got {:?}", blocks[0]);
    };
    assert_eq!(
        inl,
        &vec![
            text("a "),
            Inline::Bold(vec![text("b")]),
            text(" "),
            Inline::Italic(vec![text("c")]),
            text(" "),
            Inline::Code("d".to_string()),
            text(" "),
            Inline::Link {
                content: vec![text("e")],
                url: "http://x".to_string(),
            },
        ]
    );
}

#[test]
fn unbalanced_delimiters_stay_literal() {
    let blocks = parse_markdown("a **b c");
    let Block::Paragraph(inl) = &blocks[0] else {
        panic!("expected paragraph");
    };
    assert_eq!(inl, &vec![text("a **b c")]);
}

#[test]
fn inline_code_is_not_reparsed() {
    let blocks = parse_markdown("`**not bold**`");
    let Block::Paragraph(inl) = &blocks[0] else {
        panic!("expected paragraph");
    };
    assert_eq!(inl, &vec![Inline::Code("**not bold**".to_string())]);
}

#[test]
fn snake_case_underscores_stay_literal() {
    // Intra-word underscores must NOT be treated as italic emphasis, or
    // `custom_openai_compatible` renders as "customopenaicompatible".
    let blocks = parse_markdown("the custom_openai_compatible provider");
    let Block::Paragraph(inl) = &blocks[0] else {
        panic!("expected paragraph");
    };
    assert_eq!(inl, &vec![text("the custom_openai_compatible provider")]);
}

#[test]
fn word_bounded_underscore_still_italicizes() {
    let blocks = parse_markdown("use _this_ word");
    let Block::Paragraph(inl) = &blocks[0] else {
        panic!("expected paragraph");
    };
    assert_eq!(
        inl,
        &vec![
            text("use "),
            Inline::Italic(vec![text("this")]),
            text(" word"),
        ]
    );
}

#[test]
fn blocks_are_separated_by_blank_lines() {
    // Paragraph/heading/list spacing must survive the AST renderer, not be
    // collapsed to single newlines (the cramped-prose regression).
    let html = markdown_to_html("# Heading\n\nFirst para.\n\n- a\n- b");
    assert!(
        html.contains("</b>\n\nFirst para.\n\n• a"),
        "blocks need blank-line separation: {html:?}"
    );
}

// ── headings ────────────────────────────────────────────────────────

#[test]
fn atx_headings_carry_level() {
    let blocks = parse_markdown("# Title\n\n### Sub");
    assert_eq!(
        blocks,
        vec![
            Block::Heading {
                level: 1,
                content: vec![text("Title")],
            },
            Block::Heading {
                level: 3,
                content: vec![text("Sub")],
            },
        ]
    );
}

#[test]
fn hash_without_space_is_not_a_heading() {
    let blocks = parse_markdown("#hashtag");
    assert_eq!(blocks, vec![Block::Paragraph(vec![text("#hashtag")])]);
}

// ── lists (nesting + ordered + tasks) ───────────────────────────────

#[test]
fn nested_bullet_list() {
    let blocks = parse_markdown("- a\n- b\n  - b1\n  - b2\n- c");
    let Block::List(list) = &blocks[0] else {
        panic!("expected list, got {:?}", blocks[0]);
    };
    assert!(!list.ordered);
    assert_eq!(list.items.len(), 3);
    // Second item carries a nested list of two children.
    let Block::List(child) = &list.items[1].children[0] else {
        panic!("expected nested list under item b");
    };
    assert_eq!(child.items.len(), 2);
    assert_eq!(child.items[0].content, vec![text("b1")]);
}

#[test]
fn ordered_list_is_ordered() {
    let blocks = parse_markdown("1. one\n2. two");
    let Block::List(list) = &blocks[0] else {
        panic!("expected list");
    };
    assert!(list.ordered);
    assert_eq!(list.items.len(), 2);
}

#[test]
fn task_list_checkboxes() {
    let blocks = parse_markdown("- [ ] todo\n- [x] done");
    let Block::List(list) = &blocks[0] else {
        panic!("expected list");
    };
    assert_eq!(list.items[0].task, Some(false));
    assert_eq!(list.items[1].task, Some(true));
    assert_eq!(list.items[1].content, vec![text("done")]);
}

// ── tables ──────────────────────────────────────────────────────────

#[test]
fn pipe_table_with_alignment() {
    let md = "| Name | Qty |\n| :--- | ---: |\n| Apple | 3 |\n| Pear | 12 |";
    let blocks = parse_markdown(md);
    let Block::Table(t) = &blocks[0] else {
        panic!("expected table, got {:?}", blocks[0]);
    };
    assert_eq!(t.header.len(), 2);
    assert_eq!(t.align, vec![Align::Left, Align::Right]);
    assert_eq!(t.rows.len(), 2);
    assert_eq!(t.rows[0][0], vec![text("Apple")]);
}

#[test]
fn contains_table_detects_only_real_tables() {
    assert!(contains_table("| a | b |\n| - | - |\n| 1 | 2 |"));
    // A lone pipe line without a separator is not a table.
    assert!(!contains_table("a | b is just prose"));
    assert!(!contains_table("# heading\n\nsome text"));
}

#[test]
fn prefers_rich_render_for_tables_and_task_lists() {
    assert!(prefers_rich_render("| a | b |\n| - | - |\n| 1 | 2 |"));
    assert!(prefers_rich_render("- [ ] todo\n- [x] done"));
    assert!(prefers_rich_render("  * [x] indented task"));
    // Plain prose and ordinary bullet lists stay on the legacy path.
    assert!(!prefers_rich_render(
        "# heading\n\n- a normal bullet\n- another"
    ));
    assert!(!prefers_rich_render(
        "just a sentence with [brackets] in it"
    ));
}

// ── HTML fallback rendering ─────────────────────────────────────────

#[test]
fn table_renders_as_aligned_pre_grid() {
    let md = "| A | B |\n| - | - |\n| 1 | 22 |";
    let html = markdown_to_html(md);
    assert!(
        html.starts_with("<pre>"),
        "table must be a <pre> block: {html}"
    );
    // Header's second column is padded to the width of the widest cell ("22").
    assert!(html.contains("A | B "), "header row not padded: {html}");
    assert!(html.contains("1 | 22"), "data row missing: {html}");
}

#[test]
fn wide_table_renders_as_cards() {
    // A 3-column table too wide for a phone becomes one card per row.
    let md = "| Task | Owner | Status |\n| - | - | - |\n\
              | Driver App release | Alexander | In progress now |";
    let html = markdown_to_html(md);
    assert!(
        !html.contains("<pre>"),
        "wide table must not be a grid: {html}"
    );
    assert!(
        html.contains("<b>Driver App release</b>"),
        "first cell is the bold card title: {html}"
    );
    assert!(
        html.contains("Owner: Alexander"),
        "field line missing: {html}"
    );
    assert!(
        html.contains("Status: In progress now"),
        "field line missing: {html}"
    );
}

#[test]
fn wide_two_column_table_renders_as_key_value() {
    // A wide 2-column table collapses to a `key: value` list (no header row).
    let md = "| Metric | Value |\n| - | - |\n\
              | Total commits since the v0.3.40 release | 1052 |";
    let html = markdown_to_html(md);
    assert!(
        !html.contains("<pre>"),
        "wide 2-col must not be a grid: {html}"
    );
    assert!(
        html.contains("<b>Total commits since the v0.3.40 release</b>: 1052"),
        "key/value line missing: {html}"
    );
    // Header row ("Metric: Value") is dropped — columns are self-labelling.
    assert!(
        !html.contains("Metric"),
        "header row should be dropped: {html}"
    );
}

#[test]
fn heading_renders_bold() {
    assert_eq!(markdown_to_html("# Hi"), "<b>Hi</b>");
    assert_eq!(markdown_to_html("### Deep"), "<b><i>Deep</i></b>");
}

#[test]
fn html_special_chars_are_escaped() {
    let html = markdown_to_html("a < b & c > d");
    assert_eq!(html, "a &lt; b &amp; c &gt; d");
}

#[test]
fn task_list_renders_checkboxes() {
    let html = markdown_to_html("- [ ] todo\n- [x] done");
    assert!(html.contains("☐ todo"), "{html}");
    assert!(html.contains("☑ done"), "{html}");
}

#[test]
fn has_rich_structure_gates_native_rich_path() {
    // Structured content → rich.
    assert!(has_rich_structure("| a | b |\n| - | - |\n| 1 | 2 |"));
    assert!(has_rich_structure("# Heading\n\nbody"));
    assert!(has_rich_structure("intro\n\n- one\n- two"));
    assert!(has_rich_structure("1. first\n2. second"));
    assert!(has_rich_structure("- [ ] task"));
    assert!(has_rich_structure("```\ncode\n```"));
    assert!(has_rich_structure("$$\nx^2\n$$"));
    // <details> collapse blocks — standalone, attributed, and the inline
    // `<details><summary>` opener on ONE line (the #15 receipt cards
    // emitted that shape before the parser-safe block form; the gate
    // keeps accepting it for any producer that still sends it).
    // Before the #15 fix the gate only matched a standalone `<details>`
    // line, so the fence-less notify card fell to the HTML ladder and
    // rendered as literal tag soup on screen (owner screenshot 2026-08-28).
    assert!(has_rich_structure("<details>\nbody\n</details>"));
    assert!(has_rich_structure("<details open>\nbody\n</details>"));
    assert!(has_rich_structure(
        "<details><summary>peek</summary>\n\nbody\n\n</details>"
    ));
    // Plain prose — even with inline emphasis — stays on the existing path.
    assert!(!has_rich_structure("Just a normal reply."));
    assert!(!has_rich_structure(
        "Use the **bold** operator and `code` here."
    ));
    assert!(!has_rich_structure("Compute 5 * 3 = 15 and move on."));
    assert!(!has_rich_structure("A #hashtag is not a heading."));
}

#[tokio::test]
async fn receipt_cards_route_the_html_wire_issue_85() {
    // #85 regression (re-lands #38, supersedes the #15 rich-structure
    // gate): the `<details>` chrome must ride the HTML input mode
    // (send_rich_html_id), where it parses into a native RichBlockDetails —
    // the markdown rich mode cannot express it, and the old #1234 outbox
    // route leaked escaped tags when its internal fallback re-escaped the
    // wrapper (2026-08-29). Both card builders must emit the Html wire; the
    // empty-body guards must drop the wrapper entirely onto the flat
    // markdown wire (the RICH_MESSAGE_EMPTY class, 3 events that day).
    let (notify_wire, _) =
        crate::channels::telegram::resume::build_notify_receipt_card("Compiler", "body text").await;
    let BubbleWire::Html(_) = notify_wire else {
        panic!("notify card must ride the HTML rich wire (#85)");
    };

    let meta = crate::brain::agent::BgTaskMeta {
        task_id: "t-1".to_string(),
        success: true,
        label: "cmd".to_string(),
        elapsed_secs: 1.0,
        tail: "ok".to_string(),
    };
    let (bg_wire, _) = crate::channels::telegram::resume::build_bg_receipt_card(&meta);
    let BubbleWire::Html(_) = bg_wire else {
        panic!("bg card must ride the HTML rich wire (#85)");
    };

    let empty = crate::brain::agent::BgTaskMeta {
        task_id: "t-2".to_string(),
        success: true,
        label: "cmd".to_string(),
        elapsed_secs: 1.0,
        tail: "  \n".to_string(),
    };
    let (flat_wire, _) = crate::channels::telegram::resume::build_bg_receipt_card(&empty);
    let BubbleWire::Markdown(flat_md) = flat_wire else {
        panic!("empty-tail card must ride the flat markdown wire (#38 guard)");
    };
    assert!(
        !flat_md.contains("<details>"),
        "guard drops the wrapper: {flat_md}"
    );
}

#[test]
fn table_plus_fence_stays_rich() {
    // #476 disqualify reverted: a message with a table MUST render rich
    // (HTML unwraps tables to raw pipes). A fence no longer pulls the whole
    // message onto the HTML path; fence rendering under rich is a separate
    // cosmetic issue fixed properly by the native-block serializer (#420 B),
    // not by exclusion.
    assert!(has_rich_structure(
        "# Report\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n```\nlog\n```"
    ));
}

// ── sendRichMessage request body ────────────────────────────────────

#[test]
fn rich_request_body_passes_markdown_through() {
    // InputRichMessage takes the message as a `markdown` string; Telegram
    // parses it server-side, so we pass the model's markdown verbatim.
    let body = crate::channels::telegram::rich::api::build_body(
        12345,
        None,
        "# Title\n\n| a | b |\n| - | - |\n| 1 | 2 |",
        None,
    );
    assert_eq!(body["chat_id"], 12345);
    assert_eq!(
        body["rich_message"]["markdown"],
        "# Title\n\n| a | b |\n| - | - |\n| 1 | 2 |"
    );
    // No thread id supplied → field omitted.
    assert!(body.get("message_thread_id").is_none());
}

// ── <details>/<summary> parsing ──────────────────────────────────────────

#[test]
fn details_with_summary_parses() {
    let md = "Before\n\n<details>\n<summary>Click to expand</summary>\n\nHidden content\n\n</details>\n\nAfter";
    let blocks = parse_markdown(md);
    assert_eq!(blocks.len(), 3);
    assert_eq!(
        blocks[0],
        Block::Paragraph(vec![Inline::Text("Before".into())])
    );
    match &blocks[1] {
        Block::Details {
            summary,
            blocks,
            open,
        } => {
            assert!(!open);
            assert_eq!(summary.len(), 1);
            assert_eq!(summary[0], Inline::Text("Click to expand".into()));
            assert_eq!(blocks.len(), 1);
            assert_eq!(
                blocks[0],
                Block::Paragraph(vec![Inline::Text("Hidden content".into())])
            );
        }
        other => panic!("expected Details, got {:?}", other),
    }
    assert_eq!(
        blocks[2],
        Block::Paragraph(vec![Inline::Text("After".into())])
    );
}

#[test]
fn details_open_attribute() {
    let md = "<details open>\n<summary>Open by default</summary>\n\nBody\n\n</details>";
    let blocks = parse_markdown(md);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
        Block::Details { open, .. } => assert!(open),
        other => panic!("expected Details, got {:?}", other),
    }
}

#[test]
fn details_without_summary() {
    let md = "<details>\n\nJust content\n\n</details>";
    let blocks = parse_markdown(md);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
        Block::Details {
            summary, blocks, ..
        } => {
            assert!(summary.is_empty());
            assert_eq!(blocks.len(), 1);
        }
        other => panic!("expected Details, got {:?}", other),
    }
}

#[test]
fn nested_details() {
    let md = "<details>\n<summary>Outer</summary>\n\n<details>\n<summary>Inner</summary>\n\nDeep\n\n</details>\n\n</details>";
    let blocks = parse_markdown(md);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
        Block::Details {
            summary, blocks, ..
        } => {
            assert_eq!(summary[0], Inline::Text("Outer".into()));
            assert_eq!(blocks.len(), 1);
            match &blocks[0] {
                Block::Details {
                    summary: inner_sum,
                    blocks: inner_body,
                    ..
                } => {
                    assert_eq!(inner_sum[0], Inline::Text("Inner".into()));
                    assert_eq!(
                        inner_body[0],
                        Block::Paragraph(vec![Inline::Text("Deep".into())])
                    );
                }
                other => panic!("expected nested Details, got {:?}", other),
            }
        }
        other => panic!("expected Details, got {:?}", other),
    }
}

#[test]
fn details_renders_html_fallback() {
    use crate::channels::telegram::rich::markdown_to_html;
    let md = "<details>\n<summary>Status</summary>\n\nAll good\n\n</details>";
    let html = markdown_to_html(md);
    assert!(html.contains("<b>▸ Status</b>"), "summary header: {html}");
    assert!(html.contains("  All good"), "indented body: {html}");
}

#[test]
fn details_rich_structure_detected() {
    let md = "<details>\n<summary>Log</summary>\n\nContent\n\n</details>";
    assert!(crate::channels::telegram::rich::detect::has_rich_structure(
        md
    ));
}

// ── code fence length matching (#94) ────────────────────────────────

#[test]
fn code_fence_four_backticks_contains_three_backticks() {
    let md = "````rust\n```\nnested\n```\n````";
    let blocks = parse_markdown(md);
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0],
        Block::Code {
            lang: Some("rust".to_string()),
            text: "```\nnested\n```".to_string(),
        }
    );
}

#[test]
fn code_fence_closer_can_be_longer_than_opener() {
    let md = "```\nline 1\n`````";
    let blocks = parse_markdown(md);
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0],
        Block::Code {
            lang: None,
            text: "line 1".to_string(),
        }
    );
}

#[test]
fn code_fence_closer_shorter_than_opener_does_not_close() {
    let md = "````\n```\nstill code\n````";
    let blocks = parse_markdown(md);
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0],
        Block::Code {
            lang: None,
            text: "```\nstill code".to_string(),
        }
    );
}

#[test]
fn code_fence_tilde_matching() {
    let md = "~~~python\nprint('hello')\n~~~";
    let blocks = parse_markdown(md);
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0],
        Block::Code {
            lang: Some("python".to_string()),
            text: "print('hello')".to_string(),
        }
    );
}

#[test]
fn code_fence_tilde_does_not_close_backticks() {
    let md = "````\n~~~\ncode\n````";
    let blocks = parse_markdown(md);
    assert_eq!(blocks.len(), 1);
    assert_eq!(
        blocks[0],
        Block::Code {
            lang: None,
            text: "~~~\ncode".to_string(),
        }
    );
}
