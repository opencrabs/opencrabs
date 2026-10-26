//! TUI Application State
//!
//! Core state management for the terminal user interface.

use super::events::{
    AppMode, EventHandler, SshPasswordRequest, SshPasswordResponse, SudoPasswordRequest,
    SudoPasswordResponse, ToolApprovalRequest, ToolApprovalResponse, TuiEvent,
};

/// Live tok/s accumulator for the footer field.
///
/// Tracks ACTIVE streaming time (windows where token deltas are arriving
/// continuously) and stashes the last finalized rate so the footer keeps
/// showing the previous turn's tok/s during idle until the next turn
/// produces its first token. Excludes tool execution waits, approval
/// prompts, between-stream network round-trips — anything more than 1s
/// without a token closes the current window.
///
/// 2026-05-28 user report: pre-tracker the footer divided streaming
/// tokens by full wall-clock elapsed, so a 30s tool exec turned a
/// 200 tok/s burst into "8 tok/s" mid-turn.
#[derive(Debug, Clone, Default)]
pub struct StreamingTpsTracker {
    /// Accumulated active streaming seconds for the current turn.
    active_secs: f64,
    /// Instant the current open window started.
    window_start: Option<std::time::Instant>,
    /// Instant of the most recent advance() call (= last token event).
    last_token_at: Option<std::time::Instant>,
    /// Last finalized tok/s value. Persists across idle until the next
    /// finalize().
    pub last_tps: Option<f64>,
}

impl StreamingTpsTracker {
    /// A gap longer than this between consecutive token events closes
    /// the current active window. Tool execution, approval waits, and
    /// between-response network round-trips all exceed this threshold,
    /// so they're correctly excluded from the rate denominator.
    const IDLE_GAP_SECS: f64 = 1.0;

    /// Call on every `StreamingOutputTokens` event after incrementing
    /// the token counter. `now` is `Instant::now()` (parameterized so
    /// tests can drive the clock deterministically).
    pub fn advance(&mut self, now: std::time::Instant) {
        match (self.window_start, self.last_token_at) {
            (None, _) => {
                // First token this turn — open a window starting now.
                self.window_start = Some(now);
            }
            (Some(start), Some(last)) => {
                if (now - last).as_secs_f64() > Self::IDLE_GAP_SECS {
                    // Idle gap — close the prior window and open a new one.
                    self.active_secs += (last - start).as_secs_f64();
                    self.window_start = Some(now);
                }
                // else: still inside the active window.
            }
            (Some(_), None) => {
                // window_start without last_token: shouldn't normally
                // happen but be safe — reset window start to `now`.
                self.window_start = Some(now);
            }
        }
        self.last_token_at = Some(now);
    }

    /// Total active seconds so far, including the currently-open
    /// window if any. Window time is always measured between window
    /// start and the LAST token event — never extended to `now`, so
    /// idle ticks past the last token don't inflate the rate.
    pub fn active_secs_now(&self, _now: std::time::Instant) -> f64 {
        let in_flight = match (self.window_start, self.last_token_at) {
            (Some(start), Some(last)) => (last - start).as_secs_f64().max(0.0),
            _ => 0.0,
        };
        self.active_secs + in_flight
    }

    /// Stash the just-finished turn's rate as `last_tps` and reset the
    /// accumulator for the next turn. A turn with zero tokens leaves
    /// `last_tps` untouched (preserves the previous visible rate).
    ///
    /// `authoritative` is the agent service's computed tok/s from
    /// `AgentResponse.tokens_per_second` — provider-reported output
    /// tokens divided by summed per-iteration active streaming time.
    /// When present, it overrides the local tiktoken-estimated rate
    /// because (a) tiktoken's cl100k_base tokenizer over-counts
    /// Qwen/Kimi/GLM bytes by ~1.5-2×, and (b) the local window
    /// covers only the visible final-message streaming, not earlier
    /// tool-call-iteration streaming. When `None` (CLI providers, or
    /// any turn where the streaming layer couldn't measure active
    /// time) we fall back to the local estimate so the footer
    /// continues to show something rather than going blank.
    pub fn finalize(&mut self, total_tokens: u32, authoritative: Option<f64>) {
        let active = match (self.window_start, self.last_token_at) {
            (Some(start), Some(last)) => self.active_secs + (last - start).as_secs_f64().max(0.0),
            _ => self.active_secs,
        };
        if let Some(tps) = authoritative.filter(|t| t.is_finite() && *t > 0.0) {
            self.last_tps = Some(tps);
        } else if total_tokens > 0 && active > 0.0 {
            self.last_tps = Some(total_tokens as f64 / active);
        }
        self.active_secs = 0.0;
        self.window_start = None;
        self.last_token_at = None;
    }
}
use super::onboarding::OnboardingWizard;
use crate::brain::agent::AgentService;
use crate::brain::provider::Provider;
use crate::brain::{BrainLoader, CommandLoader, SelfUpdater, UserCommand};
use crate::db::models::{Message, Session};
use crate::services::{FileService, MessageService, ServiceContext, SessionService};
use crate::tui::pane::PaneManager;
use crate::tui::render::notice::{CLIPBOARD_HINT_TTL, ERROR_TTL, NOTIFICATION_TTL, notice_expired};
use crate::utils::prompt_analyzer::PromptAnalyzer;
use anyhow::Result;
use ratatui::text::Line;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Slash command definition
#[derive(Debug, Clone)]
pub struct SlashCommand {
    pub name: &'static str,
    pub description: &'static str,
}

/// Available slash commands for autocomplete
pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/new",
        description: "Start a new session",
    },
    SlashCommand {
        name: "/cd",
        description: "Change working directory",
    },
    SlashCommand {
        name: "/sessions",
        description: "List all sessions",
    },
    SlashCommand {
        name: "/compact",
        description: "Compact context now",
    },
    SlashCommand {
        name: "/clear",
        description: "Clear context here at no cost; history and title stay",
    },
    SlashCommand {
        name: "/theme",
        description: "List / set / reset the TUI color theme (/theme set dracula)",
    },
    SlashCommand {
        name: "/stop",
        description: "Abort the running turn (same as Esc x2)",
    },
    SlashCommand {
        name: "/plan",
        description: "Enter Plan mode (design a plan for approval)",
    },
    SlashCommand {
        name: "/show-plan",
        description: "Show the current plan state",
    },
    SlashCommand {
        name: "/execute",
        description: "Approve the design plan / retry the checklist seed",
    },
    SlashCommand {
        name: "/discard",
        description: "Discard the live plan",
    },
    SlashCommand {
        name: "/help",
        description: "Show available commands",
    },
    SlashCommand {
        name: "/models",
        description: "Switch model",
    },
    SlashCommand {
        name: "/usage",
        description: "Session usage stats",
    },
    SlashCommand {
        name: "/mission-control",
        description: "Mission control: analytics, activity, inbox & schedule",
    },
    SlashCommand {
        name: "/onboard",
        description: "Run setup wizard",
    },
    // Single-step setup (#1981). The legacy `/onboard:<step>` spellings
    // still resolve but are deliberately not offered here.
    SlashCommand {
        name: "/workspace",
        description: "Workspace settings",
    },
    SlashCommand {
        name: "/channels",
        description: "Setup Telegram, Slack, Discord, WhatsApp, Trello",
    },
    SlashCommand {
        name: "/voice",
        description: "Voice STT/TTS setup",
    },
    SlashCommand {
        name: "/image",
        description: "Image handling setup (vision + generation)",
    },
    SlashCommand {
        name: "/daemon",
        description: "Background service (always-on) setup",
    },
    SlashCommand {
        name: "/brain",
        description: "Brain/persona setup",
    },
    SlashCommand {
        name: "/doctor",
        description: "Run connection health check",
    },
    SlashCommand {
        name: "/cowork",
        description: "Create a Telegram cowork workspace (QR + deep link)",
    },
    SlashCommand {
        name: "/approve",
        description: "Tool approval policy",
    },
    SlashCommand {
        name: "/evolve",
        description: "Download latest release & restart",
    },
    SlashCommand {
        name: "/restart",
        description: "Restart OpenCrabs (resumes this session)",
    },
    SlashCommand {
        name: "/exit",
        description: "Exit OpenCrabs",
    },
    SlashCommand {
        name: "/whisper",
        description: "Speak anywhere, paste to clipboard",
    },
    SlashCommand {
        name: "/skills",
        description: "Browse and run loaded skills",
    },
    SlashCommand {
        name: "/rtk",
        description: "Show RTK token savings statistics",
    },
    SlashCommand {
        name: "/goal",
        description: "Set/track an autonomous goal (/goal <text>, status, pause, resume, clear)",
    },
    SlashCommand {
        name: "/profiles",
        description: "Manage OpenCrabs profiles (list, create, switch, migrate)",
    },
];

/// Commands handled only by the chat channels (Telegram / Discord / Slack), not
/// by the TUI slash dispatcher. They are documented in the `/help` dialog under
/// a CHANNEL COMMANDS heading but deliberately kept OUT of [`SLASH_COMMANDS`] so
/// they never appear in the TUI autocomplete (typing them here would just fall
/// through to "unknown command"). `/respond_to` in particular is per-chat and
/// needs a chat context the TUI does not have.
pub const CHANNEL_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/respond_to",
        description: "Show/switch auto-mention mode (/respond_to <all|mention|auto>)",
    },
    SlashCommand {
        name: "/redact",
        description: "Show/switch scoped secret redaction (/redact <global|group|dm> <on|off>)",
    },
    SlashCommand {
        name: "/rename",
        description: "Rename the current session (/rename <new title>)",
    },
];

/// Approval option selected by the user
#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalOption {
    AllowOnce,
    AllowForSession,
    AllowAlways,
}

/// State of an inline approval request
#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalState {
    Pending,
    Approved(ApprovalOption),
    Denied(String),
}

/// Data for an inline tool approval request embedded in a DisplayMessage
#[derive(Debug, Clone)]
pub struct ApprovalData {
    pub tool_name: String,
    pub tool_description: String,
    pub tool_input: Value,
    pub capabilities: Vec<String>,
    pub request_id: Uuid,
    pub response_tx: mpsc::UnboundedSender<ToolApprovalResponse>,
    pub requested_at: std::time::Instant,
    pub state: ApprovalState,
    /// 0-2, arrow key navigation
    pub selected_option: usize,
    /// V key toggle
    pub show_details: bool,
}

/// State for the /approve policy selector menu
#[derive(Debug, Clone, PartialEq)]
pub enum ApproveMenuState {
    Pending,
    Selected(usize),
}

/// Data for the /approve inline menu
#[derive(Debug, Clone)]
pub struct ApproveMenu {
    /// 0-2
    pub selected_option: usize,
    pub state: ApproveMenuState,
}

/// A file attached to the input (detected from pasted paths). Despite the
/// historical name, this now also covers videos — `is_video` selects which
/// marker the input pipeline emits (`<<VID:>>` vs `<<IMG:>>`).
#[derive(Debug, Clone)]
pub struct ImageAttachment {
    /// Display name (file name)
    pub name: String,
    /// Full path to the image or video
    pub path: String,
    /// True when the attachment is a video — tells the send pipeline to
    /// emit `<<VID:>>` so the agent calls `analyze_video` instead of
    /// `analyze_image`.
    pub is_video: bool,
}

/// Image file extensions for auto-detection
/// Hint text shown while an unpasted screenshot sits in the clipboard (#1816).
pub(crate) const CLIPBOARD_HINT_TEXT: &str = "📋 Image in clipboard - Ctrl+V to paste";
/// Minimum spacing between clipboard probes on the tick loop (#1816).
pub(crate) const CLIPBOARD_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

pub(crate) const IMAGE_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".jfif", ".gif", ".webp", ".bmp", ".svg", ".heic", ".heif", ".avif",
    ".ico", ".tiff", ".tif",
];

/// Video file extensions for auto-detection — must match the MIME table in
/// `utils::file_extract::mime_from_ext` so channels and TUI agree on what
/// counts as a video.
pub(crate) const VIDEO_EXTENSIONS: &[&str] = &[
    ".mp4", ".m4v", ".mov", ".webm", ".mkv", ".avi", ".3gp", ".flv",
];

/// Audio file extensions for auto-detection (#1743): surface the path with
/// a transcription hint: the TUI never inlines or transcribes audio
/// itself, the agent decides what to do with it.
pub(crate) const AUDIO_EXTENSIONS: &[&str] = &[
    ".mp3", ".wav", ".m4a", ".aac", ".ogg", ".oga", ".flac", ".opus", ".wma", ".aiff", ".aif",
];

/// Archive extensions for auto-detection (#1743): pointer-surfaced, never
/// extracted by the TUI. `.dmg`/`.iso`/`.pkg` are disk images or installers,
/// not data archives; they still surface as paths so the agent can decide.
pub(crate) const ARCHIVE_EXTENSIONS: &[&str] = &[
    ".zip", ".tar", ".gz", ".tgz", ".bz2", ".tbz2", ".xz", ".txz", ".7z", ".rar", ".dmg", ".iso",
    ".pkg",
];

/// Text file extensions for auto-detection (paste a path → inline content)
pub(crate) const TEXT_EXTENSIONS: &[&str] = &[
    ".txt",
    ".md",
    ".rst",
    ".log",
    ".json",
    ".yaml",
    ".yml",
    ".toml",
    ".xml",
    ".csv",
    ".tsv",
    ".js",
    ".mjs",
    ".ts",
    ".py",
    ".rb",
    ".sh",
    ".rs",
    ".go",
    ".java",
    ".c",
    ".cpp",
    ".h",
    ".html",
    ".htm",
    ".css",
    ".sql",
    // #1743: the most common languages and configs missing from the table.
    ".swift",
    ".dart",
    ".zig",
    ".tsx",
    ".jsx",
    ".kt",
    ".kts",
    ".scala",
    ".lua",
    ".php",
    ".pl",
    ".ex",
    ".exs",
    ".elm",
    ".clj",
    ".erl",
    ".nim",
    ".vue",
    ".svelte",
    ".astro",
    ".graphql",
    ".proto",
    ".ini",
    ".cfg",
    ".conf",
    ".env",
    ".properties",
    ".bat",
    ".ps1",
    ".zsh",
    ".diff",
    ".patch",
    ".ipynb",
    ".lock",
    ".gitignore",
];

/// Document extensions for auto-detection (paste a path → the agent is given
/// the path and told to call `parse_document` / `pdf_to_images`). Binary
/// formats, so we never inline their bytes — only surface the path.
pub(crate) const DOC_EXTENSIONS: &[&str] = &[
    ".pdf", ".docx", ".doc", ".pptx", ".xlsx", ".xlsb", ".xlsm", ".ods", ".epub",
    // #1743: legacy and iWork formats the classifier silently ignored.
    ".xls", ".ppt", ".rtf", ".odt", ".odp", ".pages", ".numbers", ".key",
];

/// A single tool call entry within a grouped display
#[derive(Debug, Clone)]
pub struct ToolCallEntry {
    pub description: String,
    pub success: bool,
    pub details: Option<String>,
    /// Whether the tool has finished executing
    pub completed: bool,
    /// Full raw tool input — shown untruncated in expanded view
    pub tool_input: serde_json::Value,
    /// #2146: the id of the detached task this row tracks, when the row is a
    /// detached-task row rather than a tool call. A finish event finds its row
    /// by this id and not by description, because two detached tasks can carry
    /// the same label and a description match would settle the wrong row.
    pub task_id: Option<String>,
}

/// A group of tool calls displayed as a collapsible bullet
#[derive(Debug, Clone)]
pub struct ToolCallGroup {
    pub calls: Vec<ToolCallEntry>,
    pub expanded: bool,
}

/// What a left-click on a message row should do. Resolved from the message
/// under the cursor so a click on an expandable block toggles just that block
/// (mirroring Ctrl+O, but scoped to one message) while plain text rows keep
/// the highlight-select behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickAction {
    /// Toggle the message's grouped tool-call `expanded` flag.
    ToggleToolGroup,
    /// Toggle the message's reasoning-`details` `expanded` flag.
    ToggleDetails,
    /// Not an expandable block — fall back to highlight-select.
    Select,
}

/// Display message for UI rendering
#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub id: Uuid,
    pub role: String,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub token_count: Option<i64>,
    pub cost: Option<f64>,
    pub approval: Option<ApprovalData>,
    pub approve_menu: Option<ApproveMenu>,
    /// Collapsible details (tool output, etc.) — shown when expanded
    pub details: Option<String>,
    /// Whether details are currently expanded (capped OR full)
    pub expanded: bool,
    /// Whether the expanded reasoning is shown in FULL (uncapped). Only
    /// meaningful when `expanded` is true (#727): the reasoning expands in a
    /// 3-state cycle collapsed -> capped -> full -> collapsed.
    pub expanded_full: bool,
    /// Grouped tool calls (for role == "tool_group")
    pub tool_group: Option<ToolCallGroup>,
    /// Wall-clock seconds the turn took, stamped at turn end and persisted
    /// (#964). Only ever set on assistant rows. `None` on rows written before
    /// the column existed, which fall back to the timestamp subtraction.
    pub duration_secs: Option<i64>,
}

impl DisplayMessage {
    /// Classify what a left-click on this message should do. A tool group takes
    /// precedence over reasoning details (a message carries at most one in
    /// practice); anything else falls through to highlight-select.
    pub fn click_action(&self) -> ClickAction {
        if self.tool_group.is_some() {
            ClickAction::ToggleToolGroup
        } else if self.details.is_some() {
            ClickAction::ToggleDetails
        } else {
            ClickAction::Select
        }
    }

    /// Advance the reasoning details through the 3-state expand cycle (#727):
    /// collapsed -> capped -> full -> collapsed. Driven by a click on the block
    /// or Ctrl+O.
    pub fn cycle_details_expand(&mut self) {
        match (self.expanded, self.expanded_full) {
            (false, _) => {
                self.expanded = true;
                self.expanded_full = false;
            }
            (true, false) => self.expanded_full = true,
            (true, true) => {
                self.expanded = false;
                self.expanded_full = false;
            }
        }
    }
}

impl From<Message> for DisplayMessage {
    fn from(msg: Message) -> Self {
        Self {
            id: msg.id,
            role: msg.role,
            content: msg.content,
            timestamp: msg.created_at,
            token_count: msg.token_count,
            cost: msg.cost,
            approval: None,
            approve_menu: None,
            details: msg.thinking,
            expanded: false,
            expanded_full: false,
            tool_group: None,
            duration_secs: msg.duration_secs,
        }
    }
}

/// Main application state
pub struct App {
    /// Core state
    pub current_session: Option<Session>,
    pub messages: Vec<DisplayMessage>,
    /// Index into `messages` where the in-flight turn begins.
    ///
    /// Cancel-persist reconstruction must never read above this line. Rows
    /// above it are already durable: they were loaded from the DB by
    /// `load_session`, or written by an earlier turn in this process. Re-
    /// collecting them made a turn aborted during boot resume write the whole
    /// previous turn into the fresh placeholder row, which then rendered as a
    /// second marker-less copy of history (#2127).
    ///
    /// Set at every turn start (user send, system trigger, boot resume,
    /// background-task resume) and whenever a session is (re)loaded.
    pub(crate) turn_start_len: usize,
    pub sessions: Vec<Session>,
    /// All-time usage stats from the ledger (survives session deletes)
    pub usage_ledger_stats: Vec<crate::db::repository::usage_ledger::ModelUsageStats>,
    /// Usage dashboard state (populated when /usage is opened)
    pub dashboard_state: Option<crate::usage::dashboard::DashboardState>,

    /// UI state
    pub mode: AppMode,
    pub input_buffer: String,
    /// Cursor position within input_buffer (byte offset, always on a char boundary)
    pub cursor_position: usize,
    /// Images attached to the current input (auto-detected from pasted paths)
    pub attachments: Vec<ImageAttachment>,
    /// When Some, an attachment is focused (Up/Down to navigate, Backspace/Delete to remove).
    /// Index into `attachments`. None means input text is focused.
    pub focused_attachment: Option<usize>,
    pub scroll_offset: usize,
    /// Previous frame's total rendered line count — used to adjust scroll_offset
    /// when streaming adds lines while user is scrolled up, preventing the view
    /// from drifting downward one line per chunk.
    pub prev_rendered_lines: usize,
    /// When true, new streaming content auto-scrolls to bottom.
    /// Set to false when user scrolls up; re-enabled when they scroll back to bottom or send a message.
    pub auto_scroll: bool,
    /// When false, the runner releases terminal mouse capture so the user can
    /// drag-select text natively (terminal handles selection + clipboard).
    /// Toggled with F12. Defaults to true (mouse capture on).
    pub mouse_capture_enabled: bool,
    /// Interactive theme picker (#1371). `Some` = dialog open; all keys are
    /// intercepted and routed through the picker's pure state machine.
    pub theme_picker: Option<crate::tui::render::theme_picker::ThemePickerState>,
    pub selected_session_index: usize,
    pub should_quit: bool,
    /// Pending resize dimensions — runner pre-resizes buffers to avoid blink
    pub pending_resize: Option<(u16, u16)>,

    /// Streaming state
    pub is_processing: bool,
    pub processing_started_at: Option<std::time::Instant>,
    pub streaming_response: Option<String>,
    /// Cached parsed markdown lines for the streaming response — avoids
    /// re-parsing the entire growing buffer on every render frame.
    /// Invalidated when streaming_response length changes.
    pub(crate) streaming_render_cache: Option<(usize, Vec<ratatui::text::Line<'static>>)>,
    /// Reasoning/thinking content from providers like MiniMax (display-only, cleared on complete)
    pub streaming_reasoning: Option<String>,
    pub error_message: Option<String>,
    /// When error_message was set — used to auto-dismiss after 2.5s
    pub error_message_shown_at: Option<std::time::Instant>,
    /// Transient notification (non-error, e.g. "Copied to clipboard")
    pub notification: Option<String>,
    pub notification_shown_at: Option<std::time::Instant>,
    /// Image-in-clipboard hint shown inline in the input area like the other notices (#1816)
    pub clipboard_hint: Option<String>,
    /// When clipboard_hint was set, for TTL expiry on the tick
    pub clipboard_hint_shown_at: Option<std::time::Instant>,
    /// Fingerprint of the clipboard content the probe last saw, so a fresh
    /// screenshot re-arms the hint while the same image stays quiet
    pub clipboard_hint_fp: Option<u64>,
    /// Set by an image paste: this clipboard content stops hinting until it changes
    pub clipboard_hint_suppressed: bool,
    /// Last clipboard probe instant, gates the probe cooldown
    pub clipboard_probe_at: Option<std::time::Instant>,
    /// Terminal focus from FocusGained/FocusLost; probes run only when focused
    pub window_focused: bool,
    /// Currently selected message index (left-click to select, right-click to copy)
    pub selected_message_idx: Option<usize>,
    /// Set to true when IntermediateText arrives during the current response cycle.
    /// Reset to false at the start of each new send_message call.
    /// Used in complete_response to avoid double-adding the assistant message.
    pub(crate) intermediate_text_received: bool,

    /// Rolling build output lines (last 6, cleared on RestartReady)
    pub(crate) build_lines: Vec<String>,
    /// Index of the build-progress DisplayMessage (updated in place)
    pub(crate) build_msg_idx: Option<usize>,

    /// Animation state
    pub animation_frame: usize,

    /// Escape confirmation state (double-press to clear)
    pub(crate) escape_pending_at: Option<std::time::Instant>,

    /// Ctrl+C confirmation state (first clears input, second quits)
    pub(crate) ctrl_c_pending_at: Option<std::time::Instant>,

    /// Ctrl+C expanded per-dialog command panel (#1775): open flag plus
    /// the scope it was opened for. Modal — while open it consumes every
    /// key until dismissed (Esc/q/Ctrl+C). `active_dialog_scope()` going
    /// None (the dialog closed on its own) closes it at render time.
    pub(crate) dialog_help_open: bool,
    pub(crate) dialog_help_scope: super::dialog_keys::DialogScope,

    /// Help/Settings scroll offset
    pub help_scroll_offset: usize,
    /// Command catalogue for the help screen, collected once on entry (#1530).
    /// Built here rather than in the renderer because the renderer ran
    /// `load_all_skills()` and a `commands.toml` parse on every frame.
    pub help_catalog: Vec<super::help_catalog::HelpRow>,
    /// Active help-screen filter text. Empty means "show everything", so an
    /// unfiltered screen and a cleared filter take the same path.
    pub help_search: String,
    /// Whether keystrokes are going into the help filter rather than
    /// scrolling. Mirrors `session_search_active` so the two screens behave
    /// the same way.
    pub help_search_active: bool,
    /// Rows the help screen last rendered, and the height it had to show them
    /// in. Written by the renderer, read by the key handler to clamp scroll —
    /// without them `help_scroll_offset` had no ceiling and wound off the end
    /// of the content into blank space.
    pub help_content_rows: usize,
    pub help_viewport_rows: usize,
    /// Scroll offset for the Plan overlay (Editing design .md viewer).
    pub plan_overlay_scroll: usize,

    /// Model name for display (from provider default)
    pub default_model_name: String,

    /// Approval policy state
    pub approval_auto_session: bool,
    pub approval_auto_always: bool,

    /// File picker state
    pub file_picker_files: Vec<std::path::PathBuf>,
    pub file_picker_filtered: Vec<usize>,
    pub file_picker_selected: usize,
    pub file_picker_scroll_offset: usize,
    pub file_picker_current_dir: std::path::PathBuf,
    /// Whether the directory/file picker shows dotfiles (hidden entries).
    /// Off by default (clean view, like Finder); toggled with `.` in the picker.
    pub file_picker_show_hidden: bool,
    pub file_picker_search: String,
    /// True when `file_picker_files` holds a recursive walk of the working
    /// directory rather than a flat listing of `file_picker_current_dir`.
    /// Flips on once the search query reaches 2 characters and flips off
    /// when the query falls back below the threshold or the picker reopens.
    pub file_picker_recursive: bool,

    /// Slash autocomplete state
    pub slash_suggestions_active: bool,
    /// Indices into SLASH_COMMANDS
    pub slash_filtered: Vec<usize>,
    pub slash_selected_index: usize,

    /// Emoji picker state
    pub emoji_picker_active: bool,
    /// (emoji char, shortcode) pairs matching current query
    pub emoji_filtered: Vec<(&'static str, &'static str)>,
    pub emoji_selected_index: usize,
    /// Byte offset in input_buffer where the `:` trigger starts
    pub emoji_colon_offset: usize,

    /// Optional follow-up suggestions surfaced by the `suggest_options` tool.
    /// One suggestion renders as ghost text in the input (Tab fills it); several
    /// render as a pick-list. Accepting fills the input as editable text (never
    /// submits). Cleared when the user types, submits, or a new turn starts.
    pub followup_suggestions: Vec<String>,
    /// Selected row in the multi-suggestion pick-list.
    pub followup_selected_index: usize,

    /// Session rename state
    pub session_renaming: bool,
    pub session_rename_buffer: String,

    /// Session search/filter state (activated with `/` in /sessions)
    pub session_search: String,
    pub session_search_active: bool,
    /// Viewport scroll offset for session list (first visible session index)
    pub sessions_scroll: usize,

    /// Model selector state (shared with onboarding via ProviderSelectorState)
    pub ps: crate::tui::provider_selector::ProviderSelectorState,

    /// Input history (arrow up/down to cycle through past messages)
    pub(crate) input_history: Vec<String>,
    /// None = not browsing, Some(i) = viewing history[i]
    pub(crate) input_history_index: Option<usize>,
    /// Saves current input when entering history
    pub(crate) input_history_stash: String,

    /// Working directory
    pub working_directory: std::path::PathBuf,

    /// Context hints queued by UI actions (e.g. /cd, @ file picker), keyed by
    /// the session they belong to. Drained and prepended to that session's
    /// next user message so the LLM knows what just happened without the user
    /// having to explain. App-level storage let one session's `/cd` hint ride
    /// into whichever session sent next (#2006).
    pub(crate) pending_context: std::collections::HashMap<Uuid, Vec<String>>,

    /// Editor handoff request (#1744): `(command, origin_session_id)` set by
    /// the bang allowlist branch, consumed by the runner loop (it owns the
    /// terminal + event reader and can suspend them for a full-screen child).
    /// Unix only (#1755): the editor module does not exist on Windows.
    #[cfg(unix)]
    pub pending_editor_handoff: Option<(String, Uuid)>,

    /// Brain state
    pub brain_path: PathBuf,
    pub user_commands: Vec<UserCommand>,
    /// Loaded skills (built-in + user overlay) — auto-registered as `/<name>`
    /// in the slash autocomplete and dispatched as `action=prompt`.
    pub skills: Vec<crate::brain::skills::Skill>,

    /// Mission Control state — focused panel, selection, scroll, popup.
    /// Single struct so `AppState` only carries one MC field; everything
    /// else (input, actions) lives in `tui::app::mission_control`.
    pub mc: crate::tui::app::mission_control::McState,
    /// Skills dialog state — filter buffer, selection, scroll. Same
    /// "single-struct field" pattern as MC.
    pub skills_dialog: crate::tui::app::skills_dialog::SkillsDialogState,
    /// Profiles dialog state — filter, selection, scroll. Same pattern.
    pub profiles_dialog: crate::tui::app::profiles_dialog::ProfilesDialogState,

    /// Dispatcher-level mouse-report fragment gate (#1983): fed at the top
    /// of `handle_key_event` so every mode inherits burst suppression.
    pub(crate) mouse_frag_gate: crate::tui::app::mouse_frag::MouseFragGate,

    /// Onboarding wizard state
    pub onboarding: Option<OnboardingWizard>,
    pub force_onboard: bool,
    /// Set when this launch is a first run. `initialize_async` uses it to
    /// provision the built-in provider and greet the user, with no wizard:
    /// straight to chat is the design, and the wizard is opt-in (#2079).
    pub(crate) first_run: bool,

    /// Sessions currently processing (have in-flight agent tasks)
    pub(crate) processing_sessions: HashSet<Uuid>,
    /// Per-session cancel tokens
    pub(crate) session_cancel_tokens: HashMap<Uuid, CancellationToken>,
    /// Sessions that completed while user was in a different session (unread responses)
    pub(crate) sessions_with_unread: HashSet<Uuid>,
    /// Sessions being processed by a remote channel (Telegram, etc.) — prevents
    /// concurrent TUI sends on the same session and avoids blocking reload loops.
    pub(crate) channel_processing_sessions: HashSet<Uuid>,
    /// Pending session refresh from remote channel — debounced to avoid blocking
    /// the event loop with rapid DB queries during multi-tool runs.
    pub(crate) pending_session_refresh: Option<(Uuid, std::time::Instant)>,
    /// Sessions that have pending approval requests waiting
    pub(crate) sessions_with_pending_approval: HashSet<Uuid>,
    /// Cached provider instances keyed by provider name (e.g., "anthropic", "custom:nvidia")
    pub(crate) provider_cache: HashMap<String, Arc<dyn Provider>>,

    /// Cancellation token for aborting in-progress requests
    pub(crate) cancel_token: Option<CancellationToken>,

    /// Abort handles for in-flight agent tasks, keyed by the session that owns
    /// them. Keyed rather than a single field so cancelling one session can
    /// never kill another session's turn: the old app-level `Option` was
    /// overwritten by whichever task started last, so a switch followed by a
    /// double-Escape aborted a turn the user was not even looking at (#2155).
    pub(crate) task_abort_handles: std::collections::HashMap<Uuid, tokio::task::AbortHandle>,

    /// Per-session queued message stack — single source of truth for both
    /// the UI's "Queued" indicator AND the agent's mid-tool injection
    /// callback. Wrapped in `Arc<std::sync::Mutex<...>>` so the agent task
    /// (in a separate tokio task, only holding a callback closure) can
    /// read/drain the same map the TUI writes into.
    ///
    /// Per-session keying is load-bearing: prior to 2026-04-27 the agent
    /// inject path used a separate `Arc<Mutex<Option<String>>>` slot
    /// shared across ALL sessions, so a message queued in pane A could
    /// be consumed by pane B's agent loop and end up in the wrong
    /// session's chat. Keying by session id makes that impossible — a
    /// callback for session B can't see session A's queue because the
    /// lookup key is the caller's own session id.
    ///
    /// Lock is std::sync (not tokio): held only for HashMap read/write,
    /// never across an `.await`. The `!Send` guard makes "hold across
    /// await" a compile error, not a deadlock at runtime.
    /// Per-session queue of messages waiting for the next tool-loop boundary.
    ///
    /// Holds `QueuedUserMessage`, not `String`, so synthetic entries keep the
    /// compact text that persists and displays. A bare string queue forced the
    /// drain to treat every entry as plain, which published a background
    /// task's whole `[System: ...]` scaffolding into the transcript (#765).
    pub(crate) queued_messages:
        Arc<std::sync::Mutex<HashMap<Uuid, Vec<crate::brain::agent::QueuedUserMessage>>>>,

    /// Shared session ID — channels (Telegram, WhatsApp) read this to use the same session
    pub(crate) shared_session_id: Arc<tokio::sync::Mutex<Option<Uuid>>>,

    /// Context window tracking — FOCUSED SESSION view. These are derived
    /// from `session_input_tokens` / `session_context_max` on load_session
    /// so split-pane / sessions-list navigation shows the right numbers
    /// for whichever pane is focused. Agent events for a NON-focused
    /// session write to the maps only; they don't touch these fields.
    pub context_max_tokens: u32,
    pub last_input_tokens: Option<u32>,
    /// Per-session ctx indicators. Keyed by session_id so that a
    /// background turn's token updates don't leak into the other pane's
    /// ctx display. Without isolation, two panes racing to update
    /// `last_input_tokens` showed each other's numbers — the user saw
    /// "150K" on pane A sourced from pane B's long turn.
    pub session_input_tokens: HashMap<Uuid, u32>,
    pub session_context_max: HashMap<Uuid, u32>,
    /// Per-response output token count (streaming, counted via tiktoken)
    pub streaming_output_tokens: u32,

    /// Live tok/s accumulator + persisted last rate. Excludes idle time
    /// (tool execution, between-stream waits) so the footer shows actual
    /// model speed rather than total turn duration. See
    /// [`StreamingTpsTracker`].
    pub tps_tracker: StreamingTpsTracker,

    /// Active tool call group (during processing)
    pub active_tool_group: Option<ToolCallGroup>,

    /// Version string when an update is available (shown in update prompt dialog)
    pub update_available_version: Option<String>,

    /// Session to resume after restart (set via --session CLI arg)
    pub resume_session_id: Option<Uuid>,

    /// Cache of rendered lines per message to avoid re-parsing markdown every frame.
    /// Key: (message_id, content_width). Invalidated on terminal resize, and on
    /// a theme switch via [`render_cache_theme_gen`].
    pub render_cache: HashMap<(Uuid, u16), Vec<Line<'static>>>,

    /// `theme::generation()` the cached lines above were styled under.
    ///
    /// The cache stores `Line`s whose spans already carry resolved colours, so
    /// a live `/theme` switch cannot reach them: the widgets that call
    /// `theme::role` every frame repaint, every already-rendered message keeps
    /// the old palette. `render_chat` compares this against the live
    /// generation and drops both caches when it moves (#1634).
    pub render_cache_theme_gen: u64,

    /// Mapping from rendered line index → message index (for click-to-copy).
    /// Updated each frame by render_chat.
    pub chat_line_to_msg: Vec<Option<usize>>,
    /// Rendered line → the turn whose HEADER that line is (#758). Lets a click
    /// on a turn header toggle the whole turn without disturbing the
    /// line→message routing everything else uses. Rebuilt each frame.
    pub chat_line_to_turn: Vec<Option<String>>,
    /// Explicit per-turn expand overrides, keyed by [`turn_anchor`] (#758).
    /// Not a Uuid: live rows mint a random one and a reload stamps the database
    /// id, so a UUID key never survives a session switch and every opened turn
    /// came back folded (#2155). Not an index either, because indices shift as
    /// messages arrive. Absent = the default, which is folded.
    ///
    /// [`turn_anchor`]: crate::tui::render::chat::turn_anchor
    pub turn_expanded: std::collections::HashMap<String, bool>,
    /// The scroll offset used during the last render (for coordinate mapping)
    pub chat_render_scroll: usize,
    /// The top-left Y coordinate of the chat area in the terminal
    pub chat_area_y: u16,
    /// The top-left X coordinate of the chat area in the terminal
    pub chat_area_x: u16,
    /// The width of the chat area in the terminal
    pub chat_area_width: u16,
    /// The height of the chat area in the terminal
    pub chat_area_height: u16,
    /// Plain-text snapshot of rendered chat lines (for drag-select text extraction).
    /// Indexed by logical line (matches `chat_line_to_msg` / `chat_render_scroll`).
    pub chat_rendered_lines: Vec<String>,
    /// Pending scroll anchor for a click/ctrl+o expand-collapse (#728):
    /// `(message index, screen row where its header sits)`. The next
    /// `render_chat` pins that message's first line back to that screen row so
    /// expanding a block grows it in place instead of jumping the viewport.
    /// Consumed (cleared) on the render that applies it.
    pub chat_expand_anchor: Option<(usize, u16)>,
    /// The same pin for a turn header: `(turn anchor, screen row)`.
    /// Separate from `chat_expand_anchor` because a header line maps to no
    /// message. It populates `chat_line_to_turn`, not `chat_line_to_msg`, so
    /// the message-keyed anchor above can never resolve it and folding a turn
    /// would jump the viewport by however many rows it hid.
    pub chat_expand_anchor_turn: Option<(String, u16)>,
    /// Set whenever a click or ctrl+o toggles a block, even when the header sits
    /// above the viewport and there is no row to pin (#2241). Growth
    /// compensation is suppressed for that frame: the block's new lines land
    /// where the user is looking, not appended below the viewport, so
    /// compensating would shift the transcript by the number of lines it
    /// gained. Consumed (cleared) on the render that applies it.
    pub chat_expand_growth_pending: bool,

    /// Drag selection: anchor point in terminal-screen coords (col, row), set on first drag event.
    pub drag_anchor: Option<(u16, u16)>,
    /// Drag selection: current point in terminal-screen coords (col, row), updated during drag.
    pub drag_current: Option<(u16, u16)>,

    /// Input area screen coordinates (set each render frame)
    pub input_area_x: u16,
    pub input_area_y: u16,
    pub input_area_width: u16,
    pub input_area_height: u16,
    /// Input drag selection state
    pub input_drag_anchor: Option<(u16, u16)>,
    pub input_drag_current: Option<(u16, u16)>,
    pub input_drag_selecting: bool,

    /// History paging — how many DB messages are hidden above the current view
    pub hidden_older_messages: usize,
    pub oldest_displayed_sequence: i32,
    pub display_token_count: usize,

    /// Pending sudo password request (shown as inline dialog)
    pub sudo_pending: Option<SudoPasswordRequest>,
    /// Raw password text being typed (never displayed, only dots)
    pub sudo_input: String,

    /// Pending SSH password request (shown as inline dialog, same UX as sudo)
    pub ssh_pending: Option<SshPasswordRequest>,
    /// Raw SSH password text being typed (never displayed, only dots)
    pub ssh_input: String,

    /// Active plan document for the current session (loaded from disk)
    pub plan_document: Option<crate::tui::plan::PlanDocument>,

    /// Path to the plan JSON file for the current session
    pub plan_file_path: Option<std::path::PathBuf>,

    /// Split pane manager — tracks pane layout, focus, and per-pane state
    pub pane_manager: PaneManager,
    /// Cached messages for inactive panes (keyed by session_id).
    /// Snapshotted when focus leaves a pane so it can be rendered read-only.
    pub(crate) pane_message_cache: HashMap<Uuid, Vec<DisplayMessage>>,
    /// Per-non-focused-session live state — see
    /// `super::background_session` for the shape and routing model.
    /// Entries are created lazily when an event arrives for a
    /// non-focused session and dropped on `ResponseComplete` or
    /// when the session becomes the focused pane (in which case
    /// the state is promoted into the `AppState` live fields).
    pub(crate) background_sessions:
        HashMap<Uuid, crate::tui::app::background_session::BackgroundSessionState>,

    /// Shared WhatsApp state — single bot instance broadcasts QR/connected events.
    #[cfg(feature = "whatsapp")]
    pub(crate) whatsapp_state: Arc<crate::channels::whatsapp::WhatsAppState>,

    /// Services
    pub(crate) agent_service: Arc<AgentService>,
    pub(crate) session_service: SessionService,
    pub(crate) message_service: MessageService,
    pub(crate) file_service: FileService,

    /// Session files state (populated when F is pressed in /sessions)
    pub(crate) session_files: Vec<crate::db::models::File>,
    pub(crate) selected_file_index: usize,

    /// Projects state (populated when P is pressed in /sessions)
    pub(crate) project_service: crate::services::ProjectService,
    pub(crate) projects: Vec<crate::db::models::Project>,
    pub(crate) selected_project_index: usize,
    pub(crate) project_sessions: Vec<crate::db::models::Session>,
    pub(crate) selected_project_session_index: usize,
    pub(crate) project_detail_view: bool,
    pub(crate) project_name_input: String,
    pub(crate) project_name_input_active: bool,
    /// When set, we're in "assign session to project" mode.
    /// The UUID is the target project_id. The user picks from session list.
    pub(crate) assigning_to_project: Option<uuid::Uuid>,
    /// Cached project names for session list display: project_id -> name
    pub(crate) project_name_cache: std::collections::HashMap<uuid::Uuid, String>,

    /// Events
    pub(crate) event_handler: EventHandler,

    /// Prompt analyzer
    pub(crate) prompt_analyzer: PromptAnalyzer,
}

impl App {
    /// Create a new app instance
    pub fn new(
        agent_service: Arc<AgentService>,
        context: ServiceContext,
        #[cfg(feature = "whatsapp")] whatsapp_state: Arc<crate::channels::whatsapp::WhatsAppState>,
    ) -> Self {
        let brain_path = BrainLoader::resolve_path();
        let command_loader = CommandLoader::from_brain_path(&brain_path);
        let user_commands = command_loader.load();
        let skills = crate::brain::skills::load_all_skills();

        // Load persisted approval policy from config.toml
        let (approval_auto_session, approval_auto_always) =
            Self::read_approval_policy_from_config();

        let this = Self {
            current_session: None,
            messages: Vec::new(),
            turn_start_len: 0,
            sessions: Vec::new(),
            usage_ledger_stats: Vec::new(),
            dashboard_state: None,
            mode: AppMode::Chat,
            input_buffer: String::new(),
            cursor_position: 0,
            attachments: Vec::new(),
            focused_attachment: None,
            scroll_offset: 0,
            prev_rendered_lines: 0,
            auto_scroll: true,
            mouse_capture_enabled: true,
            theme_picker: None,
            selected_session_index: 0,
            should_quit: false,
            pending_resize: None,
            is_processing: false,
            processing_started_at: None,
            streaming_response: None,
            streaming_render_cache: None,
            streaming_reasoning: None,
            error_message: None,
            error_message_shown_at: None,
            notification: None,
            notification_shown_at: None,
            clipboard_hint: None,
            clipboard_hint_shown_at: None,
            clipboard_hint_fp: None,
            clipboard_hint_suppressed: false,
            clipboard_probe_at: None,
            window_focused: true,
            selected_message_idx: None,
            intermediate_text_received: false,
            build_lines: Vec::new(),
            build_msg_idx: None,
            animation_frame: 0,

            escape_pending_at: None,
            ctrl_c_pending_at: None,
            dialog_help_open: false,
            dialog_help_scope: super::dialog_keys::DialogScope::Help,
            help_scroll_offset: 0,
            help_catalog: Vec::new(),
            help_search: String::new(),
            help_search_active: false,
            help_content_rows: 0,
            help_viewport_rows: 0,
            plan_overlay_scroll: 0,
            approval_auto_session,
            approval_auto_always,
            file_picker_files: Vec::new(),
            file_picker_filtered: Vec::new(),
            file_picker_selected: 0,
            file_picker_scroll_offset: 0,
            file_picker_current_dir: crate::utils::cwd::launch_cwd(),
            file_picker_show_hidden: false,
            file_picker_search: String::new(),
            file_picker_recursive: false,
            slash_suggestions_active: false,
            slash_filtered: Vec::new(),
            slash_selected_index: 0,
            emoji_picker_active: false,
            emoji_filtered: Vec::new(),
            emoji_selected_index: 0,
            emoji_colon_offset: 0,
            followup_suggestions: Vec::new(),
            followup_selected_index: 0,
            session_renaming: false,
            session_rename_buffer: String::new(),
            session_search: String::new(),
            session_search_active: false,
            sessions_scroll: 0,
            ps: crate::tui::provider_selector::ProviderSelectorState::default(),
            input_history: Self::load_history(),
            input_history_index: None,
            input_history_stash: String::new(),
            working_directory: crate::utils::cwd::launch_cwd(),
            pending_context: std::collections::HashMap::new(),
            #[cfg(unix)]
            pending_editor_handoff: None,
            brain_path,
            user_commands,
            skills,
            mc: crate::tui::app::mission_control::McState::default(),
            skills_dialog: crate::tui::app::skills_dialog::SkillsDialogState::default(),
            profiles_dialog: crate::tui::app::profiles_dialog::ProfilesDialogState::default(),
            mouse_frag_gate: crate::tui::app::mouse_frag::MouseFragGate::default(),
            onboarding: None,
            force_onboard: false,
            first_run: false,
            processing_sessions: HashSet::new(),
            session_cancel_tokens: HashMap::new(),
            sessions_with_unread: HashSet::new(),
            channel_processing_sessions: HashSet::new(),
            pending_session_refresh: None,
            sessions_with_pending_approval: HashSet::new(),
            provider_cache: HashMap::new(),
            cancel_token: None,
            task_abort_handles: std::collections::HashMap::new(),
            queued_messages: Arc::new(std::sync::Mutex::new(HashMap::new())),
            shared_session_id: Arc::new(tokio::sync::Mutex::new(None)),
            default_model_name: agent_service.provider_model(),
            session_input_tokens: HashMap::new(),
            session_context_max: HashMap::new(),
            context_max_tokens: agent_service
                .context_window_for_model(&agent_service.provider_model()),
            last_input_tokens: None,
            streaming_output_tokens: 0,
            tps_tracker: StreamingTpsTracker::default(),
            active_tool_group: None,
            update_available_version: None,
            resume_session_id: None,
            render_cache: HashMap::new(),
            render_cache_theme_gen: crate::tui::render::theme::generation(),
            chat_line_to_msg: Vec::new(),
            chat_line_to_turn: Vec::new(),
            turn_expanded: std::collections::HashMap::new(),
            chat_expand_anchor: None,
            chat_expand_anchor_turn: None,
            chat_expand_growth_pending: false,
            chat_render_scroll: 0,
            chat_area_y: 0,
            chat_area_x: 0,
            chat_area_width: 0,
            chat_area_height: 0,
            chat_rendered_lines: Vec::new(),
            drag_anchor: None,
            drag_current: None,
            input_area_x: 0,
            input_area_y: 0,
            input_area_width: 0,
            input_area_height: 0,
            input_drag_anchor: None,
            input_drag_current: None,
            input_drag_selecting: false,
            hidden_older_messages: 0,
            oldest_displayed_sequence: 0,
            display_token_count: 0,
            sudo_pending: None,
            sudo_input: String::new(),
            ssh_pending: None,
            ssh_input: String::new(),
            plan_document: None,
            plan_file_path: None,
            pane_manager: PaneManager::load_layout(),
            pane_message_cache: HashMap::new(),
            background_sessions: HashMap::new(),
            #[cfg(feature = "whatsapp")]
            whatsapp_state,
            session_service: SessionService::new(context.clone()),
            message_service: MessageService::new(context.clone()),
            file_service: FileService::new(context.clone()),
            project_service: crate::services::ProjectService::new(context),
            session_files: Vec::new(),
            selected_file_index: 0,
            projects: Vec::new(),
            selected_project_index: 0,
            project_sessions: Vec::new(),
            selected_project_session_index: 0,
            project_detail_view: false,
            project_name_input: String::new(),
            project_name_input_active: false,
            assigning_to_project: None,
            project_name_cache: std::collections::HashMap::new(),
            agent_service,
            event_handler: EventHandler::new(),
            prompt_analyzer: PromptAnalyzer::new(),
        };
        tracing::info!(
            "App created — provider: {} / {}",
            this.agent_service.provider_name(),
            this.agent_service.provider_model(),
        );
        this
    }

    /// Get the provider name
    pub fn provider_name(&self) -> String {
        self.agent_service.provider_name()
    }

    /// Get the provider model
    pub fn provider_model(&self) -> String {
        self.agent_service.provider_model()
    }

    /// Provider name serving the CURRENT session: the per-session override
    /// (including sticky-fallback sub-provider) when a session is loaded,
    /// the global provider otherwise. Display surfaces must use this over
    /// `provider_name()` so they never report a provider that isn't the
    /// one actually serving the session (#1464).
    pub fn provider_name_for_current_session(&self) -> String {
        match self.current_session.as_ref().map(|s| s.id) {
            Some(id) => self.agent_service.provider_name_for_session(id),
            None => self.agent_service.provider_name(),
        }
    }

    /// Model serving the CURRENT session: per-session override / sticky
    /// sub-model when a session is loaded, the global default otherwise
    /// (#1464).
    pub fn provider_model_for_current_session(&self) -> String {
        match self.current_session.as_ref().map(|s| s.id) {
            Some(id) => self.agent_service.provider_model_for_session(id),
            None => self.agent_service.provider_model(),
        }
    }

    /// Check if a session_id matches the currently active session
    pub(crate) fn is_current_session(&self, session_id: Uuid) -> bool {
        self.current_session.as_ref().map(|s| s.id) == Some(session_id)
    }

    /// Returns indices into `self.sessions` that match the current search filter.
    /// When no search is active, returns all indices.
    pub(crate) fn visible_session_indices(&self) -> Vec<usize> {
        if self.session_search.is_empty() {
            (0..self.sessions.len()).collect()
        } else {
            let search = self.session_search.to_lowercase();
            self.sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    let name = s.title.as_deref().unwrap_or("New Chat").to_lowercase();
                    name.contains(&search)
                })
                .map(|(i, _)| i)
                .collect()
        }
    }

    /// Put the picker cursor on the session the user is currently in.
    ///
    /// `selected_session_index` is an index, never an identity, and
    /// `load_sessions` re-queries `ORDER BY updated_at DESC`. Using a session
    /// bumps its `updated_at`, so by the next open it has moved and the stale
    /// index points at whatever slid into that slot — Enter then switches into
    /// the wrong session, which in the reported case was a dormant twin bound
    /// to the same Slack channel (#1465). Anchoring on the session id makes the
    /// cursor follow the session rather than the slot.
    ///
    /// The index is relative to the filtered view, so the position is resolved
    /// through `visible_session_indices()`; a current session that is filtered
    /// out (or absent entirely) falls back to the top of the list.
    pub(crate) fn focus_current_session(&mut self) {
        let Some(current_id) = self.current_session.as_ref().map(|s| s.id) else {
            self.selected_session_index = 0;
            return;
        };
        self.selected_session_index = self
            .visible_session_indices()
            .iter()
            .position(|&i| self.sessions.get(i).is_some_and(|s| s.id == current_id))
            .unwrap_or(0);
    }

    /// Route a per-session mutator to either the foreground
    /// `AppState` fields or the matching background-session sidecar.
    /// Used by the `TuiEvent` handlers in this file so each one is a
    /// one-liner regardless of whether the targeted session happens
    /// to be the focused one.
    ///
    /// Lazily creates a `BackgroundSessionState` entry on first
    /// background hit so handlers don't have to .or_default()
    /// themselves. Cleanup happens in `complete_response_for` (drops
    /// the entry when its turn finalises) and `demote_to_background`
    /// (snapshots foreground state in, drops empty entries).
    /// Whether `session_id` still has a turn in flight.
    ///
    /// `processing_sessions` is the cross-session source of truth: the
    /// foreground `is_processing` flag is derived from it on every focus
    /// switch, so this answers for a focused and an off-screen session alike.
    /// Cancelling removes the entry, which is what makes this the guard that
    /// stops a cancelled turn's queued events being applied (#1342).
    pub(crate) fn is_session_processing(&self, session_id: uuid::Uuid) -> bool {
        self.processing_sessions.contains(&session_id)
    }

    pub(crate) fn session_state_mut(
        &mut self,
        session_id: Uuid,
    ) -> super::background_session::SessionStateMut<'_> {
        if self.is_current_session(session_id) {
            super::background_session::SessionStateMut::Foreground(self)
        } else {
            super::background_session::SessionStateMut::Background(
                self.background_sessions.entry(session_id).or_default(),
            )
        }
    }

    /// Snapshot the current `AppState` live-turn fields into the
    /// background-sessions map for `session_id` (typically the
    /// session that just lost focus). Called from the focus-switch
    /// path BEFORE the new pane's state is loaded so the leaving
    /// session keeps accumulating events while it's off-screen.
    ///
    /// An empty snapshot (no live state) skips insertion to keep
    /// the map bounded.
    pub(crate) fn demote_to_background(&mut self, session_id: Uuid) {
        // Preserve any pending_messages that accumulated while this
        // session was previously in background — they're flushed
        // entries waiting to merge back on the next promote. New
        // demote, same session: extend rather than overwrite.
        let prior_pending = self
            .background_sessions
            .remove(&session_id)
            .map(|prev| prev.pending_messages)
            .unwrap_or_default();
        let bg = super::background_session::BackgroundSessionState {
            streaming_response: self.streaming_response.clone(),
            streaming_reasoning: self.streaming_reasoning.clone(),
            active_tool_group: self.active_tool_group.clone(),
            is_processing: self.is_processing,
            processing_started_at: self.processing_started_at,
            last_input_tokens: self.last_input_tokens,
            streaming_output_tokens: self.streaming_output_tokens,
            display_token_count: self.display_token_count,
            tps_tracker: self.tps_tracker.clone(),
            pending_messages: prior_pending,
        };
        // A cancelled turn has no live state to preserve. Snapshotting it
        // anyway is how a just-aborted session got its streaming fields copied
        // straight back into the sidecar, where `panes.rs` drew them as a
        // running turn for the rest of the session (#1342).
        if bg.has_live_state() && self.is_session_processing(session_id) {
            self.background_sessions.insert(session_id, bg);
        }
    }

    /// Pop the background entry for `session_id` (typically the
    /// session that just gained focus) into the `AppState` live
    /// fields. Returns true when an entry was found and promoted,
    /// false otherwise (caller falls back to the DB-reload path).
    pub(crate) fn promote_to_foreground(&mut self, session_id: Uuid) -> bool {
        let Some(bg) = self.background_sessions.remove(&session_id) else {
            return false;
        };
        self.streaming_response = bg.streaming_response;
        self.streaming_reasoning = bg.streaming_reasoning;
        self.active_tool_group = bg.active_tool_group;
        // NOT `bg.is_processing`. That field is a snapshot taken whenever the
        // session was last demoted, and `processing_sessions` is the
        // cross-session source of truth that every other caller derives from.
        // Restoring the snapshot here overwrote the correct value `load_session`
        // had just computed, so a stale `false` left a still-running session
        // looking idle and its thinking indicator hidden (#1420).
        self.is_processing = self.is_session_processing(session_id);
        self.processing_started_at = bg.processing_started_at;
        self.last_input_tokens = bg.last_input_tokens;
        self.streaming_output_tokens = bg.streaming_output_tokens;
        self.display_token_count = bg.display_token_count;
        self.tps_tracker = bg.tps_tracker;
        // Merge the per-session message delta into the freshly DB-
        // loaded `self.messages`. The caller (`load_session`) has
        // already trimmed `self.messages` to the display budget;
        // pending_messages from the background are the deltas
        // accumulated AFTER that DB snapshot, so they belong at
        // the tail. Each `DisplayMessage` carries its own UUID so
        // a future DB reload that picks up the persisted form
        // won't collide.
        if !bg.pending_messages.is_empty() {
            self.messages.extend(bg.pending_messages);
        }
        true
    }

    /// Set the plan file path for a session and attempt to load it.
    pub(crate) async fn set_plan_file_for_session(&mut self, session_id: Uuid) {
        // Resolve through the shared store so the path is project/profile-aware
        // (and falls back to the legacy flat dir) instead of hardcoding
        // `agents/session/` under a raw `$HOME` that ignores the active profile.
        self.plan_file_path = Some(crate::utils::plan_files::plan_json_read_path(session_id).await);
        self.reload_plan();
    }

    /// Reload the plan document from disk through the shared plan store.
    ///
    /// The loader resolves legacy terminal statuses (Completed archives,
    /// Cancelled deletes) and maps legacy live statuses onto Editing/Active.
    /// Live plans are NEVER discarded here: an idle Active plan (including
    /// a seed-failed empty-task one awaiting retry) and an Editing design
    /// doc both survive reloads and restarts by design.
    pub(crate) fn reload_plan(&mut self) {
        self.plan_document = self
            .plan_file_path
            .as_ref()
            .and_then(|path| crate::utils::plan_files::load_plan_from_path(path));
    }

    /// Get the shared session ID handle (for channels like Telegram/WhatsApp)
    pub fn shared_session_id(&self) -> Arc<tokio::sync::Mutex<Option<Uuid>>> {
        self.shared_session_id.clone()
    }

    /// Fast synchronous init: decide mode (Onboarding vs Chat) and arm the
    /// header card overlay. Does no DB work, so the first frame can render
    /// immediately. The actual session load happens in `initialize_async`.
    ///
    /// Returns `true` if the caller should render a first frame before
    /// running `initialize_async` (i.e. we're going to Chat and want the
    /// card visible while the session loads).
    pub fn initialize_sync(&mut self) -> bool {
        // Remove ghost custom provider entries left by previous saves
        crate::config::Config::cleanup_empty_custom_providers();

        let is_first = super::onboarding::is_first_time();

        match plan_launch(self.force_onboard, is_first) {
            LaunchPlan::Wizard => {
                self.force_onboard = false;
                tracing::info!("[init] Starting onboarding wizard");
                // Resume where the user left off rather than restarting from
                // step one. `resumed()` falls back to a fresh wizard when there
                // is no recorded progress, so an explicit `--onboard` still
                // starts clean.
                let mut wizard = OnboardingWizard::resumed();
                wizard.is_first_time = is_first;
                self.onboarding = Some(wizard);
                self.mode = AppMode::Onboarding;
                false
            }
            LaunchPlan::Chat { first_run } => {
                // A first run has to bring the built-in provider online and
                // say hello. Flagged here, done in `initialize` once the
                // session exists, because the greeting is a message and
                // messages need a session.
                self.first_run = first_run;
                self.mode = AppMode::Chat;
                true
            }
        }
    }

    /// Initialize the app by loading or creating a session
    pub async fn initialize(&mut self) -> Result<()> {
        // Resume a specific session (e.g. after a restart) or load the most recent
        if let Some(session_id) = self.resume_session_id.take() {
            self.load_session(session_id).await?;
            self.mode = AppMode::Chat;
            // Send a hidden wake-up message to the agent (not shown in UI)
            // If we also evolved, merge the evolution context into the same message
            // to avoid sending two separate prompts that produce duplicate responses.
            self.processing_sessions.insert(session_id);
            self.is_processing = true;
            self.processing_started_at = Some(std::time::Instant::now());
            let agent_service = self.agent_service.clone();
            let event_sender = self.event_sender();
            let token = CancellationToken::new();
            self.cancel_token = Some(token.clone());
            let evolution_context = std::env::var("OPENCRABS_EVOLVED_FROM")
                .ok()
                .filter(|old| old != crate::VERSION)
                .map(|old| {
                    // Clear env var so it doesn't fire again
                    // SAFETY: single-threaded at this point in startup
                    unsafe { std::env::remove_var("OPENCRABS_EVOLVED_FROM") };
                    format!(
                        " You just upgraded from v{old} to v{new}. \
                         You're already running the new version, no restart needed. \
                         Run `opencrabs --version` to confirm, then greet the user \
                         and mention one cool thing from the upgrade. Keep it brief.",
                        new = crate::VERSION,
                    )
                })
                .unwrap_or_default();
            tokio::spawn(async move {
                let wake_up = format!(
                    "[System: You just hot-reloaded to a new version. \
                    Greet the user by name if you know it from context \
                    (USER.md or memory), confirm the upgrade, and continue \
                    where you left off.{evolution_context}]"
                );
                match agent_service
                    .send_message_with_tools_and_mode(
                        session_id,
                        wake_up.to_string(),
                        None,
                        Some(token),
                    )
                    .await
                {
                    Ok(response) => {
                        let _ = event_sender.send(TuiEvent::ResponseComplete {
                            session_id,
                            response,
                        });
                    }
                    Err(e) => {
                        let _ = event_sender.send(TuiEvent::Error {
                            session_id,
                            message: e.to_string(),
                        });
                    }
                }
            });
        } else if let Some(last_id) = Self::read_last_session_id()
            && self.session_service.get_session(last_id).await?.is_some()
        {
            self.load_session(last_id).await?;
        } else if let Some(session) = self.session_service.get_most_recent_session().await? {
            self.load_session(session.id).await?;
        } else {
            // Create a new session if none exists
            self.create_new_session().await?;
        }

        tracing::info!(
            "Session loaded — provider: {} / {}, session: {:?}",
            self.provider_name_for_current_session(),
            self.provider_model_for_current_session(),
            self.current_session.as_ref().map(|s| s.id),
        );

        // Zero-setup first run (#2079): bring the built-in provider online and
        // let the agent open the conversation. Done here rather than in
        // `initialize_sync` because the greeting is a message, and a message
        // needs the session that was just created.
        if self.first_run {
            self.provision_first_run().await;
        }

        // Pre-load sessions for restored split panes so they render
        // immediately instead of showing empty until focused.
        if self.pane_manager.is_split() {
            let focused = self.pane_manager.focused;
            let pane_sessions: Vec<Uuid> = self
                .pane_manager
                .panes
                .iter()
                .filter(|p| p.id != focused)
                .filter_map(|p| p.session_id)
                .collect();
            for sid in pane_sessions {
                self.preload_pane_session(sid).await;
            }
        }

        // Load sessions list
        self.load_sessions().await?;

        // Post-evolve fallback: if OPENCRABS_EVOLVED_FROM is still set
        // (e.g. /evolve without resume_session_id), handle it here.
        // Normally this is merged into the wake-up message above.
        if let Ok(old_version) = std::env::var("OPENCRABS_EVOLVED_FROM") {
            unsafe { std::env::remove_var("OPENCRABS_EVOLVED_FROM") };
            if old_version != crate::VERSION && self.current_session.is_some() {
                let msg = format!(
                    "[System: You just upgraded from v{old} to v{new}. \
                     You're already running the new version, no restart needed. \
                     Run `opencrabs --version` to confirm, then greet the user \
                     by name if you know it from context (USER.md or memory), \
                     and mention one cool thing from the upgrade. Keep it brief.]",
                    old = old_version,
                    new = crate::VERSION,
                );
                let tx = self.event_sender();
                let _ = tx.send(TuiEvent::MessageSubmitted(msg));
            }
        }

        // Spawn background release check (immediately on startup, then daily).
        // No initial delay — if an update exists, behavior depends on
        // `agent.auto_update` (default true): silently install + restart.
        // When false, show the UpdatePrompt dialog so the user can confirm.
        {
            let tx = self.event_sender();
            let auto_update = crate::config::Config::load()
                .map(|c| c.agent.auto_update)
                .unwrap_or(true);
            tokio::spawn(async move {
                loop {
                    if let Some(latest) = crate::brain::tools::evolve::check_for_update().await {
                        if auto_update {
                            // Auto-update notice is global (not tied to any
                            // session) — Uuid::nil() bypasses the session
                            // filter in the SystemMessage handler.
                            let _ = tx.send(TuiEvent::SystemMessage {
                                session_id: Uuid::nil(),
                                text: format!("Auto-updating to v{}...", latest),
                            });
                            super::messaging::run_evolve_directly(Uuid::nil(), tx.clone()).await;
                        } else {
                            let _ = tx.send(TuiEvent::UpdateAvailable(latest));
                        }
                    }
                    // Check again in 24 hours
                    tokio::time::sleep(std::time::Duration::from_secs(86400)).await;
                }
            });
        }

        // Notify user if config was recovered from last-known-good snapshot
        let load_status = crate::config::Config::first_load_status();
        if load_status.recovered {
            // Name the cause. `recovery_reason` has been captured since #909
            // precisely so the user is told WHAT failed, and this notice threw
            // it away — leaving people to find a cryptic parse error in the
            // debug log while every config edit silently did nothing (#1116).
            let why = load_status
                .recovery_reason
                .as_deref()
                .unwrap_or("cause not recorded");
            self.push_system_message(format!(
                "🔧 Config failed to load and was recovered from the \
                 last-known-good snapshot, so edits to ~/.opencrabs/config.toml \
                 are NOT in effect until this is fixed.\n\nCause: {why}"
            ));
        }

        // Notify user about unknown config keys (possible typos)
        let typo_warnings = crate::config::Config::take_typo_warnings();
        if !typo_warnings.is_empty() {
            self.push_system_message(format!(
                "⚠️ Unknown keys in config.toml (possible typos): {}",
                typo_warnings.join(", ")
            ));
        }

        // Notify user if DB integrity check failed
        if crate::db::db_integrity_failed() {
            self.push_system_message(
                "⚠️ Database integrity check FAILED — data may be corrupted. \
                 Consider backing up and recreating the database."
                    .to_string(),
            );
        }

        // Notify user if the autonomous RSI loop has filed proposals
        // they haven't reviewed yet. The banner points at the
        // direct-action path (Mission Control's Inbox panel, where
        // `a` applies and `r` rejects) first, then mentions the
        // agent-driven path as a fallback for users who prefer the
        // chat surface.
        let pending = crate::brain::rsi_proposals::ProposalsStore::new().pending_count();
        if pending > 0 {
            self.push_system_message(format!(
                "🔧 RSI proposed {pending} new item(s) (tools / commands / skills). \
                 Open Mission Control with `/mission-control` → Inbox to review \
                 (press `a` to apply, `r` to reject), or ask me to \"show proposed\" \
                 / \"implement proposed\" if you'd rather work from the chat."
            ));
        }

        Ok(())
    }

    /// Get event handler
    pub fn event_handler(&self) -> &EventHandler {
        &self.event_handler
    }

    /// Get mutable event handler
    pub fn event_handler_mut(&mut self) -> &mut EventHandler {
        &mut self.event_handler
    }

    /// Get event sender
    pub fn event_sender(&self) -> tokio::sync::mpsc::UnboundedSender<TuiEvent> {
        self.event_handler.sender()
    }

    /// Set agent service (used to inject configured agent after app creation)
    pub fn set_agent_service(&mut self, agent_service: Arc<AgentService>) {
        self.default_model_name = agent_service.provider_model();
        self.agent_service = agent_service;
    }

    /// Rebuild agent service with a new provider
    pub(crate) async fn rebuild_agent_service(&mut self) -> Result<()> {
        // Load config - API keys are stored in keys.toml and merged with config
        let config = crate::config::Config::load()
            .map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;

        // Check all providers dynamically - log enabled providers for debugging
        let enabled_providers: Vec<&str> = vec![
            config
                .providers
                .anthropic
                .as_ref()
                .filter(|p| p.enabled)
                .map(|_| "anthropic"),
            config
                .providers
                .openai
                .as_ref()
                .filter(|p| p.enabled)
                .map(|_| "openai"),
            config
                .providers
                .gemini
                .as_ref()
                .filter(|p| p.enabled)
                .map(|_| "gemini"),
            config
                .providers
                .openrouter
                .as_ref()
                .filter(|p| p.enabled)
                .map(|_| "openrouter"),
            config
                .providers
                .minimax
                .as_ref()
                .filter(|p| p.enabled)
                .map(|_| "minimax"),
            config
                .providers
                .zai
                .as_ref()
                .filter(|p| p.enabled)
                .map(|_| "zai"),
            config
                .providers
                .moonshot
                .as_ref()
                .filter(|p| p.enabled)
                .map(|_| "moonshot"),
            config.providers.active_custom().map(|_| "custom"),
        ]
        .into_iter()
        .flatten()
        .collect();

        tracing::debug!(
            "rebuild_agent_service: enabled_providers = {:?}",
            enabled_providers
        );

        // Create new provider from config
        let (provider, provider_warning) =
            crate::brain::provider::create_provider_with_warning(&config)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create provider: {}", e))?;

        // Get existing context from current agent service
        let context = self.agent_service.context().clone();

        // Get existing tool registry from current agent service
        let tool_registry = self.agent_service.tool_registry().clone();

        // Get existing system brain from current agent service
        let system_brain = self.agent_service.system_brain().cloned();

        // Carry the live-brain handle forward so brain-file edits keep taking
        // effect after a provider/model rebuild without re-reading disk (#213).
        let brain_rebuild = self.agent_service.brain_rebuild();

        // Get event sender for approval callback
        let event_sender = self.event_sender();

        // Create approval callback that sends requests to TUI
        let approval_callback: crate::brain::agent::ApprovalCallback = Arc::new(move |tool_info| {
            let sender = event_sender.clone();
            Box::pin(async move {
                use crate::tui::events::{ToolApprovalRequest, TuiEvent};
                use tokio::sync::mpsc;

                let (response_tx, mut response_rx) = mpsc::unbounded_channel();

                let request = ToolApprovalRequest {
                    request_id: uuid::Uuid::new_v4(),
                    session_id: tool_info.session_id,
                    tool_name: tool_info.tool_name,
                    tool_description: tool_info.tool_description,
                    tool_input: tool_info.tool_input,
                    capabilities: tool_info.capabilities,
                    response_tx,
                    requested_at: std::time::Instant::now(),
                };

                sender
                    .send(TuiEvent::ToolApprovalRequested(request))
                    .map_err(|e| {
                        crate::brain::agent::AgentError::Internal(format!(
                            "Failed to send approval request: {}",
                            e
                        ))
                    })?;

                let response = response_rx.recv().await.ok_or_else(|| {
                    crate::brain::agent::AgentError::Internal("Approval channel closed".to_string())
                })?;

                // TUI handles "always" internally via approval_auto_session;
                // return false for always_approve so tool_loop doesn't duplicate it
                Ok((response.approved, false))
            })
        });

        // Preserve existing callbacks from the current agent service
        let progress_callback = self.agent_service.progress_callback().clone();
        let message_queue_callback = self.agent_service.message_queue_callback().clone();
        let sudo_callback = self.agent_service.sudo_callback().clone();
        let ssh_callback = self.agent_service.ssh_callback().clone();
        let session_updated_tx = self.agent_service.session_updated_tx();
        let working_dir = self
            .agent_service
            .working_directory()
            .read()
            .expect("working_directory lock poisoned")
            .clone();
        let brain_path = self.agent_service.brain_path().clone();

        // Create new agent service with new provider — preserve ALL callbacks
        let mut new_agent_service = AgentService::new(provider, context, &config)
            .await
            .with_tool_registry(tool_registry)
            .with_approval_callback(Some(approval_callback))
            .with_progress_callback(progress_callback)
            .with_message_queue_callback(message_queue_callback)
            .with_sudo_callback(sudo_callback)
            .with_ssh_callback(ssh_callback)
            .with_working_directory(working_dir)
            .with_auto_approve_tools(self.approval_auto_always);

        // Carry the existing background-task manager and its enqueue route into
        // the rebuilt service (#1504). Without this, a provider switch rebuilds
        // the service without a background manager, and long commands (cargo
        // test/build/clippy, …) stop detaching for every session afterward.
        new_agent_service = new_agent_service.with_existing_background_manager(
            self.agent_service.background_manager(),
            self.agent_service.message_enqueue_callback(),
        );

        if let Some(mgr) = self.agent_service.subagent_manager() {
            new_agent_service = new_agent_service.with_subagent_manager(mgr);
        }

        if let Some(tx) = session_updated_tx {
            new_agent_service = new_agent_service.with_session_updated_tx(tx);
        }

        if let Some(bp) = brain_path {
            new_agent_service = new_agent_service.with_brain_path(bp);
        }

        // Add system brain if it exists
        if let Some(brain) = system_brain {
            new_agent_service = new_agent_service.with_system_brain(brain);
        }

        // Re-attach live brain rebuilding (#213) so /models switches don't
        // revert the agent to a static, startup-frozen brain.
        if let Some(handle) = brain_rebuild {
            new_agent_service = new_agent_service.with_brain_rebuild_handle(handle);
        }

        // Preserve per-session provider entries across the rebuild so
        // other sessions (especially active pane-B turns) don't lose
        // their provider choice when the user reconfigures via /models.
        // Without this, any rebuild silently reverts every session to
        // the new global default.
        let preserved_session_providers = self.agent_service.session_provider_snapshot();
        // Carry each session's locked model so the rebuild restores the
        // exact provider+model pair, not the provider's default.
        let preserved_session_models: std::collections::HashMap<uuid::Uuid, String> = self
            .agent_service
            .session_model_snapshot()
            .into_iter()
            .collect();
        // Carry each session's OWN working directory across the rebuild (#2008).
        // The `.with_working_directory(working_dir)` above copies the global
        // only, so without this every pane that had chosen its own repo answers
        // the next prompt from the new global default and reports it in Runtime
        // Info. Same contract as the provider and model pins below.
        let preserved_session_wds = self.agent_service.session_working_dir_snapshot();

        let new_agent_service = Arc::new(new_agent_service);
        for (sid, prov) in preserved_session_providers {
            let model = preserved_session_models
                .get(&sid)
                .cloned()
                .unwrap_or_else(|| prov.default_model().to_string());
            new_agent_service.swap_provider_for_session(sid, prov, model);
        }
        for (sid, dir) in preserved_session_wds {
            new_agent_service.set_session_only_working_directory(sid, dir);
        }

        // Update app state
        self.default_model_name = new_agent_service.provider_model();
        self.agent_service = new_agent_service;

        // Surface fallback warning as TUI system message
        if let Some(warning) = provider_warning {
            self.push_system_message(warning);
        }

        Ok(())
    }

    /// Convenience wrappers around the tps_tracker so call sites don't
    /// have to know the field name. Match the names used before the
    /// tracker was extracted into a standalone struct.
    pub(crate) fn advance_streaming_window(&mut self) {
        self.tps_tracker.advance(std::time::Instant::now());
    }
    pub(crate) fn current_streaming_active_secs(&self) -> f64 {
        self.tps_tracker.active_secs_now(std::time::Instant::now())
    }
    pub(crate) fn finalize_tps(&mut self, authoritative: Option<f64>) {
        self.tps_tracker
            .finalize(self.streaming_output_tokens, authoritative);
    }
    pub(crate) fn last_tps(&self) -> Option<f64> {
        self.tps_tracker.last_tps
    }

    /// Sync the current session's provider_name and model to match the active agent service.
    /// Call after rebuild_agent_service() so the footer and sessions screen reflect the change.
    pub(crate) async fn sync_session_to_provider(&mut self) {
        let provider_name = self.agent_service.provider_name();
        let model = self.default_model_name.clone();
        if let Some(ref mut session) = self.current_session {
            session.provider_name = Some(provider_name.clone());
            session.model = Some(model);
            let session_copy = session.clone();
            if let Err(e) = self.session_service.update_session(&session_copy).await {
                tracing::warn!("Failed to persist provider to session: {}", e);
            }
        }
        // Cache provider instance
        let provider_arc = self.agent_service.provider();
        self.provider_cache.insert(provider_name, provider_arc);
    }

    /// Get the agent service
    pub fn agent_service(&self) -> &Arc<AgentService> {
        &self.agent_service
    }

    /// Receive next event (blocks until available)
    pub async fn next_event(&mut self) -> Option<TuiEvent> {
        self.event_handler.next().await
    }

    /// Try to receive next event without blocking (returns None if queue is empty)
    pub fn try_next_event(&mut self) -> Option<TuiEvent> {
        self.event_handler.try_next()
    }

    /// Handle an event
    pub async fn handle_event(&mut self, event: TuiEvent) -> Result<()> {
        match event {
            TuiEvent::Key(key_event) => {
                self.handle_key_event(key_event).await?;
            }
            TuiEvent::MouseScroll(direction) => {
                if self.mode == AppMode::Chat {
                    if direction > 0 {
                        // Scrolling up — disable auto-scroll
                        let before = self.scroll_offset;
                        self.scroll_offset = self.scroll_offset.saturating_add(1);
                        self.auto_scroll = false;
                        tracing::debug!(
                            "[SCROLL] mouse up: {} -> {} (auto_scroll=false)",
                            before,
                            self.scroll_offset
                        );
                    } else {
                        let before = self.scroll_offset;
                        self.scroll_offset = self.scroll_offset.saturating_sub(1);
                        tracing::debug!(
                            "[SCROLL] mouse down: {} -> {}",
                            before,
                            self.scroll_offset
                        );
                        // Re-enable auto-scroll when back at bottom
                        if self.scroll_offset == 0 {
                            self.auto_scroll = true;
                        }
                    }
                }
            }
            TuiEvent::MouseClick(_col, row) => {
                if self.mode == AppMode::Chat {
                    // Clear input drag selection if clicking outside input
                    if row < self.input_area_y || row >= self.input_area_y + self.input_area_height
                    {
                        self.input_drag_selecting = false;
                        self.input_drag_anchor = None;
                        self.input_drag_current = None;
                    }
                    // Mouse-DOWN only STARTS a gesture (#726). Do NOT toggle
                    // expand/select here — that fired the expand before a drag
                    // could begin, so a click-drag over a collapsible block
                    // expanded it instead of selecting text to copy. The
                    // click-vs-drag decision happens on mouse-up: handle_mouse_up
                    // click-selects (expand) when no drag occurred and
                    // extracts+copies when a drag did. Reset the drag anchor so
                    // this gesture starts clean.
                    self.drag_anchor = None;
                    self.drag_current = None;
                }
            }
            TuiEvent::MouseRightClick(_col, row) => {
                if self.mode == AppMode::Chat {
                    self.handle_right_click_copy(row);
                }
            }
            TuiEvent::MouseDrag(col, row) => {
                if self.mode == AppMode::Chat {
                    // Check input area first
                    if self.input_drag_selecting
                        || (row >= self.input_area_y
                            && row < self.input_area_y + self.input_area_height)
                    {
                        self.handle_input_mouse_drag(col, row);
                    } else {
                        self.handle_mouse_drag(col, row);
                    }
                }
            }
            TuiEvent::MouseUp(col, row) => {
                if self.mode == AppMode::Chat {
                    // Check input area first
                    if self.input_drag_selecting
                        || (row >= self.input_area_y
                            && row < self.input_area_y + self.input_area_height)
                    {
                        if self.handle_input_mouse_up(col, row) {
                            self.notification = Some("Copied to clipboard".to_string());
                            self.notification_shown_at = Some(std::time::Instant::now());
                        }
                    } else {
                        self.handle_mouse_up(col, row);
                    }
                }
            }
            TuiEvent::Paste(text) => {
                // Handle paste events in Chat mode or Onboarding mode
                if self.mode == AppMode::Chat {
                    // Filter out terminal escape sequences that leak through on
                    // focus switch (mouse tracking, SGR mode, etc.).  These look
                    // like \x1b[<35;118;37M or CSI sequences and should never
                    // appear in user-typed text.
                    let mut filtered = Self::strip_terminal_escapes(&text);

                    // Normalize Unicode whitespace (non-breaking spaces, zero-width chars)
                    // that web pages use for table formatting. These paste as invisible
                    // bytes in terminals, gluing columns together.
                    filtered = Self::normalize_unicode_whitespace(&filtered);

                    // Convert tabs to spaces so pasted table data doesn't collapse.
                    // Terminal TUI can't render tab stops, so \t chars show as
                    // zero-width, gluing columns together.  Replacing with spaces
                    // preserves visual column alignment from web pages.
                    if filtered.contains('\t') {
                        filtered = Self::expand_tabs(&filtered);
                    }
                    if filtered.trim().is_empty() {
                        // No usable text. The clipboard may instead hold raw
                        // image bytes (copied from a browser, WhatsApp, a
                        // screenshot) that bracketed paste can't carry as text.
                        // Read them straight from the OS clipboard and attach.
                        if !self.attach_image_from_clipboard("pasted image") {
                            tracing::debug!(
                                "Paste event contained only escape sequences ({} bytes) — dropped",
                                text.len()
                            );
                        }
                    } else {
                        // Ensure cursor is on a valid char boundary before inserting
                        // (defensive — prevents panics from corrupted cursor state, issue #69).
                        self.cursor_position = self
                            .input_buffer
                            .floor_char_boundary(self.cursor_position.min(self.input_buffer.len()));

                        // Check if pasted text contains image paths — extract as attachments
                        let extraction = Self::extract_attachments(&filtered);
                        // A file copied across the drop tunnel says so in the
                        // chat, with where it landed (#1311).
                        for notice in extraction.notices {
                            self.push_system_message(notice);
                        }
                        let (clean_text, new_attachments) =
                            (extraction.text, extraction.attachments);
                        if !new_attachments.is_empty() {
                            // Track them, exactly as a pasted clipboard image
                            // is tracked just above. This is what routes the
                            // file into `projects/<slug>/files/` when the
                            // session belongs to a project: dropped files were
                            // going straight into `attachments` and skipping
                            // that entirely, so a drop never became a project
                            // artifact the way every other share does.
                            if let Some(session) = &self.current_session {
                                for att in &new_attachments {
                                    let file_svc = self.file_service.clone();
                                    let sid = session.id;
                                    let path = std::path::PathBuf::from(&att.path);
                                    tokio::spawn(async move {
                                        if let Err(e) =
                                            file_svc.get_or_create_file(sid, path, None).await
                                        {
                                            tracing::warn!("Failed to track dropped file: {e}");
                                        }
                                    });
                                }
                            }
                            self.attachments.extend(new_attachments);
                            if !clean_text.trim().is_empty() {
                                self.input_buffer
                                    .insert_str(self.cursor_position, &clean_text);
                                self.cursor_position += clean_text.len();
                            }
                        } else {
                            self.input_buffer
                                .insert_str(self.cursor_position, &filtered);
                            self.cursor_position += filtered.len();
                        }
                    } // end else (non-empty after filtering)
                    self.update_slash_suggestions();
                } else if self.mode == AppMode::Onboarding {
                    // Handle paste in onboarding wizard (for API keys, etc.)
                    if let Some(ref mut wizard) = self.onboarding {
                        wizard.handle_paste(&text);
                        // Trigger model fetch if provider supports it
                        // Custom providers: fetch if base_url is set (no key needed for local endpoints)
                        // Built-in providers: require non-empty api_key_input
                        let should_fetch = if wizard.ps.is_custom() {
                            wizard.ps.supports_model_fetch()
                        } else {
                            wizard.ps.supports_model_fetch() && !wizard.ps.api_key_input.is_empty()
                        };
                        if should_fetch {
                            let provider_idx = wizard.ps.selected_provider;
                            // Real key: typed, else saved from config; never the
                            // EXISTING_KEY_SENTINEL placeholder that 401'd custom
                            // providers whose key lives in keys.toml (#656).
                            let api_key = wizard.ps.effective_api_key();
                            let base_url =
                                if wizard.ps.is_custom() && !wizard.ps.base_url.is_empty() {
                                    Some(wizard.ps.base_url.clone())
                                } else {
                                    None
                                };
                            wizard.ps.models_fetching = true;
                            let sender = self.event_sender();
                            tokio::spawn(async move {
                                let models = super::onboarding::fetch_provider_models(
                                    provider_idx,
                                    api_key.as_deref(),
                                    None,
                                    None,
                                    None,
                                    base_url.as_deref(),
                                    None,
                                )
                                .await;
                                let _ = sender.send(TuiEvent::OnboardingModelsFetched(models));
                            });
                        }
                    }
                }
            }
            TuiEvent::MessageSubmitted(content) => {
                self.send_message(content).await?;
            }
            TuiEvent::CommandSubmitted(content) => {
                self.send_command_message(content).await?;
            }
            TuiEvent::ResponseChunk { session_id, text } => {
                let is_current = self.is_current_session(session_id);
                tracing::trace!(
                    "[TUI] ResponseChunk: len={} is_current={} streaming_len={}",
                    text.len(),
                    is_current,
                    self.streaming_response
                        .as_ref()
                        .map(|s| s.len())
                        .unwrap_or(0)
                );
                // Drop chunks for a turn that has been cancelled. The agent
                // task is aborted, but events it already emitted keep draining
                // from the channel, and applying them re-populated the
                // streaming state the abort had just cleared (#1342).
                if !self.is_session_processing(session_id) {
                    return Ok(());
                }
                // Route to foreground OR the per-session background
                // sidecar so the inactive pane sees streaming chunks
                // live instead of catching up on focus switch.
                self.session_state_mut(session_id)
                    .append_streaming_chunk(&text);
            }
            TuiEvent::StripStreamedContent {
                session_id,
                bytes,
                reason,
            } => {
                if self.is_current_session(session_id) {
                    // The gaslighting preamble is always leading in the
                    // streaming buffer, so drain exactly `bytes` bytes from
                    // the start — not the entire buffer. Prior behavior
                    // wiped everything, which also destroyed any legitimate
                    // draft that followed the preamble in the same block.
                    if let Some(ref mut buf) = self.streaming_response {
                        let prior_len = buf.len();
                        // Clamp to a valid char boundary ≤ bytes to avoid
                        // panicking on multi-byte codepoints.
                        let mut cut = bytes.min(prior_len);
                        while cut > 0 && !buf.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        tracing::warn!(
                            "[TUI] StripStreamedContent: draining {}/{} bytes from start of streaming response — {}",
                            cut,
                            prior_len,
                            reason
                        );
                        buf.drain(..cut);
                        if buf.is_empty() {
                            self.streaming_response = None;
                            self.streaming_render_cache = None;
                        }
                    } else {
                        tracing::warn!(
                            "[TUI] StripStreamedContent: no buffer to strip ({} bytes requested) — {}",
                            bytes,
                            reason
                        );
                    }
                }
            }
            TuiEvent::ReasoningChunk { session_id, text } => {
                // Same cancellation guard as ResponseChunk: a cancelled turn's
                // queued reasoning was what left the pane showing "is thinking"
                // after the abort had cleared it (#1342).
                if !self.is_session_processing(session_id) {
                    return Ok(());
                }
                // Route to foreground OR background sidecar. The
                // routing helper handles empty/whitespace filtering
                // and the foreground-only `scroll_offset = 0` nudge
                // for the auto-scroll behaviour.
                self.session_state_mut(session_id)
                    .append_reasoning_chunk(&text);
            }
            TuiEvent::ResponseComplete {
                session_id,
                response,
            } => {
                if self.is_current_session(session_id) {
                    if self.is_processing {
                        self.complete_response(response).await?;
                    } else {
                        // Session was cancelled (Esc×2) — agent finished after cancel.
                        // Reload from DB to pick up any final content the agent wrote
                        // before detecting the cancellation token.
                        self.load_session(session_id).await?;
                    }
                } else {
                    // Background session completed — mark as unread
                    self.processing_sessions.remove(&session_id);
                    self.session_cancel_tokens.remove(&session_id);
                    self.sessions_with_unread.insert(session_id);
                    // Drop the background live-state sidecar — the
                    // turn is finalised and the next focus switch
                    // re-reads from the DB anyway. Without this the
                    // sidecar's `streaming_response` / `active_tool_group`
                    // would linger forever for a session the user
                    // never re-focuses, leaking memory in long sessions.
                    self.background_sessions.remove(&session_id);
                    // Refresh pane cache so inactive pane shows the completed response
                    if self.pane_manager.is_split() {
                        self.preload_pane_session(session_id).await;
                    }
                }
            }
            TuiEvent::Error {
                session_id,
                message,
            } => {
                // Always clear session processing state — missing this for current sessions
                // caused subsequent messages to be silently queued after errors.
                self.processing_sessions.remove(&session_id);
                self.session_cancel_tokens.remove(&session_id);
                if self.is_current_session(session_id) {
                    if message == "Cancelled" {
                        // Esc×2: the input handler already promoted
                        // in-flight streaming text, reasoning, and the
                        // active tool group into `self.messages` BEFORE
                        // aborting the agent task. Reloading from DB here
                        // races the async persist_streaming_state writes
                        // and wipes what the user just watched stream —
                        // the cancel erased the visible chat until
                        // restart. Leave the in-memory state alone; the
                        // DB write is for restart-resume, not for this
                        // render.
                    } else {
                        // Non-cancel errors: reload so the user sees
                        // whatever DID get persisted and the error toast.
                        self.load_session(session_id).await?;
                        self.show_error(message);
                    }
                } else {
                    tracing::warn!("Background session {} error: {}", session_id, message);
                    // Refresh pane cache so inactive pane shows whatever was written
                    if self.pane_manager.is_split() {
                        self.preload_pane_session(session_id).await;
                    }
                }
            }
            TuiEvent::SwitchMode(mode) => {
                self.switch_mode(mode).await?;
            }
            TuiEvent::SelectSession(session_id) => {
                self.load_session(session_id).await?;
            }
            TuiEvent::NewSession => {
                self.create_new_session().await?;
            }
            TuiEvent::Quit => {
                crate::brain::agent::service::shutdown::mark_shutting_down();
                self.pane_manager.save_layout();
                self.should_quit = true;
            }
            TuiEvent::Tick => {
                // Update animation frame for spinner
                self.animation_frame = self.animation_frame.wrapping_add(1);

                // Resolve deferred health checks (shows Pending for one frame first)
                if let Some(ref mut wizard) = self.onboarding
                    && wizard.health_running
                    && !wizard.health_complete
                {
                    wizard.tick_health_check();
                }

                // Auto-dismiss the transient notices on the input border
                // (#1369). Expiry lives here, on the tick, so the renderer
                // stays a pure read of `App`. Untimestamped errors (runner
                // failures) are left alone: `notice_expired` is false for
                // them, as the old `if let Some(shown_at)` guard was.
                let now = std::time::Instant::now();
                if notice_expired(self.error_message_shown_at, now, ERROR_TTL) {
                    self.error_message = None;
                    self.error_message_shown_at = None;
                }
                if notice_expired(self.notification_shown_at, now, NOTIFICATION_TTL) {
                    self.notification = None;
                    self.notification_shown_at = None;
                }
                if notice_expired(self.clipboard_hint_shown_at, now, CLIPBOARD_HINT_TTL) {
                    self.clipboard_hint = None;
                    self.clipboard_hint_shown_at = None;
                }

                // A screenshot may have landed since the last probe; its hint
                // shares the notice slot inline in the input area (#1816).
                self.refresh_clipboard_hint(false);
            }
            TuiEvent::ToolApprovalRequested(request) => {
                self.handle_approval_requested(request);
            }
            TuiEvent::ToolApprovalResponse(_response) => {
                // Response is sent via channel, auto-scroll if enabled
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            TuiEvent::ToolCallStarted {
                session_id,
                tool_name,
                tool_input,
            } => {
                // Drop the original `is_current_session && is_processing`
                // guard. Background sessions are still mid-turn from
                // the agent's perspective; the `is_processing` check
                // would have been false on the foreground `AppState`
                // for any non-focused session anyway. Each session's
                // processing flag is now tracked per-state by
                // `ChannelProcessingStarted`/`Finished` and migrates
                // with focus, so the gating is meaningful per
                // session rather than only for the focused one.
                let desc = Self::format_tool_description(&tool_name, &tool_input);
                let entry = ToolCallEntry {
                    task_id: None,
                    description: desc,
                    success: true,
                    details: None,
                    completed: false,
                    tool_input: tool_input.clone(),
                };
                {
                    let mut state = self.session_state_mut(session_id);
                    if let Some(group) = state.active_tool_group_mut() {
                        group.calls.push(entry);
                    } else {
                        state.set_active_tool_group(Some(ToolCallGroup {
                            calls: vec![entry],
                            expanded: false,
                        }));
                    }
                }
                if self.is_current_session(session_id) {
                    tracing::info!(
                        "[TUI] ToolCallStarted: {} (active_group={}, msg_count={})",
                        tool_name,
                        self.active_tool_group.is_some(),
                        self.messages.len()
                    );
                    if self.auto_scroll {
                        self.scroll_offset = 0;
                    }
                }
            }
            TuiEvent::IntermediateText {
                session_id,
                text,
                reasoning,
            } => {
                // Background sessions also flush text + tool groups
                // between agent rounds — routing this through the
                // session_state_mut helper means the inactive pane
                // sees flushed assistant messages and tool-group
                // bullets as they happen instead of only on focus
                // switch. The original `is_processing` guard moves
                // into the foreground-only `intermediate_text_received`
                // mirror at the bottom; background sessions don't
                // use that flag (it gates the foreground "the agent
                // is mid-stream" detection and would force-clear
                // useful state if mirrored).
                let is_foreground = self.is_current_session(session_id);
                if is_foreground {
                    tracing::info!(
                        "[TUI] IntermediateText: len={} active_group={} streaming={}",
                        text.len(),
                        self.active_tool_group.is_some(),
                        self.streaming_response.is_some()
                    );
                }

                // Capture reasoning from the routed state's streaming
                // accumulator: foreground uses self.streaming_reasoning,
                // background uses the sidecar's. Either way the take()
                // clears the source so the next round starts fresh.
                let reasoning_details = if let Some(r) = reasoning {
                    Some(r)
                } else {
                    let mut state = self.session_state_mut(session_id);
                    match &mut state {
                        super::background_session::SessionStateMut::Foreground(app) => {
                            app.streaming_reasoning.take()
                        }
                        super::background_session::SessionStateMut::Background(bg) => {
                            bg.streaming_reasoning.take()
                        }
                    }
                };

                // Build a tool_group DisplayMessage helper.
                let make_tool_group = |group: ToolCallGroup| DisplayMessage {
                    id: Uuid::new_v4(),
                    role: "tool_group".to_string(),
                    content: format!(
                        "{} tool call{}",
                        group.calls.len(),
                        if group.calls.len() == 1 { "" } else { "s" }
                    ),
                    timestamp: chrono::Utc::now(),
                    token_count: None,
                    cost: None,
                    approval: None,
                    approve_menu: None,
                    details: None,
                    expanded: false,
                    expanded_full: false,
                    tool_group: Some(group),
                    duration_secs: None,
                };

                // Raw vs stripped accounting for the "eating intermediate
                // text" diagnosis (2026-06-05). Three IntermediateText
                // events emitted with text len=82/23/66 but only TWO
                // rows showed in chat. Cause: `strip_llm_artifacts` was
                // zeroing some inputs (e.g. text that was 100% HTML
                // comments or XML tool markers), which then fell into
                // the `has_reasoning && !has_text` branch and merged
                // into prior thinking — silently swallowing what the
                // model actually said.
                let raw_text_trimmed_len = text.trim().chars().count();
                let raw_text_was_nonempty = raw_text_trimmed_len > 0;
                let text_clean = crate::utils::sanitize::strip_llm_artifacts(&text);
                let stripped_len = text_clean.trim().chars().count();
                let stripped_to_empty = raw_text_was_nonempty && stripped_len == 0;

                if stripped_to_empty {
                    // Loud trace so the next repro nails which marker
                    // is over-stripping. Quote the raw text (cap so a
                    // pathological 10 KB block doesn't flood the log)
                    // for offline analysis.
                    let preview: String = text.chars().take(500).collect();
                    tracing::warn!(
                        "[IntermediateText] strip_llm_artifacts ate ALL {} chars of intermediate text — \
                         falling back to RAW text so the message isn't lost. \
                         Raw preview (up to 500 chars): {:?}",
                        raw_text_trimmed_len,
                        preview
                    );
                } else if raw_text_was_nonempty && stripped_len < raw_text_trimmed_len {
                    tracing::debug!(
                        "[IntermediateText] strip_llm_artifacts trimmed {} → {} chars",
                        raw_text_trimmed_len,
                        stripped_len
                    );
                }

                // Pick the text to actually display: stripped when it
                // kept ANY content, raw when the strip zeroed it out.
                // Raw fallback preserves the visible intermediate so it
                // doesn't get silently rolled into reasoning — markdown
                // rendering hides HTML comments naturally; CODE_EDIT_BLOCK
                // fences render as code blocks (still ugly but visible
                // beats vanished).
                let display_text = if stripped_to_empty {
                    text.clone()
                } else {
                    text_clean.clone()
                };
                let has_text = !display_text.trim().is_empty();
                let has_reasoning = reasoning_details
                    .as_ref()
                    .is_some_and(|r| !r.trim().is_empty());

                {
                    let mut state = self.session_state_mut(session_id);
                    // Clear the in-flight streaming response — the
                    // text is now becoming a permanent message.
                    match &mut state {
                        super::background_session::SessionStateMut::Foreground(app) => {
                            app.streaming_response = None;
                            app.streaming_render_cache = None;
                        }
                        super::background_session::SessionStateMut::Background(bg) => {
                            bg.streaming_response = None;
                        }
                    }

                    state.reset_processing_clock();

                    if has_text {
                        // Standard case: flush prior tool group, then
                        // push assistant text+reasoning message.
                        if let Some(group) = state.take_active_tool_group() {
                            state.push_message(make_tool_group(group));
                        }
                        state.push_message(DisplayMessage {
                            id: Uuid::new_v4(),
                            role: "assistant".to_string(),
                            content: display_text,
                            timestamp: chrono::Utc::now(),
                            token_count: None,
                            cost: None,
                            approval: None,
                            approve_menu: None,
                            details: reasoning_details.clone(),
                            expanded: false,
                            expanded_full: false,
                            tool_group: None,
                            duration_secs: None,
                        });
                        // DB persistence happens in tool_loop's per-iteration
                        // append_content with `<!-- reasoning -->` markers — this
                        // DisplayMessage.id is TUI-local and intentionally does NOT
                        // match any DB row, so writing here would silently no-op.
                    } else if has_reasoning {
                        // Reasoning-only iteration: push a SEPARATE
                        // thinking row per LLM iteration. The previous
                        // implementation merged consecutive thinking-
                        // only events into one giant `details` blob —
                        // logs from 2026-06-05 showed six consecutive
                        // reasoning-only IntermediateText events
                        // (len=0 visible text) fusing into a single
                        // unreadable wall of thought. Each reasoning
                        // pass is its own logical step (the model
                        // explicitly decided to think more before
                        // emitting a tool call or text), so render
                        // them as discrete `▸ Thinking` rows the user
                        // can expand individually. Tool group flush
                        // still happens AFTER the thinking row so
                        // order stays: think → tools → next.
                        let reasoning_text = reasoning_details.unwrap_or_default();
                        state.push_message(DisplayMessage {
                            id: Uuid::new_v4(),
                            role: "assistant".to_string(),
                            content: String::new(),
                            timestamp: chrono::Utc::now(),
                            token_count: None,
                            cost: None,
                            approval: None,
                            approve_menu: None,
                            details: Some(reasoning_text),
                            expanded: false,
                            expanded_full: false,
                            tool_group: None,
                            duration_secs: None,
                        });
                        if let Some(group) = state.take_active_tool_group() {
                            state.push_message(make_tool_group(group));
                        }
                    } else {
                        // Pure flush trigger (CLI Ping with no
                        // pending content) — just flush the tool
                        // group if any.
                        if let Some(group) = state.take_active_tool_group() {
                            state.push_message(make_tool_group(group));
                        }
                    }
                }

                if is_foreground {
                    self.intermediate_text_received = true;
                    if self.auto_scroll {
                        self.scroll_offset = 0;
                    }
                }
            }
            TuiEvent::QueuedUserMessage { session_id, text } => {
                let is_foreground = self.is_current_session(session_id);
                if is_foreground {
                    tracing::info!(
                        "[TUI] QueuedUserMessage inline: len={} active_group={}",
                        text.len(),
                        self.active_tool_group.is_some()
                    );
                }

                {
                    let mut state = self.session_state_mut(session_id);
                    // Flush any active tool group so the queued user
                    // message appears after it in the chronological
                    // chat flow. Background sessions accumulate this
                    // in pending_messages and merge on focus return.
                    if let Some(group) = state.take_active_tool_group() {
                        let count = group.calls.len();
                        state.push_message(DisplayMessage {
                            id: Uuid::new_v4(),
                            role: "tool".to_string(),
                            content: format!(
                                "{} tool call{} completed",
                                count,
                                if count == 1 { "" } else { "s" }
                            ),
                            timestamp: chrono::Utc::now(),
                            token_count: None,
                            cost: None,
                            approval: None,
                            approve_menu: None,
                            details: None,
                            expanded: false,
                            expanded_full: false,
                            tool_group: Some(group),
                            duration_secs: None,
                        });
                    }

                    state.push_message(DisplayMessage {
                        id: Uuid::new_v4(),
                        role: "user".to_string(),
                        content: text,
                        timestamp: chrono::Utc::now(),
                        token_count: None,
                        cost: None,
                        approval: None,
                        approve_menu: None,
                        details: None,
                        expanded: false,
                        expanded_full: false,
                        tool_group: None,
                        duration_secs: None,
                    });
                }

                // Foreground-only post-hooks: the queued message has now been
                // echoed into the chat, so drop it from the per-session queue
                // map, and reset scroll. Do NOT touch input_buffer — it holds
                // whatever the user has started typing for their NEXT message,
                // which must survive a queued message entering mid-turn (else
                // their in-progress text is silently wiped, and arrow-up can't
                // get it back). Background sessions' queues are cleared by their
                // own send-loop on the channels side.
                if is_foreground {
                    if let Some(sid) = self.current_session.as_ref().map(|s| s.id)
                        && let Ok(mut q) = self.queued_messages.lock()
                    {
                        q.remove(&sid);
                    }
                    if self.auto_scroll {
                        self.scroll_offset = 0;
                    }
                }
            }
            TuiEvent::ToolCallCompleted {
                session_id,
                tool_name,
                tool_input,
                success,
                summary,
            } => {
                let desc = Self::format_tool_description(&tool_name, &tool_input);
                let details = if summary.is_empty() {
                    None
                } else {
                    Some(summary)
                };

                // Update the matching Started entry in the active
                // tool group (foreground OR background). Foreground
                // edits affect the active pane's render; background
                // edits keep the inactive pane's "X tool calls"
                // badge accurate live.
                {
                    let mut state = self.session_state_mut(session_id);
                    let updated = if let Some(group) = state.active_tool_group_mut() {
                        if let Some(existing) = group
                            .calls
                            .iter_mut()
                            .rev()
                            .find(|c| c.description == desc && !c.completed)
                        {
                            existing.success = success;
                            existing.details = details.clone();
                            existing.completed = true;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if !updated {
                        let entry = ToolCallEntry {
                            task_id: None,
                            description: desc,
                            success,
                            details,
                            completed: true,
                            tool_input: tool_input.clone(),
                        };
                        if let Some(group) = state.active_tool_group_mut() {
                            group.calls.push(entry);
                        } else {
                            state.set_active_tool_group(Some(ToolCallGroup {
                                calls: vec![entry],
                                expanded: false,
                            }));
                        }
                    }
                }

                // Foreground-only flushes: message append, plan
                // reload, scroll. Background sessions accumulate
                // their group inside `active_tool_group` until the
                // session is promoted to foreground (where the next
                // IntermediateText / ResponseComplete handles the
                // flush) or until the agent's own DB-persisted
                // message arrives on focus switch via the
                // `preload_pane_session` reload path.
                if self.is_current_session(session_id) && self.is_processing {
                    self.processing_started_at = Some(std::time::Instant::now());
                    // Plan-file-changed events: the plan tool mutates the
                    // JSON directly; write tools may have edited the session
                    // .md, which the registry mirrors into the JSON
                    // description while Editing. Reload on both.
                    if matches!(tool_name.as_str(), "plan" | "write_file" | "edit_file") {
                        self.reload_plan();
                    }
                    if self.auto_scroll {
                        self.scroll_offset = 0;
                    }
                    let all_done = self
                        .active_tool_group
                        .as_ref()
                        .is_some_and(|g| g.calls.iter().all(|c| c.completed));
                    if all_done && let Some(group) = self.active_tool_group.take() {
                        let count = group.calls.len();
                        self.messages.push(DisplayMessage {
                            id: Uuid::new_v4(),
                            role: "tool_group".to_string(),
                            content: format!(
                                "{} tool call{}",
                                count,
                                if count == 1 { "" } else { "s" }
                            ),
                            timestamp: chrono::Utc::now(),
                            token_count: None,
                            cost: None,
                            approval: None,
                            approve_menu: None,
                            details: None,
                            expanded: false,
                            expanded_full: false,
                            tool_group: Some(group),
                            duration_secs: None,
                        });
                    }
                }
            }
            TuiEvent::BuildLine(line) => {
                // Keep a rolling window of the last 6 build lines
                self.build_lines.push(line);
                if self.build_lines.len() > 6 {
                    self.build_lines.remove(0);
                }
                // Build the display content: header + rolling lines
                let content = format!("🦀 Building OpenCrabs...\n{}", self.build_lines.join("\n"));
                if let Some(idx) = self.build_msg_idx {
                    // Update existing build message in place
                    if let Some(msg) = self.messages.get_mut(idx) {
                        msg.content = content;
                    }
                } else {
                    // Create the build progress message
                    self.messages.push(DisplayMessage {
                        id: Uuid::new_v4(),
                        role: "system".to_string(),
                        content,
                        timestamp: chrono::Utc::now(),
                        token_count: None,
                        cost: None,
                        approval: None,
                        approve_menu: None,
                        details: None,
                        expanded: false,
                        expanded_full: false,
                        tool_group: None,
                        duration_secs: None,
                    });
                    self.build_msg_idx = Some(self.messages.len() - 1);
                }
                self.scroll_offset = 0;
            }
            TuiEvent::RestartReady {
                status,
                binary_path,
            } => {
                // Clear build progress
                if let Some(idx) = self.build_msg_idx.take()
                    && idx < self.messages.len()
                {
                    self.messages.remove(idx);
                }
                self.build_lines.clear();
                tracing::info!("RestartReady: {status}");
                // Auto exec() restart — no prompt, no permission needed.
                if let Some(session) = &self.current_session {
                    let session_id = session.id;
                    // Prefer the exact binary the producer built;
                    // fall back to current_exe() via auto_detect only when no
                    // path was given (/evolve replaced the exe in place).
                    let result = match &binary_path {
                        Some(path) => SelfUpdater::restart_into(path, session_id),
                        None => SelfUpdater::auto_detect().and_then(|u| u.restart(session_id)),
                    };
                    // Only reached on error — exec() replaces the process.
                    if let Err(e) = result {
                        self.show_error(format!("Restart failed: {}", e));
                        self.switch_mode(AppMode::Chat).await?;
                    }
                }
            }
            TuiEvent::ConfigReloaded => {
                // Refresh commands autocomplete
                self.reload_user_commands();
                // Refresh approval policy
                (self.approval_auto_session, self.approval_auto_always) =
                    Self::read_approval_policy_from_config();
                // Refresh the context-budget footer. A changed
                // `providers.<name>.context_window` (or a provider swap from new
                // keys) changes the effective window; the watcher fires this
                // event AFTER swapping the provider, so re-read it now. Mirrors
                // the load_session path so the active session updates at once.
                if let Some(session) = self.current_session.as_ref() {
                    let sid = session.id;
                    let max_ctx = self.agent_service.context_limit_for_session(sid);
                    self.context_max_tokens = max_ctx;
                    self.session_context_max.insert(sid, max_ctx);
                }
                // Provider swap is already handled by the ConfigWatcher callback
                // (ui.rs). Do NOT re-create the provider here — it causes a
                // redundant create_provider call every reload, and the model-name
                // comparison (config alias vs provider full ID) never matches,
                // so it would fire on every single reload.
                tracing::info!(
                    "Config reloaded — refreshed commands, approval policy, context budget"
                );
            }
            TuiEvent::TokenCountUpdated { session_id, count } => {
                // Always cache the per-session value so a future
                // focus switch shows accurate context usage.
                self.session_input_tokens.insert(session_id, count as u32);
                // Mirror into the visible display fields via the
                // routing helper so the inactive pane's footer
                // updates live, not just on focus switch.
                let mut state = self.session_state_mut(session_id);
                state.set_display_token_count(count);
                state.set_last_input_tokens(count as u32);
            }
            TuiEvent::StreamingOutputTokens { session_id, tokens } => {
                self.session_state_mut(session_id)
                    .add_streaming_output_tokens(tokens);
            }
            // Automatic compaction is fully silent — the agent's
            // `ProgressEvent::CompactionSummary` is dropped at the
            // channel bridge (`cli/ui.rs:528-530`, "summary goes to
            // memory log only") and manual `/compact` triggers a
            // normal `MessageSubmitted` rather than this variant.
            // Nothing in the codebase constructs
            // `TuiEvent::CompactionSummary` today; this no-op arm
            // exists only so a future re-wiring of visible
            // compaction has a designated landing site rather than
            // silently falling through to a panic.
            TuiEvent::CompactionSummary { .. } => {}

            TuiEvent::SessionTitleUpdated { session_id, title } => {
                // Cheap in-memory refresh — update the title field on
                // current_session if it matches, and on the cached
                // sessions list entry so the /sessions screen reflects
                // the new title without a re-query. No DB roundtrip, no
                // message reload, no scroll disturbance. Fires from the
                // auto-title spawn in tool_loop after the first user
                // message of a fresh session.
                if let Some(ref mut s) = self.current_session
                    && s.id == session_id
                {
                    s.title = Some(title.clone());
                }
                if let Some(entry) = self.sessions.iter_mut().find(|s| s.id == session_id) {
                    entry.title = Some(title);
                }
            }

            TuiEvent::SessionUpdated(session_id) => {
                // A remote channel updated a session. Don't reload immediately —
                // during multi-tool runs this fires after every tool call, and each
                // reload is a blocking DB query that freezes the event loop (making
                // Ctrl+C unresponsive and garbling the display). Instead, debounce:
                // schedule a refresh and let the main loop's idle tick handle it.
                if self.is_current_session(session_id) {
                    // Only schedule refresh if the session isn't being processed
                    // by the TUI itself (channel processing is fine — the refresh
                    // fires after ChannelProcessingFinished).
                    if !self.processing_sessions.contains(&session_id)
                        || self.channel_processing_sessions.contains(&session_id)
                    {
                        self.pending_session_refresh =
                            Some((session_id, std::time::Instant::now()));
                    }
                } else {
                    self.sessions_with_unread.insert(session_id);
                }
            }

            TuiEvent::ChannelProcessingStarted(session_id) => {
                self.channel_processing_sessions.insert(session_id);
                self.processing_sessions.insert(session_id);
                // Mark the session as processing in foreground OR
                // background state so the inactive pane's
                // `[processing...]` label fires the moment a remote
                // channel starts a turn, not on focus switch.
                self.session_state_mut(session_id).set_processing(true);
            }

            TuiEvent::ChannelProcessingFinished(session_id) => {
                self.channel_processing_sessions.remove(&session_id);
                // Foreground-only safeguard: only clear the
                // foreground's processing/cancel_token state when
                // this is the focused session AND no local task is
                // also running. Background sessions clear via the
                // routing helper unconditionally — their turn really
                // did just finish.
                let clear_state = if self.is_current_session(session_id) {
                    self.cancel_token.is_none()
                } else {
                    true
                };
                if clear_state {
                    self.processing_sessions.remove(&session_id);
                    self.session_state_mut(session_id).set_processing(false);
                    if self.is_current_session(session_id) {
                        // Schedule a debounced refresh instead of blocking with
                        // load_session().await — a direct DB query here freezes the
                        // event loop, eating queued terminal events and garbling the
                        // display when channels process messages in parallel.
                        self.pending_session_refresh =
                            Some((session_id, std::time::Instant::now()));
                    }
                }
            }

            TuiEvent::PendingResumed {
                session_id,
                cancel_token,
            } => {
                // A pending request was resumed on startup — wire the cancel token
                // so double-Escape can abort it.
                self.processing_sessions.insert(session_id);
                if self.is_current_session(session_id) {
                    self.is_processing = true;
                    self.processing_started_at = Some(std::time::Instant::now());
                    self.cancel_token = Some(cancel_token);
                    // A resumed turn starts from the end of the loaded history.
                    // Its own rows come after this line; everything above is
                    // already durable and must never be re-persisted (#2127).
                    self.turn_start_len = self.messages.len();
                } else {
                    self.session_cancel_tokens.insert(session_id, cancel_token);
                }
            }

            TuiEvent::DetachedTaskStarted {
                session_id,
                task_id,
                label,
            } => {
                // #2146: the task gets ONE row in the turn's tool block, pushed
                // synchronously while that turn is still live. The finish finds
                // it again by `task_id`, so a task that takes a minute does not
                // leave the block and come back as a second message.
                let entry = ToolCallEntry {
                    description: label,
                    success: true,
                    details: None,
                    completed: false,
                    tool_input: serde_json::Value::Null,
                    task_id: Some(task_id),
                };
                let mut state = self.session_state_mut(session_id);
                if let Some(group) = state.active_tool_group_mut() {
                    group.calls.push(entry);
                } else {
                    state.set_active_tool_group(Some(ToolCallGroup {
                        calls: vec![entry],
                        expanded: false,
                    }));
                }
                if self.is_current_session(session_id) && self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }

            TuiEvent::DetachedTaskFinished {
                session_id,
                task_id,
                label,
                success,
                elapsed_secs,
            } => {
                // #2146: settle the row the start already drew. A finish with no
                // row is not an error: the turn may have been settled, or the
                // task outlived a restart. Since #2218 the receipt push is the
                // fallback for a task the manager holds no callback for, so a
                // surface that did get this event must not expect a second
                // `BackgroundTaskDone` to follow.
                let found = {
                    let mut state = self.session_state_mut(session_id);
                    if let Some(row) = state.find_detached_row(&task_id) {
                        row.success = success;
                        row.completed = true;
                        row.details = Some(format!("{label} finished in {elapsed_secs:.1}s"));
                        true
                    } else {
                        false
                    }
                };
                if !found {
                    tracing::debug!(
                        "[TUI] DetachedTaskFinished for {task_id} has no row; \
                         the outcome was already settled elsewhere"
                    );
                }
            }

            TuiEvent::BackgroundTaskDone {
                session_id,
                context_text,
                display_text,
            } => {
                // A backgrounded long task finished (#722). If the session is
                // still processing, inject the result mid-turn via the queue
                // (drained at the next tool-call boundary). If it's idle, start a
                // fresh turn with the completion.
                if self.processing_sessions.contains(&session_id) {
                    if let Ok(mut q) = self.queued_messages.lock() {
                        // Synthetic: the model reads the full result, history
                        // and the transcript get the compact tag. Queuing the
                        // context alone published the whole `[System: ...]`
                        // block, command and output included, as a user turn
                        // (#765).
                        q.entry(session_id).or_default().push(
                            crate::brain::agent::QueuedUserMessage::system(
                                context_text,
                                display_text,
                            ),
                        );
                    }
                } else {
                    self.resume_background_turn(session_id, context_text, display_text)
                        .await;
                }
            }

            TuiEvent::OnboardingModelsFetched(models) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.ps.models_fetching = false;
                    if !models.is_empty() {
                        wizard.ps.models = models;
                        wizard.ps.resolve_selected_model_index();
                    } else {
                        // Fetch returned empty — fall back to static PROVIDERS models
                        let provider = wizard.ps.current_provider();
                        if !provider.models.is_empty() {
                            wizard.ps.models =
                                provider.models.iter().map(|s| s.to_string()).collect();
                            wizard.ps.resolve_selected_model_index();
                        }
                    }
                }
            }
            TuiEvent::GitHubDeviceCode(code) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.github_user_code = Some(code);
                    wizard.github_device_flow_status =
                        super::onboarding::GitHubDeviceFlowStatus::WaitingForUser;
                }
            }
            TuiEvent::GitHubOAuthComplete(oauth_token) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.github_device_flow_status =
                        super::onboarding::GitHubDeviceFlowStatus::Complete;
                    // Save the OAuth token to keys.toml
                    if let Err(e) =
                        crate::config::write_secret_key("providers.github", "api_key", &oauth_token)
                    {
                        tracing::warn!("Failed to save Copilot OAuth token: {}", e);
                    }
                    // Mark key as existing and advance to model selection
                    wizard.ps.api_key_input = super::onboarding::EXISTING_KEY_SENTINEL.to_string();
                    wizard.auth_field = super::onboarding::AuthField::Model;
                    wizard.ps.models.clear();
                    wizard.ps.selected_model = 0;
                    // Trigger model fetch using the OAuth token
                    let token = oauth_token.clone();
                    let sender = self.event_sender();
                    tokio::spawn(async move {
                        let models = super::onboarding::fetch_provider_models(
                            2,
                            Some(&token),
                            None,
                            None,
                            None,
                            None,
                            None,
                        )
                        .await;
                        let _ = sender.send(TuiEvent::OnboardingModelsFetched(models));
                    });
                }
            }
            TuiEvent::GitHubOAuthError(err) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.github_device_flow_status =
                        super::onboarding::GitHubDeviceFlowStatus::Failed(err);
                    wizard.github_user_code = None;
                }
            }
            TuiEvent::CodexDeviceCode(code) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.ps.codex_user_code = Some(code);
                    wizard.ps.codex_device_flow_status =
                        super::onboarding::CodexDeviceFlowStatus::WaitingForUser;
                }
            }
            TuiEvent::CodexOAuthComplete => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.ps.codex_device_flow_status =
                        super::onboarding::CodexDeviceFlowStatus::Complete;
                    // Mark as authenticated (no API key needed, tokens stored separately)
                    wizard.ps.api_key_input = super::onboarding::EXISTING_KEY_SENTINEL.to_string();
                    wizard.auth_field = super::onboarding::AuthField::Model;
                    wizard.ps.models.clear();
                    wizard.ps.selected_model = 0;
                    // Enable the provider in config
                    let _ = crate::config::Config::write_key("providers.codex", "enabled", "true");
                    // Trigger model fetch
                    let sender = self.event_sender();
                    tokio::spawn(async move {
                        let models = super::onboarding::fetch_provider_models(
                            2, None, None, None, None, None, None,
                        )
                        .await;
                        let _ = sender.send(TuiEvent::OnboardingModelsFetched(models));
                    });
                }
            }
            TuiEvent::CodexOAuthError(err) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.ps.codex_device_flow_status =
                        super::onboarding::CodexDeviceFlowStatus::Failed(err);
                    wizard.ps.codex_user_code = None;
                }
            }
            TuiEvent::WhatsAppQrCode(qr_data) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.set_whatsapp_qr(&qr_data);
                }
            }
            TuiEvent::WhatsAppConnected => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.set_whatsapp_connected();
                    let _ =
                        crate::config::Config::write_key("channels.whatsapp", "enabled", "true");
                }
            }
            TuiEvent::WhatsAppError(err) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.set_whatsapp_error(err);
                }
            }
            TuiEvent::ChannelTestResult {
                success,
                error,
                detected_telegram_user_id,
                ..
            } => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.channel_test_status = if success {
                        super::onboarding::ChannelTestStatus::Success
                    } else {
                        super::onboarding::ChannelTestStatus::Failed(
                            error.unwrap_or_else(|| "Unknown error".to_string()),
                        )
                    };
                    // Auto-fill detected Telegram user ID
                    if let Some(ref uid) = detected_telegram_user_id
                        && wizard.telegram_user_id_input.is_empty()
                    {
                        wizard.telegram_user_id_input = uid.clone();
                    }
                }
            }
            TuiEvent::BrainGenerationResult { result } => match result {
                Ok(msg) => {
                    self.push_system_message(format!("✓ {}", msg));
                }
                Err(e) => {
                    self.push_system_message(format!("Brain generation: {}", e));
                }
            },
            TuiEvent::WhisperDownloadProgress(progress) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.stt_model_download_progress = Some(progress);
                }
            }
            TuiEvent::WhisperDownloadComplete(result) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.stt_model_download_progress = None;
                    match result {
                        Ok(()) => {
                            wizard.stt_model_downloaded = true;
                            wizard.stt_model_download_error = None;
                        }
                        Err(e) => {
                            wizard.stt_model_download_error = Some(e);
                        }
                    }
                }
            }
            TuiEvent::PiperDownloadProgress(progress) => {
                if let Some(ref mut wizard) = self.onboarding {
                    // Ignore stale progress events after download completed
                    if !wizard.tts_voice_downloaded {
                        wizard.tts_voice_download_progress = Some(progress);
                    }
                }
            }
            TuiEvent::PiperDownloadComplete(result) => {
                if let Some(ref mut wizard) = self.onboarding {
                    wizard.tts_voice_download_progress = None;
                    match result {
                        #[cfg(feature = "local-tts")]
                        Ok(voice_id) => {
                            wizard.tts_voice_downloaded = true;
                            wizard.tts_voice_download_error = None;
                            tokio::spawn(async move {
                                if let Err(e) =
                                    crate::channels::voice::local_tts::preview_voice(&voice_id)
                                        .await
                                {
                                    tracing::warn!("Voice preview failed: {}", e);
                                }
                            });
                        }
                        #[cfg(not(feature = "local-tts"))]
                        Ok(_) => {
                            wizard.tts_voice_downloaded = true;
                            wizard.tts_voice_download_error = None;
                        }
                        Err(e) => {
                            wizard.tts_voice_download_error = Some(e);
                        }
                    }
                }
            }
            TuiEvent::SudoPasswordRequested(request) => {
                self.sudo_pending = Some(request);
                self.sudo_input.clear();
            }
            TuiEvent::SshPasswordRequested(request) => {
                self.ssh_pending = Some(request);
                self.ssh_input.clear();
            }
            TuiEvent::SystemMessage { session_id, text } => {
                // Self-healing alerts and other session-scoped system
                // messages must stay in the session that triggered them so
                // other open sessions never see a 🔧 alert that belongs to a
                // different chat. `Uuid::nil()` is a sentinel for global
                // notices (e.g. auto-update banner) that always show in the
                // currently focused session.
                if session_id == Uuid::nil() || self.is_current_session(session_id) {
                    self.push_system_message(text);
                }
            }
            TuiEvent::SuggestedOptions {
                session_id,
                options,
            } => {
                // Only surface in the session that produced them, and only when
                // the input is empty so we never shove a suggestion in front of
                // text the user is already typing. Always optional.
                if self.is_current_session(session_id)
                    && self.input_buffer.is_empty()
                    && !options.is_empty()
                {
                    self.followup_suggestions = options;
                    self.followup_selected_index = 0;
                }
            }
            TuiEvent::ProviderSwitched {
                session_id,
                to_name,
                to_model,
                reason: _,
            } => {
                // A fallback in one session must never leak into another
                // pane's footer, AND must always stick — both in DB (survives
                // restart) and in session_providers (so the next turn doesn't
                // re-walk the failing primary). Background sessions are the
                // critical case: if the session isn't currently focused, the
                // sessions cache may not contain it and the in-memory pin may
                // still point at the wrapper that just failed.
                let mut updated_in_cache = false;
                if let Some(target_session) = self.sessions.iter_mut().find(|s| s.id == session_id)
                {
                    target_session.provider_name = Some(to_name.clone());
                    target_session.model = Some(to_model.clone());
                    let target_copy = target_session.clone();
                    updated_in_cache = true;
                    if let Err(e) = self.session_service.update_session(&target_copy).await {
                        tracing::warn!(
                            "Failed to persist provider swap to session {}: {}",
                            session_id,
                            e
                        );
                    }
                }
                if !updated_in_cache {
                    // Cache miss — fetch fresh from DB and update directly so
                    // the swap persists regardless of what's loaded into the
                    // sessions sidebar. Without this, fallbacks fired on a
                    // session that isn't currently in the cache silently drop
                    // their persistence and the next turn re-runs the bounce.
                    match self.session_service.get_session(session_id).await {
                        Ok(Some(mut s)) => {
                            s.provider_name = Some(to_name.clone());
                            s.model = Some(to_model.clone());
                            if let Err(e) = self.session_service.update_session(&s).await {
                                tracing::warn!(
                                    "Failed to persist provider swap to uncached session {}: {}",
                                    session_id,
                                    e
                                );
                            }
                        }
                        Ok(None) => {
                            tracing::warn!("ProviderSwitched for unknown session {}", session_id);
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to load session {} for provider swap persistence: {}",
                                session_id,
                                e
                            );
                        }
                    }
                }

                // Always rebuild a clean session_providers entry from config
                // by name — for every session, focused or not. The agent's
                // own swap at tool_loop.rs:1302 reuses the fallback Arc,
                // which can still carry the failing primary internally. A
                // fresh by-name build is what makes the swap actually stick.
                if let Ok(config) = crate::config::Config::load()
                    && let Ok(new_provider) =
                        crate::brain::provider::factory::create_provider_by_name(&config, &to_name)
                            .await
                {
                    // Pair the rebuilt provider with the fallback's remapped
                    // model (to_model) atomically.
                    self.agent_service.swap_provider_for_session(
                        session_id,
                        new_provider.clone(),
                        to_model.clone(),
                    );
                    self.provider_cache.insert(to_name.clone(), new_provider);
                    tracing::info!(
                        "[ProviderSwitched] rebuilt session_providers for session {} → {}",
                        session_id,
                        to_name
                    );
                }

                // Footer + current_session mirror only when the swapped
                // session is the focused one. Other panes keep their own.
                if self.is_current_session(session_id) {
                    self.default_model_name = to_model.clone();
                    if let Some(ref mut session) = self.current_session {
                        session.provider_name = Some(to_name.clone());
                        session.model = Some(to_model.clone());
                    }
                }
            }
            TuiEvent::UpdateAvailable(version) => {
                self.update_available_version = Some(version);
                self.switch_mode(AppMode::UpdatePrompt).await?;
            }
            TuiEvent::FocusGained => {
                self.window_focused = true;
                // Immediate probe, bypassing the cooldown: alt-tabbing back is
                // the moment a screenshot taken elsewhere becomes visible (#1816).
                self.refresh_clipboard_hint(true);
            }
            TuiEvent::FocusLost => {
                self.window_focused = false;
            }
            TuiEvent::Resize(w, h) => {
                // Invalidate render cache on terminal resize (content width changes)
                self.render_cache.clear();
                // Store new dimensions so the runner can pre-resize ratatui
                // buffers without clearing the screen (avoids blink).
                self.pending_resize = Some((w, h));
            }
            TuiEvent::AgentProcessing => {
                // Handled by the render loop
            }
            TuiEvent::LauncherFailed { target, detail } => {
                // The optimistic "Opened …" line was written when spawn
                // succeeded; the exit status is the real answer (#2170).
                self.notification = Some(format!("Could not open {target}: {detail}"));
                self.notification_shown_at = Some(std::time::Instant::now());
            }
        }
        Ok(())
    }

    /// Handle keyboard input
    async fn handle_key_event(&mut self, event: crossterm::event::KeyEvent) -> Result<()> {
        use super::events::keys;
        use crossterm::event::{KeyCode, KeyModifiers};

        // Mouse-report fragments (#1983): when an escape read splits,
        // crossterm re-publishes SGR/URXVT bursts as individual Char
        // events. The #1943 gate only covered the chat plain-char path;
        // this choke point makes every surface (onboarding, dialogs,
        // mission control, sudo, rename) inherit the suppression before
        // any mode consumes the key. F12 and other non-Char keys always
        // pass, so the mouse-capture escape hatch stays reachable.
        if self.mouse_frag_gate.absorb(&event.code) {
            tracing::debug!("[MOUSEFRAG] dropped fragment {:?} at dispatch", event.code);
            return Ok(());
        }

        // Ctrl+C expanded command panel (#1775). Modal: while open, every
        // key is consumed; Esc/q/Ctrl+C dismiss it. Checked before all
        // dialog handlers so the panel wins focus from any of them.
        if self.dialog_help_open {
            let dismiss =
                matches!(event.code, KeyCode::Esc | KeyCode::Char('q')) || keys::is_quit(&event);
            if dismiss {
                self.dialog_help_open = false;
            }
            return Ok(());
        }

        // Ctrl+C with a dialog open expands that dialog's command panel
        // instead of starting the quit flow (#1775). Placed before the
        // sudo/theme/ssh intercepts because those swallow everything —
        // without this, Ctrl+C inside them pushed a literal 'c' into the
        // password buffer. In Chat mode `active_dialog_scope()` is None
        // and the global quit path below runs exactly as before.
        if keys::is_quit(&event)
            && let Some(scope) = self.active_dialog_scope()
        {
            self.dialog_help_scope = scope;
            self.dialog_help_open = true;
            self.ctrl_c_pending_at = None;
            return Ok(());
        }

        // F12 toggles mouse capture so the user can drag-select text natively.
        // Handled before everything else (including modal dialogs) so it always
        // works as an escape hatch when the TUI is hogging mouse events.
        if event.code == KeyCode::F(12) {
            self.mouse_capture_enabled = !self.mouse_capture_enabled;
            self.notification = Some(if self.mouse_capture_enabled {
                "Mouse capture ON (F12 to enable text selection)".to_string()
            } else {
                "Mouse capture OFF — drag to select, F12 to re-enable".to_string()
            });
            self.notification_shown_at = Some(std::time::Instant::now());
            return Ok(());
        }

        // Sudo password dialog intercepts all keys when active
        if self.sudo_pending.is_some() {
            match event.code {
                KeyCode::Enter => {
                    // Submit password
                    if let Some(request) = self.sudo_pending.take() {
                        let password = std::mem::take(&mut self.sudo_input);
                        let _ = request.response_tx.send(SudoPasswordResponse {
                            password: Some(password),
                        });
                    }
                }
                KeyCode::Esc => {
                    // Cancel sudo
                    if let Some(request) = self.sudo_pending.take() {
                        let _ = request
                            .response_tx
                            .send(SudoPasswordResponse { password: None });
                    }
                    self.sudo_input.clear();
                }
                KeyCode::Backspace => {
                    self.sudo_input.pop();
                }
                KeyCode::Char(c) => {
                    self.sudo_input.push(c);
                }
                _ => {}
            }
            return Ok(());
        }

        // Theme picker dialog (#1371) intercepts all keys while open. The
        // state machine is pure — every global effect (live preview, apply
        // + persist, Esc revert) executes here in one place.
        if self.theme_picker.is_some() {
            let (origin, action) = {
                let picker = self
                    .theme_picker
                    .as_mut()
                    .expect("theme_picker checked Some above");
                (picker.origin, picker.handle_key(&event, 10))
            };
            use crate::tui::render::theme_picker::PickerAction;
            match action {
                PickerAction::Preview(t) => crate::tui::render::theme::set(t),
                PickerAction::Apply(t) => {
                    crate::tui::render::theme::set(t);
                    // Bare name, not a pre-quoted literal: see #1428.
                    if let Err(e) = crate::config::Config::write_key_string("tui", "theme", t.name)
                    {
                        self.push_system_message(format!(
                            "Applied '{}' (live). Persist failed: {e}",
                            t.name
                        ));
                    } else {
                        self.push_system_message(format!(
                            "Theme switched to '{}' — applied live and persisted.",
                            t.name
                        ));
                    }
                    self.theme_picker = None;
                }
                PickerAction::Cancel => {
                    crate::tui::render::theme::set(origin);
                    self.theme_picker = None;
                }
                PickerAction::None => {}
            }
            return Ok(());
        }

        // SSH password dialog intercepts all keys when active. Same UX as sudo.
        if self.ssh_pending.is_some() {
            match event.code {
                KeyCode::Enter => {
                    if let Some(request) = self.ssh_pending.take() {
                        let password = std::mem::take(&mut self.ssh_input);
                        let _ = request.response_tx.send(SshPasswordResponse {
                            password: Some(password),
                        });
                    }
                }
                KeyCode::Esc => {
                    if let Some(request) = self.ssh_pending.take() {
                        let _ = request
                            .response_tx
                            .send(SshPasswordResponse { password: None });
                    }
                    self.ssh_input.clear();
                }
                KeyCode::Backspace => {
                    self.ssh_input.pop();
                }
                KeyCode::Char(c) => {
                    self.ssh_input.push(c);
                }
                _ => {}
            }
            return Ok(());
        }

        // Ctrl+C: first press clears input, second press (within 3s) quits.
        // When scrolled up, first press snaps back to bottom instead.
        if keys::is_quit(&event) {
            if !self.auto_scroll {
                // User is scrolled up — snap to bottom, clear input, no quit hint
                self.scroll_offset = 0;
                self.auto_scroll = true;
                self.input_buffer.clear();
                self.cursor_position = 0;
                self.slash_suggestions_active = false;
                self.error_message = None;
                self.error_message_shown_at = None;
                self.ctrl_c_pending_at = None;
                return Ok(());
            }
            if let Some(pending_at) = self.ctrl_c_pending_at
                && pending_at.elapsed() < std::time::Duration::from_secs(3)
            {
                // Second Ctrl+C within window — quit
                // Raise the shutdown flag BEFORE cancelling: the turn is being
                // cancelled because the process is going down, not because the
                // user abandoned it, and only that distinction keeps its
                // recovery ticket alive for the next boot (#1462).
                crate::brain::agent::service::shutdown::mark_shutting_down();
                // Cancel any running agent task
                if let Some(token) = &self.cancel_token {
                    token.cancel();
                }
                self.pane_manager.save_layout();
                self.should_quit = true;
                // Force exit after 1s in case spawn_blocking tasks are stuck
                tokio::spawn(async {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    std::process::exit(0);
                });
                return Ok(());
            }
            // First Ctrl+C — clear input and show hint
            self.input_buffer.clear();
            self.cursor_position = 0;
            self.slash_suggestions_active = false;
            self.error_message = Some("Press Ctrl+C again to quit".to_string());
            self.error_message_shown_at = Some(std::time::Instant::now());
            self.ctrl_c_pending_at = Some(std::time::Instant::now());
            return Ok(());
        }

        // Any non-Ctrl+C key resets the quit confirmation
        self.ctrl_c_pending_at = None;

        // Delete word — comprehensive handling across platforms.
        // macOS Option+Delete, Ctrl+Backspace, Ctrl+W, Ctrl+H — all delete the
        // previous word.  Terminals encode these in many ways:
        //   - KeyCode::Backspace + ALT/CONTROL modifier (standard)
        //   - KeyCode::Char('\x7f') + ALT (macOS Option+Delete with enhancement)
        //   - KeyCode::Char('\x08') + CONTROL (Ctrl+Backspace as Ctrl+H)
        //   - KeyCode::Char('h') + CONTROL (Ctrl+H without enhancement)
        //   - KeyCode::Char('w') + CONTROL (Ctrl+W)
        //   - KeyCode::Char('\x17') + CONTROL or NONE (Ctrl+W raw)
        {
            let is_delete_word = match event.code {
                KeyCode::Backspace => {
                    event.modifiers.contains(KeyModifiers::CONTROL)
                        || event.modifiers.contains(KeyModifiers::ALT)
                        || event.modifiers.contains(KeyModifiers::SUPER)
                }
                KeyCode::Char('\x7f') => {
                    // DEL char — macOS Option+Delete with keyboard enhancement
                    event.modifiers.contains(KeyModifiers::ALT)
                        || event.modifiers.contains(KeyModifiers::CONTROL)
                        || event.modifiers.contains(KeyModifiers::SUPER)
                        || event.modifiers.is_empty()
                }
                KeyCode::Char('\x08') => true, // raw Ctrl+H / Ctrl+Backspace
                KeyCode::Char('\x17') => true, // raw Ctrl+W
                KeyCode::Char('h') => event.modifiers.contains(KeyModifiers::CONTROL),
                KeyCode::Char('w') => event.modifiers.contains(KeyModifiers::CONTROL),
                _ => false,
            };
            if is_delete_word {
                self.delete_last_word();
                return Ok(());
            }
        }

        // Ctrl+Left or Alt+Left — jump to previous word boundary
        if event.code == KeyCode::Left
            && (event.modifiers.contains(KeyModifiers::CONTROL)
                || event.modifiers.contains(KeyModifiers::ALT))
        {
            let before = &self.input_buffer[..self.cursor_position];
            // Skip whitespace, then find start of word
            let trimmed = before.trim_end();
            self.cursor_position = trimmed
                .rfind(char::is_whitespace)
                .map(|pos| trimmed.ceil_char_boundary(pos + 1))
                .unwrap_or(0);
            return Ok(());
        }
        // macOS: Option+Left sends Char('b') with Alt modifier in some terminals
        if event.code == KeyCode::Char('b') && event.modifiers.contains(KeyModifiers::ALT) {
            let before = &self.input_buffer[..self.cursor_position];
            let trimmed = before.trim_end();
            self.cursor_position = trimmed
                .rfind(char::is_whitespace)
                .map(|pos| trimmed.ceil_char_boundary(pos + 1))
                .unwrap_or(0);
            return Ok(());
        }

        // Ctrl+Right or Alt+Right — jump to next word boundary
        if event.code == KeyCode::Right
            && (event.modifiers.contains(KeyModifiers::CONTROL)
                || event.modifiers.contains(KeyModifiers::ALT))
        {
            let after = &self.input_buffer[self.cursor_position..];
            // Skip current word chars, then skip whitespace
            let word_end = after.find(char::is_whitespace).unwrap_or(after.len());
            let rest = &after[word_end..];
            let space_end = rest
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len());
            self.cursor_position += word_end + space_end;
            return Ok(());
        }
        // macOS: Option+Right sends Char('f') with Alt modifier in some terminals
        if event.code == KeyCode::Char('f') && event.modifiers.contains(KeyModifiers::ALT) {
            let after = &self.input_buffer[self.cursor_position..];
            let word_end = after.find(char::is_whitespace).unwrap_or(after.len());
            let rest = &after[word_end..];
            let space_end = rest
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len());
            self.cursor_position += word_end + space_end;
            return Ok(());
        }

        // Ctrl+U — delete to start of current line
        if event.code == KeyCode::Char('u') && event.modifiers == KeyModifiers::CONTROL {
            let line_start = self.input_buffer[..self.cursor_position]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            self.input_buffer.drain(line_start..self.cursor_position);
            self.cursor_position = line_start;
            return Ok(());
        }

        if keys::is_new_session(&event) {
            self.create_new_session().await?;
            return Ok(());
        }

        if keys::is_list_sessions(&event) {
            self.switch_mode(AppMode::Sessions).await?;
            return Ok(());
        }

        if keys::is_clear_session(&event) {
            self.clear_session().await?;
            return Ok(());
        }

        // Split pane focus & close (global — work from Chat mode)
        if keys::is_close_pane(&event) && self.pane_manager.is_split() {
            self.pane_manager.close_focused();
            self.pane_manager.save_layout();
            if let Some(pane) = self.pane_manager.focused_pane()
                && let Some(session_id) = pane.session_id
            {
                self.load_session(session_id).await?;
                // Ensure the session's provider entry exists in the HashMap
                // after load. load_session skips swap if saved == current,
                // but current may be from global fallback (closed pane's
                // provider), causing contamination in the footer.
                if let Some(ref session) = self.current_session
                    && let Some(ref saved_provider) = session.provider_name
                {
                    let current_prov = self.agent_service.provider_name_for_session(session.id);
                    if saved_provider != &current_prov
                        && let Some(cached) = self.provider_cache.get(saved_provider).cloned()
                    {
                        // Restore the session's saved provider+model pair.
                        let model = session
                            .model
                            .clone()
                            .unwrap_or_else(|| cached.default_model().to_string());
                        self.agent_service
                            .swap_provider_for_session(session.id, cached, model);
                    }
                }
            }
            return Ok(());
        }
        if keys::is_focus_next_pane(&event) && self.pane_manager.is_split() {
            self.pane_manager.focus_next();
            if let Some(pane) = self.pane_manager.focused_pane()
                && let Some(session_id) = pane.session_id
            {
                self.load_session(session_id).await?;
                // Ensure the session's provider entry exists in the HashMap
                // after load. load_session skips swap if saved == current,
                // but current may be from global fallback (other pane's
                // provider), causing contamination in the footer.
                if let Some(ref session) = self.current_session
                    && let Some(ref saved_provider) = session.provider_name
                {
                    let current_prov = self.agent_service.provider_name_for_session(session.id);
                    if saved_provider != &current_prov
                        && let Some(cached) = self.provider_cache.get(saved_provider).cloned()
                    {
                        // Restore the session's saved provider+model pair.
                        let model = session
                            .model
                            .clone()
                            .unwrap_or_else(|| cached.default_model().to_string());
                        self.agent_service
                            .swap_provider_for_session(session.id, cached, model);
                    }
                }
            }
            return Ok(());
        }

        // Mode-specific handling
        tracing::trace!("Current mode: {:?}", self.mode);
        match self.mode {
            AppMode::Chat => self.handle_chat_key(event).await?,
            AppMode::Sessions => self.handle_sessions_key(event).await?,
            AppMode::FilePicker => self.handle_file_picker_key(event).await?,
            AppMode::DirectoryPicker => self.handle_directory_picker_key(event).await?,
            AppMode::UsageDashboard => {
                use crate::usage::dashboard::{UsageKeyOutcome, decide};
                // The dashboard's own keymap decides what is live; this arm
                // keeps only the effects that need the App: re-fetching after
                // a period change and leaving the mode (#2027).
                let outcome = match &mut self.dashboard_state {
                    Some(ds) => decide(ds, event),
                    // Mode without state: the only sane move is out.
                    None => UsageKeyOutcome::Close,
                };
                match outcome {
                    UsageKeyOutcome::Period(period) => self.set_dashboard_period(period).await,
                    UsageKeyOutcome::Close => {
                        self.dashboard_state = None;
                        self.switch_mode(AppMode::Chat).await?;
                    }
                    UsageKeyOutcome::Consumed | UsageKeyOutcome::NotConsumed => {}
                }
            }
            AppMode::RestartPending => {
                if keys::is_cancel(&event) {
                    self.switch_mode(AppMode::Chat).await?;
                } else if keys::is_enter(&event) {
                    // Perform the restart
                    if let Some(session) = &self.current_session {
                        let session_id = session.id;
                        if let Ok(updater) = SelfUpdater::auto_detect()
                            && let Err(e) = updater.restart(session_id)
                        {
                            self.show_error(format!("Restart failed: {}", e));
                            self.switch_mode(AppMode::Chat).await?;
                        }
                        // If restart succeeds, this process is replaced — we never reach here
                    }
                }
            }
            AppMode::UpdatePrompt => {
                if keys::is_cancel(&event) {
                    // Decline — return to chat so user keeps working on current version
                    self.update_available_version = None;
                    self.switch_mode(AppMode::Chat).await?;
                } else if keys::is_enter(&event) {
                    let version = self.update_available_version.take();
                    self.switch_mode(AppMode::Chat).await?;
                    if let Some(v) = version {
                        self.push_system_message(format!("Updating to v{}...", v));
                        let tx = self.event_sender();
                        let sid = self
                            .current_session
                            .as_ref()
                            .map(|s| s.id)
                            .unwrap_or(Uuid::nil());
                        tokio::spawn(async move {
                            super::messaging::run_evolve_directly(sid, tx).await;
                        });
                    }
                }
            }
            AppMode::Onboarding => {
                self.handle_onboarding_key(event).await?;
            }
            AppMode::Help | AppMode::Settings => {
                use crossterm::event::KeyCode;
                // Typing goes into the filter while it is open (#1527). Only
                // the help screen has a catalogue to filter; Settings shares
                // this arm for scrolling and falls through to it.
                if self.help_search_active && self.mode == AppMode::Help {
                    match event.code {
                        // Esc clears the filter and stays, matching the
                        // sessions screen. Leaving help takes a second Esc,
                        // so a mistyped filter costs one key, not the screen.
                        KeyCode::Esc => {
                            self.help_search_active = false;
                            self.help_search.clear();
                            self.help_scroll_offset = 0;
                        }
                        // Backspace past the start closes the filter rather
                        // than leaving an empty prompt the user has to Esc
                        // out of anyway.
                        KeyCode::Backspace => {
                            if self.help_search.is_empty() {
                                self.help_search_active = false;
                            } else {
                                self.help_search.pop();
                            }
                            self.help_scroll_offset = 0;
                        }
                        KeyCode::Char(c) if !c.is_control() => {
                            self.help_search.push(c);
                            // The result set changed under the offset, so an
                            // old scroll position means nothing now.
                            self.help_scroll_offset = 0;
                        }
                        _ => {}
                    }
                    return Ok(());
                }

                let max = super::help_catalog::max_scroll(
                    self.help_content_rows,
                    self.help_viewport_rows,
                );
                if keys::is_cancel(&event) {
                    self.help_scroll_offset = 0;
                    self.help_search.clear();
                    self.switch_mode(AppMode::Chat).await?;
                } else if self.mode == AppMode::Help && event.code == KeyCode::Char('/') {
                    self.help_search_active = true;
                    self.help_scroll_offset = 0;
                } else if keys::is_up(&event) {
                    self.help_scroll_offset = self.help_scroll_offset.saturating_sub(1);
                } else if keys::is_down(&event) {
                    // Clamped: an unbounded offset used to scroll the screen
                    // into blank space with no cue for how far back to go.
                    self.help_scroll_offset = self.help_scroll_offset.saturating_add(1).min(max);
                } else if keys::is_page_up(&event) {
                    self.help_scroll_offset = self.help_scroll_offset.saturating_sub(10);
                } else if keys::is_page_down(&event) {
                    self.help_scroll_offset = self.help_scroll_offset.saturating_add(10).min(max);
                }
            }
            AppMode::PlanOverlay => {
                use crossterm::event::KeyCode;
                if keys::is_cancel(&event) {
                    self.plan_overlay_scroll = 0;
                    self.switch_mode(AppMode::Chat).await?;
                } else if keys::is_up(&event) {
                    self.plan_overlay_scroll = self.plan_overlay_scroll.saturating_sub(1);
                } else if keys::is_down(&event) {
                    self.plan_overlay_scroll = self.plan_overlay_scroll.saturating_add(1);
                } else if keys::is_page_up(&event) {
                    self.plan_overlay_scroll = self.plan_overlay_scroll.saturating_sub(10);
                } else if keys::is_page_down(&event) {
                    self.plan_overlay_scroll = self.plan_overlay_scroll.saturating_add(10);
                } else if event.code == KeyCode::Char('a') {
                    // Approve: the same idle path as /execute, deliberately
                    // NOT the tool-policy approve flow. Close the overlay
                    // first so the seed turn's activity is visible in chat.
                    self.plan_overlay_scroll = 0;
                    self.switch_mode(AppMode::Chat).await?;
                    let _ = self.handle_slash_command("/execute").await;
                } else if event.code == KeyCode::Char('d') {
                    self.plan_overlay_scroll = 0;
                    self.switch_mode(AppMode::Chat).await?;
                    let _ = self.handle_slash_command("/discard").await;
                }
            }
            AppMode::MissionControl => {
                crate::tui::app::mission_control::input::handle_key(self, event).await;
            }
            AppMode::SkillsList => {
                crate::tui::app::skills_dialog::input::handle_key(self, event).await;
            }
            AppMode::Profiles => {
                crate::tui::app::profiles_dialog::input::handle_key(self, event).await;
            }
            AppMode::SessionFiles => {
                self.handle_session_files_key(event).await?;
            }
            AppMode::Projects => {
                self.handle_projects_key(event).await?;
            }
        }

        Ok(())
    }

    /// Hard-abort the agent task belonging to `session_id`, if one is running.
    ///
    /// Scoped to the session on purpose: aborting by session means a cancel in
    /// one pane can never kill a turn another session is still streaming, which
    /// is what the old app-level `Option<AbortHandle>` allowed (#2155).
    pub(crate) fn abort_session_task(&mut self, session_id: Uuid) {
        if let Some(handle) = self.task_abort_handles.remove(&session_id) {
            handle.abort();
        }
    }

    /// Show an error message
    pub(crate) fn show_error(&mut self, error: String) {
        self.is_processing = false;
        self.processing_started_at = None;
        self.streaming_response = None;
        self.streaming_render_cache = None;
        self.streaming_reasoning = None;
        self.cancel_token = None;
        // Drop only THIS session's abort handle: another session's turn may
        // still be running and must survive an error shown here (#2155).
        if let Some(sid) = self.current_session.as_ref().map(|s| s.id) {
            self.task_abort_handles.remove(&sid);
        }
        self.escape_pending_at = None;
        // Preserve context token count from real-time updates if we never got a complete response
        if self.last_input_tokens.is_none() && self.display_token_count > 0 {
            self.last_input_tokens = Some(self.display_token_count as u32);
        }
        // Deny any pending approvals so agent callbacks don't hang, then remove
        for msg in &mut self.messages {
            if let Some(ref mut approval) = msg.approval
                && approval.state == ApprovalState::Pending
            {
                let _ = approval.response_tx.send(ToolApprovalResponse {
                    request_id: approval.request_id,
                    approved: false,
                    reason: Some("Error occurred".to_string()),
                });
                approval.state = ApprovalState::Denied("Error occurred".to_string());
            }
        }
        // Finalize any active tool group
        if let Some(group) = self.active_tool_group.take() {
            let count = group.calls.len();
            self.messages.push(DisplayMessage {
                id: Uuid::new_v4(),
                role: "tool_group".to_string(),
                content: format!("{} tool call{}", count, if count == 1 { "" } else { "s" }),
                timestamp: chrono::Utc::now(),
                token_count: None,
                cost: None,
                approval: None,
                approve_menu: None,
                details: None,
                expanded: false,
                expanded_full: false,
                tool_group: Some(group),
                duration_secs: None,
            });
        }
        // Persist the error as a permanent chat bubble so the user
        // can see (and scroll back to) what went wrong AFTER the
        // 2.5s transient toast expires. Without this the toast
        // disappears and the user is left staring at a turn with
        // tool calls + thinking but no completion, with no way to
        // tell self-heal already gave up. Confirmed user report
        // 2026-06-01: a 5xx exhaustion fired `TuiEvent::Error` →
        // `show_error` → 2.5s toast → vanished. User saw nothing
        // and assumed the agent silently dropped the request.
        //
        // Filter out the special `"Cancelled"` and
        // `"Press Ctrl+C again to quit"` sentinels — those aren't
        // turn-failure errors, they're transient UI hints.
        if error != "Cancelled" && !error.starts_with("Press Ctrl+C") {
            self.messages.push(DisplayMessage {
                id: Uuid::new_v4(),
                role: "error".to_string(),
                content: error.clone(),
                timestamp: chrono::Utc::now(),
                token_count: None,
                cost: None,
                approval: None,
                approve_menu: None,
                details: None,
                expanded: false,
                expanded_full: false,
                tool_group: None,
                duration_secs: None,
            });
        }
        self.error_message = Some(error);
        self.error_message_shown_at = Some(std::time::Instant::now());
        // Auto-scroll to show the error
        self.scroll_offset = 0;
    }

    /// Probe the macOS clipboard and raise the image-in-clipboard hint (#1816).
    ///
    /// Runs on the tick behind a cooldown and immediately on FocusGained:
    /// screenshot hotkeys do not steal focus, so the poll is what covers
    /// staying inside the TUI, while the focus event covers alt-tabbing back.
    fn refresh_clipboard_hint(&mut self, force: bool) {
        if self.mode != AppMode::Chat || !self.window_focused {
            return;
        }
        let now = std::time::Instant::now();
        if !force
            && self
                .clipboard_probe_at
                .is_some_and(|t| now.saturating_duration_since(t) < CLIPBOARD_PROBE_INTERVAL)
        {
            return;
        }
        // On Windows the probe is a PowerShell spawn, 100 to 300ms, which is
        // far too heavy for a 3 second tick (#2174). A focus change is the
        // only event that justifies that cost.
        #[cfg(windows)]
        if !force {
            return;
        }
        self.clipboard_probe_at = Some(now);
        let Some((kinds, probe)) = super::clipboard_route::read_clipboard_kinds() else {
            return;
        };
        let fp = super::clipboard_route::clipboard_info_fingerprint(&probe);
        if self.clipboard_hint_fp != Some(fp) {
            // The clipboard content changed: a fresh screenshot hints again.
            self.clipboard_hint_fp = Some(fp);
            self.clipboard_hint_suppressed = false;
        }
        if self.clipboard_hint_suppressed || !kinds.is_image_only() {
            return;
        }
        self.clipboard_hint = Some(CLIPBOARD_HINT_TEXT.to_string());
        self.clipboard_hint_shown_at = Some(now);
    }

    /// Switch to a different mode
    pub(crate) async fn switch_mode(&mut self, mode: AppMode) -> Result<()> {
        tracing::info!("🔄 Switching mode to: {:?}", mode);
        self.mode = mode;

        if mode == AppMode::Sessions {
            self.load_sessions().await?;
            // Only on the open path: `load_sessions` also runs on rename and
            // delete while the picker is open, where moving the cursor would
            // yank it out from under a browsing user (#1465).
            self.focus_current_session();
        }

        if mode == AppMode::Help {
            // Collected here rather than in the renderer, which used to walk
            // the skills directories and re-parse commands.toml on every
            // frame the help screen was open (#1530). Re-read on each visit
            // so a skill added mid-session still shows up.
            self.help_catalog = super::help_catalog::load();
            self.help_search.clear();
            self.help_search_active = false;
            self.help_scroll_offset = 0;
        }

        Ok(())
    }

    /// Get total token count for current session (from DB, not in-memory messages).
    /// In-memory messages only cover the current context window — the DB has the
    /// cumulative total across all compactions.
    pub fn total_tokens(&self) -> i64 {
        self.current_session
            .as_ref()
            .map(|s| s.token_count)
            .unwrap_or(0)
    }

    /// Get context usage as a percentage
    /// Uses the calibrated message token count (excludes tool schema overhead)
    pub fn context_usage_percent(&self) -> f64 {
        if self.context_max_tokens == 0 {
            return 0.0;
        }
        let used = self.last_input_tokens.unwrap_or(0) as f64;
        (used / self.context_max_tokens as f64) * 100.0
    }

    /// Get total cost for current session (from DB, not in-memory messages).
    pub fn total_cost(&self) -> f64 {
        self.current_session
            .as_ref()
            .map(|s| s.total_cost)
            .unwrap_or(0.0)
    }

    /// Handle tool approval request — inline in chat (session-aware)
    fn handle_approval_requested(&mut self, request: ToolApprovalRequest) {
        let is_current = self.is_current_session(request.session_id);

        // Always read approval policy from config at runtime — never trust
        // cached flags. This ensures changes to config.toml or /approve
        // take effect immediately without session restart.
        let (auto_session, auto_always) = Self::read_approval_policy_from_config();
        // Sync cached state for render/display consistency
        self.approval_auto_session = auto_session;
        self.approval_auto_always = auto_always;

        tracing::info!(
            "[APPROVAL] handle_approval_requested tool='{}' session={} is_current={} auto_session={} auto_always={}",
            request.tool_name,
            request.session_id,
            is_current,
            auto_session,
            auto_always
        );

        // Auto-approve silently if policy allows
        if auto_always || auto_session {
            let response = ToolApprovalResponse {
                request_id: request.request_id,
                approved: true,
                reason: None,
            };
            let _ = request.response_tx.send(response.clone());
            let _ = self
                .event_sender()
                .send(TuiEvent::ToolApprovalResponse(response));
            return;
        }

        // Background session approval — auto-approve (user can't interact with it)
        // They'll see the results when they switch to that session
        if !is_current {
            tracing::info!(
                "[APPROVAL] Auto-approving background session {} tool '{}'",
                request.session_id,
                request.tool_name
            );
            let response = ToolApprovalResponse {
                request_id: request.request_id,
                approved: true,
                reason: Some("Auto-approved (background session)".to_string()),
            };
            let _ = request.response_tx.send(response.clone());
            let _ = self
                .event_sender()
                .send(TuiEvent::ToolApprovalResponse(response));
            return;
        }

        // Deny stale pending approvals from previous requests in THIS session only
        for msg in &mut self.messages {
            if let Some(ref mut approval) = msg.approval
                && approval.state == ApprovalState::Pending
            {
                let _ = approval.response_tx.send(ToolApprovalResponse {
                    request_id: approval.request_id,
                    approved: false,
                    reason: Some("Superseded by new request".to_string()),
                });
                approval.state = ApprovalState::Denied("Superseded by new request".to_string());
            }
        }

        // Clear streaming overlay so the approval dialog is visible
        self.streaming_render_cache = None;
        if let Some(text) = self.streaming_response.take()
            && !text.trim().is_empty()
        {
            // Persist any streamed text as a regular message before showing approval
            self.messages.push(DisplayMessage {
                id: Uuid::new_v4(),
                role: "assistant".to_string(),
                content: text,
                timestamp: chrono::Utc::now(),
                token_count: None,
                cost: None,
                approval: None,
                approve_menu: None,
                details: None,
                expanded: false,
                expanded_full: false,
                tool_group: None,
                duration_secs: None,
            });
        }

        // Show inline approval in chat
        self.messages.push(DisplayMessage {
            id: Uuid::new_v4(),
            role: "approval".to_string(),
            content: String::new(),
            timestamp: chrono::Utc::now(),
            token_count: None,
            cost: None,
            approval: Some(ApprovalData {
                tool_name: request.tool_name,
                tool_description: request.tool_description,
                tool_input: request.tool_input,
                capabilities: request.capabilities,
                request_id: request.request_id,
                response_tx: request.response_tx,
                requested_at: request.requested_at,
                state: ApprovalState::Pending,
                selected_option: 0,
                show_details: false,
            }),
            approve_menu: None,
            details: None,
            expanded: false,
            expanded_full: false,
            tool_group: None,
            duration_secs: None,
        });
        // Auto-collapse all tool groups so the approval dialog is immediately visible
        if let Some(ref mut group) = self.active_tool_group {
            group.expanded = false;
        }
        for msg in self.messages.iter_mut() {
            if let Some(ref mut group) = msg.tool_group {
                group.expanded = false;
            }
        }
        self.auto_scroll = true;
        self.scroll_offset = 0;
        tracing::info!(
            "[APPROVAL] Pushed approval message for tool='{}', total messages={}, has_pending={}",
            self.messages
                .last()
                .map(|m| m
                    .approval
                    .as_ref()
                    .map(|a| a.tool_name.as_str())
                    .unwrap_or("?"))
                .unwrap_or("?"),
            self.messages.len(),
            self.has_pending_approval()
        );
        // Stay in AppMode::Chat — no mode switch
    }

    /// Update slash command autocomplete suggestions (built-in + user-defined)
    pub(crate) fn update_slash_suggestions(&mut self) {
        let input = self.input_buffer.trim_start();
        if input.starts_with('/') && !input.contains(' ') && !input.is_empty() {
            let prefix = input.to_lowercase();

            // Built-in commands: indices 0..SLASH_COMMANDS.len()
            self.slash_filtered = SLASH_COMMANDS
                .iter()
                .enumerate()
                .filter(|(_, cmd)| cmd.name.starts_with(&prefix))
                .map(|(i, _)| i)
                .collect();

            // User-defined commands: indices starting at SLASH_COMMANDS.len()
            // Skip user commands that shadow a built-in name
            let base_user = SLASH_COMMANDS.len();
            for (i, ucmd) in self.user_commands.iter().enumerate() {
                if ucmd.name.to_lowercase().starts_with(&prefix)
                    && !SLASH_COMMANDS.iter().any(|b| b.name == ucmd.name)
                {
                    self.slash_filtered.push(base_user + i);
                }
            }

            // Skills: indices starting at SLASH_COMMANDS.len() + user_commands.len()
            // Skip skills shadowed by a built-in or a user command of the same
            // `/<name>`. The skill's precomputed `slash_name` already carries
            // the leading slash for direct comparison.
            let base_skill = SLASH_COMMANDS.len() + self.user_commands.len();
            for (i, skill) in self.skills.iter().enumerate() {
                let lower = skill.slash_name.to_lowercase();
                if lower.starts_with(&prefix)
                    && !SLASH_COMMANDS.iter().any(|b| b.name == skill.slash_name)
                    && !self
                        .user_commands
                        .iter()
                        .any(|c| c.name == skill.slash_name)
                {
                    self.slash_filtered.push(base_skill + i);
                }
            }

            // Sort suggestions alphabetically by command name. Inline the
            // index → name resolution to avoid an immutable borrow of self
            // inside the sort_by closure (which holds a mutable borrow of
            // self.slash_filtered).
            let n_builtin = SLASH_COMMANDS.len();
            let n_user = self.user_commands.len();
            let name_at = |idx: usize| -> &str {
                if idx < n_builtin {
                    SLASH_COMMANDS[idx].name
                } else if idx < n_builtin + n_user {
                    self.user_commands
                        .get(idx - n_builtin)
                        .map(|c| c.name.as_str())
                        .unwrap_or("")
                } else {
                    self.skills
                        .get(idx - n_builtin - n_user)
                        .map(|s| s.slash_name.as_str())
                        .unwrap_or("")
                }
            };
            self.slash_filtered
                .sort_by(|&a, &b| name_at(a).cmp(name_at(b)));

            self.slash_suggestions_active = !self.slash_filtered.is_empty();
            // Clamp selected index
            if self.slash_selected_index >= self.slash_filtered.len() {
                self.slash_selected_index = 0;
            }
        } else {
            self.slash_suggestions_active = false;
            self.slash_filtered.clear();
            self.slash_selected_index = 0;
        }
    }

    /// Get the name of a slash command by its combined index
    /// (0..N built-ins, N..M user commands, M.. skills as `/<slug>`).
    pub fn slash_command_name(&self, index: usize) -> Option<&str> {
        let n_builtin = SLASH_COMMANDS.len();
        let n_user = self.user_commands.len();
        if index < n_builtin {
            Some(SLASH_COMMANDS[index].name)
        } else if index < n_builtin + n_user {
            self.user_commands
                .get(index - n_builtin)
                .map(|c| c.name.as_str())
        } else {
            self.skills
                .get(index - n_builtin - n_user)
                .map(|s| s.slash_name.as_str())
        }
    }

    /// Get the description of a slash command by its combined index
    pub fn slash_command_description(&self, index: usize) -> Option<&str> {
        let n_builtin = SLASH_COMMANDS.len();
        let n_user = self.user_commands.len();
        if index < n_builtin {
            Some(SLASH_COMMANDS[index].description)
        } else if index < n_builtin + n_user {
            self.user_commands
                .get(index - n_builtin)
                .map(|c| c.description.as_str())
        } else {
            self.skills
                .get(index - n_builtin - n_user)
                .map(|s| s.description.as_str())
        }
    }

    /// Reload user commands from brain workspace (called after agent responses)
    pub(crate) fn reload_user_commands(&mut self) {
        let command_loader = CommandLoader::from_brain_path(&self.brain_path);
        self.user_commands = command_loader.load();
        // Skills can also be edited live (~/.opencrabs/skills/<name>/SKILL.md);
        // reload alongside user commands so the autocomplete reflects edits
        // without restart.
        self.skills = crate::brain::skills::load_all_skills();
    }

    /// Update emoji picker based on the text behind the cursor.
    /// Triggers when there's `:query` (colon + at least 1 char, no spaces).
    pub(crate) fn update_emoji_picker(&mut self) {
        // Search backwards from cursor for an unmatched ':'
        let before_cursor = &self.input_buffer[..self.cursor_position];
        if let Some(colon_pos) = before_cursor.rfind(':') {
            let query = &before_cursor[colon_pos + 1..];
            // Must have at least 1 char, no spaces, no other ':'
            if !query.is_empty() && !query.contains(' ') && !query.contains(':') {
                let query_lower = query.to_lowercase();
                let max_results = 8;
                self.emoji_filtered = emojis::iter()
                    .filter_map(|e| {
                        e.shortcodes()
                            .find(|sc| sc.contains(&*query_lower))
                            .map(|sc| (e.as_str(), sc))
                    })
                    .take(max_results)
                    .collect();
                if !self.emoji_filtered.is_empty() {
                    self.emoji_picker_active = true;
                    self.emoji_colon_offset = colon_pos;
                    if self.emoji_selected_index >= self.emoji_filtered.len() {
                        self.emoji_selected_index = 0;
                    }
                    return;
                }
            }
        }
        self.dismiss_emoji_picker();
    }

    /// Dismiss the emoji picker.
    pub(crate) fn dismiss_emoji_picker(&mut self) {
        self.emoji_picker_active = false;
        self.emoji_filtered.clear();
        self.emoji_selected_index = 0;
    }

    /// Insert the selected emoji, replacing `:query` with the emoji char.
    pub(crate) fn accept_emoji(&mut self) {
        if let Some(&(emoji, _)) = self.emoji_filtered.get(self.emoji_selected_index) {
            let colon = self.emoji_colon_offset;
            let end = self.cursor_position;
            self.input_buffer.replace_range(colon..end, emoji);
            self.cursor_position = colon + emoji.len();
            self.dismiss_emoji_picker();
        }
    }
}

/// Which screen a launch opens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchPlan {
    /// The opt-in wizard: `--onboard` or `/onboard`.
    Wizard,
    /// Straight to chat. `first_run` marks the launch that still has to bring
    /// the built-in provider online and greet the user (#2079).
    Chat { first_run: bool },
}

/// Decide the landing screen.
///
/// Zero setup is the design: a download, open, and the built-in provider is
/// already there talking to you. So a first run goes to chat, NOT to the
/// wizard, and the wizard only appears when the user asks for it with
/// `--onboard` / `/onboard` (#2079). Landing a new install on step 1 of 6 is
/// the regression this encodes against.
pub(crate) fn plan_launch(force_onboard: bool, is_first_time: bool) -> LaunchPlan {
    if force_onboard {
        LaunchPlan::Wizard
    } else {
        LaunchPlan::Chat {
            first_run: is_first_time,
        }
    }
}
