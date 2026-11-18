//! Tests for voice/local_tts.rs: sample rate extraction, Piper voices,
//! PCM/WAV/OGG encoding.

use crate::channels::voice::local_tts::{
    PIPER_VOICES, extract_sample_rate, find_piper_voice, pcm_to_wav,
};
use crate::channels::voice::opus::{encode_ogg_opus, ogg_crc32};

// ── extract_sample_rate ───────────────────────────────────────────────

#[test]
fn extract_sample_rate_present() {
    let config = r#"{"sample_rate": 22050, "other": "stuff"}"#;
    assert_eq!(extract_sample_rate(config), Some(22050));
}

#[test]
fn extract_sample_rate_missing() {
    let config = r#"{"other": "stuff"}"#;
    assert_eq!(extract_sample_rate(config), None);
}

// ── find_piper_voice ──────────────────────────────────────────────────

#[test]
fn find_piper_voice_known() {
    assert!(find_piper_voice("ryan").is_some());
    assert!(find_piper_voice("amy").is_some());
}

#[test]
fn find_piper_voice_unknown() {
    assert!(find_piper_voice("nonexistent").is_none());
}

#[test]
fn piper_voice_urls() {
    let ryan = find_piper_voice("ryan").unwrap();
    assert!(ryan.onnx_url().contains("en_US"));
    assert!(ryan.onnx_url().contains("ryan"));
    assert!(ryan.onnx_url().ends_with(".onnx"));
    assert!(ryan.config_url().ends_with(".onnx.json"));
}

// ── default voice ─────────────────────────────────────────────────────

#[test]
fn default_voice_is_ryan() {
    assert_eq!(PIPER_VOICES[0].id, "ryan");
}

// ── pcm_to_wav ────────────────────────────────────────────────────────

#[test]
fn pcm_to_wav_header() {
    let samples = vec![0i16, 100, -100, 32767, -32768];
    let wav = pcm_to_wav(&samples, 22050).unwrap();
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
}

// ── encode_ogg_opus ──────────────────────────────────────────────────

#[test]
fn encode_ogg_opus_produces_ogg() {
    let samples = vec![0i16; 960];
    let ogg = encode_ogg_opus(&samples, 48000).unwrap();
    assert_eq!(&ogg[..4], b"OggS", "Should produce OGG container");
}

// ── ogg_crc32 ────────────────────────────────────────────────────────

#[test]
fn ogg_crc32_empty() {
    assert_eq!(ogg_crc32(&[]), 0);
}
