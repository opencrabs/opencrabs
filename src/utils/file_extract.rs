//! File classification + vision-aware file ingestion pipeline.
//!
//! Two entry points:
//! - `classify_file()` — legacy, no vision (text-extract only)
//! - `process_file_with_vision()` — new, checks vision availability and routes

use crate::config::Config;
use std::fs;
use std::path::PathBuf;

// ── Legacy classify_file (kept for backward compat) ──

/// Returns `true` if any provider has a `vision_model` configured,
/// or if `image.vision` is enabled with an API key.
///
/// When this returns `false`, channel handlers route incoming images
/// to the "unsupported format" notice — and previously the user got
/// no clue WHY. We now log each failed branch at `info` so a user
/// asking "the bot ignored my image, what happened?" can read the
/// log instead of source-diving (#141).
pub fn is_vision_available(config: &Config) -> bool {
    if crate::brain::provider::factory::active_provider_vision(config).is_some() {
        return true;
    }
    if config.image.vision.enabled {
        match config.image.vision.api_key.as_deref() {
            Some(k) if !k.is_empty() => return true,
            Some(_) => {
                tracing::info!(
                    target: "vision",
                    "vision unavailable: [image.vision] enabled but api_key is empty — \
                     keys.toml `[image]` section is the place to put the Gemini key \
                     (config.toml `[image.vision] api_key` is silently ignored, the \
                     field carries `#[serde(skip)]`)"
                );
            }
            None => {
                tracing::info!(
                    target: "vision",
                    "vision unavailable: [image.vision] enabled but no api_key loaded — \
                     add a Gemini key to keys.toml `[image]` section"
                );
            }
        }
    } else {
        let (provider, _model) = config.providers.active_provider_and_model();
        tracing::info!(
            target: "vision",
            active_provider = %provider,
            "vision unavailable: active provider has no `vision_model` set AND \
             [image.vision] is disabled. Two fixes: (1) set `vision_model = \"<model>\"` \
             on [providers.{provider}] in config.toml (preferred — uses the same \
             provider for chat + vision), or (2) run /image to enable Gemini \
             vision globally"
        );
    }
    false
}

// ── FileContent enum ──

/// Result of classifying and extracting content from a user-sent file.
pub enum FileContent {
    /// UTF-8 text extracted inline (capped at 8 000 chars)
    Text(String),
    /// Single image — caller should write bytes to the returned temp path
    Image(PathBuf),
    /// PDF rendered to page images (vision path)
    PdfPages { paths: Vec<PathBuf>, label: String },
    /// Video attachment — caller should write bytes to the returned temp path.
    /// The agent gets a `<<VID:path>>` marker and is told to call
    /// `analyze_video` (Gemini-native video support; future fallback path
    /// handles non-video-capable providers via frame extraction).
    Video(PathBuf),
    /// Unsupported format or failed extraction
    Unsupported(String),
}

// ── Helpers ──

/// Generic text file inline cap. Keeps short notes / small source
/// files fully visible without burning the entire context window on
/// a single ingestion. PDFs use the larger `PDF_TEXT_LIMIT` below
/// because PDFs commonly contain whole documents that the agent
/// needs end-to-end.
const TEXT_LIMIT: usize = 8_000;
/// Inline cap for extracted PDF text. Sized so a ~60-page report
/// fits without forcing the agent to chase pages through tool calls.
/// When exceeded we still save the original PDF to temp and tell the
/// agent how to call `parse_document` with `pages=[...]` for the
/// remainder, so nothing is silently lost.
const PDF_TEXT_LIMIT: usize = 200_000;
const MAX_PDF_PAGES: usize = 100;

/// Determine whether a MIME type is a text file.
pub fn is_text_mime(mime: &str) -> bool {
    let lower = mime.to_lowercase();
    lower.starts_with("text/")
        || matches!(
            lower.as_str(),
            "application/json"
                | "application/xml"
                | "application/x-yaml"
                | "application/yaml"
                | "application/toml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/x-sh"
                | "application/x-python"
                | "application/x-ruby"
        )
}

/// Collapse trailing double extensions (e.g. "file.docx.docx" → "file.docx").
/// Telegram preserves the original filename from forwarded messages; if the source
/// already had an extension matching its MIME type, Telegram appends the same
/// extension again, producing e.g. `zaiavlenie_s_ekzamena.docx.docx`.
pub(crate) fn collapse_double_extension(name: &str) -> String {
    // Split off the last two dot-separated parts
    let parts: Vec<&str> = name.rsplitn(3, '.').collect();
    // parts_rev = [base, inner_ext, outer_ext]
    // e.g. "file.docx.docx" → ["docx", "docx", "file"] → rev: ["file", "docx", "docx"]
    if parts.len() < 3 {
        return name.to_string();
    }
    let parts_rev = parts.into_iter().rev().collect::<Vec<_>>();
    let inner = parts_rev[1];
    let outer = parts_rev[2];
    if inner.eq_ignore_ascii_case(outer) && !inner.is_empty() {
        // Rebuild: base + "." + inner (preserve original case of inner ext)
        format!("{}.{}", parts_rev[0], inner)
    } else {
        name.to_string()
    }
}

/// Guess MIME from filename extension.
pub fn mime_from_ext(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "txt" | "md" | "rst" | "log" => "text/plain",
        "json" => "application/json",
        "xml" | "svg" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "csv" | "tsv" => "text/csv",
        "html" | "htm" => "text/html",
        "js" | "mjs" => "application/javascript",
        "ts" => "text/plain",
        "py" | "rb" | "sh" | "rs" | "go" | "java" | "c" | "cpp" | "h" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xls" => "application/vnd.ms-excel",
        "xlsb" => "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
        "xlsm" => "application/vnd.ms-excel.sheet.macroEnabled.12",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "zip" => "application/zip",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "3gp" => "video/3gpp",
        "flv" => "video/x-flv",
        _ => "application/octet-stream",
    }
}

/// Returns true for video MIME types we route to `analyze_video`.
pub fn is_video_mime(mime: &str) -> bool {
    mime.to_lowercase().starts_with("video/")
}

/// Returns true if a video-capable analysis backend is configured.
///
/// Mirrors [`is_vision_available`]: any active provider `vision_model` counts,
/// not just Gemini. A provider with image vision can describe ffmpeg-extracted
/// frames, so video vision is available wherever image vision is. Gemini adds
/// native video understanding on top, but is no longer required.
pub(crate) fn is_video_vision_available(config: &Config) -> bool {
    if crate::brain::provider::factory::active_provider_vision(config).is_some() {
        return true;
    }
    config.image.vision.enabled
        && config
            .image
            .vision
            .api_key
            .as_ref()
            .is_some_and(|k| !k.is_empty())
}

/// Write file bytes to a temp path under `~/.opencrabs/tmp/files/` and return the path.
fn save_to_temp(bytes: &[u8], filename: &str) -> Result<PathBuf, String> {
    let tmp_dir = crate::config::opencrabs_home().join("tmp").join("files");
    fs::create_dir_all(&tmp_dir).map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let safe_name = filename
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
        .collect::<String>();
    // Collapse double extensions (e.g. "file.docx.docx") — Telegram preserves the
    // original filename from forwarded messages; if the source already had the
    // extension, Telegram appends it again based on MIME type.
    let safe_name = collapse_double_extension(&safe_name);
    let path = tmp_dir.join(format!("{}_{safe_name}", uuid::Uuid::new_v4()));
    fs::write(&path, bytes).map_err(|e| format!("Failed to write temp file: {e}"))?;
    Ok(path)
}

/// Extract text from PDF bytes. The inline content is capped at
/// `PDF_TEXT_LIMIT` chars (~60 pages of a typical report). The
/// original PDF is also saved to `~/.opencrabs/tmp/files/` and the
/// path is included in the message so the agent can call
/// `parse_document(path, pages=[...])` to pull the remainder
/// without losing fidelity. Form-feed (`\u{000C}`) page boundaries
/// are preserved so `parse_document`'s `pages` filter works on the
/// same text the agent already saw.
fn extract_pdf_text(bytes: &[u8], filename: &str) -> FileContent {
    let raw = match pdf_extract::extract_text_from_mem(bytes) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("pdf_extract failed for {filename}: {e} — surfacing as unsupported");
            return FileContent::Unsupported(format!(
                "[File received: {filename} (PDF) — failed to extract text: {e}]"
            ));
        }
    };
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return FileContent::Unsupported(format!(
            "[File received: {filename} (PDF) — no extractable text found, may be image-based]"
        ));
    }

    // Save the original PDF so the agent has a path to pass to
    // `parse_document` when it needs pages we truncated. We always
    // save (not only on truncation) so the agent can re-query for
    // any page even when the inline preview was complete — it can
    // verify a quote, paginate, or pull just one page for focused
    // analysis without re-uploading.
    let saved_path = save_to_temp(bytes, filename).ok();

    let full_len = trimmed.chars().count();
    let total_pages = trimmed.matches('\u{000C}').count() + 1;
    let truncated_text = if full_len > PDF_TEXT_LIMIT {
        let preview: String = trimmed.chars().take(PDF_TEXT_LIMIT).collect();
        let preview_pages = preview.matches('\u{000C}').count() + 1;
        let path_hint = saved_path
            .as_ref()
            .map(|p| {
                format!(
                    "\n\n[Inline preview shows pages 1-{preview_pages} of {total_pages} (~{} of {} chars). \
                     Original PDF saved at: {}\n\
                     Call `parse_document(path='{}', pages=[{}, ...])` for the remaining pages.]",
                    PDF_TEXT_LIMIT,
                    full_len,
                    p.display(),
                    p.display(),
                    preview_pages + 1,
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "\n\n[Inline preview truncated at {PDF_TEXT_LIMIT} of {full_len} chars; \
                     full PDF could not be saved to disk for `parse_document` follow-up.]"
                )
            });
        format!("{preview}…{path_hint}")
    } else if let Some(ref p) = saved_path {
        format!(
            "{trimmed}\n\n[Full PDF saved at: {} — call `parse_document(path='{}', pages=[N])` to re-query any specific page.]",
            p.display(),
            p.display()
        )
    } else {
        trimmed
    };

    // Text extraction drops embedded figures/screenshots/charts. Point the
    // agent at the on-demand page renderer for anything visual the text
    // can't convey (only when we have a saved path to hand it).
    let visual_hint = saved_path
        .as_ref()
        .map(|p| {
            format!(
                "\n[If the user asks about figures, diagrams, screenshots, charts, or \
                 anything visual that the text above doesn't convey, call \
                 `pdf_to_images(path='{}', page_range='N')` to render those pages, then \
                 view them with `analyze_image`.]",
                p.display()
            )
        })
        .unwrap_or_default();

    FileContent::Text(format!(
        "[File: {filename} ({total_pages} pages)]\n```\n{truncated_text}\n```{visual_hint}"
    ))
}

// ── Vision-aware pipeline ──

/// Process file bytes with vision-first routing.
///
/// Priority:
/// 1. **Vision available** → render PDF pages to images, or save image for vision
/// 2. **No vision** → extract text (PDFs/text files) or return unsupported
///
/// Channels pass the result to `inject_file_content()` to format
/// for the agent prompt, or match on `FileContent` directly.
pub fn process_file_with_vision(
    bytes: &[u8],
    mime: &str,
    filename: &str,
    config: &Config,
) -> FileContent {
    let effective = if mime.is_empty() || mime == "application/octet-stream" {
        mime_from_ext(filename)
    } else {
        mime
    };

    let has_vision = is_vision_available(config);

    // ── Images ──
    if effective.starts_with("image/") {
        if has_vision {
            return match save_to_temp(bytes, filename) {
                Ok(path) => FileContent::Image(path),
                Err(e) => FileContent::Unsupported(format!(
                    "[Image attachment: {filename} — failed to save for vision: {e}]"
                )),
            };
        }
        return FileContent::Unsupported(format!(
            "[Image attachment: {filename} — no vision model configured. \
             Set `image.vision.enabled = true` with an API key, or add `vision_model` \
             to your provider config in config.toml.]"
        ));
    }

    // ── Videos ──
    if is_video_mime(effective) {
        if is_video_vision_available(config) {
            return match save_to_temp(bytes, filename) {
                Ok(path) => FileContent::Video(path),
                Err(e) => FileContent::Unsupported(format!(
                    "[Video attachment: {filename} — failed to save for vision: {e}]"
                )),
            };
        }
        return FileContent::Unsupported(format!(
            "[Video attachment: {filename} — no vision model configured. \
             Set `image.vision.enabled = true` with an API key, or add `vision_model` \
             to your provider config in config.toml.]"
        ));
    }

    // ── PDFs ──
    if effective == "application/pdf" {
        return process_pdf_smart(bytes, filename, has_vision);
    }

    // ── ZIP archives ──
    if effective == "application/zip" || effective == "application/x-zip-compressed" {
        return extract_zip_contents(bytes, filename, config);
    }

    // ── Excel spreadsheets ──
    if matches!(
        effective,
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" // .xlsx
        | "application/vnd.ms-excel" // .xls
        | "application/vnd.ms-excel.sheet.binary.macroEnabled.12" // .xlsb
        | "application/vnd.ms-excel.sheet.macroEnabled.12"       // .xlsm
        | "application/vnd.oasis.opendocument.spreadsheet"       // .ods
        | "application/xlsx"
        | "application/xls"
    ) {
        return match save_to_temp(bytes, filename) {
            Ok(path) => {
                let path_str = path.to_string_lossy();
                FileContent::Text(format!(
                    "[Excel file received: {filename}]\n\nFile saved at: {}\n\nCall `parse_document(path='{}')` to extract and view the spreadsheet contents.",
                    path_str, path_str
                ))
            }
            Err(e) => {
                FileContent::Unsupported(format!("[Excel file: {filename} — failed to save: {e}]"))
            }
        };
    }

    // ── Text files ──
    if is_text_mime(effective) {
        let raw = String::from_utf8_lossy(bytes);
        let truncated = if raw.len() > TEXT_LIMIT {
            format!(
                "{}…[truncated]",
                raw.chars().take(TEXT_LIMIT).collect::<String>()
            )
        } else {
            raw.into_owned()
        };
        return FileContent::Text(format!("[File: {filename}]\n```\n{truncated}\n```"));
    }

    FileContent::Unsupported(format!(
        "[File received: {filename} ({effective}) — unsupported format]"
    ))
}

/// Minimum chars / page for a PDF to count as "text-rich". Below
/// this we assume the doc is scanned or image-only and reach for
/// vision. Tuned to catch even sparse layouts (e.g. one paragraph
/// per page in a slide deck) while rejecting OCR-empty scans.
const PDF_TEXT_DENSITY_MIN_CHARS_PER_PAGE: usize = 100;
/// Minimum total chars before we trust `pdf_extract` enough to
/// avoid the vision path. Combined with the per-page floor this
/// rejects PDFs where the metadata bled into the first page as a
/// single short line.
const PDF_TEXT_DENSITY_MIN_TOTAL: usize = 500;

/// Route a PDF to the right backend based on extractable text density.
///
/// Always tries `pdf_extract` first. For text-rich PDFs (~95% of
/// real-world reports, contracts, papers, books) this returns the
/// text inline + the saved PDF path so the agent can call
/// `parse_document` for any remaining pages. No page images are
/// bundled — the request body stays small (typically ~200 KB),
/// providers never hit body-size limits, and vision tokens are not
/// burned on every page upfront.
///
/// Only when `pdf_extract` returns empty or sparse text (likely a
/// scanned PDF) do we render pages via the vision pipeline. Even
/// then, the rendered page paths are surfaced as a list for the
/// agent to call `analyze_image` on lazily — one page per tool
/// call, never bundled.
fn process_pdf_smart(bytes: &[u8], filename: &str, has_vision: bool) -> FileContent {
    let raw_text = pdf_extract::extract_text_from_mem(bytes).ok();
    let trimmed = raw_text
        .as_ref()
        .map(|t| t.trim().to_string())
        .unwrap_or_default();
    let char_count = trimmed.chars().count();
    let total_pages = if trimmed.is_empty() {
        0
    } else {
        trimmed.matches('\u{000C}').count() + 1
    };
    let chars_per_page = char_count.checked_div(total_pages).unwrap_or(0);
    let has_readable_text = char_count >= PDF_TEXT_DENSITY_MIN_TOTAL
        && chars_per_page >= PDF_TEXT_DENSITY_MIN_CHARS_PER_PAGE;

    if has_readable_text {
        tracing::debug!(
            "PDF {filename}: {char_count} chars / {total_pages} pages \
             ({chars_per_page}/page) — using text path"
        );
        return extract_pdf_text(bytes, filename);
    }

    if !has_vision {
        tracing::warn!(
            "PDF {filename}: sparse text ({char_count} chars / {total_pages} pages) and no \
             vision configured — surfacing as Unsupported"
        );
        return FileContent::Unsupported(format!(
            "[File received: {filename} (PDF, {total_pages} pages) — only {char_count} chars extracted (~{chars_per_page}/page). \
             Likely a scanned/image-based PDF that needs a vision model. \
             Enable `[image.vision]` in config.toml or set `vision_model` on your provider.]"
        ));
    }

    tracing::info!(
        "PDF {filename}: sparse text ({char_count} chars / {total_pages} pages) — \
         rendering pages for lazy vision"
    );
    process_pdf_vision(bytes, filename)
}

fn process_pdf_vision(bytes: &[u8], filename: &str) -> FileContent {
    // Save PDF to temp so pdfium-render can read it
    let pdf_path = match save_to_temp(bytes, filename) {
        Ok(p) => p,
        Err(e) => {
            return FileContent::Unsupported(format!(
                "[PDF received: {filename} — failed to prepare: {e}]"
            ));
        }
    };

    // Render pages into a DEDICATED sub-directory, never the shared
    // tmp/files/ dir. `render_pdf_pages` wipes its output dir on failure;
    // pointing it at the PDF's parent would delete the file we just saved
    // (and every other session's files in there) — the exact reason an
    // uploaded scanned PDF vanished and the agent's `find` came up empty.
    let stem = pdf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("pdf");
    let render_dir = pdf_path
        .parent()
        .map(|p| p.join(format!("{stem}_pages")))
        .unwrap_or_else(|| pdf_path.with_extension("pages"));

    let rendered = super::pdf_vision::render_pdf_pages(
        pdf_path.to_str().unwrap_or(""),
        MAX_PDF_PAGES,
        render_dir.to_str().unwrap_or(""),
    );

    match rendered {
        Ok(paths) if !paths.is_empty() => {
            let page_count = paths.len();
            let label = if page_count == 1 {
                "scanned PDF".to_string()
            } else {
                format!("scanned {page_count}-page PDF")
            };
            FileContent::PdfPages { paths, label }
        }
        other => {
            // Rendering is unavailable here (no pdfium feature / no
            // pdftoppm). The saved PDF still exists — DO NOT lose it.
            // Hand the agent the real path so it can retry with
            // parse_document / pdf_to_images instead of blindly `find`ing.
            let render_err = match other {
                Ok(_) => "renderer produced no output".to_string(),
                Err(e) => e,
            };
            tracing::warn!(
                "PDF {filename}: vision render failed: {render_err}; text path also empty — \
                 keeping saved PDF at {}",
                pdf_path.display()
            );
            FileContent::Unsupported(format!(
                "[User attached a scanned/image PDF: {filename}. Text extraction found nothing and \
                 page rendering is unavailable in this runtime ({render_err}). The file is saved at \
                 {p} — call `pdf_to_images(path='{p}', page_range='1-3')` then `analyze_image` to \
                 view pages, or `parse_document(path='{p}')` to retry text. Do NOT search for it.]",
                p = pdf_path.display()
            ))
        }
    }
}

// ── Channel injection helper ──

/// Format `FileContent` into an injectable string for the agent prompt.
///
/// Returns `(text, needs_vision)` where `needs_vision` is true when
/// the result contains `<<IMG:...>>` or `<<VID:...>>` markers that
/// `build_user_message` will base64-inline into the user Message.
/// PdfPages deliberately does NOT use those markers — see the
/// comment on that branch for why — so for scanned PDFs the bool is
/// false and the agent reaches images via per-page `analyze_image`
/// tool calls instead.
pub fn inject_file_content(content: &FileContent) -> (String, bool) {
    match content {
        FileContent::Image(path) => {
            let path_str = path.to_string_lossy();
            (
                format!(
                    "[User attached an image. Call analyze_image with this path to view it. If the user asks to edit, modify, replace elements, or restyle the image, call generate_image with this path as the 'image' parameter instead.]\n<<IMG:{path_str}>>"
                ),
                true,
            )
        }
        FileContent::PdfPages { paths, label } => {
            // Surface page paths as a plain text list. We deliberately
            // do NOT emit `<<IMG:...>>` markers here — those would
            // make `build_user_message` base64-inline every page
            // image into a single user Message, which on a 32-page
            // PDF balloons the request body past most providers'
            // limits (Dialagram caps at 10 MB; OpenRouter 20 MB).
            // Instead, the agent calls `analyze_image` per page as
            // it needs them, so each request ships at most one
            // image and vision tokens are paid only for pages
            // actually read.
            let path_list: String = paths
                .iter()
                .enumerate()
                .map(|(i, p)| format!("- Page {}: {}", i + 1, p.to_string_lossy()))
                .collect::<Vec<_>>()
                .join("\n");
            (
                format!(
                    "[User attached a {label} ({n} page(s)). No extractable text — pages were \
                     rendered as images. Call `analyze_image(image='<path>', question='...')` \
                     ONE PAGE AT A TIME as you need content. Do NOT try to read all pages in \
                     one turn — providers cap request body size and bundling fails.]\n{path_list}",
                    n = paths.len(),
                ),
                false,
            )
        }
        FileContent::Video(path) => {
            let path_str = path.to_string_lossy();
            (
                format!(
                    "[User attached a video. Call analyze_video with this path to view it. \
                     analyze_video accepts an optional `question` arg — pass the user's actual \
                     question if they asked something specific, otherwise it defaults to a \
                     general description.]\n<<VID:{path_str}>>"
                ),
                true,
            )
        }
        FileContent::Text(text) => (text.clone(), false),
        FileContent::Unsupported(note) => (note.clone(), false),
    }
}

/// Legacy classify_file — text-extract only, no vision routing.
pub fn classify_file(bytes: &[u8], mime: &str, filename: &str) -> FileContent {
    // ... (backward compat — delegates to process_file_with_vision with a dummy config)
    // Actually, keep original behavior to not break callers that don't have Config
    let effective = if mime.is_empty() || mime == "application/octet-stream" {
        mime_from_ext(filename)
    } else {
        mime
    };

    if effective.starts_with("image/") {
        return FileContent::Image(PathBuf::new());
    }

    if effective == "application/pdf" {
        return extract_pdf_text(bytes, filename);
    }

    if is_text_mime(effective) {
        let raw = String::from_utf8_lossy(bytes);
        let truncated = if raw.len() > TEXT_LIMIT {
            format!(
                "{}…[truncated]",
                raw.chars().take(TEXT_LIMIT).collect::<String>()
            )
        } else {
            raw.into_owned()
        };
        return FileContent::Text(format!("[File: {filename}]\n```\n{truncated}\n```"));
    }

    FileContent::Unsupported(format!(
        "[File received: {filename} ({effective}) — unsupported format]"
    ))
}

/// Extract and process files from a ZIP archive.
///
/// For each entry in the archive:
/// - Text files → inline content
/// - Images → save to temp + vision marker
/// - PDFs → text extraction
/// - Nested archives → skip with note
/// - Binary/unsupported → note with filename
///
/// Returns a combined FileContent with all extracted files.
fn extract_zip_contents(bytes: &[u8], archive_name: &str, config: &Config) -> FileContent {
    use std::io::Read as _;

    let reader = std::io::Cursor::new(bytes);
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(a) => a,
        Err(e) => {
            return FileContent::Unsupported(format!(
                "[ZIP archive: {archive_name} — failed to open: {e}]"
            ));
        }
    };

    let mut parts: Vec<String> = Vec::new();
    let file_count = archive.len();

    for i in 0..file_count {
        let mut file = match archive.by_index(i) {
            Ok(f) => f,
            Err(e) => {
                parts.push(format!("[Error reading entry {i}: {e}]"));
                continue;
            }
        };

        let name = match file.name() {
            Ok(name) => name.to_string(),
            Err(e) => {
                parts.push(format!("[Error reading entry name {i}: {e}]"));
                continue;
            }
        };

        // Skip directories
        if file.is_dir() {
            continue;
        }

        // Skip hidden files and macOS metadata
        let basename = name.rsplit('/').next().unwrap_or(&name);
        if basename.starts_with('.') || basename.starts_with("__MACOSX") {
            continue;
        }

        // Read file bytes (cap at 10MB per entry)
        let mut buf = Vec::new();
        if let Err(e) = file.read_to_end(&mut buf) {
            parts.push(format!("[{name} — read error: {e}]"));
            continue;
        }
        if buf.len() > 10 * 1024 * 1024 {
            parts.push(format!(
                "[{name} — skipped, too large ({}MB)]",
                buf.len() / 1024 / 1024
            ));
            continue;
        }

        let entry_mime = mime_from_ext(&name);
        let content = process_file_with_vision(&buf, entry_mime, &name, config);

        match content {
            FileContent::Text(t) => parts.push(t),
            FileContent::Image(path) => {
                parts.push(format!("<<IMG:{}>>", path.display()));
            }
            FileContent::Video(path) => {
                parts.push(format!("<<VID:{}>>", path.display()));
            }
            FileContent::PdfPages { paths, label } => {
                // Same reasoning as the `inject_file_content`
                // PdfPages branch: list page paths as plain text so
                // the agent calls `analyze_image` per page on
                // demand, instead of base64-inlining every page
                // upfront and busting provider body limits.
                let path_list: String = paths
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("- Page {}: {}", i + 1, p.display()))
                    .collect::<Vec<_>>()
                    .join("\n");
                parts.push(format!(
                    "[{label} from zip] Call `analyze_image` per page as needed:\n{path_list}"
                ));
            }
            FileContent::Unsupported(msg) => {
                parts.push(msg);
            }
        }

        // Safety cap: stop after 50 files
        if parts.len() >= 50 {
            parts.push(format!(
                "[... and {} more files truncated]",
                file_count - i - 1
            ));
            break;
        }
    }

    if parts.is_empty() {
        return FileContent::Unsupported(format!(
            "[ZIP archive: {archive_name} — empty or no processable files]"
        ));
    }

    let combined = if file_count == 1 {
        parts.into_iter().next().unwrap()
    } else {
        format!(
            "[ZIP archive: {archive_name} — {file_count} files]

{}",
            parts.join(
                "

---

"
            )
        )
    };

    FileContent::Text(combined)
}
