//! The tool-group bubble slot for a live turn (#2192).
//!
//! A turn posts one tool-group bubble and edits it in place. The message id
//! is born on the first tool event, and every later event appends to it. The
//! birth runs through `governor::say`, which can SLEEP for the channel park
//! window (`Decision::Hold`, up to `pause_ceiling`). Before this module the
//! id lived in a single `Mutex<Option<MessageId>>` that the birth held across
//! that sleep, so the settle path (which takes the same lock to stamp the
//! finished bubble) waited on the park: a turn that had already produced its
//! answer could not deliver until the governor woke up.
//!
//! The slot splits the two concerns:
//!
//! * [`BubbleSlot::read`] and [`BubbleSlot::store`] take the id lock for a
//!   pointer-sized read or write and nothing else. A settle never waits for a
//!   birth.
//! * [`BubbleSlot::birth_claim`] serialises births only. Two tool events that
//!   race before the bubble exists must not post two bubbles, so the loser
//!   waits for the winner and then finds the id through `read`.
//!
//! The id itself stays reachable as a plain `Arc<Mutex<Option<MessageId>>>`
//! through [`BubbleSlot::mid_handle`], so the flow ticker, the settle paths
//! and the resume plumbing keep their existing shape.

use std::sync::Arc;

use serenity::model::id::MessageId;
use tokio::sync::Mutex;

use super::DiscordState;
use super::tool_group::{GroupEntry, GroupState};

/// The tool-group bubble for one turn: its message id plus a birth claim.
pub(crate) struct BubbleSlot {
    mid: Arc<Mutex<Option<MessageId>>>,
    birth: Mutex<()>,
}

impl BubbleSlot {
    pub(crate) fn new() -> Self {
        Self {
            mid: Arc::new(Mutex::new(None)),
            birth: Mutex::new(()),
        }
    }

    /// The id as a plain lock handle, for the readers that only need to look
    /// at it (flow ticker, settle stamp, resume). Taking this lock never
    /// waits for a birth: births do not hold it across the send.
    pub(crate) fn mid_handle(&self) -> Arc<Mutex<Option<MessageId>>> {
        self.mid.clone()
    }

    /// The current id, without waiting on an in-flight birth.
    pub(crate) async fn read(&self) -> Option<MessageId> {
        *self.mid.lock().await
    }

    pub(crate) async fn store(&self, mid: MessageId) {
        *self.mid.lock().await = Some(mid);
    }

    /// Claim the right to birth the bubble. Held across the post so two tool
    /// events cannot create two bubbles; released as soon as the id is
    /// stored. The id lock is NOT held during the post (#2192).
    pub(crate) async fn birth_claim(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.birth.lock().await
    }
}

/// Append tool entries to an existing bubble and repaint it.
pub(crate) async fn append_tool_entry(
    dstate: &DiscordState,
    channel: serenity::model::id::ChannelId,
    http: &serenity::http::Http,
    mid: MessageId,
    entries: Vec<GroupEntry>,
    label: &str,
) {
    let group = dstate
        .upsert_tool_group(
            mid.get(),
            GroupState {
                page: 0,
                entries,
                notes: Vec::new(),
                expanded: false,
                started_at: std::time::Instant::now(),
                settled: None,
            },
        )
        .await;
    let edit = serenity::builder::EditMessage::new()
        .content(super::tool_group::render_content(&group))
        .embeds(super::tool_group::render_embeds(&group))
        .components(super::tool_group::render_components(&group, mid.get()));
    if let Some(Err(e)) =
        super::governor::edit_chrome(&dstate.governor, channel, http, mid, edit).await
    {
        tracing::warn!("Discord: {label} tool group edit failed (append): {e}");
    }
}
