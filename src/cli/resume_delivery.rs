//! Delivery for boot-revived turns (#1950, #1952).
//!
//! The generic resume arm used to end with a chain of `if let ... && let ...`
//! guards and no else: if the channel's transport was not connected when the
//! revived turn finished (likely, since nothing orders the recovery pass
//! against the channel's own connect), the generated answer was dropped with
//! zero log lines. And `boot_report::record_delivered()` fired before the
//! send was even attempted, so the boot ledger counted those drops as
//! delivered. Two drop tests on 2026-10-05: 2 answers generated, 0 delivered,
//! 0 errors logged.
//!
//! The whole revived-turn task lives here so delivery, announcements and
//! counting are one auditable unit. Sends wait for the transport within the
//! bounded grace (#1242 pattern), every skip path logs loudly, the ledger
//! counts outcomes rather than intentions, and the channel gets a visible
//! "continuing your interrupted turn" notice as soon as it wakes.

use std::sync::Arc;
use uuid::Uuid;

use crate::brain::agent::AgentService;
use crate::brain::agent::service::boot_report;
use crate::channels::transport_ready::await_transport;
use crate::tui::events::TuiEvent;

/// How a resumed turn's delivery ended (#1952). The boot ledger counts
/// these outcomes, never intentions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeSendOutcome {
    /// The channel confirmed the send.
    Sent,
    /// The answer reached the restarted TUI over the event bus.
    SurfacedTui,
    /// A send was attempted and the channel returned an error.
    SendFailed,
    /// The channel's transport never connected within `CONNECT_GRACE`.
    TransportGone,
    /// The pending row carried no channel address to send to.
    NoAddress,
    /// The chat id on the row did not parse for this channel.
    BadAddress,
    /// No delivery routing exists for this channel name.
    Unsupported,
}

impl ResumeSendOutcome {
    /// Whether the boot ledger may count this as delivered (#1952): only
    /// outcomes where a surface actually received the answer.
    pub(crate) fn counts_as_delivered(self) -> bool {
        matches!(self, Self::Sent | Self::SurfacedTui)
    }
}

/// The channel-side announcement that a restarted process is continuing an
/// interrupted turn (#1950). Telegram posts its own through the streaming
/// resume and the TUI paints the `PendingResumed` card; every other surface
/// used to get nothing at all while its turn silently revived.
pub(crate) fn resume_notice_text(session_id: Uuid) -> String {
    format!(
        "🔁 Restart recovery: continuing session {}'s interrupted turn. The answer will land here when done.",
        &session_id.simple().to_string()[..8]
    )
}

/// TUI presence for a revived remote turn (#1951).
///
/// Boot recovery replays the interrupted turn directly, bypassing the
/// channel ingress that normally emits `ChannelProcessingStarted`, so a
/// resumed non-TUI turn had no road to the TUI at all: the session list
/// kept showing it idle while its answer was still being generated. This
/// guard sends the started event on creation and the finished event on
/// drop, covering every exit path of the revived task.
pub(crate) struct ResumePresence {
    tx: tokio::sync::mpsc::UnboundedSender<TuiEvent>,
    session_id: Uuid,
}

impl ResumePresence {
    /// Announce processing for a revived turn. Rows the restarted TUI
    /// already paints via `PendingResumed` (`tui`) return `None`; a second
    /// badge would claim the local turn is a remote one.
    pub(crate) fn start(
        tx: &tokio::sync::mpsc::UnboundedSender<TuiEvent>,
        session_id: Uuid,
        channel: &str,
    ) -> Option<Self> {
        if channel == "tui" {
            return None;
        }
        let _ = tx.send(TuiEvent::ChannelProcessingStarted(session_id));
        Some(Self {
            tx: tx.clone(),
            session_id,
        })
    }
}

impl Drop for ResumePresence {
    fn drop(&mut self) {
        let _ = self
            .tx
            .send(TuiEvent::ChannelProcessingFinished(self.session_id));
    }
}

/// Chunk `text` for the channel's send surface (#1953). Discord must stay
/// under its message cap, split with the fence-aware splitter the ingress
/// uses so code blocks survive the seam. Every other channel keeps the
/// single-message path: the proven cap disease is Discord's, and inventing
/// chunking rules for whatsapp/slack is out of scope.
pub(crate) fn chunks_for(channel: &str, text: &str) -> Vec<String> {
    #[cfg(feature = "discord")]
    if channel == "discord" {
        return crate::channels::discord::handler::split_message(text, DISCORD_MAX);
    }
    let _ = channel;
    vec![text.to_string()]
}

/// Clones of the channel transports a revived turn may deliver through.
/// Fields are feature-gated exactly like the channels themselves.
#[derive(Clone)]
pub(crate) struct ResumeTransports {
    #[cfg(feature = "discord")]
    pub(crate) discord: Arc<crate::channels::discord::DiscordState>,
    #[cfg(feature = "whatsapp")]
    pub(crate) whatsapp: Arc<crate::channels::whatsapp::WhatsAppState>,
    #[cfg(feature = "slack")]
    pub(crate) slack: Arc<crate::channels::slack::SlackState>,
}

/// Discord's message cap. Resume answers ship through the same surface as
/// live ones, so they must be chunked by the same fence-safe splitter the
/// ingress uses (#1953, the #1949 disease on the other resume path).
#[cfg(feature = "discord")]
pub(crate) const DISCORD_MAX: usize = 2000;

/// Send `text` on `channel` to `target`, waiting for the transport within
/// the bounded grace before giving up, and loudly (#1950): every skip path
/// either logs an error naming the session or returns an outcome the caller
/// logs. No answer may vanish without a line saying where it died.
pub(crate) async fn resume_send_text(
    channel: &str,
    session_id: Uuid,
    target: &str,
    text: &str,
    transports: &ResumeTransports,
) -> ResumeSendOutcome {
    // Harmless when the channel features are on (the arms below use them);
    // keeps a no-channel build warning-free the same way the old inline cfg
    // match arms did.
    #[cfg(not(any(feature = "discord", feature = "whatsapp", feature = "slack")))]
    let _ = (&target, &text, &transports);
    match channel {
        #[cfg(feature = "discord")]
        "discord" => {
            let Ok(ch_id) = target.parse::<u64>() else {
                tracing::error!(
                    "[boot-resume] discord: chat id {target:?} for session {session_id} did not parse; answer not delivered"
                );
                return ResumeSendOutcome::BadAddress;
            };
            // Bounded wait, not a silent drop (#1242 parity): at boot the
            // transport is usually seconds away. `await_transport` logs the
            // error itself when the whole grace passes.
            let Some(http) =
                await_transport("discord", session_id, || transports.discord.http()).await
            else {
                return ResumeSendOutcome::TransportGone;
            };
            let chan = serenity::model::id::ChannelId::new(ch_id);
            // #1953: send in chunks. One `say` over the cap fails outright,
            // and the old code lost the entire answer to that error (the
            // #1949 disease, resume path).
            let chunks = chunks_for(channel, text);
            let total = chunks.len();
            let mut send_failed = false;
            for (index, chunk) in chunks.into_iter().enumerate() {
                if let Err(e) = chan.say(&http, &chunk).await {
                    send_failed = true;
                    if index == 0 {
                        tracing::error!(error = %e, "[boot-resume] discord: first of {total} chunks failed, the whole answer was lost for session {session_id}");
                    } else {
                        tracing::warn!(error = %e, "[boot-resume] discord: chunk {} of {total} failed for session {session_id}", index + 1);
                    }
                }
            }
            if send_failed {
                ResumeSendOutcome::SendFailed
            } else {
                ResumeSendOutcome::Sent
            }
        }
        #[cfg(feature = "whatsapp")]
        "whatsapp" => {
            let Ok(jid) = target.parse::<wacore_binary::jid::Jid>() else {
                tracing::error!(
                    "[boot-resume] whatsapp: chat id {target:?} for session {session_id} did not parse; answer not delivered"
                );
                return ResumeSendOutcome::BadAddress;
            };
            let Some(client) =
                await_transport("whatsapp", session_id, || transports.whatsapp.client()).await
            else {
                return ResumeSendOutcome::TransportGone;
            };
            let msg = waproto::whatsapp::Message {
                conversation: Some(text.to_string()),
                ..Default::default()
            };
            match client.send_message(jid, msg).await {
                Ok(_) => ResumeSendOutcome::Sent,
                Err(e) => {
                    tracing::warn!(error = %e, "[boot-resume] whatsapp: send_message failed for session {session_id}");
                    ResumeSendOutcome::SendFailed
                }
            }
        }
        #[cfg(feature = "slack")]
        "slack" => {
            // Slack needs both halves to post, so the pair is what
            // readiness means here (same as the bg-resume path).
            let Some((token_val, client)) = await_transport("slack", session_id, || async {
                match (
                    transports.slack.bot_token().await,
                    transports.slack.client().await,
                ) {
                    (Some(token), Some(client)) => Some((token, client)),
                    _ => None,
                }
            })
            .await
            else {
                return ResumeSendOutcome::TransportGone;
            };
            let api_token = slack_morphism::prelude::SlackApiToken::new(
                slack_morphism::prelude::SlackApiTokenValue::from(token_val),
            );
            let session = client.open_session(&api_token);
            let req = slack_morphism::prelude::SlackApiChatPostMessageRequest::new(
                target.to_string().into(),
                slack_morphism::prelude::SlackMessageContent::new().with_text(text.to_string()),
            );
            match session.chat_post_message(&req).await {
                Ok(_) => ResumeSendOutcome::Sent,
                Err(e) => {
                    tracing::warn!(error = %e, "[boot-resume] slack: chat_post_message failed for session {session_id}");
                    ResumeSendOutcome::SendFailed
                }
            }
        }
        other => {
            tracing::warn!(
                "[boot-resume] no recovery routing for channel {other} (session {session_id}); answer saved to DB only"
            );
            ResumeSendOutcome::Unsupported
        }
    }
}

/// The whole revived-turn task for rows the boot pass dispatches generically
/// (everything except telegram's streaming arm and system-origin rows): run
/// the turn, announce the recovery on its channel, deliver the answer
/// through the loud path, and let the ledger count the outcome (#1950,
/// #1952).
pub(crate) async fn resume_delivery_task(
    agent: Arc<AgentService>,
    ev_tx: tokio::sync::mpsc::UnboundedSender<TuiEvent>,
    transports: ResumeTransports,
    token: tokio_util::sync::CancellationToken,
    channel: String,
    channel_chat_id: Option<String>,
    session_id: Uuid,
) {
    // Do NOT swap the shared agent's provider here. The agent_service is
    // shared across all sessions, so swapping it for one session's saved
    // provider contaminates every other session. The FallbackProvider
    // handles model remapping automatically.

    // #1951: a revived turn is doing work that no surface can see until the
    // answer lands. Paint it on the TUI's session list for the whole task;
    // the guard's Drop fires the finished event on every exit below.
    let _presence = ResumePresence::start(&ev_tx, session_id, &channel);

    // #1950: announce the recovery on the originating channel as soon as its
    // transport wakes. Telegram's streaming arm and the TUI card announce
    // themselves; a row without a chat id cannot be announced, and that skip
    // is reported at the response instead.
    let notice_handle = if channel != "tui" && channel != "telegram" {
        channel_chat_id.clone().map(|cid| {
            let ch = channel.clone();
            let t = transports.clone();
            tokio::spawn(async move {
                let outcome =
                    resume_send_text(&ch, session_id, &cid, &resume_notice_text(session_id), &t)
                        .await;
                if outcome != ResumeSendOutcome::Sent {
                    tracing::warn!(
                        "[boot-resume] resuming notice not delivered for session {session_id} on {ch}: {outcome:?}"
                    );
                }
                outcome
            })
        })
    } else {
        None
    };

    // One wording for every surface (#2627): the window resumes the same
    // way the terminal does, so the text lives on the recovery module.
    let prompt = crate::brain::agent::service::restart_recovery::CONTINUATION_PROMPT.to_string();

    match agent
        .resume_interrupted_turn(
            session_id,
            prompt,
            None,
            Some(token),
            None,
            None,
            &channel,
            channel_chat_id.as_deref(),
        )
        .await
    {
        Ok(response) => {
            tracing::info!(
                "Resume completed for session {} ({}): {} chars",
                session_id,
                channel,
                response.content.len()
            );
            // Keep the promised order (#1950): the notice lands before the
            // answer it announced. The wait is bounded by the same grace the
            // notice itself uses.
            if let Some(handle) = notice_handle {
                let _ = handle.await;
            }
            // A revived sub-agent session (#110): its result belongs to the
            // session that spawned it, not to the surface-less default. It
            // reaches no surface here, so the ledger calls it parked, not
            // delivered (#1952).
            if let Some(mut agent_status) =
                crate::brain::agent::service::work_status::WorkStatus::find_agent_by_session(
                    &session_id.to_string(),
                )
            {
                boot_report::record_parked();
                crate::brain::agent::service::restart_recovery::deliver_revived_agent_outcome(
                    &mut agent_status,
                    Ok(&response.content),
                );
                return;
            }
            let outcome = if channel == "tui" {
                match ev_tx.send(TuiEvent::ResponseComplete {
                    session_id,
                    response: Box::new(response),
                }) {
                    Ok(()) => ResumeSendOutcome::SurfacedTui,
                    Err(_) => ResumeSendOutcome::SendFailed,
                }
            } else {
                match &channel_chat_id {
                    Some(cid) => {
                        resume_send_text(&channel, session_id, cid, &response.content, &transports)
                            .await
                    }
                    None => {
                        tracing::error!(
                            "[boot-resume] response for session {session_id} (channel {channel}) has no chat id on its pending row; answer not delivered"
                        );
                        ResumeSendOutcome::NoAddress
                    }
                }
            };
            // #1952: the ledger counts outcomes, not intentions. The old line
            // incremented `delivered` before the send was even attempted,
            // which is how two silently dropped replies hid behind a green
            // number while the channel stayed empty.
            if outcome.counts_as_delivered() {
                boot_report::record_delivered();
            } else {
                tracing::error!(
                    "[boot-resume] session {session_id} ({channel}): answer generated but not delivered: {outcome:?}"
                );
                boot_report::record_failed();
            }
        }
        Err(e) => {
            tracing::error!("Resume failed for session {}: {}", session_id, e);
            boot_report::record_failed();
            // A revived sub-agent session whose resume failed (#110): the
            // parent is waiting on this outcome either way, so report and
            // finalize instead of dropping it.
            if let Some(mut agent_status) =
                crate::brain::agent::service::work_status::WorkStatus::find_agent_by_session(
                    &session_id.to_string(),
                )
            {
                crate::brain::agent::service::restart_recovery::deliver_revived_agent_outcome(
                    &mut agent_status,
                    Err(&e.to_string()),
                );
                return;
            }
            if channel == "tui" {
                let _ = ev_tx.send(TuiEvent::Error {
                    session_id,
                    message: e.to_string(),
                });
            }
        }
    }
}
