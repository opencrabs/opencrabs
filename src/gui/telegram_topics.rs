//! The window's half of "New topic on a Telegram forum" (#2603).
//!
//! Why this exists at all: `sessions::create_topic` writes a local row and
//! nothing else. On a Telegram forum that row is invisible to the bridge, so
//! the topic the user named never reaches the chat. This module routes the
//! window's create through the library's forum-topic path, which calls the Bot
//! API first and keys the row on the thread id Telegram returns.
//!
//! Every failure has to reach the user: a create that quietly leaves a local
//! row behind, or that does nothing at all, is the bug being fixed.

use std::path::PathBuf;
use uuid::Uuid;

/// Create a forum topic when `parent` is a Telegram forum.
///
/// `Ok(Some(id))` is the new topic row. `Ok(None)` means this parent is not a
/// Telegram forum (a local conversation, a DM, a plain group), so the caller
/// keeps its ordinary local topic behaviour. `Err` means it IS a forum and the
/// create failed: show it, never fall back to a local row.
///
/// The work runs on its own thread with its own runtime for the same reason
/// the rest of `sessions` does: the Bot API call is network-bound and the
/// window must not wait on it inside a draw.
#[cfg(feature = "telegram")]
pub async fn create_forum_topic_if_forum(
    db_path: PathBuf,
    parent: Uuid,
    name: String,
) -> Result<Option<Uuid>, String> {
    let (tx, rx) = futures::channel::oneshot::channel::<Result<Option<Uuid>, String>>();
    let spawned = std::thread::Builder::new()
        .name("forum-topic-create".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let result = match runtime {
                Ok(rt) => rt.block_on(async move {
                    let db = match opencrabs::db::Database::connect(&db_path).await {
                        Ok(db) => db,
                        Err(e) => return Err(format!("{e:#}")),
                    };
                    opencrabs::channels::telegram::topic_create::create_forum_topic_session(
                        db.pool(),
                        parent,
                        &name,
                    )
                    .await
                    .map(|created| created.map(|topic| topic.session))
                }),
                Err(e) => Err(format!("tokio runtime failed: {e}")),
            };
            let _ = tx.send(result);
        });
    match spawned {
        Ok(_) => match rx.await {
            Ok(result) => result,
            Err(_) => Err("forum topic create thread dropped its result".into()),
        },
        Err(e) => Err(format!("forum topic create thread failed to spawn: {e}")),
    }
}

/// A build without the Telegram channel has no forum to create a topic in, so
/// the window keeps its local behaviour. Kept as a function with the same
/// shape rather than a `cfg` at every call site.
#[cfg(not(feature = "telegram"))]
pub async fn create_forum_topic_if_forum(
    _db_path: PathBuf,
    _parent: Uuid,
    _name: String,
) -> Result<Option<Uuid>, String> {
    Ok(None)
}
