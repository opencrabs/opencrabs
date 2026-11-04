//! LID to phone-number session reconciliation (#2178).
//!
//! WhatsApp keys a DM session on whichever of the pair it picked at the time:
//! a phone number (PN) or an opaque privacy id (LID). #1533 fixed that choice
//! for NEW messages but nothing reconciled the rows already on disk, so an
//! install that ran WhatsApp across the change keeps two live sessions for one
//! human. The LID row stops receiving writes and everything said before the
//! cutover becomes unreachable: the bot "forgets" a conversation that is still
//! in the database, with no error anywhere.
//!
//! These run against a migrated in-memory database, which is the only honest
//! way to prove a migration before it touches a live one: the real host has a
//! split pair (`[chat:wa-236927743742100]` with 6 messages against
//! `[chat:wa-351933536442]` with 101) and the mapping row that ties them.

use anyhow::Result;
use uuid::Uuid;

use crate::channels::whatsapp::lid_migration::reconcile_lid_sessions;
use crate::db::Database;
use crate::db::Pool;
use crate::db::models::{Message, Session};
use crate::db::repository::{MessageRepository, SessionRepository};

/// In-memory database with every migration applied.
async fn test_pool() -> Pool {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory DB should connect");
    db.run_migrations().await.expect("migrations should apply");
    db.pool().clone()
}

fn lid_key(lid: &str) -> String {
    format!("[chat:wa-{lid}]")
}

fn pn_key(pn: &str) -> String {
    format!("[chat:wa-{pn}]")
}

/// The label `handle_message` generates, so a test can seed a row that looks
/// exactly like a real one.
fn generated_title(id: &str) -> String {
    format!("WhatsApp: {id} {}", lid_key(id))
}

async fn seed_session(
    pool: &Pool,
    wa_id: &str,
    title: Option<String>,
    tokens: i64,
    cost: f64,
) -> Session {
    let repo = SessionRepository::new(pool.clone());
    let mut session = Session::new(title, None, None);
    session.token_count = tokens;
    session.total_cost = cost;
    let key = lid_key(wa_id);
    repo.insert_or_resolve_channel(&session, &key)
        .await
        .expect("seed the channel session")
}

async fn seed_message(pool: &Pool, session_id: Uuid, seq: i32, content: &str, at: i64) {
    let repo = MessageRepository::new(pool.clone());
    let mut msg = Message::new(session_id, "user".to_string(), content.to_string(), seq);
    msg.created_at = chrono::DateTime::from_timestamp(at, 0).expect("valid epoch");
    repo.create(&msg).await.expect("seed the message");
}

async fn messages_of(pool: &Pool, session_id: Uuid) -> Vec<Message> {
    MessageRepository::new(pool.clone())
        .find_by_session(session_id)
        .await
        .expect("read messages")
}

async fn session_by_key(pool: &Pool, key: &str) -> Option<Session> {
    SessionRepository::new(pool.clone())
        .find_by_channel_chat_key(key)
        .await
        .expect("look up session by key")
}

/// The pre-#1533 install with a LID key and no PN session yet: the row must
/// become the PN row, history intact. This is the second contact in the issue,
/// whose LID session held 14 messages while a fresh PN session started later.
#[tokio::test]
async fn a_lid_only_session_is_renamed_to_its_phone_number() -> Result<()> {
    let pool = test_pool().await;
    let lid = "83911178752119";
    let pn = "6285270679623";

    let lid_session = seed_session(&pool, lid, Some(generated_title(lid)), 4200, 0.25).await;
    seed_message(
        &pool,
        lid_session.id,
        0,
        "before the cutover",
        1_780_000_000,
    )
    .await;
    seed_message(&pool, lid_session.id, 1, "still before it", 1_780_000_100).await;

    let report = reconcile_lid_sessions(&pool, vec![(lid.into(), pn.into())]).await?;

    assert_eq!(
        report.renamed, 1,
        "the LID row is the PN row now: {report:?}"
    );
    assert_eq!(report.merged, 0, "nothing to merge, no PN session existed");
    assert_eq!(report.messages_moved, 0, "a rename moves no messages");

    let moved = session_by_key(&pool, &pn_key(pn))
        .await
        .expect("the session now answers on its phone-number key");
    assert_eq!(moved.id, lid_session.id, "same row, new key");
    assert_eq!(moved.token_count, 4200, "lifetime counters travel with it");
    assert!(
        session_by_key(&pool, &lid_key(lid)).await.is_none(),
        "the LID key must be gone, or the split is only half closed"
    );
    assert_eq!(
        messages_of(&pool, moved.id).await.len(),
        2,
        "every message before the cutover is still there"
    );
    assert_eq!(
        moved.title.as_deref(),
        Some(format!("WhatsApp: {pn} {}", pn_key(pn)).as_str()),
        "a generated label follows the key it now carries"
    );
    Ok(())
}

/// Both sessions live, which is the reported host: the transcript has to move
/// into the PN session and read in time order afterwards. Sequences are per
/// session, so the merge renumbers; without that the older half of the
/// conversation sorts after the newer one.
#[tokio::test]
async fn a_split_conversation_is_merged_in_time_order() -> Result<()> {
    let pool = test_pool().await;
    let lid = "236927743742100";
    let pn = "351933536442";

    let lid_session = seed_session(&pool, lid, Some(generated_title(lid)), 177_219, 1.5).await;
    // The LID half is the OLDER half: 100, 200.
    seed_message(&pool, lid_session.id, 0, "august message", 1_780_000_000).await;
    seed_message(&pool, lid_session.id, 1, "september message", 1_780_000_200).await;

    let pn_session = seed_session(&pool, pn, Some(generated_title(pn)), 2_532_737, 9.0).await;
    // The PN half is the NEWER half: 150, 300.
    seed_message(&pool, pn_session.id, 0, "july message", 1_780_000_150).await;
    seed_message(&pool, pn_session.id, 1, "october message", 1_780_000_300).await;

    let report = reconcile_lid_sessions(&pool, vec![(lid.into(), pn.into())]).await?;

    assert_eq!(report.merged, 1, "one LID session folded in: {report:?}");
    assert_eq!(
        report.renamed, 0,
        "the PN key was taken, so nothing renamed"
    );
    assert_eq!(report.messages_moved, 2, "both LID messages moved");

    let survivor = session_by_key(&pool, &pn_key(pn))
        .await
        .expect("the PN session survives");
    assert_eq!(survivor.id, pn_session.id, "the PN row is the survivor");
    assert_eq!(
        survivor.token_count,
        2_532_737 + 177_219,
        "counters add up instead of one half's spend vanishing"
    );
    assert!(
        (survivor.total_cost - 10.5).abs() < 1e-9,
        "costs add up too"
    );

    let all = messages_of(&pool, survivor.id).await;
    assert_eq!(all.len(), 4, "one conversation, not two halves");
    let contents: Vec<&str> = all.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        contents,
        vec![
            "august message",
            "july message",
            "september message",
            "october message"
        ],
        "interleaved by time, not LID block then PN block"
    );
    let seqs: Vec<i32> = all.iter().map(|m| m.sequence).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3], "sequences renumbered without gaps");

    assert!(
        session_by_key(&pool, &lid_key(lid)).await.is_none(),
        "the LID row is archived, so nothing writes to it again"
    );
    let archived = SessionRepository::new(pool.clone())
        .find_by_id(lid_session.id)
        .await?
        .expect("the LID row still exists");
    assert!(
        archived.archived_at.is_some(),
        "archived, never deleted: rows keyed to it stay readable"
    );
    Ok(())
}

/// A title a human set is theirs. Only the generated label is rewritten, so a
/// renamed chat does not silently revert (#121 keeps custom titles across
/// auto-rename; the same courtesy applies here).
#[tokio::test]
async fn a_custom_title_survives_the_rename() -> Result<()> {
    let pool = test_pool().await;
    let lid = "111111111111111";
    let pn = "351911111111";

    seed_session(&pool, lid, Some("Mum".to_string()), 10, 0.0).await;
    reconcile_lid_sessions(&pool, vec![(lid.into(), pn.into())]).await?;

    let moved = session_by_key(&pool, &pn_key(pn))
        .await
        .expect("key rewritten even when the title is custom");
    assert_eq!(
        moved.title.as_deref(),
        Some("Mum"),
        "the human's name for the chat is left alone"
    );
    Ok(())
}

/// Boot-time safety: the pass runs on every start, so a second run must be a
/// no-op. Without this, a rename loop or a duplicate merge would corrupt the
/// transcript on every restart.
#[tokio::test]
async fn a_second_pass_finds_nothing_to_do() -> Result<()> {
    let pool = test_pool().await;
    let lid = "222222222222222";
    let pn = "351922222222";

    seed_session(&pool, lid, Some(generated_title(lid)), 5, 0.0).await;
    let first = reconcile_lid_sessions(&pool, vec![(lid.into(), pn.into())]).await?;
    assert_eq!(first.renamed, 1);

    let second = reconcile_lid_sessions(&pool, vec![(lid.into(), pn.into())]).await?;
    assert!(
        second.is_noop(),
        "the LID key is gone, so there is nothing left to reconcile: {second:?}"
    );
    let live = SessionRepository::new(pool.clone()).list_active().await?;
    let on_pn = live
        .iter()
        .filter(|s| s.channel_chat_key.as_deref() == Some(pn_key(pn).as_str()))
        .count();
    assert_eq!(
        on_pn, 1,
        "still exactly one live session on the PN key, not a fresh split"
    );
    Ok(())
}

/// The mapping table carries ~120 pairs on a real host and most of them never
/// produced a session. Junk pairs must not create or move anything: an empty
/// side would key a session to `wa-` and collide with every other empty one.
#[tokio::test]
async fn unmapped_and_malformed_pairs_change_nothing() -> Result<()> {
    let pool = test_pool().await;

    let report = reconcile_lid_sessions(
        &pool,
        vec![
            ("999999999999999".to_string(), "351999999999".to_string()),
            (String::new(), "351900000000".to_string()),
            ("333333333333333".to_string(), String::new()),
            ("444444444444444".to_string(), "444444444444444".to_string()),
        ],
    )
    .await?;

    assert!(report.is_noop(), "nothing to reconcile: {report:?}");
    assert_eq!(
        SessionRepository::new(pool.clone()).count(false).await?,
        0,
        "no session was invented"
    );
    Ok(())
}
