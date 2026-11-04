//! The shared [`WhatsAppState`] struct: fields, `new()` and `Default`.
//!
//! Every field is `pub(super)` so the per-concern impl modules beside this
//! file (`approval`, `cancel`, `connection`, `followups`,
//! `onboarding_events`, `pairing`, `photos`, `sessions`) can reach them
//! without widening the crate-visible surface. Behaviour lives there, not
//! here.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use whatsapp_rust::client::Client;

use super::WaApproval;

/// Shared WhatsApp client state for proactive messaging.
///
/// Set when the bot connects (either via static agent or whatsapp_connect tool).
/// Read by the `whatsapp_send` tool to send messages on demand.
pub struct WhatsAppState {
    pub(super) client: Mutex<Option<Arc<Client>>>,
    /// Owner's JID (phone@s.whatsapp.net) — first in allowed_phones list
    pub(super) owner_jid: Mutex<Option<String>>,
    /// Pending tool approvals: phone → oneshot sender of WaApproval.
    /// When a tool approval is in flight, the next message from that phone
    /// (text or button tap) is interpreted as Yes/Always/No instead of
    /// being routed to the agent.
    pub pending_approvals: Mutex<HashMap<String, tokio::sync::oneshot::Sender<WaApproval>>>,
    /// Pending follow-up questions keyed by phone: oneshot sender for
    /// the chosen option string plus the option list. WhatsApp's
    /// ButtonsMessage is deprecated, so we render the question as a
    /// numbered text list and parse the user's next numeric reply.
    /// Per-session OPTIONAL follow-up suggestions from `suggest_options`
    /// (#600). WhatsApp has no working button UI, so these render as a numbered
    /// text list; a bare numeric reply selects the matching suggestion. Keyed by
    /// session; consumed on a valid numeric reply, cleared on any other message.
    pub(super) pending_followups: Mutex<HashMap<Uuid, Vec<String>>>,
    /// Per-session cancel tokens for aborting in-flight agent tasks via /stop
    pub(super) cancel_tokens: Mutex<HashMap<Uuid, CancellationToken>>,
    /// Sessions with a turn in flight (#2179). Telegram's `active_turns`
    /// twin (#501), Discord's by #1990: an inbound message arriving while
    /// the slot is held queues as a follow-up instead of forking a second
    /// concurrent tool loop. std Mutex, not tokio's, because the claim must
    /// be atomic and no await may run under the lock, and because the
    /// guard's Drop fires synchronously on any unwind.
    pub(super) active_turns: std::sync::Mutex<std::collections::HashSet<Uuid>>,
    /// Per-session follow-ups queued while the slot is held (#2179).
    /// Consumed between tool rounds by the queue callback wired in
    /// manager.rs, and flushed into a fresh turn by whoever held the slot
    /// last. Distinct from `pending_followups` above, which holds the
    /// `suggest_options` labels a numeric reply selects.
    pub(super) turn_queue: std::sync::Mutex<
        HashMap<Uuid, std::collections::VecDeque<crate::brain::agent::QueuedUserMessage>>,
    >,
    /// Broadcast channel for QR codes — onboarding subscribes to this.
    pub(super) qr_tx: tokio::sync::broadcast::Sender<String>,
    /// Broadcast channel for connection events — onboarding subscribes to this.
    pub(super) connected_tx: tokio::sync::broadcast::Sender<()>,
    /// Broadcast channel for error events — onboarding subscribes to this.
    pub(super) error_tx: tokio::sync::broadcast::Sender<String>,
    /// Broadcast channel for delivered message ids (from `ReceiptType::Delivered`
    /// receipts). The onboarding connection test waits on this so it confirms
    /// only when a message actually reached WhatsApp — not merely when the send
    /// stanza was transmitted (which still returns Ok even if the server later
    /// rejects it with error 400).
    pub(super) delivered_tx: tokio::sync::broadcast::Sender<String>,
    /// Last QR code broadcast. The QR channel is a plain broadcast with no
    /// replay, so a connect flow that subscribes AFTER the agent already
    /// emitted its QR would see nothing until the next ~20s refresh (the
    /// "press Enter twice" bug). New subscribers replay this immediately.
    pub(super) last_qr: std::sync::Mutex<Option<String>>,
    /// Set by the onboarding connect/reset flow to force a fresh pairing.
    /// `reconcile_whatsapp` aborts the live agent and starts a new one against
    /// the wiped `session.db`, so old auth is dropped at RUNTIME (not only on
    /// disk) and the agent re-pairs with a fresh QR.
    pub(super) restart_requested: std::sync::atomic::AtomicBool,
    /// True once pairing/connection succeeds. Locks the QR: once connected, a
    /// late or stale `broadcast_qr` is suppressed so the onboarding UI never
    /// flashes a QR after the account is already linked. Reset by
    /// `request_restart` when a fresh pairing is requested.
    pub(super) connected: std::sync::atomic::AtomicBool,
    /// Set on `Event::PairSuccess` so the subsequent `Event::Connected`
    /// knows this is a fresh pairing (first-time or re-pair after reset),
    /// not a routine reconnect after a restart. Consumed by
    /// `take_first_pair_pending` so the greeting fires only once per
    /// pairing and never on a plain app restart.
    pub(super) first_pair_pending: std::sync::atomic::AtomicBool,
    /// Photo batching buffer: (chat_jid) → Vec<(img_marker, caption)>
    /// When multiple photos arrive in quick succession (WhatsApp sends
    /// each as a separate message), we buffer them and dispatch together.
    #[allow(clippy::type_complexity)]
    pub(super) photo_buffer: Mutex<HashMap<String, Vec<(String, Option<String>)>>>,
    /// Photo debounce cancellation tokens: chat_jid → CancellationToken
    pub(crate) photo_debounce: Mutex<HashMap<String, CancellationToken>>,
    /// session_id → chat JID, so a finished background task can resume the
    /// originating chat (#731). WhatsApp keeps no other session→target map;
    /// registered on each handled turn.
    pub(super) session_jids: Mutex<HashMap<Uuid, String>>,
    /// Reverse ownership map (#148): chat JID → session_id, written in
    /// lockstep with `session_jids` at `register_session_jid` (the ONLY
    /// write site for both). Last writer wins — mirrors the forward map.
    pub(super) jid_sessions: Mutex<HashMap<String, Uuid>>,
    /// Local mirror of the server blocklist (#1487), so the inbound guard
    /// can drop a blocked sender without a server round trip per message.
    /// See `blocklist.rs`.
    pub(crate) blocklist: super::blocklist::Blocklist,
    /// Option lists for polls this bot created (#1482). Votes reference
    /// options by hash, never by name, so labelling a vote needs the poll's
    /// own list. See `poll.rs`.
    pub(crate) polls: super::poll::PollOptions,
    /// Bounded memory of recently seen messages (#1484), so `forward` can
    /// hand `Client::forward_message` the original proto. See `recent.rs`.
    pub(crate) recent: super::recent::RecentMessages,
    /// Last editable message per session (#1408). `SendResult.message_id`
    /// used to be discarded at every send site; edit-in-place streaming,
    /// self-reactions and pin/forward all need it, so it is captured once
    /// here. See `outbox.rs`.
    pub(crate) outbox: super::outbox::Outbox,
    /// Outbound rate limiter (#1407): token-bucket pacing + rolling 24h
    /// cap with a FIFO queue. Shared by the send tool, the handler chunk
    /// loops, the resume path and the drainer task - one budget for the
    /// whole channel. `pub(crate)` so brain/tools can gate through it.
    pub(crate) rate_limiter: std::sync::Arc<super::rate_limit::WhatsappRateLimiter>,
}

impl Default for WhatsAppState {
    fn default() -> Self {
        Self::new()
    }
}

impl WhatsAppState {
    pub fn new() -> Self {
        let (qr_tx, _) = tokio::sync::broadcast::channel(8);
        let (connected_tx, _) = tokio::sync::broadcast::channel(4);
        let (error_tx, _) = tokio::sync::broadcast::channel(4);
        let (delivered_tx, _) = tokio::sync::broadcast::channel(32);
        Self {
            client: Mutex::new(None),
            owner_jid: Mutex::new(None),
            pending_approvals: Mutex::new(HashMap::new()),
            pending_followups: Mutex::new(HashMap::new()),
            cancel_tokens: Mutex::new(HashMap::new()),
            active_turns: std::sync::Mutex::new(std::collections::HashSet::new()),
            turn_queue: std::sync::Mutex::new(HashMap::new()),
            qr_tx,
            connected_tx,
            error_tx,
            delivered_tx,
            last_qr: std::sync::Mutex::new(None),
            restart_requested: std::sync::atomic::AtomicBool::new(false),
            connected: std::sync::atomic::AtomicBool::new(false),
            first_pair_pending: std::sync::atomic::AtomicBool::new(false),
            photo_buffer: Mutex::new(HashMap::new()),
            photo_debounce: Mutex::new(HashMap::new()),
            session_jids: Mutex::new(HashMap::new()),
            jid_sessions: Mutex::new(HashMap::new()),
            polls: super::poll::PollOptions::default(),
            recent: super::recent::RecentMessages::default(),
            blocklist: super::blocklist::Blocklist::default(),
            outbox: super::outbox::Outbox::default(),
            rate_limiter: std::sync::Arc::new(super::rate_limit::WhatsappRateLimiter::new()),
        }
    }
}
