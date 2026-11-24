//! A session's messages: recent pages, reactions, truncation.
//!
//! Operations land here as their GUI doors move behind the contract.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::models::Message;
use crate::db::repository::message::MessageRepository;
use crate::db::repository::message_reaction::MessageReactionRepository;
use crate::db::repository::session::SessionRepository;
use crate::ui_core::rail;

use super::{BackendError, Ctx, Op, Scope};

/// How many of the newest rows one transcript read brings back, matching the
/// TUI display window: a pane built to show this many rows, rebuilt on every
/// keystroke. Older history stays behind the window's `Marker` block.
pub const RECENT_WINDOW: usize = 200;

/// Reactions keyed by message id, each a list of `(emoji, count)` in the order
/// the pictures were first used. Rows nobody reacted to are absent.
pub type Reactions = HashMap<Uuid, Vec<(String, usize)>>;

/// Every reaction on the given rows, in a single query (#2825).
///
/// A failure is answered, never swallowed: the tolerance (an empty map and a
/// log line, so the transcript still draws without chips) belongs to the
/// window, which is the one surface that can race the migrations on its first
/// paint. A remote caller gets the same `Failed` text and makes the same call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReactionsFor {
    /// The rows a transcript read just loaded.
    pub message_ids: Vec<Uuid>,
}

impl Op for ReactionsFor {
    const NAME: &'static str = "transcript.reactions_for";
    const SCOPE: Scope = Scope::Read;
    type Reply = Reactions;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        MessageReactionRepository::new(db.pool().clone())
            .for_messages(&self.message_ids)
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

/// Toggle one reaction on one stored row (#2825). The reply is whether the
/// reaction is there afterwards, so the caller can say which way the click
/// went without a second read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToggleReaction {
    /// The stored row reacted to.
    pub message_id: Uuid,
    /// The picture, stored as given.
    pub emoji: String,
}

impl Op for ToggleReaction {
    const NAME: &'static str = "transcript.toggle_reaction";
    const SCOPE: Scope = Scope::Manage;
    type Reply = bool;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        MessageReactionRepository::new(db.pool().clone())
            .toggle(self.message_id, &self.emoji)
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

/// Drop every row after a point in one session (#2826).
///
/// The row at `sequence` survives, because the edit replaces it and the
/// resend speaks for everything that followed. The reply is how many rows
/// went.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruncateAfter {
    /// The session cut.
    pub session_id: Uuid,
    /// The last sequence kept.
    pub sequence: i32,
}

impl Op for TruncateAfter {
    const NAME: &'static str = "transcript.truncate_after";
    const SCOPE: Scope = Scope::Manage;
    type Reply = usize;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        MessageRepository::new(db.pool().clone())
            .delete_after(self.session_id, self.sequence)
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

/// One transcript read: the rows, the total the family holds, and which topic
/// each row came from when the read merged several sessions (#2762).
///
/// Reactions are not part of it: they come from [`ReactionsFor`], which the
/// window asks for and tolerates on its own.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Recent {
    pub rows: Vec<Message>,
    pub total: i64,
    /// Row id to topic label. Empty for a single session, which is every read
    /// that is not a forum's `All` view.
    pub origins: HashMap<Uuid, String>,
}

/// The newest [`RECENT_WINDOW`] rows of a conversation plus the total row
/// count.
///
/// A session that has topics comes back as its whole family: the parent plus
/// every topic under it, merged by time, with each row's topic recorded in
/// [`Recent::origins`] (#2762). That is what `All` shows. A topic opened on
/// its own has no children, so it reads alone and carries no labels at all.
///
/// Attachments stay where they are, as markers inside each row's `content`:
/// the reply carries the text the window has always parsed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadRecent {
    /// The session opened, parent or topic.
    pub session_id: Uuid,
}

impl Op for LoadRecent {
    const NAME: &'static str = "transcript.load_recent";
    const SCOPE: Scope = Scope::Read;
    type Reply = Recent;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        load_recent(ctx, self.session_id)
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

/// The body of [`LoadRecent`], with the ledger's own error so the reply keeps
/// the `{e:#}` text the window showed.
async fn load_recent(ctx: &Ctx, session_id: Uuid) -> anyhow::Result<Recent> {
    let db = ctx.database().await?;
    let repo = MessageRepository::new(db.pool().clone());
    let sessions = SessionRepository::new(db.pool().clone());
    let family = sessions.family_of(session_id).await?;
    // One row in the family is the ordinary read this function has always
    // made.
    if family.len() <= 1 {
        let rows = repo
            .find_recent_by_session(session_id, RECENT_WINDOW)
            .await?;
        let total = repo.count_by_session(session_id).await?;
        return Ok(Recent {
            rows,
            total,
            origins: HashMap::new(),
        });
    }
    let ids: Vec<Uuid> = family.iter().map(|(id, _)| *id).collect();
    let mut total: i64 = 0;
    for id in &ids {
        total += repo.count_by_session(*id).await?;
    }
    // One conversation, one window (#2805). A window per member drew 3,627
    // rows for a 44-member family into a pane built to show 200, and every one
    // of them was rebuilt on every keystroke.
    let mut rows = repo.find_recent_by_sessions(&ids, RECENT_WINDOW).await?;
    let labels: HashMap<Uuid, String> = family
        .iter()
        .filter_map(|(id, title)| topic_family_label(title).map(|label| (*id, label)))
        .collect();
    let mut origins: HashMap<Uuid, String> = HashMap::new();
    for row in &rows {
        if let Some(label) = labels.get(&row.session_id) {
            origins.insert(row.id, label.clone());
        }
    }
    // Same order the TUI's reader gets from SQL (#3018).
    rows.sort_by_key(family_row_order);
    Ok(Recent {
        rows,
        total,
        origins,
    })
}

/// The order a topic family's rows are drawn in (#3018).
///
/// `created_at` places rows from different sessions in one stream. Within a
/// session it cannot order a turn: the assistant row is created at turn start
/// and updated in place, so it carries the same second as the question it
/// answers. `sequence` is the per-session order the single-session read
/// already uses, so it decides the tie; `id` stays as the last resort for two
/// different sessions stamped the same second.
pub fn family_row_order(m: &Message) -> (chrono::DateTime<chrono::Utc>, i32, Uuid) {
    (m.created_at, m.sequence, m.id)
}

/// What a stored session title reads as once the bookkeeping is off it. `All`
/// labels every row with this, and the label names the conversation the row
/// came from, never the transport (#2766): a topic reads `Memes`, and a row
/// from the base chat reads `OpenCrabs Dev`.
pub fn topic_family_label(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(topic) = rail::topic_name(Some(trimmed)) {
        return Some(topic);
    }
    Some(rail::chat_name(Some(trimmed)))
}

ops! { ReactionsFor, ToggleReaction, TruncateAfter, LoadRecent }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::parity::{Gateway, forbidden, parity};
    use crate::db::models::Session;
    use serde::de::DeserializeOwned;
    use serde_json::Value;

    const SESSION: Uuid = Uuid::from_u128(0x5e55_1000);
    const TOPIC: Uuid = Uuid::from_u128(0x5e55_2000);

    /// A row with every id and stamp pinned, so two crabs seeded with it
    /// hold the same ledger and answer the same JSON.
    fn row(session: Uuid, n: u128, role: &str, content: &str, sequence: i32) -> Message {
        let mut m = Message::new(session, role.into(), content.into(), sequence);
        m.id = Uuid::from_u128(0xa000 + n);
        m.created_at =
            chrono::DateTime::from_timestamp(1_795_000_000 + n as i64, 0).expect("a valid second");
        m
    }

    fn session(id: Uuid, title: &str) -> Session {
        let mut s = Session::new(Some(title.into()), None, None);
        s.id = id;
        s
    }

    /// One session with three rows, the same on every crab it is called on.
    async fn seed(gateway: &Gateway) -> (SessionRepository, MessageRepository) {
        let db = gateway.ctx.db().await.expect("db");
        let sessions = SessionRepository::new(db.pool().clone());
        let messages = MessageRepository::new(db.pool().clone());
        sessions
            .create(&session(SESSION, "chat"))
            .await
            .expect("create session");
        for (n, role, text) in [
            (1, "user", "hello"),
            (2, "assistant", "hi"),
            (3, "user", "bye"),
        ] {
            messages
                .create(&row(SESSION, n, role, text, n as i32))
                .await
                .expect("create message");
        }
        (sessions, messages)
    }

    /// A write is not idempotent, so its two doors run on two crabs seeded
    /// alike: the local door on `a`, the remote door on `b`.
    async fn two_crabs<O: Op + Clone>(a: &Gateway, b: &Gateway, op: O) -> Value {
        let local = serde_json::to_value(a.local(op.clone()).await).expect("local encode");
        let remote = serde_json::to_value(b.remote(op, Scope::ALL).await).expect("remote encode");
        assert_eq!(local, remote, "{}: the doors disagree", O::NAME);
        local
    }

    /// A fixture names its op, its scope, a request that decodes as the op,
    /// and a response that decodes as its answer.
    fn fixture<O: Op>(text: &str) -> O {
        let fixture: Value = serde_json::from_str(text).expect("fixture is JSON");
        assert_eq!(fixture["op"], O::NAME);
        assert_eq!(
            fixture["scope"],
            serde_json::to_value(O::SCOPE).expect("scope")
        );
        let _: Result<O::Reply, BackendError> =
            serde_json::from_value(fixture["response"].clone()).expect("response decodes");
        decode::<O>(&fixture["request"])
    }

    fn decode<T: DeserializeOwned>(value: &Value) -> T {
        serde_json::from_value(value.clone()).expect("request decodes")
    }

    #[test]
    fn every_fixture_decodes_as_its_op() {
        fixture::<ReactionsFor>(include_str!(
            "../../tests/fixtures/gui_backend/ReactionsFor.json"
        ));
        fixture::<ToggleReaction>(include_str!(
            "../../tests/fixtures/gui_backend/ToggleReaction.json"
        ));
        fixture::<TruncateAfter>(include_str!(
            "../../tests/fixtures/gui_backend/TruncateAfter.json"
        ));
        fixture::<LoadRecent>(include_str!(
            "../../tests/fixtures/gui_backend/LoadRecent.json"
        ));
    }

    #[tokio::test]
    async fn reactions_for_has_parity() {
        let gateway = Gateway::start().await;
        seed(&gateway).await;
        let first = Uuid::from_u128(0xa001);
        ToggleReaction {
            message_id: first,
            emoji: "👍".into(),
        }
        .run(&gateway.ctx)
        .await
        .expect("react");

        let answer = parity!(
            gateway,
            ReactionsFor {
                message_ids: vec![first, Uuid::from_u128(0xa002)],
            }
        );
        let map = answer["Ok"].as_object().expect("a map of reactions");
        assert_eq!(map.len(), 1, "a row nobody reacted to is absent: {answer}");
        assert_eq!(map[&first.to_string()], serde_json::json!([["👍", 1]]));
        forbidden!(
            gateway,
            ReactionsFor {
                message_ids: vec![first],
            }
        );
    }

    #[tokio::test]
    async fn reactions_for_reports_a_missing_table() {
        // The window's first paint can run one migration ahead of the ledger:
        // the op says so, and the window decides to draw without chips.
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = Ctx::new(dir.path().join("bare.db"));
        let err = ReactionsFor {
            message_ids: vec![Uuid::from_u128(1)],
        }
        .run(&ctx)
        .await
        .expect_err("no table, no reactions");
        assert!(matches!(err, BackendError::Failed(_)), "{err:?}");
    }

    #[tokio::test]
    async fn toggle_reaction_has_parity() {
        let (a, b) = (Gateway::start().await, Gateway::start().await);
        seed(&a).await;
        seed(&b).await;
        let op = ToggleReaction {
            message_id: Uuid::from_u128(0xa002),
            emoji: "🦀".into(),
        };
        assert_eq!(two_crabs(&a, &b, op.clone()).await["Ok"], true);
        assert_eq!(
            two_crabs(&a, &b, op).await["Ok"],
            false,
            "a second click takes it away"
        );

        // On one crab: a row the ledger does not hold is refused the same way
        // by both doors, and a write cannot flip the answer in between.
        parity!(
            a,
            ToggleReaction {
                message_id: Uuid::from_u128(0xdead),
                emoji: "🦀".into(),
            }
        );
        forbidden!(
            a,
            ToggleReaction {
                message_id: Uuid::from_u128(0xa002),
                emoji: "🦀".into(),
            }
        );
        let left = ReactionsFor {
            message_ids: vec![Uuid::from_u128(0xa002)],
        }
        .run(&a.ctx)
        .await
        .expect("read back");
        assert!(left.is_empty(), "a refused toggle wrote nothing: {left:?}");
    }

    #[tokio::test]
    async fn truncate_after_has_parity() {
        let (a, b) = (Gateway::start().await, Gateway::start().await);
        seed(&a).await;
        let (_, messages) = seed(&b).await;
        let op = TruncateAfter {
            session_id: SESSION,
            sequence: 1,
        };
        assert_eq!(two_crabs(&a, &b, op.clone()).await["Ok"], 2);
        let kept = messages.find_by_session(SESSION).await.expect("read back");
        assert_eq!(kept.len(), 1, "the row at the sequence survives");
        assert_eq!(kept[0].sequence, 1);

        // Repeated on one crab there is nothing left past the point.
        let again = parity!(
            a,
            TruncateAfter {
                session_id: SESSION,
                sequence: 1,
            }
        );
        assert_eq!(again["Ok"], 0);
        messages
            .create(&row(SESSION, 9, "user", "again", 9))
            .await
            .expect("one more row");
        forbidden!(b, op);
        assert_eq!(
            messages.count_by_session(SESSION).await.expect("count"),
            2,
            "a refused truncate deleted nothing"
        );
    }

    #[tokio::test]
    async fn load_recent_has_parity_for_one_session() {
        let gateway = Gateway::start().await;
        seed(&gateway).await;
        let answer = parity!(
            gateway,
            LoadRecent {
                session_id: SESSION
            }
        );
        let recent: Recent = decode(&answer["Ok"]);
        assert_eq!(recent.total, 3);
        assert_eq!(
            recent.rows.iter().map(|m| m.sequence).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(recent.origins.is_empty(), "one session carries no labels");
        forbidden!(
            gateway,
            LoadRecent {
                session_id: SESSION
            }
        );
    }

    #[tokio::test]
    async fn load_recent_has_parity_for_a_family() {
        let gateway = Gateway::start().await;
        let (sessions, messages) = seed(&gateway).await;
        sessions
            .create(&session(TOPIC, "Telegram: Dev / Memes [chat:-100:topic:5]"))
            .await
            .expect("create topic");
        sessions
            .attach_topic(TOPIC, SESSION)
            .await
            .expect("attach topic");
        messages
            .create(&row(TOPIC, 4, "user", "meme", 1))
            .await
            .expect("topic row");

        let answer = parity!(
            gateway,
            LoadRecent {
                session_id: SESSION
            }
        );
        let recent: Recent = decode(&answer["Ok"]);
        assert_eq!(recent.total, 4, "the count covers the whole family");
        assert_eq!(recent.rows.len(), 4);
        assert_eq!(
            recent
                .origins
                .get(&Uuid::from_u128(0xa004))
                .map(String::as_str),
            Some("Memes")
        );
        assert_eq!(
            recent
                .origins
                .get(&Uuid::from_u128(0xa001))
                .map(String::as_str),
            Some("chat")
        );
    }

    #[test]
    fn a_family_turn_keeps_its_question_above_its_answer() {
        let stamp = chrono::DateTime::from_timestamp(1_795_000_000, 0).expect("a valid second");
        let mut question = row(SESSION, 0, "user", "q", 2266);
        question.created_at = stamp;
        question.id = Uuid::from_u128(0xffff_0000);
        let mut answer = row(SESSION, 0, "assistant", "a", 2267);
        answer.created_at = stamp;
        answer.id = Uuid::from_u128(0x0001);
        let mut rows = [answer, question];
        rows.sort_by_key(family_row_order);
        assert_eq!(rows[0].role, "user");
    }
}
