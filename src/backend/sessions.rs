//! The session rail: listing, naming, topics, favourites and closing.
//!
//! Operations land here as their GUI doors move behind the contract.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::models::Session;
use crate::db::repository::channel_message::ChannelMessageRepository;
use crate::db::repository::session::{SessionListOptions, SessionRepository};
use crate::db::repository::session_favorites::SessionFavoritesRepository;
use crate::db::repository::{ContentSearchRow, MessageRepository};
use crate::ui_core::rail;

use super::{BackendError, BlobRef, Ctx, Op, Scope};

/// The hard cap so a long history cannot make the rail unbounded.
pub const RAIL_LIMIT: usize = 200;

/// The rail's session list, newest first, at most [`RAIL_LIMIT`] rows.
///
/// The read example every other load copies: open the ledger through
/// [`Ctx::db`] (pragmas only, never migrations, so a read cannot race the
/// daemon that owns the schema), ask the repository, and carry a failure as
/// its `{e:#}` text, which is what the window showed before.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadSessions {}

impl Op for LoadSessions {
    const NAME: &'static str = "sessions.load";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<Session>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.list(SessionListOptions {
            limit: Some(RAIL_LIMIT),
            ..Default::default()
        })
        .await
        .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

/// Rename a session or topic. The caller re-reads the rail afterwards.
///
/// The write example: same door as the read, a `Manage` scope because it
/// changes the other crab's ledger, and `()` as the reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameSession {
    /// The session or topic to rename.
    pub id: Uuid,
    /// Its new title, stored as given.
    pub title: String,
}

impl Op for RenameSession {
    const NAME: &'static str = "sessions.rename";
    const SCOPE: Scope = Scope::Manage;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.rename(self.id, &self.title)
            .await
            .map_err(|e| BackendError::Failed(format!("{e:#}")))
    }
}

/// A repository failure as the window has always shown it: `{e:#}`.
fn failed(e: impl std::fmt::Display) -> BackendError {
    BackendError::Failed(format!("{e:#}"))
}

/// The ledger's change signature, for the rail's "anything new?" poll
/// (#2881). `None` when the read failed, which the caller must treat as "no
/// news" rather than "the ledger is empty", so this never answers an error.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RailLedgerSignature {}

impl Op for RailLedgerSignature {
    const NAME: &'static str = "sessions.ledger_signature";
    const SCOPE: Scope = Scope::Read;
    type Reply = Option<(i64, i64)>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let Ok(db) = ctx.database().await else {
            return Ok(None);
        };
        let repo = SessionRepository::new(db.pool().clone());
        Ok(repo.ledger_signature().await.ok())
    }
}

/// The pinned set, as ids: the rail already holds the rows, this only says
/// which of them go on top.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadFavorites {}

impl Op for LoadFavorites {
    const NAME: &'static str = "sessions.favorites";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<Uuid>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx
            .database()
            .await
            .map_err(|e| BackendError::Failed(format!("db connect: {e}")))?;
        let repo = SessionFavoritesRepository::new(db.pool().clone());
        let ids = repo
            .all()
            .await
            .map_err(|e| BackendError::Failed(format!("favorites read: {e}")))?;
        // The sidecar stores ids as text; a malformed one is a foreign key
        // that got past SQLite, so it is skipped rather than failing the
        // whole rail.
        Ok(ids.iter().filter_map(|s| Uuid::parse_str(s).ok()).collect())
    }
}

/// One session's latest message, as the rail card's preview reads it. The
/// window shapes the line itself; this carries the stored role and text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    /// The row's role: `user`, `assistant`, or whatever a tool turn wrote.
    pub role: String,
    /// The content, whitespace already collapsed by the repository.
    pub text: String,
}

/// The preview of every session in `ids`, in one batched query, so a rail of
/// 200 cards costs one round trip rather than 200. A session with no message
/// is absent from the answer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadPreviews {
    /// The sessions to preview.
    pub ids: Vec<Uuid>,
}

impl Op for LoadPreviews {
    const NAME: &'static str = "sessions.previews";
    const SCOPE: Scope = Scope::Read;
    type Reply = HashMap<Uuid, Preview>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = MessageRepository::new(db.pool().clone());
        let previews = repo.last_messages_for(&self.ids).await.map_err(failed)?;
        Ok(previews
            .into_iter()
            .map(|(id, p)| {
                (
                    id,
                    Preview {
                        role: p.role,
                        text: p.text,
                    },
                )
            })
            .collect())
    }
}

/// How many content matches one search answers at most, so a common word
/// cannot flood the rail.
pub const CONTENT_MATCH_LIMIT: usize = 50;

/// One session a content search found, the wire twin of [`ContentSearchRow`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentMatch {
    pub session_id: Uuid,
    pub title: String,
    /// Snippet centered on the query, taken from the session's newest hit.
    pub snippet: String,
    /// How many of the returned message hits belonged to this session.
    pub match_count: usize,
    /// `created_at` of the newest matching hit. Rows sort by this, newest first.
    pub last_hit_at: DateTime<Utc>,
}

impl From<ContentSearchRow> for ContentMatch {
    fn from(row: ContentSearchRow) -> Self {
        Self {
            session_id: row.session_id,
            title: row.title,
            snippet: row.snippet,
            match_count: row.match_count,
            last_hit_at: row.last_hit_at,
        }
    }
}

impl From<ContentMatch> for ContentSearchRow {
    fn from(row: ContentMatch) -> Self {
        Self {
            session_id: row.session_id,
            title: row.title,
            snippet: row.snippet,
            match_count: row.match_count,
            last_hit_at: row.last_hit_at,
        }
    }
}

/// ctrl+f's deep read (#2720): which sessions mention `query` in their
/// messages, with a snippet. Spans every session, archived included, at most
/// [`CONTENT_MATCH_LIMIT`] rows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadContentMatches {
    /// What to look for.
    pub query: String,
}

impl Op for LoadContentMatches {
    const NAME: &'static str = "sessions.content_matches";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<ContentMatch>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let messages = MessageRepository::new(db.pool().clone());
        let sessions = SessionRepository::new(db.pool().clone());
        let rows = crate::db::repository::search_session_content(
            &messages,
            &sessions,
            &self.query,
            CONTENT_MATCH_LIMIT,
        )
        .await
        .map_err(failed)?;
        Ok(rows.into_iter().map(ContentMatch::from).collect())
    }
}

/// What the topic info panel shows (#2304).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicInfo {
    pub id: Uuid,
    pub parent_title: Option<String>,
    pub created_at: i64,
    pub messages: i64,
}

/// One session's info for the panel. A session that is not a topic reports
/// no parent; an unknown one answers `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadTopicInfo {
    /// The session to describe.
    pub id: Uuid,
}

impl Op for LoadTopicInfo {
    const NAME: &'static str = "sessions.topic_info";
    const SCOPE: Scope = Scope::Read;
    type Reply = Option<TopicInfo>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        let Some((parent, created_at, messages)) =
            repo.topic_info(self.id).await.map_err(failed)?
        else {
            return Ok(None);
        };
        let parent_title = match parent {
            Some(p) => repo
                .find_by_id(p)
                .await
                .map_err(failed)?
                .and_then(|s| s.title),
            None => None,
        };
        Ok(Some(TopicInfo {
            id: self.id,
            parent_title,
            created_at,
            messages,
        }))
    }
}

/// Every closed session. A closed topic is dimmed and refuses new turns (#2305).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadClosed {}

impl Op for LoadClosed {
    const NAME: &'static str = "sessions.closed";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<Uuid>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.closed_ids().await.map_err(failed)
    }
}

/// Close or reopen one topic. The caller re-reads the closed set afterwards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetClosed {
    pub id: Uuid,
    pub closed: bool,
}

impl Op for SetClosed {
    const NAME: &'static str = "sessions.set_closed";
    const SCOPE: Scope = Scope::Manage;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.set_closed(self.id, self.closed).await.map_err(failed)
    }
}

/// Every (topic, parent) pair. Read apart from the rows, like favorites, so
/// the grouping does not change the row model (#2303).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadTopicParents {}

impl Op for LoadTopicParents {
    const NAME: &'static str = "sessions.topic_parents";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<(Uuid, Uuid)>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.topic_parents().await.map_err(failed)
    }
}

/// Create a topic under a session and answer its id. The database refuses a
/// parent that is already a topic; that refusal comes back as the error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTopic {
    pub parent: Uuid,
    pub title: String,
}

impl Op for CreateTopic {
    const NAME: &'static str = "sessions.create_topic";
    const SCOPE: Scope = Scope::Manage;
    type Reply = Uuid;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.create_topic(self.parent, &self.title)
            .await
            .map_err(failed)
    }
}

/// Candidates for the Group topics picker: every listed session other than
/// `target`, each as (id, title, date). Topic status is filtered by the
/// caller from the rail's own parent map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadGroupCandidates {
    pub target: Uuid,
}

impl Op for LoadGroupCandidates {
    const NAME: &'static str = "sessions.group_candidates";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<(Uuid, String, String)>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        let rows = repo
            .list(SessionListOptions::default())
            .await
            .map_err(failed)?;
        Ok(rows
            .into_iter()
            .filter(|s| s.id != self.target)
            .map(|s| {
                let title = s.title.unwrap_or_else(|| "(untitled)".to_string());
                let date = s.created_at.format("%m-%d %H:%M").to_string();
                (s.id, title, date)
            })
            .collect())
    }
}

/// Attach one existing session as a topic of `parent`. A guard refusal comes
/// back as its plain sentence, so the picker can show why instead of a raw
/// constraint error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachTopic {
    pub child: Uuid,
    pub parent: Uuid,
}

impl Op for AttachTopic {
    const NAME: &'static str = "sessions.attach_topic";
    const SCOPE: Scope = Scope::Manage;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.attach_topic(self.child, self.parent)
            .await
            .map_err(failed)
    }
}

/// Every session with topics turned on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadTopics {}

impl Op for LoadTopics {
    const NAME: &'static str = "sessions.topics";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<Uuid>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.topic_enabled_ids().await.map_err(failed)
    }
}

/// The prefix a custom image icon stores in front of its filename (#2790).
/// The same value the window's icon store writes.
pub const ICON_IMG_PREFIX: &str = "img:";

/// One topic's icon: the stored value as written, and for a custom image the
/// file it names on this crab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicIcon {
    pub id: Uuid,
    /// The stored value: an embedded mark name, a legacy glyph, or `img:<file>`.
    pub stored: String,
    /// For an `img:` value, the file under `gui-icons/` in this crab's home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<BlobRef>,
}

/// The file a stored `img:` value names, or `None` for any other value and
/// for anything but a bare filename: separators and `..` are refused, so a
/// stored value never points outside the icon store (#2790).
fn icon_blob(ctx: &Ctx, stored: &str) -> Option<BlobRef> {
    let name = stored.strip_prefix(ICON_IMG_PREFIX)?;
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    let home = ctx
        .home()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(crate::config::opencrabs_home);
    let path = home.join("gui-icons").join(name);
    Some(BlobRef::at(path.to_string_lossy().into_owned()))
}

/// Every topic's own icon, read with the topic state. An image icon carries a
/// [`BlobRef`] so a remote window can fetch the picture instead of opening a
/// path on its own disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadTopicIcons {}

impl Op for LoadTopicIcons {
    const NAME: &'static str = "sessions.topic_icons";
    const SCOPE: Scope = Scope::Read;
    type Reply = Vec<TopicIcon>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        let icons = repo.topic_icons().await.map_err(failed)?;
        Ok(icons
            .into_iter()
            .map(|(id, stored)| TopicIcon {
                id,
                blob: icon_blob(ctx, &stored),
                stored,
            })
            .collect())
    }
}

/// The live name of every Telegram topic session among `titles`, in ONE
/// query (#2490). A row keeps the name it was created with; Telegram is the
/// truth, and a rename lands in `channel_messages`. A title that names no
/// topic thread is skipped, and with none left the ledger is never opened.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadTelegramTopicTitles {
    /// Each session's id and stored title.
    pub titles: Vec<(Uuid, String)>,
}

impl Op for LoadTelegramTopicTitles {
    const NAME: &'static str = "sessions.telegram_topic_titles";
    const SCOPE: Scope = Scope::Read;
    type Reply = HashMap<Uuid, String>;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let wanted: Vec<(Uuid, String, String)> = self
            .titles
            .iter()
            .filter_map(|(id, title)| {
                let (chat, thread) = rail::topic_thread(title)?;
                Some((*id, chat.to_string(), thread.to_string()))
            })
            .collect();
        if wanted.is_empty() {
            return Ok(HashMap::new());
        }
        let db = ctx.db().await?;
        let repo = ChannelMessageRepository::new(db.pool().clone());
        let pairs: Vec<(String, String)> = wanted
            .iter()
            .map(|(_, chat, thread)| (chat.clone(), thread.clone()))
            .collect();
        let names = repo.topic_names("telegram", &pairs).await.map_err(failed)?;
        let mut out = HashMap::with_capacity(wanted.len());
        for (id, chat, thread) in wanted {
            if let Some(name) = names.get(&(chat, thread)) {
                out.insert(id, name.clone());
            }
        }
        Ok(out)
    }
}

/// Give one topic an icon, or clear it with `None`. The caller re-reads
/// afterwards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetTopicIcon {
    pub id: Uuid,
    #[serde(default)]
    pub icon: Option<String>,
}

impl Op for SetTopicIcon {
    const NAME: &'static str = "sessions.set_topic_icon";
    const SCOPE: Scope = Scope::Manage;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.set_topic_icon(self.id, self.icon.as_deref())
            .await
            .map_err(failed)
    }
}

/// Keep an uploaded image as a custom topic icon and answer the value a
/// topic stores for it (`img:<file>`), for [`SetTopicIcon`] to write.
///
/// A window on another machine cannot write this crab's `gui-icons/`: it
/// sends the picked file by `blob.put` and names the upload here. Only a file
/// in the upload directory is taken. The copy keeps the upload's own file
/// name (`<16 hex of its sha256>-<safe name>`), a bare name the icon store
/// accepts, so the same bytes kept twice are one icon and the answer repeats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IconFromUpload {
    /// The path `blob.put` answered.
    pub upload: String,
}

impl Op for IconFromUpload {
    const NAME: &'static str = "sessions.icon_from_upload";
    const SCOPE: Scope = Scope::Send;
    type Reply = String;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let home = super::blob::home_of(ctx);
        let uploads = home.join(super::blob::UPLOAD_DIR);
        let source = super::blob::resolve(&[uploads], &self.upload)?;
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty() && !name.contains(['/', '\\']) && !name.contains(".."))
            .ok_or_else(|| BackendError::BadParams(format!("not an icon name: {}", self.upload)))?
            .to_string();
        let dir = home.join("gui-icons");
        let target = dir.join(&name);
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir).and_then(|()| std::fs::copy(&source, &target))
        })
        .await
        .map_err(|e| BackendError::Failed(format!("icon: worker: {e}")))?
        .map_err(failed)?;
        Ok(format!("{ICON_IMG_PREFIX}{name}"))
    }
}

/// Turn topics on or off for one session. The caller re-reads afterwards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetTopics {
    pub id: Uuid,
    pub enabled: bool,
}

impl Op for SetTopics {
    const NAME: &'static str = "sessions.set_topics";
    const SCOPE: Scope = Scope::Manage;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.set_topics_enabled(self.id, self.enabled)
            .await
            .map_err(failed)
    }
}

/// Fold every topic of `id` back into it and switch topics off (#2863). One
/// repository transaction: it answers how many topics came home, or fails
/// with nothing changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeTopics {
    pub id: Uuid,
}

impl Op for MergeTopics {
    const NAME: &'static str = "sessions.merge_topics";
    const SCOPE: Scope = Scope::Manage;
    type Reply = usize;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionRepository::new(db.pool().clone());
        repo.merge_topics_into_parent(self.id).await.map_err(failed)
    }
}

/// Pin or unpin one session in the shared sidecar table. The caller refetches
/// afterwards, so the rail never shows a pin the ledger does not have.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetFavorite {
    pub id: Uuid,
    pub pinned: bool,
}

impl Op for SetFavorite {
    const NAME: &'static str = "sessions.set_favorite";
    const SCOPE: Scope = Scope::Manage;
    type Reply = ();

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let db = ctx.db().await?;
        let repo = SessionFavoritesRepository::new(db.pool().clone());
        let sid = self.id.to_string();
        if self.pinned {
            repo.add(&sid).await.map_err(failed)
        } else {
            repo.remove(&sid).await.map_err(failed)
        }
    }
}

/// Everything the rail reads on a refresh, in one answer.
///
/// Each part carries its own outcome, the way the window's separate loads
/// each did: a failed favorites read does not cost the rail its rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rail {
    /// Read first, so it is never newer than the rows beside it.
    pub signature: Option<(i64, i64)>,
    pub sessions: Result<Vec<Session>, String>,
    pub favorites: Result<Vec<Uuid>, String>,
    pub closed: Result<Vec<Uuid>, String>,
    pub topic_parents: Result<Vec<(Uuid, Uuid)>, String>,
    pub topics: Result<Vec<Uuid>, String>,
    pub topic_icons: Result<Vec<TopicIcon>, String>,
    /// Live Telegram topic names for the listed sessions.
    pub telegram_titles: Result<HashMap<Uuid, String>, String>,
    /// Previews for the listed sessions.
    pub previews: Result<HashMap<Uuid, Preview>, String>,
}

/// The rail in one round trip: a remote window pays the tunnel once per
/// refresh instead of once per load. Locally it is the same reads in a row.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RailSnapshot {}

impl Op for RailSnapshot {
    const NAME: &'static str = "sessions.rail_snapshot";
    const SCOPE: Scope = Scope::Read;
    type Reply = Rail;

    async fn run(self, ctx: &Ctx) -> Result<Self::Reply, BackendError> {
        let signature = RailLedgerSignature {}.run(ctx).await.ok().flatten();
        let sessions = LoadSessions {}.run(ctx).await.map_err(BackendError::into_message);
        let listed = sessions.as_deref().unwrap_or_default();
        let titles: Vec<(Uuid, String)> = listed
            .iter()
            .filter_map(|s| Some((s.id, s.title.clone()?)))
            .collect();
        let ids: Vec<Uuid> = listed.iter().map(|s| s.id).collect();
        Ok(Rail {
            signature,
            favorites: LoadFavorites {}.run(ctx).await.map_err(BackendError::into_message),
            closed: LoadClosed {}.run(ctx).await.map_err(BackendError::into_message),
            topic_parents: LoadTopicParents {}.run(ctx).await.map_err(BackendError::into_message),
            topics: LoadTopics {}.run(ctx).await.map_err(BackendError::into_message),
            topic_icons: LoadTopicIcons {}.run(ctx).await.map_err(BackendError::into_message),
            telegram_titles: LoadTelegramTopicTitles { titles }
                .run(ctx)
                .await
                .map_err(BackendError::into_message),
            previews: LoadPreviews { ids }.run(ctx).await.map_err(BackendError::into_message),
            sessions,
        })
    }
}

ops! {
    LoadSessions,
    RenameSession,
    RailLedgerSignature,
    LoadFavorites,
    LoadPreviews,
    LoadContentMatches,
    LoadTopicInfo,
    LoadClosed,
    SetClosed,
    LoadTopicParents,
    CreateTopic,
    LoadGroupCandidates,
    AttachTopic,
    LoadTopics,
    LoadTopicIcons,
    LoadTelegramTopicTitles,
    SetTopicIcon,
    IconFromUpload,
    SetTopics,
    MergeTopics,
    SetFavorite,
    RailSnapshot,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ctx::temp_ctx;

    #[tokio::test]
    async fn load_sessions_lists_what_the_ledger_holds() {
        let (_dir, ctx) = temp_ctx().await;
        let db = ctx.db().await.expect("db");
        let repo = SessionRepository::new(db.pool().clone());
        let first = Session::new(Some("first".into()), None, None);
        let second = Session::new(Some("second".into()), None, None);
        repo.create(&first).await.expect("create first");
        repo.create(&second).await.expect("create second");

        let rows = LoadSessions {}.run(&ctx).await.expect("load");
        let mut ids: Vec<_> = rows.iter().map(|s| s.id).collect();
        ids.sort();
        let mut want = vec![first.id, second.id];
        want.sort();
        assert_eq!(ids, want);
    }

    #[tokio::test]
    async fn rename_session_writes_the_new_title() {
        let (_dir, ctx) = temp_ctx().await;
        let db = ctx.db().await.expect("db");
        let repo = SessionRepository::new(db.pool().clone());
        let session = Session::new(Some("before".into()), None, None);
        repo.create(&session).await.expect("create");

        RenameSession {
            id: session.id,
            title: "after".into(),
        }
        .run(&ctx)
        .await
        .expect("rename");

        let stored = repo
            .find_by_id(session.id)
            .await
            .expect("read back")
            .expect("the row is still there");
        assert_eq!(stored.title.as_deref(), Some("after"));
    }

    #[tokio::test]
    async fn load_sessions_carries_a_failed_connect_as_text() {
        // A directory where the ledger file should be: the connect cannot
        // open it, and the failure arrives as `Failed`, never a panic.
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = Ctx::new(dir.path().to_path_buf());
        let err = LoadSessions {}
            .run(&ctx)
            .await
            .expect_err("a directory is no ledger");
        assert!(matches!(err, BackendError::Failed(_)), "{err:?}");
    }

    mod doors {
        use super::super::*;
        use crate::backend::parity::{Gateway, forbidden, parity};
        use crate::db::models::{ChannelMessage, Message};

        async fn repo(gateway: &Gateway) -> SessionRepository {
            let db = gateway.ctx.db().await.expect("db");
            SessionRepository::new(db.pool().clone())
        }

        async fn session(gateway: &Gateway, title: &str) -> Session {
            let session = Session::new(Some(title.into()), None, None);
            repo(gateway).await.create(&session).await.expect("create");
            session
        }

        async fn favorites(gateway: &Gateway) -> SessionFavoritesRepository {
            let db = gateway.ctx.db().await.expect("db");
            SessionFavoritesRepository::new(db.pool().clone())
        }

        async fn say(gateway: &Gateway, session: Uuid, text: &str) {
            let db = gateway.ctx.db().await.expect("db");
            MessageRepository::new(db.pool().clone())
                .create(&Message::new(session, "user".into(), text.into(), 1))
                .await
                .expect("message");
        }

        #[tokio::test]
        async fn rail_ledger_signature_has_parity() {
            let gateway = Gateway::start().await;
            session(&gateway, "one").await;
            let answer = parity!(gateway, RailLedgerSignature {});
            assert!(answer["Ok"].is_array(), "{answer}");
            forbidden!(gateway, RailLedgerSignature {});
        }

        #[tokio::test]
        async fn load_favorites_has_parity() {
            let gateway = Gateway::start().await;
            let pinned = session(&gateway, "pinned").await;
            favorites(&gateway)
                .await
                .add(&pinned.id.to_string())
                .await
                .expect("pin");
            let answer = parity!(gateway, LoadFavorites {});
            assert_eq!(answer["Ok"], serde_json::json!([pinned.id]), "{answer}");
            forbidden!(gateway, LoadFavorites {});
        }

        #[tokio::test]
        async fn load_previews_has_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway, "chat").await;
            say(&gateway, chat.id, "hello there").await;
            let answer = parity!(gateway, LoadPreviews { ids: vec![chat.id] });
            let preview = &answer["Ok"][chat.id.to_string()];
            assert_eq!(preview["role"], "user", "{answer}");
            assert_eq!(preview["text"], "hello there", "{answer}");
            forbidden!(gateway, LoadPreviews { ids: vec![chat.id] });
        }

        #[tokio::test]
        async fn load_content_matches_has_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway, "chat").await;
            say(&gateway, chat.id, "the walrus speaks").await;
            let answer = parity!(
                gateway,
                LoadContentMatches {
                    query: "walrus".into(),
                }
            );
            assert_eq!(
                answer["Ok"][0]["session_id"],
                chat.id.to_string(),
                "{answer}"
            );
            forbidden!(
                gateway,
                LoadContentMatches {
                    query: "walrus".into(),
                }
            );
        }

        #[tokio::test]
        async fn load_topic_info_has_parity() {
            let gateway = Gateway::start().await;
            let parent = session(&gateway, "parent").await;
            let topic = repo(&gateway)
                .await
                .create_topic(parent.id, "topic")
                .await
                .expect("topic");
            let answer = parity!(gateway, LoadTopicInfo { id: topic });
            assert_eq!(answer["Ok"]["parent_title"], "parent", "{answer}");
            forbidden!(gateway, LoadTopicInfo { id: topic });
        }

        #[tokio::test]
        async fn load_closed_and_set_closed_have_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway, "chat").await;
            parity!(
                gateway,
                SetClosed {
                    id: chat.id,
                    closed: true,
                }
            );
            let answer = parity!(gateway, LoadClosed {});
            assert_eq!(answer["Ok"], serde_json::json!([chat.id]), "{answer}");
            forbidden!(gateway, LoadClosed {});
            forbidden!(
                gateway,
                SetClosed {
                    id: chat.id,
                    closed: false,
                }
            );
            let still = repo(&gateway).await.closed_ids().await.expect("closed");
            assert_eq!(still, vec![chat.id], "a refused reopen wrote nothing");
        }

        #[tokio::test]
        async fn load_topic_parents_has_parity() {
            let gateway = Gateway::start().await;
            let parent = session(&gateway, "parent").await;
            let topic = repo(&gateway)
                .await
                .create_topic(parent.id, "topic")
                .await
                .expect("topic");
            let answer = parity!(gateway, LoadTopicParents {});
            assert_eq!(
                answer["Ok"],
                serde_json::json!([[topic, parent.id]]),
                "{answer}"
            );
            forbidden!(gateway, LoadTopicParents {});
        }

        #[tokio::test]
        async fn create_topic_has_parity() {
            // A create answers a fresh id each run, so the two doors are
            // compared on the refusal, which is the same every time: a parent
            // that is already a topic.
            let gateway = Gateway::start().await;
            let parent = session(&gateway, "parent").await;
            let topic = repo(&gateway)
                .await
                .create_topic(parent.id, "topic")
                .await
                .expect("topic");
            let answer = parity!(
                gateway,
                CreateTopic {
                    parent: topic,
                    title: "nested".into(),
                }
            );
            assert!(answer.get("Err").is_some(), "{answer}");

            let made = gateway
                .remote(
                    CreateTopic {
                        parent: parent.id,
                        title: "remote".into(),
                    },
                    Scope::ALL,
                )
                .await
                .expect("the remote door creates");
            let pairs = repo(&gateway).await.topic_parents().await.expect("pairs");
            assert!(pairs.contains(&(made, parent.id)), "{pairs:?}");

            forbidden!(
                gateway,
                CreateTopic {
                    parent: parent.id,
                    title: "refused".into(),
                }
            );
            let after = repo(&gateway).await.topic_parents().await.expect("pairs");
            assert_eq!(after.len(), pairs.len(), "a refused create wrote nothing");
        }

        #[tokio::test]
        async fn load_group_candidates_has_parity() {
            let gateway = Gateway::start().await;
            let target = session(&gateway, "target").await;
            let other = session(&gateway, "other").await;
            let answer = parity!(gateway, LoadGroupCandidates { target: target.id });
            let rows = answer["Ok"].as_array().expect("a list");
            assert_eq!(rows.len(), 1, "{answer}");
            assert_eq!(rows[0][0], other.id.to_string(), "{answer}");
            forbidden!(gateway, LoadGroupCandidates { target: target.id });
        }

        #[tokio::test]
        async fn attach_topic_has_parity() {
            // An attach that worked refuses the second time, so the doors are
            // compared on the guard refusal; the success runs once, remotely.
            let gateway = Gateway::start().await;
            let parent = session(&gateway, "parent").await;
            let child = session(&gateway, "child").await;
            let answer = parity!(
                gateway,
                AttachTopic {
                    child: parent.id,
                    parent: parent.id,
                }
            );
            assert!(answer.get("Err").is_some(), "{answer}");

            forbidden!(
                gateway,
                AttachTopic {
                    child: child.id,
                    parent: parent.id,
                }
            );
            assert!(
                repo(&gateway)
                    .await
                    .topic_parents()
                    .await
                    .expect("pairs")
                    .is_empty(),
                "a refused attach wrote nothing"
            );

            gateway
                .remote(
                    AttachTopic {
                        child: child.id,
                        parent: parent.id,
                    },
                    Scope::ALL,
                )
                .await
                .expect("the remote door attaches");
            let pairs = repo(&gateway).await.topic_parents().await.expect("pairs");
            assert_eq!(pairs, vec![(child.id, parent.id)]);
        }

        #[tokio::test]
        async fn load_topics_and_set_topics_have_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway, "chat").await;
            parity!(
                gateway,
                SetTopics {
                    id: chat.id,
                    enabled: true,
                }
            );
            let answer = parity!(gateway, LoadTopics {});
            assert_eq!(answer["Ok"], serde_json::json!([chat.id]), "{answer}");
            forbidden!(gateway, LoadTopics {});
            forbidden!(
                gateway,
                SetTopics {
                    id: chat.id,
                    enabled: false,
                }
            );
            let still = repo(&gateway).await.topic_enabled_ids().await.expect("ids");
            assert_eq!(still, vec![chat.id], "a refused switch wrote nothing");
        }

        #[tokio::test]
        async fn load_topic_icons_and_set_topic_icon_have_parity() {
            let gateway = Gateway::start().await;
            let parent = session(&gateway, "parent").await;
            let topic = repo(&gateway)
                .await
                .create_topic(parent.id, "topic")
                .await
                .expect("topic");
            parity!(
                gateway,
                SetTopicIcon {
                    id: topic,
                    icon: Some("img:crab.png".into()),
                }
            );
            let answer = parity!(gateway, LoadTopicIcons {});
            let icon = &answer["Ok"][0];
            assert_eq!(icon["stored"], "img:crab.png", "{answer}");
            let want = gateway.home().join("gui-icons").join("crab.png");
            assert_eq!(
                icon["blob"]["path"],
                want.to_string_lossy().into_owned(),
                "{answer}"
            );
            forbidden!(gateway, LoadTopicIcons {});
            forbidden!(
                gateway,
                SetTopicIcon {
                    id: topic,
                    icon: None
                }
            );
            let still = repo(&gateway).await.topic_icons().await.expect("icons");
            assert_eq!(still, vec![(topic, "img:crab.png".to_string())]);
        }

        #[tokio::test]
        async fn icon_from_upload_has_parity() {
            use base64::Engine as _;
            let gateway = Gateway::start().await;
            let upload = gateway
                .local(crate::backend::blob::BlobPut {
                    name: "crab.png".into(),
                    base64: base64::engine::general_purpose::STANDARD.encode(b"png"),
                })
                .await
                .expect("upload");
            let answer = parity!(
                gateway,
                IconFromUpload {
                    upload: upload.path.clone(),
                }
            );
            let stored = answer["Ok"].as_str().expect("a stored value").to_string();
            let name = stored.strip_prefix(ICON_IMG_PREFIX).expect("an img: value");
            assert!(name.ends_with("-crab.png"), "{stored}");
            let ctx = Ctx::new("/nowhere/opencrabs.db".into()).with_home(gateway.home().into());
            let blob = icon_blob(&ctx, &stored).expect("the icon store accepts the name");
            assert_eq!(std::fs::read(&blob.path).expect("the icon"), b"png");
            forbidden!(
                gateway,
                IconFromUpload {
                    upload: upload.path
                }
            );
        }

        #[tokio::test]
        async fn icon_from_upload_takes_only_an_upload() {
            let gateway = Gateway::start().await;
            let path = gateway.ctx.db_path().to_string_lossy().into_owned();
            let answer = parity!(gateway, IconFromUpload { upload: path });
            assert!(answer["Err"].is_object(), "{answer}");
            assert!(
                !gateway.home().join("gui-icons").exists(),
                "a refused upload wrote no icon"
            );
        }

        #[test]
        fn only_a_bare_image_filename_becomes_a_blob() {
            let ctx = Ctx::new("/nowhere/opencrabs.db".into()).with_home("/crab".into());
            assert!(icon_blob(&ctx, "img:crab.png").is_some());
            for stored in [
                "topic:ops",
                "img:",
                "img:../evil.png",
                "img:a/b.png",
                "img:a\\b.png",
            ] {
                assert_eq!(icon_blob(&ctx, stored), None, "{stored}");
            }
        }

        #[tokio::test]
        async fn load_telegram_topic_titles_has_parity() {
            let gateway = Gateway::start().await;
            let topic = session(&gateway, "Group [chat:-100:topic:7]").await;
            let db = gateway.ctx.db().await.expect("db");
            ChannelMessageRepository::new(db.pool().clone())
                .insert(&ChannelMessage {
                    id: Uuid::new_v4(),
                    channel: "telegram".into(),
                    channel_chat_id: "-100".into(),
                    channel_chat_name: None,
                    sender_id: "1".into(),
                    sender_name: "someone".into(),
                    content: "hi".into(),
                    message_type: "text".into(),
                    platform_message_id: None,
                    created_at: chrono::Utc::now(),
                    thread_id: Some("7".into()),
                    topic_name: Some("Renamed".into()),
                })
                .await
                .expect("channel message");
            let titles = vec![(topic.id, topic.title.clone().unwrap_or_default())];
            let answer = parity!(
                gateway,
                LoadTelegramTopicTitles {
                    titles: titles.clone(),
                }
            );
            assert_eq!(answer["Ok"][topic.id.to_string()], "Renamed", "{answer}");
            forbidden!(gateway, LoadTelegramTopicTitles { titles });
        }

        #[tokio::test]
        async fn merge_topics_has_parity() {
            // The first fold empties the group, so the doors are compared on
            // a session with no topics (both answer 0), and the real fold
            // runs once, remotely.
            let gateway = Gateway::start().await;
            let lone = session(&gateway, "lone").await;
            let answer = parity!(gateway, MergeTopics { id: lone.id });
            assert_eq!(answer["Ok"], 0, "{answer}");

            let parent = session(&gateway, "parent").await;
            repo(&gateway)
                .await
                .create_topic(parent.id, "topic")
                .await
                .expect("topic");
            forbidden!(gateway, MergeTopics { id: parent.id });
            assert_eq!(
                repo(&gateway)
                    .await
                    .topic_parents()
                    .await
                    .expect("pairs")
                    .len(),
                1,
                "a refused fold moved nothing"
            );
            let folded = gateway
                .remote(MergeTopics { id: parent.id }, Scope::ALL)
                .await
                .expect("the remote door folds");
            assert_eq!(folded, 1);
        }

        #[tokio::test]
        async fn set_favorite_has_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway, "chat").await;
            parity!(
                gateway,
                SetFavorite {
                    id: chat.id,
                    pinned: true,
                }
            );
            let pinned = favorites(&gateway).await.all().await.expect("all");
            assert_eq!(pinned, vec![chat.id.to_string()]);
            forbidden!(
                gateway,
                SetFavorite {
                    id: chat.id,
                    pinned: false,
                }
            );
            let still = favorites(&gateway).await.all().await.expect("all");
            assert_eq!(still, pinned, "a refused unpin wrote nothing");
        }

        #[tokio::test]
        async fn rail_snapshot_has_parity() {
            let gateway = Gateway::start().await;
            let chat = session(&gateway, "chat").await;
            say(&gateway, chat.id, "hello").await;
            favorites(&gateway)
                .await
                .add(&chat.id.to_string())
                .await
                .expect("pin");
            let answer = parity!(gateway, RailSnapshot {});
            let rail = &answer["Ok"];
            assert_eq!(
                rail["sessions"]["Ok"][0]["id"],
                chat.id.to_string(),
                "{answer}"
            );
            assert_eq!(
                rail["favorites"]["Ok"],
                serde_json::json!([chat.id]),
                "{answer}"
            );
            assert_eq!(
                rail["previews"]["Ok"][chat.id.to_string()]["text"],
                "hello",
                "{answer}"
            );
            forbidden!(gateway, RailSnapshot {});
        }
    }
}
