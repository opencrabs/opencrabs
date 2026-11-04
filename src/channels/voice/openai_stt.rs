//! OpenAI-compatible STT provider.
//!
//! Supports any OpenAI-compatible `/v1/audio/transcriptions` endpoint, including:
//! - Official OpenAI API
//! - Groq (`https://api.groq.com/openai/v1`)
//! - Ollama (`http://localhost:11434/v1`)
//! - LocalAI, OMLX, etc.

use anyhow::{Context, Result};
use base64::Engine as _;
use reqwest::Client;
use serde::Deserialize;

use super::openai_tts::build_endpoint_url;

/// Transcribe audio bytes using an OpenAI-compatible STT endpoint.
///
/// # Arguments
/// * `audio_bytes` - OGG/Opus audio data
/// * `api_key` - API key for the endpoint
/// * `model` - Model name (e.g. "whisper-large-v3-turbo")
/// * `base_url` - Base URL of the endpoint
///
/// # Returns
/// Transcribed text.
pub async fn transcribe_audio(
    audio_bytes: Vec<u8>,
    api_key: &str,
    model: &str,
    base_url: &str,
) -> Result<String> {
    let transcribe_url = build_endpoint_url(base_url, "v1/audio/transcriptions")?;

    let client = Client::new();

    let model_owned = model.to_string();

    let file_part = reqwest::multipart::Part::bytes(audio_bytes)
        .file_name("voice.ogg")
        .mime_str("audio/ogg")?;

    let form = reqwest::multipart::Form::new()
        .part("file", file_part)
        .text("model", model_owned)
        .text("response_format", "json");

    let response = client
        .post(&transcribe_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .multipart(form)
        .send()
        .await
        .context("Failed to send audio to STT endpoint")?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response.text().await.unwrap_or_default();
        anyhow::bail!("STT error ({}): {}", status, error_text);
    }

    let result: TranscriptionResponse = response
        .json()
        .await
        .context("Failed to parse transcription response")?;

    tracing::info!(
        "OpenAI-compatible STT: transcribed {} chars (model={}, url={})",
        result.text.len(),
        model,
        transcribe_url,
    );

    Ok(result.text)
}

/// True when `base_url` serves transcription on the chat route instead of
/// `/v1/audio/transcriptions`.
///
/// The built-in inference endpoint answers no audio route at all: it takes the
/// audio as an `input_audio` part on `v1/chat/completions` and returns the
/// transcript in the assistant message. Every other [OI]-compatible endpoint
/// keeps the multipart transcription request it has always used.
///
/// Pure on purpose, so the routing decision is testable without a network
/// call. The host check is shared with the install client, so a custom
/// provider section that copied the install token reaches the same endpoint
/// under a different name and still gets the chat wire.
pub fn serves_audio_on_chat_route(base_url: &str) -> bool {
    crate::brain::provider::opencrabs_install::is_opencrabs_endpoint(Some(base_url))
}

/// Transcribe audio by sending it as an `input_audio` part on the chat route.
///
/// Used for endpoints where [`serves_audio_on_chat_route`] is true. `content`
/// carries the answer whenever there is one; `reasoning_content` is the model's
/// thinking trace and only repeats the answer when `content` is empty, so it is
/// read second and never first.
pub async fn transcribe_audio_chat(
    audio_bytes: Vec<u8>,
    api_key: &str,
    model: &str,
    base_url: &str,
) -> Result<String> {
    let chat_url = build_endpoint_url(base_url, "v1/chat/completions")?;
    let audio_b64 = base64::engine::general_purpose::STANDARD.encode(&audio_bytes);

    let body = serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": "Transcribe this audio note verbatim. Reply with the transcription only."
                },
                {
                    "type": "input_audio",
                    "input_audio": { "data": audio_b64, "format": "ogg" }
                }
            ]
        }]
    });

    let response = Client::new()
        .post(&chat_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&body)
        .send()
        .await
        .context("Failed to send audio to the chat transcription route")?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response.text().await.unwrap_or_default();
        anyhow::bail!("STT chat-route error ({}): {}", status, error_text);
    }

    let payload: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse chat transcription response")?;

    let message = &payload["choices"][0]["message"];
    // `content` carries the answer whenever there is one. `reasoning_content` is the
    // thinking trace, and on some models it repeats the answer when `content` is empty,
    // so it is the fallback and never the first read.
    let text = ["content", "reasoning_content"]
        .iter()
        .filter_map(|field| message.get(*field).and_then(|value| value.as_str()))
        .map(str::trim)
        .find(|text| !text.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Chat transcription returned no text: content and reasoning_content were both empty"
            )
        })?;

    tracing::info!(
        "STT chat route: transcribed {} chars (model={}, url={})",
        text.len(),
        model,
        chat_url,
    );

    Ok(text.to_string())
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}
