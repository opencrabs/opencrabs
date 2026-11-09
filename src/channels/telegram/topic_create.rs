//! Create a real Telegram forum topic from the desktop client (#2603).
//!
//! The window's "New topic" wrote a local session row and stopped there. That
//! row carries no Telegram chat key, so the inbound resolver
//! (`find_session_by_channel_chat_key`) can never find it, and the forum
//! thread it claims to represent does not exist: the user gets a dead local
//! session wearing a topic name while the chat itself stays empty.
//!
//! Only the Bot API can create a forum topic, and only Telegram can say which
//! thread id the new topic got. So the order is fixed: call the API first,
//! then key the local row on the thread id that came back. A refused or
//! offline API call is reported to the caller and NO row is written, because
//! a local-only row is exactly the bug this replaces.
//!
//! The row itself is written by [`write_forum_topic_row`], which takes the
//! thread id as an argument. That split is what makes the linking rules
//! testable without a network: the API call is the only part that needs a
//! live bot.

use teloxide::prelude::*;
use teloxide::types::ChatId;
use uuid::Uuid;

use crate::db::repository::SessionRepository;
use crate::services::{ServiceContext, SessionService};

use super::session_resolve::{build_session_title, chat_id_suffix};

/// What one successful create left behind.
#[derive(Debug, Clone)]
pub struct CreatedForumTopic {
    /// The forum chat the topic was made in.
    pub chat_id: i64,
    /// Telegram's thread id for the new topic. This is the identity the
    /// inbound resolver looks up, so it is never invented locally.
    pub thread_id: i32,
    /// The local session row keyed on that thread.
    pub session: Uuid,
    /// The forum parent the new row hangs under.
    pub parent: Uuid,
    /// The canonical title, ending in `[chat:<id>:topic:<tid>]`.
    pub title: String,
}

/// The chat a forum PARENT title belongs to, as `(chat id, chat name)`.
///
/// Parents are the rows [`super::topic_link`] creates or adopts: they read
/// `Telegram: <chat name> [chat:<id>]`. A topic title (same string plus
/// ` / <topic name>` and a `:topic:` suffix) is not a parent and returns
/// `None`, because attaching a new topic to a topic is refused by the depth
/// trigger and would only produce an error the user cannot act on.
///
/// Returns `None` for every other channel's rows, which is what keeps the
/// window's topic action from calling the Bot API on a local conversation.
pub fn chat_of_base_title(title: &str) -> Option<(i64, String)> {
    title.strip_prefix("Telegram: ")?;
    let bracket = title.rfind("[chat:")?;
    let inner = title
        .get(bracket..)?
        .strip_prefix("[chat:")?
        .strip_suffix(']')?;
    if inner.contains(":topic:") {
        return None;
    }
    let chat_id: i64 = inner.parse().ok()?;
    let name = title.get("Telegram: ".len()..bracket)?.trim_end();
    if name.is_empty() {
        return None;
    }
    Some((chat_id, name.to_string()))
}

/// The canonical title of a forum topic row.
///
/// Delegates to [`build_session_title`] so the window and the inbound resolver
/// spell the same key. Anything else would create a row that looks right and
/// resolves never.
pub fn forum_topic_title(chat_name: &str, chat_id: i64, thread_id: i32, name: &str) -> String {
    build_session_title(
        false,
        "",
        0,
        chat_name,
        chat_id,
        Some(thread_id),
        Some(name),
    )
}

/// The bot token, or a sentence saying why the window cannot create a topic.
///
/// A disabled channel or an empty token is a configuration answer, not a
/// network failure, and the caller shows it as-is.
fn telegram_token() -> Result<String, String> {
    let config = crate::config::Config::load()
        .map_err(|e| format!("could not read the config to create a topic: {e}"))?;
    let telegram = &config.channels.telegram;
    if !telegram.enabled {
        return Err("Telegram is turned off, so the forum cannot make a topic".to_string());
    }
    telegram
        .token
        .clone()
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| "Telegram has no bot token configured".to_string())
}

/// Create a forum topic in Telegram, then key a session row on the thread id
/// it returns.
///
/// `Ok(None)` means "this parent is not a Telegram forum": the caller keeps
/// its ordinary local topic behaviour. `Err` means the parent IS a forum and
/// the create failed, and the caller must surface it instead of falling back
/// to a local row.
pub async fn create_forum_topic_session(
    pool: &crate::db::Pool,
    parent: Uuid,
    name: &str,
) -> Result<Option<CreatedForumTopic>, String> {
    let session_svc = SessionService::new(ServiceContext::new(pool.clone()));
    let repo = SessionRepository::new(pool.clone());

    let parent_row = session_svc
        .get_session(parent)
        .await
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| format!("the conversation {parent} is gone"))?;
    let parent_title = parent_row.title.clone().unwrap_or_default();
    let Some((chat_id, chat_name)) = chat_of_base_title(&parent_title) else {
        return Ok(None);
    };

    // A forum is the only chat that keeps one row per topic. Asking first
    // keeps a plain group or a DM out of a refused API call.
    let topic_rows = repo
        .count_topics_for_chat(chat_id)
        .await
        .map_err(|e| format!("{e:#}"))?;
    if topic_rows == 0 {
        return Ok(None);
    }

    let name = name.trim();
    if name.is_empty() {
        return Err("a topic needs a name".to_string());
    }
    let token = telegram_token()?;
    let bot = teloxide::Bot::new(token);
    let topic = bot
        .create_forum_topic(ChatId(chat_id), name.to_string())
        .await
        .map_err(|e| format!("Telegram refused the topic: {e}"))?;
    let thread_id = topic.thread_id.0.0;

    let created = write_forum_topic_row(
        &session_svc,
        &repo,
        pool,
        parent,
        chat_id,
        &chat_name,
        thread_id,
        name,
    )
    .await?;
    tracing::info!(
        "GUI: created Telegram forum topic {thread_id} (\"{name}\") in chat {chat_id}; \
         session {} linked under {parent}",
        created.session
    );
    Ok(Some(created))
}

/// Write the local half: key a session row on `thread_id`, hang it under the
/// forum parent, and record the topic name for the chips.
///
/// Idempotent on the thread id, the same way an inbound message is: Telegram
/// thread ids are unique per chat, so the keyed insert resolves an existing
/// row instead of duplicating it.
#[allow(clippy::too_many_arguments)]
pub async fn write_forum_topic_row(
    session_svc: &SessionService,
    repo: &SessionRepository,
    pool: &crate::db::Pool,
    parent: Uuid,
    chat_id: i64,
    chat_name: &str,
    thread_id: i32,
    name: &str,
) -> Result<CreatedForumTopic, String> {
    // The child inherits the parent's provider, model and directory: a topic
    // opened from the window answers with the same brain as the chat it hangs
    // under, not with whatever the global default happens to be.
    let parent_row = session_svc
        .get_session(parent)
        .await
        .map_err(|e| format!("{e:#}"))?
        .ok_or_else(|| format!("the conversation {parent} is gone"))?;

    let title = forum_topic_title(chat_name, chat_id, thread_id, name);
    let key = chat_id_suffix(chat_id, Some(thread_id));
    let (child, created) = session_svc
        .insert_or_resolve_channel_session_reported(
            Some(title.clone()),
            parent_row.provider_name.clone(),
            parent_row.model.clone(),
            parent_row.working_directory.clone(),
            &key,
        )
        .await
        .map_err(|e| format!("{e:#}"))?;

    repo.attach_topic(child.id, parent)
        .await
        .map_err(|e| format!("{e}"))?;

    if created {
        // The chips read names from `channel_messages`, so a topic made from
        // the window has to announce itself the same way one made by the
        // agent tool does.
        super::record_topic_created(Some(pool.clone()), chat_id, thread_id, name, false).await;
    }

    Ok(CreatedForumTopic {
        chat_id,
        thread_id,
        session: child.id,
        parent,
        title,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_parent(pool: &crate::db::Pool, id: Uuid, title: &str) {
        let sid = id.to_string();
        let title = title.to_string();
        pool.get()
            .await
            .unwrap()
            .interact(move |conn| {
                conn.execute(
                    "INSERT INTO sessions (id, title, model, created_at, updated_at)
                     VALUES (?1, ?2, 'm', 0, 0)",
                    rusqlite::params![sid, title],
                )
            })
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn a_base_title_yields_its_chat() {
        assert_eq!(
            chat_of_base_title("Telegram: Memes [chat:-1001234567890]"),
            Some((-1001234567890, "Memes".to_string()))
        );
    }

    #[tokio::test]
    async fn a_topic_title_is_not_a_parent() {
        assert_eq!(
            chat_of_base_title("Telegram: Memes / Funny [chat:-100:topic:42]"),
            None
        );
    }

    #[tokio::test]
    async fn other_channels_are_not_a_forum() {
        assert_eq!(chat_of_base_title("Discord: #general [chat:99]"), None);
        assert_eq!(chat_of_base_title("a local conversation"), None);
    }

    #[test]
    fn the_topic_title_carries_the_thread_key() {
        let title = forum_topic_title("Memes", -100, 42, "Funny");
        assert!(
            title.ends_with("[chat:-100:topic:42]"),
            "unexpected title: {title}"
        );
        assert_eq!(title, "Telegram: Memes / Funny [chat:-100:topic:42]");
    }

    #[tokio::test]
    async fn the_row_is_keyed_and_hangs_under_the_parent() {
        let db = crate::db::Database::connect_in_memory().await.unwrap();
        db.run_migrations().await.unwrap();
        let pool = db.pool().clone();
        let svc = SessionService::new(ServiceContext::new(pool.clone()));
        let repo = SessionRepository::new(pool.clone());

        let parent = Uuid::new_v4();
        seed_parent(&pool, parent, "Telegram: Memes [chat:-100]").await;

        let created = write_forum_topic_row(&svc, &repo, &pool, parent, -100, "Memes", 42, "Funny")
            .await
            .expect("the row should be written");

        assert!(created.title.ends_with("[chat:-100:topic:42]"));
        assert_eq!(
            repo.find_parent_id(created.session).await.unwrap(),
            Some(parent),
            "the new topic must hang under its forum"
        );
        let row = svc.get_session(created.session).await.unwrap().unwrap();
        assert_eq!(
            row.channel_chat_key.as_deref(),
            Some("[chat:-100:topic:42]")
        );
        let found = svc
            .find_session_by_channel_chat_key("[chat:-100:topic:42]")
            .await
            .unwrap();
        assert_eq!(
            found.map(|s| s.id),
            Some(created.session),
            "the inbound resolver has to find the row the window made"
        );
    }

    #[tokio::test]
    async fn a_second_write_reuses_the_same_row() {
        let db = crate::db::Database::connect_in_memory().await.unwrap();
        db.run_migrations().await.unwrap();
        let pool = db.pool().clone();
        let svc = SessionService::new(ServiceContext::new(pool.clone()));
        let repo = SessionRepository::new(pool.clone());

        let parent = Uuid::new_v4();
        seed_parent(&pool, parent, "Telegram: Memes [chat:-100]").await;

        let first = write_forum_topic_row(&svc, &repo, &pool, parent, -100, "Memes", 42, "Funny")
            .await
            .unwrap();
        let second = write_forum_topic_row(&svc, &repo, &pool, parent, -100, "Memes", 42, "Funny")
            .await
            .unwrap();
        assert_eq!(first.session, second.session);
    }

    #[tokio::test]
    async fn a_chat_with_no_topic_rows_is_not_a_forum() {
        let db = crate::db::Database::connect_in_memory().await.unwrap();
        db.run_migrations().await.unwrap();
        let pool = db.pool().clone();
        let repo = SessionRepository::new(pool.clone());
        assert_eq!(repo.count_topics_for_chat(-100).await.unwrap(), 0);
    }
}
