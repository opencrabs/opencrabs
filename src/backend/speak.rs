//! The spoken answer's place in the ledger.
//!
//! The window synthesizes a voice turn's answer and saves the clip (#3044);
//! the row that carries it is the crab's. Operations land here as their GUI
//! doors move behind the contract.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::repository::MessageRepository;

use super::{BackendError, Ctx, Op, Scope};

/// Put a spoken answer's clip on the answer row, for the turn that began as
/// a recording (#3044).
///
/// The marker goes on the session's last assistant row, so every later read
/// of the conversation draws the clip and the row keeps it across restarts,
/// exactly as a channel reply does. That row is the answer being spoken: it
/// settles just before the window starts the synthesis. Each failure keeps
/// the exact line the window logged when it opened the ledger itself.
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
        let marker = format!("\n\n<<AUD:{}>>", path.display());
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
}
