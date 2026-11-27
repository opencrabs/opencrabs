//! Tests for `AgentService::augment_user_message` (time-marker injection).
//!
//! Extracted from an inline `#[cfg(test)]` block in
//! `src/brain/agent/service/context.rs`; project policy (CONTRIBUTING.md)
//! requires all tests under `src/tests/`.

use std::fs;

use tempfile::TempDir;
use uuid::Uuid;

use crate::brain::agent::service::builder::AgentService;

#[tokio::test]
async fn augment_user_message_injects_time_marker() {
    use crate::config::profile::with_home_override_async;
    let dir = TempDir::new().unwrap();
    let user_md = dir.path().join("USER.md");
    fs::write(&user_md, "Timezone: UTC+3 (MSK)\n").unwrap();

    let session_id = Uuid::new_v4();
    // The recall (#799) and plan-reminder rides append AFTER the message, and
    // recall_for reads the live ~/.opencrabs/MEMORY.md. A populated MEMORY.md
    // matches almost any English message on tokenized FTS, so the ends_with
    // checks below only hold against a clean home. Isolate per the #1399 rule.
    let augmented = with_home_override_async(
        dir.path().to_path_buf(),
        AgentService::augment_user_message(session_id, "hello world", Some(dir.path())),
    )
    .await;
    assert!(augmented.contains("[Current time:"));
    assert!(augmented.contains("UTC"));
    assert!(augmented.contains("MSK"));
    assert!(augmented.ends_with("hello world"));
}

#[tokio::test]
async fn augment_user_message_falls_back_to_utc() {
    use crate::config::profile::with_home_override_async;
    let dir = TempDir::new().unwrap();
    let session_id = Uuid::new_v4();
    let augmented = with_home_override_async(
        dir.path().to_path_buf(),
        AgentService::augment_user_message(session_id, "test message", None),
    )
    .await;
    assert!(augmented.contains("[Current time:"));
    assert!(augmented.contains("UTC"));
    assert!(!augmented.contains("(user:"));
    assert!(augmented.ends_with("test message"));
}

/// #3222: the spoken-block rule rides with a voice note and nothing else, so a
/// typed turn never pays for it or writes a block nobody plays.
#[tokio::test]
async fn spoken_instruction_rides_only_with_a_voice_note() {
    use crate::channels::voice::spoken::{SPOKEN_INSTRUCTION, voice_prefix};
    use crate::config::profile::with_home_override_async;
    let dir = TempDir::new().unwrap();
    let session_id = Uuid::new_v4();

    let voice = format!(
        "[Channel: Telegram (chat_id: 1)]\n{}",
        voice_prefix("turn it up")
    );
    let typed = "turn it up";
    let (voiced, plain) = with_home_override_async(dir.path().to_path_buf(), async {
        (
            AgentService::augment_user_message(session_id, &voice, Some(dir.path())).await,
            AgentService::augment_user_message(session_id, typed, Some(dir.path())).await,
        )
    })
    .await;
    assert!(voiced.contains(SPOKEN_INSTRUCTION));
    assert!(!plain.contains(SPOKEN_INSTRUCTION));
    assert!(!plain.contains("<<SPOKEN:"));
}
