//! Tests for the remote panel's conversion (`crate::remote_panel`).
//!
//! The panel has no painter of its own: it hands the window's transcript the
//! rows it fetched. So the only thing worth testing here is the conversion,
//! because every rule the two views share is applied to the rows *after* they
//! are converted. A row that arrives without its measured columns draws a card
//! with no fold block and no footer while sharing a renderer with the local
//! chat, and that drift is invisible until the two are side by side (Daniel,
//! 21 Nov 2026: "a UI nao parece ser a mesma").

use crate::remote_panel::state_for;
use crate::transcript;
use opencrabs::remote::instance_client::RemoteMessage;

const SID: &str = "fd3fdf11-58bb-4414-b499-6dd1dc2203a9";

fn message(role: &str, content: &str) -> RemoteMessage {
    RemoteMessage {
        id: String::from("m1"),
        role: role.to_string(),
        content: content.to_string(),
        created_at: 1_760_000_000,
        ..Default::default()
    }
}

fn drawn(state: &transcript::TranscriptState) -> Vec<(String, String)> {
    state
        .turns
        .iter()
        .flat_map(|t| t.blocks.iter())
        .filter_map(|b| match b {
            transcript::Block::Msg { role, content, .. } => Some((role.clone(), content.clone())),
            _ => None,
        })
        .collect()
}

/// The seed the harness sends itself is scaffolding. It reaches the state
/// (it is a stored row) and must not be painted: `row_blocks` withholds it,
/// which is why going through the transcript is not the same as painting
/// `content`.
#[test]
fn the_seed_the_harness_sent_itself_is_not_painted() {
    let rows = vec![
        message(
            "user",
            "[SYSTEM: Setup just finished. This session runs on the opencrabs provider.]",
        ),
        message("assistant", "Brain's online."),
    ];
    let state = state_for(SID, &rows);
    assert_eq!(state.rows.len(), 2);
    let painted = drawn(&state);
    assert_eq!(painted.len(), 1, "painted {painted:?}");
    assert_eq!(painted[0].0, "assistant");
}

#[test]
fn a_compaction_marker_is_not_painted() {
    let rows = vec![
        message("user", "[CONTEXT COMPACTION] summary of everything"),
        message("user", "olá"),
        message("assistant", "olá!"),
    ];
    let state = state_for(SID, &rows);
    let painted = drawn(&state);
    assert_eq!(painted.len(), 2, "painted {painted:?}");
    assert!(!painted.iter().any(|(_, c)| c.contains("COMPACTION")));
}

#[test]
fn reasoning_folds_instead_of_leaking_into_the_answer() {
    let rows = vec![
        message("user", "pergunta"),
        message(
            "assistant",
            "<!-- reasoning -->\nI should think.\n<!-- /reasoning -->\n\nThe answer.",
        ),
    ];
    let state = state_for(SID, &rows);
    let thinking = state
        .turns
        .iter()
        .flat_map(|t| t.blocks.iter())
        .filter(|b| matches!(b, transcript::Block::Thinking { .. }))
        .count();
    assert_eq!(thinking, 1, "the chain of thought was not folded");
    let answer = drawn(&state)
        .into_iter()
        .filter(|(role, _)| role == "assistant")
        .map(|(_, content)| content)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(answer.contains("The answer."), "got {answer:?}");
    assert!(
        !answer.contains("I should think"),
        "thinking leaked into the answer: {answer:?}"
    );
}

/// The whole road a remote row takes, pinned: wire fields -> `Message` ->
/// the card's footer. The host measures the prompt size and the turn's own
/// rate; nothing on this side can re-derive them, so a conversion that drops
/// them leaves the card permanently footerless.
#[test]
fn a_remote_row_arrives_with_what_the_card_draws() {
    let rows = vec![
        message("user", "pergunta"),
        RemoteMessage {
            id: String::from("a1"),
            role: String::from("assistant"),
            content: String::from("a resposta"),
            created_at: 1_760_000_001,
            thinking: Some(String::from("pensei nisto")),
            tool_calls: Some(3),
            duration_secs: Some(7),
            ctx_window: Some(200_000),
            tokens_per_sec: Some(42.5),
            input_tokens: Some(74_000),
            continuation: false,
        },
    ];
    let state = state_for(SID, &rows);
    let row = state.rows.last().expect("the answer row");
    assert_eq!(row.thinking.as_deref(), Some("pensei nisto"));
    assert_eq!(row.tool_calls, Some(3));
    let metrics = transcript::TurnMetrics::from_row(&state.rows).expect("a footer");
    assert_eq!(metrics.ctx_used, 74_000);
    assert_eq!(metrics.ctx_max, 200_000);
    assert_eq!(metrics.tps, Some(42.5));
    assert_eq!(metrics.tool_calls, 3);
}

/// Two rows carrying the same unusable id must still be two rows: a shared
/// row id makes one of them unclickable and one fold toggle both.
#[test]
fn a_non_uuid_remote_id_still_gets_a_distinct_row_id() {
    let rows = vec![
        RemoteMessage {
            id: String::from("not-a-uuid"),
            role: String::from("user"),
            content: String::from("um"),
            created_at: 1_760_000_000,
            ..Default::default()
        },
        RemoteMessage {
            id: String::from("not-a-uuid"),
            role: String::from("assistant"),
            content: String::from("dois"),
            created_at: 1_760_000_001,
            ..Default::default()
        },
    ];
    let state = state_for(SID, &rows);
    assert_ne!(state.rows[0].id, state.rows[1].id);
}

#[test]
fn an_empty_session_paints_nothing_and_does_not_panic() {
    let state = state_for(SID, &[]);
    assert!(state.turns.is_empty());
    assert!(drawn(&state).is_empty());
}
