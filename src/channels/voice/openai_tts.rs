//! OpenAI-compatible TTS provider.
//!
//! Supports any OpenAI-compatible `/v1/audio/speech` endpoint, including:
//! - Official OpenAI API
//! - Ollama (`http://localhost:11434/v1`)
//! - LocalAI, OMLX, Unsloth, etc.

use anyhow::{Context, Result};
use url::Url;

/// Synthesize speech using an OpenAI-compatible TTS endpoint.
///
/// # Arguments
/// * `text` - Text to synthesize
/// * `api_key` - API key for the endpoint
/// * `voice` - Voice name (e.g. "ash", "alloy")
/// * `model` - Model name (e.g. "gpt-4o-mini-tts")
/// * `base_url` - Base URL of the endpoint (e.g. "https://api.openai.com" or "http://localhost:11434")
///
/// # Returns
/// OGG/Opus audio bytes suitable for Telegram voice notes.
pub async fn synthesize_speech(
    text: &str,
    api_key: &str,
    voice: &str,
    model: &str,
    base_url: &str,
) -> Result<Vec<u8>> {
    if text.is_empty() {
        anyhow::bail!("Cannot synthesize empty text");
    }

    let speech_url = build_endpoint_url(base_url, "v1/audio/speech")?;

    let client = crate::utils::http::shared();

    let body = serde_json::json!({
        "model": model,
        "input": text,
        "voice": voice,
        "response_format": "opus",
    });

    let response = client
        .post(&speech_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        // The proxy tells an OpenCrabs caller from any other client by this
        // header (#3138); the voice route is another way in.
        .header("User-Agent", crate::brain::provider::identity::user_agent())
        .json(&body)
        .send()
        .await
        .context("Failed to send TTS request")?;

    let status = response.status();
    if !status.is_success() {
        let error_text = response.text().await.unwrap_or_default();
        anyhow::bail!("TTS error ({}): {}", status, error_text);
    }

    let audio_bytes = response
        .bytes()
        .await
        .context("Failed to read TTS audio bytes")?
        .to_vec();

    tracing::info!(
        "OpenAI-compatible TTS: generated {} bytes of audio (voice={}, model={}, url={})",
        audio_bytes.len(),
        voice,
        model,
        speech_url,
    );

    Ok(audio_bytes)
}

/// Build a properly joined endpoint URL from base_url + path.
///
/// Handles missing trailing slashes, `v1/` prefix variations, etc.
pub fn build_endpoint_url(base_url: &str, path: &str) -> Result<String> {
    let mut url = Url::parse(base_url).context("Invalid base URL")?;
    // Ensure trailing slash so `join` replaces the last segment
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    let joined = url.join(path).context("Failed to join URL path")?;
    Ok(joined.to_string())
}
