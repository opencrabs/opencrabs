//! WhatsApp Message Handler
//!
//! Processes incoming WhatsApp messages: text + images, allowlist enforcement,
//! session routing (owner shares TUI session, others get per-phone sessions).

use crate::brain::agent::AgentService;
use crate::brain::agent::{ApprovalCallback, ProgressCallback, ProgressEvent};
use crate::channels::group_history;
use crate::channels::whatsapp::WhatsAppState;
use crate::config::Config;
use crate::db::ChannelMessageRepository;
use crate::db::models::ChannelMessage as DbChannelMessage;
use crate::services::SessionService;
use crate::utils::sanitize::redact_secrets;
use crate::utils::truncate_str;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

use tokio_util::sync::CancellationToken;
use wacore::types::message::MessageInfo;
use wacore_binary::jid::JidExt;
use waproto::whatsapp::Message;
use whatsapp_rust::SendOptions;
use whatsapp_rust::client::Client;

/// True when an inbound stanza is a contact's status (story) rather than a
/// message addressed to us.
///
/// Status posts arrive as group traffic on `status@broadcast`, so without this
/// they look like a DM from the poster: a full agent turn runs on the image,
/// and the session's reply target becomes a JID that cannot receive
/// `send_message` (#2176). A status is never a message, so it is dropped
/// before session resolution rather than filtered later.
pub(crate) fn is_inbound_status(chat: &wacore_binary::jid::Jid, is_from_me: bool) -> bool {
    !is_from_me && chat.is_status_broadcast()
}

/// Composing pings are best-effort, so the retry policy is deliberately
/// conservative (#2181): the loop used to retry every 5 seconds forever with
/// no connection check, which during an outage wrote one WARN per 5 seconds
/// per session for a call that could not succeed (48 lines in 4 minutes).
pub(crate) const COMPOSING_BASE_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

/// Ceiling for the exponential backoff. The indicator is a nicety, not a
/// delivery mechanism, so past 30 seconds there is nothing to gain from
/// pinging more often.
pub(crate) const COMPOSING_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// Give up after this many consecutive failures. The turn's cancel token ends
/// the loop anyway, so a cap only stops the pointless tail of a dead
/// connection.
pub(crate) const COMPOSING_MAX_FAILURES: u32 = 5;

/// Delay before the next composing attempt, or `None` when the loop should
/// stop. `failures` counts consecutive failures, reset to zero on success.
///
/// Pure so the policy is testable without a socket: 5s, 10s, 20s, 30s, then
/// give up.
pub(crate) fn composing_retry_delay(failures: u32) -> Option<std::time::Duration> {
    if failures == 0 {
        return Some(COMPOSING_BASE_DELAY);
    }
    if failures >= COMPOSING_MAX_FAILURES {
        return None;
    }
    let factor = 1u64 << (failures - 1).min(3);
    let base = COMPOSING_BASE_DELAY.as_millis() as u64;
    let cap = COMPOSING_MAX_DELAY.as_millis() as u64;
    Some(std::time::Duration::from_millis(
        base.saturating_mul(factor).min(cap),
    ))
}

/// Header prepended to all outgoing messages so the user knows it's from the agent.
pub const MSG_HEADER: &str = "\u{1f980} *OpenCrabs*";

/// Send a WhatsApp message, then resend it once after a short delay reusing the
/// SAME message id.
///
/// wacore's parallel encrypt fan-out silently skips any recipient device whose
/// Signal session is not yet established (`session ... not found. Skipping.`),
/// and the server then rejects the WHOLE stanza with `error 400` — so a reply
/// to a chat that has a freshly-seen or stale linked device can never arrive,
/// even though the agent produced it. The skipped device's prekey is fetched as
/// a side effect of that first attempt, so a second attempt encrypts for every
/// device and lands. Reusing the message id makes the pair idempotent:
/// recipients dedupe by id, so a first attempt that DID deliver is never shown
/// twice. The resend is spawned so the caller is never blocked.
///
/// Returns the message id when the first send succeeded, so the caller can
/// track it for a later edit or reaction (#1408). `None` means the send
/// failed and there is nothing to track.
async fn send_resilient(
    client: &Arc<Client>,
    jid: wacore_binary::jid::Jid,
    msg: Message,
) -> Option<String> {
    let gen_id = client.generate_message_id();
    let opts = SendOptions::default().with_message_id(gen_id.clone());
    let mut sent = Some(gen_id);
    if let Err(e) = client
        .send_message_with_options(jid.clone(), msg.clone(), opts.clone())
        .await
    {
        tracing::error!("WhatsApp: send failed: {e}");
        sent = None;
    }
    let client = client.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let Err(e) = client.send_message_with_options(jid, msg, opts).await {
            tracing::debug!("WhatsApp: idempotent resend failed: {e}");
        }
    });
    sent
}

/// Unwrap nested message wrappers (device_sent, ephemeral, view_once, etc.)
/// Returns the innermost Message that contains actual content.
fn unwrap_message(msg: &Message) -> &Message {
    // device_sent_message: wraps messages synced across linked devices
    if let Some(dsm) = msg.device_sent_message.as_option()
        && let Some(inner) = dsm.message.as_option()
    {
        return unwrap_message(inner);
    }
    // ephemeral_message: disappearing messages
    if let Some(eph) = msg.ephemeral_message.as_option()
        && let Some(inner) = eph.message.as_option()
    {
        return unwrap_message(inner);
    }
    // view_once_message
    if let Some(vo) = msg.view_once_message.as_option()
        && let Some(inner) = vo.message.as_option()
    {
        return unwrap_message(inner);
    }
    // document_with_caption_message
    if let Some(dwc) = msg.document_with_caption_message.as_option()
        && let Some(inner) = dwc.message.as_option()
    {
        return unwrap_message(inner);
    }
    msg
}

/// Extract quoted/replied-to message text from a WhatsApp message.
fn extract_reply_context(msg: &Message) -> Option<String> {
    let msg = unwrap_message(msg);
    let ctx = msg
        .extended_text_message
        .as_option()?
        .context_info
        .as_option()?;
    let quoted = ctx.quoted_message.as_option()?;
    let quoted_text = crate::utils::strip_ctx_footer(&extract_text(quoted)?);
    if quoted_text.is_empty() {
        return None;
    }
    let sender = ctx
        .participant
        .as_ref()
        .map(|p| p.split('@').next().unwrap_or(p).to_string())
        .unwrap_or_else(|| "unknown".to_string());
    Some(format!("[Replying to {sender}: \"{quoted_text}\"]"))
}

/// Extract plain text from a WhatsApp message.
pub(crate) fn extract_text(msg: &Message) -> Option<String> {
    let msg = unwrap_message(msg);
    // Try conversation field first (simple text messages)
    if let Some(ref conv) = msg.conversation
        && !conv.is_empty()
    {
        return Some(conv.clone());
    }
    // Try extended text message (messages with link previews, etc.)
    if let Some(ext) = msg.extended_text_message.as_option()
        && let Some(ref text) = ext.text
    {
        return Some(text.clone());
    }
    // Try image caption
    if let Some(img) = msg.image_message.as_option()
        && let Some(ref caption) = img.caption
        && !caption.is_empty()
    {
        return Some(caption.clone());
    }
    None
}

/// Check if the message has a downloadable image.
pub(crate) fn has_image(msg: &Message) -> bool {
    let msg = unwrap_message(msg);
    msg.image_message.is_set()
}

/// Check if the message has a downloadable audio/voice note.
fn has_audio(msg: &Message) -> bool {
    let msg = unwrap_message(msg);
    msg.audio_message.is_set()
}

/// Check if the message has a document attachment.
fn has_document(msg: &Message) -> bool {
    let msg = unwrap_message(msg);
    msg.document_message.is_set()
}

/// Check if the message carries a video (#1410).
fn has_video(msg: &Message) -> bool {
    let msg = unwrap_message(msg);
    msg.video_message.is_set()
}

/// Check if the message carries a sticker (#1483).
fn has_sticker(msg: &Message) -> bool {
    let msg = unwrap_message(msg);
    msg.sticker_message.is_set()
}

/// Download a video from WhatsApp. Returns (bytes, mime, filename) on success.
///
/// WhatsApp does not name video blobs, so the filename is synthesised from the
/// mimetype: the vision/file funnel keys off the extension, and "file" with no
/// extension would land it in the wrong branch.
async fn download_video(
    msg: &Message,
    client: &Client,
    ctx: &super::media_retry::MediaContext,
) -> Option<(Vec<u8>, String, String)> {
    let msg = unwrap_message(msg);
    let video = msg.video_message.as_option()?;
    let mime = video
        .mimetype
        .clone()
        .unwrap_or_else(|| "video/mp4".to_string());
    let ext = match mime.split(';').next().unwrap_or("").trim() {
        "video/3gpp" => "3gp",
        "video/quicktime" => "mov",
        "video/webm" => "webm",
        _ => "mp4",
    };
    let bytes = super::media_retry::download_with_retry(client, video, ctx, "video", |m, path| {
        m.direct_path = Some(path)
    })
    .await?;
    tracing::debug!("WhatsApp: downloaded video ({} bytes)", bytes.len());
    Some((bytes, mime, format!("video.{ext}")))
}

/// Download a sticker from WhatsApp. Returns (bytes, mime, filename) on
/// success. Stickers are WebP images, so they ride the image funnel.
async fn download_sticker(
    msg: &Message,
    client: &Client,
    ctx: &super::media_retry::MediaContext,
) -> Option<(Vec<u8>, String, String)> {
    let msg = unwrap_message(msg);
    let sticker = msg.sticker_message.as_option()?;
    let mime = sticker
        .mimetype
        .clone()
        .unwrap_or_else(|| "image/webp".to_string());
    let bytes =
        super::media_retry::download_with_retry(client, sticker, ctx, "sticker", |m, path| {
            m.direct_path = Some(path)
        })
        .await?;
    tracing::debug!("WhatsApp: downloaded sticker ({} bytes)", bytes.len());
    Some((bytes, mime, "sticker.webp".to_string()))
}

/// Render the inbound types that carry no media: location, live location,
/// contact card and reaction (#1483). `None` means this message is not one of
/// them, or is a reaction being removed.
fn describe_non_media(msg: &Message) -> Option<String> {
    let msg = unwrap_message(msg);
    if let Some(loc) = msg.location_message.as_option() {
        return Some(super::inbound::describe_location(loc));
    }
    if let Some(loc) = msg.live_location_message.as_option() {
        return Some(super::inbound::describe_live_location(loc));
    }
    if let Some(contact) = msg.contact_message.as_option() {
        return Some(super::inbound::describe_contact(contact));
    }
    if let Some(reaction) = msg.reaction_message.as_option() {
        return super::inbound::describe_reaction(reaction);
    }
    None
}

/// Download a document from WhatsApp. Returns (bytes, mime, filename) on success.
async fn download_document(
    msg: &Message,
    client: &Client,
    ctx: &super::media_retry::MediaContext,
) -> Option<(Vec<u8>, String, String)> {
    let msg = unwrap_message(msg);
    let doc = msg.document_message.as_option()?;
    let mime = doc.mimetype.clone().unwrap_or_default();
    let fname = doc.file_name.clone().unwrap_or_else(|| "file".to_string());
    let bytes = super::media_retry::download_with_retry(client, doc, ctx, "document", |m, path| {
        m.direct_path = Some(path)
    })
    .await?;
    tracing::debug!(
        "WhatsApp: downloaded document {} ({} bytes)",
        fname,
        bytes.len()
    );
    Some((bytes, mime, fname))
}

/// Download audio from WhatsApp. Returns raw bytes on success.
async fn download_audio(
    msg: &Message,
    client: &Client,
    ctx: &super::media_retry::MediaContext,
) -> Option<Vec<u8>> {
    let msg = unwrap_message(msg);
    let audio = msg.audio_message.as_option()?;
    let bytes = super::media_retry::download_with_retry(client, audio, ctx, "audio", |m, path| {
        m.direct_path = Some(path)
    })
    .await?;
    tracing::debug!("WhatsApp: downloaded audio ({} bytes)", bytes.len());
    Some(bytes)
}

/// Download image from WhatsApp. Returns (bytes, mime, filename) on success.
async fn download_image(
    msg: &Message,
    client: &Client,
    ctx: &super::media_retry::MediaContext,
) -> Option<(Vec<u8>, String, String)> {
    let msg = unwrap_message(msg);
    let img = msg.image_message.as_option()?;

    let mime = img.mimetype.as_deref().unwrap_or("image/jpeg").to_string();
    let ext = match mime.as_str() {
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "jpg",
    };
    let fname = format!("image.{ext}");

    match super::media_retry::download_with_retry(client, img, ctx, "image", |m, path| {
        m.direct_path = Some(path)
    })
    .await
    {
        Some(bytes) => {
            tracing::debug!(
                "WhatsApp: downloaded image ({} bytes, mime={})",
                bytes.len(),
                mime
            );
            Some((bytes, mime, fname))
        }
        None => {
            tracing::error!("WhatsApp: image unavailable even after a re-upload request");
            None
        }
    }
}

/// Extract the sender's phone number (digits only) from message info.
/// JID format is "351933536442@s.whatsapp.net" or "351933536442:34@s.whatsapp.net"
/// Extract sender phone from MessageInfo.
/// (linked device suffix) — we return just "351933536442" in both cases.
fn sender_phone(info: &MessageInfo) -> String {
    let full = info.source.sender.to_string();
    let without_server = full.split('@').next().unwrap_or(&full);
    // Strip linked-device suffix (e.g. ":34" for WhatsApp Web/Desktop)
    without_server
        .split(':')
        .next()
        .unwrap_or(without_server)
        .to_string()
}

/// Extract the chat's user part from MessageInfo (who/where the message is in).
/// Mirrors [`sender_phone`]: strips the `@server` and any `:device` suffix so a
/// JID like "351933536442:34@s.whatsapp.net" becomes "351933536442".
fn chat_user(info: &MessageInfo) -> String {
    let full = info.source.chat.to_string();
    let without_server = full.split('@').next().unwrap_or(&full);
    without_server
        .split(':')
        .next()
        .unwrap_or(without_server)
        .to_string()
}

/// Decide whether the WhatsApp handler should respond to an incoming message.
///
/// Security-critical owner / self-chat gate. The bot pairs *as* the owner's
/// WhatsApp account, so the owner talks to it in the "Message Yourself"
/// self-chat: `is_from_me` is true and the chat JID equals the sender JID.
/// That case is **number-agnostic** — even if `allowed_phones` was configured
/// with a number that does not exactly match the real paired account, the
/// owner can never be locked out of their own self-chat.
///
/// Rules (numbers normalised by stripping a leading `+` on both sides):
/// * `allowed` empty  -> open mode, respond to everyone.
/// * owner self-chat (`is_from_me && sender_user == chat_user`) -> respond.
/// * an explicitly allow-listed contact messaging in
///   (`!is_from_me && allowed contains sender`) -> respond.
/// * otherwise -> ignore (never respond to arbitrary chats).
pub(crate) fn wa_should_respond(
    policy: crate::config::types::WaResponsePolicy,
    is_from_me: bool,
    sender_user: &str,
    sender_alt_user: Option<&str>,
    chat_user: &str,
    allowed: &[String],
    operators: &[String],
) -> bool {
    use crate::config::types::WaResponsePolicy;
    let sender = sender_user.trim_start_matches('+');
    // The sender's OTHER identity: WhatsApp addresses DMs by LID (privacy id)
    // or PN (phone number) and the allow list holds phone numbers, so a
    // LID-addressed sender only ever matches through its PN twin (#276).
    let alt = sender_alt_user.map(|a| a.trim_start_matches('+'));
    let chat = chat_user.trim_start_matches('+');
    let in_list = |list: &[String]| {
        list.iter().any(|entry| {
            let entry = entry.trim_start_matches('+');
            entry == sender || alt.is_some_and(|a| a == entry)
        })
    };
    // The paired account messaging itself (self-chat) — always allowed.
    // Operators (bot_owner) messaging the bot from a DIFFERENT account — always
    // allowed. But when the paired account DMs someone else, is_from_me is true
    // and the sender matches bot_owner — that should NOT trigger a response in
    // the other person's chat (the bot should stay silent in the owner's DMs
    // with other people).
    if (is_from_me && sender == chat) || (!is_from_me && in_list(operators)) {
        return true;
    }
    match policy {
        // Legacy: open when no allow-list, otherwise allow-listed contacts only.
        WaResponsePolicy::Auto => {
            if allowed.is_empty() {
                true
            } else {
                !is_from_me && in_list(allowed)
            }
        }
        WaResponsePolicy::OwnerOnly => false,
        WaResponsePolicy::Allowlist => !is_from_me && in_list(allowed),
        WaResponsePolicy::Open => true,
    }
}

/// Split a message into chunks that fit WhatsApp's limit (~65536 chars, but we use 4000 for readability).
pub fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max_len).min(text.len());
        // Ensure end falls on a char boundary (back up if inside a multi-byte char)
        while end < text.len() && !text.is_char_boundary(end) {
            end -= 1;
        }
        let break_at = if end < text.len() {
            text[start..end]
                .rfind('\n')
                .filter(|&pos| pos > end - start - 200)
                .map(|pos| start + pos + 1)
                .unwrap_or(end)
        } else {
            end
        };
        chunks.push(&text[start..break_at]);
        start = break_at;
    }
    chunks
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_message(
    msg: Message,
    info: MessageInfo,
    client: Arc<Client>,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    shared_session: Arc<TokioMutex<Option<Uuid>>>,
    wa_state: Arc<WhatsAppState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
    channel_msg_repo: ChannelMessageRepository,
) {
    let phone = sender_phone(&info);

    // #1487: a blocked contact must not reach the handlers. WhatsApp stops
    // delivering from a blocked account server-side, so this is a second line
    // rather than the only one - it closes the window between the bot issuing
    // a block and the server acting on it, and it costs a set lookup instead
    // of the server round trip `Blocking::is_blocked` would need per message.
    // Never applies to our own echoes, which carry the paired account's JID.
    //
    // Both identities are checked, because a modern 1:1 DM arrives addressed
    // by LID while the blocklist holds phone numbers - the server list and
    // `block_contact` both key on the PN. Checking `sender` alone let a
    // blocked contact messaging from a LID walk straight past this guard
    // (surfaced by the LID routing work in #1531).
    let blocked = !info.source.is_from_me && {
        let sender = info.source.sender.to_string();
        let alt = info.source.sender_alt.as_ref().map(|j| j.to_string());
        wa_state
            .blocklist
            .blocks_either(&sender, alt.as_deref())
            .await
    };
    if blocked {
        tracing::info!(
            target: "whatsapp",
            "dropping inbound message from a blocked contact (#1487)"
        );
        return;
    }

    // #2176: a status (story) is not a message. It arrives as group traffic on
    // `status@broadcast`, which used to run a full agent turn and then poison
    // the session's reply target with a JID that cannot receive a send. Dropped
    // here, before any session is resolved, so no reply target can bind to it.
    if is_inbound_status(&info.source.chat, info.source.is_from_me) {
        tracing::debug!(
            target: "whatsapp",
            "dropping inbound status broadcast from {}",
            phone
        );
        return;
    }
    tracing::debug!(
        "WhatsApp handler: from={}, is_from_me={}, has_text={}, has_image={}, has_audio={}",
        phone,
        info.source.is_from_me,
        extract_text(&msg).is_some(),
        has_image(&msg),
        has_audio(&msg),
    );

    // #1484: keep the raw proto so `forward` can hand the original message to
    // `Client::forward_message` (which relays media from the same CDN blob
    // instead of re-uploading). Recorded before any content filtering, because
    // a message worth forwarding is not always one worth answering.
    wa_state.recent.remember(info.id.clone(), msg.clone()).await;

    // Skip bot's own outgoing replies (they echo back as is_from_me).
    // User messages from their phone are also is_from_me (same account),
    // so we only skip if the text starts with our agent header.
    // Never skip audio/image — those are real user messages even when is_from_me.
    if info.source.is_from_me {
        if let Some(text) = extract_text(&msg) {
            if text.starts_with(MSG_HEADER) {
                return;
            }
        } else if !has_audio(&msg) && !has_image(&msg) {
            // No text, no audio, no image and is_from_me — non-content echo, skip
            return;
        }
    }

    // Build message content: text, image, audio, document, video, sticker, or
    // one of the non-media types (#1410, #1483).
    let has_img = has_image(&msg);
    let has_aud = has_audio(&msg);
    let has_doc = has_document(&msg);
    let has_vid = has_video(&msg);
    let has_stk = has_sticker(&msg);
    let text = extract_text(&msg);
    // #1488: a media URL expires, and the server will re-upload the blob if
    // asked with the message's own coordinates. Built once and handed to every
    // downloader so an old photo comes back instead of erroring.
    let media_ctx = super::media_retry::MediaContext {
        msg_id: info.id.clone(),
        chat: info.source.chat.clone(),
        is_from_me: info.source.is_from_me,
        participant: if info.source.is_group {
            Some(info.source.sender.clone())
        } else {
            None
        },
    };

    // Location, contact card and inbound reaction carry no blob to download;
    // they turn straight into a line of text for the agent.
    let non_media = describe_non_media(&msg);
    // #1482: a poll vote carries no text either, but unlike the types above it
    // cannot be decoded as a pure function: the vote is encrypted, and opening
    // it needs the poll's stored message secret and the client's LID/PN
    // resolution. A vote we cannot label yields None and is logged there.
    let poll_vote = match unwrap_message(&msg).poll_update_message.as_option() {
        Some(update) => {
            let voter = if info.push_name.trim().is_empty() {
                phone.clone()
            } else {
                info.push_name.trim().to_string()
            };
            super::poll::decode_vote(
                &client,
                &wa_state,
                update,
                &info.source.chat,
                &info.source.sender,
                &voter,
            )
            .await
        }
        None => None,
    };

    // Require at least one thing we can act on. Everything else really is
    // noise (receipts, protocol messages) and returning is correct.
    if text.is_none()
        && !has_img
        && !has_aud
        && !has_doc
        && !has_vid
        && !has_stk
        && non_media.is_none()
        && poll_vote.is_none()
    {
        return;
    }

    // Passively capture message for channel history (groups and DMs)
    if let Some(ref t) = text
        && !t.is_empty()
    {
        let chat_id = format!("{}", info.source.chat);
        let is_group = info.source.is_group;
        let push_name = info.push_name.clone();
        let cm = DbChannelMessage::new(
            "whatsapp".into(),
            chat_id,
            if is_group {
                Some(format!("{}", info.source.chat))
            } else {
                None
            },
            phone.clone(),
            push_name,
            t.clone(),
            "text".into(),
            None,
        );
        if let Err(e) = channel_msg_repo.insert(&cm).await {
            tracing::warn!("Failed to store WhatsApp channel message: {e}");
        }
    }

    // Read latest config from watch channel — single source of truth
    let cfg = config_rx.borrow().clone();
    let wa_cfg = &cfg.channels.whatsapp;
    let idle_timeout_hours = wa_cfg.session_idle_hours;
    let voice_config = cfg.voice_config();

    // #1525: recovered history frames must not wake the agent. Offline sync
    // and PDO recovery arrive through this same pipeline, flagged on the
    // message info; for opted-in chats they are stored (windowed, tagged
    // `imported`, deduped by platform id) and the handler ends here.
    // Non-opted-in chats keep exactly their existing handling — the feature
    // changes only what it was added for.
    {
        let recovered = info.is_offline || info.unavailable_request_id.is_some();
        if recovered && !wa_cfg.history_import_chats.is_empty() {
            let hist_key = format!("{}", info.source.chat);
            if super::history::opted_in(&wa_cfg.history_import_chats, &hist_key) {
                if let Some(t) = text.as_deref()
                    && !t.is_empty()
                    && super::history::in_window(info.timestamp, chrono::Utc::now())
                {
                    let pmid = info.id.to_string();
                    match channel_msg_repo
                        .content_by_platform_message_id("whatsapp", &hist_key, &pmid)
                        .await
                    {
                        Ok(None) => {
                            let cm = DbChannelMessage {
                                created_at: info.timestamp,
                                ..DbChannelMessage::new(
                                    "whatsapp".into(),
                                    hist_key.clone(),
                                    if info.source.is_group {
                                        Some(hist_key.clone())
                                    } else {
                                        None
                                    },
                                    phone.clone(),
                                    info.push_name.clone(),
                                    t.to_string(),
                                    super::history::IMPORTED_TYPE.into(),
                                    Some(pmid),
                                )
                            };
                            if let Err(e) = channel_msg_repo.insert(&cm).await {
                                tracing::warn!("whatsapp history: import capture failed: {e}");
                            }
                        }
                        Ok(Some(_)) => {
                            // An earlier frame of the same sync already stored
                            // this message — id-level dedupe, nothing to add.
                        }
                        Err(e) => {
                            tracing::warn!("whatsapp history: dedupe lookup failed: {e}")
                        }
                    }
                }
                return;
            }
        }
    }

    // SECURITY: owner / self-chat authorization. The bot pairs AS the owner's
    // account, so the owner's messages arrive in the "Message Yourself"
    // self-chat (is_from_me, chat == sender). `wa_should_respond` accepts that
    // self-chat number-agnostically — a config/paired-number mismatch can never
    // lock the owner out — plus any explicitly allow-listed contact. Everything
    // else is dropped. Open mode when `allowed_phones` is empty.
    //
    // LID vs PN (#276): most DMs now arrive addressed by LID (the privacy id),
    // whose digits never equal a phone number, so matching only the raw sender
    // silently dropped every allow-listed contact. `sender_alt` carries the
    // other identity (the PN JID for a LID sender, from the stanza's
    // `sender_pn`) — the gate matches the allow/operator lists against both.
    let sender_user = phone.trim_start_matches('+').to_string();
    let sender_alt_user = info.source.sender_alt.as_ref().map(|j| j.user.to_string());
    // One of the pair, chosen the same way every time, for anything used as a
    // MAP KEY (#1533). `phone` is whichever address this stanza happened to
    // arrive on, so keying on it gives a contact two entries the moment the
    // addressing mode changes: two sessions, an approval that can never be
    // resolved, a photo album that never batches. See `identity.rs` for why
    // "prefer sender_alt" is the wrong rule rather than the obvious one.
    //
    // Deliberately NOT used for the allow/owner checks: those accept either
    // identity, which is a different question from picking one.
    let canonical_phone =
        super::identity::canonical_user(&info.source.sender, info.source.sender_alt.as_ref());
    let chat_user_part = chat_user(&info);
    if !wa_should_respond(
        wa_cfg.response_policy,
        info.source.is_from_me,
        &sender_user,
        sender_alt_user.as_deref(),
        &chat_user_part,
        &wa_cfg.allowed_phones,
        &wa_cfg.bot_owner,
    ) {
        tracing::debug!(
            "WhatsApp: ignoring message from={} (alt={:?}) chat={} is_from_me={}",
            sender_user,
            sender_alt_user,
            chat_user_part,
            info.source.is_from_me,
        );
        return;
    }

    // #1411: a tap on a SUGGESTION card is not an approval. The digit lives
    // here, captured inside the approval block below (the only scope with the
    // parsed tap id) and fed into `content` where the ordinary follow-up
    // router can claim it exactly like a typed "2".
    let mut suggestion_pick: Option<usize> = None;

    // Pending approval check: if a tool approval is waiting for this phone,
    // interpret this message as Yes / Always / No instead of routing to the agent.
    // Handles both button taps and plain text replies. #1411: a native-flow
    // tap answers as an InteractiveResponseMessage, not the ButtonsResponse
    // shape this used to read, so a tap on a card sent with
    // `interactive_buttons` on would have been parsed as nothing at all.
    {
        use crate::channels::whatsapp::WaApproval;

        let btn_id = super::interactive::parse_tap(unwrap_message(&msg));

        let choice: Option<WaApproval> = if let Some(id) = btn_id.as_deref() {
            match id {
                "wa_approve_yes" => Some(WaApproval::Yes),
                "wa_approve_always" => Some(WaApproval::Always),
                "wa_approve_yolo" => Some(WaApproval::Yolo),
                "wa_approve_no" => Some(WaApproval::No),
                _ => None,
            }
        } else if let Some(raw_text) = extract_text(&msg) {
            let answer = raw_text.trim().to_lowercase();
            if matches!(answer.as_str(), "yes" | "y" | "sim" | "s") {
                Some(WaApproval::Yes)
            } else if matches!(answer.as_str(), "always" | "sempre") {
                Some(WaApproval::Always)
            } else if matches!(answer.as_str(), "yolo") {
                Some(WaApproval::Yolo)
            } else if matches!(answer.as_str(), "no" | "n" | "nao" | "não") {
                Some(WaApproval::No)
            } else {
                None
            }
        } else {
            None
        };

        // #1411: a tap on a SUGGESTION card is not an approval. Capture the
        // button's number here (the only scope with the parsed tap id); edit 3
        // feeds it into `content` where the ordinary follow-up router can claim
        // it exactly like a typed "2".
        if choice.is_none()
            && let Some(n) = btn_id
                .as_deref()
                .and_then(super::interactive::parse_suggestion_tap)
        {
            suggestion_pick = Some(n);
        }

        // OC-01: an approval prompt is keyed by sender phone, so an allowlisted
        // non-owner who caused the tool could approve or YOLO their own call. Gate
        // the resolution on true ownership (canonical resolver, so an empty
        // allowlist is unconfigured/deny, not "everyone is owner"). A non-owner
        // reply is dropped, not resolved.
        if let Some(c) = choice {
            // Either identity is accepted, matching the gate further down this
            // function. A LID-addressed owner replying "yes" arrives as digits
            // that are not their phone number, so checking `phone` alone
            // refused the owner's own approval and the tool call then timed out
            // after five minutes (#1533). This needs no canonical value: the
            // question is "is this person the owner", not "which of their two
            // names do we file them under".
            let is_owner =
                crate::config::owner::is_owner(&wa_cfg.allowed_phones, &wa_cfg.bot_owner, &phone)
                    || sender_alt_user.as_deref().is_some_and(|alt| {
                        crate::config::owner::is_owner(
                            &wa_cfg.allowed_phones,
                            &wa_cfg.bot_owner,
                            alt,
                        )
                    });
            if !is_owner {
                tracing::warn!(
                    "WhatsApp: non-owner {} replied approval {:?} — refused (OC-01)",
                    phone,
                    c
                );
                return;
            }
            if wa_state
                .resolve_pending_approval(&canonical_phone, c)
                .await
                .is_some()
            {
                tracing::info!("WhatsApp: approval from {}: {:?}", phone, c);
                if c == WaApproval::Always {
                    crate::utils::persist_auto_session_policy();
                } else if c == WaApproval::Yolo {
                    crate::utils::persist_auto_always_policy();
                }
                return;
            }
        }
    }

    let text_preview = text
        .as_deref()
        .map(|t| truncate_str(t, 50))
        .unwrap_or("[image]");
    tracing::info!("WhatsApp: message from {}: {}", phone, text_preview);

    // Audio/voice note → show typing immediately and transcribe
    if has_aud
        && voice_config.stt_enabled
        && let Err(e) = client.chatstate().send_composing(&info.source.chat).await
    {
        tracing::warn!(error = %e, "failed to send WhatsApp composing indicator");
    }
    let mut content;
    if has_aud
        && voice_config.stt_enabled
        && let Some(audio_bytes) = download_audio(&msg, &client, &media_ctx).await
    {
        // Durable copy of the voice note into the channel-attachment store
        // (#1729), annotated so the agent can re-read the raw audio later.
        let stored_audio =
            crate::channels::persist_channel_attachment("whatsapp", "voice-note.ogg", &audio_bytes);
        match crate::channels::voice::transcribe(audio_bytes, &voice_config).await {
            Ok(transcript) => {
                tracing::info!(
                    "WhatsApp: transcribed voice: {}",
                    truncate_str(&transcript, 80)
                );
                content = transcript;
            }
            Err(e) => {
                tracing::error!("WhatsApp: STT error: {e}");
                content = text.unwrap_or_default();
            }
        }
        if let Some(p) = stored_audio {
            content.push_str(&format!("\n\n[file: {}]", p.display()));
        }
    } else {
        content = text.unwrap_or_default();
    }

    // Download image if present, use photo batching for multi-image support
    if has_img
        && !has_aud
        && let Some((img_bytes, img_mime, img_fname)) =
            download_image(&msg, &client, &media_ctx).await
    {
        // Durable copy into the channel-attachment store (#1729) before the
        // funnel consumes the bytes; the path is annotated onto the final
        // content below so the agent can re-open the original image.
        let stored_img =
            crate::channels::persist_channel_attachment("whatsapp", &img_fname, &img_bytes);
        use crate::utils::{inject_file_content, process_file_with_vision};
        let cfg = crate::config::Config::load();
        if let Ok(cfg) = cfg {
            let fc = process_file_with_vision(&img_bytes, &img_mime, &img_fname, &cfg);
            let (injected, _) = inject_file_content(&fc);
            if !injected.is_empty() {
                // Buffer the image marker for batching
                let caption = extract_text(&msg);
                wa_state
                    .buffer_photo(&canonical_phone, injected, caption)
                    .await;

                // Reset debounce timer
                let token = wa_state.reset_photo_debounce(&canonical_phone).await;

                // Wait for debounce to expire
                if !wa_state.wait_photo_debounce(&token).await {
                    // Cancelled by another incoming photo, return early
                    return;
                }

                // Debounce expired, drain all buffered photos
                let (markers, first_caption) = wa_state.drain_photo_buffer(&canonical_phone).await;
                wa_state.cleanup_photo_debounce(&canonical_phone).await;

                if markers.is_empty() {
                    return;
                }

                // Combine all image markers
                content = markers.join("\n\n");

                // Prepend caption if present
                if let Some(caption) = first_caption
                    && !caption.trim().is_empty()
                {
                    content = format!("{}\n\n{}", caption.trim(), content);
                }

                // Annotate the durable copy's path (#1729) so the agent can
                // re-open the original image bytes later.
                if let Some(p) = stored_img {
                    content.push_str(&format!("\n\n[file: {}]", p.display()));
                }
            }
        }
    }

    // Stickers are WebP images: same funnel as an inbound photo (#1483).
    if has_stk
        && !has_img
        && !has_aud
        && let Some((bytes, mime, fname)) = download_sticker(&msg, &client, &media_ctx).await
    {
        // Durable copy into the channel-attachment store (#1729).
        let stored = crate::channels::persist_channel_attachment("whatsapp", &fname, &bytes);
        use crate::utils::{inject_file_content, process_file_with_vision};
        if let Ok(cfg) = crate::config::Config::load() {
            let fc = process_file_with_vision(&bytes, &mime, &fname, &cfg);
            let injected = inject_file_content(&fc).0;
            if !injected.is_empty() {
                content.push_str(&format!("\n\n{injected}"));
            }
        }
        if let Some(p) = stored {
            content.push_str(&format!("\n\n[file: {}]", p.display()));
        }
    }

    // Video: stored and analysed through the same funnel as an image (#1410).
    if has_vid
        && !has_aud
        && let Some((bytes, mime, fname)) = download_video(&msg, &client, &media_ctx).await
    {
        // Durable copy into the channel-attachment store (#1729).
        let stored = crate::channels::persist_channel_attachment("whatsapp", &fname, &bytes);
        use crate::utils::{inject_file_content, process_file_with_vision};
        if let Ok(cfg) = crate::config::Config::load() {
            let fc = process_file_with_vision(&bytes, &mime, &fname, &cfg);
            let injected = inject_file_content(&fc).0;
            if !injected.is_empty() {
                content.push_str(&format!("\n\n{injected}"));
            }
        }
        if let Some(p) = stored {
            content.push_str(&format!("\n\n[file: {}]", p.display()));
        }
    }

    // Poll vote: a decoded line naming the voter and what they picked (#1482).
    // The labels are kept for the follow-up selector below: a suggestion poll
    // votes by label, and the numeric selector can never match one (#1616).
    let voted_labels = poll_vote.as_ref().map(|v| v.chosen.clone());
    if let Some(vote) = poll_vote {
        if content.trim().is_empty() {
            content = vote.line;
        } else {
            content.push_str(&format!("\n\n{}", vote.line));
        }
    }

    // Location, contact card, inbound reaction: no blob, just a line (#1483).
    if let Some(extract) = non_media {
        if content.trim().is_empty() {
            content = extract;
        } else {
            content.push_str(&format!("\n\n{extract}"));
        }
    }

    // Handle document attachment
    if has_doc
        && !has_aud
        && !has_img
        && let Some((bytes, mime, fname)) = download_document(&msg, &client, &media_ctx).await
    {
        // Durable copy into the channel-attachment store (#1729).
        let stored = crate::channels::persist_channel_attachment("whatsapp", &fname, &bytes);
        use crate::utils::{inject_file_content, process_file_with_vision};
        let cfg = crate::config::Config::load();
        if let Ok(cfg) = cfg {
            let fc = process_file_with_vision(&bytes, &mime, &fname, &cfg);
            let injected = inject_file_content(&fc).0;
            if !injected.is_empty() {
                content.push_str(&format!("\n\n{injected}"));
            }
        }
        if let Some(p) = stored {
            content.push_str(&format!("\n\n[file: {}]", p.display()));
        }
    }

    // #1411: a suggestion-card tap carries no caption; stand the captured
    // digit in for the typed reply the follow-up router below expects. A tap
    // that arrives with its own caption keeps the caption (typed wins).
    if let Some(n) = suggestion_pick
        && content.trim().is_empty()
    {
        content = n.to_string();
    }

    if content.is_empty() {
        return;
    }

    // The bot pairs AS the owner, so the owner's own messages arrive in the
    // "Message Yourself" self-chat addressed by LID (e.g. 236927743742100),
    // while the connection greeting and config identify the owner by PN
    // (351933536442). Keying the session by the raw sender would create a
    // SECOND owner session (wa-<LID>) separate from the greeting's (wa-<PN>) —
    // the "two sessions every time" bug. Collapse the owner's self-chat to the
    // configured owner number (the same one the greeting uses) so the owner
    // always resolves to exactly one session regardless of PN/LID addressing.
    let is_owner_self_chat = info.source.is_from_me && sender_user == chat_user_part;
    // The PAIRED account's own number (captured at PairSuccess into owner_jid) is
    // the correct self-chat target. allowed_phones[0] is only right when it IS
    // the paired account (self-DM); for a number paired to serve other people's
    // DMs it may be an allowed CONTACT, so prefer owner_jid and fall back to the
    // first allowed phone.
    let owner_number = match wa_state.owner_jid().await {
        Some(jid) => jid
            .split('@')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .trim_start_matches('+')
            .to_string(),
        None => wa_cfg
            .allowed_phones
            .first()
            .map(|a| a.trim_start_matches('+').to_string())
            .unwrap_or_default(),
    };
    let owner_number = if owner_number.is_empty() {
        None
    } else {
        Some(owner_number)
    };
    let session_phone = match (is_owner_self_chat, &owner_number) {
        (true, Some(num)) => num.clone(),
        // Preferring `sender_alt` outright reads as "prefer the phone number"
        // and is only that for a LID-addressed sender; for a PN-addressed one
        // it prefers the LID, so the session was keyed by an opaque id and its
        // label read "WhatsApp: 83911178752119" (#1533).
        _ => canonical_phone.clone(),
    };

    // Where ALL agent output for this turn is sent. For the owner self-chat the
    // owner's LID JID (<lid>@lid) is rejected by the server with error 400 (its
    // LID-form device session can't be established, so the encrypt fan-out skips
    // that device and the whole stanza is refused), while the owner's PN
    // (<num>@s.whatsapp.net) delivers — same target the greeting uses. Both the
    // streamed intermediate text and the final reply must use this, or a
    // streamed turn (which suppresses the final reply) goes only to the LID and
    // is dropped. Non-owner chats keep their original chat JID.
    //
    // LID-addressed DMs (#276 follow-up): non-owner contacts also send from LID
    // JIDs now, and the same encrypt fan-out issue applies. When the chat JID is
    // a LID and sender_alt carries the PN, convert to PN JID so the response
    // can be delivered.
    let reply_target: wacore_binary::jid::Jid = match (is_owner_self_chat, &owner_number) {
        (true, Some(num)) => format!("{num}@s.whatsapp.net")
            .parse()
            .unwrap_or_else(|_| info.source.chat.clone()),
        _ => {
            // For non-owner DMs, if the chat JID is a LID (privacy-preserving),
            // convert to PN JID using sender_alt so the response can be delivered.
            // LID JIDs can't receive messages (same encrypt fan-out issue as owner self-chat).
            if !info.source.is_group && info.source.chat.is_lid() {
                if let Some(ref alt) = sender_alt_user {
                    let alt_jid = format!("{alt}@s.whatsapp.net");
                    if let Ok(jid) = alt_jid.parse() {
                        jid
                    } else {
                        info.source.chat.clone()
                    }
                } else {
                    info.source.chat.clone()
                }
            } else {
                info.source.chat.clone()
            }
        }
    };

    // Second line (#2176): the drop above covers the inbound path, and this
    // covers any future path that computes a target. A broadcast JID rejects
    // every `send_message`, so binding a session to one costs a full turn and
    // poisons every later progress event in that session.
    if reply_target.is_status_broadcast() {
        tracing::warn!(
            target: "whatsapp",
            "refusing to reply into status@broadcast; dropping this stanza"
        );
        return;
    }

    // is_owner gates /new archiving and owner-only flows. The self-chat is
    // always the owner even though its LID sender won't match the configured PN.
    //
    // `allowed.is_empty()` used to be an owner condition (OC-02): an empty
    // allowed_phones under response_policy=auto answered everyone AND elevated
    // every contact to owner, handing them /evolve, /exit, /cd. Open
    // DM access (answering an unlisted contact) is a separate policy decision
    // and is not the same as ownership, so it is gone from here. Ownership is
    // now the self-chat, the configured owner number, or the canonical owner
    // resolver over allowed_phones + bot_owner.
    let is_owner = is_owner_self_chat
        || owner_number.as_deref() == Some(phone.as_str())
        || sender_alt_user
            .as_deref()
            .is_some_and(|alt| owner_number.as_deref() == Some(alt))
        || crate::config::owner::is_owner(&wa_cfg.allowed_phones, &wa_cfg.bot_owner, &phone)
        || sender_alt_user.as_deref().is_some_and(|alt| {
            crate::config::owner::is_owner(&wa_cfg.allowed_phones, &wa_cfg.bot_owner, alt)
        });

    // Sessions are keyed by a stable `[chat:wa-<phone>]` suffix so auto-rename
    // of the visible label still resolves to the same row (issue #121).
    let session_id = {
        use crate::channels::session_resolve;
        let legacy_title = format!("WhatsApp: {}", session_phone);
        let suffix = session_resolve::chat_id_suffix(&format!("wa-{session_phone}"));
        let session_title = format!("{legacy_title} {suffix}");

        match session_resolve::resolve_or_create_channel_session(
            &session_svc,
            &suffix,
            &legacy_title,
            &session_title,
            idle_timeout_hours,
            "WhatsApp",
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::error!("WhatsApp: failed to resolve session: {e:#} (#442)");
                let reply = waproto::whatsapp::Message {
                    conversation: Some(format!(
                        "⚠️ Could not load this chat's session ({e}). Your history is \
                         intact and this message was NOT processed. Try again, or send \
                         /new if you deliberately want a fresh session."
                    )),
                    ..Default::default()
                };
                if let Err(e) = client.send_message(reply_target.clone(), reply).await {
                    tracing::warn!(error = %e, "failed to send WhatsApp message");
                }
                return;
            }
        }
    };

    // Session gate (#1051, ADR-003): mark group sessions so memory_search
    // keeps external index content out of them by default.
    if info.source.is_group {
        crate::memory::mark_session_shared(session_id);
    }

    // Remember which chat this session is handled in, so a finished background
    // task can resume the right chat (#731).
    wa_state
        .register_session_jid(session_id, reply_target.to_string())
        .await;

    // The outbox release for this turn happens after the turn slot is claimed
    // (#2179, see below): a message that loses the claim is queued, and a
    // queued message must not clear the live turn's entry.

    // Claim this session's background-task completions for WhatsApp: a completion
    // must be delivered by the surface that OWNS the session, not by whichever
    // service happened to run the command (#940).
    crate::brain::agent::service::session_routes::claim_for_channel(
        session_id,
        agent.message_enqueue_callback(),
    );

    // Optional follow-up suggestions (#600): a bare numeric reply selects the
    // matching suggestion — rewrite the turn text to it. Any other message
    // clears the stale set so an old number can't fire later.
    // A poll vote names its option by label (#1616); a typed reply names it by
    // number (#600). Try whichever this turn actually is, then fall through to
    // clearing so a stale set cannot fire on a later message.
    let voted_pick = match voted_labels.as_deref() {
        Some([label]) => wa_state.take_followup_by_label(session_id, label).await,
        _ => None,
    };
    if let Some(picked) = voted_pick {
        content = picked;
    } else if let Some(picked) = wa_state
        .take_followup_by_reply(session_id, content.trim())
        .await
    {
        content = picked;
    } else {
        wa_state.clear_pending_followups(session_id).await;
    }

    // Fast-cancel: any recognised stop intent, in any supported language (#965).
    //
    // Cancellation is scoped to explicit stop requests. An ordinary follow-up
    // no longer cancels anything: it queues behind the live turn through the
    // turn slot (#2179). Channel commands like /models, /help, /usage, /new
    // must NEVER abort an in-flight task: switching models applies to the next
    // run, it does not drop current work (#266).
    if crate::utils::stop_intent::is_stop_command_or_intent(&content) {
        wa_state.cancel_session(session_id).await;
        let reply = waproto::whatsapp::Message {
            conversation: Some("Operation cancelled.".to_string()),
            ..Default::default()
        };
        if let Err(e) = client.send_message(reply_target.clone(), reply).await {
            tracing::warn!(error = %e, "failed to send WhatsApp message");
        }
        return;
    }

    // Restore session's own provider (each session keeps its provider independently)
    let session_meta = session_svc.get_session(session_id).await.ok().flatten();
    crate::channels::commands::sync_provider_for_session(
        &agent,
        session_id,
        session_meta
            .as_ref()
            .and_then(|s| s.provider_name.as_deref()),
        session_meta.as_ref().and_then(|s| s.model.as_deref()),
    )
    .await;

    // `/respond_to` writes Telegram's section when it reaches the shared parser
    // with no chat id. WhatsApp has no respond mode, so it only says so (#2013).
    if let Some(reply) = crate::channels::respond_to_scope::respond_to_outside_telegram(
        &content, is_owner, "WhatsApp", None,
    ) {
        let reply_msg = waproto::whatsapp::Message {
            conversation: Some(reply),
            ..Default::default()
        };
        if let Err(e) = client.send_message(reply_target.clone(), reply_msg).await {
            tracing::warn!(error = %e, "failed to send WhatsApp message");
        }
        return;
    }

    // ── Channel commands (/help, /usage, /models, /stop) ────────────────────
    {
        use crate::channels::commands::{self, ChannelCommand};
        let cmd =
            commands::handle_command(&content, session_id, &agent, &session_svc, is_owner, None)
                .await;

        // Handle simple text-response commands (Help, Usage, Evolve, Doctor, etc.)
        if let Some(reply_text) = commands::try_execute_text_command(&cmd).await {
            let reply = waproto::whatsapp::Message {
                conversation: Some(reply_text),
                ..Default::default()
            };
            if let Err(e) = client.send_message(reply_target.clone(), reply).await {
                tracing::warn!(error = %e, "failed to send WhatsApp message");
            }
            return;
        }

        match cmd {
            ChannelCommand::Models(resp) => {
                // WhatsApp has no inline buttons — send plain text list
                let reply = waproto::whatsapp::Message {
                    conversation: Some(resp.text),
                    ..Default::default()
                };
                if let Err(e) = client.send_message(reply_target.clone(), reply).await {
                    tracing::warn!(error = %e, "failed to send WhatsApp message");
                }
                return;
            }
            ChannelCommand::NewSession => {
                let session_title = format!("WhatsApp: {}", phone);
                // The new session inherits its working directory from the
                // session that received this /new (same chat), not the global
                // most-recent session (#263).
                let prior_session = session_svc
                    .find_session_by_title(&session_title)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::error!("WhatsApp: /new prior-session lookup failed: {e:#}");
                        None
                    });
                // Archive the previous session on /new, except for the owner —
                // owner sessions stay non-archived so they remain visible in
                // /sessions for history review. Guest sessions get archived
                // so the next title lookup resolves cleanly to the new row.
                if !is_owner
                    && let Some(old) = prior_session.as_ref()
                    && let Err(e) = session_svc.archive_session(old.id).await
                {
                    tracing::error!("WhatsApp: failed to archive old session {}: {}", old.id, e);
                }
                match crate::channels::session_init::create_channel_session(
                    &session_svc,
                    Some(session_title),
                    prior_session.as_ref(),
                )
                .await
                {
                    Ok(new_session) => {
                        if is_owner {
                            *shared_session.lock().await = Some(new_session.id);
                        }
                        // Sync provider for the new session so baseline is accurate
                        let new_meta = session_svc.get_session(new_session.id).await.ok().flatten();
                        crate::channels::commands::sync_provider_for_session(
                            &agent,
                            new_session.id,
                            new_meta.as_ref().and_then(|s| s.provider_name.as_deref()),
                            new_meta.as_ref().and_then(|s| s.model.as_deref()),
                        )
                        .await;
                        let baseline = agent.base_context_tokens();
                        let ctx_max = agent.context_limit_for_session(new_session.id);
                        let footer = crate::utils::format_ctx_footer(baseline, ctx_max, None);
                        let msg_text = format!("✅ New session started.\n\n{footer}");
                        let reply = waproto::whatsapp::Message {
                            conversation: Some(msg_text),
                            ..Default::default()
                        };
                        if let Err(e) = client.send_message(reply_target.clone(), reply).await {
                            tracing::warn!(error = %e, "failed to send WhatsApp message");
                        }
                        tracing::info!(
                            "WhatsApp /new: sent ctx footer='{}' (baseline={}, ctx_max={})",
                            footer,
                            baseline,
                            ctx_max,
                        );
                    }
                    Err(e) => {
                        tracing::error!("WhatsApp: failed to create session: {}", e);
                        let reply = waproto::whatsapp::Message {
                            conversation: Some("Failed to create session.".to_string()),
                            ..Default::default()
                        };
                        if let Err(e) = client.send_message(reply_target.clone(), reply).await {
                            tracing::warn!(error = %e, "failed to send WhatsApp message");
                        }
                    }
                }
                return;
            }
            ChannelCommand::Sessions(resp) => {
                // WhatsApp has no inline buttons — send plain text list
                let reply = waproto::whatsapp::Message {
                    conversation: Some(resp.text),
                    ..Default::default()
                };
                if let Err(e) = client.send_message(reply_target.clone(), reply).await {
                    tracing::warn!(error = %e, "failed to send WhatsApp message");
                }
                return;
            }
            ChannelCommand::Stop => {
                let cancelled = wa_state.cancel_session(session_id).await;
                let text = if cancelled {
                    "Operation cancelled."
                } else {
                    "No operation in progress."
                };
                let reply = waproto::whatsapp::Message {
                    conversation: Some(text.to_string()),
                    ..Default::default()
                };
                if let Err(e) = client.send_message(reply_target.clone(), reply).await {
                    tracing::warn!(error = %e, "failed to send WhatsApp message");
                }
                return;
            }
            ChannelCommand::StopTarget(_) => {
                // #2108: targeting another session is Telegram-only for now.
                // Answer explicitly instead of falling through to the agent,
                // which would look like the command ran.
                let reply = waproto::whatsapp::Message {
                    conversation: Some(
                        "Targeted stop is Telegram-only. Use /stop here.".to_string(),
                    ),
                    ..Default::default()
                };
                if let Err(e) = client.send_message(reply_target.clone(), reply).await {
                    tracing::warn!(error = %e, "failed to send WhatsApp message");
                }
                return;
            }
            ChannelCommand::Compact => {
                let status = waproto::whatsapp::Message {
                    conversation: Some("⏳ Compacting context...".to_string()),
                    ..Default::default()
                };
                if let Err(e) = client.send_message(reply_target.clone(), status).await {
                    tracing::warn!(error = %e, "failed to send WhatsApp message");
                }
                content =
                    "[SYSTEM: Compact context now. Summarize this conversation for continuity.]"
                        .to_string();
            }
            ChannelCommand::ClearContext => {
                // No agent turn: the marker row is the whole operation (#1585).
                let reply = match agent.clear_context(session_id).await {
                    Ok(receipt) => receipt.user_line(),
                    Err(e) => {
                        tracing::error!("/clear failed: {e}");
                        format!("/clear did nothing, the context is unchanged: {e}")
                    }
                };
                let status = waproto::whatsapp::Message {
                    conversation: Some(reply),
                    ..Default::default()
                };
                if let Err(e) = client.send_message(reply_target.clone(), status).await {
                    tracing::warn!(error = %e, "failed to send WhatsApp clear receipt");
                }
                return;
            }
            ChannelCommand::UserPrompt(prompt) => {
                content = prompt;
                // fall through to agent with the prompt as the message
            }
            ChannelCommand::NotACommand => {}
            // Help, Usage, Evolve, Doctor, UserSystem handled by try_execute_text_command above
            ChannelCommand::Profiles(resp) => {
                let reply = waproto::whatsapp::Message {
                    conversation: Some(resp.text.clone()),
                    ..Default::default()
                };
                if let Err(e) = client.send_message(reply_target.clone(), reply).await {
                    tracing::warn!(error = %e, "failed to send WhatsApp message");
                }
                return;
            }
            _ => {}
        }
    }

    // Extract replied-to message context so the agent knows what the user is referencing.
    let reply_context = extract_reply_context(&msg);

    // Build the human-readable display text (used for DB persistence + TUI).
    // Owner DMs keep the bare text; non-owner / group messages prefix with
    // sender so OpenCrabs sessions stay readable without the LLM-only
    // metadata brackets.
    let display_text = if is_owner && !info.source.is_group {
        content.clone()
    } else {
        let name = info.push_name.trim();
        let sender = if name.is_empty() {
            format!("+{}", phone)
        } else {
            name.to_string()
        };
        format!("{sender}: {content}")
    };

    // Name the current sender. In a group this always runs — even for the owner
    // — because the history block below carries other members' names, and
    // without the label the model addresses the sender by one of those (#682).
    // One-to-one chats keep the old shape: nobody else's name is in play.
    let agent_input = if info.source.is_group {
        let name = info.push_name.trim().to_string();
        let display = if name.is_empty() {
            format!("+{}", phone)
        } else {
            name
        };
        let group = info.source.chat.to_string();
        let group_id = group.split('@').next().unwrap_or(&group).to_string();
        let role = if is_owner { "owner" } else { "user" };
        format!(
            "{}\n{}",
            group_history::current_sender_label(
                "WhatsApp group",
                &group_id,
                &display,
                &format!(" (+{phone})"),
                role,
            ),
            content
        )
    } else if !is_owner {
        let name = info.push_name.trim().to_string();
        let from = if name.is_empty() {
            format!("+{}", phone)
        } else {
            format!("{} (+{})", name, phone)
        };
        format!("[WhatsApp message from {}]\n{}", from, content)
    } else {
        content
    };

    // Prepend reply context if the user is replying to a specific message.
    let agent_input = if let Some(ref ctx) = reply_context {
        format!("{ctx}\n{agent_input}")
    } else {
        agent_input
    };

    // Inject recent group history so the agent has full conversation context.
    // Deduped against the live session window: after a compaction the model
    // still holds those turns, so re-sending all 30 every turn was pure waste
    // (#1618, the WhatsApp half of #133).
    let agent_input = if info.source.is_group {
        let chat_id_str = info.source.chat.to_string();
        let fetched = channel_msg_repo
            .recent(Some("whatsapp"), &chat_id_str, 30, None, None)
            .await
            .unwrap_or_default();
        match group_history::build_preamble(
            session_svc.pool(),
            session_id,
            fetched,
            "group",
            "WhatsApp",
        )
        .await
        {
            Some(preamble) => format!("{preamble}\n{agent_input}"),
            None => agent_input,
        }
    } else {
        agent_input
    };

    // Tell the LLM its text response is automatically delivered to the chat,
    // so it should NOT use whatsapp_send for plain replies — but the tool
    // EXISTS and is the only path for media, polls, and reactions (#1489:
    // the old text denied the tool outright while catalog.rs registered it,
    // so photo requests fell back to generic tools or bare paths).
    // Surface the chat JID so the agent can target THIS chat for cron reports
    // without guessing (#533, mirror of upstream #510).
    let chat_id = reply_target.to_string();
    let agent_input = format!(
        "[Channel: WhatsApp (chat_id: {chat_id}) — your text response is automatically sent to this chat. \
         Do NOT call whatsapp_send to deliver your answer. Only use whatsapp_send for: \
         sending to a different chat_id, media (photo/video/audio/document/sticker), polls, \
         reactions, quote-replies, locations, contacts, or deleting a message. \
         ORDERING: send any files/documents/photos FIRST, then write your final text — \
         the turn must never end on a bare attachment with no closing text after it.]\n{agent_input}"
    );

    // ── Turn slot (#2179) ─────────────────────────────────────────────────
    // One turn per session, claimed atomically BEFORE the typing setup, the
    // stream setup and the agent call. A message that loses the claim is
    // queued for the live turn (injected between tool rounds, flushed at turn
    // end) instead of forking a second concurrent tool loop on this session.
    // `store_cancel_token` no longer kills the running turn, so this slot is
    // the only thing standing between a follow-up and two turns racing on one
    // transcript.
    let turn_guard = match wa_state.try_begin_turn(session_id) {
        Some(guard) => guard,
        None => {
            tracing::info!(
                "WhatsApp: message arrived mid-turn on session {session_id} — queued for \
                 injection between tool rounds (#2179)"
            );
            wa_state.enqueue_turn_message(
                session_id,
                crate::brain::agent::QueuedUserMessage {
                    context_text: agent_input,
                    display_text,
                    origin: crate::brain::agent::PushOrigin::Ingress,
                    bg_meta: None,
                },
            );
            return;
        }
    };

    // A new inbound turn never edits the previous turn's message (#1614). The
    // outbox entry has to outlive its own turn (the final edit and the
    // completion reaction both run after the agent call), so the turn that
    // starts is the one that releases it. Skipping this let a follow-up sent
    // inside the 15-minute edit window overwrite the answer already on screen.
    // Only a turn that WON the slot reaches this: a queued message must leave
    // the live turn's entry alone, or its final edit lands as a new message.
    wa_state.begin_turn(session_id).await;

    // Typing indicator — send composing every 5 s while the agent thinks
    let typing_cancel = CancellationToken::new();
    // The agent-call token is created HERE rather than at the call because
    // the handover tail below must watch it: `/stop` is the event that should
    // end this indicator, and it cancels exactly this token (#1989). A
    // follow-up no longer cancels it: since #2179 it queues behind the turn.
    let cancel_token = CancellationToken::new();
    tokio::spawn({
        let client = client.clone();
        let chat_jid = reply_target.clone();
        let cancel = typing_cancel.clone();
        let stop = cancel_token.clone();
        let background = agent.background_manager();
        let agents = agent.subagent_manager();
        let state = wa_state.clone();
        async move {
            // #2181: check the connection before every attempt and back off on
            // failure instead of retrying every 5s forever. The first failure
            // of an episode is the only one worth a WARN.
            let mut failures: u32 = 0;
            loop {
                let delay = if !state.is_connected().await {
                    failures = failures.saturating_add(1);
                    tracing::debug!(
                        failures,
                        "WhatsApp not connected; skipping composing indicator"
                    );
                    match composing_retry_delay(failures) {
                        Some(d) => d,
                        None => break,
                    }
                } else {
                    match client.chatstate().send_composing(&chat_jid).await {
                        Ok(()) => {
                            failures = 0;
                            composing_retry_delay(0).unwrap_or(COMPOSING_BASE_DELAY)
                        }
                        Err(e) => {
                            failures = failures.saturating_add(1);
                            if failures == 1 {
                                tracing::warn!(
                                    error = %e,
                                    "failed to send WhatsApp composing indicator"
                                );
                            } else {
                                tracing::debug!(
                                    error = %e,
                                    failures,
                                    "composing indicator still failing"
                                );
                            }
                            match composing_retry_delay(failures) {
                                Some(d) => d,
                                None => break,
                            }
                        }
                    }
                };
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            // Keep composing past the end of the turn while this session still
            // has detached work (#812). Spawning a long background command ENDS
            // the turn, so without this the indicator dies at the moment the
            // user most needs a sign that something is happening. This tail
            // now runs through the shared tick (#1989): sub-agent work holds
            // the indicator too, and `/stop` or the next turn ends it instead
            // of leaving a second loop pinging under the new turn's indicator.
            crate::channels::typing_tick::tick_while_detached(
                background,
                agents,
                session_id,
                Some(stop.clone()),
                std::time::Duration::from_secs(5),
                // #1984: the tail is bounded even if some row leaks every
                // stuck-entry cleanup (the stop token already ends it on
                // /stop or a new turn, #1989).
                crate::channels::typing_tick::DEFAULT_TICK_CEILING,
                || async {
                    // #2181: the detached tail pings on its own cadence, so it
                    // needs the same connection check. A dead socket must not
                    // produce one WARN per tick.
                    if !state.is_connected().await {
                        return;
                    }
                    if let Err(e) = client.chatstate().send_composing(&chat_jid).await {
                        tracing::debug!(
                            error = %e,
                            "failed to send WhatsApp composing indicator"
                        );
                    }
                },
            )
            .await;
            // Paused only when the work ended on its own: after a `/stop` or
            // a new turn, the silence (or the new turn's composing) is owned
            // elsewhere, and a stale paused would snuff it.
            if !stop.is_cancelled()
                && let Err(e) = client.chatstate().send_paused(&chat_jid).await
            {
                tracing::warn!(error = %e, "failed to send WhatsApp paused indicator");
            }
        }
    });

    // Progress callback: forward intermediate texts (between tool-call
    // iterations) to WhatsApp in real time. The comment that used to sit here
    // claimed WhatsApp does not support message editing. It does, and the
    // 15-minute window covers virtually every turn (#1408), so chunks now grow
    // ONE living message instead of posting one message each. Images
    // (<<IMG:...>>) are stripped here - the main handler delivers them as
    // actual WhatsApp image messages.
    let was_streamed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sent_intermediates: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    // A single consumer task owns send/edit ordering for this turn. The
    // progress callback is synchronous and used to spawn one task per chunk,
    // which let two chunks race for the same edit and land out of order.
    let (stream_sink, stream_task) = {
        let rl_jid = reply_target.to_string();
        super::stream::spawn(super::stream::StreamConfig {
            client: client.clone(),
            jid: reply_target.clone(),
            session_id,
            state: wa_state.clone(),
            rate_limit: wa_cfg.rate_limit.clone(),
            is_owner: wa_cfg.is_owner(rl_jid.split('@').next().unwrap_or(&rl_jid)),
            header: MSG_HEADER.to_string(),
        })
    };
    let stream_sink_cb = stream_sink.clone();
    let progress_cb: ProgressCallback = {
        let client_cb = client.clone();
        let jid_cb = reply_target.clone();
        let was_streamed_cb = was_streamed.clone();
        let wa_state_cb = wa_state.clone();
        // Copied, not borrowed: the callback must be 'static, so `wa_cfg`
        // cannot be referenced inside it (#1411).
        let interactive_cb = wa_cfg.interactive_buttons;
        // #1407 gating for streamed text now lives in `stream.rs`, alongside
        // the send-or-edit choice it has to pace.
        Arc::new(move |session_id, event| match event {
            ProgressEvent::IntermediateText { text, .. } => {
                let (clean, _) = crate::utils::extract_img_markers(&text);
                let clean = redact_secrets(&clean);
                let clean = crate::utils::sanitize::strip_llm_artifacts(&clean);
                let clean = crate::utils::slack_fmt::markdown_to_mrkdwn(&clean);
                if !clean.trim().is_empty() {
                    // Pre-send dedup: skip if this exact text was already sent
                    let mut prev = sent_intermediates.lock().unwrap();
                    if prev.iter().any(|s| s == &clean) {
                        drop(prev);
                        return;
                    }
                    prev.push(clean.clone());
                    drop(prev);
                    was_streamed_cb.store(true, std::sync::atomic::Ordering::Relaxed);
                    // Ordering, rate limiting and the send-or-edit decision
                    // all live in the consumer task (#1408); this side only
                    // queues, so two fast chunks cannot race.
                    stream_sink_cb.push(clean);
                }
            }
            ProgressEvent::SelfHealingAlert { message } => {
                let client = client_cb.clone();
                let jid = jid_cb.clone();
                let alert = format!(
                    "{}\n\n🔧 {}",
                    MSG_HEADER,
                    crate::utils::sanitize::normalize_dashes(&message)
                );
                tokio::spawn(async move {
                    let msg = waproto::whatsapp::Message {
                        conversation: Some(alert),
                        ..Default::default()
                    };
                    if let Err(e) = client.send_message(jid, msg).await {
                        tracing::error!("WhatsApp: self-healing alert send failed: {}", e);
                    }
                });
            }
            ProgressEvent::RetryAttempt {
                attempt,
                max,
                reason,
            } => {
                let client = client_cb.clone();
                let jid = jid_cb.clone();
                let text = format!(
                    "{}\n\n⏳ Retry {}/{} — {}",
                    MSG_HEADER, attempt, max, reason
                );
                tokio::spawn(async move {
                    let msg = waproto::whatsapp::Message {
                        conversation: Some(text),
                        ..Default::default()
                    };
                    if let Err(e) = client.send_message(jid, msg).await {
                        tracing::error!("WhatsApp: retry attempt send failed: {}", e);
                    }
                });
            }
            ProgressEvent::ProviderSwitched {
                to_name, to_model, ..
            } => {
                let client = client_cb.clone();
                let jid = jid_cb.clone();
                let text = format!("{}\n\n🔄 Now using {}/{}", MSG_HEADER, to_name, to_model);
                tokio::spawn(async move {
                    let msg = waproto::whatsapp::Message {
                        conversation: Some(text),
                        ..Default::default()
                    };
                    if let Err(e) = client.send_message(jid, msg).await {
                        tracing::error!("WhatsApp: provider switched send failed: {}", e);
                    }
                });
            }
            // Optional follow-up suggestions (#600): a native-flow card when
            // the owner opted into `interactive_buttons` and the set fits the
            // 3-button cap (#1411), a single-choice poll when the set is
            // larger than that cap (#1616), and the numbered text list
            // otherwise. The typed path survives all three: a bare numeric
            // reply selects an option via the inbound router, the card body
            // carries the same instructions, and the poll question does too,
            // so a client that renders neither is never stuck. Anything else
            // clears the set.
            ProgressEvent::SuggestedOptions(options) if !options.is_empty() => {
                let client = client_cb.clone();
                let jid = jid_cb.clone();
                let wa = wa_state_cb.clone();
                let interactive = interactive_cb;
                let raw_options: Vec<String> = options.into_iter().map(|item| item.label).collect();
                tokio::spawn(async move {
                    super::followups::send_suggestions(
                        client,
                        jid,
                        wa,
                        session_id,
                        raw_options,
                        interactive,
                    )
                    .await;
                });
            }
            _ => {}
        })
    };

    // Build per-call approval callback.
    // If the user previously chose "Always (session)", auto-approve without asking.
    // Otherwise send a 3-button message (Yes / Always / No) and wait up to 5 min.
    // Copied, not borrowed: the callback is 'static and `wa_cfg` is a
    // reference into this turn's config snapshot.
    let interactive_buttons = wa_cfg.interactive_buttons;
    let approval_cb: ApprovalCallback = {
        use crate::channels::whatsapp::WaApproval;
        use crate::utils::check_approval_policy;

        let client = client.clone();
        let chat_jid = reply_target.clone();
        // Must match what `resolve_pending_approval` is called with, or the
        // reply looks up a key that was never registered (#1533).
        let phone_key = canonical_phone.clone();
        let wa_state = wa_state.clone();
        Arc::new(move |tool_info| {
            let client = client.clone();
            let chat_jid = chat_jid.clone();
            let phone_key = phone_key.clone();
            let wa_state = wa_state.clone();
            Box::pin(async move {
                // Respect config-level approval policy (single source of truth)
                if let Some(result) = check_approval_policy() {
                    return Ok(result);
                }

                // Redact secrets before display
                let safe_input = crate::utils::redact_tool_input(&tool_info.tool_input);
                let input_preview = serde_json::to_string_pretty(&safe_input).unwrap_or_default();
                let body = format!(
                    "🔐 *Tool Approval Required*\n\nTool: `{}`\n```\n{}\n```",
                    tool_info.tool_name,
                    truncate_str(&input_preview, 600),
                );

                // #1411: `ButtonsMessage` is deprecated and silently never
                // renders, which is why 2f15f1d1 removed it. NativeFlow is a
                // different path and may render, but an approval prompt that
                // does not is a message the owner cannot answer, so it stays
                // opt-in behind `interactive_buttons`. Either way the SAME
                // instructions ride in the text: a card that renders without
                // its buttons still tells the reader what to type.
                let prompt = format!(
                    "{}\n\n{}\n\nReply *yes*, *always* (session), *yolo* (permanent), or *no* (5 min timeout).",
                    MSG_HEADER, body
                );
                let text_msg = if interactive_buttons {
                    super::interactive::build(
                        &prompt,
                        Some("Tap a button, or reply with the word."),
                        &[
                            super::interactive::Button::new("wa_approve_yes", "Yes"),
                            super::interactive::Button::new("wa_approve_always", "Always"),
                            super::interactive::Button::new("wa_approve_no", "No"),
                        ],
                    )
                } else {
                    waproto::whatsapp::Message {
                        conversation: Some(prompt),
                        ..Default::default()
                    }
                };
                tracing::info!(
                    "WhatsApp approval: sending request for tool '{}' to {}",
                    tool_info.tool_name,
                    phone_key
                );
                if let Err(e) = client.send_message(chat_jid.clone(), text_msg).await {
                    tracing::error!("WhatsApp: failed to send approval request: {}", e);
                    return Ok((false, false));
                }

                let (tx, rx) = tokio::sync::oneshot::channel::<WaApproval>();
                wa_state
                    .register_pending_approval(phone_key.clone(), tx)
                    .await;
                tracing::info!(
                    "WhatsApp approval: registered pending for phone={}, waiting for response",
                    phone_key
                );

                match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
                    Ok(Ok(WaApproval::Yes)) => {
                        tracing::info!("WhatsApp approval: user approved (phone={})", phone_key);
                        Ok((true, false))
                    }
                    Ok(Ok(WaApproval::Always)) => {
                        tracing::info!(
                            "WhatsApp approval: user chose Always (phone={})",
                            phone_key
                        );
                        // Policy persistence happens once, at the owner-gated
                        // reply handler that resolves this oneshot (OC-01). This
                        // arm only fires because that handler already accepted
                        // the choice from the owner, so persisting again here
                        // would be a second, ungated write of the same value.
                        Ok((true, true))
                    }
                    Ok(Ok(WaApproval::Yolo)) => {
                        tracing::info!("WhatsApp approval: user chose YOLO (phone={})", phone_key);
                        Ok((true, true))
                    }
                    Ok(Ok(WaApproval::No)) => {
                        tracing::info!("WhatsApp approval: user denied (phone={})", phone_key);
                        Ok((false, false))
                    }
                    _ => {
                        tracing::warn!(
                            "WhatsApp: approval timed out or channel dropped — denying (phone={})",
                            phone_key
                        );
                        let timeout_msg = waproto::whatsapp::Message {
                            conversation: Some(format!(
                                "{}\n\n⏰ No response in 5 minutes — *{}* was denied.\n\nSend your message again and reply *yes*, *always*, or *no* when prompted.",
                                MSG_HEADER, tool_info.tool_name,
                            )),
                            ..Default::default()
                        };
                        if let Err(e) = client.send_message(chat_jid, timeout_msg).await {
                            tracing::warn!(error = %e, "failed to send WhatsApp message");
                        }
                        Ok((false, false))
                    }
                }
            })
        })
    };

    // Send to agent with WhatsApp approval + progress callbacks.
    // `cancel_token` itself is created further up, where the typing task
    // spawns: the handover tail must watch it too (#1989). This call still
    // wires it to the agent call and to `/stop`.
    wa_state
        .store_cancel_token(session_id, cancel_token.clone())
        .await;

    let wa_chat_id = format!("{}", info.source.chat);
    let result = agent
        .send_message_with_tools_and_display(
            session_id,
            agent_input,
            Some(display_text),
            None,
            Some(cancel_token),
            Some(approval_cb),
            Some(progress_cb),
            "whatsapp",
            Some(&wa_chat_id),
            None,
        )
        .await;

    // Close the stream channel and let the consumer drain before the final
    // response is handled: that block edits whatever the stream left tracked,
    // so the stream has to be finished first. `progress_cb` was moved into the
    // call above and dropped with it, which makes this the last sink clone.
    drop(stream_sink);
    match tokio::time::timeout(std::time::Duration::from_secs(60), stream_task).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "WhatsApp streaming task panicked"),
        Err(_) => tracing::warn!(
            "WhatsApp streaming task still running after 60s; a progress callback \
             clone outlived the agent call, so the final edit may be skipped"
        ),
    }

    wa_state.remove_cancel_token(session_id).await;
    typing_cancel.cancel();

    match result {
        Ok(response) => {
            // Send to the same target the streamed intermediate text used (PN
            // for the owner self-chat, original chat otherwise) — see
            // `reply_target` above.
            let reply_jid = reply_target.clone();

            // Extract <<IMG:path>> markers — send each as a real WhatsApp image message.
            let (text_content, img_paths) = crate::utils::extract_img_markers(&response.content);
            let text_content = crate::utils::sanitize::strip_llm_artifacts(&text_content);
            let text_content = redact_secrets(&text_content);
            let text_content = crate::utils::slack_fmt::markdown_to_mrkdwn(&text_content);

            // Context budget footer is appended INLINE to the last chunk of
            // the final response below (see the `!streamed` block), never a
            // separate message and never stored in the DB.
            //
            // Streamed turns still skip it, but the reason has changed. It
            // used to be that a streamed turn had no final message to attach
            // it to; since #1408 it has one, the edited message. The skip is
            // kept on purpose: a footer on a growing message would either be
            // re-sent with every edit or appear only once at the end, and
            // matching Telegram (commit 7a0ca1c9) beats both.

            // Send images before text
            for img_path in img_paths {
                match tokio::fs::read(&img_path).await {
                    Ok(bytes) => {
                        use wacore::download::MediaType;
                        use waproto::whatsapp::message::ImageMessage;
                        use whatsapp_rust::upload::UploadOptions;
                        match client
                            .upload(bytes, MediaType::Image, UploadOptions::new())
                            .await
                        {
                            Ok(upload) => {
                                let mime = if img_path.ends_with(".png") {
                                    "image/png"
                                } else {
                                    "image/jpeg"
                                };
                                let img_msg = waproto::whatsapp::Message {
                                    image_message: ImageMessage {
                                        url: Some(upload.url),
                                        direct_path: Some(upload.direct_path),
                                        media_key: Some(upload.media_key.to_vec()),
                                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                                        file_sha256: Some(upload.file_sha256.to_vec()),
                                        file_length: Some(upload.file_length),
                                        mimetype: Some(mime.to_string()),
                                        ..Default::default()
                                    }
                                    .into(),
                                    ..Default::default()
                                };
                                if let Err(e) =
                                    client.send_message(reply_jid.clone(), img_msg).await
                                {
                                    tracing::error!(
                                        "WhatsApp: failed to send generated image: {}",
                                        e
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::error!(
                                    "WhatsApp: image upload failed for {}: {}",
                                    img_path,
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("WhatsApp: failed to read image {}: {}", img_path, e);
                    }
                }
            }

            // Send text response (markers stripped).
            // Skip if already delivered progressively via the intermediate-text callback
            // (happens when the agent used tool calls — text was sent between iterations).
            // Context budget footer is appended to last chunk for display only, never stored in DB.
            let streamed = was_streamed.load(std::sync::atomic::Ordering::Relaxed);
            if streamed && !text_content.trim().is_empty() {
                // #1408 AC1: a streamed turn ends as exactly ONE message. Edit
                // what the stream left behind up to the finished answer so the
                // user is not left reading the last intermediate. A false here
                // means the intermediates stand as the delivery, which is the
                // pre-#1408 behaviour and loses nothing.
                let finished = format!("{}\n\n{}", MSG_HEADER, text_content.trim());
                if !super::stream::finalize(&client, &reply_jid, &wa_state, session_id, &finished)
                    .await
                {
                    tracing::debug!(
                        target: "whatsapp",
                        "edit-in-place unavailable; streamed intermediates stand as the final answer"
                    );
                }
            }
            if !text_content.is_empty() && !streamed {
                let ctx_max = agent.context_limit_for_session(session_id);
                let footer = crate::utils::format_ctx_footer(
                    response.context_tokens,
                    ctx_max,
                    response.tokens_per_second,
                );
                let tagged = format!("{}\n\n{}", MSG_HEADER, text_content);
                let mut chunks: Vec<String> = split_message(&tagged, 4000)
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect();
                if let Some(last) = chunks.last_mut() {
                    last.push_str("\n\n");
                    last.push_str(&footer);
                } else if !footer.is_empty() {
                    chunks.push(footer.clone());
                }
                // #1407: final-text chunks are agent-output sends: gate
                // them through the shared limiter. Queued chunks park
                // FIFO for the drainer; order is preserved across flush.
                let rl_jid = reply_jid.to_string();
                let rl_owner = wa_cfg.is_owner(rl_jid.split('@').next().unwrap_or(&rl_jid));
                let mut rl_queued = 0usize;
                for chunk in &chunks {
                    match wa_state
                        .rate_limiter
                        .gate(&wa_cfg.rate_limit, &rl_jid, chunk, rl_owner)
                        .await
                    {
                        super::rate_limit::GateOutcome::Queued { .. } => {
                            rl_queued += 1;
                            continue;
                        }
                        super::rate_limit::GateOutcome::SendNow => {}
                    }
                    let reply_msg = waproto::whatsapp::Message {
                        conversation: Some(chunk.to_string()),
                        ..Default::default()
                    };
                    match send_resilient(&client, reply_jid.clone(), reply_msg).await {
                        Some(message_id) => {
                            wa_state
                                .record_outbound(
                                    session_id,
                                    super::outbox::OutboxEntry::new(message_id, chunk),
                                )
                                .await;
                        }
                        None => tracing::warn!(
                            target: "whatsapp",
                            "final-text chunk send failed; nothing tracked for this turn"
                        ),
                    }
                }
                if rl_queued > 0 {
                    tracing::warn!(
                        target: "whatsapp",
                        "daily cap reached; {rl_queued} final-text chunk(s) queued for drainer flush"
                    );
                }
            }

            // Record the bot's reply in channel_messages so recent() context
            // queries on the next turn see both sides of the conversation,
            // not only user messages. Matches the pattern added for Telegram
            // and Discord. Applies to both group and DM threads — WhatsApp
            // stores user messages for both, so we stay symmetric.
            if !text_content.trim().is_empty() {
                let chat_id = format!("{}", info.source.chat);
                let is_group = info.source.is_group;
                let cm = DbChannelMessage::new(
                    "whatsapp".into(),
                    chat_id,
                    if is_group {
                        Some(format!("{}", info.source.chat))
                    } else {
                        None
                    },
                    "bot:opencrabs".to_string(),
                    "OpenCrabs".to_string(),
                    text_content.clone(),
                    "text".into(),
                    None,
                );
                if let Err(e) = channel_msg_repo.insert(&cm).await {
                    tracing::warn!(
                        "WhatsApp: failed to record bot reply in channel_messages: {}",
                        e
                    );
                }
            }

            // #1409: acknowledge a finished multi-step turn with a reaction on
            // our own final message, the way the crab does on Telegram. Only
            // on streamed turns: those are the ones that ran tools and took
            // long enough that an acknowledgement means something. A plain
            // chat reply needs no tick on itself.
            if streamed {
                super::reaction::acknowledge_completion(
                    &client,
                    &reply_jid,
                    &wa_state,
                    session_id,
                    super::reaction::COMPLETION_EMOJI,
                )
                .await;
            }

            // If input was voice AND TTS is enabled, also send voice note after text
            if has_aud && voice_config.tts_enabled {
                // Show "recording..." across synthesis and upload, the way a
                // human preparing a voice note appears (#1486). Cleared on
                // every exit path below, success or failure, so the chat is
                // never left stuck on the indicator.
                if let Err(e) = client.chatstate().send_recording(&reply_jid).await {
                    tracing::warn!(error = %e, "WhatsApp: recording indicator failed");
                }
                match crate::channels::voice::synthesize(&response.content, &voice_config).await {
                    Ok(audio_bytes) => {
                        // WhatsApp requires uploading media to its servers first,
                        // then sending the message with the returned URL + crypto keys.
                        use wacore::download::MediaType;
                        use waproto::whatsapp::message::AudioMessage;
                        use whatsapp_rust::upload::UploadOptions;
                        match client
                            .upload(audio_bytes, MediaType::Audio, UploadOptions::new())
                            .await
                        {
                            Ok(upload) => {
                                let audio_msg = waproto::whatsapp::Message {
                                    audio_message: AudioMessage {
                                        url: Some(upload.url),
                                        direct_path: Some(upload.direct_path),
                                        media_key: Some(upload.media_key.to_vec()),
                                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                                        file_sha256: Some(upload.file_sha256.to_vec()),
                                        file_length: Some(upload.file_length),
                                        mimetype: Some("audio/ogg; codecs=opus".to_string()),
                                        ptt: Some(true),
                                        ..Default::default()
                                    }
                                    .into(),
                                    ..Default::default()
                                };
                                if let Err(e) =
                                    client.send_message(reply_jid.clone(), audio_msg).await
                                {
                                    tracing::error!("WhatsApp: failed to send TTS voice: {}", e);
                                }
                            }
                            Err(e) => {
                                tracing::error!("WhatsApp: TTS audio upload failed: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("WhatsApp: TTS synthesis error: {}", e);
                    }
                }
                if let Err(e) = client.chatstate().send_paused(&reply_jid).await {
                    tracing::warn!(error = %e, "WhatsApp: clearing recording indicator failed");
                }
            }

            // ctx footer already appended inline above
        }
        Err(ref e) if matches!(e, crate::brain::agent::AgentError::Cancelled) => {
            tracing::info!("WhatsApp: agent call cancelled for session {}", session_id);
        }
        Err(e) => {
            tracing::error!("WhatsApp: agent error: {}", e);
            // Shared helper translates the raw error into something
            // actionable. Same wording as TUI / Telegram / Discord /
            // Slack.
            let error_msg = waproto::whatsapp::Message {
                conversation: Some(format!(
                    "{}\n\n❌ Error\n\n{}",
                    MSG_HEADER,
                    crate::brain::agent::format_user_error(&e)
                )),
                ..Default::default()
            };
            if let Err(e) = client.send_message(reply_target.clone(), error_msg).await {
                tracing::warn!(error = %e, "failed to send WhatsApp message");
            }
        }
    }

    // Drop the guard before flushing so a message arriving during the flush
    // takes the slot fresh instead of re-queueing against a finished turn.
    drop(turn_guard);

    // #2179 safeguard: a follow-up that landed during the FINAL round had no
    // further between-rounds drain after it. Flush it into a fresh turn so a
    // mid-turn message is never silently stranded. Empty is the common case
    // (one lock check); a real inference only fires when something was queued.
    super::turns::flush_after_turn(wa_state.clone(), agent.clone(), session_id);
}

/// Send a real agent-generated confirmation greeting into the owner's self-chat
/// right after pairing/connect, proving the full round trip (agent turn + send).
///
/// This reuses the exact path `handle_message` uses for the owner: it resolves
/// the same persistent per-phone WhatsApp session (`wa-<number>`), restores its
/// provider, runs ONE agent turn with an internal first-message prompt, then
/// sends the agent's reply to the owner's self-chat JID. On any failure (bad
/// JID, session error, no agent reply, send error) it broadcasts a WhatsApp
/// error so onboarding surfaces a real failure instead of a hollow "connected".
pub(crate) async fn send_connection_greeting(
    client: Arc<Client>,
    agent: Arc<AgentService>,
    session_svc: SessionService,
    wa_state: Arc<WhatsAppState>,
    owner_number: String,
) {
    let owner_number = owner_number.trim_start_matches('+').to_string();
    if owner_number.is_empty() {
        wa_state.broadcast_error(
            "WhatsApp connected but the owner number is empty — cannot send confirmation.",
        );
        return;
    }

    let jid_str = format!("{owner_number}@s.whatsapp.net");
    let jid: wacore_binary::jid::Jid = match jid_str.parse() {
        Ok(j) => j,
        Err(e) => {
            wa_state.broadcast_error(&format!(
                "WhatsApp connected but owner JID '{jid_str}' is invalid: {e}"
            ));
            return;
        }
    };

    // Resolve the same persistent per-phone session handle_message uses.
    let idle_timeout_hours = Config::load()
        .ok()
        .and_then(|c| c.channels.whatsapp.session_idle_hours);
    let session_id = {
        use crate::channels::session_resolve;
        let legacy_title = format!("WhatsApp: {}", owner_number);
        let suffix = session_resolve::chat_id_suffix(&format!("wa-{owner_number}"));
        let session_title = format!("{legacy_title} {suffix}");
        match session_resolve::resolve_or_create_channel_session(
            &session_svc,
            &suffix,
            &legacy_title,
            &session_title,
            idle_timeout_hours,
            "WhatsApp",
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                wa_state.broadcast_error(&format!(
                    "WhatsApp connected but failed to resolve owner session for the \
                     confirmation greeting: {e}"
                ));
                return;
            }
        }
    };

    // Restore the session's own provider so the greeting uses the right model.
    let session_meta = session_svc.get_session(session_id).await.ok().flatten();
    crate::channels::commands::sync_provider_for_session(
        &agent,
        session_id,
        session_meta
            .as_ref()
            .and_then(|s| s.provider_name.as_deref()),
        session_meta.as_ref().and_then(|s| s.model.as_deref()),
    )
    .await;

    // A real agent turn (not a hardcoded string): the model greets with full
    // context (preamble/USER/SOUL/AGENTS) via the persistent WhatsApp session.
    // The prompt intentionally does NOT dictate the wording, so the greeting
    // reads as a genuine message in the agent's own voice rather than a canned
    // "online and ready" line repeated verbatim every time.
    let prompt = "[Channel: WhatsApp — your text response is automatically sent to this chat. \
         Do NOT call whatsapp_send to deliver your answer; it exists only for media, polls, \
         reactions, quote-replies, and sends to a different chat_id. Just reply with text.]\n\
         You have just connected to the owner over WhatsApp. Send one short, \
         natural first message in your own voice letting them know you are here. \
         Do not use a generic canned status line."
        .to_string();

    let result = agent
        .send_message_with_tools_and_display(
            session_id,
            prompt,
            None,
            None,
            None,
            None,
            None,
            "whatsapp",
            Some(&jid_str),
            None,
        )
        .await;

    match result {
        Ok(response) => {
            let (text_content, _imgs) = crate::utils::extract_img_markers(&response.content);
            let text_content = crate::utils::sanitize::strip_llm_artifacts(&text_content);
            let text_content = redact_secrets(&text_content);
            let text_content = crate::utils::slack_fmt::markdown_to_mrkdwn(&text_content);
            if text_content.trim().is_empty() {
                wa_state.broadcast_error(
                    "WhatsApp connected but the agent produced no greeting — onboarding \
                     round trip failed.",
                );
                return;
            }
            let tagged = format!("{}\n\n{}", MSG_HEADER, text_content.trim());
            for chunk in split_message(&tagged, 4000) {
                let msg = waproto::whatsapp::Message {
                    conversation: Some(chunk.to_string()),
                    ..Default::default()
                };
                if send_resilient(&client, jid.clone(), msg).await.is_none() {
                    tracing::warn!(
                        target: "whatsapp",
                        "onboarding greeting chunk failed to send"
                    );
                }
            }
            tracing::info!("WhatsApp: sent connection greeting to owner self-chat {jid_str}");
        }
        Err(e) => {
            wa_state.broadcast_error(&format!(
                "WhatsApp connected but the agent could not generate a greeting: {e}"
            ));
        }
    }
}
