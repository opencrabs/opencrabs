//! Poll votes reach the session that sent the poll (#2288).
//!
//! `poll_answer` is its own update type: no chat, no message, option INDICES
//! only. Two things had to be true for a vote to arrive at all, and both were
//! false: the update type was missing from `allowed_updates`, and nothing
//! remembered which session a poll id belonged to. These tests pin the text the
//! session sees, because a vote rendered wrong is a vote the agent reads wrong.

use teloxide::types::PollAnswer;

use crate::channels::telegram::poll_answer::vote_summary;

fn answer(json: &str) -> PollAnswer {
    serde_json::from_str(json).expect("poll answer fixture must parse")
}

const VOTED: &str = r#"{
    "poll_id": "poll-42",
    "user": {"id": 7, "is_bot": false, "first_name": "Ana", "last_name": "Silva"},
    "option_ids": [1]
}"#;

const RETRACTED: &str = r#"{
    "poll_id": "poll-42",
    "user": {"id": 7, "is_bot": false, "first_name": "Ana"},
    "option_ids": []
}"#;

const UNKNOWN_INDEX: &str = r#"{
    "poll_id": "poll-42",
    "user": {"id": 7, "is_bot": false, "first_name": "Ana"},
    "option_ids": [9]
}"#;

const CHANNEL_VOTE: &str = r#"{
    "poll_id": "poll-42",
    "voter_chat": {"id": -1001, "type": "channel", "title": "Team Channel"},
    "option_ids": [0]
}"#;

fn options() -> Vec<String> {
    vec!["Ship it".to_string(), "Hold".to_string()]
}

#[test]
fn a_vote_names_the_option_and_the_voter() {
    let text = vote_summary(&answer(VOTED), &options(), "Release today?");
    assert!(text.contains("Ana Silva"), "voter missing: {text}");
    assert!(text.contains("Hold"), "option label missing: {text}");
    assert!(!text.contains("Ship it"), "wrong option reported: {text}");
}

#[test]
fn a_vote_carries_the_poll_id_and_question() {
    let text = vote_summary(&answer(VOTED), &options(), "Release today?");
    assert!(text.contains("poll-42"), "poll id missing: {text}");
    assert!(text.contains("Release today?"), "question missing: {text}");
}

#[test]
fn a_retraction_is_not_reported_as_a_vote() {
    let text = vote_summary(&answer(RETRACTED), &options(), "Release today?");
    assert!(text.contains("retracted"), "retraction lost: {text}");
    assert!(!text.contains("voted"), "retraction read as a vote: {text}");
}

#[test]
fn an_index_we_cannot_label_is_reported_not_swallowed() {
    // The vote happened; only the label is unknown. Reporting this as a
    // retraction would tell the agent the opposite of the truth.
    let text = vote_summary(&answer(UNKNOWN_INDEX), &options(), "Release today?");
    assert!(
        !text.contains("retracted"),
        "unknown label read as retraction: {text}"
    );
    assert!(text.contains('9'), "raw index missing: {text}");
    assert!(text.contains("labels unknown"), "loss not surfaced: {text}");
}

#[test]
fn a_channel_voter_reports_the_chat_title() {
    let text = vote_summary(&answer(CHANNEL_VOTE), &options(), "Release today?");
    assert!(text.contains("Team Channel"), "voter chat missing: {text}");
    assert!(text.contains("Ship it"), "option label missing: {text}");
}
