//! Voice Processing Module
//!
//! Speech-to-text and text-to-speech services.
//! Supports:
//! - API-based STT (Groq Whisper, OpenAI-compatible, Voicebox)
//! - Local STT (whisper.cpp / rwhisper)
//! - API-based TTS (OpenAI, OpenAI-compatible, Voicebox)
//! - Local TTS (Piper)
//!
//! This file is declarations only — no function definitions live here
//! (CONTRIBUTING.md); [`availability`] holds the local-backend probes.

mod availability;
pub mod openai_stt;
pub mod openai_tts;
pub mod voicebox_stt;
pub mod voicebox_tts;

#[cfg(feature = "local-stt")]
pub mod local_whisper;

#[cfg(feature = "local-tts")]
pub mod local_tts;

// The OGG/Opus encoder is shared: the channel replies encode with it, and the
// window records with it. Gated on either feature so a GUI build without Piper
// still gets a playable voice note.
#[cfg(any(feature = "local-tts", feature = "gui"))]
pub mod opus;

pub mod text_cleanup;

// The spoken summary a voice turn answers with, kept out of the `local-tts`
// gate because every provider reads it.
pub mod spoken;

pub(crate) mod service;

pub use service::transcribe_with_language;
pub use service::{synthesize, synthesize_speech, transcribe, transcribe_audio};

#[cfg(feature = "local-stt")]
pub use service::{preload_local_whisper, transcribe_audio_local};

pub use availability::{local_stt_available, local_tts_available};
