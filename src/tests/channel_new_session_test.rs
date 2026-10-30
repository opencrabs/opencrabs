//! `/new` must hand the chat key to the row it creates (#2186).
//!
//! The pre-fix Discord handler built the session title WITHOUT the stable
//! `[chat:discord-…]` suffix, so `create_channel_session` took the plain
//! insert path, stored `channel_chat_key: NULL`, and the next typed message
//! resolved straight back to the previous session. The new row was an orphan
//! nobody ever wrote to, and `/new` looked like a no-op.
//!
//! These tests drive the shared helper the handler now calls and then ask the
//! per-message resolver the only question that matters: which session does the
//! NEXT message land on?

use crate::channels::session_init::start_new_channel_session;
use crate::channels::session_resolve::{chat_id_suffix, resolve_or_create_channel_session};
use crate::db::Database;
use crate::services::{ServiceContext, SessionService};

async fn fresh_session_service() -> SessionService {
    let db = Database::connect_in_memory()
        .await
        .expect("in-memory db connect");
    db.run_migrations().await.expect("run migrations");
    SessionService::new(ServiceContext::new(db.pool().clone()))
}

/// Seed the chat's existing session the way the per-message resolver would.
async fn seed_existing(
    svc: &SessionService,
    chat_key: &str,
    legacy_title: &str,
    working_directory: Option<&str>,
) -> crate::db::models::Session {
    let title = format!("{legacy_title} {chat_key}");
    svc.insert_or_resolve_channel_session(
        Some(title),
        None,
        None,
        working_directory.map(str::to_string),
        chat_key,
    )
    .await
    .expect("seed existing channel session")
}

/// The next message must land on the session `/new` just created.
#[tokio::test]
async fn guest_new_session_takes_the_chat_key_and_the_next_message_lands_on_it() {
    let svc = fresh_session_service().await;
    let chat_key = chat_id_suffix("discord-424242");
    let legacy = "Discord: #424242";
    let old = seed_existing(&svc, &chat_key, legacy, None).await;

    let new = start_new_channel_session(&svc, &chat_key, legacy, false, "Discord")
        .await
        .expect("start new session");

    assert_ne!(new.id, old.id, "/new must create a distinct row");
    assert_eq!(
        new.channel_chat_key.as_deref(),
        Some(chat_key.as_str()),
        "the new row must claim the chat key, not be an orphan"
    );

    let archived = svc
        .get_session(old.id)
        .await
        .expect("get old")
        .expect("old row exists");
    assert!(
        archived.archived_at.is_some(),
        "a guest's previous session is archived so the key is free"
    );

    let next = resolve_or_create_channel_session(
        &svc,
        &chat_key,
        legacy,
        &format!("{legacy} {chat_key}"),
        None,
        "Discord",
    )
    .await
    .expect("resolve next message");
    assert_eq!(
        next, new.id,
        "the message after /new must land on the new session"
    );
}

/// The owner's previous session stays visible in `/sessions`, so it cannot be
/// archived. It releases the key instead, and `/new` still takes effect.
#[tokio::test]
async fn owner_new_session_releases_the_key_and_the_next_message_still_lands_on_it() {
    let svc = fresh_session_service().await;
    let chat_key = chat_id_suffix("discord-dm-777");
    let legacy = "Discord: DM owner (777)";
    let old = seed_existing(&svc, &chat_key, legacy, None).await;

    let new = start_new_channel_session(&svc, &chat_key, legacy, true, "Discord")
        .await
        .expect("start new session");

    assert_ne!(
        new.id, old.id,
        "/new must create a distinct row for the owner"
    );
    assert_eq!(new.channel_chat_key.as_deref(), Some(chat_key.as_str()));

    let kept = svc
        .get_session(old.id)
        .await
        .expect("get old")
        .expect("old row exists");
    assert!(
        kept.archived_at.is_none(),
        "the owner's previous session stays live so /sessions keeps showing it"
    );
    assert!(
        kept.channel_chat_key.is_none(),
        "it must give up the chat key, or the keyed insert resolves it back and /new does nothing"
    );

    let next = resolve_or_create_channel_session(
        &svc,
        &chat_key,
        legacy,
        &format!("{legacy} {chat_key}"),
        None,
        "Discord",
    )
    .await
    .expect("resolve next message");
    assert_eq!(next, new.id);
}

/// #263: the new session inherits the working directory of the session that
/// received `/new`. The pre-fix exact-title lookup missed every suffixed row,
/// so this never fired on any channel.
#[tokio::test]
async fn new_session_inherits_working_directory_from_the_prior_session() {
    let svc = fresh_session_service().await;
    let chat_key = chat_id_suffix("discord-999");
    let legacy = "Discord: #999";
    let old = seed_existing(&svc, &chat_key, legacy, Some("/srv/work/opencrabs")).await;

    let new = start_new_channel_session(&svc, &chat_key, legacy, false, "Discord")
        .await
        .expect("start new session");

    assert_eq!(
        new.working_directory.as_deref(),
        Some("/srv/work/opencrabs"),
        "the wd comes from the session that received /new (#263)"
    );
    assert_ne!(new.id, old.id);
}

/// A brand-new chat has no prior session: `/new` still creates a keyed row
/// instead of failing or producing an orphan.
#[tokio::test]
async fn new_session_in_a_fresh_chat_is_keyed() {
    let svc = fresh_session_service().await;
    let chat_key = chat_id_suffix("discord-1000");
    let legacy = "Discord: #1000";

    let new = start_new_channel_session(&svc, &chat_key, legacy, false, "Discord")
        .await
        .expect("start new session in a fresh chat");

    assert_eq!(new.channel_chat_key.as_deref(), Some(chat_key.as_str()));
    assert!(new.working_directory.is_none());

    let next = resolve_or_create_channel_session(
        &svc,
        &chat_key,
        legacy,
        &format!("{legacy} {chat_key}"),
        None,
        "Discord",
    )
    .await
    .expect("resolve next message");
    assert_eq!(next, new.id);
}

/// Pressing `/new` twice must not leave two live rows claiming one chat.
#[tokio::test]
async fn repeated_new_sessions_leave_one_live_claimant() {
    let svc = fresh_session_service().await;
    let chat_key = chat_id_suffix("discord-1001");
    let legacy = "Discord: #1001";

    let first = start_new_channel_session(&svc, &chat_key, legacy, true, "Discord")
        .await
        .expect("first /new");
    let second = start_new_channel_session(&svc, &chat_key, legacy, true, "Discord")
        .await
        .expect("second /new");

    assert_ne!(first.id, second.id);
    let claimant = svc
        .find_session_by_channel_chat_key(&chat_key)
        .await
        .expect("lookup by key")
        .expect("one live claimant");
    assert_eq!(claimant.id, second.id, "the newest /new owns the chat");
}
