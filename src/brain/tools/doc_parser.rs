//! Document Parser Tool
//!
//! Parses various document formats (PDF, DOCX, TXT, etc.) to extract text content.

use super::error::{Result, ToolError};
use super::r#trait::{Tool, ToolCapability, ToolExecutionContext, ToolResult};
use super::vba_modules;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::Path;

/// Decode a text event and resolve its XML entities.
///
/// quick-xml 0.41 dropped `BytesText::unescape`, which did both steps in one
/// call, and split it into `decode` plus the free `escape::unescape`. The
/// version bump was forced by RUSTSEC-2026-0194 and -0195 (both 7.5, DoS on
/// malicious XML), so this keeps the three extraction sites reading the way
/// they did rather than spreading the two-step dance through each of them.
///
/// `None` on either a decoding or an entity error, matching the previous
/// `if let Ok(..)` sites: a document whose text will not decode is skipped
/// rather than failing the whole extraction.
fn decoded_text(e: &quick_xml::events::BytesText<'_>) -> Option<String> {
    let raw = e.decode().ok()?;
    Some(quick_xml::escape::unescape(&raw).ok()?.into_owned())
}

/// Dump every sheet of an already-opened calamine workbook into `output`
/// as `=== Sheet: name ===` blocks with ` | `-joined rows (#955).
fn dump_workbook_sheets<RS, R>(workbook: &mut R, output: &mut String)
where
    RS: std::io::Read + std::io::Seek,
    R: calamine::Reader<RS>,
{
    let sheet_names = workbook.sheet_names().to_vec();
    for sheet_name in &sheet_names {
        if let Ok(range) = workbook.worksheet_range(sheet_name) {
            output.push_str(&format!("\n=== Sheet: {} ===\n", sheet_name));
            for row in range.rows() {
                let cells: Vec<String> = row.iter().map(|cell| cell.to_string()).collect();
                output.push_str(&cells.join(" | "));
                output.push('\n');
            }
        }
    }
}

/// Document Parser Tool - extracts text from various document formats
pub struct DocParserTool;

/// Maximum file size for document parsing (50MB)
/// This prevents memory exhaustion from very large documents
const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
struct DocParserInput {
    /// Path to the document file
    path: String,

    /// Optional: Maximum characters to extract (default: no limit)
    #[serde(skip_serializing_if = "Option::is_none")]
    max_chars: Option<usize>,

    /// Optional: Specific pages to extract (PDF only, 1-indexed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pages: Option<Vec<usize>>,

    /// Optional: Page range string like "1-30", "5,7,10-15", or
    /// "1,3,5". Merged with `pages` if both are supplied. Easier for
    /// the agent to use than `pages` for contiguous spans.
    #[serde(skip_serializing_if = "Option::is_none")]
    page_range: Option<String>,

    /// Optional: Include metadata in output
    #[serde(skip_serializing_if = "Option::is_none")]
    include_metadata: Option<bool>,
}

/// Parse a page-range string like `"1-30"`, `"5,7,10-15"` into a
/// flat `Vec<usize>` of 1-indexed page numbers. Invalid tokens are
/// silently skipped — the caller's responsibility is to pass a
/// well-formed string; we don't error on garbage so the tool stays
/// forgiving when the model emits a slightly malformed value.
pub(crate) fn parse_page_range(spec: &str) -> Vec<usize> {
    // Normalise space around the dash so verbose model output like
    // `"1 - 5"` doesn't get split as three tokens (`"1"`, `"-"`,
    // `"5"`) and silently drop pages 2-4. We only collapse spaces
    // *immediately adjacent* to a dash; spaces elsewhere still
    // separate tokens, so `"1-3 5-7"` continues to parse as two
    // ranges.
    let normalised = spec.replace(" -", "-").replace("- ", "-");
    let mut out: Vec<usize> = Vec::new();
    for token in normalised
        .split([',', ' '])
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        if let Some((start, end)) = token.split_once('-') {
            let (Ok(s), Ok(e)) = (start.trim().parse::<usize>(), end.trim().parse::<usize>())
            else {
                continue;
            };
            if s == 0 || e == 0 || s > e {
                continue;
            }
            for p in s..=e {
                out.push(p);
            }
        } else if let Ok(p) = token.parse::<usize>()
            && p > 0
        {
            out.push(p);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[derive(Debug, Serialize)]
struct DocumentMetadata {
    format: String,
    file_size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<String>,
}

#[async_trait]
impl Tool for DocParserTool {
    fn name(&self) -> &str {
        "parse_document"
    }

    fn description(&self) -> &str {
        "Parse and extract text content from documents (PDF, DOC, DOCX, XLSX, XLSM, XLSB, XLS, ODS, CSV, TXT, MD, HTML, JSON, XML). \
        Use this whenever an incoming PDF's inline preview was truncated — the file is \
        saved at the path included in the preview message; pass that path here with \
        `page_range` (e.g. \"31-60\") or `pages` to read the rest. \
        For spreadsheets (Excel/CSV), extracts all sheets and formats as readable tables. \
        For multi-page PDFs prefer `page_range` (string like \"1-30\" or \"5,7,10-15\") \
        over `pages` (raw array) — it's shorter and more natural for spans. \
        Returns full document text with `--- Page N ---` markers when a range is requested."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the document file (PDF, DOC, DOCX, XLSX, XLSM, XLSB, XLS, ODS, CSV, TXT, MD, HTML, JSON, XML)"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Optional: Maximum characters to extract (default: unlimited). Use for very large documents when you only need a preview.",
                    "minimum": 1
                },
                "pages": {
                    "type": "array",
                    "items": {"type": "integer", "minimum": 1},
                    "description": "Optional: Specific page numbers to extract (PDF only, 1-indexed). Prefer `page_range` for contiguous spans."
                },
                "page_range": {
                    "type": "string",
                    "description": "Optional: Page range like \"1-30\", \"5,7,10-15\", or \"3\". Easier than `pages` for spans. Merged with `pages` when both are supplied."
                },
                "include_metadata": {
                    "type": "boolean",
                    "description": "Optional: Include document metadata (page count, title, author) in output (default: false)"
                }
            },
            "required": ["path"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadFiles]
    }

    fn requires_approval(&self) -> bool {
        false // Reading documents is generally safe
    }

    fn validate_input(&self, input: &Value) -> Result<()> {
        let _: DocParserInput = serde_json::from_value(input.clone())
            .map_err(|e| ToolError::InvalidInput(format!("Invalid input: {}", e)))?;
        Ok(())
    }

    async fn execute(&self, input: Value, context: &ToolExecutionContext) -> Result<ToolResult> {
        let input: DocParserInput = serde_json::from_value(input)?;

        // Resolve path (tilde expansion + absolute/relative resolution).
        let path = super::error::resolve_tool_path(&input.path, &context.working_dir());

        // Check if file exists
        if !path.exists() {
            return Ok(ToolResult::error(format!(
                "File not found: {}",
                path.display()
            )));
        }

        // Check if it's a file (not a directory)
        if !path.is_file() {
            return Ok(ToolResult::error(format!(
                "Path is not a file: {}",
                path.display()
            )));
        }

        // Check file size to prevent memory exhaustion
        let file_size = std::fs::metadata(&path).map_err(ToolError::Io)?.len();
        if file_size > MAX_FILE_SIZE {
            return Ok(ToolResult::error(format!(
                "File size ({} MB) exceeds maximum allowed size ({} MB). \
                Consider splitting the document or using a different approach.",
                file_size / (1024 * 1024),
                MAX_FILE_SIZE / (1024 * 1024)
            )));
        }

        // Determine file type and parse accordingly
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let (text, metadata) = match extension.as_str() {
            "pdf" => self.parse_pdf(&path, &input).await?,
            "doc" => self.parse_legacy_doc(&path).await?,
            "docx" => self.parse_docx(&path).await?,
            "xlsx" | "xlsm" | "xlsb" | "xls" | "ods" => self.parse_excel(&path).await?,
            "csv" => self.parse_csv(&path).await?,
            "txt" | "md" | "markdown" | "rst" | "text" => {
                self.parse_text(&path, &extension).await?
            }
            "html" | "htm" => self.parse_html(&path).await?,
            "json" => self.parse_json(&path).await?,
            "xml" => self.parse_xml(&path).await?,
            _ => {
                return Ok(ToolResult::error(format!(
                    "Unsupported document format: .{}. Supported formats: PDF, DOC, DOCX, XLSX, XLSM, XLSB, XLS, ODS, CSV, TXT, MD, HTML, JSON, XML",
                    extension
                )));
            }
        };

        // Apply character limit if specified
        let text = if let Some(max_chars) = input.max_chars {
            if text.len() > max_chars {
                format!(
                    "{}...\n\n[Truncated: {} of {} characters shown]",
                    crate::utils::truncate_str(&text, max_chars),
                    max_chars,
                    text.len()
                )
            } else {
                text
            }
        } else {
            text
        };

        // Build output
        let output = if input.include_metadata.unwrap_or(false) {
            let meta = DocumentMetadata {
                format: extension.clone(),
                file_size,
                page_count: metadata.page_count,
                title: metadata.title,
                author: metadata.author,
            };
            format!(
                "=== Document Metadata ===\n{}\n\n=== Content ===\n{}",
                serde_json::to_string_pretty(&meta).unwrap_or_default(),
                text
            )
        } else {
            text
        };

        let output_len = output.len();
        Ok(ToolResult::success(output)
            .with_metadata("path".to_string(), path.display().to_string())
            .with_metadata("format".to_string(), extension)
            .with_metadata("chars".to_string(), output_len.to_string()))
    }
}

#[derive(Default)]
struct ParsedMetadata {
    page_count: Option<usize>,
    title: Option<String>,
    author: Option<String>,
}

impl DocParserTool {
    /// Parse PDF document. When `pages` and/or `page_range` are
    /// supplied, only those pages (deduped + sorted) are returned
    /// with `--- Page N ---` headers. If both produce no valid
    /// page numbers we fall back to the full document.
    async fn parse_pdf(
        &self,
        path: &Path,
        input: &DocParserInput,
    ) -> Result<(String, ParsedMetadata)> {
        let path = path.to_path_buf();

        // Merge `pages` + `page_range` into a single deduped list.
        let mut requested: Vec<usize> = input.pages.clone().unwrap_or_default();
        if let Some(ref spec) = input.page_range {
            requested.extend(parse_page_range(spec));
        }
        requested.sort_unstable();
        requested.dedup();
        // Empty vec → return all pages.
        let requested = if requested.is_empty() {
            None
        } else {
            Some(requested)
        };

        // Run PDF parsing in blocking task
        tokio::task::spawn_blocking(move || {
            let bytes = std::fs::read(&path).map_err(ToolError::Io)?;

            // Extract text from PDF
            let text = pdf_extract::extract_text_from_mem(&bytes)
                .map_err(|e| ToolError::Execution(format!("Failed to parse PDF: {}", e)))?;

            // Count pages once so metadata reflects the source doc,
            // not the filtered slice we return to the agent.
            let total_pages = text.matches('\u{000C}').count() + 1;

            // If specific pages requested, filter them
            let text = if let Some(page_nums) = requested {
                let pages: Vec<&str> = text.split('\u{000C}').collect();
                let mut selected_text = String::new();
                let mut skipped: Vec<usize> = Vec::new();

                for page_num in page_nums {
                    if page_num > 0 && page_num <= pages.len() {
                        selected_text.push_str(&format!("--- Page {page_num} ---\n"));
                        selected_text.push_str(pages[page_num - 1].trim());
                        selected_text.push_str("\n\n");
                    } else {
                        skipped.push(page_num);
                    }
                }

                if !skipped.is_empty() {
                    selected_text.push_str(&format!(
                        "\n[Requested pages not present in document ({total_pages} total): {skipped:?}]\n"
                    ));
                }

                if selected_text.is_empty() {
                    text // Fall back to full text if no valid pages
                } else {
                    selected_text
                }
            } else {
                text
            };

            let metadata = ParsedMetadata {
                page_count: Some(total_pages),
                title: None,
                author: None,
            };

            Ok((text.trim().to_string(), metadata))
        })
        .await
        .map_err(|e| ToolError::Execution(format!("PDF parsing task failed: {e}")))?
    }

    /// Parse DOCX document (Office Open XML)
    async fn parse_docx(&self, path: &Path) -> Result<(String, ParsedMetadata)> {
        let path = path.to_path_buf();

        tokio::task::spawn_blocking(move || {
            let file = std::fs::File::open(&path).map_err(ToolError::Io)?;
            let mut archive = zip::ZipArchive::new(file)
                .map_err(|e| ToolError::Execution(format!("Failed to open DOCX: {}", e)))?;

            let mut text_content = String::new();
            let mut title = None;
            let mut author = None;

            // Extract document.xml (main content)
            if let Ok(mut document_xml) = archive.by_name("word/document.xml") {
                let mut xml_content = String::new();
                document_xml
                    .read_to_string(&mut xml_content)
                    .map_err(ToolError::Io)?;

                // Parse XML to extract text
                text_content = Self::extract_text_from_docx_xml(&xml_content);
            }

            // Extract core.xml (metadata)
            if let Ok(mut core_xml) = archive.by_name("docProps/core.xml") {
                let mut xml_content = String::new();
                core_xml
                    .read_to_string(&mut xml_content)
                    .map_err(ToolError::Io)?;

                let (t, a) = Self::extract_metadata_from_core_xml(&xml_content);
                title = t;
                author = a;
            }

            let metadata = ParsedMetadata {
                page_count: None, // DOCX doesn't store page count in metadata
                title,
                author,
            };

            Ok((text_content, metadata))
        })
        .await
        .map_err(|e| ToolError::Execution(format!("DOCX parsing task failed: {}", e)))?
    }

    /// Extract text from DOCX XML content
    fn extract_text_from_docx_xml(xml: &str) -> String {
        let mut text = String::new();
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut buf = Vec::new();
        let mut in_text = false;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(ref e)) => {
                    // w:t is the text element in DOCX
                    if e.name().as_ref() == b"w:t" {
                        in_text = true;
                    }
                    // w:p is paragraph - add newline after each
                    if e.name().as_ref() == b"w:p" && !text.is_empty() {
                        text.push('\n');
                    }
                }
                Ok(quick_xml::events::Event::Text(e)) => {
                    if in_text && let Some(t) = decoded_text(&e) {
                        text.push_str(&t);
                    }
                }
                Ok(quick_xml::events::Event::End(ref e)) if e.name().as_ref() == b"w:t" => {
                    in_text = false;
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        text.trim().to_string()
    }

    /// Extract metadata from DOCX core.xml
    fn extract_metadata_from_core_xml(xml: &str) -> (Option<String>, Option<String>) {
        let mut title = None;
        let mut author = None;
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut buf = Vec::new();
        let mut current_element = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Start(ref e)) => {
                    current_element = String::from_utf8_lossy(e.name().as_ref()).to_string();
                }
                Ok(quick_xml::events::Event::Text(e)) => {
                    if let Some(t) = decoded_text(&e) {
                        match current_element.as_str() {
                            "dc:title" => title = Some(t.to_string()),
                            "dc:creator" => author = Some(t.to_string()),
                            _ => {}
                        }
                    }
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        (title, author)
    }

    /// Parse plain text files
    async fn parse_text(&self, path: &Path, _format: &str) -> Result<(String, ParsedMetadata)> {
        let text = tokio::fs::read_to_string(path)
            .await
            .map_err(ToolError::Io)?;

        let metadata = ParsedMetadata {
            page_count: None,
            title: None,
            author: None,
        };

        Ok((text, metadata))
    }

    /// Parse HTML files
    async fn parse_html(&self, path: &Path) -> Result<(String, ParsedMetadata)> {
        let html = tokio::fs::read_to_string(path)
            .await
            .map_err(ToolError::Io)?;

        // Simple HTML tag removal
        let text = Self::strip_html_tags(&html);

        let metadata = ParsedMetadata {
            page_count: None,
            title: Self::extract_html_title(&html),
            author: None,
        };

        Ok((text, metadata))
    }

    /// Strip HTML tags from content
    pub(crate) fn strip_html_tags(html: &str) -> String {
        let mut text = String::new();
        let mut in_tag = false;
        let mut in_script = false;
        let mut in_style = false;

        let chars: Vec<char> = html.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let c = chars[i];

            if c == '<' {
                in_tag = true;
                // Check for script/style tags
                let remaining: String = chars[i..].iter().take(10).collect();
                if remaining.to_lowercase().starts_with("<script") {
                    in_script = true;
                } else if remaining.to_lowercase().starts_with("<style") {
                    in_style = true;
                } else if remaining.to_lowercase().starts_with("</script") {
                    in_script = false;
                } else if remaining.to_lowercase().starts_with("</style") {
                    in_style = false;
                }
            } else if c == '>' {
                in_tag = false;
            } else if !in_tag && !in_script && !in_style {
                text.push(c);
            }

            i += 1;
        }

        // Clean up whitespace
        let lines: Vec<&str> = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        lines.join("\n")
    }

    /// Extract title from HTML
    pub(crate) fn extract_html_title(html: &str) -> Option<String> {
        let lowercase = html.to_lowercase();
        if let Some(start) = lowercase.find("<title>") {
            let start = start + 7;
            if let Some(end) = lowercase[start..].find("</title>") {
                return html.get(start..start + end).map(|s| s.trim().to_string());
            }
        }
        None
    }

    /// Parse JSON files
    async fn parse_json(&self, path: &Path) -> Result<(String, ParsedMetadata)> {
        let json_text = tokio::fs::read_to_string(path)
            .await
            .map_err(ToolError::Io)?;

        // Pretty print JSON
        let text = match serde_json::from_str::<Value>(&json_text) {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or(json_text),
            Err(_) => json_text,
        };

        let metadata = ParsedMetadata {
            page_count: None,
            title: None,
            author: None,
        };

        Ok((text, metadata))
    }

    /// Parse XML files
    async fn parse_xml(&self, path: &Path) -> Result<(String, ParsedMetadata)> {
        let xml_text = tokio::fs::read_to_string(path)
            .await
            .map_err(ToolError::Io)?;

        // Extract text content from XML
        let mut text = String::new();
        let mut reader = quick_xml::Reader::from_str(&xml_text);
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Text(e)) => {
                    if let Some(t) = decoded_text(&e) {
                        let trimmed = t.trim();
                        if !trimmed.is_empty() {
                            text.push_str(trimmed);
                            text.push('\n');
                        }
                    }
                }
                Ok(quick_xml::events::Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        let metadata = ParsedMetadata {
            page_count: None,
            title: None,
            author: None,
        };

        Ok((text.trim().to_string(), metadata))
    }

    /// Parse spreadsheets: XLSX, XLSM (OOXML), XLSB, XLS (BIFF), ODS.
    ///
    /// calamine 0.38 reads all five natively; the dispatch previously only
    /// routed xlsx/xls to it (#955). XLSM shares the XLSX reader since both
    /// are zipped OOXML packages.
    ///
    /// The three macro-capable formats also get their VBA project dumped
    /// after the sheets (#960). ODS does not: it carries Basic, not VBA.
    async fn parse_excel(&self, path: &Path) -> Result<(String, ParsedMetadata)> {
        let path = path.to_path_buf();

        tokio::task::spawn_blocking(move || {
            use calamine::{Ods, Xls, Xlsb, Xlsx, open_workbook};

            let extension = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();

            let mut output = String::new();

            match extension.as_str() {
                "xlsx" | "xlsm" => {
                    let mut workbook: Xlsx<_> = open_workbook(&path)
                        .map_err(|e| ToolError::Execution(format!("Failed to open XLSX: {}", e)))?;
                    dump_workbook_sheets(&mut workbook, &mut output);
                    vba_modules::append(&mut workbook, &mut output);
                }
                "xlsb" => {
                    let mut workbook: Xlsb<_> = open_workbook(&path)
                        .map_err(|e| ToolError::Execution(format!("Failed to open XLSB: {}", e)))?;
                    dump_workbook_sheets(&mut workbook, &mut output);
                    vba_modules::append(&mut workbook, &mut output);
                }
                "xls" => {
                    let mut workbook: Xls<_> = open_workbook(&path)
                        .map_err(|e| ToolError::Execution(format!("Failed to open XLS: {}", e)))?;
                    dump_workbook_sheets(&mut workbook, &mut output);
                    vba_modules::append(&mut workbook, &mut output);
                }
                "ods" => {
                    let mut workbook: Ods<_> = open_workbook(&path)
                        .map_err(|e| ToolError::Execution(format!("Failed to open ODS: {}", e)))?;
                    dump_workbook_sheets(&mut workbook, &mut output);
                }
                _ => {
                    return Err(ToolError::Execution(format!(
                        "Unsupported Excel format: .{}",
                        extension
                    )));
                }
            }

            let metadata = ParsedMetadata {
                page_count: None,
                title: None,
                author: None,
            };

            Ok((output.trim().to_string(), metadata))
        })
        .await
        .map_err(|e| ToolError::Execution(format!("Excel parsing task failed: {}", e)))?
    }

    /// Parse legacy binary Word documents (.doc, Word 97-2003, OLE/CFB).
    ///
    /// rwml's dependency-light core (default-features off: no docx/zip, no
    /// render stack) extracts the MS-DOC text layer. We keep our own DOCX
    /// parser for the modern format (#955).
    async fn parse_legacy_doc(&self, path: &Path) -> Result<(String, ParsedMetadata)> {
        let path = path.to_path_buf();

        tokio::task::spawn_blocking(move || {
            let bytes = std::fs::read(&path).map_err(ToolError::Io)?;
            let text = rwml::extract_text(&bytes)
                .map_err(|e| ToolError::Execution(format!("Failed to parse legacy DOC: {}", e)))?;

            let metadata = ParsedMetadata {
                page_count: None,
                title: None,
                author: None,
            };

            Ok((text.trim().to_string(), metadata))
        })
        .await
        .map_err(|e| ToolError::Execution(format!("Legacy DOC parsing task failed: {}", e)))?
    }

    /// Parse CSV files
    async fn parse_csv(&self, path: &Path) -> Result<(String, ParsedMetadata)> {
        let csv_text = tokio::fs::read_to_string(path)
            .await
            .map_err(ToolError::Io)?;

        // CSV is already readable, just format it nicely
        let mut output = String::new();
        let lines: Vec<&str> = csv_text.lines().collect();

        // Cap at 1000 rows to prevent context overflow
        let max_rows = 1000;
        let truncated = lines.len() > max_rows;

        for (i, line) in lines.iter().enumerate() {
            if i >= max_rows {
                output.push_str(&format!(
                    "\n... [truncated: {} more rows]",
                    lines.len() - max_rows
                ));
                break;
            }

            // Replace tabs and multiple spaces with pipes for better readability
            let formatted = line.replace('\t', " | ").replace("  ", " | ");

            output.push_str(&formatted);
            output.push('\n');
        }

        let metadata = ParsedMetadata {
            page_count: Some(if truncated { max_rows } else { lines.len() }),
            title: None,
            author: None,
        };

        Ok((output.trim().to_string(), metadata))
    }
}
