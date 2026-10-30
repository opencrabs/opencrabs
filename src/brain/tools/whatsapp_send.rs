//! WhatsApp Send Tool
//!
//! Agent-callable tool for full WhatsApp control: send, reply, delete,
//! send photos/documents/audio/video/stickers, locations, contacts,
//! reactions, polls, typing indicators, and read receipts.
//! Uses the shared `WhatsAppState` to access the connected client.

use super::error::Result;
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolHints, ToolResult};
use crate::channels::whatsapp::WhatsAppState;
use crate::channels::whatsapp::broadcast;
use crate::channels::whatsapp::interactive;
use crate::channels::whatsapp::rate_limit::GateOutcome;
use crate::config::Config;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use wacore_binary::JidExt;
use wacore_binary::jid::Jid;

/// Tool for comprehensive WhatsApp control (14 actions).
pub struct WhatsAppSendTool {
    whatsapp_state: Arc<WhatsAppState>,
    config_rx: tokio::sync::watch::Receiver<Config>,
}

impl WhatsAppSendTool {
    pub fn new(
        whatsapp_state: Arc<WhatsAppState>,
        config_rx: tokio::sync::watch::Receiver<Config>,
    ) -> Self {
        Self {
            whatsapp_state,
            config_rx,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a required non-empty string param, returning ToolResult::error on failure.
#[allow(clippy::result_large_err)]
pub(crate) fn get_str<'a>(input: &'a Value, key: &str) -> std::result::Result<&'a str, ToolResult> {
    match input.get(key).and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(ToolResult::error(format!(
            "Missing required parameter '{key}'."
        ))),
    }
}

/// Extract an optional f64 param.
pub(crate) fn get_f64(input: &Value, key: &str) -> Option<f64> {
    input.get(key).and_then(|v| v.as_f64())
}

/// Macro to early-return Ok(err_result) when a param helper returns Err.
macro_rules! pget {
    ($expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => return Ok(e),
        }
    };
}

/// Pace one non-text send through the shared #1407 budget.
///
/// #1407 landed covering `send` and `reply` only, so every media send, poll
/// and reaction went straight to the wire and the budget under-counted by
/// however many of those a turn made. They cannot use `gate`: that parks an
/// over-cap send in a queue holding `{ jid, text }`, and the drainer replays
/// it as plain text, which would deliver a photo's caption as a bare message
/// and drop the image. So these pace on the bucket and, under a saturated
/// daily cap, refuse and tell the agent rather than pretending to have sent.
///
/// Gates the actions that DELIVER A MESSAGE. Receipts, presence and account
/// operations (typing, mark_read, delete, pin, block, profile) are not
/// messages and are not charged to a message budget.
macro_rules! gate_send {
    ($self:expr, $jid_str:expr, $what:expr) => {{
        let (cfg, owner) = {
            let wa = &$self.config_rx.borrow().channels.whatsapp;
            (
                wa.rate_limit.clone(),
                wa.is_owner($jid_str.split('@').next().unwrap_or(&$jid_str)),
            )
        };
        if !$self
            .whatsapp_state
            .rate_limiter
            .gate_action(&cfg, owner)
            .await
        {
            return Ok(ToolResult::error(format!(
                "Daily WhatsApp send cap reached; {} was NOT sent. Unlike text, this is \
                 not queued for retry - try again once the 24h window slides.",
                $what
            )));
        }
    }};
}

/// Resolve the WhatsApp client, returning an error if not connected.
#[allow(clippy::result_large_err)]
async fn get_client(
    whatsapp_state: &WhatsAppState,
) -> std::result::Result<Arc<whatsapp_rust::client::Client>, ToolResult> {
    whatsapp_state.client().await.ok_or_else(|| {
        ToolResult::error(
            "WhatsApp is not connected. Ask the user to connect WhatsApp first \
                 (use the whatsapp_connect tool)."
                .to_string(),
        )
    })
}

/// Resolve target JID from `phone` param, originating session, or owner fallback,
/// with allowlist check.
#[allow(clippy::result_large_err)]
pub(crate) async fn resolve_jid(
    input: &Value,
    whatsapp_state: &WhatsAppState,
    config_rx: &tokio::sync::watch::Receiver<Config>,
    session_id: uuid::Uuid,
) -> std::result::Result<(Jid, String), ToolResult> {
    if let Some(raw_target) = input.get("phone").and_then(|v| v.as_str()) {
        let allowed = &config_rx.borrow().channels.whatsapp.allowed_phones;
        let target_clean = raw_target.trim().trim_start_matches('+');
        let user_part = target_clean
            .split('@')
            .next()
            .unwrap_or(target_clean)
            .split(':')
            .next()
            .unwrap_or(target_clean);

        let phone_allowed = allowed.is_empty()
            || allowed
                .iter()
                .any(|p| p.trim_start_matches('+') == user_part);
        if !phone_allowed {
            return Err(ToolResult::error(format!(
                "Sending to {} is not permitted. This number is not in the \
                 allowed_users config.",
                raw_target
            )));
        }
        let jid_str = if target_clean.contains('@') {
            target_clean.to_string()
        } else {
            format!("{}@s.whatsapp.net", user_part)
        };
        let jid: Jid = jid_str
            .parse()
            .map_err(|e| ToolResult::error(format!("Invalid phone number format: {}", e)))?;
        if !crate::cron::send_scope::may_send("whatsapp", &jid_str) {
            let reason = crate::cron::send_scope::refusal_for("whatsapp", &jid_str);
            tracing::warn!("whatsapp_send: {reason}");
            return Err(ToolResult::error(reason));
        }
        Ok((jid, jid_str))
    } else if let Some(session_jid_str) = whatsapp_state.session_jid(session_id).await {
        let jid: Jid = session_jid_str
            .parse()
            .map_err(|e| ToolResult::error(format!("Invalid session chat JID: {}", e)))?;
        if !crate::cron::send_scope::may_send("whatsapp", &session_jid_str) {
            let reason = crate::cron::send_scope::refusal_for("whatsapp", &session_jid_str);
            tracing::warn!("whatsapp_send: {reason}");
            return Err(ToolResult::error(reason));
        }
        Ok((jid, session_jid_str))
    } else {
        let jid_str = whatsapp_state.owner_jid().await.ok_or_else(|| {
            ToolResult::error(
                "No owner phone number configured and no 'phone' parameter provided. \
                 Specify a phone number to send to."
                    .to_string(),
            )
        })?;
        let jid: Jid = jid_str
            .parse()
            .map_err(|e| ToolResult::error(format!("Invalid owner JID: {}", e)))?;
        if !crate::cron::send_scope::may_send("whatsapp", &jid_str) {
            let reason = crate::cron::send_scope::refusal_for("whatsapp", &jid_str);
            tracing::warn!("whatsapp_send: {reason}");
            return Err(ToolResult::error(reason));
        }
        Ok((jid, jid_str))
    }
}

/// Prefix outgoing text with the channel attribution header. Shared by the
/// send and reply paths so persisted history carries ONE consistent format
/// for every agent-authored message (#1490-E: reply persisted bare text
/// while send persisted the tagged form).
pub(crate) fn tag_with_header(message: &str) -> String {
    format!(
        "{}\n\n{}",
        crate::channels::whatsapp::handler::MSG_HEADER,
        message
    )
}

/// Concatenate the chunks that actually left. `split_message` returns
/// contiguous slices of its input, so the concat is the exact delivered
/// prefix of the tagged text (#1490-B).
pub(crate) fn delivered_prefix(delivered: &[&str]) -> String {
    delivered.concat()
}

/// Error text for a chunked send that died partway: names how many chunks
/// were delivered (and persisted) vs which one failed, and tells the model
/// what a retry may and must-not resend (#1490-B; blind retries used to
/// duplicate chunk 1).
pub(crate) fn partial_failure_report(total: usize, delivered: usize, err: &str) -> String {
    if delivered == 0 {
        format!(
            "Failed to send WhatsApp message: {err}. No chunks were delivered; safe to retry the whole message."
        )
    } else {
        format!(
            "Failed to send WhatsApp message: chunk {} of {} failed: {err}. Chunks 1-{} were DELIVERED and persisted; do NOT resend them. Retry only the remaining text if needed.",
            delivered + 1,
            total,
            delivered
        )
    }
}

/// Persist outgoing messages to `channel_messages` for reply-recovery.
pub(crate) async fn persist_outgoing(jid: &Jid, content: &str) {
    if content.trim().is_empty() {
        return;
    }
    let Some(pool) = crate::db::global_pool() else {
        return;
    };
    let repo = crate::db::ChannelMessageRepository::new(pool.clone());
    let chat_id = jid.to_string();
    let cm = crate::db::models::ChannelMessage::new(
        "whatsapp".to_string(),
        chat_id,
        None,
        "bot:opencrabs".to_string(),
        "OpenCrabs".to_string(),
        content.to_string(),
        "text".to_string(),
        None,
    );
    if let Err(e) = repo.insert(&cm).await {
        tracing::warn!("whatsapp_send: failed to persist outgoing message: {}", e);
    }
}

/// Read a local file, expanding tilde. Returns (bytes, detected mime, filename).
#[allow(clippy::result_large_err)]
async fn read_local_media(
    path: &str,
    default_mime: &str,
) -> std::result::Result<(Vec<u8>, String, String), ToolResult> {
    let expanded = crate::brain::tools::error::expand_tilde(path);
    let bytes = tokio::fs::read(&expanded).await.map_err(|e| {
        ToolResult::error(format!(
            "Failed to read file '{}': {}",
            expanded.display(),
            e
        ))
    })?;
    let filename = expanded
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let mime = mime_from_extension(&expanded.to_string_lossy())
        .unwrap_or_else(|| default_mime.to_string());
    Ok((bytes, mime, filename))
}

/// Detect MIME type from file extension.
pub(crate) fn mime_from_extension(path: &str) -> Option<String> {
    let ext = path.rsplit('.').next()?.to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "png" => Some("image/png".to_string()),
        "gif" => Some("image/gif".to_string()),
        "webp" => Some("image/webp".to_string()),
        "mp4" => Some("video/mp4".to_string()),
        "3gp" => Some("video/3gpp".to_string()),
        "avi" => Some("video/x-msvideo".to_string()),
        "mov" => Some("video/quicktime".to_string()),
        "mp3" => Some("audio/mpeg".to_string()),
        "ogg" | "opus" => Some("audio/ogg".to_string()),
        "aac" => Some("audio/aac".to_string()),
        "m4a" => Some("audio/mp4".to_string()),
        "pdf" => Some("application/pdf".to_string()),
        "doc" | "docx" => Some("application/msword".to_string()),
        "xls" | "xlsx" => Some("application/vnd.ms-excel".to_string()),
        "ppt" | "pptx" => Some("application/vnd.ms-powerpoint".to_string()),
        "txt" => Some("text/plain".to_string()),
        "csv" => Some("text/csv".to_string()),
        "zip" => Some("application/zip".to_string()),
        _ => None,
    }
}

/// Largest edge of the status thumbnail, in pixels. WhatsApp requires a JPEG
/// thumbnail alongside an image status; a 32x32 JPEG is a couple of hundred
/// bytes and is all the client needs for the notification preview, so the
/// full-resolution bytes go to the CDN untouched.
pub(crate) const STATUS_THUMBNAIL_MAX_EDGE: u32 = 32;

/// Build the JPEG thumbnail a WhatsApp image status must carry.
///
/// Returns `None` when the bytes are not a decodable image (or cannot be
/// re-encoded), which the caller reports instead of posting a status whose
/// thumbnail is missing.
pub(crate) fn make_jpeg_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let thumb = img.thumbnail(STATUS_THUMBNAIL_MAX_EDGE, STATUS_THUMBNAIL_MAX_EDGE);
    let mut out = std::io::Cursor::new(Vec::new());
    thumb.write_to(&mut out, image::ImageFormat::Jpeg).ok()?;
    Some(out.into_inner())
}

// ---------------------------------------------------------------------------
// Status fan-out (#2177)
// ---------------------------------------------------------------------------

/// What one status fan-out actually did, recipient by recipient.
///
/// A status goes to a LIST, so a partial result is normal and has to be
/// reportable: who got it, who the daily cap refused, and whose send failed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusFanOut {
    /// Recipients that received the status.
    pub sent: Vec<String>,
    /// Recipients the daily cap refused. Always a tail of the list, because a
    /// refusal ends the fan-out.
    pub refused: Vec<String>,
    /// Recipients whose send failed, with the error.
    pub failed: Vec<(String, String)>,
}

impl StatusFanOut {
    /// Recipients that did not receive the status.
    fn missed(&self) -> usize {
        self.refused.len() + self.failed.len()
    }
}

/// The two per-recipient steps of a status fan-out.
///
/// Split out of the dispatch arm so the pacing and the cap accounting can be
/// exercised without a WhatsApp connection: the arm supplies a live sink, a
/// test supplies a recorder.
#[async_trait]
pub(crate) trait StatusSink {
    /// Charge one slot of the shared #1407 budget for `jid`.
    ///
    /// `false` means the daily cap refused this recipient. Charging happens
    /// per recipient, immediately before that recipient's send, so a refusal
    /// can never spend a slot on a status that was not delivered.
    async fn charge(&mut self, jid: &Jid) -> bool;

    /// Deliver the status to exactly this recipient.
    async fn deliver(&mut self, jid: &Jid) -> std::result::Result<(), String>;
}

/// Post one status per recipient, pacing between the sends (#2177).
///
/// The old shape charged and slept once per recipient and then posted a single
/// status for the whole list, so every recipient got it at the same instant
/// after one delay, and a cap hit part-way through spent the earlier slots on
/// nothing. Here the charge and the send belong to the same recipient and the
/// sleep sits between two sends.
pub(crate) async fn fan_out_status<S: StatusSink>(
    sink: &mut S,
    jids: &[Jid],
    pace: std::time::Duration,
) -> StatusFanOut {
    let mut out = StatusFanOut::default();
    for (i, jid) in jids.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(pace).await;
        }
        if !sink.charge(jid).await {
            // The cap is empty: the rest cannot be charged either, so report
            // them instead of sleeping through recipients that will not get
            // anything.
            out.refused
                .extend(jids[i..].iter().map(std::string::ToString::to_string));
            break;
        }
        match sink.deliver(jid).await {
            Ok(()) => out.sent.push(jid.to_string()),
            Err(e) => out.failed.push((jid.to_string(), e)),
        }
    }
    out
}

/// The report for a finished status fan-out.
///
/// Success only when every recipient got the status; a partial run says how
/// many landed and names the ones a retry still owes, and a run that delivered
/// nothing is an error that says a status cannot ride the text queue.
pub(crate) fn status_report(
    kind: &str,
    total: usize,
    out: &StatusFanOut,
    pace: std::time::Duration,
) -> ToolResult {
    let misses = describe_status_misses(out);
    if out.sent.is_empty() {
        return ToolResult::error(format!(
            "Status update ({kind}) reached none of the {total} allowlisted recipient(s).{misses}"
        ));
    }
    if out.missed() == 0 {
        return ToolResult::success(format!(
            "Status update ({kind}) posted to {total} allowlisted recipient(s), one send per \
             recipient, paced at {}s (at least {}s of wall clock).",
            pace.as_secs(),
            broadcast::burst_floor(total, pace).as_secs()
        ));
    }
    ToolResult::success(format!(
        "Status update ({kind}) posted to {} of {total} allowlisted recipient(s).{misses}",
        out.sent.len()
    ))
}

/// The "who did not get it, and why" clause shared by both report shapes.
fn describe_status_misses(out: &StatusFanOut) -> String {
    let mut parts = Vec::new();
    if !out.refused.is_empty() {
        parts.push(format!(
            " The daily WhatsApp send cap refused {} recipient(s): {}. Unlike text, a status is \
             not queued for retry - try again once the 24h window slides.",
            out.refused.len(),
            out.refused.join(", ")
        ));
    }
    if !out.failed.is_empty() {
        parts.push(format!(
            " {} recipient(s) failed: {}.",
            out.failed.len(),
            out.failed
                .iter()
                .map(|(jid, err)| format!("{jid} ({err})"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    parts.join("")
}

/// The payload of a status, prepared once and delivered per recipient.
enum StatusPayload {
    Text {
        body: String,
    },
    Image {
        /// Boxed: `UploadResponse` is 232 bytes and would make every `Text`
        /// status pay for it.
        upload: Box<whatsapp_rust::upload::UploadResponse>,
        thumbnail: Vec<u8>,
        caption: Option<String>,
    },
}

/// Live [`StatusSink`]: charges the real rate limiter and sends through the
/// connected client.
struct LiveStatusSink<'a> {
    tool: &'a WhatsAppSendTool,
    client: &'a whatsapp_rust::client::Client,
    payload: StatusPayload,
}

#[async_trait]
impl StatusSink for LiveStatusSink<'_> {
    async fn charge(&mut self, jid: &Jid) -> bool {
        let jid_str = jid.to_string();
        let (cfg, owner) = {
            let wa = &self.tool.config_rx.borrow().channels.whatsapp;
            (
                wa.rate_limit.clone(),
                wa.is_owner(jid_str.split('@').next().unwrap_or(&jid_str)),
            )
        };
        self.tool
            .whatsapp_state
            .rate_limiter
            .gate_action(&cfg, owner)
            .await
    }

    async fn deliver(&mut self, jid: &Jid) -> std::result::Result<(), String> {
        let one = std::slice::from_ref(jid);
        let opts = whatsapp_rust::features::StatusSendOptions::default();
        let posted = match &self.payload {
            StatusPayload::Text { body } => {
                self.client
                    .status()
                    .send_text(
                        body,
                        broadcast::STATUS_BACKGROUND_ARGB,
                        waproto::whatsapp::message::extended_text_message::FontType::SYSTEM,
                        one,
                        opts,
                    )
                    .await
            }
            StatusPayload::Image {
                upload,
                thumbnail,
                caption,
            } => {
                self.client
                    .status()
                    .send_image(
                        (**upload).clone(),
                        thumbnail.clone(),
                        caption.as_deref(),
                        one,
                        opts,
                    )
                    .await
            }
        };
        posted.map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Upload media to WhatsApp servers and return the upload response.
/// Uses the same pattern as the WhatsApp handler.
#[allow(clippy::result_large_err)]
async fn upload_media(
    client: &whatsapp_rust::client::Client,
    data: Vec<u8>,
    media_type: wacore::download::MediaType,
) -> std::result::Result<whatsapp_rust::upload::UploadResponse, ToolResult> {
    client
        .upload(
            data,
            media_type,
            whatsapp_rust::upload::UploadOptions::new(),
        )
        .await
        .map_err(|e| ToolResult::error(format!("Media upload failed: {}", e)))
}

/// Build a vCard string from name and phone number.
pub(crate) fn build_vcard(name: &str, phone: &str) -> String {
    format!(
        "BEGIN:VCARD\nVERSION:3.0\nFN:{}\nTEL;TYPE=CELL:{}\nEND:VCARD",
        name, phone
    )
}

/// Parse a newsletter (channel) target for `post_newsletter` (#1827).
///
/// A channel JID is `120363...@newsletter`. That suffix is what the send path
/// branches on: `whatsapp-rust` builds a plaintext SMAX stanza for it and
/// rejects the encrypted path. A phone JID or a bare invite code is therefore
/// refused here rather than handed to `send_message`, where it would silently
/// take the DM path and deliver somewhere nobody asked for.
pub(crate) fn newsletter_jid(raw: &str) -> std::result::Result<Jid, String> {
    let jid: Jid = raw
        .parse()
        .map_err(|e| format!("'{raw}' is not a valid JID: {e:?}"))?;
    if !jid.is_newsletter() {
        return Err(format!(
            "'{raw}' is not a newsletter JID; expected the '...@newsletter' form that \
             list_newsletters and create_newsletter return. Nothing was sent."
        ));
    }
    Ok(jid)
}

/// The proto a `post_newsletter` call puts on the wire (#1827).
///
/// A post is text, an image, or an image with a caption: the same shape a
/// status takes, so `message` doubles as the caption when `media_path` is
/// given. A post carrying neither is refused here rather than published as an
/// empty channel update. Kept out of the dispatch arm so the proto shape is
/// unit-testable without a live connection.
pub(crate) fn build_post(
    text: Option<&str>,
    image: Option<waproto::whatsapp::message::ImageMessage>,
) -> std::result::Result<waproto::whatsapp::Message, String> {
    match (text, image) {
        (None, None) => Err(
            "a newsletter post needs 'message' (the post text, or the caption for an image \
             post) or 'media_path' (a local image file). Nothing was sent."
                .to_string(),
        ),
        (Some(body), None) => Ok(waproto::whatsapp::Message {
            conversation: Some(body.to_string()),
            ..Default::default()
        }),
        (body, Some(mut img)) => {
            if let Some(body) = body {
                img.caption = Some(body.to_string());
            }
            Ok(waproto::whatsapp::Message {
                image_message: img.into(),
                ..Default::default()
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Tool for WhatsAppSendTool {
    fn name(&self) -> &str {
        "whatsapp_send"
    }

    fn description(&self) -> &str {
        "Full WhatsApp control: send messages, reply, delete, send photos/documents/audio/video/stickers, \
         locations, contacts, emoji reactions, polls, typing indicators, and mark messages as read. \
         If no phone number is specified, messages go to the owner (primary user). \
         Requires WhatsApp to be connected first."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "send", "reply", "delete",
                        "pin", "unpin", "forward", "set_profile_name", "set_profile_status",
                        "send_photo", "send_document", "send_audio", "send_video", "send_sticker",
                        "send_location", "send_contact",
                        "react", "send_poll",
                        "typing", "mark_read",
                        "block_contact", "unblock_contact", "list_blocked",
                        "send_buttons",
                        "status_update", "list_newsletters",
                        "create_newsletter", "post_newsletter",
                        "create_label", "assign_label"
                    ],
                    "description": "The WhatsApp action to perform."
                },
                "message": {
                    "type": "string",
                    "description": "Message text (send, reply, caption for media); the new name or status text for set_profile_name / set_profile_status. For status_update: the text status, or the caption when 'media_path' is given. For create_newsletter: the channel name. For post_newsletter: the post text, or the caption when 'media_path' is given."
                },
                "phone": {
                    "type": "string",
                    "description": "Phone number in E.164 format (e.g. '+15551234567'). Omit to message the owner."
                },
                "message_id": {
                    "type": "string",
                    "description": "WhatsApp message ID for reply, delete, react, mark_read, forward"
                },
                "from_me": {
                    "type": "boolean",
                    "description": "Whether the target message was sent by us (for react/mark_read). Default false."
                },
                "media_path": {
                    "type": "string",
                    "description": "Local file path for media (send_photo, send_document, send_audio, send_video, send_sticker). For status_update: a local image file to post as an image status instead of a text status, in which case 'message' becomes its caption. For post_newsletter: a local image file to publish with the post, in which case 'message' becomes its caption."
                },
                "multi_select": {
                    "type": "boolean",
                    "description": "For send_poll: true lets a voter pick several options. Default false (single choice)."
                },
                "voice_note": {
                    "type": "boolean",
                    "description": "For send_audio: true (default) renders a native WhatsApp voice note (tap-to-play bubble) and shows a recording indicator while uploading. false sends a plain audio file attachment."
                },
                "buttons": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "For send_buttons: up to 3 button labels. WhatsApp draws at most 3; extras are dropped rather than silently ignored on the wire."
                },
                "footer": {
                    "type": "string",
                    "description": "For send_buttons: small text under the buttons."
                },
                "targets": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "For status_update: phone numbers to post the status to. Every one must appear in channels.whatsapp.broadcast.allowed_targets or NOTHING is sent. Omit to use the whole allowlist."
                },
                "newsletter": {
                    "type": "string",
                    "description": "For post_newsletter: the newsletter (channel) JID to post to, e.g. '120363...@newsletter' as returned by list_newsletters or create_newsletter. It must appear in channels.whatsapp.broadcast.allowed_targets or NOTHING is sent."
                },
                "description": {
                    "type": "string",
                    "description": "For create_newsletter: the channel description. Optional."
                },
                "label_id": {
                    "type": "string",
                    "description": "For create_label (omit to make a new label; supplying an existing id renames/recolors it) and assign_label (required)"
                },
                "label_color": {
                    "type": "integer",
                    "description": "For create_label: WhatsApp colour index. Default 0."
                },
                "remove": {
                    "type": "boolean",
                    "description": "For assign_label: true removes the label from the chat instead of adding it."
                },
                "caption": {
                    "type": "string",
                    "description": "Caption for media messages (send_photo, send_video, send_document). status_update takes its caption from 'message'."
                },
                "latitude": {
                    "type": "number",
                    "description": "Latitude for send_location"
                },
                "longitude": {
                    "type": "number",
                    "description": "Longitude for send_location"
                },
                "location_name": {
                    "type": "string",
                    "description": "Optional location name for send_location"
                },
                "location_address": {
                    "type": "string",
                    "description": "Optional location address for send_location"
                },
                "contact_name": {
                    "type": "string",
                    "description": "Contact display name for send_contact"
                },
                "contact_phone": {
                    "type": "string",
                    "description": "Contact phone number for send_contact"
                },
                "emoji": {
                    "type": "string",
                    "description": "Emoji for react action (e.g. '👍', '❤️'). Empty string to remove reaction."
                },
                "poll_question": {
                    "type": "string",
                    "description": "Poll question text for send_poll"
                },
                "poll_options": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Array of poll option strings (2-12) for send_poll"
                },
                "typing_active": {
                    "type": "boolean",
                    "description": "For typing action: true = composing, false = paused. Default true."
                },
                "ephemeral": {
                    "type": "integer",
                    "description": "For send: disappearing-message TTL in seconds (86400 = 24h, 604800 = 7d, 7776000 = 90d max). 0 means the message does not expire, overriding the channel default. Omit to use the channel default."
                }
            },
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    fn hints(&self) -> ToolHints {
        ToolHints {
            read_only: false,
            destructive: true,
            idempotent: false,
            open_world: true,
        }
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let action = match input.get("action").and_then(|v| v.as_str()) {
            Some(a) if !a.is_empty() => a.to_string(),
            _ => {
                return Ok(ToolResult::error(
                    "Missing required 'action' parameter.".to_string(),
                ));
            }
        };

        let client = pget!(get_client(&self.whatsapp_state).await);

        match action.as_str() {
            // ── send ─────────────────────────────────────────────────────────
            "send" => {
                let message = match input.get("message").and_then(|v| v.as_str()) {
                    Some(m) if !m.is_empty() => m.to_string(),
                    _ => {
                        return Ok(ToolResult::error(
                            "Missing or empty 'message' parameter.".to_string(),
                        ));
                    }
                };
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );

                // Convert markdown to WhatsApp format and prepend agent header
                let message = crate::utils::slack_fmt::markdown_to_mrkdwn(&message);
                let tagged = tag_with_header(&message);
                let chunks = crate::channels::whatsapp::handler::split_message(&tagged, 4000);
                let total = chunks.len();
                let mut delivered: Vec<&str> = Vec::with_capacity(total);
                // #1407: pace sends through the shared limiter; over-cap
                // chunks park FIFO for the drainer (queued text must NOT
                // be resent by the model). Owner-bound sends bypass.
                let (rl_cfg, rl_owner) = {
                    let wa = &self.config_rx.borrow().channels.whatsapp;
                    (
                        wa.rate_limit.clone(),
                        wa.is_owner(jid_str.split('@').next().unwrap_or(&jid_str)),
                    )
                };
                // #1487: disappearing messages. An explicit `ephemeral: 0`
                // means "do not expire" and overrides the channel default,
                // so this is not a plain `or`.
                let ttl = crate::channels::whatsapp::ephemeral::resolve(
                    input.get("ephemeral").and_then(|v| v.as_u64()),
                    self.config_rx.borrow().channels.whatsapp.ephemeral_ttl,
                );
                let send_opts = match ttl {
                    Some(seconds) => whatsapp_rust::send::SendOptions::default()
                        .with_ephemeral_expiration(seconds),
                    None => whatsapp_rust::send::SendOptions::default(),
                };
                let mut queued_any = false;
                for chunk in chunks {
                    if let GateOutcome::Queued { .. } = self
                        .whatsapp_state
                        .rate_limiter
                        .gate(&rl_cfg, &jid_str, chunk, rl_owner)
                        .await
                    {
                        queued_any = true;
                        continue;
                    }
                    let wa_msg = waproto::whatsapp::Message {
                        conversation: Some(chunk.to_string()),
                        ..Default::default()
                    };
                    if let Err(e) = client
                        .send_message_with_options(jid.clone(), wa_msg, send_opts.clone())
                        .await
                    {
                        // #1490-B: account for what already left the building.
                        // Delivered chunks are persisted so history matches
                        // reality, and the error names exactly what arrived vs
                        // what failed so a retry cannot duplicate chunk 1.
                        let prefix = delivered_prefix(&delivered);
                        if !prefix.trim().is_empty() {
                            persist_outgoing(&jid, &prefix).await;
                        }
                        return Ok(ToolResult::error(partial_failure_report(
                            total,
                            delivered.len(),
                            &e.to_string(),
                        )));
                    }
                    delivered.push(chunk);
                }

                if queued_any {
                    // Queued chunks flush via the drainer (which persists
                    // them); record only what actually left now.
                    if !delivered.is_empty() {
                        let prefix = delivered_prefix(&delivered);
                        if !prefix.trim().is_empty() {
                            persist_outgoing(&jid, &prefix).await;
                        }
                    }
                    return Ok(ToolResult::success(format!(
                        "WhatsApp daily cap reached: {} of {} chunk(s) queued for automatic delivery as the 24h window slides (owner alerted once); {} delivered now. Do NOT resend the queued part.",
                        total - delivered.len(),
                        total,
                        delivered.len()
                    )));
                }
                persist_outgoing(&jid, &tagged).await;
                Ok(ToolResult::success(match ttl {
                    Some(seconds) => format!(
                        "Message sent to {} via WhatsApp; it disappears after {}.",
                        jid_str,
                        crate::channels::whatsapp::ephemeral::describe(seconds)
                    ),
                    None => format!("Message sent to {} via WhatsApp.", jid_str),
                }))
            }

            // ── reply ────────────────────────────────────────────────────────
            "reply" => {
                let message = pget!(get_str(&input, "message")).to_string();
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let msg_id = pget!(get_str(&input, "message_id")).to_string();

                let formatted = crate::utils::slack_fmt::markdown_to_mrkdwn(&message);
                // #1490-E: the reply carries and persists the same tagged form
                // as the send path - one event class, one format.
                let tagged = tag_with_header(&formatted);
                // #1490-C: chunk the reply like send does. WhatsApp rejects
                // or truncates over-long message text, so a long reply must
                // not ride a single ExtendedTextMessage. Chunk 0 keeps the
                // quote context; later chunks go as plain conversation
                // messages (one quoted reply plus N-1 normal follow-ups -
                // quoting the same stanza N times is noise).
                let chunks = crate::channels::whatsapp::handler::split_message(&tagged, 4000);
                let total = chunks.len();
                let mut delivered: Vec<&str> = Vec::with_capacity(total);
                // #1407: same limiter gating as the send arm. Queued reply
                // chunks lose their quote context on drainer flush
                // (documented tradeoff); ordering is preserved FIFO.
                let (rl_cfg, rl_owner) = {
                    let wa = &self.config_rx.borrow().channels.whatsapp;
                    (
                        wa.rate_limit.clone(),
                        wa.is_owner(jid_str.split('@').next().unwrap_or(&jid_str)),
                    )
                };
                let mut queued_any = false;
                for (i, chunk) in chunks.into_iter().enumerate() {
                    if let GateOutcome::Queued { .. } = self
                        .whatsapp_state
                        .rate_limiter
                        .gate(&rl_cfg, &jid_str, chunk, rl_owner)
                        .await
                    {
                        queued_any = true;
                        continue;
                    }
                    let wa_msg = if i == 0 {
                        // Only the lead chunk quotes the original message.
                        let quoted = waproto::whatsapp::message::ExtendedTextMessage {
                            text: Some(chunk.to_string()),
                            context_info: waproto::whatsapp::ContextInfo {
                                stanza_id: Some(msg_id.clone()),
                                remote_jid: Some(jid_str.clone()),
                                ..Default::default()
                            }
                            .into(),
                            ..Default::default()
                        };
                        waproto::whatsapp::Message {
                            extended_text_message: quoted.into(),
                            ..Default::default()
                        }
                    } else {
                        waproto::whatsapp::Message {
                            conversation: Some(chunk.to_string()),
                            ..Default::default()
                        }
                    };
                    if let Err(e) = client.send_message(jid.clone(), wa_msg).await {
                        // Same accounting as the send path (#1490-B).
                        let prefix = delivered_prefix(&delivered);
                        if !prefix.trim().is_empty() {
                            persist_outgoing(&jid, &prefix).await;
                        }
                        return Ok(ToolResult::error(partial_failure_report(
                            total,
                            delivered.len(),
                            &e.to_string(),
                        )));
                    }
                    delivered.push(chunk);
                }

                if queued_any {
                    if !delivered.is_empty() {
                        let prefix = delivered_prefix(&delivered);
                        if !prefix.trim().is_empty() {
                            persist_outgoing(&jid, &prefix).await;
                        }
                    }
                    return Ok(ToolResult::success(format!(
                        "WhatsApp daily cap reached: {} of {} reply chunk(s) queued for automatic delivery as the 24h window slides (owner alerted once); {} delivered now. Do NOT resend the queued part.",
                        total - delivered.len(),
                        total,
                        delivered.len()
                    )));
                }
                persist_outgoing(&jid, &tagged).await;
                Ok(ToolResult::success(format!(
                    "Reply sent to {} via WhatsApp.",
                    jid_str
                )))
            }

            // ── delete ───────────────────────────────────────────────────────
            "delete" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let msg_id = pget!(get_str(&input, "message_id")).to_string();

                gate_send!(self, jid_str, "the message deletion");

                match client
                    .revoke_message(jid, msg_id.clone(), Default::default())
                    .await
                {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Message {} deleted for {}.",
                        msg_id, jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to delete message: {}",
                        e
                    ))),
                }
            }

            // ── pin / unpin ──────────────────────────────────────────────────
            // #1484. The lib pins a CHAT, not a message (`pin_chat` /
            // `unpin_chat` in features/chat_actions.rs), so these take a
            // phone and no message id - the issue's "chat_id + message_id"
            // shape does not exist in the protocol.
            "pin" | "unpin" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let pinning = action == "pin";
                let result = if pinning {
                    client.chat_actions().pin_chat(&jid).await
                } else {
                    client.chat_actions().unpin_chat(&jid).await
                };
                match result {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Chat with {} {}.",
                        jid_str,
                        if pinning { "pinned" } else { "unpinned" }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to {} chat {jid_str}: {e}",
                        if pinning { "pin" } else { "unpin" }
                    ))),
                }
            }

            // ── forward ──────────────────────────────────────────────────────
            // #1484. `Client::forward_message` needs the ORIGINAL proto, not an
            // id: it rebuilds the body with the forward flags and relays media
            // from the same CDN blob. The channel remembers the last 200
            // inbound messages for exactly this (see `recent.rs`); anything
            // older is honestly reported as out of the window rather than
            // silently sending nothing.
            "forward" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the forwarded message");
                let msg_id = pget!(get_str(&input, "message_id")).to_string();
                let Some(original) = self.whatsapp_state.recent.get(&msg_id).await else {
                    return Ok(ToolResult::error(format!(
                        "Message {msg_id} is not in the recent-message window, so it cannot be \
                         forwarded. Only messages this session has seen can be forwarded."
                    )));
                };
                match client.forward_message(jid, &original).await {
                    Ok(result) => Ok(ToolResult::success(format!(
                        "Forwarded message {} to {} via WhatsApp (new id {}).",
                        msg_id, jid_str, result.message_id
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to forward {msg_id} to {jid_str}: {e}"
                    ))),
                }
            }

            // ── set_profile_name / set_profile_status ────────────────────────
            // #1484 asked for a separate `whatsapp_profile` tool. Kept as
            // actions here instead: the channel already routes every WhatsApp
            // operation through one tool, and a second one would split that
            // surface for two setters.
            "set_profile_name" => {
                let name = pget!(get_str(&input, "message")).to_string();
                match client.profile().set_push_name(&name).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "WhatsApp profile name set to '{name}'."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set the profile name: {e}"
                    ))),
                }
            }

            "set_profile_status" => {
                let status = pget!(get_str(&input, "message")).to_string();
                match client.profile().set_status_text(&status).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "WhatsApp profile status set to '{status}'."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to set the profile status: {e}"
                    ))),
                }
            }

            // ── send_photo ───────────────────────────────────────────────────
            "send_photo" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the photo");
                let path = pget!(get_str(&input, "media_path")).to_string();
                let caption = input
                    .get("caption")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let (bytes, mime, _filename) = pget!(read_local_media(&path, "image/jpeg").await);
                let upload =
                    pget!(upload_media(&client, bytes, wacore::download::MediaType::Image).await);

                let wa_msg = waproto::whatsapp::Message {
                    image_message: waproto::whatsapp::message::ImageMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        caption,
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Photo sent to {} via WhatsApp.",
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send photo: {}", e))),
                }
            }

            // ── send_document ────────────────────────────────────────────────
            "send_document" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the document");
                let path = pget!(get_str(&input, "media_path")).to_string();
                let caption = input
                    .get("caption")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let (bytes, mime, filename) =
                    pget!(read_local_media(&path, "application/octet-stream").await);
                let upload = pget!(
                    upload_media(&client, bytes, wacore::download::MediaType::Document).await
                );

                let wa_msg = waproto::whatsapp::Message {
                    document_message: waproto::whatsapp::message::DocumentMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        file_name: Some(filename),
                        caption,
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Document sent to {} via WhatsApp.",
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send document: {}", e))),
                }
            }

            // ── send_audio ───────────────────────────────────────────────────
            "send_audio" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the audio");
                let path = pget!(get_str(&input, "media_path")).to_string();

                let (bytes, mime, _filename) = pget!(read_local_media(&path, "audio/ogg").await);
                // #1486: WhatsApp renders a tap-to-play voice bubble only when
                // the message carries ptt. Without it the same bytes arrive as
                // an unstyled file attachment. Default on, because the caller
                // that wants a plain audio FILE is the rare one, and pass
                // `voice_note: false` to get the old rendering.
                let ptt = crate::channels::whatsapp::voice_note::wants_voice_note(&input);
                let mime = crate::channels::whatsapp::voice_note::voice_note_mimetype(&mime, ptt);
                let upload =
                    pget!(upload_media(&client, bytes, wacore::download::MediaType::Audio).await);

                // Show "recording..." while the upload is in flight, exactly
                // as a human sending a voice note would appear (#1486).
                if ptt && let Err(e) = client.chatstate().send_recording(&jid).await {
                    tracing::warn!(error = %e, "WhatsApp: recording indicator failed");
                }

                let wa_msg = waproto::whatsapp::Message {
                    audio_message: waproto::whatsapp::message::AudioMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        ptt: Some(ptt),
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                let sent = client.send_message(jid.clone(), wa_msg).await;
                // Clear the indicator whether or not the send worked, so the
                // chat is not left showing "recording..." forever.
                if ptt && let Err(e) = client.chatstate().send_paused(&jid).await {
                    tracing::warn!(error = %e, "WhatsApp: clearing recording indicator failed");
                }
                match sent {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "{} sent to {} via WhatsApp.",
                        if ptt { "Voice note" } else { "Audio" },
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send audio: {}", e))),
                }
            }

            // ── send_video ───────────────────────────────────────────────────
            "send_video" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the video");
                let path = pget!(get_str(&input, "media_path")).to_string();
                let caption = input
                    .get("caption")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let (bytes, mime, _filename) = pget!(read_local_media(&path, "video/mp4").await);
                let upload =
                    pget!(upload_media(&client, bytes, wacore::download::MediaType::Video).await);

                let wa_msg = waproto::whatsapp::Message {
                    video_message: waproto::whatsapp::message::VideoMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        caption,
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Video sent to {} via WhatsApp.",
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send video: {}", e))),
                }
            }

            // ── send_sticker ─────────────────────────────────────────────────
            "send_sticker" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the sticker");
                let path = pget!(get_str(&input, "media_path")).to_string();

                let (bytes, mime, _filename) = pget!(read_local_media(&path, "image/webp").await);
                let upload =
                    pget!(upload_media(&client, bytes, wacore::download::MediaType::Sticker).await);

                let wa_msg = waproto::whatsapp::Message {
                    sticker_message: waproto::whatsapp::message::StickerMessage {
                        url: Some(upload.url),
                        direct_path: Some(upload.direct_path),
                        media_key: Some(upload.media_key.to_vec()),
                        file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                        file_sha256: Some(upload.file_sha256.to_vec()),
                        file_length: Some(upload.file_length),
                        mimetype: Some(mime),
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Sticker sent to {} via WhatsApp.",
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send sticker: {}", e))),
                }
            }

            // ── send_location ────────────────────────────────────────────────
            "send_location" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the location");
                let lat = match get_f64(&input, "latitude") {
                    Some(v) => v,
                    None => {
                        return Ok(ToolResult::error(
                            "Missing required 'latitude' parameter.".to_string(),
                        ));
                    }
                };
                let lng = match get_f64(&input, "longitude") {
                    Some(v) => v,
                    None => {
                        return Ok(ToolResult::error(
                            "Missing required 'longitude' parameter.".to_string(),
                        ));
                    }
                };
                let name = input
                    .get("location_name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let address = input
                    .get("location_address")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let wa_msg = waproto::whatsapp::Message {
                    location_message: waproto::whatsapp::message::LocationMessage {
                        degrees_latitude: Some(lat),
                        degrees_longitude: Some(lng),
                        name,
                        address,
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Location ({}, {}) sent to {} via WhatsApp.",
                        lat, lng, jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send location: {}", e))),
                }
            }

            // ── send_contact ─────────────────────────────────────────────────
            "send_contact" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the contact card");
                let contact_name = pget!(get_str(&input, "contact_name")).to_string();
                let contact_phone = pget!(get_str(&input, "contact_phone")).to_string();

                let vcard = build_vcard(&contact_name, &contact_phone);
                let wa_msg = waproto::whatsapp::Message {
                    contact_message: waproto::whatsapp::message::ContactMessage {
                        display_name: Some(contact_name.clone()),
                        vcard: Some(vcard),
                        ..Default::default()
                    }
                    .into(),
                    ..Default::default()
                };
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Contact '{}' sent to {} via WhatsApp.",
                        contact_name, jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to send contact: {}", e))),
                }
            }

            // ── react ────────────────────────────────────────────────────────
            "react" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the reaction");
                let msg_id = pget!(get_str(&input, "message_id")).to_string();
                let emoji = input
                    .get("emoji")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let from_me = input
                    .get("from_me")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let message_key = waproto::whatsapp::MessageKey {
                    remote_jid: Some(jid.to_string()),
                    from_me: Some(from_me),
                    id: Some(msg_id.clone()),
                    ..Default::default()
                };

                let reaction = waproto::whatsapp::message::ReactionMessage {
                    key: message_key.into(),
                    text: if emoji.is_empty() {
                        None
                    } else {
                        Some(emoji.clone())
                    },
                    sender_timestamp_ms: Some(chrono::Utc::now().timestamp_millis()),
                    ..Default::default()
                };

                let reaction_msg = waproto::whatsapp::Message {
                    reaction_message: reaction.into(),
                    ..Default::default()
                };
                match client.send_message(jid, reaction_msg).await {
                    Ok(_) => {
                        if emoji.is_empty() {
                            Ok(ToolResult::success(format!(
                                "Reaction removed from message {} for {}.",
                                msg_id, jid_str
                            )))
                        } else {
                            Ok(ToolResult::success(format!(
                                "Reaction '{}' set on message {} for {}.",
                                emoji, msg_id, jid_str
                            )))
                        }
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to send reaction: {}", e))),
                }
            }

            // ── send_poll ────────────────────────────────────────────────────
            "send_poll" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the poll");
                let question = pget!(get_str(&input, "poll_question")).to_string();
                let opts: Vec<String> = match input.get("poll_options").and_then(|v| v.as_array()) {
                    Some(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect(),
                    None => {
                        return Ok(ToolResult::error(
                            "Missing required 'poll_options' parameter.".to_string(),
                        ));
                    }
                };
                if opts.len() < 2 {
                    return Ok(ToolResult::error(
                        "'poll_options' must have at least 2 options.".to_string(),
                    ));
                }
                if opts.len() > 12 {
                    return Ok(ToolResult::error(
                        "'poll_options' supports a maximum of 12 options.".to_string(),
                    ));
                }

                // #1482: built through the lib rather than by hand. A poll
                // whose MessageContextInfo carries no `message_secret` can
                // never have its votes decrypted - voters derive their
                // encryption key from it - and the hand-rolled version set
                // none, so every vote on a bot poll was unreadable by
                // construction. `polls().create` mints the secret, persists it
                // for the decode path, and picks the proto version WhatsApp
                // Web uses for the select count (v3 single, v1 multi).
                // WhatsApp encodes "how many options a voter may pick".
                // Default 1 (single choice); `multi_select: true` lets a voter
                // pick any number, which is what the count means at its max.
                let selectable = if input
                    .get("multi_select")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    opts.len() as u32
                } else {
                    1
                };
                match client
                    .polls()
                    .create(jid, &question, &opts, selectable)
                    .await
                {
                    Ok((result, _secret)) => {
                        // Votes name their options by hash, never by label, so
                        // a vote can only be read back against the poll's own
                        // option list. Remember it, keyed on the message id.
                        self.whatsapp_state
                            .polls
                            .remember(result.message_id, opts)
                            .await;
                        Ok(ToolResult::success(format!(
                            "Poll '{}' sent to {} via WhatsApp.",
                            question, jid_str
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to send poll: {}", e))),
                }
            }

            // ── typing ───────────────────────────────────────────────────────
            "typing" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let active = input
                    .get("typing_active")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                let result = if active {
                    client.chatstate().send_composing(&jid).await
                } else {
                    client.chatstate().send_paused(&jid).await
                };
                match result {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Typing {} sent to {}.",
                        if active { "composing" } else { "paused" },
                        jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to send typing indicator: {}",
                        e
                    ))),
                }
            }

            // ── mark_read ────────────────────────────────────────────────────
            "mark_read" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let msg_id = pget!(get_str(&input, "message_id")).to_string();

                // Build read receipt node manually (send_protocol_receipt is pub(crate))
                // Receipt goes TO the message sender (jid), not ourselves
                let node = wacore_binary::builder::NodeBuilder::new("receipt")
                    .attrs([
                        ("id", msg_id.clone()),
                        ("type", "read".to_string()),
                        ("to", jid.to_string()),
                    ])
                    .build();
                match client.send_node(node).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Message {} marked as read for {}.",
                        msg_id, jid_str
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to mark message as read: {}",
                        e
                    ))),
                }
            }

            // ── send_buttons ─────────────────────────────────────────────────
            // #1411. Exists so the native-flow card can be tried against a
            // real phone WITHOUT handing the approval flow over to it first:
            // that flow is safety-critical, and a prompt that does not render
            // is a prompt the owner cannot answer. Send one of these, look at
            // the screen, and only then decide about `interactive_buttons`.
            "send_buttons" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                gate_send!(self, jid_str, "the button message");
                let body = pget!(get_str(&input, "message")).to_string();
                let labels: Vec<String> = match input.get("buttons").and_then(|v| v.as_array()) {
                    Some(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .filter(|l| !l.trim().is_empty())
                        .collect(),
                    None => Vec::new(),
                };
                if labels.is_empty() {
                    return Ok(ToolResult::error(
                        "send_buttons needs a non-empty 'buttons' array of labels.".to_string(),
                    ));
                }
                let too_many = labels.len() > interactive::MAX_BUTTONS;
                // Ids are derived from the labels so a tap is legible in the
                // logs without the caller having to invent a parallel id
                // scheme for a one-off test message.
                let buttons: Vec<interactive::Button> = labels
                    .iter()
                    .take(interactive::MAX_BUTTONS)
                    .map(|l| {
                        interactive::Button::new(
                            format!("btn_{}", l.trim().to_lowercase().replace(' ', "_")),
                            l.trim(),
                        )
                    })
                    .collect();
                let wa_msg = interactive::build(
                    &body,
                    input.get("footer").and_then(|v| v.as_str()),
                    &buttons,
                );
                match client.send_message(jid, wa_msg).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Button message sent to {} with {} button(s){}. If the card does not \
                         render on the recipient's phone, that is the #1411 answer: WhatsApp \
                         drops it silently rather than erroring, so a successful send here is \
                         NOT proof it was seen.",
                        jid_str,
                        buttons.len(),
                        if too_many {
                            format!(
                                " ({} dropped; WhatsApp draws at most {})",
                                labels.len() - interactive::MAX_BUTTONS,
                                interactive::MAX_BUTTONS
                            )
                        } else {
                            String::new()
                        }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to send the button message: {e}"
                    ))),
                }
            }

            // ── status_update ────────────────────────────────────────────────
            // #1485. A status goes to a LIST of recipients, so it is the one
            // action here that can reach many people from one call. Both rails
            // apply: every target must be on the configured allowlist, and the
            // sends are paced, so there is no fire-and-forget mass-send path.
            "status_update" => {
                // #1485 follow-up: a status is text OR an image. `message` is
                // therefore optional so it can double as the image caption; a
                // status carrying neither is refused here rather than sending
                // an empty one.
                let text = input
                    .get("message")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let media_path = input
                    .get("media_path")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                if text.is_none() && media_path.is_none() {
                    return Ok(ToolResult::error(
                        "status_update needs 'message' (the text status, or the caption for an \
                         image status) or 'media_path' (a local image file). Nothing was sent."
                            .to_string(),
                    ));
                }
                let (allowlist, pace) = {
                    let wa = &self.config_rx.borrow().channels.whatsapp;
                    (
                        wa.broadcast.allowed_targets.clone(),
                        broadcast::delay(wa.broadcast.min_delay_seconds),
                    )
                };
                let targets: Vec<String> = match input.get("targets").and_then(|v| v.as_array()) {
                    Some(arr) => arr
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                    None => allowlist.clone(),
                };
                if targets.is_empty() {
                    return Ok(ToolResult::error(
                        "No status recipients. Set channels.whatsapp.broadcast.allowed_targets \
                         in config.toml, or pass 'targets'. An empty allowlist allows nobody on \
                         purpose."
                            .to_string(),
                    ));
                }
                let (allowed, refused) = broadcast::partition(&targets, &allowlist);
                if !refused.is_empty() {
                    return Ok(ToolResult::error(format!(
                        "{} target(s) are not on channels.whatsapp.broadcast.allowed_targets and \
                         NOTHING was sent: {}. Broadcast is opt-in per number; add them to the \
                         config first.",
                        refused.len(),
                        refused
                            .iter()
                            .map(|t| t.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )));
                }
                let mut jids = Vec::with_capacity(allowed.len());
                for target in &allowed {
                    match broadcast::target_jid(target) {
                        Some(jid) => jids.push(jid),
                        None => {
                            return Ok(ToolResult::error(format!(
                                "'{target}' is not a usable phone number; nothing was sent."
                            )));
                        }
                    }
                }
                // #2177: build the payload ONCE, before any slot is
                // charged, so a bad image or a failed upload never spends
                // budget. Then charge and send per recipient, with the pace
                // between two sends instead of before the first.
                let (payload, kind) = match media_path {
                    Some(path) => {
                        let (bytes, mime, _filename) =
                            pget!(read_local_media(&path, "image/jpeg").await);
                        if !mime.starts_with("image/") {
                            return Ok(ToolResult::error(format!(
                                "'{path}' is {mime}; a status carries text or an image, and \
                                 nothing was sent."
                            )));
                        }
                        let thumbnail = match make_jpeg_thumbnail(&bytes) {
                            Some(t) => t,
                            None => {
                                return Ok(ToolResult::error(format!(
                                    "'{path}' could not be decoded as an image; nothing was sent."
                                )));
                            }
                        };
                        let upload = pget!(
                            upload_media(&client, bytes, wacore::download::MediaType::Image).await
                        );
                        (
                            StatusPayload::Image {
                                upload: Box::new(upload),
                                thumbnail,
                                caption: text,
                            },
                            format!("image status from {path}"),
                        )
                    }
                    None => (
                        StatusPayload::Text {
                            body: text.unwrap_or_default(),
                        },
                        "text status".to_string(),
                    ),
                };
                let mut sink = LiveStatusSink {
                    tool: self,
                    client: client.as_ref(),
                    payload,
                };
                let outcome = fan_out_status(&mut sink, &jids, pace).await;
                Ok(status_report(&kind, jids.len(), &outcome, pace))
            }

            // ── list_newsletters ─────────────────────────────────────────────
            // #1485. Read-only, so no rails: it names what this account already
            // follows and is the only way to learn a newsletter JID to post to.
            "list_newsletters" => match client.newsletter().list_subscribed().await {
                Ok(list) if list.is_empty() => Ok(ToolResult::success(
                    "This WhatsApp account follows no newsletters (channels).".to_string(),
                )),
                Ok(list) => Ok(ToolResult::success(format!(
                    "{} newsletter(s):\n{}",
                    list.len(),
                    list.iter()
                        .map(|n| format!("- {} ({})", n.name, n.jid))
                        .collect::<Vec<_>>()
                        .join("\n")
                ))),
                Err(e) => Ok(ToolResult::error(format!(
                    "Failed to list newsletters: {e}"
                ))),
            },

            // ── create_newsletter ────────────────────────────────────────────
            // #1827. Creating a channel is an account-modifying broadcast-surface
            // action: it publishes nothing, so the #1407 budget is not charged,
            // but it is refused until a human has switched the broadcast rail on
            // (non-empty allowlist). Same posture as status_update, because an
            // unconfigured install must not be one tool call away from minting a
            // channel on the owner's number.
            "create_newsletter" => {
                let allowlist = self
                    .config_rx
                    .borrow()
                    .channels
                    .whatsapp
                    .broadcast
                    .allowed_targets
                    .clone();
                if allowlist.is_empty() {
                    return Ok(ToolResult::error(
                        "Creating a newsletter is a broadcast-surface action and the broadcast \
                         rail is off. Add at least one entry to \
                         channels.whatsapp.broadcast.allowed_targets in config.toml first; an \
                         unconfigured install must not be one tool call away from minting a \
                         channel on the owner's number."
                            .to_string(),
                    ));
                }
                let name = pget!(get_str(&input, "message")).to_string();
                let description = input
                    .get("description")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                match client.newsletter().create(&name, description).await {
                    Ok(meta) => {
                        let invite = match &meta.invite_code {
                            Some(code) => {
                                format!(" Invite link: https://whatsapp.com/channel/{code}.")
                            }
                            None => String::new(),
                        };
                        Ok(ToolResult::success(format!(
                            "Newsletter '{}' created ({}).{invite} Add the JID to \
                             channels.whatsapp.newsletters to have its posts digested, and to \
                             channels.whatsapp.broadcast.allowed_targets to post to it from \
                             here.",
                            meta.name, meta.jid
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to create the newsletter: {e}"
                    ))),
                }
            }

            // ── post_newsletter ──────────────────────────────────────────────
            // #1827. This one DELIVERS a message, so it takes the same rails as
            // every other message-delivering action: the target must be on the
            // broadcast allowlist (opt-in per channel, like status) and the send
            // is charged to the shared #1407 budget. There is no fire-and-forget
            // path. The body is text or an image-with-caption, built by
            // `build_post` so the proto shape is testable without a connection.
            //
            // A channel JID carries no phone, so `gate_send!` resolves the owner
            // as false and the post paces on the non-owner budget. That is
            // stricter than a DM, not a hole.
            "post_newsletter" => {
                let raw = pget!(get_str(&input, "newsletter")).to_string();
                let jid = match newsletter_jid(&raw) {
                    Ok(j) => j,
                    Err(reason) => return Ok(ToolResult::error(reason)),
                };
                let allowlist = self
                    .config_rx
                    .borrow()
                    .channels
                    .whatsapp
                    .broadcast
                    .allowed_targets
                    .clone();
                if !broadcast::is_allowed(&raw, &allowlist) {
                    return Ok(ToolResult::error(format!(
                        "'{raw}' is not on channels.whatsapp.broadcast.allowed_targets, so \
                         NOTHING was sent. Broadcast is opt-in per channel; add the JID to the \
                         config first."
                    )));
                }
                let text = input
                    .get("message")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let media_path = input
                    .get("media_path")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let image = match media_path {
                    Some(path) => {
                        let (bytes, mime, _filename) =
                            pget!(read_local_media(&path, "image/jpeg").await);
                        if !mime.starts_with("image/") {
                            return Ok(ToolResult::error(format!(
                                "'{path}' is {mime}; a newsletter post carries text or an \
                                 image, and nothing was sent."
                            )));
                        }
                        let upload = pget!(
                            upload_media(&client, bytes, wacore::download::MediaType::Image).await
                        );
                        Some(waproto::whatsapp::message::ImageMessage {
                            url: Some(upload.url),
                            direct_path: Some(upload.direct_path),
                            media_key: Some(upload.media_key.to_vec()),
                            file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
                            file_sha256: Some(upload.file_sha256.to_vec()),
                            file_length: Some(upload.file_length),
                            mimetype: Some(mime),
                            ..Default::default()
                        })
                    }
                    None => None,
                };
                let msg = match build_post(text.as_deref(), image) {
                    Ok(m) => m,
                    Err(reason) => return Ok(ToolResult::error(reason)),
                };
                let jid_str = jid.to_string();
                gate_send!(self, jid_str, "the newsletter post");
                match client.send_message(jid, msg).await {
                    Ok(sent) => Ok(ToolResult::success(format!(
                        "Posted to newsletter {jid_str} (message id {}).",
                        sent.message_id
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to post to {jid_str}: {e}"
                    ))),
                }
            }

            // ── create_label / assign_label ──────────────────────────────────
            // #1485. Labels are private organisation on the owner's own account:
            // nothing is delivered to anyone, so the broadcast rails do not
            // apply and the send budget is not charged.
            "create_label" => {
                let name = pget!(get_str(&input, "message")).to_string();
                let label_id = input
                    .get("label_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        // App state is an upsert keyed by label_id, so a caller
                        // that supplies none gets a fresh label rather than
                        // silently renaming an existing one.
                        uuid::Uuid::new_v4().to_string()
                    });
                let color = input
                    .get("label_color")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32;
                match client.labels().create_label(&label_id, &name, color).await {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Label '{name}' saved with id {label_id}."
                    ))),
                    Err(e) => Ok(ToolResult::error(format!("Failed to save the label: {e}"))),
                }
            }

            "assign_label" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                let label_id = pget!(get_str(&input, "label_id")).to_string();
                let remove = input
                    .get("remove")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let result = if remove {
                    client.labels().remove_chat_label(&label_id, &jid).await
                } else {
                    client.labels().add_chat_label(&label_id, &jid).await
                };
                match result {
                    Ok(_) => Ok(ToolResult::success(format!(
                        "Label {label_id} {} chat with {jid_str}.",
                        if remove { "removed from" } else { "added to" }
                    ))),
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to update the label on {jid_str}: {e}"
                    ))),
                }
            }

            // ── block_contact ────────────────────────────────────────────────
            // #1487: the owner's first-line defence against a spamming number.
            // The resolved JID is logged so the block is auditable after the
            // fact, and the local mirror is updated so the inbound guard drops
            // that sender on the very next message without a server round trip.
            "block_contact" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                match client.blocking().block(&jid).await {
                    Ok(_) => {
                        tracing::info!(
                            target: "whatsapp",
                            jid = %jid,
                            "blocked contact on owner request (#1487)"
                        );
                        self.whatsapp_state.blocklist.insert(&jid.to_string()).await;
                        Ok(ToolResult::success(format!(
                            "Blocked {} on WhatsApp. Their messages no longer reach the bot.",
                            jid_str
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!("Failed to block {jid_str}: {e}"))),
                }
            }

            // ── unblock_contact ──────────────────────────────────────────────
            "unblock_contact" => {
                let (jid, jid_str) = pget!(
                    resolve_jid(
                        &input,
                        &self.whatsapp_state,
                        &self.config_rx,
                        context.session_id
                    )
                    .await
                );
                match client.blocking().unblock(&jid).await {
                    Ok(_) => {
                        tracing::info!(
                            target: "whatsapp",
                            jid = %jid,
                            "unblocked contact on owner request (#1487)"
                        );
                        self.whatsapp_state.blocklist.remove(&jid.to_string()).await;
                        Ok(ToolResult::success(format!(
                            "Unblocked {} on WhatsApp.",
                            jid_str
                        )))
                    }
                    Err(e) => Ok(ToolResult::error(format!(
                        "Failed to unblock {jid_str}: {e}"
                    ))),
                }
            }

            // ── list_blocked ─────────────────────────────────────────────────
            // Reads the server, not the mirror: the owner may have blocked
            // someone from their phone, and reporting a stale local set as
            // fact would be worse than one extra round trip. The mirror is
            // refreshed from the answer.
            "list_blocked" => match client.blocking().get_blocklist().await {
                Ok(entries) => {
                    let jids: Vec<String> = entries.iter().map(|e| e.jid.to_string()).collect();
                    self.whatsapp_state.blocklist.replace(jids.clone()).await;
                    if jids.is_empty() {
                        return Ok(ToolResult::success(
                            "No blocked contacts on this WhatsApp account.".to_string(),
                        ));
                    }
                    Ok(ToolResult::success(format!(
                        "{} blocked contact(s):\n{}",
                        jids.len(),
                        jids.join("\n")
                    )))
                }
                Err(e) => Ok(ToolResult::error(format!(
                    "Failed to fetch the blocklist: {e}"
                ))),
            },

            unknown => Ok(ToolResult::error(format!(
                "Unknown action '{}'. Valid actions: send, reply, delete, send_photo, \
                 send_document, send_audio, send_video, send_sticker, send_location, \
                 send_contact, react, send_poll, typing, mark_read, block_contact, \
                 unblock_contact, list_blocked, pin, unpin, forward, set_profile_name, \
                 set_profile_status, status_update, list_newsletters, create_newsletter, \
                 post_newsletter, create_label, assign_label, send_buttons",
                unknown
            ))),
        }
    }
}
