//! WhatsApp Agent
//!
//! Single bot instance — handles pairing, reconnection, and message processing.
//! Onboarding subscribes to QR/connected events via WhatsAppState.

use super::WhatsAppState;
use super::handler;
use super::history;
use super::newsletter;
use crate::brain::agent::AgentService;
use crate::channels::owner_alert::OwnerAlert;
use crate::config::Config;
use crate::db::ChannelMessageRepository;
use crate::services::{ServiceContext, SessionService};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use super::store::Store;
use wacore::types::events::{ConnectFailureReason, Event, TempBanReason};
use whatsapp_rust::TokioRuntime;
use whatsapp_rust::bot::Bot;
use whatsapp_rust_tokio_transport::TokioWebSocketTransportFactory;
use whatsapp_rust_ureq_http_client::UreqHttpClient;

/// #1999: a ban, an account lock or a connect failure used to land in the
/// debug-only catch-all, so the server's one-time reason never reached a
/// human. Each formatter turns one payload into the line the log and the
/// onboarding UI get. They are pure so the mapping is pinned without a live
/// socket; the event arms wire them in.
pub(crate) fn logout_notice(reason: &ConnectFailureReason, server_header: Option<&str>) -> String {
    let mut msg = format!("WhatsApp logged out (reason: {reason:?})");
    if let Some(header) = server_header {
        msg.push_str(&format!(": {header}"));
    }
    msg
}

pub(crate) fn temp_ban_notice(
    code: &TempBanReason,
    expire: chrono::Duration,
    message: Option<&str>,
    url: Option<&str>,
) -> String {
    let mut msg = format!(
        "WhatsApp temporarily banned ({code:?}); usable again in {} min",
        expire.num_minutes().max(1)
    );
    if let Some(m) = message {
        msg.push_str(&format!(": {m}"));
    }
    if let Some(u) = url {
        msg.push_str(&format!(" Appeal: {u}"));
    }
    msg
}

pub(crate) fn connect_failure_notice(
    reason: &ConnectFailureReason,
    message: Option<&str>,
) -> String {
    let mut msg = format!("WhatsApp connect failure ({reason:?})");
    if let Some(m) = message {
        msg.push_str(&format!(": {m}"));
    }
    msg
}

/// The owner-facing line for a temporary ban (#1999). Every ban is alertable:
/// the server has already decided this account is restricted.
pub(crate) fn ban_alert(
    code: &TempBanReason,
    expire: chrono::Duration,
    url: Option<&str>,
) -> String {
    let mut msg = format!(
        "WhatsApp temporary ban ({code:?}), expires in about {} h.",
        expire.num_hours()
    );
    if let Some(url) = url {
        msg.push_str(&format!(" Appeal: {url}"));
    }
    msg
}

/// The owner-facing line for an account lock or a connect-time temporary ban
/// (#1999). `None` for everything else, including a routine logout and a manual
/// unlink (`device_removed`), which need no alert.
pub(crate) fn lock_alert(reason: &ConnectFailureReason) -> Option<String> {
    match reason {
        ConnectFailureReason::AccountLocked => Some(
            "WhatsApp account locked (403). Do not re-pair this number: the lock is server-side. Check the account in the WhatsApp app.".to_string(),
        ),
        ConnectFailureReason::TempBanned => Some(
            "WhatsApp temporary ban at connect (402). Messages will not go through until it expires.".to_string(),
        ),
        _ => None,
    }
}

/// WhatsApp agent that forwards messages to the AgentService
pub struct WhatsAppAgent {
    agent_service: Arc<AgentService>,
    session_service: SessionService,
    shared_session_id: Arc<Mutex<Option<Uuid>>>,
    whatsapp_state: Arc<WhatsAppState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    channel_msg_repo: ChannelMessageRepository,
    owner_alert: OwnerAlert,
}

impl WhatsAppAgent {
    pub fn new(
        agent_service: Arc<AgentService>,
        service_context: ServiceContext,
        shared_session_id: Arc<Mutex<Option<Uuid>>>,
        whatsapp_state: Arc<WhatsAppState>,
        config_rx: tokio::sync::watch::Receiver<Config>,
        channel_msg_repo: ChannelMessageRepository,
        owner_alert: OwnerAlert,
    ) -> Self {
        Self {
            agent_service,
            session_service: SessionService::new(service_context),
            shared_session_id,
            whatsapp_state,
            config_rx,
            channel_msg_repo,
            owner_alert,
        }
    }

    /// Start as a background task. Returns JoinHandle.
    /// Always starts — if no session exists, emits QR events for onboarding.
    /// If already paired, reconnects and handles messages.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        let owner_alert = self.owner_alert.clone();
        tokio::spawn(async move {
            // Spawn the outbound rate-limit drainer exactly once per
            // process (#1407). The drainer snapshots the rate_limit
            // config here; later config changes reach the inline gate
            // paths immediately (they re-read config per turn) but the
            // drainer keeps this snapshot until restart (v1 tradeoff,
            // documented in rate_limit.rs).
            let rl_cfg = self.config_rx.borrow().channels.whatsapp.rate_limit.clone();
            super::rate_limit::spawn_drainer(self.whatsapp_state.clone(), rl_cfg);

            let db_path = crate::config::opencrabs_home()
                .join("whatsapp")
                .join("session.db");

            // Ensure parent directory exists
            if let Some(parent) = db_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            // `with_backend` takes the backend by value now (it wraps it
            // internally); the old blanket `Backend for Arc<T>` impl is gone.
            let backend = match Store::new(db_path.to_string_lossy().as_ref()).await {
                Ok(store) => store,
                Err(e) => {
                    let msg = format!(
                        "Failed to open session store at {}: {}",
                        db_path.display(),
                        e
                    );
                    tracing::error!("WhatsApp: {}", msg);
                    self.whatsapp_state.broadcast_error(&msg);
                    return;
                }
            };

            let cfg = self.config_rx.borrow().clone();
            tracing::info!(
                "WhatsApp agent running (STT={}, TTS={})",
                cfg.voice_config().stt_enabled,
                cfg.voice_config().tts_enabled,
            );

            // Derive owner JID from first allowed phone (for proactive messaging)
            let owner_jid = cfg
                .channels
                .whatsapp
                .allowed_phones
                .first()
                .map(|p| format!("{}@s.whatsapp.net", p.trim_start_matches('+')));

            let agent = self.agent_service.clone();
            let session_svc = self.session_service.clone();
            let shared_session = self.shared_session_id.clone();
            let wa_state = self.whatsapp_state.clone();
            let config_rx = self.config_rx.clone();
            let channel_msg_repo = self.channel_msg_repo.clone();
            let owner_jid_clone = owner_jid.clone();

            // #2178: fold any session still keyed by a LID into its phone-number
            // twin. #1533 fixed the key for new messages but left the rows that
            // already existed, so an install that ran WhatsApp across that change
            // keeps two live sessions for one human and answers pre-cutover
            // questions as if the history were gone. The mapping is authoritative
            // (the Signal store learned it), so this needs no inference, and the
            // pass is idempotent: after the first boot there is nothing left to
            // rename or merge.
            {
                use wacore::store::traits::ProtocolStore;
                match backend.get_all_lid_mappings().await {
                    Ok(entries) if !entries.is_empty() => {
                        let pairs: Vec<(String, String)> = entries
                            .into_iter()
                            .map(|e| (e.lid, e.phone_number))
                            .collect();
                        match super::lid_migration::reconcile_lid_sessions(
                            &session_svc.pool(),
                            pairs,
                        )
                        .await
                        {
                            Ok(report) if !report.is_noop() => tracing::info!(
                                "WhatsApp: reconciled {} LID session(s) into their phone-number \
                                 key, merged {} with {} message(s) moved (#2178)",
                                report.renamed,
                                report.merged,
                                report.messages_moved
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(
                                "WhatsApp: LID session reconciliation failed, sessions left as \
                                 they were: {e:#} (#2178)"
                            ),
                        }
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        "WhatsApp: could not read the LID/PN mapping, skipping session \
                         reconciliation: {e} (#2178)"
                    ),
                }
            }

            let bot_result = Bot::builder()
                .with_backend(backend)
                .with_transport_factory(TokioWebSocketTransportFactory::new())
                .with_http_client(UreqHttpClient::new())
                .with_runtime(TokioRuntime)
                .on_event(move |event, client| {
                    let agent = agent.clone();
                    let session_svc = session_svc.clone();
                    let shared_session = shared_session.clone();
                    let wa_state = wa_state.clone();
                    let owner_jid = owner_jid_clone.clone();
                    let config_rx = config_rx.clone();
                    let channel_msg_repo = channel_msg_repo.clone();
                    let owner_alert = owner_alert.clone();
                    async move {
                        match &*event {
                            Event::PairingQrCode(qr) => {
                                let code = &qr.code;
                                tracing::info!(
                                    "WhatsApp: QR code available (scan with your phone)"
                                );
                                wa_state.broadcast_qr(code);
                            }
                            Event::PairSuccess(s) => {
                                // The paired account's JID IS the owner (the
                                // person who scanned). Pin them as the
                                // authoritative owner so a config mismatch can
                                // never lock them out, and ensure they are in
                                // the allow list. Numbers are stored WITHOUT a
                                // leading '+' (matching sender_phone); the
                                // handler's wa_should_respond normalises both
                                // sides, so '+'-prefixed legacy entries still
                                // match.
                                let full = s.id.to_string();
                                let num = full
                                    .split('@')
                                    .next()
                                    .unwrap_or(&full)
                                    .split(':')
                                    .next()
                                    .unwrap_or(&full)
                                    .trim_start_matches('+')
                                    .to_string();
                                if num.is_empty() {
                                    tracing::warn!(
                                        "WhatsApp: pairing successful but could not extract \
                                         owner number from JID '{full}'"
                                    );
                                } else {
                                    tracing::info!("WhatsApp: pairing successful — owner is {num}");
                                    if let Err(e) = Config::write_key(
                                        "channels.whatsapp",
                                        "bot_owner",
                                        &format!("[\"{num}\"]"),
                                    ) {
                                        tracing::warn!(
                                            "WhatsApp: failed to persist bot_owner: {e}"
                                        );
                                    }
                                    // Append to allowed_phones if not already present.
                                    let mut allowed: Vec<String> =
                                        config_rx.borrow().channels.whatsapp.allowed_phones.clone();
                                    let present =
                                        allowed.iter().any(|a| a.trim_start_matches('+') == num);
                                    if !present {
                                        allowed.push(num.clone());
                                        let json = format!(
                                            "[{}]",
                                            allowed
                                                .iter()
                                                .map(|a| format!("\"{a}\""))
                                                .collect::<Vec<_>>()
                                                .join(",")
                                        );
                                        if let Err(e) = Config::write_key(
                                            "channels.whatsapp",
                                            "allowed_phones",
                                            &json,
                                        ) {
                                            tracing::warn!(
                                                "WhatsApp: failed to persist allowed_phones: {e}"
                                            );
                                        }
                                    }
                                    // Make the freshly-paired owner available to
                                    // the Connected handler (which sends the
                                    // confirmation greeting once the socket is
                                    // ready) and to the whatsapp_send tool.
                                    wa_state
                                        .set_owner_jid(format!("{num}@s.whatsapp.net"))
                                        .await;
                                    // Flag this as a fresh pairing so the
                                    // Connected handler knows to fire the
                                    // one-time greeting (not suppress it as a
                                    // routine restart).
                                    wa_state.set_first_pair_pending();
                                }
                            }
                            Event::Connected(_) => {
                                tracing::info!("WhatsApp: connected successfully");
                                // #1525: fire one bounded history request per
                                // opted-in chat, on connect. The phone answers
                                // with encrypted frames that arrive through the
                                // normal event stream; the capture gate in
                                // `handle_message` stores them and keeps them
                                // off the agent. Empty opt-in (the default)
                                // costs nothing here, and the work is spawned
                                // so the event loop never waits on the phone.
                                {
                                    let hist_chats = config_rx
                                        .borrow()
                                        .channels
                                        .whatsapp
                                        .history_import_chats
                                        .clone();
                                    if !hist_chats.is_empty() {
                                        let client = client.clone();
                                        let wa_state = wa_state.clone();
                                        let repo = channel_msg_repo.clone();
                                        tokio::spawn(async move {
                                            let owner = wa_state.owner_jid.lock().await.clone();
                                            for chat in hist_chats {
                                                let oldest =
                                                    match repo.oldest_for_chat("whatsapp", &chat).await
                                                    {
                                                        Ok(o) => o,
                                                        Err(e) => {
                                                            tracing::warn!(
                                                                "whatsapp history: anchor lookup failed for {chat}: {e}"
                                                            );
                                                            continue;
                                                        }
                                                    };
                                                let now = chrono::Utc::now();
                                                let plan = match &oldest {
                                                    Some((pid, sid, ts)) => history::plan_import(
                                                        Some((
                                                            pid.as_str(),
                                                            history::from_me_of(
                                                                sid,
                                                                owner.as_deref(),
                                                            ),
                                                            *ts,
                                                        )),
                                                        now,
                                                    ),
                                                    None => history::plan_import(None, now),
                                                };
                                                let Some(plan) = plan else { continue };
                                                let Ok(jid) =
                                                    chat.parse::<wacore_binary::jid::Jid>()
                                                else {
                                                    tracing::warn!(
                                                        "whatsapp history: unparseable chat id {chat}"
                                                    );
                                                    continue;
                                                };
                                                match client
                                                    .fetch_message_history(
                                                        &jid,
                                                        &plan.oldest_msg_id,
                                                        plan.oldest_from_me,
                                                        plan.oldest_ts_ms,
                                                        plan.count,
                                                    )
                                                    .await
                                                {
                                                    Ok(req) => tracing::info!(
                                                        "whatsapp history: import requested for {chat} (up to {} msgs, request {req})",
                                                        plan.count
                                                    ),
                                                    Err(e) => tracing::warn!(
                                                        "whatsapp history: request failed for {chat}: {e}"
                                                    ),
                                                }
                                            }
                                        });
                                    }
                                }
                                // #1487: refresh the local blocklist mirror so
                                // the inbound guard reflects blocks the owner
                                // made from their phone, not only ones the bot
                                // issued. One query per connection, never per
                                // message. A failure leaves the previous mirror
                                // in place; the server still enforces the block,
                                // so the worst case is the belt-and-braces layer
                                // being stale for this session.
                                // #1488: announce availability once per
                                // connection. Without it the paired account
                                // reads as permanently offline to everyone it
                                // talks to, and WhatsApp withholds presence
                                // updates from a client that never publishes
                                // its own. Non-fatal: a failure costs presence,
                                // not messaging.
                                if let Err(e) = client.presence().set_available().await {
                                    tracing::warn!(
                                        target: "whatsapp",
                                        error = %e,
                                        "could not publish availability; the account will \
                                         appear offline and contact presence may not arrive"
                                    );
                                }
                                match client.blocking().get_blocklist().await {
                                    Ok(entries) => {
                                        let jids: Vec<String> =
                                            entries.iter().map(|e| e.jid.to_string()).collect();
                                        let count = jids.len();
                                        wa_state.blocklist.replace(jids).await;
                                        tracing::debug!(
                                            target: "whatsapp",
                                            count,
                                            "blocklist mirror refreshed"
                                        );
                                    }
                                    Err(e) => tracing::warn!(
                                        target: "whatsapp",
                                        error = %e,
                                        "could not refresh the blocklist mirror; keeping the previous one"
                                    ),
                                }
                                // Prefer the freshly-paired owner (set on
                                // PairSuccess) over the startup-derived one,
                                // which is None on a first-time pairing.
                                let owner = match wa_state.owner_jid().await {
                                    Some(j) => Some(j),
                                    None => owner_jid.clone(),
                                };
                                // Greet only on a fresh pairing (first-time or
                                // re-pair after reset), not on every app restart
                                // or reconnect. The `first_pair_pending` flag is
                                // set by PairSuccess and consumed here: it is
                                // `true` exactly once per pairing, so a plain
                                // restart (where no PairSuccess fires) never
                                // triggers the greeting. The `was_connected`
                                // guard still suppresses keepalive reconnects
                                // within the same session.
                                let was_connected = wa_state.is_connected().await;
                                wa_state.set_connected(client.clone(), owner.clone()).await;
                                // #1529: the newsletter poller, one task per
                                // process session. First connect only: the
                                // event loop survives keepalive reconnects and
                                // a second poller would race the cursor.
                                // Empty opt-in (the default) never spawns.
                                if !was_connected
                                    && let Some(owner_jid) = owner.clone()
                                {
                                    let chans = config_rx
                                        .borrow()
                                        .channels
                                        .whatsapp
                                        .newsletters
                                        .clone();
                                    if !chans.is_empty() {
                                        tokio::spawn(newsletter::run_poller(
                                            wa_state.clone(),
                                            channel_msg_repo.clone(),
                                            owner_jid,
                                            chans,
                                        ));
                                    }
                                }
                                if was_connected {
                                    tracing::debug!(
                                        "WhatsApp: reconnected — suppressing duplicate \
                                         confirmation greeting"
                                    );
                                } else if wa_state.take_first_pair_pending() {
                                    // Fresh pairing: a real agent turn into the
                                    // owner's self-chat. Spawned so the event loop
                                    // is never blocked by a full agent turn.
                                    if let Some(jid) = owner {
                                        let num = jid.split('@').next().unwrap_or(&jid).to_string();
                                        tokio::spawn(handler::send_connection_greeting(
                                            client.clone(),
                                            agent.clone(),
                                            session_svc.clone(),
                                            wa_state.clone(),
                                            num,
                                        ));
                                    } else {
                                        tracing::warn!(
                                            "WhatsApp: connected but no owner number known — \
                                             skipping confirmation greeting"
                                        );
                                    }
                                } else {
                                    tracing::debug!(
                                        "WhatsApp: connected (app restart) — no fresh pair, \
                                         staying silent"
                                    );
                                }
                            }
                            Event::Messages(batch) => {
                                tracing::debug!(
                                    count = batch.messages.len(),
                                    "WhatsApp: Event::Messages received"
                                );
                                // One task per inbound message: the agent turn
                                // is a very large async state machine, and
                                // polling it inline inside the event-loop
                                // future overflows the worker stack (and would
                                // block the loop). Live traffic is one message
                                // per batch; an offline drain delivers several.
                                for inbound in batch.messages.iter() {
                                    tokio::spawn(handler::handle_message(
                                        (*inbound.message).clone(),
                                        (*inbound.info).clone(),
                                        client.clone(),
                                        agent.clone(),
                                        session_svc.clone(),
                                        shared_session.clone(),
                                        wa_state.clone(),
                                        config_rx.clone(),
                                        channel_msg_repo.clone(),
                                    ));
                                }
                            }
                            Event::LoggedOut(lo) => {
                                tracing::error!(
                                    reason = ?lo.reason,
                                    on_connect = lo.on_connect,
                                    "WhatsApp: logged out"
                                );
                                // The raw stanza carries a one-time appeal token;
                                // it is never logged.
                                if let Some(msg) = lock_alert(&lo.reason) {
                                    owner_alert(msg);
                                }
                                wa_state.broadcast_error(&logout_notice(
                                    &lo.reason,
                                    lo.logout_message
                                        .as_ref()
                                        .and_then(|m| m.header.as_deref()),
                                ));
                            }
                            Event::TemporaryBan(ban) => {
                                tracing::error!(
                                    code = ?ban.code,
                                    expire_secs = ban.expire.num_seconds(),
                                    "WhatsApp: temporary ban"
                                );
                                tracing::debug!(raw = ?ban.raw, "WhatsApp: temporary ban stanza");
                                owner_alert(ban_alert(
                                    &ban.code,
                                    ban.expire,
                                    ban.url.as_deref(),
                                ));
                                wa_state.broadcast_error(&temp_ban_notice(
                                    &ban.code,
                                    ban.expire,
                                    ban.message.as_deref(),
                                    ban.url.as_deref(),
                                ));
                            }
                            Event::ConnectFailure(cf) => {
                                tracing::error!(
                                    reason = ?cf.reason,
                                    "WhatsApp: connect failure"
                                );
                                if let Some(msg) = lock_alert(&cf.reason) {
                                    owner_alert(msg);
                                }
                                wa_state.broadcast_error(&connect_failure_notice(
                                    &cf.reason,
                                    cf.message.as_deref(),
                                ));
                            }
                            Event::Disconnected(_) => {
                                tracing::warn!("WhatsApp: disconnected");
                            }
                            Event::Receipt(receipt) => {
                                // A message we sent was accepted/delivered.
                                // `Delivered` is the normal recipient receipt;
                                // `Sender` is what a self-chat send gets back —
                                // the bot is paired AS the owner, so its replies
                                // go to the owner's own devices, which ack with
                                // `sender`, not `delivered`. Either one means the
                                // message landed, so surface the id for the
                                // onboarding connection test.
                                if matches!(
                                    receipt.r#type,
                                    wacore::types::presence::ReceiptType::Delivered
                                        | wacore::types::presence::ReceiptType::Sender
                                ) {
                                    for id in &receipt.message_ids {
                                        wa_state.broadcast_delivered(id);
                                    }
                                }
                            }
                            Event::ClientOutdated(_) => {
                                tracing::error!(
                                    "WhatsApp: the server refuses this client build; update OpenCrabs"
                                );
                            }
                            Event::StreamReplaced(_) => {
                                tracing::warn!(
                                    "WhatsApp: this account's stream was replaced by another connection"
                                );
                            }
                            Event::StreamError(_) => {
                                tracing::warn!("WhatsApp: stream error");
                            }
                            Event::UndecryptableMessage(_) => {
                                tracing::warn!("WhatsApp: an incoming message could not be decrypted");
                            }
                            Event::IdentityChange(_) => {
                                tracing::warn!("WhatsApp: a contact's identity key changed");
                            }
                            other => {
                                tracing::debug!("WhatsApp: unhandled event: {:?}", other);
                            }
                        }
                    }
                })
                .build()
                .await;

            let bot = match bot_result {
                Ok(b) => b,
                Err(e) => {
                    let msg = format!("Failed to build WhatsApp bot: {}", e);
                    tracing::error!("WhatsApp: {}", msg);
                    self.whatsapp_state.broadcast_error(&msg);
                    return;
                }
            };

            // `run()` drives the bot until the connection ends. It returns `()`
            // (no Result, no separate handle: build/credential failures surface
            // before this point). The live client is published to WhatsAppState
            // from the Connected event callback, so we don't need a handle here.
            bot.run().await;
            tracing::info!("WhatsApp: bot run loop exited");
        })
    }
}
