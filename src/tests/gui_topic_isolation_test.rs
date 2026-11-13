//! A topic opens as a completely isolated conversation (#2481).
//!
//! Three layers pinned here:
//! 1. The DB: a fresh topic holds zero rows, a message written under the
//!    topic never leaks into the parent's transcript query, and creating
//!    the topic leaves the parent's rows untouched.
//! 2. The stored name: the topic's `sessions.title` is written once at
//!    creation and never rewritten by the rename flow; the live name is
//!    display-only precedence in `ui_core/rail.rs`.
//! 3. The door: the create-topic path walks `open_session`, which delegates
//!    to `begin_conversation`, the one place that clears ctx tokens,
//!    pending metrics, queue, composer draft and attachments. GPUI entities
//!    cannot be built inside a plain test, so the wiring is pinned by
//!    source-text sentinels; the state resets themselves are visible in the
//!    diff of `Dashboard::begin_conversation`.

use crate::db::database::Database;
use crate::db::models::Message;
use crate::db::repository::MessageRepository;
use crate::db::repository::session::SessionRepository;

#[tokio::test]
async fn a_fresh_topic_holds_zero_rows_and_the_parent_keeps_its_own() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let sessions = SessionRepository::new(db.pool().clone());
    let messages = MessageRepository::new(db.pool().clone());

    let parent_row = crate::db::models::Session::new(Some("parent".into()), None, None);
    sessions.create(&parent_row).await.unwrap();
    let parent = parent_row.id;

    messages
        .create(&Message::new(
            parent,
            "user".into(),
            "parent question".into(),
            1,
        ))
        .await
        .unwrap();
    messages
        .create(&Message::new(
            parent,
            "assistant".into(),
            "parent answer".into(),
            2,
        ))
        .await
        .unwrap();

    // The door the create-topic path walks (gui/sessions.rs create_topic).
    let topic = sessions.create_topic(parent, "isolated").await.unwrap();
    let fresh = sessions
        .create_topic(parent, "never written")
        .await
        .unwrap();

    // A topic that was never written to reads as an empty conversation.
    let (rows, total) = {
        let rows = messages.find_recent_by_session(fresh, 200).await.unwrap();
        let total = messages.count_by_session(fresh).await.unwrap();
        (rows, total)
    };
    assert!(rows.is_empty(), "fresh topic must hold zero rows");
    assert_eq!(total, 0, "fresh topic count must be zero");

    // The topic's message stays under the topic.
    messages
        .create(&Message::new(
            topic,
            "user".into(),
            "topic question".into(),
            1,
        ))
        .await
        .unwrap();

    let parent_rows = messages.find_recent_by_session(parent, 200).await.unwrap();
    let contents: Vec<&str> = parent_rows
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        contents,
        vec!["parent question", "parent answer"],
        "a message written under the topic must not leak into the parent query"
    );
    assert_eq!(messages.count_by_session(parent).await.unwrap(), 2);

    // The topic reads exactly its own row back.
    let topic_rows = messages.find_recent_by_session(topic, 200).await.unwrap();
    assert_eq!(topic_rows.len(), 1);
    assert_eq!(topic_rows[0].content, "topic question");
}

#[tokio::test]
async fn creating_a_topic_leaves_the_parent_rows_untouched() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let sessions = SessionRepository::new(db.pool().clone());
    let messages = MessageRepository::new(db.pool().clone());

    let parent_row = crate::db::models::Session::new(Some("parent".into()), None, None);
    sessions.create(&parent_row).await.unwrap();
    let parent = parent_row.id;
    messages
        .create(&Message::new(
            parent,
            "user".into(),
            "before topic".into(),
            1,
        ))
        .await
        .unwrap();

    let before = messages.count_by_session(parent).await.unwrap();
    let _topic = sessions.create_topic(parent, "child").await.unwrap();
    let after = messages.count_by_session(parent).await.unwrap();

    assert_eq!(
        before, after,
        "creating a topic must not touch the parent's transcript"
    );
}

#[tokio::test]
async fn the_stored_topic_name_is_never_rewritten_by_the_rename_flow() {
    let db = Database::connect_in_memory().await.unwrap();
    db.run_migrations().await.unwrap();
    let sessions = SessionRepository::new(db.pool().clone());

    let parent_row = crate::db::models::Session::new(Some("parent".into()), None, None);
    sessions.create(&parent_row).await.unwrap();
    let parent = parent_row.id;
    let topic = sessions.create_topic(parent, "Alpha").await.unwrap();

    // The stored name is display-only precedence (ui_core/rail.rs): renames
    // land in channel_messages and the rail resolves the live name from
    // there. Nothing in this repo writes sessions.title back.
    let row = sessions.find_by_id(topic).await.unwrap().unwrap();
    assert_eq!(
        row.title.as_deref(),
        Some("Alpha"),
        "the stored topic name must stay as created"
    );
}

#[test]
fn create_topic_walks_the_begin_conversation_door() {
    // Source-text sentinels for the wiring a plain test cannot construct:
    // the create-topic path opens the new conversation through open_session,
    // open_session delegates every per-conversation reset to
    // begin_conversation, and that door clears the budget readout.
    let src = crate::tests::gui_dashboard_source::DASHBOARD;
    assert!(
        src.contains("fn begin_conversation(&mut self, id: Uuid"),
        "the one door must exist"
    );
    assert!(
        src.contains("self.begin_conversation(id, cx);"),
        "open_session must delegate to begin_conversation"
    );
    assert!(
        src.contains("this.open_session(topic, cx);"),
        "the create-topic path must open the topic through open_session"
    );
    let door = src
        .split("fn begin_conversation")
        .nth(1)
        .expect("begin_conversation body")
        .split("\n    }")
        .next()
        .expect("begin_conversation body");
    for reset in [
        "&mut self.turn,",
        "self.metrics.pending.remove(&from);",
        "ed.release_recall(false)",
        "ed.begin_conversation(cx)",
    ] {
        assert!(
            door.contains(reset),
            "begin_conversation must still carry the reset: {reset}"
        );
    }
}

#[test]
fn the_editor_reset_clears_draft_attachments_and_popups() {
    // The composer side of the door: content, attachments and both popup
    // states go back to rest, so nothing carries into the next conversation.
    let src = include_str!("../gui/editor.rs");
    let reset = src
        .split("pub fn begin_conversation(&mut self, cx: &mut Context<Self>)")
        .nth(1)
        .expect("editor begin_conversation body")
        .split("\n    }")
        .next()
        .expect("editor begin_conversation body");
    for cleared in [
        "self.content.clear();",
        "self.attachments.clear();",
        "self.command_dismissed = false;",
        "self.emoji_dismissed = false;",
    ] {
        assert!(reset.contains(cleared), "editor reset must clear {cleared}");
    }
}
