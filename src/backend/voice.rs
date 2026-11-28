//! Speech on the crab: a take heard into words, an answer spoken into a clip.
//!
//! The window records and plays. Hearing, speaking and keeping the clips are
//! the crab's: its config chooses the engines and its keys pay for them, its
//! ledger says which project a session belongs to, and its disk keeps the
//! audio. A window looking at another crab therefore never transcribes with
//! this machine's keys, and never leaves a path in that crab's ledger that
//! only exists here.
//!
//! The folder rule is the window's (#3114), moved here unchanged: a session
//! that belongs to a project keeps its clips in that project's attachments
//! folder, and only a session with no project falls back to
//! `<profile home>/voice/`. The names are the window's too: a take is
//! `voice-<millis>.ogg`, a spoken answer `reply-<millis>.ogg` (#3044).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Config;
use crate::services::{FileService, ServiceContext};

use super::blob::{BlobError, BlobRef, MAX_BLOB_BYTES};
use super::door::{self, Door};
use super::{BackendError, Ctx, Op, Scope};

/// The fallback folder for a clip, under the profile home, for a session
/// with no project (#3114).
pub const VOICE_DIR: &str = "voice";

/// What a take is named inside its folder: only a `voice-*` file is a
/// message whose words came out of the microphone (#3044).
pub const RECORDING_PREFIX: &str = "voice-";

/// What a spoken answer is named inside the same folder. Deliberately not
/// [`RECORDING_PREFIX`], so the agent's own voice is never read back as a
/// take.
pub const REPLY_PREFIX: &str = "reply-";

/// What a take that could not be heard answers when no engine is set up.
const STT_OFF: &str = "no speech-to-text provider is configured yet: add one in Settings, and the microphone has somewhere to send the audio";

/// What a take answers when the engine heard nothing.
const HEARD_NOTHING: &str =
    "the transcription came back empty: check the microphone level and try again";

/// What a remote caller reads when the speech-to-text engine failed. The
/// engine's own error can name the crab's endpoint, its files or the
/// provider's answer, so it stays in the crab's log.
const TRANSCRIBE_FAILED: &str = "the crab could not transcribe this take; see the crab's log";

/// What a remote caller reads when the text-to-speech engine failed.
const SPEAK_FAILED: &str = "the crab could not speak this reply; see the crab's log";

/// The text an engine failure answers with: the whole chain at the local
/// door, as before, and `generic` at the remote one, with the chain logged
/// on the crab instead.
fn engine_failure(door: Door, detail: String, generic: &str) -> String {
    match door {
        Door::Local => detail,
        Door::Remote => {
            tracing::warn!("{generic} (remote caller): {detail}");
            generic.to_string()
        }
    }
}

/// Refuse the crab's voice on `session_id` to a remote caller whose
/// principal did not open the session's running or last remote turn
/// ([`super::turn::turn_principal`]): its engines would be paid for and its
/// clips kept in someone else's conversation. The local door is not
/// asked, and neither is a take with no session.
///
/// `before_any_turn` lets a take through on a session no remote turn has
/// run on yet: the question is heard before the turn that asks it starts.
fn own_turn(session_id: Option<Uuid>, before_any_turn: bool) -> Result<(), BackendError> {
    let (Some(caller), Some(session_id)) = (door::caller(), session_id) else {
        return Ok(());
    };
    match super::turn::turn_principal(session_id) {
        Some(owner) if owner == caller.principal => Ok(()),
        None if before_any_turn => Ok(()),
        _ => Err(BackendError::Forbidden {
            scope: Scope::Admin,
        }),
    }
}

/// Which voice engines the crab has, so the window promises only what the
/// crab can do: a microphone where it can hear, a spoken answer where it can
/// speak.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceStatus {}

/// The crab's answer to [`VoiceStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceReady {
    /// A speech-to-text engine is configured.
    pub stt_enabled: bool,
    /// A text-to-speech engine is configured.
    pub tts_enabled: bool,
}

impl Op for VoiceStatus {
    const NAME: &'static str = "voice.status";
    const SCOPE: Scope = Scope::Read;
    type Reply = VoiceReady;

    async fn run(self, _ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        // The in-memory mirror behind an `Arc`, never a disk read: the same
        // source the window's `voice_ready` asks.
        let voice = Config::current().voice_config();
        Ok(VoiceReady {
            stt_enabled: voice.stt_enabled,
            tts_enabled: voice.tts_enabled,
        })
    }
}

/// Hear one take on the crab, and keep it as a voice note when it is a
/// message (#3044).
///
/// `keep: false` is the Seeker's dictation: a search query is not a message,
/// so no clip is written. A take is kept only after it was heard, so a take
/// with no words leaves no orphan file; a save that fails is logged and the
/// words still come back, because the transcript is what the agent answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTranscribe {
    /// The conversation the take belongs to, which decides its folder.
    pub session_id: Option<Uuid>,
    /// The take as OGG/Opus, standard base64. A take that is not kept may
    /// come as the WAV it was recorded as: the engines read either.
    pub ogg_base64: String,
    /// Whether to keep the take as the message's voice note.
    pub keep: bool,
}

/// The crab's answer to [`VoiceTranscribe`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heard {
    /// The words, trimmed, never empty.
    pub text: String,
    /// The language the engine reported, when it did.
    pub language: Option<String>,
    /// The kept voice note on the crab, when one was asked for and saved.
    pub clip: Option<BlobRef>,
}

impl Op for VoiceTranscribe {
    const NAME: &'static str = "voice.transcribe";
    const SCOPE: Scope = Scope::Send;
    type Reply = Heard;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let VoiceTranscribe {
            session_id,
            ogg_base64,
            keep,
        } = self;
        let door = door::current();
        // Refuse an oversized take before decoding it, as `blob.put` does:
        // base64 is four characters per three bytes.
        let encoded = ogg_base64.len() as u64;
        if encoded / 4 * 3 > MAX_BLOB_BYTES + 3 {
            return Err(BlobError::TooLarge {
                size: encoded / 4 * 3,
            }
            .into());
        }
        let ogg = BASE64
            .decode(ogg_base64.as_bytes())
            .map_err(|e| BlobError::BadEncoding(e.to_string()))?;
        own_turn(session_id, true)?;
        let voice = Config::current().voice_config();
        if !voice.stt_enabled {
            return Err(BackendError::Failed(STT_OFF.to_string()));
        }
        let kept = keep.then(|| ogg.clone());
        let (text, language) = crate::channels::voice::transcribe_with_language(ogg, &voice)
            .await
            .map_err(|e| {
                BackendError::Failed(engine_failure(door, format!("{e:#}"), TRANSCRIBE_FAILED))
            })?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err(BackendError::Failed(HEARD_NOTHING.to_string()));
        }
        tracing::info!(
            "voice capture: transcribed {} character(s) in {}",
            text.chars().count(),
            language.as_deref().unwrap_or("an unreported language"),
        );
        let clip = match kept {
            Some(bytes) => match write_clip(ctx, &bytes, RECORDING_PREFIX, session_id).await {
                Ok(path) => Some(BlobRef {
                    path: path.to_string_lossy().into_owned(),
                    size: Some(bytes.len() as u64),
                }),
                Err(reason) => {
                    tracing::error!("voice capture: {reason}");
                    None
                }
            },
            None => None,
        };
        Ok(Heard {
            text,
            language,
            clip,
        })
    }
}

/// Speak one answer on the crab, for the turn that began as a recording
/// (#3044).
///
/// The crab synthesizes with its own engine, keeps the clip in the
/// session's folder on its own disk, and puts the marker on the answer row
/// with the path it wrote itself: no caller ever names the path. The reply is
/// the kept clip, `None` when the crab has no speech engine (the turn
/// already answered in text) or the answer has no words.
///
/// By the remote door only the principal that opened the session's running
/// or last remote turn is spoken to; any other caller is refused.
///
/// No remote caller names the words either: by the remote door the crab
/// speaks the answer its own ledger keeps for the session, and `text` is not
/// read. Only the window, at the local door, hands in the answer as its turn
/// settled it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSpeak {
    /// The session whose answer is spoken.
    pub session_id: Uuid,
    /// The answer as the turn settled it. Read at the local door only.
    pub text: String,
}

impl Op for VoiceSpeak {
    const NAME: &'static str = "voice.speak";
    const SCOPE: Scope = Scope::Send;
    type Reply = Option<BlobRef>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let VoiceSpeak { session_id, text } = self;
        let door = door::current();
        let text = words_to_speak(ctx, door, session_id, text).await?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        own_turn(Some(session_id), false)?;
        let voice = Config::current().voice_config();
        if !voice.tts_enabled {
            // A recording is answered in text when speech output is off, the
            // same switch that decides whether a Telegram voice note gets a
            // voice back.
            tracing::info!("voice reply: tts disabled, answering in text only");
            return Ok(None);
        }
        // Only the spoken half is read aloud. A model that skipped the block
        // is still read, from the paragraph it led with (#3092).
        let (answer, spoken) = crate::channels::voice::spoken::split(&text);
        let speech = match spoken {
            Some(spoken) => spoken,
            None => {
                tracing::warn!("voice reply: no spoken block, reading the first paragraph");
                crate::channels::voice::spoken::first_paragraph(&answer)
            }
        };
        let audio = crate::channels::voice::synthesize(&speech, &voice)
            .await
            .map_err(|e| {
                BackendError::Failed(engine_failure(
                    door,
                    format!("voice reply synthesis: {e:#}"),
                    SPEAK_FAILED,
                ))
            })?;
        keep_reply(ctx, session_id, &audio).await.map(Some)
    }
}

/// The answer [`VoiceSpeak`] reads aloud: the window's own at the local door,
/// the session's last assistant row in the crab's ledger at the remote one,
/// whatever text the remote caller sent. Empty when the session has no
/// answer yet, which speaks nothing.
async fn words_to_speak(
    ctx: &Ctx,
    door: Door,
    session_id: Uuid,
    text: String,
) -> Result<String, BackendError> {
    if door == Door::Local {
        return Ok(text);
    }
    let db = ctx
        .database()
        .await
        .map_err(|e| BackendError::Failed(format!("voice reply db connect: {e}")))?;
    let row = crate::db::repository::MessageRepository::new(db.pool().clone())
        .get_last_assistant_message(session_id)
        .await
        .map_err(|e| BackendError::Failed(format!("voice reply row lookup: {e}")))?;
    Ok(row.map(|row| row.content).unwrap_or_default())
}

/// Keep a synthesized answer in the session's folder and mark the answer row
/// with the path written here, on the crab.
async fn keep_reply(ctx: &Ctx, session_id: Uuid, ogg: &[u8]) -> Result<BlobRef, BackendError> {
    let path = write_clip(ctx, ogg, REPLY_PREFIX, Some(session_id))
        .await
        .map_err(|e| BackendError::Failed(format!("voice reply save: {e}")))?;
    super::speak::attach_marker(ctx, session_id, &path).await?;
    tracing::info!(
        "voice reply: {} bytes spoken into {}",
        ogg.len(),
        path.display()
    );
    Ok(BlobRef {
        path: path.to_string_lossy().into_owned(),
        size: Some(ogg.len() as u64),
    })
}

/// The folder a clip belongs in (#3114): the session's project attachments
/// folder when it has one, the profile's `voice/` otherwise.
async fn clip_dir(ctx: &Ctx, session: Option<Uuid>) -> PathBuf {
    project_clip_dir(ctx, session)
        .await
        .unwrap_or_else(|| crate::config::opencrabs_home().join(VOICE_DIR))
}

/// `projects/<slug>/files/` for the session's project, through the resolver
/// every other attachment goes through, on the crab's own ledger.
async fn project_clip_dir(ctx: &Ctx, session: Option<Uuid>) -> Option<PathBuf> {
    let session = session?;
    let db = ctx.database().await.ok()?;
    FileService::new(ServiceContext::new(db.pool().clone()))
        .project_files_dir(session)
        .await
}

/// Write one finished clip into its conversation's folder, named for the
/// millisecond it was saved: two clips inside one second would otherwise
/// overwrite each other, and the older message would play the newer audio.
async fn write_clip(
    ctx: &Ctx,
    bytes: &[u8],
    prefix: &str,
    session: Option<Uuid>,
) -> Result<PathBuf, String> {
    let dir = clip_dir(ctx, session).await;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("the voice folder would not open ({}): {e}", dir.display()))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{prefix}{stamp}.ogg"));
    std::fs::write(&path, bytes)
        .map_err(|e| format!("the clip would not save ({}): {e}", path.display()))?;
    Ok(path)
}

ops! {
    VoiceStatus,
    VoiceTranscribe,
    VoiceSpeak,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::blob;
    use crate::backend::door::{self, Caller};
    use crate::backend::parity::{Gateway, forbidden, parity};
    use crate::db::models::{Message, Project, Session};
    use crate::db::repository::{MessageRepository, ProjectRepository, SessionRepository};

    /// A session with a question and its answer, inside a project when
    /// `project` names one.
    async fn answered(gateway: &Gateway, project: Option<&str>) -> Session {
        let db = gateway.ctx.db().await.expect("db");
        let mut chat = Session::new(Some("chat".into()), None, None);
        if let Some(name) = project {
            let project = Project::new(name.into(), None);
            ProjectRepository::new(db.pool().clone())
                .create(&project)
                .await
                .expect("create project");
            chat.project_id = Some(project.id);
        }
        SessionRepository::new(db.pool().clone())
            .create(&chat)
            .await
            .expect("create session");
        let messages = MessageRepository::new(db.pool().clone());
        for (seq, (role, content)) in [("user", "question"), ("assistant", "answer")]
            .into_iter()
            .enumerate()
        {
            let row = Message::new(chat.id, role.into(), content.into(), seq as i32 + 1);
            messages.create(&row).await.expect("message");
        }
        chat
    }

    async fn answer_row(gateway: &Gateway, chat: &Session) -> String {
        let db = gateway.ctx.db().await.expect("db");
        MessageRepository::new(db.pool().clone())
            .get_last_assistant_message(chat.id)
            .await
            .expect("read")
            .expect("the answer row")
            .content
    }

    #[tokio::test]
    async fn voice_status_has_parity() {
        let gateway = Gateway::start().await;
        let answer = parity!(gateway, VoiceStatus {});
        assert!(answer["Ok"]["stt_enabled"].is_boolean(), "{answer}");
        assert!(answer["Ok"]["tts_enabled"].is_boolean(), "{answer}");
        forbidden!(gateway, VoiceStatus {});
    }

    /// A take that is not base64 is refused before any engine is asked, by
    /// both doors alike.
    #[tokio::test]
    async fn voice_transcribe_has_parity() {
        let gateway = Gateway::start().await;
        let answer = parity!(
            gateway,
            VoiceTranscribe {
                session_id: None,
                ogg_base64: "not base64!".into(),
                keep: true,
            }
        );
        assert_eq!(answer["Err"]["kind"], "bad_params", "{answer}");
        forbidden!(
            gateway,
            VoiceTranscribe {
                session_id: None,
                ogg_base64: BASE64.encode(b"OggS"),
                keep: false,
            }
        );
    }

    /// An answer with no words is not spoken, and nothing is written.
    #[tokio::test]
    async fn voice_speak_has_parity() {
        let gateway = Gateway::start().await;
        let chat = answered(&gateway, None).await;
        let answer = parity!(
            gateway,
            VoiceSpeak {
                session_id: chat.id,
                text: "  \n ".into(),
            }
        );
        assert_eq!(answer, serde_json::json!({ "Ok": null }));
        forbidden!(
            gateway,
            VoiceSpeak {
                session_id: chat.id,
                text: "spoken".into(),
            }
        );
        assert_eq!(answer_row(&gateway, &chat).await, "answer");
    }

    /// The remote door reads aloud the answer the crab's ledger keeps, never
    /// the words its caller sent; the window's door reads the answer it hands
    /// in, as before.
    #[tokio::test]
    async fn the_remote_door_speaks_the_ledgers_answer_not_the_callers_text() {
        let gateway = Gateway::start().await;
        let chat = answered(&gateway, None).await;
        let remote = gateway
            .within(words_to_speak(
                &gateway.ctx,
                Door::Remote,
                chat.id,
                "free words".into(),
            ))
            .await
            .expect("remote words");
        assert_eq!(remote, "answer", "the remote door speaks the stored answer");
        let local = gateway
            .within(words_to_speak(
                &gateway.ctx,
                Door::Local,
                chat.id,
                "free words".into(),
            ))
            .await
            .expect("local words");
        assert_eq!(local, "free words", "the window's answer is its own");
    }

    /// A remote caller that sends words for a session with no answer gets
    /// nothing spoken and nothing kept: its text is never synthesized.
    #[tokio::test]
    async fn a_remote_caller_cannot_have_its_own_text_spoken() {
        let gateway = Gateway::start().await;
        let silent = Uuid::new_v4();
        let caller = Caller {
            principal: "api_key".to_string(),
            scopes: vec![Scope::Read, Scope::Send],
        };
        let spoken = gateway
            .within(door::remote_as(
                caller,
                VoiceSpeak {
                    session_id: silent,
                    text: "words the crab never said".into(),
                }
                .run(&gateway.ctx),
            ))
            .await
            .expect("an answer");
        assert_eq!(spoken, None);
        assert!(
            !gateway.home().join(VOICE_DIR).exists(),
            "no clip was written"
        );
    }

    /// Run `op` by the remote door for `principal`, holding `send`.
    async fn as_principal<O: Op>(
        gateway: &Gateway,
        principal: &str,
        op: O,
    ) -> Result<O::Reply, BackendError> {
        let caller = Caller {
            principal: principal.to_string(),
            scopes: vec![Scope::Read, Scope::Send],
        };
        gateway
            .within(door::remote_as(caller, op.run(&gateway.ctx)))
            .await
    }

    const NOT_YOURS: BackendError = BackendError::Forbidden {
        scope: Scope::Admin,
    };

    /// The principal whose remote turn the session ran is spoken to; another
    /// principal on the same session, or any principal on a session no
    /// remote turn ran on, is refused before any engine or clip. The window
    /// is never asked.
    #[tokio::test]
    async fn only_the_principal_of_the_sessions_turn_has_its_answer_spoken() {
        let gateway = Gateway::start().await;
        let chat = answered(&gateway, None).await;
        let speak = || VoiceSpeak {
            session_id: chat.id,
            text: "spoken".into(),
        };
        assert_eq!(
            as_principal(&gateway, "alice", speak()).await,
            Err(NOT_YOURS),
            "no remote turn ran on the session"
        );
        crate::backend::turn::turn_ran_for(chat.id, "alice");
        assert_eq!(
            crate::backend::turn::turn_principal(chat.id).as_deref(),
            Some("alice")
        );
        let alice = as_principal(&gateway, "alice", speak()).await;
        assert_ne!(alice, Err(NOT_YOURS), "alice's own turn: {alice:?}");
        assert_eq!(as_principal(&gateway, "bob", speak()).await, Err(NOT_YOURS));
        let local = gateway.local(speak()).await;
        assert_ne!(local, Err(NOT_YOURS), "the window is not asked: {local:?}");
    }

    /// A take on a session whose remote turn is another principal's is
    /// refused before it is heard; the turn's own principal, a session no
    /// remote turn ran on, a take with no session and the window are heard.
    #[tokio::test]
    async fn only_the_principal_of_the_sessions_turn_has_a_take_heard_there() {
        let gateway = Gateway::start().await;
        let chat = answered(&gateway, None).await;
        let take = |session_id| VoiceTranscribe {
            session_id,
            ogg_base64: BASE64.encode(b"OggS"),
            keep: false,
        };
        let fresh = as_principal(&gateway, "bob", take(Some(chat.id))).await;
        assert_ne!(fresh, Err(NOT_YOURS), "before any turn: {fresh:?}");
        crate::backend::turn::turn_ran_for(chat.id, "alice");
        assert_eq!(
            as_principal(&gateway, "bob", take(Some(chat.id))).await,
            Err(NOT_YOURS)
        );
        for answer in [
            as_principal(&gateway, "alice", take(Some(chat.id))).await,
            as_principal(&gateway, "bob", take(None)).await,
            gateway.local(take(Some(chat.id))).await,
        ] {
            assert_ne!(answer, Err(NOT_YOURS), "{answer:?}");
        }
    }

    /// An engine failure tells the window the whole chain, as it did, and a
    /// remote caller only that the crab could not hear or speak.
    #[test]
    fn an_engine_failure_is_generic_by_the_remote_door_only() {
        let detail = "voice reply synthesis: POST http://10.0.0.5:8000/tts: 401 sk-...";
        assert_eq!(
            engine_failure(Door::Local, detail.to_string(), SPEAK_FAILED),
            detail
        );
        for generic in [TRANSCRIBE_FAILED, SPEAK_FAILED] {
            let remote = engine_failure(Door::Remote, detail.to_string(), generic);
            assert_eq!(remote, generic);
            assert!(!remote.contains("10.0.0.5") && !remote.contains("sk-"));
        }
    }

    /// A take over the blob ceiling is refused on its length, before a byte
    /// of it is decoded: this one is not even base64, and the answer is the
    /// size, not the encoding.
    #[tokio::test]
    async fn a_take_over_the_ceiling_is_refused_before_decoding() {
        let gateway = Gateway::start().await;
        let oversized = "!".repeat(((MAX_BLOB_BYTES + 6) / 3 * 4) as usize);
        let take = VoiceTranscribe {
            session_id: None,
            ogg_base64: oversized,
            keep: true,
        };
        let local = gateway.local(take.clone()).await;
        let remote = gateway.within(door::remote(take.run(&gateway.ctx))).await;
        for answer in [local, remote] {
            match answer {
                Err(BackendError::Failed(reason)) => {
                    assert!(reason.contains("over the 50 MiB limit"), "{reason}")
                }
                other => panic!("refused on its size, before decoding: {other:?}"),
            }
        }
    }

    /// A spoken answer kept for a remote caller is marked with the path the
    /// crab wrote, inside the roots the crab serves blobs from, so the window
    /// that asked fetches it from the crab instead of opening its own disk.
    #[tokio::test]
    async fn a_reply_spoken_by_the_remote_door_is_marked_inside_the_crab() {
        let gateway = Gateway::start().await;
        let chat = answered(&gateway, Some("Voice Work")).await;
        let caller = Caller {
            principal: "api_key".to_string(),
            scopes: vec![Scope::Read, Scope::Send],
        };
        let kept = gateway
            .within(door::remote_as(
                caller,
                keep_reply(&gateway.ctx, chat.id, b"OggS clip"),
            ))
            .await
            .expect("kept");
        assert_eq!(kept.size, Some(9));
        let file = PathBuf::from(&kept.path);
        assert!(
            file.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(REPLY_PREFIX) && name.ends_with(".ogg")),
            "an answer is named as one: {file:?}"
        );
        assert!(
            blob::resolve(&blob::roots(gateway.home()), &kept.path).is_ok(),
            "the clip sits where the crab serves blobs from: {file:?}"
        );
        assert_eq!(
            answer_row(&gateway, &chat).await,
            format!("answer\n\n<<AUD:{}>>", kept.path),
            "the row carries the crab's own path"
        );
    }
}
