//! Shared Telegram runtime state.
//!
//! `TelegramState` and its turn-guard / cancel-token / queue / photo-buffer
//! plumbing, plus the small type aliases it uses. Split out of `mod.rs` so the
//! module root is declarations and re-exports only.

use super::cowork;
use std::collections::{HashMap, VecDeque};
use teloxide::prelude::Bot;
use teloxide::types::MessageId;
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// The answer bubble a merged suggestion keyboard rides on.
///
/// When `suggest_options` attaches its buttons to the final response bubble
/// instead of posting a separate "Suggested next" message, the tap handler
/// needs the bubble's exact HTML to record the pick WITHOUT erasing the
/// answer text (`edit_message_text` replaces the whole body).
/// #59: bound on rescued dead-host records (FIFO eviction).
const STALE_HOST_CAP: usize = 32;

#[derive(Clone)]
pub(crate) struct MergedHost {
    pub message_id: MessageId,
    /// Body last rendered in that bubble (final response text, plus the
    /// folded option list when present). HTML for html-plane hosts; the
    /// merged markdown payload for markdown-plane hosts.
    pub html: String,
    /// Host lives on the native rich API: tap-record edits must ride
    /// `super::rich::api::edit_rich_html` (or `edit_rich_markdown` for
    /// markdown-plane hosts), not teloxide's edit_message_text.
    pub rich: bool,
    /// Markdown-plane host (#79 piece 4): the merged markdown payload.
    /// When set, pick redraws and strips ride `edit_rich_markdown` —
    /// the server-side render keeps tables intact (#679). `None` = the
    /// html plane.
    pub markdown: Option<String>,
}

/// Merge candidate captured by deliver_final_response (#tg-suggest-merge):
/// the bubble the final response landed in, whatever surface sent it.
#[derive(Clone)]
pub(crate) struct MergeBubble {
    pub message_id: MessageId,
    /// Body last delivered in that bubble: classic HTML for classic
    /// bubbles, the captured markdown for rich ones. Markdown-plane
    /// hosts (#79 piece 4) keep the raw markdown — the merge edit rides
    /// `edit_rich_markdown`, whose server-side render keeps tables
    /// intact (#679).
    pub body: BubbleBody,
}

/// How a captured [`MergeBubble`] was sent — decides which edit call merges
/// the suggestion controls onto it.
#[derive(Clone)]
pub(crate) enum BubbleBody {
    /// Classic parse-mode HTML exactly as delivered.
    Html(String),
    /// Native rich message sent from this markdown (tables render
    /// server-side). Merging re-renders via `rich::markdown_to_html`, so
    /// capture SKIPS table-bearing answers — rich HTML input flattens
    /// tables (#679) — and those keep the standalone fallback.
    Markdown(String),
}

/// What a targeted `/stop <id>` actually found (#2108).
///
/// Three states, because "no operation in progress" is a lie in two of them:
/// a turn can be running, or nothing can be running while messages still sit
/// queued behind the last one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetStop {
    /// A turn was running; its token is cancelled.
    Delivered { queued_dropped: usize },
    /// Nothing was running, but follow-ups were queued and are now dropped.
    Queued { dropped: usize },
    /// Nothing to do: idle, nothing queued.
    Idle,
}

/// One registered follow-up suggestion keyboard: which session it belongs to,
/// the options it offered, and — when the buttons were merged onto the answer
/// bubble (#tg-suggest-merge) — that bubble. Keyed by an opaque short token,
/// NOT by session: consecutive turns' keyboards coexist, and each tap
/// resolves against its OWN keyboard's set instead of whatever newer set
/// last overwrote a per-session slot (#1217).
#[derive(Clone)]
pub(crate) struct PendingFollowupEntry {
    pub session_id: Uuid,
    pub options: Vec<String>,
    pub host: Option<MergedHost>,
}

/// Photo buffer entry: (img_marker, Optional caption)
type PhotoEntry = (String, Option<String>);

/// Photo buffer key: (chat_id, user_id, media_group_id)
type PhotoBufferKey = (i64, i64, String);
type DirBrowserKey = (i64, Option<i32>);
type DirBrowserState = (String, Option<String>);

/// Where a mid-turn queued message came from (#1213).
///
/// The queue's designed consumer is the live tool loop, which drains it
/// between rounds and does not care about origin. The end-of-turn flush does:
/// it runs when the loop has already exited, and the two kinds need different
/// destinations from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueuedOrigin {
    /// An emoji reaction. A single toolless round is the right size for it.
    Reaction,
    /// A background command or sub-agent result. Needs a real tool loop.
    DetachedWork,
}

/// A queued message with the origin that decides how it is flushed.
pub(crate) struct QueuedItem {
    pub(crate) origin: QueuedOrigin,
    pub(crate) msg: crate::brain::agent::QueuedUserMessage,
}

/// Sync mirror behind the channel-ownership gate (fork #17). Maintained ONLY
/// by `TelegramState::register_session_chat` — the single write site for the
/// ownership maps — so it cannot drift from them locally. See the
/// `channel_ownership` field doc for why a sync copy exists at all.
#[derive(Default)]
struct ChannelOwnershipMirror {
    /// session → the `(chat_id, topic_id)` it was bound to.
    session_channel: HashMap<Uuid, (i64, Option<i32>)>,
    /// `(chat_id, topic_id)` → the session currently bound to it.
    channel_owner: HashMap<(i64, Option<i32>), Uuid>,
}

/// Shared Telegram state for proactive messaging.
///
/// Set when the bot connects (agent stores Bot) and when the owner
/// sends their first message (handler stores chat_id).
/// Read by the `telegram_send` tool to send messages on demand.
pub struct TelegramState {
    bot: Mutex<Option<Bot>>,
    /// Chat ID of the owner's conversation — used as default for proactive sends
    owner_chat_id: Mutex<Option<i64>>,
    /// Cached `(full_name, username)` of the owner, captured when an owner
    /// message arrives. Used to flag non-owner senders whose display name or
    /// username mimics the owner (impersonation detection in group chats).
    owner_identity: Mutex<Option<(String, Option<String>)>>,
    /// Bot's @username — set at startup via get_me(), used for @mention detection in groups
    bot_username: Mutex<Option<String>>,
    /// Bot's numeric user ID — set at startup via get_me(), used to distinguish
    /// replies to THIS bot from replies to other bots in group chats.
    bot_user_id: Mutex<Option<i64>>,
    /// Maps session_id → Telegram chat_id for approval routing. Topic-agnostic:
    /// approval/question replies route back by `chat_id` plus the per-message
    /// `thread_id` captured at send time, so the topic does not belong here.
    session_chats: Mutex<HashMap<Uuid, i64>>,
    /// Settled flow-card handles (#1377): session_id → the StreamingState Arc
    /// whose card settled last in this session's chat. Background-task
    /// completion acks fold into that card (line append + counter re-stamp +
    /// re-render) instead of spraying standalone bubbles. Registered at turn
    /// settle, overwritten by the next settle, cleared on teardown. The Arc
    /// keeps the full render pipeline (flow lines, sections, chrome) usable
    /// after the turn task drops its own reference.
    session_flow_states: Mutex<
        HashMap<
            Uuid,
            std::sync::Arc<std::sync::Mutex<crate::channels::telegram::flow::StreamingState>>,
        >,
    >,
    /// Reverse map: (chat_id, forum_topic_id) → session_id. The topic component
    /// is `Some` only for genuine forum-topic messages (#215); DMs, non-forum
    /// groups, and the General topic key on `(chat_id, None)`, preserving the
    /// pre-topic behaviour. Each forum topic therefore binds its own session.
    chat_sessions: Mutex<HashMap<(i64, Option<i32>), Uuid>>,
    session_topic: Mutex<HashMap<Uuid, Option<i32>>>,
    /// Sync mirror of the ownership triangle for the channel-ownership gate
    /// (fork #17): session → its `(chat, topic)` and `(chat, topic)` → current
    /// occupant. Written ONLY in `register_session_chat`, beside the async
    /// maps above — the delivery gate's probe is a sync closure and cannot
    /// take tokio mutexes. Never used for routing; the async maps stay the
    /// source of truth for everything else.
    channel_ownership: std::sync::Mutex<ChannelOwnershipMirror>,
    /// Evidence-based forum detection (#1220): chat_id → true once a
    /// thread-scoped message FLAGGED `is_topic_message` was seen from that
    /// chat. A bare thread id alone is NOT proof: Telegram also sets
    /// `message_thread_id` on ordinary reply chains in non-forum groups, so
    /// trusting it permanently misclassified real chats as forums and
    /// corrupted their session routing (#1708). Consumed by
    /// `normalize_topic` at ingress to give the General topic its own
    /// session bucket instead of None.
    chat_forums: Mutex<HashMap<i64, bool>>,
    /// Pending approval channels: approval_id → oneshot sender of (approved, always).
    pending_approvals: Mutex<HashMap<String, oneshot::Sender<(bool, bool)>>>,
    /// Pending follow-up questions: question_id → (oneshot sender of
    /// the chosen option string, list of options keyed by index). The
    /// inline-keyboard callback data only carries the option index (to
    /// tool unblocks and returns it as the answer, instead of queueing.
    /// Per-session OPTIONAL follow-up suggestions surfaced by the
    /// `suggest_options` tool (#597). Unlike `pending_questions` these are
    /// non-blocking: the buttons ride under the response and a tap injects the
    /// chosen suggestion as the user's next message. Keyed by session so the
    /// tap handler resolves `idx -> suggestion string`; cleared on tap or when
    /// the user sends anything.
    pending_followups: Mutex<HashMap<String, PendingFollowupEntry>>,
    /// #59: merged-host records of stash entries cleared by #597 (user sent
    /// their own message). The stash entry dies, but RICH-merged buttons
    /// survive visually inside the bubble body — a stale-shell tap needs to
    /// know WHAT SHAPE of dead keyboard to strip and from where. Keyed
    /// token-first in a FIFO deque, bounded at STALE_HOST_CAP (oldest evicted).
    stale_hosts: Mutex<VecDeque<(String, MergedHost)>>,
    /// Solo-owner auto-registration cache (#1155): chat_id → decision already
    /// reached. `true` = eligible solo group, full owner catalog registered;
    /// `false` = evaluated and ineligible. Cleared on membership events so the
    /// next message re-evaluates instead of trusting a stale snapshot — the
    /// exact staleness class Gap 2 of #1155 describes.
    solo_evaluated: Mutex<HashMap<i64, bool>>,
    /// Skills signature at the time the command menus were last published
    /// (#1317). Compared on every inbound message by `menu_refresh`; a
    /// mismatch re-publishes the scoped menus so new/edited skills appear
    /// in the `/` picker without a restart or config write.
    menu_skills_sig: Mutex<Option<u64>>,
    /// Per-session cancel tokens for aborting in-flight agent tasks via /stop
    cancel_tokens: Mutex<HashMap<Uuid, CancellationToken>>,
    /// Who started each live guest turn, keyed by session (#2143).
    ///
    /// A guest turn's session belongs to a chat the bot is not in, so the
    /// caller can only type `/stop` somewhere else: in a DM, or in a group
    /// where the bot IS a member. Plain `/stop` there resolves its target from
    /// the message's own session, which is not the guest one, so without this
    /// map a guest turn had no kill switch from chat at all. std lock because
    /// the write happens on the turn's own path and must not await.
    guest_owners: std::sync::Mutex<HashMap<Uuid, i64>>,
    /// Persistent plan-card message per session (#580): the single Telegram
    /// message that shows the plan title + checklist and the Approve/Discard
    /// keyboard, edited in place across the creation/execution/completion turns
    /// instead of re-rendered inside each per-turn flow block. The `String` is a
    /// content signature (rendered body + keyboard) so a per-tick refresh skips
    /// the edit API call when nothing changed — avoiding a redundant-edit
    /// rate-limit storm.
    plan_cards: Mutex<HashMap<Uuid, (teloxide::types::MessageId, String)>>,
    /// Durable backing for `plan_cards` (#809). The map alone is process-local,
    /// so a restart lost the tracked message id and the card could then neither
    /// be edited nor removed: the next turn posted a SECOND card below the
    /// stale one instead of updating it. Set once at startup; `None` on
    /// surfaces built without a database, which keeps the old in-memory-only
    /// behaviour rather than failing.
    plan_card_store: Mutex<Option<crate::db::repository::PlanCardRepository>>,
    /// Backing for the plan card's `channel_messages` row (#1684). Every other
    /// bot bubble is recorded there, which is what lets a later turn read a
    /// reply to it, resolve a reaction on it, or see it in group history. The
    /// card path had no such write, so the only bot bubble in Telegram was
    /// invisible to the agent that posted it. `None` on surfaces built without
    /// a database: cards still render, nothing is recorded.
    channel_message_store: Mutex<Option<crate::db::repository::ChannelMessageRepository>>,
    /// Durable backing for `pending_followups` (#1226 item 3): rows are
    /// written when a keyboard arms (and when its merge host attaches),
    /// deleted on tap/drop/clear, and hydrated back into the map at boot so
    /// keyboards that survived a restart keep resolving taps instead of
    /// dying as unknown-token stale shells. `None` on surfaces built
    /// without a database — old in-memory-only behaviour.
    followup_store: Mutex<Option<crate::db::repository::PendingFollowupRepository>>,
    /// Durable backing for `session_bindings` table (#170).
    /// Used by `bind_session_topic` to persist proactive topic bindings.
    binding_store: Mutex<Option<crate::db::SessionBindingRepository>>,
    /// Enqueue callback hook for background task routing (#170).
    /// Used by `bind_session_topic` to claim session routes for Telegram.
    enqueue_callback: Mutex<Option<crate::brain::agent::service::MessageEnqueueCallback>>,
    /// Session → when card writes may resume, after Telegram asked us to wait
    /// (#814). Without this the next refresh wrote immediately and renewed the
    /// flood-control window, so the countdown never elapsed.
    plan_card_backoff: Mutex<HashMap<Uuid, std::time::Instant>>,
    /// Session → lock serialising card writes (#822).
    ///
    /// `refresh_plan_card` reads whether a card is tracked, decides to edit or
    /// post, then records the id. With nothing held across that, two concurrent
    /// refreshes both saw no card, both posted, and the second id overwrote the
    /// first: one card was left visible but untracked, so it could never be
    /// edited or deleted again. Concurrency here is routine, since the
    /// streaming path refreshes repeatedly while the settle and resume paths
    /// also fire.
    ///
    /// Per session, so unrelated chats never wait on each other.
    plan_card_locks: Mutex<HashMap<Uuid, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    /// Photo batching buffer: (chat_id, user_id, media_group_id) → Vec<(img_marker, Option<caption>)>
    /// When user sends multiple photos in an album, we buffer them and only fire the agent
    /// after a quiet period (no new photos for 3s). Keyed by media_group_id to avoid merging
    /// unrelated photos sent within 3s of each other.
    photo_buffer: Mutex<HashMap<PhotoBufferKey, Vec<PhotoEntry>>>,
    /// Photo debounce tokens: (chat_id, user_id, media_group_id) → CancellationToken
    /// Each new photo in the same album cancels the previous timer and starts a new 3s one.
    photo_debounce: Mutex<HashMap<PhotoBufferKey, CancellationToken>>,
    /// Fragments of a client-split long message: (chat_id, user_id) → texts, in
    /// arrival order.
    ///
    /// A message too long for one send is split by the Telegram client, and
    /// unlike an album the pieces carry NO grouping id — nothing marks fragment
    /// two as a continuation. Only near-limit messages are buffered, so an
    /// ordinary message never waits (#950).
    text_buffer: Mutex<HashMap<(i64, i64), Vec<String>>>,
    /// Debounce tokens for the above: each new fragment cancels the previous
    /// timer and starts a fresh one, so the window measures the gap between
    /// fragments rather than the age of the first.
    text_debounce: Mutex<HashMap<(i64, i64), CancellationToken>>,
    /// Active /cowork conversations: user_id → CoworkState
    cowork_conversations: Mutex<HashMap<i64, cowork::CoworkState>>,
    /// Cowork session lookup: session_id → CoworkState (for startgroup detection)
    cowork_sessions: Mutex<HashMap<String, cowork::CoworkState>>,
    /// Active sender tracking for auto mention-only mode (#244).
    /// Maps chat_id → set of user_ids that have sent ≥1 message.
    /// Set never shrinks — once >1 sender is detected, the chat
    /// permanently switches to mention-only until manually reset.
    active_senders: Mutex<HashMap<i64, std::collections::HashSet<i64>>>,
    /// Directory browser state: chat_id → (current_path, filter).
    /// Used by /cd inline-keyboard callbacks to know which directory
    /// is being browsed without encoding full paths in callback data.
    dir_browsers: Mutex<HashMap<DirBrowserKey, DirBrowserState>>,
    /// Profile create flow state: chat_id → true when awaiting a profile name
    prof_create_states: Mutex<HashMap<i64, bool>>,
    /// Pending file-save JoinHandles keyed by chat_id. The spawned task that
    /// downloads incoming media to tmp registers its handle here so the
    /// downstream tmp-photo pickup can `drain + await` before scanning,
    /// eliminating the race between fire-and-forget saves and mention handling.
    pending_file_saves: Mutex<HashMap<i64, Vec<tokio::task::JoinHandle<()>>>>,
    /// Reactions that landed while a turn was already running, waiting to be
    /// injected into that turn's tool loop between rounds. Keyed by session_id,
    /// drained FIFO (#302 Stage 2). `std::sync::Mutex` (not tokio) so the drain
    /// callback and the RAII active-turn guard can touch it without awaiting.
    pending_reactions: std::sync::Mutex<HashMap<Uuid, std::collections::VecDeque<QueuedItem>>>,
    /// Sessions with an agent turn currently in flight, so `handle_reaction` can
    /// tell mid-turn (enqueue for injection) from idle (fire a standalone turn).
    /// Maintained via [`ActiveTurnGuard`] so a crashed turn can't leave a
    /// session looking permanently busy.
    active_turns: std::sync::Mutex<std::collections::HashSet<Uuid>>,
    /// #61: sessions with a LIVE flow roll on screen — the in-flight
    /// `StreamingState` plus the chat coordinates its block renders in.
    /// The push-echo path consults this to decide that a session notify
    /// folds a line INTO the roll instead of posting a standalone bubble.
    /// Registered at both turn sites (user turns and push-initiated turns)
    /// via [`TelegramState::register_live_flow`], which returns a
    /// [`LiveFlowRegistration`] drop-guard so any turn exit — normal,
    /// error, or panic — unregisters. `std::sync::Mutex` like
    /// `active_turns`: the echo path is async but the lock is never held
    /// across an await, so blocking contention is nil.
    live_flows: std::sync::Mutex<HashMap<Uuid, LiveFlowHandle>>,
    /// #61 fold-dedupe (Alexey 2026-09-06): fingerprints of notifies
    /// already folded into this session's roll. The 776a1fe5 incident
    /// folded ONE notify into TWO concurrent rolls of the same session
    /// (the #845 single-turn guard breach is lane #105's root cause;
    /// this set is the roll-side defense). Entries must OUTLIVE any
    /// single roll — the duplicate crossed rolls 25 minutes apart — so
    /// the set is keyed by session, not by roll; it stays bounded via
    /// `TelegramState::NOTIFY_FOLD_DEDUP_CAP` + TTL expiry.
    notify_fold_dedup:
        std::sync::Mutex<HashMap<Uuid, std::collections::VecDeque<(u64, std::time::Instant)>>>,
    /// Newest NON-STICKY message id seen per chat (#451, semantics sharpened
    /// by #1150). This is burial EVIDENCE for the flow block: user messages
    /// (recorded in the handler) plus non-sticky bot bubbles — intermediate
    /// reports (#582), follow-up questions. Sticky elements deliberately do
    /// NOT feed it: flow-block reposts and plan-card reposts never call these
    /// recorders, so one restick can never manufacture evidence that buries
    /// the other sticky element (the ping-pong #1150 closes). Telegram message
    /// ids are per-chat monotonic, so a plain max is a valid "is the block
    /// buried" test.
    chat_newest_msg_id: std::sync::Mutex<HashMap<i64, i32>>,
    /// Per-chat instant of the last sticky-stack action (#1150): a flow-block
    /// restick or a plan-card move, each of which is a delete+create pair.
    /// One shared flood-control budget instead of two independent gates —
    /// uncoordinated bursts of both were exactly the #814 regression shape.
    last_sticky_action: std::sync::Mutex<HashMap<i64, std::time::Instant>>,
    /// Dedup of outbound media uploaded via `telegram_send` so an identical
    /// file+caption isn't delivered twice back-to-back (#721).
    media_dedup: super::outbound_dedup::MediaSendDedup,
    /// Tracks which session sent which inline-button callback data (#878).
    /// When `telegram_send send_buttons` posts buttons, each callback_data
    /// string is mapped to the sending session's UUID.  The callback
    /// dispatcher looks up this map BEFORE falling back to chat-context
    /// resolution, ensuring callbacks route to the session that ORIGINATED
    /// the buttons — not whatever session last handled a message in that
    /// chat.  Entries are ephemeral, in-memory, and evictable (LIFO
    /// overwrite is fine; one active set per session is the norm).
    callback_origins: std::sync::Mutex<HashMap<String, Uuid>>,
}

impl Default for TelegramState {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard that keeps a session marked "turn active" for its lifetime and
/// clears the flag on drop (#302 Stage 2). Held for the whole span of a
/// `handle_message` turn so a reaction arriving mid-turn is enqueued for
/// injection rather than firing a second concurrent turn on the same session.
pub(crate) struct ActiveTurnGuard {
    state: std::sync::Arc<TelegramState>,
    session_id: Uuid,
}

impl Drop for ActiveTurnGuard {
    fn drop(&mut self) {
        if let Ok(mut set) = self.state.active_turns.lock() {
            set.remove(&self.session_id);
        }
    }
}

/// #61: everything the push-echo path needs to fold a notify line into a
/// session's LIVE flow roll. `streaming` is the same Arc the edit loop
/// renders from, so a fold under its lock is ordered with the loop's own
/// writes; `chat`/`thread_id` are the coordinates the block's refresh
/// (`refresh_flow`) edits through. A handle is only meaningful while
/// `streaming.open_group_msg_id` is `Some` — the caller re-checks that at
/// fold time, because the roll can close between registration and echo.
#[derive(Clone)]
pub(crate) struct LiveFlowHandle {
    pub(crate) streaming: std::sync::Arc<std::sync::Mutex<super::flow::StreamingState>>,
}

/// RAII counterpart of [`ActiveTurnGuard`] for the #61 live-flow registry:
/// held for the whole span of a turn so the echo path can trust that a
/// registered handle belongs to a turn that is still running. Drop
/// unregisters — normal return, `?`, and panic all end here.
pub(crate) struct LiveFlowRegistration {
    state: std::sync::Arc<TelegramState>,
    session_id: Uuid,
    /// The streaming Arc this registration wrote — drop compares pointer
    /// identity so a stale guard never evicts a successor turn's entry.
    streaming: std::sync::Arc<std::sync::Mutex<super::flow::StreamingState>>,
}

impl Drop for LiveFlowRegistration {
    fn drop(&mut self) {
        if let Ok(mut map) = self.state.live_flows.lock() {
            let mine = map
                .get(&self.session_id)
                .is_some_and(|h| std::sync::Arc::ptr_eq(&h.streaming, &self.streaming));
            if mine {
                map.remove(&self.session_id);
            }
        }
    }
}

impl TelegramState {
    pub fn new() -> Self {
        Self {
            bot: Mutex::new(None),
            owner_chat_id: Mutex::new(None),
            owner_identity: Mutex::new(None),
            bot_username: Mutex::new(None),
            bot_user_id: Mutex::new(None),
            session_chats: Mutex::new(HashMap::new()),
            session_flow_states: Mutex::new(HashMap::new()),
            chat_sessions: Mutex::new(HashMap::new()),
            session_topic: Mutex::new(HashMap::new()),
            channel_ownership: std::sync::Mutex::new(ChannelOwnershipMirror::default()),
            chat_forums: Mutex::new(HashMap::new()),
            pending_approvals: Mutex::new(HashMap::new()),
            pending_followups: Mutex::new(HashMap::new()),
            stale_hosts: Mutex::new(VecDeque::new()),
            solo_evaluated: Mutex::new(HashMap::new()),
            menu_skills_sig: Mutex::new(None),
            cancel_tokens: Mutex::new(HashMap::new()),
            guest_owners: std::sync::Mutex::new(HashMap::new()),
            plan_cards: Mutex::new(HashMap::new()),
            plan_card_store: Mutex::new(None),
            channel_message_store: Mutex::new(None),
            followup_store: Mutex::new(None),
            binding_store: Mutex::new(None),
            enqueue_callback: Mutex::new(None),
            plan_card_backoff: Mutex::new(HashMap::new()),
            plan_card_locks: Mutex::new(HashMap::new()),
            photo_buffer: Mutex::new(HashMap::new()),
            photo_debounce: Mutex::new(HashMap::new()),
            text_buffer: Mutex::new(HashMap::new()),
            text_debounce: Mutex::new(HashMap::new()),
            cowork_conversations: Mutex::new(HashMap::new()),
            cowork_sessions: Mutex::new(HashMap::new()),
            active_senders: Mutex::new(HashMap::new()),
            dir_browsers: Mutex::new(HashMap::new()),
            prof_create_states: Mutex::new(HashMap::new()),
            pending_file_saves: Mutex::new(HashMap::new()),
            pending_reactions: std::sync::Mutex::new(HashMap::new()),
            active_turns: std::sync::Mutex::new(std::collections::HashSet::new()),
            live_flows: std::sync::Mutex::new(HashMap::new()),
            notify_fold_dedup: std::sync::Mutex::new(HashMap::new()),
            chat_newest_msg_id: std::sync::Mutex::new(HashMap::new()),
            last_sticky_action: std::sync::Mutex::new(HashMap::new()),
            media_dedup: super::outbound_dedup::MediaSendDedup::default(),
            callback_origins: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Claim an outbound media send, returning `true` if it is fresh and
    /// `false` if an identical file+caption was sent to this chat within the
    /// dedup window (#721). Callers skip the actual upload on `false`.
    pub(crate) fn claim_media_send(
        &self,
        action: &str,
        chat_id: i64,
        reference: &str,
        caption: Option<&str>,
    ) -> bool {
        let sig =
            super::outbound_dedup::MediaSendDedup::signature(action, chat_id, reference, caption);
        self.media_dedup.claim(sig, std::time::Instant::now())
    }

    /// Record an incoming message id for a chat, keeping the per-chat maximum
    /// (#451). Called at the top of the handler for every message so burial
    /// detection sees non-mention chatter too.
    pub(crate) fn note_incoming_msg(&self, chat_id: i64, msg_id: i32) {
        let mut map = self
            .chat_newest_msg_id
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(chat_id).or_insert(msg_id);
        if msg_id > *entry {
            *entry = msg_id;
        }
    }

    /// Record a NON-STICKY bot-sent bubble as burial evidence (#1150):
    /// intermediate rich reports (#582) and follow-up question bubbles land
    /// below the flow block exactly like user chatter, but before this
    /// recorder nothing tracked them, so the block stayed buried under its own
    /// output for the rest of the turn. Sticky sends (flow-block reposts,
    /// plan-card reposts) must NEVER call this — their ids are positions, not
    /// evidence, and counting them is the restick ping-pong #1150 closes.
    pub(crate) fn note_bot_bubble(&self, chat_id: i64, msg_id: i32) {
        self.note_incoming_msg(chat_id, msg_id);
    }

    /// Claim permission for one sticky-stack action (flow-block restick or
    /// plan-card move) in `chat_id` (#1150). Returns false when another such
    /// action fired less than `min_interval` ago; the caller then skips its
    /// delete+create pair instead of bursting toward Telegram's flood control
    /// (#814). Both sticky mechanisms draw from this one budget so they can't
    /// combine into the storm two independent gates allowed.
    pub(crate) fn claim_sticky_action(
        &self,
        chat_id: i64,
        min_interval: std::time::Duration,
    ) -> bool {
        let mut map = self
            .last_sticky_action
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        if let Some(last) = map.get(&chat_id)
            && now.duration_since(*last) < min_interval
        {
            return false;
        }
        map.insert(chat_id, now);
        true
    }

    /// Minimum spacing between ANY sticky-stack actions in one chat (#1150):
    /// a flow-block restick and its coordinated plan-card move count as ONE
    /// action; the next restick (either mechanism) waits this long. Bounds
    /// delete+create churn well under Telegram's group flood limits while
    /// keeping #451 burial recovery responsive.
    pub(crate) const STICKY_STACK_MIN_INTERVAL: std::time::Duration =
        std::time::Duration::from_secs(15);

    /// Newest incoming message id seen in a chat, if any (#451). The streaming
    /// edit loop compares this against its open flow block's message id to
    /// decide whether the block was buried and should re-stick to the bottom.
    pub(crate) fn newest_incoming_msg_id(&self, chat_id: i64) -> Option<i32> {
        self.chat_newest_msg_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&chat_id)
            .copied()
    }

    /// Register callback_data → originating session for inline buttons (#878).
    /// Called by `telegram_send send_buttons` so the callback dispatcher can
    /// route taps to the session that SENT the buttons, not the one bound
    /// to the chat.
    pub(crate) fn register_callback_origins(
        &self,
        session_id: Uuid,
        callback_datas: impl IntoIterator<Item = String>,
    ) {
        if let Ok(mut map) = self.callback_origins.lock() {
            for data in callback_datas {
                map.insert(data, session_id);
            }
        }
    }

    /// Look up which session originated a given callback_data string (#878).
    pub(crate) fn lookup_callback_origin(&self, callback_data: &str) -> Option<Uuid> {
        self.callback_origins
            .lock()
            .ok()
            .and_then(|map| map.get(callback_data).copied())
    }

    /// Store the connected Bot instance.
    pub async fn set_bot(&self, bot: Bot) {
        *self.bot.lock().await = Some(bot);
    }

    /// Update the owner's chat ID (called on each owner message).
    pub async fn set_owner_chat_id(&self, chat_id: i64) {
        *self.owner_chat_id.lock().await = Some(chat_id);
    }

    /// Cache the owner's display identity (captured from an owner message) so
    /// later non-owner senders can be checked for impersonation.
    pub async fn set_owner_identity(&self, full_name: String, username: Option<String>) {
        *self.owner_identity.lock().await = Some((full_name, username));
    }

    pub async fn owner_identity(&self) -> Option<(String, Option<String>)> {
        self.owner_identity.lock().await.clone()
    }

    /// Get a clone of the Bot, if connected.
    pub async fn bot(&self) -> Option<Bot> {
        self.bot.lock().await.clone()
    }

    /// Get the owner's chat ID for proactive messaging.
    pub async fn owner_chat_id(&self) -> Option<i64> {
        *self.owner_chat_id.lock().await
    }

    /// Store the bot's @username (set at startup via get_me).
    pub async fn set_bot_username(&self, username: String) {
        *self.bot_username.lock().await = Some(username);
    }

    /// Store the bot's numeric user ID (set at startup via get_me).
    pub async fn set_bot_user_id(&self, id: i64) {
        *self.bot_user_id.lock().await = Some(id);
    }

    /// Get the bot's @username for mention detection.
    pub async fn bot_username(&self) -> Option<String> {
        self.bot_username.lock().await.clone()
    }

    /// Get the bot's numeric user ID for reply-to-bot detection.
    pub async fn bot_user_id(&self) -> Option<i64> {
        *self.bot_user_id.lock().await
    }

    /// Cached solo-owner evaluation for a chat, if one was reached (#1155).
    pub async fn solo_evaluated(&self, chat_id: i64) -> Option<bool> {
        self.solo_evaluated.lock().await.get(&chat_id).copied()
    }

    /// Record a solo-owner evaluation outcome (#1155).
    pub async fn set_solo_evaluated(&self, chat_id: i64, eligible: bool) {
        self.solo_evaluated.lock().await.insert(chat_id, eligible);
    }

    /// Forget a chat's solo-owner evaluation — membership changed, next
    /// message re-evaluates (#1155).
    pub async fn clear_solo_evaluated(&self, chat_id: i64) {
        self.solo_evaluated.lock().await.remove(&chat_id);
    }

    /// Chats auto-registered as solo-owner groups (#1155): eligible and
    /// currently menu-published. `menu_refresh` re-publishes exactly these
    /// on skills changes, since `maybe_auto_register` evaluates each chat
    /// once and never revisits an already-evaluated chat (#1317).
    pub async fn solo_registered_chats(&self) -> Vec<i64> {
        self.solo_evaluated
            .lock()
            .await
            .iter()
            .filter(|(_, eligible)| **eligible)
            .map(|(chat_id, _)| *chat_id)
            .collect()
    }

    /// Skills signature at the last menu publish, if menus were ever
    /// published in this process (#1317).
    pub async fn menu_skills_sig(&self) -> Option<u64> {
        *self.menu_skills_sig.lock().await
    }

    /// Record the skills signature the current menus were published
    /// under (#1317).
    pub async fn set_menu_skills_sig(&self, sig: u64) {
        *self.menu_skills_sig.lock().await = Some(sig);
    }

    /// Check if Telegram is currently connected.
    pub async fn is_connected(&self) -> bool {
        self.bot.lock().await.is_some()
    }

    /// Record forum-ness evidence for a chat (#1220, gated per #1708). A
    /// thread-scoped message that Telegram itself flagged
    /// `is_topic_message` proves the chat is a forum group. A bare
    /// `thread_id` WITHOUT the flag is an ordinary reply-chain id and
    /// proves nothing — non-forum groups set it on plain replies, and
    /// trusting it permanently misclassified real chats as forums
    /// (#1708). One-way: forum-ness never un-proves itself for the
    /// process lifetime.
    pub async fn note_thread_evidence(
        &self,
        chat_id: i64,
        is_topic_message: bool,
        thread_id: Option<i32>,
    ) {
        if is_topic_message && thread_id.is_some() {
            self.chat_forums.lock().await.insert(chat_id, true);
        }
    }

    /// Whether this chat has ever hosted a thread-scoped message (#1220).
    /// False until the first observation — the cold-start window during which
    /// General-topic messages keep the legacy shared-session behaviour.
    pub async fn is_known_forum(&self, chat_id: i64) -> bool {
        self.chat_forums
            .lock()
            .await
            .get(&chat_id)
            .copied()
            .unwrap_or(false)
    }

    /// Record which chat_id corresponds to a given session (for approval routing).
    /// Also maintains a reverse map so callbacks can resolve session from chat.
    ///
    /// The reverse map keys on `(chat_id, topic_id)` so distinct forum topics in
    /// one supergroup bind distinct sessions (#215); since #1220 the General
    /// topic of a KNOWN forum passes `Some(GENERAL_TOPIC_ID)` (normalized at
    /// ingress), while plain `None` remains for DMs / non-forum groups. The
    /// forward `session_chats` map stays topic-agnostic (approval routing only
    /// needs the chat_id).
    pub async fn register_session_chat(
        &self,
        session_id: Uuid,
        chat_id: i64,
        topic_id: Option<i32>,
    ) {
        self.session_chats.lock().await.insert(session_id, chat_id);
        self.session_topic.lock().await.insert(session_id, topic_id);
        self.chat_sessions
            .lock()
            .await
            .insert((chat_id, topic_id), session_id);
        // Keep the sync ownership mirror in lockstep (fork #17). This is the
        // ONLY write site for the maps above, so one extra lock keeps the
        // mirror exact; the delivery gate's probe reads it synchronously.
        if let Ok(mut mirror) = self.channel_ownership.lock() {
            mirror
                .session_channel
                .insert(session_id, (chat_id, topic_id));
            mirror.channel_owner.insert((chat_id, topic_id), session_id);
        }
    }

    /// Look up the chat_id for a given session_id.
    pub async fn session_chat(&self, session_id: Uuid) -> Option<i64> {
        self.session_chats.lock().await.get(&session_id).copied()
    }

    /// Which platform owns this session, from the durable binding (#1889):
    /// "telegram", "discord", "slack", "whatsapp"... `None` when no binding
    /// was ever recorded — targetless cron and A2A sessions land here, and
    /// callers must treat unknown origin as no objection.
    pub async fn session_origin_channel(&self, session_id: Uuid) -> Option<String> {
        let store = self.binding_store.lock().await.clone()?;
        store
            .by_session(&session_id.to_string())
            .await
            .ok()
            .flatten()
            .map(|b| b.channel)
    }

    /// Register the settled flow-card state for a session (#1377). Called at
    /// turn settle when a flow card exists; overwrites any previous card.
    pub(crate) async fn register_flow_state(
        &self,
        session_id: Uuid,
        streaming: std::sync::Arc<
            std::sync::Mutex<crate::channels::telegram::flow::StreamingState>,
        >,
    ) {
        self.session_flow_states
            .lock()
            .await
            .insert(session_id, streaming);
    }

    /// Take the settled flow-card state for a session, if one is registered.
    pub(crate) async fn flow_state_for(
        &self,
        session_id: Uuid,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::channels::telegram::flow::StreamingState>>>
    {
        self.session_flow_states
            .lock()
            .await
            .get(&session_id)
            .cloned()
    }

    /// Clear the settled flow-card registration (card teardown, #1377).
    pub async fn clear_flow_state(&self, session_id: Uuid) {
        self.session_flow_states.lock().await.remove(&session_id);
    }

    /// Look up the forum topic_id for a given session_id. Returns `Some(tid)`
    /// for forum-topic sessions — including General-topic sessions of known
    /// forums (`Some(GENERAL_TOPIC_ID)`, #1220) — and `None` for DMs /
    /// non-forum groups. Used by `make_approval_callback` to route
    /// messages to the correct forum topic (#247, #249).
    pub async fn session_topic(&self, session_id: Uuid) -> Option<i32> {
        self.session_topic
            .lock()
            .await
            .get(&session_id)
            .copied()
            .flatten()
    }

    /// The full binding of a session — `(chat_id, topic_id)` — in ONE read
    /// (#148). Topic follows the `session_topic` rules above: `Some(1)` is
    /// a known forum's General topic, `None` a DM / non-forum group.
    /// Consumed by the tool loop's `origin_target` derivation; nothing else
    /// should need both halves separately.
    pub async fn session_binding(&self, session_id: Uuid) -> Option<(i64, Option<i32>)> {
        let chat = self.session_chats.lock().await.get(&session_id).copied()?;
        let topic = self
            .session_topic
            .lock()
            .await
            .get(&session_id)
            .copied()
            .flatten();
        Some((chat, topic))
    }

    /// Does `session_id` still own the channel it was bound to (fork #17)?
    /// Reads the sync ownership mirror (see field doc): `Owned` when the bound
    /// chat/topic still resolves back to this session, `Occupied` naming the
    /// session that took it over, `Unknown` when no binding was ever recorded
    /// (fresh boot, or the session never claimed since one). Callers: the
    /// delivery gate's probe (session_routes.rs) and the bg-resume
    /// choke-point guard (resume.rs).
    pub(crate) fn channel_ownership_of(
        &self,
        session_id: Uuid,
    ) -> crate::brain::agent::service::session_routes::ChannelOwnership {
        use crate::brain::agent::service::session_routes::ChannelOwnership;
        let Ok(mirror) = self.channel_ownership.lock() else {
            return ChannelOwnership::Unknown;
        };
        let Some(channel) = mirror.session_channel.get(&session_id).copied() else {
            return ChannelOwnership::Unknown;
        };
        match mirror.channel_owner.get(&channel).copied() {
            Some(owner) if owner != session_id => ChannelOwnership::Occupied { occupant: owner },
            Some(_) => ChannelOwnership::Owned,
            None => ChannelOwnership::Unknown,
        }
    }

    /// Reverse lookup: find the session_id for a given chat_id, scoped to the
    /// forum topic (#215). Used by callback handlers to resolve the correct
    /// session for the chat where a button was pressed (instead of using the
    /// shared TUI session). `(chat_id, None)` matches the base/General session;
    /// `(chat_id, Some(tid))` matches that topic's own session.
    pub async fn chat_session(&self, chat_id: i64, topic_id: Option<i32>) -> Option<Uuid> {
        self.chat_sessions
            .lock()
            .await
            .get(&(chat_id, topic_id))
            .copied()
    }

    /// Register a pending file-save JoinHandle for a chat. The spawned task
    /// that downloads incoming media calls this so the tmp-photo pickup can
    /// await completion before scanning for files.
    pub async fn push_pending_save(&self, chat_id: i64, handle: tokio::task::JoinHandle<()>) {
        self.pending_file_saves
            .lock()
            .await
            .entry(chat_id)
            .or_default()
            .push(handle);
    }

    /// Drain all pending file-save handles for a chat and await each one.
    /// Called just before tmp-photo pickup to eliminate the race between
    /// fire-and-forget downloads and mention-triggered file lookups.
    pub async fn drain_pending_saves(&self, chat_id: i64) {
        let handles = self
            .pending_file_saves
            .lock()
            .await
            .remove(&chat_id)
            .unwrap_or_default();
        for h in handles {
            if let Err(e) = h.await {
                tracing::warn!("Telegram: pending file-save task panicked: {e}");
            }
        }
    }

    /// Register a pending approval channel by id.
    pub async fn register_pending_approval(&self, id: String, tx: oneshot::Sender<(bool, bool)>) {
        self.pending_approvals.lock().await.insert(id, tx);
    }

    /// Resolve a pending approval.
    /// `approved` — whether tool is allowed; `always` — auto-approve all future tools.
    /// Returns true if a pending approval existed.
    pub async fn resolve_pending_approval(&self, id: &str, approved: bool, always: bool) -> bool {
        if let Some(tx) = self.pending_approvals.lock().await.remove(id) {
            let _ = tx.send((approved, always));
            true
        } else {
            false
        }
    }

    /// Stash this session's optional follow-up suggestions (#597) so the tap
    /// handler can resolve `idx -> suggestion string`. Replaces any prior set.
    /// `host` is set when the keyboard was MERGED onto the final response
    /// bubble: the tap handler uses it to record the pick without erasing
    /// the answer text.
    pub(crate) async fn register_pending_followups(
        &self,
        session_id: Uuid,
        options: Vec<String>,
    ) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string()[..8].to_string();
        let count = options.len();
        let entry = PendingFollowupEntry {
            session_id,
            options,
            host: None,
        };
        self.pending_followups
            .lock()
            .await
            .insert(token.clone(), entry.clone());
        self.persist_followup(&token, &entry).await;
        // #1226: stash lifecycle used to be invisible — a register line lets
        // every later tap be mapped to the arm that minted its token.
        tracing::info!(
            "Telegram followups: registered stash token {token} for session \
             {session_id} ({count} options)"
        );
        token
    }

    /// Record the answer-bubble host after a successful merge-edit (#1217).
    pub(crate) async fn attach_followup_host(&self, token: &str, host: MergedHost) {
        let updated = {
            let mut map = self.pending_followups.lock().await;
            match map.get_mut(token) {
                Some(entry) => {
                    entry.host = Some(host);
                    Some(entry.clone())
                }
                None => None,
            }
        };
        if let Some(entry) = updated {
            self.persist_followup(token, &entry).await;
        }
    }

    /// Peek at the merged host WITHOUT consuming the stash (#31): the
    /// trailer-send decision in render_suggestions needs to know whether the
    /// RICH merge actually landed (the embedded trailer shipped with it) or
    /// the placement fell back to standalone (no embed ever hit the wire).
    /// `attach_followup_host` fires only on merge success, so Some(host) ==
    /// "the merge path landed".
    pub(crate) async fn peek_followup_host(&self, token: &str) -> Option<MergedHost> {
        self.pending_followups
            .lock()
            .await
            .get(token)
            .and_then(|e| e.host.clone())
    }

    /// #59: peek the dead-host record of a #597-cleared stash entry WITHOUT
    /// consuming it — the stale-shell tap needs the host SHAPE (rich body
    /// buttons vs glued/classic reply-markup) to pick the right strip; the
    /// record is forgotten only after the strip succeeds (`forget_stale_host`).
    pub(crate) async fn peek_stale_host(&self, token: &str) -> Option<MergedHost> {
        self.stale_hosts
            .lock()
            .await
            .iter()
            .find(|(t, _)| t == token)
            .map(|(_, h)| h.clone())
    }

    /// #59: current size of the stale-host map (bounded diagnostics).
    /// Test-support only — the production path logs via peek/forget, never the count.
    #[cfg(test)]
    pub(crate) async fn stale_host_count(&self) -> usize {
        self.stale_hosts.lock().await.len()
    }

    /// #59: forget a stale-host record once its dead keyboard is confirmed
    /// stripped — repeated taps on the same zombie stay log-attributable and
    /// cannot loop on a record whose buttons are already gone.
    pub(crate) async fn forget_stale_host(&self, token: &str) {
        let mut stale = self.stale_hosts.lock().await;
        let before = stale.len();
        stale.retain(|(t, _)| t != token);
        if stale.len() < before {
            tracing::info!("Telegram followups: forgot stripped stale-host record {token} (#59)");
        }
    }

    /// Forget an unused registration (buttons never landed).
    pub(crate) async fn drop_pending_followup(&self, token: &str) {
        let existed = self.pending_followups.lock().await.remove(token).is_some();
        self.forget_followup(token).await;
        if existed {
            tracing::info!("Telegram followups: dropped unplaced stash token {token}");
        }
    }

    /// Take a tapped follow-up suggestion by index, consuming the WHOLE set for
    /// the session (one suggestion starts one turn; the rest are stale). Returns
    /// the suggestion string plus the merged host (when the keyboard lived on
    /// the answer bubble), or None if nothing is pending or the index is out
    /// of range.
    pub(crate) async fn take_pending_followup(
        &self,
        token: &str,
        idx: usize,
    ) -> Option<(PendingFollowupEntry, String, usize)> {
        let entry = self.pending_followups.lock().await.remove(token)?;
        self.forget_followup(token).await;
        // #67: idx rides along — the tap-redraw rewrite runs in a spawned
        // task lexically outside the arm that binds idx, so the tapped
        // index must come back through the take's return tuple.
        entry
            .options
            .get(idx)
            .cloned()
            .map(|text| (entry, text, idx))
    }

    /// Drop this session's pending follow-up suggestions (the user sent their
    /// own message, so the buttons are stale).
    pub async fn clear_pending_followups(&self, session_id: Uuid) {
        // #59: before wiping, rescue the merged-host records into `stale_hosts`
        // — the #597 clear kills the stash, but rich-merged buttons keep
        // rendering inside the bubble body. Without the rescued shape the
        // stale-shell tap can only try a blind markup strip, which on a rich
        // host is a guaranteed "message is not modified" no-op (the zombie).
        let rescued: Vec<(String, MergedHost)> = {
            let mut map = self.pending_followups.lock().await;
            let mut rescued = Vec::new();
            map.retain(|token, e| {
                if e.session_id == session_id {
                    if let Some(h) = e.host.take() {
                        rescued.push((token.clone(), h));
                    }
                    false
                } else {
                    true
                }
            });
            rescued
        };
        let removed = rescued.len();
        if !rescued.is_empty() {
            let mut stale = self.stale_hosts.lock().await;
            for pair in rescued {
                stale.push_back(pair);
            }
            while stale.len() > STALE_HOST_CAP {
                if let Some((t, _)) = stale.pop_front() {
                    tracing::debug!(
                        "Telegram followups: evicted oldest stale-host record {t} (cap {STALE_HOST_CAP})"
                    );
                }
            }
        }
        let guard = self.followup_store.lock().await;
        if let Some(store) = guard.as_ref()
            && let Err(e) = store.delete_session(&session_id.to_string()).await
        {
            tracing::warn!("Telegram followup store session-clear failed for {session_id}: {e}");
        }
        drop(guard);
        if removed > 0 {
            // #1226: the #597 wipe used to leave zero log lines, which made
            // stale-shell taps impossible to explain from logs alone.
            tracing::info!(
                "Telegram followups: cleared {removed} stale stash entries for \
                 session {session_id} (#597 — user sent their own message)"
            );
        }
    }

    /// Store a cancel token for a session (before starting an agent call).
    ///
    /// Overwrites any prior token WITHOUT cancelling it (#652). The old code
    /// cancelled the previous token here "to prevent concurrent calls piling
    /// up", but that predates the atomic `try_begin_turn` gate (#501) which is
    /// now the single source of truth for one-turn-per-session. With that gate
    /// in place the cancel here is redundant and harmful: a near-simultaneous
    /// resend that reached this point would hard-cancel the user's genuinely
    /// in-flight turn, dropping the running request instead of queuing it
    /// (`is_turn_active` couldn't tell an old live turn apart from the new
    /// turn's own just-set guard, so it "cancelled in-flight" on routine
    /// resends). A completed turn leaves a stale, non-cancelled token
    /// (`remove_cancel_token` only reaps cancelled ones); overwriting it is
    /// enough. Genuine cancellation (/stop, provider swap) goes through
    /// `cancel_session`, not here.
    pub async fn store_cancel_token(&self, session_id: Uuid, token: CancellationToken) {
        self.cancel_tokens.lock().await.insert(session_id, token);
    }

    /// Cancel and remove the token for a session. Returns true if a token existed.
    pub async fn cancel_session(&self, session_id: Uuid) -> bool {
        if let Some(token) = self.cancel_tokens.lock().await.remove(&session_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Record who started a guest turn, so plain `/stop` typed anywhere by the
    /// same person can reach it (#2143). See `guest_owners`.
    pub(crate) fn register_guest_owner(&self, session_id: Uuid, user_id: i64) {
        if let Ok(mut owners) = self.guest_owners.lock() {
            owners.insert(session_id, user_id);
        }
    }

    /// Drop the ownership record once the guest turn is over, so a later
    /// `/stop` cannot cancel a finished turn by accident.
    pub(crate) fn forget_guest_owner(&self, session_id: Uuid) {
        if let Ok(mut owners) = self.guest_owners.lock() {
            owners.remove(&session_id);
        }
    }

    /// Cancel every live guest turn `user_id` started, returning how many were
    /// stopped (#2143).
    ///
    /// Plain `/stop` resolves its target from the message's own session, and a
    /// guest turn's session belongs to a chat the bot is not in, so that lookup
    /// could never find one. A caller who summoned the bot and then wanted it
    /// to stop had no reachable kill switch: the turn kept editing a stranger's
    /// reply until it ran out of query lifetime. This is the missing link.
    ///
    /// Only sessions with a turn in flight are touched, and the queue behind
    /// each is dropped the same way `stop_other_session` does it, so a stopped
    /// turn does not leave a follow-up to start a new one.
    pub(crate) async fn cancel_guest_turns_for(&self, user_id: i64) -> usize {
        let sessions: Vec<Uuid> = match self.guest_owners.lock() {
            Ok(owners) => owners
                .iter()
                .filter(|(_, owner)| **owner == user_id)
                .map(|(session_id, _)| *session_id)
                .collect(),
            Err(e) => {
                tracing::error!("Telegram: could not read guest turn owners: {e}");
                return 0;
            }
        };
        let mut cancelled = 0;
        for session_id in sessions {
            if !self.is_turn_active(session_id) {
                self.forget_guest_owner(session_id);
                continue;
            }
            if self.cancel_session(session_id).await {
                self.drain_queued_items(session_id);
                self.forget_guest_owner(session_id);
                cancelled += 1;
                tracing::info!(
                    "Telegram guest: /stop from the caller cancelled the guest turn on session \
                     {session_id}"
                );
            }
        }
        cancelled
    }

    /// Session ids with a turn in flight right now, sorted for stable output.
    ///
    /// `active_turns` is the authority: `cancel_tokens` keeps a completed,
    /// non-cancelled token until the next turn, so its keys alone would report
    /// idle sessions as running. Used by `/stop <id>` to resolve a target and
    /// by the ambiguous-prose guard to know whether a targeted stop is even
    /// relevant (#2108).
    pub(crate) fn running_session_ids(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .active_turns
            .lock()
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        ids.sort();
        ids
    }

    /// Stop another session's turn from here: cancel its token, drop whatever
    /// was queued behind it, and report which of the three states it was in
    /// (#2108).
    ///
    /// The order matters. `is_turn_active` is read BEFORE the drain, because
    /// draining is what makes the queue empty and would otherwise erase the
    /// evidence that this session had anything to cancel.
    pub(crate) async fn stop_session_target(&self, session_id: Uuid) -> TargetStop {
        let was_running = self.is_turn_active(session_id);
        let token_cancelled = self.cancel_session(session_id).await;
        let dropped = self.drain_queued_items(session_id).len();
        if was_running || token_cancelled {
            TargetStop::Delivered {
                queued_dropped: dropped,
            }
        } else if dropped > 0 {
            TargetStop::Queued { dropped }
        } else {
            TargetStop::Idle
        }
    }

    /// Remove the cancel token after the agent call completes (cleanup).
    /// Only removes if the stored token is already cancelled — this prevents a
    /// finishing old call from accidentally removing a newer call's live token.
    pub async fn remove_cancel_token(&self, session_id: Uuid) {
        let mut tokens = self.cancel_tokens.lock().await;
        if let Some(token) = tokens.get(&session_id)
            && token.is_cancelled()
        {
            tokens.remove(&session_id);
        }
    }

    /// The persistent plan-card message id + last rendered signature for a
    /// session, if one is live (#580).
    pub(crate) async fn plan_card(
        &self,
        session_id: Uuid,
    ) -> Option<(teloxide::types::MessageId, String)> {
        if let Some(hit) = self.plan_cards.lock().await.get(&session_id).cloned() {
            return Some(hit);
        }
        // Miss: either no card, or this process just started and the map is
        // empty. Rehydrating here rather than scanning every session at boot
        // means the cost is paid once, only for sessions that actually ask
        // (#809).
        let stored = {
            let guard = self.plan_card_store.lock().await;
            let repo = guard.as_ref()?;
            match repo.get(&session_id.to_string()).await {
                Ok(row) => row?,
                Err(e) => {
                    tracing::warn!("Plan-card lookup failed for session {session_id}: {e}");
                    return None;
                }
            }
        };
        let card = (
            teloxide::types::MessageId(stored.message_id as i32),
            stored.signature,
        );
        self.plan_cards
            .lock()
            .await
            .insert(session_id, card.clone());
        tracing::info!("Recovered plan card for session {session_id} after restart");
        Some(card)
    }

    /// In-memory-only card check (#62): whether a card message id is live in
    /// THIS process. Unlike [`Self::plan_card`] it never rehydrates from the
    /// store, so it is cheap enough to gate the per-settle restick claim —
    /// cardless settles must not spend the shared sticky budget (#1150) they
    /// do not need, or the flow-block restick would starve for 15s after
    /// every settle. A card tracked by a previous process but not yet
    /// re-posted here skips at most one restick: the refresh below re-posted
    /// and re-tracked it, so the next settle resticks normally.
    pub(crate) async fn plan_card_cached(
        &self,
        session_id: Uuid,
    ) -> Option<(teloxide::types::MessageId, String)> {
        self.plan_cards.lock().await.get(&session_id).cloned()
    }

    /// The lock serialising card writes for a session (#822).
    ///
    /// Returned as an `Arc` so the caller holds it across its API calls; the
    /// inner map lock is released immediately, so acquiring one session's lock
    /// never blocks another's.
    pub(crate) async fn plan_card_lock(
        &self,
        session_id: Uuid,
    ) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        self.plan_card_locks
            .lock()
            .await
            .entry(session_id)
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Are card writes for this session currently suppressed (#814)?
    pub(crate) async fn plan_card_suppressed(&self, session_id: Uuid) -> bool {
        let mut map = self.plan_card_backoff.lock().await;
        match map.get(&session_id) {
            Some(until) if *until > std::time::Instant::now() => true,
            Some(_) => {
                // Window elapsed; drop the entry so the map cannot grow
                // without bound across long-lived sessions.
                map.remove(&session_id);
                false
            }
            None => false,
        }
    }

    /// Suppress card writes for this session for `wait`.
    pub(crate) async fn suppress_plan_card(&self, session_id: Uuid, wait: std::time::Duration) {
        self.plan_card_backoff
            .lock()
            .await
            .insert(session_id, std::time::Instant::now() + wait);
    }

    /// Give the session binding map durable backing. Called at startup (#170).
    pub(crate) async fn set_binding_store(&self, repo: crate::db::SessionBindingRepository) {
        *self.binding_store.lock().await = Some(repo);
    }

    /// Set the message enqueue callback for background task routing (#170).
    pub(crate) async fn set_enqueue_callback(
        &self,
        cb: Option<crate::brain::agent::service::MessageEnqueueCallback>,
    ) {
        *self.enqueue_callback.lock().await = cb;
    }

    /// Authoritative method to bind a session to a chat/topic (#170).
    /// Updates in-memory mappings, persistent database storage, session delivery route,
    /// turn probe, and channel ownership probe.
    pub async fn bind_session_topic(
        self: &std::sync::Arc<Self>,
        session_id: Uuid,
        chat_id: i64,
        topic_id: Option<i32>,
    ) -> Result<(), String> {
        // 1. In-memory mappings + sync ownership mirror
        self.register_session_chat(session_id, chat_id, topic_id)
            .await;

        // 2. Persistent storage
        let store = self.binding_store.lock().await.clone();
        if let Some(repo) = store {
            repo.upsert(
                session_id.to_string(),
                "telegram",
                &chat_id.to_string(),
                topic_id,
            )
            .await
            .map_err(|e| format!("could not persist session binding for {session_id}: {e}"))?;
        }

        // 3. Session routing
        let enqueue = self.enqueue_callback.lock().await.clone();
        crate::brain::agent::service::session_routes::claim_for_channel(session_id, enqueue);

        // 4. Probes
        {
            let probe_state = std::sync::Arc::clone(self);
            crate::brain::agent::service::session_routes::register_turn_probe(
                session_id,
                std::sync::Arc::new(move || probe_state.is_turn_active(session_id)),
            );
        }
        {
            let probe_state = std::sync::Arc::clone(self);
            crate::brain::agent::service::session_routes::register_channel_owner_probe(
                session_id,
                std::sync::Arc::new(move || probe_state.channel_ownership_of(session_id)),
            );
        }

        Ok(())
    }

    /// Give the plan-card map durable backing. Called once at startup.
    pub(crate) async fn set_plan_card_store(
        &self,
        repo: crate::db::repository::PlanCardRepository,
    ) {
        *self.plan_card_store.lock().await = Some(repo);
    }

    /// Give the plan-card path a `channel_messages` writer (#1684). Called once
    /// at startup, next to `set_plan_card_store`: the card is a bot bubble like
    /// any other, and recording it needs the same repository the reply path
    /// already uses. Surfaces built without a database leave it `None`.
    pub(crate) async fn set_channel_message_store(
        &self,
        repo: crate::db::repository::ChannelMessageRepository,
    ) {
        *self.channel_message_store.lock().await = Some(repo);
    }

    /// The `channel_messages` writer, when this surface has one.
    pub(crate) async fn channel_messages(
        &self,
    ) -> Option<crate::db::repository::ChannelMessageRepository> {
        self.channel_message_store.lock().await.clone()
    }

    /// Give the follow-up stash durable backing and hydrate what the last
    /// process armed (#1226 item 3). Called once at startup. In-memory
    /// entries win over rows (the map is authoritative); a hydration error
    /// only degrades to the old restart-orphan behaviour.
    pub(crate) async fn set_followup_store(
        &self,
        repo: crate::db::repository::PendingFollowupRepository,
    ) {
        match repo.load_all().await {
            Ok(rows) => {
                let mut restored = 0usize;
                let mut map = self.pending_followups.lock().await;
                for row in rows {
                    let Ok(sid) = Uuid::parse_str(&row.session_id) else {
                        continue;
                    };
                    let host = row.host.map(|h| MergedHost {
                        message_id: MessageId(h.message_id as i32),
                        html: h.html,
                        rich: h.rich,
                        // Port note (#79 p4): the markdown column is NULL
                        // for rows persisted before this change, so hydrate
                        // reads the plane straight from the row.
                        markdown: h.markdown,
                    });
                    let token = row.token;
                    let entry = PendingFollowupEntry {
                        session_id: sid,
                        options: row.options,
                        host,
                    };
                    if let std::collections::hash_map::Entry::Vacant(slot) = map.entry(token) {
                        slot.insert(entry);
                        restored += 1;
                    }
                }
                tracing::info!(
                    target: "telegram",
                    "Followup store hydrated: {restored} keyboard(s) restored"
                );
            }
            Err(e) => tracing::warn!("Followup store hydration failed: {e}"),
        }
        *self.followup_store.lock().await = Some(repo);
    }

    /// Mirror one stash entry into the durable store. Storage failures are
    /// logged and swallowed: the map stays authoritative, and a lost row can
    /// only fall back to the stale-tap strip, never break a turn.
    async fn persist_followup(&self, token: &str, entry: &PendingFollowupEntry) {
        let row = crate::db::repository::PendingFollowup {
            token: token.to_string(),
            session_id: entry.session_id.to_string(),
            options: entry.options.clone(),
            host: entry
                .host
                .as_ref()
                .map(|h| crate::db::repository::FollowupHost {
                    message_id: i64::from(h.message_id.0),
                    html: h.html.clone(),
                    rich: h.rich,
                    markdown: h.markdown.clone(),
                }),
        };
        let guard = self.followup_store.lock().await;
        if let Some(store) = guard.as_ref()
            && let Err(e) = store.save(&row).await
        {
            tracing::warn!("Telegram followup store save failed for {token}: {e}");
        }
    }

    /// Forget one token's durable row (tap consumed it or buttons never landed).
    async fn forget_followup(&self, token: &str) {
        let guard = self.followup_store.lock().await;
        if let Some(store) = guard.as_ref()
            && let Err(e) = store.delete(token).await
        {
            tracing::warn!("Telegram followup store delete failed for {token}: {e}");
        }
    }

    /// Track the plan-card message id + its rendered signature for a session.
    pub(crate) async fn set_plan_card(
        &self,
        session_id: Uuid,
        chat_id: teloxide::types::ChatId,
        thread_id: Option<teloxide::types::ThreadId>,
        msg_id: teloxide::types::MessageId,
        signature: String,
    ) {
        self.plan_cards
            .lock()
            .await
            .insert(session_id, (msg_id, signature.clone()));
        // Persist alongside, so a restart can still find and update THIS
        // message instead of posting a second card below the stale one (#809).
        let guard = self.plan_card_store.lock().await;
        if let Some(repo) = guard.as_ref()
            && let Err(e) = repo
                .set(crate::db::repository::PlanCard {
                    session_id: session_id.to_string(),
                    chat_id: chat_id.0,
                    thread_id: thread_id.map(|t| t.0.0 as i64),
                    message_id: msg_id.0 as i64,
                    signature,
                })
                .await
        {
            tracing::warn!("Failed to persist plan card for session {session_id}: {e}");
        }
    }

    /// Stop tracking the plan card and return the id that was tracked, if any
    /// (the caller deletes the message). Used on discard / plan completion.
    pub(crate) async fn take_plan_card(
        &self,
        session_id: Uuid,
    ) -> Option<teloxide::types::MessageId> {
        // Resolve through `plan_card` so a card tracked by a PREVIOUS process
        // is still removable. Without that, a restart left the stale card
        // undeletable and it lingered in the chat forever (#809).
        let existing = self.plan_card(session_id).await.map(|(mid, _)| mid);
        self.plan_cards.lock().await.remove(&session_id);
        let guard = self.plan_card_store.lock().await;
        if let Some(repo) = guard.as_ref()
            && let Err(e) = repo.delete(&session_id.to_string()).await
        {
            tracing::warn!("Failed to clear plan card for session {session_id}: {e}");
        }
        existing
    }

    /// Mark `session_id` as having a turn in flight, returning an RAII guard
    /// that clears the flag on drop — normal return, early return, panic, or
    /// cancellation — so a crashed turn never leaves a session looking busy
    /// (which would silently queue every future reaction forever). #302 Stage 2.
    ///
    /// Deliberately does NOT reuse `cancel_tokens`: `remove_cancel_token` keeps
    /// a completed (non-cancelled) token in the map until the next turn, so
    /// `cancel_tokens.contains_key` yields false positives for idle sessions.
    pub(crate) fn mark_turn_active(
        self: &std::sync::Arc<Self>,
        session_id: Uuid,
    ) -> ActiveTurnGuard {
        if let Ok(mut set) = self.active_turns.lock() {
            set.insert(session_id);
        }
        ActiveTurnGuard {
            state: self.clone(),
            session_id,
        }
    }

    /// Atomically begin a turn for `session_id` (#501). Under ONE lock:
    /// returns `Some(guard)` and marks the session active when no turn is
    /// running, or `None` when a turn already is (the caller then treats the
    /// message as a mid-turn follow-up). This closes the check-then-act race
    /// where `is_turn_active` was checked ~600 lines before `mark_turn_active`
    /// ran, so a follow-up arriving in that window read the session as idle
    /// and forked a second concurrent turn instead of enqueuing.
    pub(crate) fn try_begin_turn(
        self: &std::sync::Arc<Self>,
        session_id: Uuid,
    ) -> Option<ActiveTurnGuard> {
        let mut set = self.active_turns.lock().ok()?;
        if !set.insert(session_id) {
            // Already present: a turn is in flight for this session.
            return None;
        }
        Some(ActiveTurnGuard {
            state: self.clone(),
            session_id,
        })
    }

    /// True while a turn is in flight for `session_id`.
    pub(crate) fn is_turn_active(&self, session_id: Uuid) -> bool {
        self.active_turns
            .lock()
            .map(|s| s.contains(&session_id))
            .unwrap_or(false)
    }

    /// #61: register this turn's live flow roll, returning the drop-guard
    /// that unregisters it. Takes `&Arc<Self>` because the guard must be
    /// able to reach the registry after the turn's stack is gone. One live
    /// roll per session by construction — but a re-register CAN precede
    /// the previous turn's guard drop (turn overlap windows), and a naive
    /// guard drop would then evict the NEW turn's entry. So the guard
    /// holds its own streaming Arc and its drop removes the entry only if
    /// the stored handle's streaming Arc is the same allocation
    /// (`Arc::ptr_eq`) — a stale guard is a no-op against the successor.
    pub(crate) fn register_live_flow(
        self: &std::sync::Arc<Self>,
        session_id: Uuid,
        handle: LiveFlowHandle,
    ) -> LiveFlowRegistration {
        if let Ok(mut map) = self.live_flows.lock() {
            map.insert(session_id, handle.clone());
        }
        LiveFlowRegistration {
            state: std::sync::Arc::clone(self),
            session_id,
            streaming: handle.streaming,
        }
    }

    /// #61: clone out the live-flow handle for `session_id`, if one is
    /// registered. Registration proves a turn is in flight, NOT that the
    /// roll block is still open — the caller must re-check
    /// `open_group_msg_id.is_some()` under the streaming lock before
    /// folding (the roll can close between lookup and fold).
    pub(crate) fn live_flow(&self, session_id: Uuid) -> Option<LiveFlowHandle> {
        self.live_flows
            .lock()
            .ok()
            .and_then(|map| map.get(&session_id).cloned())
    }

    /// #61: explicit unregister — used by tests and by turns that defuse
    /// the drop-guard (rare; the guard's Drop normally suffices).
    #[cfg(test)]
    pub(crate) fn unregister_live_flow(&self, session_id: Uuid) {
        if let Ok(mut map) = self.live_flows.lock() {
            map.remove(&session_id);
        }
    }

    /// #61 fold-dedupe cap: max fingerprints retained per session.
    /// `pub(crate)` so the tests under `src/tests/` can pin the window against
    /// the real constants instead of restating their values.
    pub(crate) const NOTIFY_FOLD_DEDUP_CAP: usize = 64;
    /// #61 fold-dedupe TTL: fingerprints expire after this window, so a
    /// genuine re-send hours later still folds.
    pub(crate) const NOTIFY_FOLD_DEDUP_TTL: std::time::Duration =
        std::time::Duration::from_secs(30 * 60);

    /// #61 fold-dedupe (Alexey 2026-09-06): atomically check-and-record a
    /// folded notify fingerprint for the session. Returns `true` when the
    /// fingerprint was ALREADY recorded within the TTL window — a
    /// duplicate delivery the caller suppresses; `false` records it and
    /// grants fold permission. Check and record share one lock scope, so
    /// two racing folds of the same notify cannot both pass.
    pub(crate) fn note_notify_fold(&self, session_id: Uuid, fingerprint: u64) -> bool {
        self.note_notify_fold_at(session_id, fingerprint, std::time::Instant::now())
    }

    /// `note_notify_fold` with an injected clock — the test seam.
    pub(crate) fn note_notify_fold_at(
        &self,
        session_id: Uuid,
        fingerprint: u64,
        now: std::time::Instant,
    ) -> bool {
        let mut map = self
            .notify_fold_dedup
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let duplicate = match map.get_mut(&session_id) {
            Some(entry) => {
                entry.retain(|(_, at)| {
                    now.checked_duration_since(*at)
                        .is_some_and(|age| age < Self::NOTIFY_FOLD_DEDUP_TTL)
                });
                if entry.iter().any(|(fp, _)| *fp == fingerprint) {
                    true
                } else {
                    entry.push_back((fingerprint, now));
                    if entry.len() > Self::NOTIFY_FOLD_DEDUP_CAP {
                        entry.pop_front();
                    }
                    false
                }
            }
            None => {
                map.insert(
                    session_id,
                    std::collections::VecDeque::from([(fingerprint, now)]),
                );
                false
            }
        };
        if duplicate && map.get(&session_id).is_some_and(|e| e.is_empty()) {
            map.remove(&session_id);
        }
        duplicate
    }

    /// Enqueue a mid-turn reaction message for injection into the running loop.
    pub(crate) fn enqueue_reaction(
        &self,
        session_id: Uuid,
        msg: crate::brain::agent::QueuedUserMessage,
    ) {
        self.enqueue_item(session_id, QueuedOrigin::Reaction, msg);
    }

    /// Enqueue the result of DETACHED work — a background command or a
    /// sub-agent — for injection into the running loop.
    ///
    /// Same queue, different origin, because the two need different treatment
    /// if the loop ends before draining them (#1213). A reaction is an
    /// acknowledgement and a single toolless round answers it fine. A
    /// detached result is evidence the agent is expected to ACT on, and
    /// answering it without a tool registry produces a turn that announces
    /// follow-up work it is structurally unable to perform.
    pub(crate) fn enqueue_detached_result(
        &self,
        session_id: Uuid,
        msg: crate::brain::agent::QueuedUserMessage,
    ) {
        self.enqueue_item(session_id, QueuedOrigin::DetachedWork, msg);
    }

    fn enqueue_item(
        &self,
        session_id: Uuid,
        origin: QueuedOrigin,
        msg: crate::brain::agent::QueuedUserMessage,
    ) {
        // Durable twin (#111): the in-memory queue dies with the process, the
        // row survives it. Best-effort, fire-and-forget; a no-runtime context
        // (sync tests) skips the row and the queue alone holds the message.
        crate::brain::agent::service::notify_queue::persist(session_id, &msg);
        match self.pending_reactions.lock() {
            Ok(mut map) => map
                .entry(session_id)
                .or_default()
                .push_back(QueuedItem { origin, msg }),
            Err(e) => {
                // The message is gone at this point, and for detached work
                // that means a result nobody will ever see.
                tracing::error!(
                    "Telegram: could not queue a {origin:?} message for session {session_id}, \
                     it is dropped: {e}"
                );
            }
        }
    }

    /// Drain EVERYTHING queued for `session_id` (FIFO), folded into ONE
    /// joined message (#1837).
    ///
    /// The tool loop invokes the queue callback once per round-end, so a
    /// pop-one drain fragmented a burst of rapid follow-ups into N injections,
    /// N model calls and N separate replies, all after the user's last
    /// message. The shared/TUI callback (`cli/ui.rs`) already takes the whole
    /// stack and joins it; this path now does the same via
    /// [`QueuedUserMessage::join`], so one round-end answers the whole burst
    /// in a single injection. Returns `None` when nothing is queued.
    pub(crate) fn drain_reaction(
        &self,
        session_id: Uuid,
    ) -> Option<crate::brain::agent::QueuedUserMessage> {
        let mut map = self.pending_reactions.lock().ok()?;
        // Removing the entry up front is the "empty queue leaves no husk"
        // behaviour the old pop-one path got with its is_empty check.
        let queue = map.remove(&session_id)?;
        // Delivered into the running loop (#111): the durable twin is
        // redundant from here, for EVERY item. A failed clear costs a
        // next-boot duplicate, never a loss.
        for item in &queue {
            crate::brain::agent::service::notify_queue::clear_on_delivery(session_id, &item.msg);
        }
        let msgs: Vec<_> = queue.into_iter().map(|i| i.msg).collect();
        crate::brain::agent::QueuedUserMessage::join(&msgs)
    }

    /// Take everything still queued for `session_id`, WITH origins.
    ///
    /// Used by the end-of-turn flush, which is the one consumer that has to
    /// tell the two apart: by then the loop has exited, so whatever is left
    /// needs a destination chosen by what it is (#1213).
    pub(crate) fn drain_queued_items(&self, session_id: Uuid) -> Vec<QueuedItem> {
        match self.pending_reactions.lock() {
            Ok(mut map) => map.remove(&session_id).map(Vec::from).unwrap_or_default(),
            Err(e) => {
                tracing::error!(
                    "Telegram: could not drain queued messages for session {session_id}: {e}"
                );
                Vec::new()
            }
        }
    }

    /// A [`MessageQueueCallback`](crate::brain::agent::MessageQueueCallback) that
    /// drains this state's pending reactions, keyed per session. Wired into the
    /// Telegram `AgentService` so the tool loop injects a queued reaction between
    /// rounds (the same rail the TUI uses for follow-up messages).
    pub(crate) fn reaction_queue_callback(
        self: &std::sync::Arc<Self>,
    ) -> crate::brain::agent::MessageQueueCallback {
        let state = self.clone();
        std::sync::Arc::new(move |session_id: Uuid| {
            let state = state.clone();
            Box::pin(async move { state.drain_reaction(session_id) })
        })
    }

    /// Buffer a photo marker for batching. Returns the current buffer size.
    /// Photos are accumulated per (chat_id, user_id, media_group_id) until the debounce timer expires.
    /// Only called for album photos (media_group_id is Some).
    pub async fn buffer_photo(
        &self,
        chat_id: i64,
        user_id: i64,
        media_group_id: &str,
        img_marker: String,
        caption: Option<String>,
    ) -> usize {
        let key = (chat_id, user_id, media_group_id.to_string());
        let mut buffer = self.photo_buffer.lock().await;
        buffer
            .entry(key.clone())
            .or_default()
            .push((img_marker, caption));
        buffer.get(&key).map(|v| v.len()).unwrap_or(0)
    }

    /// Reset the photo debounce timer for a (chat_id, user_id, media_group_id).
    /// Cancels any existing timer and creates a new one.
    /// Returns a CancellationToken that will be cancelled if another photo arrives.
    /// Only called for album photos (media_group_id is Some).
    pub async fn reset_photo_debounce(
        &self,
        chat_id: i64,
        user_id: i64,
        media_group_id: &str,
    ) -> CancellationToken {
        let key = (chat_id, user_id, media_group_id.to_string());
        let token = CancellationToken::new();

        let mut debounce = self.photo_debounce.lock().await;
        if let Some(old) = debounce.remove(&key) {
            old.cancel();
        }
        debounce.insert(key, token.clone());

        token
    }

    /// Wait for the photo debounce period (3 seconds) or until cancelled.
    /// Returns true if the timer expired (no new photos), false if cancelled.
    pub async fn wait_photo_debounce(&self, token: CancellationToken) -> bool {
        tokio::select! {
            _ = token.cancelled() => false,
            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => true,
        }
    }

    /// Drain all buffered photos for a (chat_id, user_id, media_group_id).
    /// Returns the vector of (img_marker, caption) tuples, or empty if none buffered.
    /// Only called for album photos (media_group_id is Some).
    pub async fn drain_photo_buffer(
        &self,
        chat_id: i64,
        user_id: i64,
        media_group_id: &str,
    ) -> Vec<(String, Option<String>)> {
        let key = (chat_id, user_id, media_group_id.to_string());
        let mut buffer = self.photo_buffer.lock().await;
        buffer.remove(&key).unwrap_or_default()
    }

    /// Clean up the debounce token after processing.
    /// Only called for album photos (media_group_id is Some).
    pub async fn cleanup_photo_debounce(&self, chat_id: i64, user_id: i64, media_group_id: &str) {
        let key = (chat_id, user_id, media_group_id.to_string());
        self.photo_debounce.lock().await.remove(&key);
    }

    /// Buffer a fragment of a possibly-split message. Returns how many are held.
    pub async fn buffer_text(&self, chat_id: i64, user_id: i64, text: String) -> usize {
        let mut buffer = self.text_buffer.lock().await;
        let frags = buffer.entry((chat_id, user_id)).or_default();
        frags.push(text);
        frags.len()
    }

    /// Start (or restart) the wait for another fragment from this sender.
    ///
    /// Cancels any timer already running for the pair, so the window measures
    /// the gap between fragments rather than the age of the first one.
    pub async fn reset_text_debounce(&self, chat_id: i64, user_id: i64) -> CancellationToken {
        let token = CancellationToken::new();
        let mut debounce = self.text_debounce.lock().await;
        if let Some(old) = debounce.insert((chat_id, user_id), token.clone()) {
            old.cancel();
        }
        token
    }

    /// Wait for the gap to pass, or until another fragment cancels it.
    /// `true` means the window expired and the message is complete.
    ///
    /// Short on purpose: the client emits the pieces back to back — measured at
    /// 56ms apart — so this only has to outlast network jitter, and it is paid
    /// solely by messages already at the split threshold.
    pub async fn wait_text_debounce(&self, token: CancellationToken) -> bool {
        tokio::select! {
            _ = token.cancelled() => false,
            _ = tokio::time::sleep(std::time::Duration::from_millis(1500)) => true,
        }
    }

    /// Take every buffered fragment for this sender, in arrival order.
    pub async fn drain_text_buffer(&self, chat_id: i64, user_id: i64) -> Vec<String> {
        self.text_buffer
            .lock()
            .await
            .remove(&(chat_id, user_id))
            .unwrap_or_default()
    }

    /// Drop the debounce token once the fragments have been dispatched.
    pub async fn cleanup_text_debounce(&self, chat_id: i64, user_id: i64) {
        self.text_debounce.lock().await.remove(&(chat_id, user_id));
    }

    // ── Cowork state management ──────────────────────────────────────────

    /// Start a new /cowork conversation for a user.
    pub async fn start_cowork(&self, user_id: i64, chat_id: i64, session_id: String) {
        let state = cowork::CoworkState::new(user_id, chat_id, session_id.clone());
        self.cowork_sessions
            .lock()
            .await
            .insert(session_id, state.clone());
        self.cowork_conversations
            .lock()
            .await
            .insert(user_id, state);
    }

    /// Get the active /cowork state for a user (if any).
    pub async fn get_cowork_state(&self, user_id: i64) -> Option<cowork::CoworkState> {
        self.cowork_conversations
            .lock()
            .await
            .get(&user_id)
            .cloned()
    }

    /// Take (remove) a cowork state by session_id. Used when bot joins a group.
    pub async fn take_cowork_by_session(&self, session_id: &str) -> Option<cowork::CoworkState> {
        let state = self.cowork_sessions.lock().await.remove(session_id);
        if let Some(ref s) = state {
            self.cowork_conversations.lock().await.remove(&s.user_id);
        }
        state
    }

    /// Clear the cowork state for a user.
    pub async fn clear_cowork(&self, user_id: i64) {
        if let Some(state) = self.cowork_conversations.lock().await.remove(&user_id) {
            self.cowork_sessions.lock().await.remove(&state.session_id);
        }
    }

    // ── Active sender tracking (#244) ───────────────────────────────────

    /// Record a message sender for a chat. Returns the total number of
    /// unique senders after adding this one. Set never shrinks.
    pub async fn track_active_sender(&self, chat_id: i64, user_id: i64) -> usize {
        let mut map = self.active_senders.lock().await;
        let set = map.entry(chat_id).or_default();
        set.insert(user_id);
        set.len()
    }

    // ── Directory browser state ─────────────────────────────────────────

    /// Set the browsing path for a chat+topic (called on /cd and navigation).
    pub async fn set_dir_browser(
        &self,
        chat_id: i64,
        topic_id: Option<i32>,
        path: String,
        filter: Option<String>,
    ) {
        self.dir_browsers
            .lock()
            .await
            .insert((chat_id, topic_id), (path, filter));
    }

    /// Get the current browsing path and filter for a chat+topic.
    pub async fn get_dir_browser(
        &self,
        chat_id: i64,
        topic_id: Option<i32>,
    ) -> Option<(String, Option<String>)> {
        self.dir_browsers
            .lock()
            .await
            .get(&(chat_id, topic_id))
            .cloned()
    }

    /// Clear the directory browser state for a chat+topic (after confirming).
    pub async fn clear_dir_browser(&self, chat_id: i64, topic_id: Option<i32>) {
        self.dir_browsers.lock().await.remove(&(chat_id, topic_id));
    }

    /// Set the profile-create flow state for a chat.
    pub async fn set_prof_create(&self, chat_id: i64, active: bool) {
        if active {
            self.prof_create_states.lock().await.insert(chat_id, true);
        } else {
            self.prof_create_states.lock().await.remove(&chat_id);
        }
    }

    /// Check if a chat is in the profile-create flow.
    pub async fn is_prof_create(&self, chat_id: i64) -> bool {
        self.prof_create_states
            .lock()
            .await
            .get(&chat_id)
            .copied()
            .unwrap_or(false)
    }

    /// Clear the profile-create flow state.
    pub async fn clear_prof_create(&self, chat_id: i64) {
        self.prof_create_states.lock().await.remove(&chat_id);
    }

    /// All topic-scoped session keys for a chat (#148): the topic ids the
    /// reverse map currently holds for `chat_id`, used by the target resolver's
    /// multi-topic ambiguity rule (a bare `oc://telegram/<chat>` on a forum with
    /// several topic sessions must refuse and list the topic URLs, never guess).
    /// Empty when the chat is unbound or a DM / non-forum group.
    pub async fn topic_sessions_for_chat(&self, chat_id: i64) -> Vec<i32> {
        let map = self.chat_sessions.lock().await;
        let mut topics: Vec<i32> = map
            .keys()
            .filter_map(|(c, t)| if *c == chat_id { *t } else { None })
            .collect();
        topics.sort();
        topics
    }
}
