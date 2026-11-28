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

use uuid::Uuid;

/// Create a forum topic when `parent` is a Telegram forum.
///
/// `Ok(Some(id))` is the new topic row. `Ok(None)` means this parent is not a
/// Telegram forum (a local conversation, a DM, a plain group), so the caller
/// keeps its ordinary local topic behaviour. `Err` means it IS a forum and the
/// create failed: show it, never fall back to a local row.
///
/// The work runs on the backend's worker lane (`CreateForumTopic`): the Bot
/// API call is network-bound and the window must not wait on it inside a draw.
#[cfg(feature = "telegram")]
pub async fn create_forum_topic_if_forum(
    backend: crate::backend_glue::Backend,
    parent: Uuid,
    name: String,
) -> Result<Option<Uuid>, String> {
    backend
        .call(opencrabs::backend::telegram_topics::CreateForumTopic { parent, name })
        .await
        .map_err(opencrabs::backend::BackendError::into_message)
}

/// A build without the Telegram channel has no forum to create a topic in, so
/// the window keeps its local behaviour. Kept as a function with the same
/// shape rather than a `cfg` at every call site.
#[cfg(not(feature = "telegram"))]
pub async fn create_forum_topic_if_forum(
    _backend: crate::backend_glue::Backend,
    _parent: Uuid,
    _name: String,
) -> Result<Option<Uuid>, String> {
    Ok(None)
}
