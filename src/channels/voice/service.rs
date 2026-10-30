//! Voice Processing Module
//!
//! Speech-to-text (Groq Whisper) and text-to-speech (OpenAI TTS) services
//! used by the Telegram bot for voice note support.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;

use crate::config::{SttMode, TtsMode, VoiceConfig};
use crate::utils::string::truncate_str;

const GROQ_TRANSCRIPTION_URL: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
const OPENAI_SPEECH_URL: &str = "https://api.openai.com/v1/audio/speech";

/// Transcribe audio bytes using Groq Whisper (whisper-large-v3-turbo).
///
/// Accepts OGG/Opus audio (Telegram voice note format).
/// Returns the transcribed text.
pub async fn transcribe_audio(audio_bytes: Vec<u8>, groq_api_key: &str) -> Result<String> {
    transcribe_audio_with_url(audio_bytes, groq_api_key, GROQ_TRANSCRIPTION_URL).await
}

/// Dispatch STT transcription based on config.
///
/// - Voicebox STT (voicebox_stt_enabled) — priority when enabled
/// - OpenAI-compatible STT (stt_base_url + stt_model)
/// - Groq Whisper API (stt_provider)
/// - Local whisper (local-stt feature)
///
/// When the active provider fails, walks `voice_config.stt_fallback_chain`
/// in order (user-configured, e.g. `["groq", "openai_compatible", "local"]`)
/// and tries each entry that has the credentials/config it needs. Empty
/// chain falls back to the default order (groq → openai_compatible →
/// local, skipping whichever the primary was).
///
/// Returns success on the first provider that produces a transcription;
/// returns a composite error citing every attempted provider if the whole
/// chain failed.
pub async fn transcribe(audio_bytes: Vec<u8>, voice_config: &VoiceConfig) -> Result<String> {
    let primary = resolve_primary_stt(voice_config);
    let chain = resolve_fallback_chain(voice_config, primary);
    let candidates: Vec<SttProviderKind> = std::iter::once(primary).chain(chain).collect();

    let mut attempts: Vec<String> = Vec::with_capacity(candidates.len());
    let mut audio = Some(audio_bytes);
    for kind in candidates {
        // Take the audio out once — Vec<u8> isn't free to clone for a
        // multi-MB voice note. Each attempt CONSUMES it; the next
        // attempt re-clones from a stashed copy.
        let bytes = match audio.take() {
            Some(b) => {
                if !attempts.is_empty() {
                    // Already failed once — clone before consuming so
                    // a later fallback still has a copy.
                }
                b
            }
            None => break,
        };
        let bytes_for_next = bytes.clone();
        attempts.push(kind.label());
        match try_stt(kind, bytes, voice_config).await {
            Ok(text) => {
                if attempts.len() > 1 {
                    tracing::info!(
                        "STT recovered via fallback chain: {} (attempts: {})",
                        kind.label(),
                        attempts.join(" → "),
                    );
                }
                return Ok(text);
            }
            Err(e) => {
                tracing::warn!(
                    "STT provider '{}' failed: {} — trying next in chain",
                    kind.label(),
                    e,
                );
                if let Some(s) = attempts.last_mut() {
                    s.push_str(&format!(": {e}"));
                }
                audio = Some(bytes_for_next);
            }
        }
    }

    anyhow::bail!(
        "All STT providers failed. Attempts:\n  - {}",
        attempts.join("\n  - "),
    )
}

/// Resolve which provider runs first.
///
/// A non-empty user chain is authoritative (#1399): its first entry that
/// is actually configured is the primary, so `fallback_chain = ["groq",
/// "local"]` means "Groq first", not "Groq after whatever the heuristic
/// picked". Only when the chain is empty (or names nothing usable) does
/// the default priority below decide.
pub(crate) fn resolve_primary_stt(cfg: &VoiceConfig) -> SttProviderKind {
    if let Some(head) = cfg
        .stt_fallback_chain
        .iter()
        .filter_map(|s| SttProviderKind::from_label(s))
        .find(|k| provider_is_configured(*k, cfg))
    {
        return head;
    }
    if cfg.voicebox_stt_enabled {
        SttProviderKind::Voicebox
    } else if cfg.stt_base_url.is_some() && cfg.stt_model.is_some() && cfg.stt_api_key.is_some() {
        SttProviderKind::OpenAiCompatible
    } else if cfg
        .stt_provider
        .as_ref()
        .and_then(|p| p.api_key.as_ref())
        .is_some()
    {
        SttProviderKind::Groq
    } else {
        SttProviderKind::Local
    }
}

/// Build the ordered list of fallback providers after `primary`.
/// Honours the user-configured chain when present, otherwise uses the
/// default priority list with the primary removed.
pub(crate) fn resolve_fallback_chain(
    cfg: &VoiceConfig,
    primary: SttProviderKind,
) -> Vec<SttProviderKind> {
    let user_chain = &cfg.stt_fallback_chain;
    let raw: Vec<SttProviderKind> = if user_chain.is_empty() {
        vec![
            SttProviderKind::Voicebox,
            SttProviderKind::OpenAiCompatible,
            SttProviderKind::Groq,
            SttProviderKind::Local,
        ]
    } else {
        user_chain
            .iter()
            .filter_map(|s| SttProviderKind::from_label(s))
            .collect()
    };
    raw.into_iter()
        .filter(|k| *k != primary && provider_is_configured(*k, cfg))
        .collect()
}

/// Returns true when `kind` has the config it needs to attempt a
/// transcription — no point routing to a provider whose API key is
/// missing.
fn provider_is_configured(kind: SttProviderKind, cfg: &VoiceConfig) -> bool {
    match kind {
        SttProviderKind::Voicebox => cfg.voicebox_stt_enabled,
        SttProviderKind::OpenAiCompatible => {
            cfg.stt_base_url.is_some() && cfg.stt_model.is_some() && cfg.stt_api_key.is_some()
        }
        SttProviderKind::Groq => cfg
            .stt_provider
            .as_ref()
            .and_then(|p| p.api_key.as_ref())
            .is_some(),
        // Compiled in AND switched on: `providers.stt.local.enabled = false`
        // used to leave Local "configured" and receiving dispatch attempts
        // (#1399). `stt_mode` is Local exactly when that flag is set.
        SttProviderKind::Local => cfg!(feature = "local-stt") && cfg.stt_mode == SttMode::Local,
    }
}

async fn try_stt(kind: SttProviderKind, audio_bytes: Vec<u8>, cfg: &VoiceConfig) -> Result<String> {
    match kind {
        SttProviderKind::Voicebox => {
            super::voicebox_stt::transcribe(audio_bytes, &cfg.voicebox_stt_base_url).await
        }
        SttProviderKind::OpenAiCompatible => {
            let base = cfg
                .stt_base_url
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("openai-compatible STT missing base_url"))?;
            let model = cfg
                .stt_model
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("openai-compatible STT missing model"))?;
            let key = cfg
                .stt_api_key
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("openai-compatible STT missing api_key"))?;
            // The built-in endpoint serves transcription on the chat route and
            // answers no /v1/audio/* path at all; every other [OI]-compatible
            // endpoint keeps the multipart request it has always used (#2107).
            if super::openai_stt::serves_audio_on_chat_route(base) {
                super::openai_stt::transcribe_audio_chat(audio_bytes, key, model, base).await
            } else {
                super::openai_stt::transcribe_audio(audio_bytes, key, model, base).await
            }
        }
        SttProviderKind::Groq => {
            let key = cfg
                .stt_provider
                .as_ref()
                .and_then(|p| p.api_key.as_ref())
                .ok_or_else(|| anyhow::anyhow!("Groq STT missing api_key"))?;
            transcribe_audio(audio_bytes, key).await
        }
        SttProviderKind::Local => {
            #[cfg(feature = "local-stt")]
            {
                if !super::local_stt_available() {
                    anyhow::bail!("Local STT is not supported on this CPU — AVX2 is required.");
                }
                let model_id = &cfg.local_stt_model;
                let preset = super::local_whisper::find_local_model(model_id)
                    .ok_or_else(|| anyhow::anyhow!("Unknown local STT model: {}", model_id))?;
                if !super::local_whisper::is_model_downloaded(preset) {
                    tracing::info!(
                        "Local STT model '{}' not in candle format — downloading automatically",
                        model_id
                    );
                    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                    let preset_id = model_id.to_string();
                    let download_preset = super::local_whisper::find_local_model(&preset_id)
                        .ok_or_else(|| anyhow::anyhow!("Unknown model: {}", preset_id))?;
                    let download_handle = tokio::spawn(async move {
                        super::local_whisper::download_model(download_preset, tx).await
                    });
                    while let Some(progress) = rx.recv().await {
                        if progress.done {
                            break;
                        }
                        if let Some(err) = progress.error {
                            anyhow::bail!("Model download failed: {}", err);
                        }
                    }
                    download_handle
                        .await
                        .map_err(|e| anyhow::anyhow!("Download task failed: {}", e))??;
                    tracing::info!("Local STT model '{}' downloaded successfully", model_id);
                }
                tracing::info!("Local STT: transcribing with model {}", model_id);
                transcribe_audio_local(audio_bytes, model_id.clone()).await
            }
            #[cfg(not(feature = "local-stt"))]
            {
                let _ = audio_bytes;
                anyhow::bail!("Local STT feature not compiled in")
            }
        }
    }
}

/// Tag for the STT providers the dispatcher knows about. Kept `pub(crate)`
/// so the fallback-chain test module can construct expectations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SttProviderKind {
    Voicebox,
    OpenAiCompatible,
    Groq,
    Local,
}

impl SttProviderKind {
    pub(crate) fn label(self) -> String {
        match self {
            SttProviderKind::Voicebox => "voicebox".to_string(),
            SttProviderKind::OpenAiCompatible => "openai_compatible".to_string(),
            SttProviderKind::Groq => "groq".to_string(),
            SttProviderKind::Local => "local".to_string(),
        }
    }

    pub(crate) fn from_label(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "voicebox" => Some(SttProviderKind::Voicebox),
            "openai_compatible" | "openai-compatible" | "openai" => {
                Some(SttProviderKind::OpenAiCompatible)
            }
            "groq" => Some(SttProviderKind::Groq),
            "local" | "whisper" | "local_whisper" => Some(SttProviderKind::Local),
            _ => None,
        }
    }
}

/// Internal: transcribe with configurable URL (for testing).
pub(crate) async fn transcribe_audio_with_url(
    audio_bytes: Vec<u8>,
    api_key: &str,
    url: &str,
) -> Result<String> {
    let client = Client::new();

    let file_part = reqwest::multipart::Part::bytes(audio_bytes)
        .file_name("voice.ogg")
        .mime_str("audio/ogg")?;

    let form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("model", "whisper-large-v3-turbo")
        .text("response_format", "json");

    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await
        .context("Failed to send audio to Groq Whisper")?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response.text().await.unwrap_or_default();
        anyhow::bail!("Groq STT error ({}): {}", status, error_text);
    }

    let result: TranscriptionResponse = response
        .json()
        .await
        .context("Failed to parse Groq transcription response")?;

    tracing::info!("Groq STT: transcribed {} chars", result.text.len());

    Ok(result.text)
}

/// Maximum bytes per TTS chunk. Empirically, Voicebox (Kokoro) handles up to
/// ~3000 characters in ~90s. The budget is in bytes because that is the unit
/// both the engines and `str` slicing take, so 1500 stays well under the
/// timeout while avoiding quality degradation from very long synthesis.
const MAX_TTS_CHUNK_BYTES: usize = 1500;

/// Dispatch TTS synthesis based on config.
///
/// - Voicebox TTS (voicebox_tts_enabled)
/// - OpenAI-compatible TTS (tts_base_url + tts_model)
/// - OpenAI TTS API (tts_provider)
/// - Local Piper TTS (local-tts feature)
///
/// Long text is cleaned of markdown, chunked at sentence boundaries, and
/// synthesized in pieces that are concatenated into a single audio stream.
/// All audio is converted to OGG/Opus before returning.
pub async fn synthesize(text: &str, voice_config: &VoiceConfig) -> Result<Vec<u8>> {
    if text.is_empty() {
        anyhow::bail!("Cannot synthesize empty text");
    }

    // Clean markdown from text for ALL providers (not just Piper).
    // This prevents TTS engines from reading out loud code fences,
    // asterisks, headers, URLs, etc.
    let cleaned = super::text_cleanup::clean_for_tts(text);
    if cleaned.is_empty() {
        anyhow::bail!("No speakable text after cleaning markdown");
    }

    let chunks = split_for_tts(&cleaned, MAX_TTS_CHUNK_BYTES);

    if chunks.len() > 1 {
        tracing::info!(
            "TTS: text split into {} chunks ({} bytes total, max {}/chunk)",
            chunks.len(),
            cleaned.len(),
            MAX_TTS_CHUNK_BYTES,
        );
    }

    let primary = resolve_primary_tts(voice_config);
    let chain = resolve_tts_fallback_chain(voice_config, primary);
    let candidates: Vec<TtsProviderKind> = std::iter::once(primary).chain(chain).collect();

    let mut attempts: Vec<String> = Vec::with_capacity(candidates.len());
    let mut audio: Option<Vec<u8>> = None;
    for kind in &candidates {
        attempts.push(kind.label());
        match try_tts_chunked(*kind, &chunks, voice_config).await {
            Ok(bytes) => {
                if attempts.len() > 1 {
                    tracing::info!(
                        "TTS recovered via fallback chain: {} (attempts: {})",
                        kind.label(),
                        attempts.join(" → "),
                    );
                }
                audio = Some(bytes);
                break;
            }
            Err(e) => {
                tracing::warn!(
                    "TTS provider '{}' failed: {} — trying next in chain",
                    kind.label(),
                    e,
                );
                if let Some(s) = attempts.last_mut() {
                    s.push_str(&format!(": {e}"));
                }
            }
        }
    }

    let audio = audio.ok_or_else(|| {
        anyhow::anyhow!(
            "All TTS providers failed. Attempts:\n  - {}",
            attempts.join("\n  - "),
        )
    })?;

    // Ensure Opus format for Telegram — converts WAV/MP3 if needed
    Ok(ensure_opus(audio).await)
}

/// Synthesize each chunk with the given provider and concatenate audio.
///
/// For single-chunk text (the common case), this is a direct passthrough.
/// For multi-chunk, each piece is synthesized independently and the resulting
/// audio bytes are concatenated. OGG/Opus concatenation works because
/// decoders handle back-to-back OGG streams.
async fn try_tts_chunked(
    kind: TtsProviderKind,
    chunks: &[String],
    cfg: &VoiceConfig,
) -> Result<Vec<u8>> {
    if chunks.len() == 1 {
        return try_tts(kind, &chunks[0], cfg).await;
    }

    let mut all_audio = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        tracing::info!(
            "TTS chunk {}/{} ({} chars) via {}",
            i + 1,
            chunks.len(),
            chunk.len(),
            kind.label(),
        );
        let audio = try_tts(kind, chunk, cfg).await?;
        all_audio.extend_from_slice(&audio);
    }

    tracing::info!(
        "TTS: concatenated {} chunks into {} bytes of audio",
        chunks.len(),
        all_audio.len(),
    );
    Ok(all_audio)
}

/// Split text into chunks at sentence boundaries.
///
/// Tries to break at `. `, `! `, `? `, or `\n` to keep sentences intact.
/// Each chunk is at most `max_bytes` bytes, rounded down to a char boundary.
/// Falls back to a hard split at word boundaries if no sentence break is found.
///
/// The budget is BYTES, and the parameter says so: both the TTS engines and
/// `str` slicing work in bytes, so a run of 600 emoji is 2400 bytes and splits
/// into more chunks than 600 ASCII letters would.
pub(crate) fn split_for_tts(text: &str, max_bytes: usize) -> Vec<String> {
    if text.len() <= max_bytes {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_bytes {
            chunks.push(remaining.to_string());
            break;
        }

        // The search window has to END on a char boundary. A raw
        // `&remaining[..max_bytes]` panics the moment the cut falls inside a
        // multibyte char, and the panic loses the whole TTS output: one
        // accented letter, an emoji or a `->` past byte 1500 was enough
        // (#2051, the same class #1919 fixed in the cron scheduler). Round the
        // budget down with the shared helper instead of cutting blind.
        let search_area = truncate_str(remaining, max_bytes);
        let window_end = search_area.len();
        let mut split_pos = find_sentence_break(search_area)
            .or_else(|| find_word_break(search_area))
            .unwrap_or(window_end);

        // Progress guard. When the first char is wider than the budget the
        // rounded-down window is empty, so every break finder returns 0 and the
        // loop would push an empty chunk without ever shrinking `remaining`: a
        // hang, which is worse than the panic it replaces. Emit exactly one
        // char: the end of the first char is always a valid boundary.
        if split_pos == 0 {
            split_pos = remaining
                .char_indices()
                .nth(1)
                .map(|(i, _)| i)
                .unwrap_or(remaining.len());
        }

        chunks.push(remaining[..split_pos].trim().to_string());
        remaining = remaining[split_pos..].trim_start();
    }

    chunks
}

/// Find the last sentence-ending punctuation within text.
pub(crate) fn find_sentence_break(text: &str) -> Option<usize> {
    let candidates = [". ", "! ", "? ", ".\n", "!\n", "?\n", "\n\n", "\n"];
    let mut best = None;
    for delim in &candidates {
        if let Some(pos) = text.rfind(delim) {
            let end = pos + delim.len();
            if best.is_none_or(|(b, _): (usize, &str)| end > b) {
                best = Some((end, *delim));
            }
        }
    }
    best.map(|(pos, _)| pos)
}

/// Find the last word boundary within text.
///
/// The index comes from the matched char's own width, not `pos + 1`: whitespace
/// is not always one byte. U+00A0 (the non-breaking space any paste from a web
/// page carries) is two bytes and U+3000 is three, so `pos + 1` landed inside
/// the char and the caller's slice panicked (#2051).
fn find_word_break(text: &str) -> Option<usize> {
    text.char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map(|(pos, c)| pos + c.len_utf8())
}

/// Resolve which TTS provider runs first based on current config flags.
/// TTS twin of [`resolve_primary_stt`]: a non-empty user chain names the
/// primary; the default priority applies only when it does not (#1399).
pub(crate) fn resolve_primary_tts(cfg: &VoiceConfig) -> TtsProviderKind {
    if let Some(head) = cfg
        .tts_fallback_chain
        .iter()
        .filter_map(|s| TtsProviderKind::from_label(s))
        .find(|k| tts_provider_is_configured(*k, cfg))
    {
        return head;
    }
    if cfg.voicebox_tts_enabled {
        TtsProviderKind::Voicebox
    } else if cfg.tts_base_url.is_some() && cfg.tts_api_key.is_some() {
        TtsProviderKind::OpenAiCompatible
    } else if cfg
        .tts_provider
        .as_ref()
        .and_then(|p| p.api_key.as_ref())
        .is_some()
    {
        TtsProviderKind::OpenAi
    } else {
        TtsProviderKind::Local
    }
}

/// Build the ordered list of TTS fallback providers after `primary`.
/// Mirrors the STT helper's contract — user-configured chain wins,
/// otherwise default priority with primary removed; unconfigured
/// providers are filtered out so the dispatcher doesn't waste a turn
/// on a provider that will fail at the auth check.
pub(crate) fn resolve_tts_fallback_chain(
    cfg: &VoiceConfig,
    primary: TtsProviderKind,
) -> Vec<TtsProviderKind> {
    let user_chain = &cfg.tts_fallback_chain;
    let raw: Vec<TtsProviderKind> = if user_chain.is_empty() {
        vec![
            TtsProviderKind::Voicebox,
            TtsProviderKind::OpenAiCompatible,
            TtsProviderKind::OpenAi,
            TtsProviderKind::Local,
        ]
    } else {
        user_chain
            .iter()
            .filter_map(|s| TtsProviderKind::from_label(s))
            .collect()
    };
    raw.into_iter()
        .filter(|k| *k != primary && tts_provider_is_configured(*k, cfg))
        .collect()
}

fn tts_provider_is_configured(kind: TtsProviderKind, cfg: &VoiceConfig) -> bool {
    match kind {
        TtsProviderKind::Voicebox => cfg.voicebox_tts_enabled,
        TtsProviderKind::OpenAiCompatible => {
            cfg.tts_base_url.is_some() && cfg.tts_api_key.is_some()
        }
        TtsProviderKind::OpenAi => cfg
            .tts_provider
            .as_ref()
            .and_then(|p| p.api_key.as_ref())
            .is_some(),
        // Compiled in AND switched on (#1399); `tts_mode` is Local exactly
        // when `providers.tts.local.enabled` is set.
        TtsProviderKind::Local => cfg!(feature = "local-tts") && cfg.tts_mode == TtsMode::Local,
    }
}

async fn try_tts(kind: TtsProviderKind, text: &str, cfg: &VoiceConfig) -> Result<Vec<u8>> {
    match kind {
        TtsProviderKind::Voicebox => {
            tracing::info!(
                "TTS dispatch → Voicebox (base_url={}, profile_id={}, engine={})",
                cfg.voicebox_tts_base_url,
                cfg.voicebox_tts_profile_id,
                cfg.voicebox_tts_engine
            );
            let client = super::voicebox_tts::VoiceboxTts::new(
                &cfg.voicebox_tts_base_url,
                &cfg.voicebox_tts_profile_id,
                &cfg.voicebox_tts_engine,
            );
            client.synthesize(text).await
        }
        TtsProviderKind::OpenAiCompatible => {
            let base_url = cfg
                .tts_base_url
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("openai-compatible TTS missing base_url"))?;
            let api_key = cfg
                .tts_api_key
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("openai-compatible TTS missing api_key"))?;
            tracing::info!(
                "TTS dispatch → OpenAI-compatible (base_url={}, model={}, voice={})",
                base_url,
                cfg.tts_model,
                cfg.tts_voice
            );
            super::openai_tts::synthesize_speech(
                text,
                api_key,
                &cfg.tts_voice,
                &cfg.tts_model,
                base_url,
            )
            .await
        }
        TtsProviderKind::OpenAi => {
            let api_key = cfg
                .tts_provider
                .as_ref()
                .and_then(|p| p.api_key.as_ref())
                .ok_or_else(|| anyhow::anyhow!("OpenAI TTS missing api_key"))?;
            tracing::info!(
                "TTS dispatch → OpenAI (model={}, voice={})",
                cfg.tts_model,
                cfg.tts_voice
            );
            synthesize_speech(text, api_key, &cfg.tts_voice, &cfg.tts_model).await
        }
        TtsProviderKind::Local => {
            #[cfg(feature = "local-tts")]
            {
                let voice_id = cfg.local_tts_voice.clone();
                tracing::info!("TTS dispatch → Local Piper (voice={})", voice_id);
                synthesize_speech_local(text, &voice_id).await
            }
            #[cfg(not(feature = "local-tts"))]
            {
                let _ = text;
                let _ = cfg;
                anyhow::bail!("Local TTS feature not compiled in")
            }
        }
    }
}

/// Tag for the TTS providers the dispatcher knows about. `pub(crate)` so
/// the fallback-chain test module can exercise the resolution helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TtsProviderKind {
    Voicebox,
    OpenAiCompatible,
    OpenAi,
    Local,
}

impl TtsProviderKind {
    pub(crate) fn label(self) -> String {
        match self {
            TtsProviderKind::Voicebox => "voicebox".to_string(),
            TtsProviderKind::OpenAiCompatible => "openai_compatible".to_string(),
            TtsProviderKind::OpenAi => "openai".to_string(),
            TtsProviderKind::Local => "local".to_string(),
        }
    }

    pub(crate) fn from_label(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "voicebox" => Some(TtsProviderKind::Voicebox),
            "openai_compatible" | "openai-compatible" => Some(TtsProviderKind::OpenAiCompatible),
            "openai" => Some(TtsProviderKind::OpenAi),
            "local" | "piper" | "local_piper" => Some(TtsProviderKind::Local),
            _ => None,
        }
    }
}

/// Detect if audio bytes are already in OGG/Opus format.
fn is_opus(audio: &[u8]) -> bool {
    audio.starts_with(b"OggS")
}

/// Convert audio bytes to OGG/Opus format if needed.
///
/// Checks magic bytes to detect format. If already OGG/Opus, returns as-is.
/// Otherwise uses ffmpeg to convert with a 5-minute timeout.
/// Two-stage fallback: voip-tuned → basic opus → original bytes.
async fn ensure_opus(audio_bytes: Vec<u8>) -> Vec<u8> {
    if is_opus(&audio_bytes) {
        return audio_bytes;
    }

    let audio_len = audio_bytes.len();
    let audio_fallback = audio_bytes.clone();
    let handle = tokio::task::spawn_blocking(move || {
        let ffmpeg_path = std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_string());

        // Stage 1: voip-tuned opus (optimized for voice notes)
        let mut cmd = std::process::Command::new(&ffmpeg_path);
        cmd.args([
            "-i",
            "pipe:0",
            "-f",
            "ogg",
            "-c:a",
            "libopus",
            "-b:a",
            "48k",
            "-application",
            "voip",
            "-y",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

        if let Ok(mut child) = cmd.spawn() {
            use std::io::Write;
            let write_ok = child
                .stdin
                .take()
                .map(|mut stdin| stdin.write_all(&audio_bytes).is_ok())
                .unwrap_or(false);

            if write_ok {
                if let Ok(output) = child.wait_with_output() {
                    if output.status.success() && !output.stdout.is_empty() {
                        tracing::info!(
                            "TTS: voip opus conversion ok ({} → {} bytes)",
                            audio_bytes.len(),
                            output.stdout.len()
                        );
                        return output.stdout;
                    }
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!(
                        "TTS: voip opus failed ({}): {}",
                        output.status,
                        stderr.lines().next().unwrap_or("")
                    );
                }
            } else {
                tracing::warn!("TTS: failed to write audio to ffmpeg stage 1");
            }
        }

        // Stage 2: basic opus fallback (no voip tuning, higher bitrate)
        tracing::info!("TTS: trying basic opus fallback");
        let mut cmd2 = std::process::Command::new(&ffmpeg_path);
        cmd2.args([
            "-i", "pipe:0", "-f", "ogg", "-c:a", "libopus", "-b:a", "64k", "-y", "pipe:1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

        if let Ok(mut child) = cmd2.spawn() {
            use std::io::Write;
            let write_ok = child
                .stdin
                .take()
                .map(|mut stdin| stdin.write_all(&audio_bytes).is_ok())
                .unwrap_or(false);

            if write_ok {
                if let Ok(output) = child.wait_with_output() {
                    if output.status.success() && !output.stdout.is_empty() {
                        tracing::info!(
                            "TTS: basic opus fallback ok ({} → {} bytes)",
                            audio_bytes.len(),
                            output.stdout.len()
                        );
                        return output.stdout;
                    }
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::warn!(
                        "TTS: basic opus fallback failed ({}): {}",
                        output.status,
                        stderr.lines().next().unwrap_or("")
                    );
                }
            } else {
                tracing::warn!("TTS: failed to write audio to ffmpeg stage 2");
            }
        } else {
            tracing::warn!("TTS: ffmpeg not found at '{}'", ffmpeg_path);
        }

        // Both stages failed — return original
        audio_bytes
    });

    match tokio::time::timeout(std::time::Duration::from_secs(300), handle).await {
        Ok(result) => result.unwrap_or(audio_fallback),
        Err(_) => {
            tracing::warn!(
                "TTS: ffmpeg conversion timed out after 5m ({} bytes), sending original audio",
                audio_len
            );
            audio_fallback
        }
    }
}

/// Synthesize speech using local Piper TTS. Runs in a blocking thread.
#[cfg(feature = "local-tts")]
async fn synthesize_speech_local(text: &str, voice_id: &str) -> Result<Vec<u8>> {
    let text = text.to_string();
    let voice_id = voice_id.to_string();

    let handle = tokio::task::spawn_blocking(move || {
        let engine = super::local_tts::PiperTts::new(&voice_id)?;
        engine.synthesize_opus(&text)
    });

    match tokio::time::timeout(std::time::Duration::from_secs(120), handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => anyhow::bail!("Local TTS task panicked: {}", e),
        Err(_) => anyhow::bail!("Local TTS timed out after 120s"),
    }
}

/// Synthesize speech from text using OpenAI TTS.
///
/// Returns OGG/Opus audio bytes suitable for Telegram voice notes.
pub async fn synthesize_speech(
    text: &str,
    openai_api_key: &str,
    voice: &str,
    model: &str,
) -> Result<Vec<u8>> {
    synthesize_speech_with_url(text, openai_api_key, voice, model, OPENAI_SPEECH_URL).await
}

/// Internal: synthesize with configurable URL (for testing).
pub(crate) async fn synthesize_speech_with_url(
    text: &str,
    api_key: &str,
    voice: &str,
    model: &str,
    url: &str,
) -> Result<Vec<u8>> {
    let client = Client::new();

    let body = serde_json::json!({
        "model": model,
        "input": text,
        "voice": voice,
        "response_format": "opus",
    });

    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send TTS request to OpenAI")?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response.text().await.unwrap_or_default();
        anyhow::bail!("OpenAI TTS error ({}): {}", status, error_text);
    }

    let audio_bytes = response
        .bytes()
        .await
        .context("Failed to read TTS audio bytes")?
        .to_vec();

    tracing::info!(
        "OpenAI TTS: generated {} bytes of audio (voice={}, model={})",
        audio_bytes.len(),
        voice,
        model,
    );

    Ok(audio_bytes)
}

#[derive(Deserialize)]
pub(crate) struct TranscriptionResponse {
    pub(crate) text: String,
}

/// Cached local whisper model — loaded once, reused across transcriptions.
#[cfg(feature = "local-stt")]
static CACHED_WHISPER: tokio::sync::OnceCell<super::local_whisper::LocalWhisper> =
    tokio::sync::OnceCell::const_new();

/// Preload the local whisper model into the cache in the background.
/// Called at startup when local STT is configured so the first voice message is fast.
#[cfg(feature = "local-stt")]
pub async fn preload_local_whisper(model_id: &str) -> Result<()> {
    CACHED_WHISPER
        .get_or_try_init(|| async {
            let preset = super::local_whisper::find_local_model(model_id)
                .ok_or_else(|| anyhow::anyhow!("Unknown local STT model: {}", model_id))?;
            super::local_whisper::LocalWhisper::new(preset).await
        })
        .await?;
    Ok(())
}

/// Transcribe audio bytes using a local whisper model (rwhisper).
///
/// The model is loaded once and cached for subsequent calls.
#[cfg(feature = "local-stt")]
pub async fn transcribe_audio_local(audio_bytes: Vec<u8>, model_id: String) -> Result<String> {
    let len = audio_bytes.len();
    tracing::info!(
        "Local STT: starting transcription ({} bytes audio, model {})",
        len,
        model_id
    );

    let whisper = CACHED_WHISPER
        .get_or_try_init(|| async {
            let preset = super::local_whisper::find_local_model(&model_id)
                .ok_or_else(|| anyhow::anyhow!("Unknown local STT model: {}", model_id))?;
            super::local_whisper::LocalWhisper::new(preset).await
        })
        .await?;

    // Generous timeout — CPU transcription of long audio can take minutes
    match tokio::time::timeout(
        std::time::Duration::from_secs(300),
        whisper.transcribe(&audio_bytes),
    )
    .await
    {
        Ok(Ok(text)) => {
            tracing::info!("Local STT: transcription complete");
            Ok(text)
        }
        Ok(Err(e)) => {
            tracing::error!("Local STT: transcription failed: {}", e);
            Err(e)
        }
        Err(_) => {
            tracing::error!("Local STT: transcription timed out after 300s");
            anyhow::bail!("Local STT transcription timed out (300s)")
        }
    }
}
