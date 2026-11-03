//! Tests for `latest_thread_id_for_chat` — the proactive-path lookup that
//! resolves a Telegram chat's most recent thread_id from
//! `channel_messages` so `telegram_send` and startup-resume route into
//! the originating forum topic (issue #130 proactive path).
//!
//! The helper itself reads via `crate::db::global_pool()` which is set
//! only by file-backed `Database::connect`. Tests can't safely install
//! into that OnceLock (other tests share the process), so we cover:
//!   1. The `i32 -> ThreadId(MessageId)` parse path inline.
//!   2. The repo contract the helper depends on (round-trip, ordering).
//!   3. The graceful-None path when the global pool is absent.

use crate::channels::telegram::send::latest_thread_id_for_chat;
use crate::db::models::ChannelMessage;
use crate::db::{ChannelMessageRepository, Database};
use chrono::{Duration, Utc};
use teloxide::types::{MessageId, ThreadId};

#[test]
fn parses_numeric_thread_id_string_into_thread_id() {
    let tid_str = "42";
    let result: Option<ThreadId> = tid_str.parse::<i32>().ok().map(|n| ThreadId(MessageId(n)));
    assert_eq!(result, Some(ThreadId(MessageId(42))));
}

#[test]
fn non_numeric_thread_id_string_returns_none() {
    let tid_str = "not a number";
    let result: Option<ThreadId> = tid_str.parse::<i32>().ok().map(|n| ThreadId(MessageId(n)));
    assert_eq!(result, None);
}

#[test]
fn thread_id_overflowing_i32_returns_none() {
    // teloxide's ThreadId wraps MessageId(i32), so values outside i32
    // range can't be represented. The helper must return None rather
    // than panic on overflow.
    let tid_str = "9999999999999";
    let result: Option<ThreadId> = tid_str.parse::<i32>().ok().map(|n| ThreadId(MessageId(n)));
    assert_eq!(result, None);
}

#[tokio::test]
async fn channel_message_thread_id_round_trips_through_repo() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    let chat_id = "test-chat-thread-roundtrip";
    let msg = ChannelMessage::new(
        "telegram".into(),
        chat_id.into(),
        Some("Some Group".into()),
        "u1".into(),
        "alice".into(),
        "hello from topic 17".into(),
        "text".into(),
        Some("msg-1".into()),
    )
    .with_thread(Some("17".to_string()), Some("General".into()));

    repo.insert(&msg).await.expect("insert");
    let recent = repo
        .recent(Some("telegram"), chat_id, 1, None, None)
        .await
        .expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].thread_id.as_deref(), Some("17"));
}

/// #226: forum-topic isolation. `recent()` with a thread_id must return ONLY
/// that topic's messages — the handler was passing `None`, so every topic saw
/// every other topic's history. Two topics in the same chat must not bleed.
#[tokio::test]
async fn recent_scoped_to_thread_does_not_bleed_across_topics() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    let chat_id = "test-chat-topic-isolation";
    let mk = |body: &str, mid: &str, thread: &str| {
        ChannelMessage::new(
            "telegram".into(),
            chat_id.into(),
            None,
            "u1".into(),
            "alice".into(),
            body.into(),
            "text".into(),
            Some(mid.into()),
        )
        .with_thread(Some(thread.to_string()), None)
    };

    repo.insert(&mk("topic ten message", "m-10", "10"))
        .await
        .unwrap();
    repo.insert(&mk("topic twenty message", "m-20", "20"))
        .await
        .unwrap();

    // Scoped to topic 10 → only topic-10 content, never topic 20.
    let only_10 = repo
        .recent(Some("telegram"), chat_id, 30, Some("10"), None)
        .await
        .unwrap();
    assert_eq!(
        only_10.len(),
        1,
        "topic 10 must see exactly its own message"
    );
    assert_eq!(only_10[0].thread_id.as_deref(), Some("10"));
    assert!(only_10.iter().all(|m| !m.content.contains("twenty")));

    // Unscoped (None) returns both — kept for the non-forum group case where
    // there's a single shared conversation.
    let all = repo
        .recent(Some("telegram"), chat_id, 30, None, None)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
}

/// #1655: General-topic isolation. General rows persist with `thread_id`
/// NULL (General messages carry no `message_thread_id`), so scoping history
/// "to General" means `thread_id IS NULL` — which `recent()`'s `None`
/// filter cannot express: None means "no filter" and returns EVERY topic's
/// rows. That is exactly how General prompts bled cross-topic history while
/// real topics were already isolated (#226). `recent_general()` must return
/// only the NULL-thread rows of the given chat.
#[tokio::test]
async fn recent_general_returns_only_general_rows_not_other_topics() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    let chat_id = "test-chat-general-isolation";
    let mk = |chat: &str, body: &str, mid: &str, thread: Option<&str>| {
        ChannelMessage::new(
            "telegram".into(),
            chat.into(),
            None,
            "u1".into(),
            "alice".into(),
            body.into(),
            "text".into(),
            Some(mid.into()),
        )
        .with_thread(thread.map(str::to_string), None)
    };

    // Interleave General (NULL) and topic rows so ordering cannot fake a pass.
    repo.insert(&mk(chat_id, "general message one", "m-g1", None))
        .await
        .unwrap();
    repo.insert(&mk(chat_id, "topic ten message", "m-10", Some("10")))
        .await
        .unwrap();
    repo.insert(&mk(chat_id, "general message two", "m-g2", None))
        .await
        .unwrap();
    repo.insert(&mk(chat_id, "topic twenty message", "m-20", Some("20")))
        .await
        .unwrap();
    // A different chat's General row must not leak into this chat's fetch.
    repo.insert(&mk("other-chat", "other chat general", "m-x1", None))
        .await
        .unwrap();

    // General scoping → ONLY this chat's NULL-thread rows.
    let general = repo
        .recent_general("telegram", chat_id, 30, None)
        .await
        .unwrap();
    assert_eq!(general.len(), 2, "General must see exactly its own rows");
    assert!(general.iter().all(|m| m.thread_id.is_none()));
    assert!(
        general
            .iter()
            .all(|m| !m.content.contains("ten") && !m.content.contains("twenty"))
    );
    assert!(general.iter().all(|m| !m.content.contains("other chat")));

    // The pre-#1655 path for contrast: recent(None) returns all 4 rows of
    // the chat across topics — the bleed mode this test pins as fixed.
    let unscoped = repo
        .recent(Some("telegram"), chat_id, 30, None, None)
        .await
        .unwrap();
    assert_eq!(unscoped.len(), 4);

    // Real topics keep their exact pre-#1655 filter semantics.
    let only_10 = repo
        .recent(Some("telegram"), chat_id, 30, Some("10"), None)
        .await
        .unwrap();
    assert_eq!(only_10.len(), 1);
}

/// The session label reads the topic NAME from `latest_topic_name`, which must
/// return the most recent non-null name for a thread — so an in-topic reply
/// (which omits the name) doesn't drop the label back to the numeric id.
#[tokio::test]
async fn latest_topic_name_returns_most_recent_non_null() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    let chat_id = "test-chat-topic-name";
    let mut named = ChannelMessage::new(
        "telegram".into(),
        chat_id.into(),
        None,
        "u1".into(),
        "alice".into(),
        "first message".into(),
        "text".into(),
        Some("m1".into()),
    )
    .with_thread(Some("2".to_string()), Some("Devops".into()));
    named.created_at = Utc::now() - Duration::minutes(5);

    // A later in-topic reply with NO topic name must not erase the label.
    let nameless = ChannelMessage::new(
        "telegram".into(),
        chat_id.into(),
        None,
        "u1".into(),
        "alice".into(),
        "a reply".into(),
        "text".into(),
        Some("m2".into()),
    )
    .with_thread(Some("2".to_string()), None);

    repo.insert(&named).await.unwrap();
    repo.insert(&nameless).await.unwrap();

    let name = repo
        .latest_topic_name("telegram", chat_id, "2")
        .await
        .unwrap();
    assert_eq!(name.as_deref(), Some("Devops"));

    // Unknown thread → None (label falls back to the numeric id).
    let missing = repo
        .latest_topic_name("telegram", chat_id, "999")
        .await
        .unwrap();
    assert_eq!(missing, None);
}

#[tokio::test]
async fn recent_returns_newest_first_so_helper_picks_latest_thread() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    let chat_id = "test-chat-recent-order";
    let mut old = ChannelMessage::new(
        "telegram".into(),
        chat_id.into(),
        None,
        "u1".into(),
        "alice".into(),
        "older".into(),
        "text".into(),
        Some("msg-old".into()),
    )
    .with_thread(Some("100".to_string()), None);
    old.created_at = Utc::now() - Duration::hours(1);

    let new = ChannelMessage::new(
        "telegram".into(),
        chat_id.into(),
        None,
        "u1".into(),
        "alice".into(),
        "newer".into(),
        "text".into(),
        Some("msg-new".into()),
    )
    .with_thread(Some("200".to_string()), None);

    repo.insert(&old).await.expect("insert old");
    repo.insert(&new).await.expect("insert new");

    let recent = repo
        .recent(Some("telegram"), chat_id, 1, None, None)
        .await
        .expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].thread_id.as_deref(), Some("200"));
}

#[tokio::test]
async fn latest_thread_id_returns_none_for_missing_chat_or_uninit_pool() {
    // Helper hits global_pool(); when uninitialized in the test process
    // it returns None. When initialized, a chat with no stored messages
    // also returns None. Either way: never panic, never invent a thread.
    let result = latest_thread_id_for_chat(99_999_999_999).await;
    assert_eq!(result, None);
}

/// Minimal row builder for the #143 rename-precedence pins: 4 args =
/// (chat_id, platform_message_id, thread_id, topic_name). Rows with an
/// e* id prefix carry message_type "topic_edited" (what the rename
/// handler persists); all others are plain "text" messages re-teaching
/// a creation name via their reply target.
#[allow(dead_code)]
fn msg(
    chat_id: &str,
    platform_id: &str,
    thread: Option<&str>,
    topic: Option<&str>,
) -> ChannelMessage {
    let mut row = ChannelMessage::new(
        "telegram".into(),
        chat_id.into(),
        Some("Test Group".into()),
        "u1".into(),
        "tester".into(),
        format!("content {platform_id}"),
        "text".into(),
        Some(platform_id.into()),
    )
    .with_thread(thread.map(str::to_string), topic.map(str::to_string));
    if platform_id.starts_with('e') {
        row.message_type = "topic_edited".into();
    }
    row
}

#[tokio::test]
async fn latest_topic_name_rename_outranks_creation_name() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    // Creation name, then a rename row, then a post-rename regular
    // message re-teaching the CREATION name (reply-target behavior).
    // Newest-non-null alone would serve the re-taught old name —
    // the rename row must win outright (#143).
    repo.insert(&msg("rr", "m1", Some("5"), Some("alpha")))
        .await
        .unwrap();
    repo.insert(&msg("rr", "e1", Some("5"), Some("beta")))
        .await
        .unwrap();
    repo.insert(&msg("rr", "m2", Some("5"), Some("alpha")))
        .await
        .unwrap();

    let name = repo.latest_topic_name("telegram", "rr", "5").await.unwrap();
    assert_eq!(
        name.as_deref(),
        Some("beta"),
        "renamed name must beat re-taught creation name"
    );
}

#[tokio::test]
async fn latest_topic_name_falls_back_without_rename_rows() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    // No rename present: newest non-null name still serves (pre-#143
    // behavior preserved for threads never renamed).
    repo.insert(&msg("rr2", "m1", Some("7"), Some("first")))
        .await
        .unwrap();
    repo.insert(&msg("rr2", "m2", Some("7"), Some("first")))
        .await
        .unwrap();

    let name = repo
        .latest_topic_name("telegram", "rr2", "7")
        .await
        .unwrap();
    assert_eq!(name.as_deref(), Some("first"));
}

#[tokio::test]
async fn topic_names_batch_serves_every_thread_with_rename_precedence() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    // Thread 5 is renamed after creation; thread 7 never is. One batched
    // query must serve both with the same precedence the row-at-a-time
    // lookup keeps (#2490): rename > creation name > nothing.
    repo.insert(&msg("rr", "m1", Some("5"), Some("alpha")))
        .await
        .unwrap();
    repo.insert(&msg("rr", "e1", Some("5"), Some("beta")))
        .await
        .unwrap();
    repo.insert(&msg("rr", "m2", Some("5"), Some("alpha")))
        .await
        .unwrap();
    repo.insert(&msg("rr2", "m1", Some("7"), Some("first")))
        .await
        .unwrap();

    let names = repo
        .topic_names(
            "telegram",
            &[("rr".into(), "5".into()), ("rr2".into(), "7".into())],
        )
        .await
        .unwrap();
    assert_eq!(
        names
            .get(&("rr".to_string(), "5".to_string()))
            .map(String::as_str),
        Some("beta"),
        "rename must win per thread"
    );
    assert_eq!(
        names
            .get(&("rr2".to_string(), "7".to_string()))
            .map(String::as_str),
        Some("first"),
        "creation name stands when no rename exists"
    );
    assert_eq!(names.len(), 2, "exactly the requested threads come back");
}

#[tokio::test]
async fn topic_names_collapses_duplicate_pairs_and_drops_unknown_threads() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let repo = ChannelMessageRepository::new(db.pool().clone());

    repo.insert(&msg("rr", "m1", Some("5"), Some("alpha")))
        .await
        .unwrap();

    // Same pair twice must not break the VALUES list or double the work;
    // a thread with no name row must simply be absent, never an error.
    let names = repo
        .topic_names(
            "telegram",
            &[
                ("rr".into(), "5".into()),
                ("rr".into(), "5".into()),
                ("rr".into(), "404".into()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(names.len(), 1, "duplicates collapse, unknown threads drop");
    assert_eq!(
        names
            .get(&("rr".to_string(), "5".to_string()))
            .map(String::as_str),
        Some("alpha")
    );

    let empty = repo.topic_names("telegram", &[]).await.unwrap();
    assert!(
        empty.is_empty(),
        "an empty batch short-circuits to an empty map"
    );
}
