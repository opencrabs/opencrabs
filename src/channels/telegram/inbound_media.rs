//! Inbound media ingestion, extracted from `handler.rs` (#1086 seam 4).
//!
//! One arm per inbound kind (text, voice, photo, video, animation, video
//! note, document, unhandled) resolving a Telegram message down to the text
//! the agent actually sees: STT transcripts for voice, vision markers for
//! images, extracted content for documents. Arms that fully answer the
//! message (an unsupported kind, a failed download, an empty body) report
//! `Ingested::Handled` so the caller returns without running a turn.

use std::sync::Arc;

use teloxide::prelude::*;
use teloxide::types::ThreadId;

use super::handler::forward_origin_label;
use super::media::{fetch_file_or_notify, prepend_caption};
use super::send::{fire_chat_action, message_in_thread};
use super::state::TelegramState;
use crate::config::Config;
use crate::config::types::VoiceConfig;
use crate::utils::truncate_str;

pub(crate) enum Ingested {
    /// The message was fully handled here; the caller returns immediately.
    Handled,
    /// Text resolved from the message or its media, and whether it came from
    /// a voice note (which changes how the reply is delivered).
    Text { text: String, is_voice: bool },
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn ingest_message_text(
    bot: &Bot,
    msg: &Message,
    user: &teloxide::types::User,
    user_id: i64,
    thread_id: Option<ThreadId>,
    is_dm: bool,
    bot_token: &Arc<String>,
    telegram_state: &TelegramState,
    cfg: &Config,
    voice_config: &VoiceConfig,
    tmp_voice_transcript: &mut Option<String>,
) -> Result<Ingested, teloxide::RequestError> {
    // Extract text from either text message or voice note (via STT)
    let (text, is_voice) = if let Some(t) = msg.text() {
        if t.is_empty() && tmp_voice_transcript.is_none() {
            return Ok(Ingested::Handled);
        }
        (t.to_string(), false)
    } else if let Some(voice) = msg.voice() {
        // Voice note -- transcribe via STT provider
        if !voice_config.stt_enabled {
            message_in_thread(bot, msg.chat.id, thread_id, "Voice notes are not enabled.").await?;
            return Ok(Ingested::Handled);
        }

        tracing::info!(
            "Telegram: voice note from user {} ({}) — {}s",
            user_id,
            user.first_name,
            voice.duration,
        );

        // Show typing immediately so user knows we're processing
        fire_chat_action(
            bot,
            msg.chat.id,
            thread_id,
            teloxide::types::ChatAction::Typing,
            "immediate ack",
        )
        .await;

        // Download the voice file from Telegram
        let Some(file) = fetch_file_or_notify(
            bot,
            voice.file.id.clone(),
            msg.chat.id,
            thread_id,
            "voice note",
        )
        .await
        else {
            return Ok(Ingested::Handled);
        };
        let download_url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            bot_token.as_str(),
            file.path
        );

        let audio_bytes = match crate::utils::download::fetch_bytes(&download_url).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Telegram: voice note download failed: {e:#}");
                message_in_thread(bot, msg.chat.id, thread_id, e.user_message("voice note"))
                    .await?;
                return Ok(Ingested::Handled);
            }
        };

        // Transcribe with STT dispatch (API or Local based on config)
        match crate::channels::voice::transcribe(audio_bytes, voice_config).await {
            Ok(transcript) => {
                tracing::info!(
                    "Telegram: transcribed voice: {}",
                    truncate_str(&transcript, 80)
                );
                (transcript, true)
            }
            Err(e) => {
                tracing::error!("Telegram: STT error: {}", e);
                message_in_thread(
                    bot,
                    msg.chat.id,
                    thread_id,
                    format!("Transcription error: {}", e),
                )
                .await?;
                return Ok(Ingested::Handled);
            }
        }
    } else if let Some(photos) = msg.photo() {
        // Photo -- download and send to agent as image attachment
        let Some(photo) = photos.last() else {
            return Ok(Ingested::Handled);
        };
        tracing::info!(
            "Telegram: photo from user {} ({}) — {}x{}",
            user_id,
            user.first_name,
            photo.width,
            photo.height,
        );

        let Some(file) =
            fetch_file_or_notify(bot, photo.file.id.clone(), msg.chat.id, thread_id, "photo").await
        else {
            return Ok(Ingested::Handled);
        };
        let download_url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            bot_token.as_str(),
            file.path
        );

        let photo_bytes = match crate::utils::download::fetch_bytes(&download_url).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Telegram: photo download failed: {e:#}");
                message_in_thread(bot, msg.chat.id, thread_id, e.user_message("photo"))
                    .await?;
                return Ok(Ingested::Handled);
            }
        };

        // Route through the shared vision pipeline — saves to ~/.opencrabs/tmp/files/
        // and returns a <<IMG:path>> marker. Centralized temp management, single cleanup.
        use crate::utils::{inject_file_content, process_file_with_vision};
        let fc = process_file_with_vision(&photo_bytes, "image/jpeg", "photo.jpg", cfg);
        let img_marker = inject_file_content(&fc).0;

        // Check if this photo is part of an album (media group).
        // Telegram tags every album item with the same media_group_id.
        // Only debounce for albums — single photos dispatch immediately (no 3s latency).
        let chat_id = msg.chat.id.0;
        let result = if let Some(media_group_id) = msg.media_group_id() {
            // Album photo — buffer with caption for batching
            let caption = msg.caption().map(|s| s.to_string());
            let buffer_size = telegram_state
                .buffer_photo(
                    chat_id,
                    user_id,
                    media_group_id.0.as_str(),
                    img_marker,
                    caption,
                )
                .await;
            tracing::info!(
                "Telegram: buffered album photo {} for user {} in chat {} (media_group={})",
                buffer_size,
                user_id,
                chat_id,
                media_group_id
            );

            // Reset debounce timer and wait. If another photo arrives in the same album,
            // it cancels this wait and we return early. If 3 seconds pass with no new photos,
            // we drain the buffer and process all photos together.
            let token = telegram_state
                .reset_photo_debounce(chat_id, user_id, media_group_id.0.as_str())
                .await;
            let expired = telegram_state.wait_photo_debounce(token).await;

            if !expired {
                // Another photo cancelled our timer — that photo will handle the batch
                tracing::debug!(
                    "Telegram: album photo debounce cancelled, waiting for next photo in batch"
                );
                return Ok(Ingested::Handled);
            }

            // Debounce expired — drain all buffered photos for this album
            let buffered = telegram_state
                .drain_photo_buffer(chat_id, user_id, media_group_id.0.as_str())
                .await;
            telegram_state
                .cleanup_photo_debounce(chat_id, user_id, media_group_id.0.as_str())
                .await;

            // Bail out if buffer is empty (edge case: ghost dispatch)
            if buffered.is_empty() {
                tracing::warn!(
                    "Telegram: album photo buffer empty after drain — skipping dispatch"
                );
                return Ok(Ingested::Handled);
            }

            tracing::info!(
                "Telegram: processing album batch of {} photo(s) from user {} in chat {} (media_group={})",
                buffered.len(),
                user_id,
                chat_id,
                media_group_id
            );

            // Combine all img markers. Caption is on the first photo in the album.
            let markers: Vec<String> = buffered.iter().map(|(m, _)| m.clone()).collect();
            let caption = buffered
                .iter()
                .find_map(|(_, c)| c.clone())
                .unwrap_or_default();

            if markers.len() == 1 {
                let injected = markers.into_iter().next().unwrap();
                prepend_caption(&caption, injected)
            } else {
                let combined = markers.join("\n");
                prepend_caption(&caption, combined)
            }
        } else {
            // Single photo (not part of an album) — dispatch immediately, no debounce
            tracing::info!(
                "Telegram: processing single photo from user {} in chat {} (no media_group)",
                user_id,
                chat_id
            );

            let caption = msg.caption().unwrap_or("");
            prepend_caption(caption, img_marker)
        };
        (result, false)
    } else if let Some(vid) = msg.video() {
        let fname = vid.file_name.as_deref().unwrap_or("video.mp4").to_string();
        let mime = vid
            .mime_type
            .as_ref()
            .map(|m| m.as_ref().to_string())
            .unwrap_or_else(|| "video/mp4".to_string());
        let caption = msg.caption().unwrap_or("").to_string();

        tracing::info!(
            "Telegram: video from user {} — name={} mime={} duration={}s",
            user_id,
            fname,
            mime,
            vid.duration
        );

        let Some(file) =
            fetch_file_or_notify(bot, vid.file.id.clone(), msg.chat.id, thread_id, "video").await
        else {
            return Ok(Ingested::Handled);
        };
        let download_url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            bot_token.as_str(),
            file.path
        );

        let bytes = match crate::utils::download::fetch_bytes(&download_url).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Telegram: video download failed: {e:#}");
                message_in_thread(bot, msg.chat.id, thread_id, e.user_message("video"))
                    .await?;
                return Ok(Ingested::Handled);
            }
        };

        use crate::utils::{inject_file_content, process_file_with_vision};
        let content = process_file_with_vision(&bytes, &mime, &fname, cfg);
        let injected = inject_file_content(&content).0;
        let result = prepend_caption(&caption, injected);
        (result, false)
    } else if let Some(anim) = msg.animation() {
        // Animations are Telegram's auto-converted short videos (iPhone .mov →
        // GIF-style preview). Bytes are always MP4 internally even when
        // `mime_type` is reported as `image/gif`, so force `video/mp4`.
        let fname = anim
            .file_name
            .as_deref()
            .unwrap_or("animation.mp4")
            .to_string();
        let caption = msg.caption().unwrap_or("").to_string();

        tracing::info!(
            "Telegram: animation from user {} — name={} duration={}s",
            user_id,
            fname,
            anim.duration
        );

        let Some(file) = fetch_file_or_notify(
            bot,
            anim.file.id.clone(),
            msg.chat.id,
            thread_id,
            "animation",
        )
        .await
        else {
            return Ok(Ingested::Handled);
        };
        let download_url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            bot_token.as_str(),
            file.path
        );

        let bytes = match crate::utils::download::fetch_bytes(&download_url).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Telegram: animation download failed: {e:#}");
                message_in_thread(bot, msg.chat.id, thread_id, e.user_message("animation"))
                    .await?;
                return Ok(Ingested::Handled);
            }
        };

        use crate::utils::{inject_file_content, process_file_with_vision};
        let content = process_file_with_vision(&bytes, "video/mp4", &fname, cfg);
        let injected = inject_file_content(&content).0;
        let result = prepend_caption(&caption, injected);
        (result, false)
    } else if let Some(vnote) = msg.video_note() {
        let fname = "video_note.mp4".to_string();

        tracing::info!(
            "Telegram: video_note from user {} — duration={}s length={}px",
            user_id,
            vnote.duration,
            vnote.length
        );

        let Some(file) = fetch_file_or_notify(
            bot,
            vnote.file.id.clone(),
            msg.chat.id,
            thread_id,
            "video note",
        )
        .await
        else {
            return Ok(Ingested::Handled);
        };
        let download_url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            bot_token.as_str(),
            file.path
        );

        let bytes = match crate::utils::download::fetch_bytes(&download_url).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Telegram: video note download failed: {e:#}");
                message_in_thread(bot, msg.chat.id, thread_id, e.user_message("video note"))
                    .await?;
                return Ok(Ingested::Handled);
            }
        };

        use crate::utils::{inject_file_content, process_file_with_vision};
        let content = process_file_with_vision(&bytes, "video/mp4", &fname, cfg);
        let injected = inject_file_content(&content).0;
        (injected, false)
    } else if let Some(doc) = msg.document() {
        let fname = doc.file_name.as_deref().unwrap_or("file");
        let raw_mime = doc.mime_type.as_ref().map(|m| m.as_ref()).unwrap_or("");
        // Telegram sometimes labels MP4-backed animations as `image/gif` when
        // delivered via the document path. Detect by extension and rewrite so
        // `process_file_with_vision` routes to the video branch.
        let lower_name = fname.to_lowercase();
        let mime: &str = if raw_mime == "image/gif"
            && (lower_name.ends_with(".mp4") || lower_name.ends_with(".mov"))
        {
            "video/mp4"
        } else {
            raw_mime
        };
        let caption = msg.caption().unwrap_or("");

        tracing::info!(
            "Telegram: document from user {} — name={} mime={}",
            user_id,
            fname,
            mime
        );

        let Some(file) =
            fetch_file_or_notify(bot, doc.file.id.clone(), msg.chat.id, thread_id, "document")
                .await
        else {
            return Ok(Ingested::Handled);
        };
        let download_url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            bot_token.as_str(),
            file.path
        );

        let bytes = match crate::utils::download::fetch_bytes(&download_url).await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Telegram: file download failed: {e:#}");
                message_in_thread(bot, msg.chat.id, thread_id, e.user_message("file"))
                    .await?;
                return Ok(Ingested::Handled);
            }
        };

        use crate::utils::{inject_file_content, process_file_with_vision};
        let content = process_file_with_vision(&bytes, mime, fname, cfg);
        let result = inject_file_content(&content).0;
        let result = prepend_caption(caption, result);
        (result, false)
    } else {
        // A message that reached the handler with NO typed content. Forwards
        // of rich-formatted messages land here: teloxide's typed parse drops
        // content fields it does not know, sometimes together with the
        // forward metadata (forward_origin() exists only on Common kinds).
        // The bytes still arrived — the raw-aware listener (#354) stashed the
        // message's raw JSON before the typed parse could lose it.
        let typed_origin = forward_origin_label(msg);
        let raw = super::raw_updates::take_raw_message(msg.chat.id.0, msg.id.0);
        let raw_origin = raw
            .as_ref()
            .and_then(super::raw_updates::raw_forward_origin);
        let origin = typed_origin.or(raw_origin);
        tracing::warn!(
            "Telegram: message {} in chat {} has no typed content — origin={:?}, raw_stashed={}, kind={}",
            msg.id.0,
            msg.chat.id.0,
            origin,
            raw.is_some(),
            truncate_str(&format!("{:?}", msg.kind), 400),
        );
        let relevant = is_dm || origin.is_some();
        match (raw, relevant) {
            (Some(raw), true) => {
                let origin_note = origin
                    .map(|o| format!(" forwarded from \"{o}\""))
                    .unwrap_or_default();
                // Decode recognized rich content types into readable text
                // (#359); the raw-JSON dump stays as the safety net for
                // whatever content type comes next.
                match super::rich_decode::decode_rich_content(&raw) {
                    Some(decoded) => (format!("[A rich message{origin_note}]:\n{decoded}"), false),
                    None => {
                        let payload = super::raw_updates::raw_content_for_agent(&raw);
                        (
                            format!(
                                "[A message{origin_note} arrived in a format the Bot API \
                                 client cannot decode. Its raw Bot API payload follows — read \
                                 the content directly from it:]\n```json\n{payload}\n```"
                            ),
                            false,
                        )
                    }
                }
            }
            (None, true) => {
                // Raw stash missed too (restart raced the stash, or another
                // consumer took it). NEVER silent: tell the user plainly.
                tracing::error!(
                    "Telegram: undecodable message {} in chat {} and no raw payload \
                     available — informing the user",
                    msg.id.0,
                    msg.chat.id.0,
                );
                message_in_thread(
                    bot,
                    msg.chat.id,
                    thread_id,
                    "⚠️ I received your message but could not decode its content \
                     (unsupported message type) and the raw payload was unavailable. \
                     Please paste it as text.",
                )
                .await?;
                return Ok(Ingested::Handled);
            }
            (_, false) => {
                // Group service messages (pins, topic events, ...) — ignore.
                return Ok(Ingested::Handled);
            }
        }
    };

    Ok(Ingested::Text { text, is_voice })
}
