//! Hang a forum-topic session under its forum parent (#2603).
//!
//! Thread isolation has been correct since #215: every forum topic resolves to
//! its own row keyed on `[chat:<id>:topic:<tid>]`. What never happened is the
//! link back to the group, so 59 Telegram topic rows sit at the top of the
//! rail beside the group they belong to, and no forum ever reports
//! `topics_enabled = 1`.
//!
//! Two facts make the link necessary rather than cosmetic:
//!
//! 1. `attach_topic` is what sets `topics_enabled = 1` on the parent. Without
//!    it the GUI refuses every topic action on a forum (`start_topic_draft`
//!    bails when topics are off), which is the "New topic does nothing" bug.
//! 2. The rails (`src/ui_core/topic_bar.rs`, the TUI card list) fold by
//!    `parent_id` alone. A topic with `parent_id = NULL` is top-level by
//!    definition, however its title is spelled.
//!
//! The parent is the base `[chat:<id>]` row. It often does not exist yet:
//! `normalize_topic` promotes a known forum's General topic to
//! `Some(GENERAL_TOPIC_ID)`, so a forum's messages key on
//! `[chat:<id>:topic:1]` and the bare key is never created. In that case the
//! parent is created here as a group container that carries no messages of
//! its own. That is deliberate: one parent shape for every forum, General
//! becomes a normal chip beside the rest.

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::db::repository::SessionRepository;
use crate::services::SessionService;

use super::session_resolve;

/// What one link attempt did, for the caller's log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkedTopic {
    /// The forum parent the topic now hangs under.
    pub parent: Uuid,
    /// True when this call had to create the parent row (no `[chat:<id>]`
    /// session existed: the form every forum with topics takes today).
    pub parent_created: bool,
    /// True when the child already hung under this parent, so nothing was
    /// written. The common case on every message after the first.
    pub already_linked: bool,
}

/// Ensure the forum parent of `child` exists, then attach `child` to it.
///
/// `child` is the topic session resolved for an incoming message;
/// `parent_title` is the base-session title (no `:topic:` suffix), already
/// built by [`session_resolve::build_session_title`]. Depth stays 1: the
/// parent is always the base chat row, never another topic.
///
/// Idempotent: a second message in the same topic re-attaches to the same
/// parent, which `attach_topic` accepts without a write.
pub async fn link_topic_session(
    session_svc: &SessionService,
    chat_id: i64,
    child: Uuid,
    parent_title: String,
) -> Result<LinkedTopic> {
    let base_suffix = session_resolve::chat_id_suffix(chat_id, None);
    let repo = SessionRepository::new(session_svc.pool());

    // Already a topic of somebody: every message after the first in a forum
    // lands here, and the link is a read, not a write. `topics_enabled` is
    // left to the attach that created the link (and to the backfill), so the
    // hot path stays a single SELECT.
    if let Some(parent) = repo.find_parent_id(child).await? {
        return Ok(LinkedTopic {
            parent,
            parent_created: false,
            already_linked: true,
        });
    }

    // The chat key is the identity; the title suffix is the fallback for rows
    // created before #1721 stamped keys.
    let mut parent = match session_svc
        .find_session_by_channel_chat_key(&base_suffix)
        .await?
    {
        Some(found) => Some(found),
        None => {
            session_svc
                .find_session_by_title_suffix(&base_suffix)
                .await?
        }
    };

    let mut parent_created = false;
    if parent.is_none() {
        // Inherit provider/model/working directory from the topic that
        // triggered this, so the group does not open with defaults while its
        // own topic is already pinned to the user's active provider.
        let inherit = session_svc.get_session(child).await.ok().flatten();
        let created = crate::channels::session_init::create_channel_session(
            session_svc,
            Some(parent_title),
            inherit.as_ref(),
        )
        .await
        .context("Failed to create the forum parent session")?;
        tracing::info!(
            "Telegram: created forum parent session {} for {}",
            created.id,
            base_suffix
        );
        parent = Some(created);
        parent_created = true;
    }

    let parent = parent.expect("parent is set by the branch above");
    if parent.id == child {
        // Defensive: a base session can never also be its own topic. The rows
        // carry different title suffixes, so reaching this is a bug upstream.
        anyhow::bail!("refusing to attach session {child} to itself");
    }

    match repo.attach_topic(child, parent.id).await {
        Ok(()) => {
            tracing::info!(
                "Telegram: forum topic session {child} linked under parent {} ({}created)",
                parent.id,
                if parent_created { "" } else { "not " }
            );
            Ok(LinkedTopic {
                parent: parent.id,
                parent_created,
                already_linked: false,
            })
        }
        Err(crate::db::repository::AttachTopicError::AlreadyAttached) => {
            // A different parent already owns this topic: the pre-check above
            // lost a race with another process on the same database. Keep that
            // link instead of rewriting it and say so once.
            tracing::warn!(
                "Telegram: topic session {child} was linked by another parent before \
                 {pid}; keeping the existing link",
                pid = parent.id
            );
            Ok(LinkedTopic {
                parent: parent.id,
                parent_created,
                already_linked: true,
            })
        }
        Err(e) => Err(anyhow::Error::new(e))
            .with_context(|| format!("Failed to attach topic session {child} to {}", parent.id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::services::ServiceContext;

    async fn db() -> Database {
        let db = Database::connect_in_memory().await.expect("in-memory db");
        db.run_migrations().await.expect("migrations");
        db
    }

    fn svc(db: &Database) -> SessionService {
        SessionService::new(ServiceContext::new(db.pool().clone()))
    }

    async fn session(svc: &SessionService, title: &str) -> Uuid {
        svc.create_session(Some(title.to_string()))
            .await
            .expect("create session")
            .id
    }

    async fn parent_of(db: &Database, id: Uuid) -> Option<String> {
        use rusqlite::OptionalExtension as _;
        use rusqlite::params;

        let id_str = id.to_string();
        db.pool()
            .get()
            .await
            .unwrap()
            .interact(move |conn| {
                conn.query_row(
                    "SELECT parent_id FROM sessions WHERE id = ?1",
                    params![id_str],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()
            })
            .await
            .unwrap()
            .unwrap()
            .flatten()
    }

    async fn topics_enabled(db: &Database, id: Uuid) -> i64 {
        let id_str = id.to_string();
        db.pool()
            .get()
            .await
            .unwrap()
            .interact(move |conn| {
                conn.query_row(
                    "SELECT topics_enabled FROM sessions WHERE id = ?1",
                    [id_str],
                    |r| r.get::<_, i64>(0),
                )
            })
            .await
            .unwrap()
            .unwrap()
    }

    /// A forum's first topic message creates the parent and links the topic
    /// under it, turning topics on for the parent.
    #[tokio::test]
    async fn first_topic_message_creates_and_links_the_parent() {
        let db = db().await;
        let svc = svc(&db);
        let chat_id = -1001234567890_i64;
        let topic_title = format!(
            "Telegram: CrabsDev / Memes {}",
            session_resolve::chat_id_suffix(chat_id, Some(42))
        );
        let child = session(&svc, &topic_title).await;
        let parent_title = format!(
            "Telegram: CrabsDev {}",
            session_resolve::chat_id_suffix(chat_id, None)
        );

        let linked = link_topic_session(&svc, chat_id, child, parent_title.clone())
            .await
            .expect("link");

        assert!(linked.parent_created, "no base row existed yet");
        assert_ne!(linked.parent, child);
        assert_eq!(
            parent_of(&db, child).await,
            Some(linked.parent.to_string()),
            "the topic hangs under the forum parent"
        );
        assert_eq!(
            topics_enabled(&db, linked.parent).await,
            1,
            "the parent reports topics on"
        );
        let parent = svc
            .get_session(linked.parent)
            .await
            .expect("get parent")
            .expect("parent exists");
        assert_eq!(parent.title.as_deref(), Some(parent_title.as_str()));
    }

    /// A second message in the same topic reuses the parent instead of
    /// creating another one.
    #[tokio::test]
    async fn second_message_reuses_the_same_parent() {
        let db = db().await;
        let svc = svc(&db);
        let chat_id = -1009876543210_i64;
        let child = session(
            &svc,
            &format!(
                "Telegram: Group / Dev {}",
                session_resolve::chat_id_suffix(chat_id, Some(7))
            ),
        )
        .await;
        let parent_title = format!(
            "Telegram: Group {}",
            session_resolve::chat_id_suffix(chat_id, None)
        );

        let first = link_topic_session(&svc, chat_id, child, parent_title.clone())
            .await
            .expect("first link");
        let second = link_topic_session(&svc, chat_id, child, parent_title)
            .await
            .expect("second link");

        assert_eq!(first.parent, second.parent);
        assert!(first.parent_created);
        assert!(!second.parent_created, "the parent was already there");
        assert!(
            second.already_linked,
            "the second message finds the link and writes nothing"
        );
    }

    /// A standalone base session (DM-style or a pre-topics group) is reused as
    /// the parent rather than duplicated.
    #[tokio::test]
    async fn existing_base_session_is_reused() {
        let db = db().await;
        let svc = svc(&db);
        let chat_id = -1005550001111_i64;
        let base_suffix = session_resolve::chat_id_suffix(chat_id, None);
        let base = session(&svc, &format!("Telegram: Old Group {base_suffix}")).await;
        let child = session(
            &svc,
            &format!(
                "Telegram: Old Group / General {}",
                session_resolve::chat_id_suffix(chat_id, Some(1))
            ),
        )
        .await;

        let linked = link_topic_session(
            &svc,
            chat_id,
            child,
            format!("Telegram: Old Group {base_suffix}"),
        )
        .await
        .expect("link");

        assert!(!linked.parent_created);
        assert_eq!(linked.parent, base);
        assert_eq!(parent_of(&db, child).await, Some(base.to_string()));
        assert_eq!(topics_enabled(&db, base).await, 1);
    }

    /// Two topics under one forum share the parent, so the rails render one
    /// group card with two chips.
    #[tokio::test]
    async fn sibling_topics_share_one_parent() {
        let db = db().await;
        let svc = svc(&db);
        let chat_id = -1007770002222_i64;
        let parent_title = format!(
            "Telegram: Siblings {}",
            session_resolve::chat_id_suffix(chat_id, None)
        );
        let a = session(
            &svc,
            &format!(
                "Telegram: Siblings / A {}",
                session_resolve::chat_id_suffix(chat_id, Some(3))
            ),
        )
        .await;
        let b = session(
            &svc,
            &format!(
                "Telegram: Siblings / B {}",
                session_resolve::chat_id_suffix(chat_id, Some(4))
            ),
        )
        .await;

        let la = link_topic_session(&svc, chat_id, a, parent_title.clone())
            .await
            .expect("link a");
        let lb = link_topic_session(&svc, chat_id, b, parent_title)
            .await
            .expect("link b");

        assert_eq!(la.parent, lb.parent);
        assert!(la.parent_created);
        assert!(!lb.parent_created);
        assert_eq!(parent_of(&db, a).await, parent_of(&db, b).await);
    }
}
