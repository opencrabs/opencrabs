//! Local TTS via Piper (piper-tts)
//!
//! Text-to-speech using the Piper project (Python venv + ONNX voice models).
//! No external Rust crate — spawns `piper` subprocess with text on stdin,
//! reads raw 16-bit PCM from stdout, encodes to OGG/Opus for channel delivery.
//! Gated behind the `local-tts` feature flag.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

// The OGG/Opus encoder lives in its own module now: the window needs it with
// no Piper installed, and the container has nothing to do with the voice.
use crate::channels::voice::opus::encode_ogg_opus;

// ─── Voice presets ────────────────────────────────────────────────────────────

/// A Piper voice preset.
pub struct PiperVoice {
    pub id: &'static str,
    pub label: &'static str,
    pub locale: &'static str,
    pub name: &'static str,
    pub quality: &'static str,
}

impl PiperVoice {
    /// HuggingFace URL for the ONNX model file.
    pub fn onnx_url(&self) -> String {
        format!(
            "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/{}/{}/{}/{}-{}-{}.onnx",
            self.locale, self.name, self.quality, self.locale, self.name, self.quality
        )
    }
    /// HuggingFace URL for the ONNX config file.
    pub fn config_url(&self) -> String {
        format!("{}.json", self.onnx_url())
    }
}

/// Available Piper voice presets.
pub const PIPER_VOICES: &[PiperVoice] = &[
    PiperVoice {
        id: "ryan",
        label: "Ryan (US Male)",
        locale: "en_US",
        name: "ryan",
        quality: "medium",
    },
    PiperVoice {
        id: "amy",
        label: "Amy (US Female)",
        locale: "en_US",
        name: "amy",
        quality: "medium",
    },
    PiperVoice {
        id: "lessac",
        label: "Lessac (US Female)",
        locale: "en_US",
        name: "lessac",
        quality: "medium",
    },
    PiperVoice {
        id: "kristin",
        label: "Kristin (US Female)",
        locale: "en_US",
        name: "kristin",
        quality: "medium",
    },
    PiperVoice {
        id: "joe",
        label: "Joe (US Male)",
        locale: "en_US",
        name: "joe",
        quality: "medium",
    },
    PiperVoice {
        id: "cori",
        label: "Cori (UK Female)",
        locale: "en_GB",
        name: "cori",
        quality: "medium",
    },
];

/// Look up a Piper voice preset by ID.
pub fn find_piper_voice(id: &str) -> Option<&'static PiperVoice> {
    PIPER_VOICES.iter().find(|v| v.id == id)
}

// ─── Paths & checks ──────────────────────────────────────────────────────────

/// Directory where Piper venv and voice models are stored.
pub fn piper_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("opencrabs").join("models").join("piper");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Check if the Piper venv is installed.
pub fn piper_venv_exists() -> bool {
    piper_bin_path().exists()
}

/// Path to the piper binary inside the venv.
fn piper_bin_path() -> PathBuf {
    let dir = piper_dir();
    if cfg!(target_os = "windows") {
        dir.join("venv").join("Scripts").join("piper.exe")
    } else {
        dir.join("venv").join("bin").join("piper")
    }
}

/// Check if a specific voice model is downloaded.
pub fn piper_voice_exists(voice_id: &str) -> bool {
    let dir = piper_dir();
    dir.join(format!("{voice_id}.onnx")).exists()
        && dir.join(format!("{voice_id}.onnx.json")).exists()
}

/// Delete a downloaded Piper voice model (ONNX + config).
pub fn delete_voice(voice_id: &str) {
    let dir = piper_dir();
    let onnx = dir.join(format!("{voice_id}.onnx"));
    let config = dir.join(format!("{voice_id}.onnx.json"));
    let _ = std::fs::remove_file(&onnx);
    let _ = std::fs::remove_file(&config);
    tracing::info!("Deleted Piper voice '{voice_id}'");
}

/// Delete all downloaded Piper voices except the given one.
pub fn delete_other_voices(keep_id: &str) {
    for voice in PIPER_VOICES {
        if voice.id != keep_id && piper_voice_exists(voice.id) {
            delete_voice(voice.id);
        }
    }
}

// ─── Venv setup ───────────────────────────────────────────────────────────────

/// Set up the Piper Python venv and install piper-tts.
pub async fn setup_piper_venv(
    progress_tx: tokio::sync::mpsc::UnboundedSender<SetupProgress>,
) -> Result<()> {
    let dir = piper_dir();
    let venv_dir = dir.join("venv");

    if piper_venv_exists() {
        tracing::info!("Piper venv already exists, skipping setup");
        return Ok(());
    }

    let _ = progress_tx.send(SetupProgress {
        stage: "Creating Python venv...".to_string(),
        done: false,
        error: None,
    });

    // Create venv
    let python = if cfg!(target_os = "windows") {
        "python"
    } else {
        "python3"
    };

    let output = tokio::process::Command::new(python)
        .args(["-m", "venv"])
        .arg(&venv_dir)
        .output()
        .await
        .context("Failed to create Python venv — is python3 installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let hint = if stderr.contains("ensurepip") || stderr.contains("No module named") {
            if cfg!(target_os = "linux") {
                "\n\nFix: install the venv module with:\n  sudo apt install python3-venv   # Debian/Ubuntu\n  sudo dnf install python3        # Fedora"
            } else if cfg!(target_os = "macos") {
                "\n\nFix: reinstall Python with:\n  brew install python3"
            } else {
                "\n\nFix: install Python 3 with the venv module included."
            }
        } else {
            ""
        };
        anyhow::bail!("python3 -m venv failed: {stderr}{hint}");
    }

    let _ = progress_tx.send(SetupProgress {
        stage: "Installing piper-tts...".to_string(),
        done: false,
        error: None,
    });

    // Install piper-tts
    let pip = if cfg!(target_os = "windows") {
        venv_dir.join("Scripts").join("pip.exe")
    } else {
        venv_dir.join("bin").join("pip")
    };

    let output = tokio::process::Command::new(&pip)
        .args(["install", "piper-tts", "pathvalidate"])
        .output()
        .await
        .context("Failed to install piper-tts")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("pip install piper-tts failed: {stderr}");
    }

    let _ = progress_tx.send(SetupProgress {
        stage: "Piper TTS installed".to_string(),
        done: true,
        error: None,
    });

    tracing::info!("Piper venv setup complete at {}", venv_dir.display());
    Ok(())
}

/// Setup progress info.
#[derive(Debug, Clone)]
pub struct SetupProgress {
    pub stage: String,
    pub done: bool,
    pub error: Option<String>,
}

// ─── Voice model download ─────────────────────────────────────────────────────

/// Download progress info.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub done: bool,
    pub error: Option<String>,
}

/// Download a Piper voice model (ONNX + config) with progress reporting.
pub async fn download_voice(
    voice_id: &str,
    progress_tx: tokio::sync::mpsc::UnboundedSender<DownloadProgress>,
) -> Result<()> {
    let preset = find_piper_voice(voice_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown Piper voice: {voice_id}"))?;

    let dir = piper_dir();
    let onnx_dest = dir.join(format!("{voice_id}.onnx"));
    let config_dest = dir.join(format!("{voice_id}.onnx.json"));

    // Download ONNX model
    download_file(&preset.onnx_url(), &onnx_dest, &progress_tx).await?;

    // Download config
    download_file(&preset.config_url(), &config_dest, &progress_tx).await?;

    let _ = progress_tx.send(DownloadProgress {
        downloaded: 0,
        total: None,
        done: true,
        error: None,
    });

    tracing::info!("Piper voice '{}' downloaded", voice_id);
    Ok(())
}

/// Download a single file with progress reporting. Writes to `.part` then renames.
async fn download_file(
    url: &str,
    dest: &Path,
    progress_tx: &tokio::sync::mpsc::UnboundedSender<DownloadProgress>,
) -> Result<()> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let part = dest.with_extension("part");

    tracing::info!("Downloading {} to {}", url, dest.display());

    let response = reqwest::get(url)
        .await
        .context("Failed to start download")?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed: HTTP {}", response.status());
    }

    let total = response.content_length();
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(&part)
        .await
        .context("Failed to create temp file")?;
    let mut downloaded: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Download stream error")?;
        file.write_all(&chunk)
            .await
            .context("Failed to write chunk")?;
        downloaded += chunk.len() as u64;
        let _ = progress_tx.send(DownloadProgress {
            downloaded,
            total,
            done: false,
            error: None,
        });
    }
    file.flush().await?;
    drop(file);

    // Atomic rename
    tokio::fs::rename(&part, dest)
        .await
        .context("Failed to rename downloaded file")?;

    Ok(())
}

// ─── TTS engine ───────────────────────────────────────────────────────────────

/// Local Piper TTS engine.
pub struct PiperTts {
    piper_bin: PathBuf,
    model_path: PathBuf,
    sample_rate: u32,
}

impl PiperTts {
    /// Load Piper with a specific voice model.
    pub fn new(voice_id: &str) -> Result<Self> {
        let dir = piper_dir();
        let piper_bin = piper_bin_path();
        let model_path = dir.join(format!("{voice_id}.onnx"));
        let config_path = dir.join(format!("{voice_id}.onnx.json"));

        if !piper_bin.exists() {
            anyhow::bail!("Piper not installed. Run /voice to set up local TTS.");
        }
        if !model_path.exists() {
            anyhow::bail!(
                "Piper voice '{}' not downloaded. Run /voice to download.",
                voice_id
            );
        }

        let sample_rate = if config_path.exists() {
            let config_str =
                std::fs::read_to_string(&config_path).context("Failed to read voice config")?;
            extract_sample_rate(&config_str).unwrap_or(22050)
        } else {
            22050
        };

        tracing::info!(
            "Piper TTS ready: voice={}, sample_rate={}",
            voice_id,
            sample_rate
        );

        Ok(Self {
            piper_bin,
            model_path,
            sample_rate,
        })
    }

    /// Synthesize text to raw i16 PCM samples.
    pub fn synthesize_pcm(&self, text: &str) -> Result<Vec<i16>> {
        use std::io::Write;

        let cleaned = super::text_cleanup::clean_for_tts(text);
        if cleaned.is_empty() {
            anyhow::bail!("No speakable text after cleaning");
        }

        let mut child = std::process::Command::new(&self.piper_bin)
            .arg("--model")
            .arg(&self.model_path)
            .arg("--output_raw")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn piper")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(cleaned.as_bytes())
                .context("Failed to write text to piper")?;
        }

        let result = child
            .wait_with_output()
            .context("Failed to wait for piper")?;

        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            anyhow::bail!("Piper failed: {stderr}");
        }

        let raw = &result.stdout;
        if raw.len() % 2 != 0 {
            anyhow::bail!("Piper output has odd byte count");
        }

        // The odd-length case bailed above, so the remainder is empty.
        let (chunks, _remainder) = raw.as_chunks::<2>();
        let samples: Vec<i16> = chunks.iter().map(|c| i16::from_le_bytes(*c)).collect();

        tracing::info!(
            "Piper TTS: {} samples ({:.1}s audio)",
            samples.len(),
            samples.len() as f32 / self.sample_rate as f32,
        );

        Ok(samples)
    }

    /// Synthesize text to OGG/Opus bytes (for sending via Telegram/WhatsApp/etc).
    pub fn synthesize_opus(&self, text: &str) -> Result<Vec<u8>> {
        let samples = self.synthesize_pcm(text)?;
        pcm_to_opus(&samples, self.sample_rate)
    }

    /// Output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// Convert i16 PCM samples to OGG/Opus bytes (RFC 7845).
///
/// Piper outputs at the voice's native sample rate (typically 22050 Hz).
/// Opus requires 48000 Hz, so we resample first, then encode.
/// The result is a proper OGG/Opus stream that Telegram shows with waveform.
fn pcm_to_opus(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>> {
    // Resample to 48000 Hz (required by Opus)
    let samples_48k = if sample_rate == 48000 {
        samples.to_vec()
    } else {
        resample_i16(samples, sample_rate, 48000)?
    };

    encode_ogg_opus(&samples_48k, 48000)
}

/// Resample i16 PCM from one rate to another using rubato.
fn resample_i16(samples: &[i16], from_rate: u32, to_rate: u32) -> Result<Vec<i16>> {
    use rubato::{
        Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
    };

    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };

    let ratio = to_rate as f64 / from_rate as f64;
    let chunk_size = 1024;
    let mut resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk_size, 1)
        .map_err(|e| anyhow::anyhow!("Resampler init: {e}"))?;

    // Convert i16 → f32
    let float_samples: Vec<f32> = samples
        .iter()
        .map(|&s| s as f32 / i16::MAX as f32)
        .collect();

    let mut output = Vec::with_capacity((float_samples.len() as f64 * ratio) as usize + 1024);
    let mut pos = 0;

    while pos + chunk_size <= float_samples.len() {
        let chunk = &float_samples[pos..pos + chunk_size];
        let result = resampler
            .process(&[chunk], None)
            .map_err(|e| anyhow::anyhow!("Resample error: {e}"))?;
        output.extend_from_slice(&result[0]);
        pos += chunk_size;
    }

    if pos < float_samples.len() {
        let remaining = &float_samples[pos..];
        let result = resampler
            .process_partial(Some(&[remaining]), None)
            .map_err(|e| anyhow::anyhow!("Resample error: {e}"))?;
        output.extend_from_slice(&result[0]);
    }

    // Convert f32 → i16
    Ok(output
        .iter()
        .map(|&s| (s * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16)
        .collect())
}

/// Build a WAV file from i16 PCM (for local preview playback only, not Telegram).
pub(crate) fn pcm_to_wav(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = (samples.len() * 2) as u32;
    let file_len = 36 + data_len;
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&file_len.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    Ok(buf)
}

/// Play a short voice preview to confirm setup works.
/// Synthesizes "Hey, I am {label}" and plays via system audio.
pub async fn preview_voice(voice_id: &str) -> Result<()> {
    let preset = find_piper_voice(voice_id)
        .ok_or_else(|| anyhow::anyhow!("Unknown Piper voice: {voice_id}"))?;
    let tts = PiperTts::new(voice_id)?;
    let text = format!("Hey! I am {}. Nice to meet you!", preset.label);
    let wav_bytes = {
        let samples = tts.synthesize_pcm(&text)?;
        pcm_to_wav(&samples, tts.sample_rate())?
    };

    // Write WAV to temp file and play
    let tmp = std::env::temp_dir().join(format!("opencrabs_preview_{voice_id}.wav"));
    std::fs::write(&tmp, &wav_bytes)?;

    let player = if cfg!(target_os = "macos") {
        "afplay"
    } else if cfg!(target_os = "windows") {
        "powershell"
    } else {
        "aplay"
    };

    let result = if cfg!(target_os = "windows") {
        tokio::process::Command::new(player)
            .args([
                "-c",
                &format!(
                    "(New-Object Media.SoundPlayer '{}').PlaySync()",
                    tmp.display()
                ),
            ])
            .output()
            .await
    } else {
        tokio::process::Command::new(player)
            .arg(&tmp)
            .output()
            .await
    };

    let _ = std::fs::remove_file(&tmp);

    match result {
        Ok(output) if output.status.success() => {
            tracing::info!("Piper preview played for voice '{}'", voice_id);
            Ok(())
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("Voice preview playback failed: {}", stderr);
            Ok(()) // non-fatal
        }
        Err(e) => {
            tracing::warn!("Could not play voice preview: {}", e);
            Ok(()) // non-fatal
        }
    }
}

/// Extract sample_rate from piper voice config JSON.
pub(crate) fn extract_sample_rate(config: &str) -> Option<u32> {
    let needle = "\"sample_rate\"";
    let pos = config.find(needle)?;
    let rest = &config[pos + needle.len()..];
    let colon = rest.find(':')?;
    let after_colon = rest[colon + 1..].trim_start();
    let num_end = after_colon.find(|c: char| !c.is_ascii_digit())?;
    after_colon[..num_end].parse().ok()
}
