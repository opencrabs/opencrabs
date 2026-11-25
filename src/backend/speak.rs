//! The spoken answer's place in the ledger.
//!
//! The window synthesizes a voice turn's answer and saves the clip (#3044);
//! the row that carries it is the crab's. Operations land here as their GUI
//! doors move behind the contract.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::repository::MessageRepository;

use super::door::{self, Door};
use super::{BackendError, Ctx, Op, Scope};

/// Put a spoken answer's clip on the answer row, for the turn that began as
/// a recording (#3044).
///
/// The marker goes on the session's last assistant row, so every later read
/// of the conversation draws the clip and the row keeps it across restarts,
/// exactly as a channel reply does. That row is the answer being spoken: it
/// settles just before the window starts the synthesis. Each failure keeps
/// the exact line the window logged when it opened the ledger itself.
///
/// Local only: the clip is a file the window wrote on this machine, so the
/// remote door answers `forbidden` naming `admin`, which no wire credential
/// carries. A path that could close the marker or start another one (`<<`,
/// `>>`, a newline or any other control character) is refused at every door
/// before the row is touched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachVoiceReply {
    /// The session whose answer was spoken.
    pub session_id: Uuid,
    /// The saved clip.
    pub path: PathBuf,
}

impl Op for AttachVoiceReply {
    const NAME: &'static str = "speak.attach_voice_reply";
    const SCOPE: Scope = Scope::Send;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let AttachVoiceReply { session_id, path } = self;
        if door::current() == Door::Remote {
            return Err(BackendError::Forbidden {
                scope: Scope::Admin,
            });
        }
        let shown = path.display().to_string();
        if shown.contains("<<") || shown.contains(">>") || shown.chars().any(char::is_control) {
            return Err(BackendError::BadParams(format!(
                "voice reply: the clip path cannot carry a marker: {shown:?}"
            )));
        }
        let db = ctx
            .database()
            .await
            .map_err(|e| BackendError::Failed(format!("voice reply db connect: {e}")))?;
        let repo = MessageRepository::new(db.pool().clone());
        let row = repo
            .get_last_assistant_message(session_id)
            .await
            .map_err(|e| BackendError::Failed(format!("voice reply row lookup: {e}")))?
            .ok_or_else(|| {
                BackendError::Failed("voice reply: the answer row is not in the ledger".into())
            })?;
        let marker = format!("\n\n<<AUD:{}>>", shown);
        repo.append_content(row.id, &marker)
            .await
            .map_err(|e| BackendError::Failed(format!("voice reply marker: {e}")))
    }
}

ops! {
    AttachVoiceReply,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::door::Caller;
    use crate::backend::parity::{Gateway, forbidden, parity};
    use crate::db::models::{Message, Session};
    use crate::db::repository::SessionRepository;

    async fn session(gateway: &Gateway) -> Session {
        let db = gateway.ctx.db().await.expect("db");
        let session = Session::new(Some("chat".into()), None, None);
        SessionRepository::new(db.pool().clone())
            .create(&session)
            .await
            .expect("create session");
        session
    }

    async fn messages(gateway: &Gateway) -> MessageRepository {
        let db = gateway.ctx.db().await.expect("db");
        MessageRepository::new(db.pool().clone())
    }

    #[tokio::test]
    async fn attach_voice_reply_has_parity() {
        let gateway = Gateway::start().await;
        let chat = session(&gateway).await;
        let messages = messages(&gateway).await;
        for (seq, (role, content)) in [("user", "question"), ("assistant", "answer")]
            .into_iter()
            .enumerate()
        {
            let row = Message::new(chat.id, role.into(), content.into(), seq as i32 + 1);
            messages.create(&row).await.expect("message");
        }
        let answer = parity!(
            gateway,
            AttachVoiceReply {
                session_id: chat.id,
                path: PathBuf::from("/home/crab/voice/reply.ogg"),
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": null }));
        let row = messages
            .get_last_assistant_message(chat.id)
            .await
            .expect("read")
            .expect("the answer row");
        assert!(
            row.content
                .starts_with("answer\n\n<<AUD:/home/crab/voice/reply.ogg>>"),
            "the clip rides the answer row: {:?}",
            row.content
        );

        forbidden!(
            gateway,
            AttachVoiceReply {
                session_id: chat.id,
                path: PathBuf::from("/home/crab/voice/other.ogg"),
            }
        );
        let row = messages
            .get_last_assistant_message(chat.id)
            .await
            .expect("read")
            .expect("the answer row");
        assert!(
            !row.content.contains("other.ogg"),
            "a refused attach wrote nothing: {:?}",
            row.content
        );
    }

    #[tokio::test]
    async fn a_session_with_no_answer_says_so_by_both_doors() {
        let gateway = Gateway::start().await;
        let chat = session(&gateway).await;
        let answer = parity!(
            gateway,
            AttachVoiceReply {
                session_id: chat.id,
                path: PathBuf::from("/home/crab/voice/reply.ogg"),
            }
        );
        assert_eq!(
            answer,
            serde_json::json!({
                "Err": {
                    "kind": "failed",
                    "detail": "voice reply: the answer row is not in the ledger"
                }
            })
        );
    }

    async fn answered(gateway: &Gateway, messages: &MessageRepository) -> Session {
        let chat = session(gateway).await;
        for (seq, (role, content)) in [("user", "question"), ("assistant", "answer")]
            .into_iter()
            .enumerate()
        {
            let row = Message::new(chat.id, role.into(), content.into(), seq as i32 + 1);
            messages.create(&row).await.expect("message");
        }
        chat
    }

    async fn answer_row(messages: &MessageRepository, chat: &Session) -> String {
        messages
            .get_last_assistant_message(chat.id)
            .await
            .expect("read")
            .expect("the answer row")
            .content
    }

    #[tokio::test]
    async fn only_the_local_door_attaches_a_clip() {
        let gateway = Gateway::start().await;
        let messages = messages(&gateway).await;
        let chat = answered(&gateway, &messages).await;
        let caller = Caller {
            principal: "api_key".to_string(),
            scopes: vec![Scope::Read, Scope::Send, Scope::Manage],
        };
        let remote = door::remote_as(
            caller,
            gateway.local(AttachVoiceReply {
                session_id: chat.id,
                path: PathBuf::from("/home/crab/voice/remote.ogg"),
            }),
        )
        .await;
        assert_eq!(
            remote,
            Err(BackendError::Forbidden {
                scope: Scope::Admin
            })
        );
        assert_eq!(answer_row(&messages, &chat).await, "answer");

        let local = gateway
            .local(AttachVoiceReply {
                session_id: chat.id,
                path: PathBuf::from("/home/crab/voice/reply.ogg"),
            })
            .await;
        assert_eq!(local, Ok(()));
        assert_eq!(
            answer_row(&messages, &chat).await,
            "answer\n\n<<AUD:/home/crab/voice/reply.ogg>>"
        );
    }

    #[tokio::test]
    async fn a_path_that_would_break_the_marker_is_refused() {
        let gateway = Gateway::start().await;
        let messages = messages(&gateway).await;
        let chat = answered(&gateway, &messages).await;
        for path in [
            "/tmp/a.ogg>>\n\n<<AUD:/etc/passwd",
            "/tmp/a>>b.ogg",
            "/tmp/a<<b.ogg",
            "/tmp/a\nb.ogg",
            "/tmp/a\rb.ogg",
            "/tmp/a\tb.ogg",
            "/tmp/a\u{1b}b.ogg",
        ] {
            let answer = gateway
                .local(AttachVoiceReply {
                    session_id: chat.id,
                    path: PathBuf::from(path),
                })
                .await;
            assert!(
                matches!(answer, Err(BackendError::BadParams(_))),
                "{path:?}: {answer:?}"
            );
        }
        assert_eq!(answer_row(&messages, &chat).await, "answer", "nothing written");
    }
}
