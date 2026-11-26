//! Analyze Video Tool — Gemini-native video understanding.
//!
//! Two strategies tried in order:
//!
//! 1. **Native Gemini video** (primary): upload the file and let Gemini
//!    process it as a video stream. Two upload paths depending on file size:
//!    * **Inline (≤ ~18 MB)**: base64 the bytes and pass them directly inside
//!      `inline_data` on `generateContent`. One round-trip, simplest path.
//!    * **Files API (> 18 MB)**: resumable upload to `/upload/v1beta/files`,
//!      poll the file resource until `state == "ACTIVE"`, then reference it
//!      via `file_data: { file_uri }` in `generateContent`.
//!
//! 2. **Frame extraction fallback**: used when native video isn't available,
//!    either because the primary Gemini path failed (network error, Files-API
//!    FAILED state, Gemini API error) or no Gemini key is configured at all.
//!    Extract N frames at 1 fps (capped at 30 frames) with ffmpeg, analyze
//!    each frame with the available image-vision backend, then stitch the
//!    per-frame results into a single chronological description. The per-frame
//!    backend is Gemini when a Gemini key is present, otherwise the active
//!    provider's own vision model (`ProviderVisionTool`), so video analysis
//!    works on any provider that has image vision, not just Gemini.

use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const GEMINI_UPLOAD_URL: &str = "https://generativelanguage.googleapis.com/upload/v1beta/files";

/// Above this byte threshold we route to the Files API instead of inlining
/// base64 into `generateContent`. 18 MB leaves headroom under Gemini's
/// documented 20 MB inline cap once base64 + JSON wrapping is accounted for
/// (4/3 expansion + escaping + envelope ≈ 26 MB on the wire for an 18 MB
/// payload, still safely below request-size limits in practice).
const INLINE_MAX_BYTES: u64 = 18 * 1024 * 1024;

/// How long to poll a Files-API upload waiting for `state: ACTIVE`.
const FILES_API_POLL_TIMEOUT: Duration = Duration::from_secs(120);
/// Interval between Files-API state checks while polling.
const FILES_API_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Max frames to extract in fallback mode (1 per second of video).
const FALLBACK_MAX_FRAMES: usize = 30;
/// Frame rate for fallback extraction (1 frame per N seconds).
const FALLBACK_FPS: f64 = 1.0;

pub struct AnalyzeVideoTool {
    /// Gemini API key for native video understanding. Empty on non-Gemini
    /// setups, in which case the native strategy is skipped and analysis goes
    /// straight to ffmpeg frame extraction.
    api_key: String,
    /// Gemini vision model name (used by the native path and Gemini frames).
    model: String,
    /// Active provider vision creds `(api_key, base_url, vision_model)`, present
    /// on non-Gemini setups. Lets the frame-extraction fallback describe frames
    /// with the provider's own vision model when Gemini native video isn't
    /// available.
    provider_vision: Option<(String, String, String)>,
}

impl AnalyzeVideoTool {
    /// Gemini-only constructor (native video + Gemini frame vision).
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            provider_vision: None,
        }
    }

    /// Build from config whenever any vision backend is available: Gemini
    /// `image.vision` and/or the active provider's `vision_model`. Returns
    /// `None` when neither is configured, so the tool stays unregistered.
    ///
    /// This is what makes video analysis work on non-Gemini setups: a provider
    /// with image vision can describe ffmpeg-extracted frames even with no
    /// Gemini key. Gemini, when present, still drives native video on top.
    pub fn from_config(config: &crate::config::Config) -> Option<Self> {
        let gemini_key = if config.image.vision.enabled {
            config
                .image
                .vision
                .api_key
                .clone()
                .filter(|k| !k.is_empty())
        } else {
            None
        };
        let provider_vision = crate::brain::provider::factory::active_provider_vision(config);
        if gemini_key.is_none() && provider_vision.is_none() {
            return None;
        }
        Some(Self {
            api_key: gemini_key.unwrap_or_default(),
            model: config.image.vision.model.clone(),
            provider_vision,
        })
    }

    /// Whether a Gemini key is configured, enabling the native video strategy.
    fn has_gemini(&self) -> bool {
        !self.api_key.is_empty()
    }
}

#[async_trait]
impl Tool for AnalyzeVideoTool {
    fn name(&self) -> &str {
        "analyze_video"
    }

    fn description(&self) -> &str {
        "Analyze a video file (local path) using Google Gemini multimodal vision. \
         Use when: the user attached a video and you need to understand its content, \
         the model needs to describe motion / sequence / spoken audio in a video, or \
         a `<<VID:path>>` marker is present in the prompt. Pass `question` to ask \
         something specific (e.g. 'transcribe the spoken audio', 'describe each frame \
         in chronological order'); defaults to a general detailed description. \
         Inline upload for files ≤ 18 MB, otherwise Files API."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "video": {
                    "type": "string",
                    "description": "Local file path to the video (mp4, mov, webm, mkv, avi, 3gp, flv)."
                },
                "question": {
                    "type": "string",
                    "description": "What to ask about the video. Defaults to 'Describe this video in detail — actions, subjects, setting, and any spoken audio in chronological order.'"
                }
            },
            "required": ["video"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network, ToolCapability::ReadFiles]
    }

    fn requires_approval(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        input: Value,
        context: &ToolExecutionContext,
    ) -> super::error::Result<ToolResult> {
        let video_path = match input["video"].as_str() {
            // Resolve the path (expand ~, join relative to the working dir) up
            // front so every downstream read uses the real path (#720). The
            // prompt renders paths tilde-collapsed, so the model passes ~/….
            Some(s) if !s.is_empty() => super::error::resolve_tool_path(s, &context.working_dir())
                .to_string_lossy()
                .to_string(),
            _ => {
                return Ok(ToolResult::error(
                    "Missing required parameter: video".to_string(),
                ));
            }
        };

        let question = input["question"]
            .as_str()
            .unwrap_or(
                "Describe this video in detail — actions, subjects, setting, \
                 and any spoken audio in chronological order.",
            )
            .to_string();

        let metadata = match tokio::fs::metadata(&video_path).await {
            Ok(m) => m,
            Err(e) => {
                return Ok(ToolResult::error(format!(
                    "Failed to stat video file '{}': {}",
                    video_path, e
                )));
            }
        };
        let size = metadata.len();
        let mime_type = detect_video_mime_type(&video_path);

        tracing::info!(
            "analyze_video: path={} size={} mime={} model={}",
            video_path,
            size,
            mime_type,
            self.model,
        );

        // Strategy 1: native Gemini video, only when a Gemini key is
        // configured. Capture both transport errors (Err) and API/empty-response
        // errors (Ok with success=false) so either kind triggers the
        // frame-extraction fallback.
        if self.has_gemini() {
            let native_err: String = match self
                .try_native_video(&video_path, mime_type, size, &question)
                .await
            {
                Ok(result) if result.success => return Ok(result),
                Ok(failed) => failed.error.unwrap_or_else(|| "unknown error".to_string()),
                Err(e) => e.to_string(),
            };

            tracing::warn!(
                "analyze_video: native Gemini path failed ({}). Falling back to ffmpeg \
                 frame extraction + per-frame vision.",
                native_err
            );

            // Strategy 2: ffmpeg frame extraction + per-frame Gemini vision.
            return self
                .frame_extraction_fallback(&video_path, &question, native_err, context)
                .await;
        }

        // No Gemini key: native video isn't available. Go straight to ffmpeg
        // frame extraction and describe the frames with the active provider's
        // vision model.
        tracing::info!(
            "analyze_video: no Gemini key — using ffmpeg frame extraction with provider vision"
        );
        self.frame_extraction_fallback(
            &video_path,
            &question,
            "no Gemini key configured".to_string(),
            context,
        )
        .await
    }
}

impl AnalyzeVideoTool {
    /// Strategy 1: native Gemini video understanding. Builds the inline or
    /// Files-API video part then runs `generateContent`. Returns the
    /// `ToolResult` (which may itself be `success=false` on an API error) or
    /// `Err` on a transport/IO failure — the caller treats either as a
    /// signal to fall back to frame extraction.
    async fn try_native_video(
        &self,
        video_path: &str,
        mime_type: &'static str,
        size: u64,
        question: &str,
    ) -> super::error::Result<ToolResult> {
        let video_part = if size <= INLINE_MAX_BYTES {
            self.build_inline_part(video_path, mime_type).await?
        } else {
            tracing::info!(
                "analyze_video: file size {} > {} inline cap — using Files API",
                size,
                INLINE_MAX_BYTES,
            );
            self.upload_via_files_api(video_path, mime_type, size)
                .await?
        };
        self.run_generate_content(video_part, question).await
    }

    /// Strategy 2: extract up to `FALLBACK_MAX_FRAMES` frames at
    /// `FALLBACK_FPS` with ffmpeg, analyze each frame with the Gemini vision
    /// API (same path as `analyze_image`), and stitch the per-frame
    /// descriptions into one chronological summary. Works on any setup with
    /// a Gemini vision key, including when native video upload is down.
    async fn frame_extraction_fallback(
        &self,
        video_path: &str,
        question: &str,
        native_err: String,
        context: &ToolExecutionContext,
    ) -> super::error::Result<ToolResult> {
        // ffmpeg must be on PATH. If it isn't, neither strategy is available
        // — surface both failures so the user knows why.
        if !ffmpeg_available().await {
            return Ok(ToolResult::error(format!(
                "Video analysis failed. Native Gemini video upload errored ({native_err}) and \
                 the ffmpeg frame-extraction fallback is unavailable: `ffmpeg` is not installed \
                 or not on PATH. Install ffmpeg to enable frame-based video analysis."
            )));
        }

        // Extract frames into a temp dir that auto-cleans on drop.
        let tmp = tempfile::Builder::new()
            .prefix("opencrabs-video-frames-")
            .tempdir()
            .map_err(|e| {
                super::error::ToolError::Execution(format!(
                    "Failed to create temp dir for frame extraction: {e}"
                ))
            })?;
        let pattern = tmp.path().join("frame_%03d.jpg");
        let pattern_str = pattern.to_string_lossy().to_string();

        // -vf fps=F samples F frames/sec; -frames:v caps the total. -q:v 3
        // keeps the JPEGs small but readable for vision.
        let fps_filter = format!("fps={FALLBACK_FPS}");
        let output = tokio::process::Command::new("ffmpeg")
            .kill_on_drop(true)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                video_path,
                "-vf",
                &fps_filter,
                "-frames:v",
                &FALLBACK_MAX_FRAMES.to_string(),
                "-q:v",
                "3",
                &pattern_str,
            ])
            .output()
            .await
            .map_err(|e| {
                super::error::ToolError::Execution(format!("Failed to spawn ffmpeg: {e}"))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Ok(ToolResult::error(format!(
                "Video analysis failed. Native Gemini video upload errored ({native_err}) and \
                 ffmpeg frame extraction failed: {}",
                stderr.trim()
            )));
        }

        // Collect + sort the extracted frames (frame_001.jpg, frame_002.jpg…).
        let mut frames: Vec<std::path::PathBuf> = Vec::new();
        let mut entries = tokio::fs::read_dir(tmp.path()).await.map_err(|e| {
            super::error::ToolError::Execution(format!("Failed to read frame dir: {e}"))
        })?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| {
            super::error::ToolError::Execution(format!("Failed to iterate frame dir: {e}"))
        })? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jpg") {
                frames.push(path);
            }
        }
        frames.sort();

        if frames.is_empty() {
            return Ok(ToolResult::error(format!(
                "Video analysis failed. Native Gemini video upload errored ({native_err}) and \
                 ffmpeg produced no frames (unreadable or zero-length video?)."
            )));
        }

        tracing::info!(
            "analyze_video fallback: extracted {} frame(s), analyzing each with Gemini vision",
            frames.len()
        );

        // Per-frame vision backend. Gemini image vision when a Gemini key is
        // present (request shape, error handling, and model stay in lockstep
        // with the standalone image path); otherwise the active provider's
        // vision model describes each frame.
        let vision: Box<dyn Tool> = if self.has_gemini() {
            Box::new(super::analyze_image::AnalyzeImageTool::new(
                self.api_key.clone(),
                self.model.clone(),
            ))
        } else if let Some((key, base_url, vision_model)) = &self.provider_vision {
            // Non-Gemini setup: describe each frame with the active provider's
            // own vision model. ffmpeg extracted the frames locally, so any
            // provider with image vision can analyze them. This is what makes
            // video work provider-agnostically.
            // Single-candidate list: video frames ride the first resolved
            // vision candidate; frame-level roll-through can come later.
            Box::new(super::provider_vision::ProviderVisionTool::with_candidates(
                vec![(key.clone(), base_url.clone(), vision_model.clone())],
            ))
        } else {
            return Ok(ToolResult::error(format!(
                "Video analysis failed: no vision backend available ({native_err})."
            )));
        };
        let total = frames.len();
        let mut sections: Vec<String> = Vec::with_capacity(total);
        for (idx, frame) in frames.iter().enumerate() {
            // At FALLBACK_FPS frames/sec, frame i (0-based) is ≈ i/FPS seconds in.
            let approx_secs = (idx as f64) / FALLBACK_FPS;
            let per_frame_q = format!(
                "This is frame {} of {} extracted from a video (≈{:.0}s in). Describe \
                 concisely what is visible and any action or change. The user ultimately \
                 asked: {}",
                idx + 1,
                total,
                approx_secs,
                question
            );
            let frame_path = frame.to_string_lossy().to_string();
            let res = vision
                .execute(
                    serde_json::json!({ "image": frame_path, "question": per_frame_q }),
                    context,
                )
                .await;
            let desc = match res {
                Ok(r) if r.success => r.output.trim().to_string(),
                Ok(r) => format!(
                    "[frame analysis failed: {}]",
                    r.error.unwrap_or_else(|| "unknown".to_string())
                ),
                Err(e) => format!("[frame analysis failed: {e}]"),
            };
            sections.push(format!(
                "Frame {} (≈{:.0}s): {}",
                idx + 1,
                approx_secs,
                desc
            ));
        }

        let body = sections.join("\n\n");
        let header = format!(
            "[Frame-extraction fallback — native Gemini video upload was unavailable \
             ({native_err}). Analyzed {total} frame(s) sampled at {FALLBACK_FPS} fps. \
             The descriptions below are per-frame, in chronological order.]\n\n"
        );
        Ok(ToolResult::success(format!("{header}{body}")))
    }

    /// Read the file, base64 it, and produce an `inline_data` part. Single
    /// round-trip path used for files ≤ INLINE_MAX_BYTES.
    async fn build_inline_part(
        &self,
        path: &str,
        mime_type: &'static str,
    ) -> super::error::Result<Value> {
        let bytes = tokio::fs::read(path).await.map_err(|e| {
            super::error::ToolError::Execution(format!(
                "Failed to read video file '{}': {}",
                path, e
            ))
        })?;
        let b64 = super::analyze_image::base64_encode(&bytes);
        Ok(serde_json::json!({
            "inlineData": {
                "mimeType": mime_type,
                "data": b64
            }
        }))
    }

    /// Resumable upload to the Files API, poll until ACTIVE, return a
    /// `file_data` part referencing the uploaded resource. Used for files
    /// larger than INLINE_MAX_BYTES.
    async fn upload_via_files_api(
        &self,
        path: &str,
        mime_type: &'static str,
        size: u64,
    ) -> super::error::Result<Value> {
        // Start, upload and poll all share this client, and the ceiling is a
        // client-level setting, so the slot builds it once.
        static UPLOAD_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
        let client = crate::utils::http::cached_client(&UPLOAD_CLIENT, |b| {
            b.timeout(Duration::from_secs(600))
        });

        // Step 1: start a resumable upload session — Gemini returns the
        // upload URL in the X-Goog-Upload-URL response header.
        let display_name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("video");
        let init_body = serde_json::json!({
            "file": { "display_name": display_name }
        });
        let init_resp = client
            .post(GEMINI_UPLOAD_URL)
            .header("x-goog-api-key", &self.api_key)
            .header("X-Goog-Upload-Protocol", "resumable")
            .header("X-Goog-Upload-Command", "start")
            .header("X-Goog-Upload-Header-Content-Length", size.to_string())
            .header("X-Goog-Upload-Header-Content-Type", mime_type)
            .header("Content-Type", "application/json")
            .json(&init_body)
            .send()
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        if !init_resp.status().is_success() {
            let status = init_resp.status();
            let body = init_resp.text().await.unwrap_or_default();
            return Err(super::error::ToolError::Execution(format!(
                "Files API resumable-start failed: HTTP {} — {}",
                status, body
            )));
        }
        let upload_url = init_resp
            .headers()
            .get("x-goog-upload-url")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                super::error::ToolError::Execution(
                    "Files API resumable-start: missing X-Goog-Upload-URL header".to_string(),
                )
            })?;

        // Step 2: PUT the bytes (one shot, finalize in same call).
        let bytes = tokio::fs::read(path).await.map_err(|e| {
            super::error::ToolError::Execution(format!(
                "Failed to read video file '{}': {}",
                path, e
            ))
        })?;
        let upload_resp = client
            .post(&upload_url)
            .header("Content-Length", bytes.len().to_string())
            .header("X-Goog-Upload-Offset", "0")
            .header("X-Goog-Upload-Command", "upload, finalize")
            .body(bytes)
            .send()
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        if !upload_resp.status().is_success() {
            let status = upload_resp.status();
            let body = upload_resp.text().await.unwrap_or_default();
            return Err(super::error::ToolError::Execution(format!(
                "Files API upload failed: HTTP {} — {}",
                status, body
            )));
        }
        let upload_json: Value = upload_resp.json().await.map_err(|e| {
            super::error::ToolError::Execution(format!(
                "Files API upload: failed to parse JSON response: {}",
                e
            ))
        })?;
        let file_name = upload_json["file"]["name"]
            .as_str()
            .ok_or_else(|| {
                super::error::ToolError::Execution(
                    "Files API upload: missing file.name in response".to_string(),
                )
            })?
            .to_string();
        let file_uri = upload_json["file"]["uri"]
            .as_str()
            .ok_or_else(|| {
                super::error::ToolError::Execution(
                    "Files API upload: missing file.uri in response".to_string(),
                )
            })?
            .to_string();

        // Step 3: poll until state == "ACTIVE" (or timeout). Video files
        // need server-side processing before they can be referenced in
        // generateContent.
        let deadline = std::time::Instant::now() + FILES_API_POLL_TIMEOUT;
        loop {
            if std::time::Instant::now() >= deadline {
                return Err(super::error::ToolError::Execution(format!(
                    "Files API upload: file '{}' did not reach ACTIVE state within {}s",
                    file_name,
                    FILES_API_POLL_TIMEOUT.as_secs()
                )));
            }
            let status_resp = client
                .get(format!("{}/{}", GEMINI_BASE_URL, file_name))
                .header("x-goog-api-key", &self.api_key)
                .send()
                .await
                .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;
            if !status_resp.status().is_success() {
                let status = status_resp.status();
                let body = status_resp.text().await.unwrap_or_default();
                return Err(super::error::ToolError::Execution(format!(
                    "Files API state poll failed: HTTP {} — {}",
                    status, body
                )));
            }
            let status_json: Value = status_resp.json().await.map_err(|e| {
                super::error::ToolError::Execution(format!(
                    "Files API state poll: failed to parse JSON: {}",
                    e
                ))
            })?;
            let state = status_json["state"].as_str().unwrap_or("").to_string();
            tracing::debug!("analyze_video: file '{}' state={}", file_name, state);
            match state.as_str() {
                "ACTIVE" => break,
                "FAILED" => {
                    return Err(super::error::ToolError::Execution(format!(
                        "Files API: upload '{}' entered FAILED state",
                        file_name
                    )));
                }
                _ => {
                    tokio::time::sleep(FILES_API_POLL_INTERVAL).await;
                }
            }
        }

        Ok(serde_json::json!({
            "fileData": {
                "mimeType": mime_type,
                "fileUri": file_uri
            }
        }))
    }

    /// POST a `generateContent` request with the video part + the user's
    /// question, parse out the assembled text response.
    async fn run_generate_content(
        &self,
        video_part: Value,
        question: &str,
    ) -> super::error::Result<ToolResult> {
        let url = format!("{}/models/{}:generateContent", GEMINI_BASE_URL, self.model);
        let body = serde_json::json!({
            "contents": [{
                "parts": [
                    video_part,
                    { "text": question }
                ]
            }]
        });

        // Generous timeout: video processing can take a while server-side
        // even after Files-API ACTIVE.
        let client = crate::utils::http::shared();

        tracing::info!(
            "analyze_video: calling Gemini generateContent model={} url={}",
            self.model,
            url,
        );

        let response = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(|e| super::error::ToolError::Execution(e.to_string()))?;

        let status = response.status().as_u16();
        let body_text = response.text().await.map_err(|e| {
            super::error::ToolError::Execution(format!("Failed to read response body: {}", e))
        })?;

        tracing::info!(
            "analyze_video: Gemini HTTP status={} body[..300]={}",
            status,
            &body_text.chars().take(300).collect::<String>()
        );

        if !(200..300).contains(&status) {
            return Ok(ToolResult::error(format!(
                "Gemini API error {}: {}",
                status, body_text
            )));
        }

        let json: Value = serde_json::from_str(&body_text).map_err(|e| {
            super::error::ToolError::Execution(format!(
                "Failed to parse Gemini JSON response: {}. Body[..500]: {}",
                e,
                body_text.chars().take(500).collect::<String>()
            ))
        })?;

        let empty_vec = vec![];
        let candidates = json["candidates"].as_array().unwrap_or(&empty_vec);
        let mut result_text = String::new();
        for candidate in candidates {
            let empty_parts = vec![];
            let parts = candidate["content"]["parts"]
                .as_array()
                .unwrap_or(&empty_parts);
            for part in parts {
                if let Some(text) = part["text"].as_str() {
                    result_text.push_str(text);
                }
            }
        }

        if result_text.is_empty() {
            Ok(ToolResult::error(
                "No text response from Gemini video analysis".to_string(),
            ))
        } else {
            Ok(ToolResult::success(result_text))
        }
    }
}

/// Whether `ffmpeg` can be invoked on this host. Runs `ffmpeg -version`
/// and treats a successful spawn+exit as available. Used to decide whether
/// the frame-extraction fallback is possible before attempting extraction.
async fn ffmpeg_available() -> bool {
    tokio::process::Command::new("ffmpeg")
        .kill_on_drop(true)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

pub(crate) fn detect_video_mime_type(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".mp4") || lower.ends_with(".m4v") {
        "video/mp4"
    } else if lower.ends_with(".mov") {
        "video/quicktime"
    } else if lower.ends_with(".webm") {
        "video/webm"
    } else if lower.ends_with(".mkv") {
        "video/x-matroska"
    } else if lower.ends_with(".avi") {
        "video/x-msvideo"
    } else if lower.ends_with(".3gp") {
        "video/3gpp"
    } else if lower.ends_with(".flv") {
        "video/x-flv"
    } else {
        "video/mp4"
    }
}
