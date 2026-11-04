//! The shared [`DiscordState`] struct: fields, `new()` and `Default`.
//!
//! Every field is `pub(super)` so the per-concern impl modules beside this
//! file (`approval`, `cancel`, `connection`, `pending_interactions`,
//! `sessions`, `tool_group`) can reach them without widening the
//! crate-visible surface. Behaviour lives there, not here.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{governor::Governor, interactions, tool_group};

/// Shared Discord state for proactive messaging.
///
/// Set when the bot connects via the `ready` event.
/// Read by the `discord_send` tool to send messages on demand.
pub struct DiscordState {
    pub(super) http: Mutex<Option<Arc<serenity::http::Http>>>,
    /// Channel ID of the owner's last message — used as default for proactive sends
    pub(super) owner_channel_id: Mutex<Option<u64>>,
    /// Bot's own user ID — set on ready, used for @mention detection
    pub(super) bot_user_id: Mutex<Option<u64>>,
    /// Guild ID of the last guild message — needed for guild-scoped actions
    pub(super) guild_id: Mutex<Option<u64>>,
    /// Comparison key of the application command set currently registered with
    /// Discord (the command set plus guild membership), which also tells us
    /// whether the watcher that keeps it in sync has been started: `ready`
    /// fires on every reconnect and the watcher must not be started twice.
    /// `None` until a sync succeeds, and set back to `None` when every guild
    /// refused, so a failed attempt is retried rather than remembered as done.
    /// See `commands::sync_commands`.
    pub(super) commands_sig: Mutex<Option<u64>>,
    /// Whether the config watcher that keeps [`Self::commands_sig`] in sync has
    /// been started. Separate from the key itself because a failed sync leaves
    /// the key `None`, and `ready` would otherwise spawn a new watcher on every
    /// reconnect.
    pub(super) commands_watcher_started: Mutex<bool>,
    /// Maps session_id → channel_id for approval routing
    pub(super) session_channels: Mutex<HashMap<Uuid, u64>>,
    /// Reverse ownership map (#148): channel_id → session_id, written in
    /// lockstep with `session_channels` at `register_session_channel` (the
    /// ONLY write site for both). Last writer wins — mirrors the forward map.
    pub(super) channel_sessions: Mutex<HashMap<u64, Uuid>>,
    /// Pending approval channels: approval_id → oneshot sender of (approved, always)
    pub(super) pending_approvals: Mutex<HashMap<String, oneshot::Sender<(bool, bool)>>>,
    /// Per-session cancel tokens for aborting in-flight agent tasks via /stop
    pub(super) cancel_tokens: Mutex<HashMap<Uuid, CancellationToken>>,
    /// Pending select menus: id -> (created, options) (#382). Lazy TTL:
    /// stale picks answer "expired" (#386). Bounded at
    /// [`pending_interactions::PENDING_INTERACTION_CAP`] by a prune on insert
    /// (#2193): the TTL only runs for an id someone clicks, so it never
    /// bounded an unclicked map.
    pub(super) pending_selects: Mutex<HashMap<String, (std::time::Instant, Vec<String>)>>,
    /// Pending modal forms: id -> (created, spec) (#383). Same lazy TTL and the
    /// same cap as the menus.
    pub(super) pending_forms: Mutex<HashMap<String, (std::time::Instant, interactions::FormSpec)>>,
    /// Collapsible tool groups keyed by message id, so the Expand/Collapse
    /// interaction can re-render after the turn ended. Insertion-ordered
    /// for pruning; bounded at [`Self::TOOL_GROUP_CAP`] (see `tool_group`).
    pub(super) tool_groups: Mutex<(Vec<u64>, HashMap<u64, tool_group::GroupState>)>,
    /// Sessions whose turn settled with background work still alive (#1987):
    /// session → tool-group message id, so the background-completion path in
    /// `resume.rs` can find the waiting group after the turn's closures are
    /// gone and flip it once both registries drain.
    pub(super) waiting_groups: Mutex<HashMap<Uuid, u64>>,
    /// Sessions with a live turn for this process's lifetime (#1990): an
    /// inbound message arriving while the slot is held queues as a
    /// follow-up instead of forking a second concurrent tool loop. The
    /// Telegram twin (#501); std Mutex, not tokio's, because the claim must
    /// be atomic and no await may run under the lock, and because the
    /// guard's Drop fires synchronously on any unwind.
    pub(super) active_turns: std::sync::Mutex<std::collections::HashSet<Uuid>>,
    /// Per-session follow-ups queued while the slot is held (#1990).
    /// Consumed between tool rounds by the queue callback wired in
    /// manager.rs, and flushed into a fresh tracked turn by whoever held
    /// the slot last.
    pub(super) pending_followups: std::sync::Mutex<
        HashMap<Uuid, std::collections::VecDeque<crate::brain::agent::QueuedUserMessage>>,
    >,
    /// The session's live plan card: session -> (message id, signature),
    /// mirrored from the `plan_cards` table so the dedupe check is a memory hit
    /// (#1912). Telegram's twin; rehydrated on `ready` because losing it would
    /// make the first refresh after a restart post a second card and strand the
    /// first.
    pub(super) plan_cards: Mutex<HashMap<Uuid, (u64, u64, String)>>,
    /// Per-session card mutexes (#822). Held across the render, the dedupe
    /// check and the edit, so two refreshes for one session cannot both see
    /// "no card" and post two.
    pub(super) plan_card_locks: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
    /// Outbound write governor (#1910): the one place that decides whether a
    /// channel can afford another message or edit right now. Held on the
    /// shared state because every writer reaches it, and because a 429 seen by
    /// the flow ticker has to park the settle edit too, not just the ticker.
    pub(super) governor: Governor,
}

impl Default for DiscordState {
    fn default() -> Self {
        Self::new()
    }
}

impl DiscordState {
    /// The outbound write governor (#1910), exposed crate-wide so a tool that
    /// sends on its own (the `discord_send` tool) still pays the budget and
    /// records the message (#2196).
    pub(crate) fn governor(&self) -> &Governor {
        &self.governor
    }

    pub fn new() -> Self {
        Self {
            http: Mutex::new(None),
            owner_channel_id: Mutex::new(None),
            bot_user_id: Mutex::new(None),
            guild_id: Mutex::new(None),
            commands_sig: Mutex::new(None),
            commands_watcher_started: Mutex::new(false),
            session_channels: Mutex::new(HashMap::new()),
            channel_sessions: Mutex::new(HashMap::new()),
            pending_approvals: Mutex::new(HashMap::new()),
            cancel_tokens: Mutex::new(HashMap::new()),
            pending_selects: Mutex::new(HashMap::new()),
            pending_forms: Mutex::new(HashMap::new()),
            tool_groups: Mutex::new((Vec::new(), HashMap::new())),
            waiting_groups: Mutex::new(HashMap::new()),
            active_turns: std::sync::Mutex::new(std::collections::HashSet::new()),
            pending_followups: std::sync::Mutex::new(HashMap::new()),
            plan_cards: Mutex::new(HashMap::new()),
            plan_card_locks: Mutex::new(HashMap::new()),
            governor: Governor::default(),
        }
    }
}
